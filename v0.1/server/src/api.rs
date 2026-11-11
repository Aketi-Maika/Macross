//! REST API とログインセッション

use crate::config::IceServer;
use crate::metrics::Metrics;
use crate::users::{random_token, AuthUser, LoginError, UserView};
use crate::AppState;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{async_trait, Json};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

pub const COOKIE: &str = "macross_session";

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "ログインしてください".into())
    }
    fn forbidden(msg: impl Into<String>) -> Self {
        Self(StatusCode::FORBIDDEN, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::bad(e.to_string())
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// ログイン中ユーザ（無効化されたユーザは弾く）
pub struct CurrentUser(pub AuthUser);

#[async_trait]
impl FromRequestParts<Arc<AppState>> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(COOKIE).map(|c| c.value().to_string()).ok_or_else(ApiError::unauthorized)?;
        let uid = st.sessions.lock().unwrap().get(&token).cloned().ok_or_else(ApiError::unauthorized)?;
        let user = st.users.get(&uid).ok_or_else(ApiError::unauthorized)?;
        Ok(CurrentUser(user))
    }
}

/// パスワード変更済みのログインユーザ
pub struct ActiveUser(pub AuthUser);

#[async_trait]
impl FromRequestParts<Arc<AppState>> for ActiveUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let CurrentUser(u) = CurrentUser::from_request_parts(parts, st).await?;
        if u.must_change {
            return Err(ApiError::forbidden("初期パスワードを変更してください"));
        }
        Ok(ActiveUser(u))
    }
}

/// admin
pub struct AdminUser(pub AuthUser);

#[async_trait]
impl FromRequestParts<Arc<AppState>> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let ActiveUser(u) = ActiveUser::from_request_parts(parts, st).await?;
        if !u.admin {
            return Err(ApiError::forbidden("admin 権限が必要です"));
        }
        Ok(AdminUser(u))
    }
}

#[derive(Serialize)]
pub struct PublicConfig {
    channel: String,
    ice_servers: Vec<IceServer>,
}

pub async fn config(State(st): State<Arc<AppState>>) -> Json<PublicConfig> {
    Json(PublicConfig { channel: st.cfg.channel.name.clone(), ice_servers: st.cfg.janus.ice_servers.clone() })
}

#[derive(Deserialize)]
pub struct LoginReq {
    id: String,
    password: String,
}

pub async fn login(
    State(st): State<Arc<AppState>>,
    jar: CookieJar,
    Json(req): Json<LoginReq>,
) -> ApiResult<(CookieJar, Json<AuthUser>)> {
    let user = match st.users.authenticate(req.id.trim(), &req.password) {
        Ok(u) => {
            Metrics::inc(&st.metrics.login_success_total);
            u
        }
        Err(e) => {
            Metrics::inc(&st.metrics.login_failure_total);
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Err(match e {
                LoginError::Invalid => ApiError(StatusCode::UNAUTHORIZED, "ID またはパスワードが違います".into()),
                LoginError::Disabled => ApiError::forbidden("この ID は無効化されています"),
            });
        }
    };
    let token = random_token(48);
    st.sessions.lock().unwrap().insert(token.clone(), user.id.clone());
    tracing::info!("ログイン: {} ({})", user.name, user.id);
    let cookie = Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(st.cfg.server.cookie_secure);
    Ok((jar.add(cookie), Json(user)))
}

pub async fn logout(State(st): State<Arc<AppState>>, jar: CookieJar) -> (CookieJar, StatusCode) {
    if let Some(c) = jar.get(COOKIE) {
        st.sessions.lock().unwrap().remove(c.value());
    }
    (jar.remove(Cookie::build((COOKIE, "")).path("/")), StatusCode::NO_CONTENT)
}

pub async fn me(CurrentUser(u): CurrentUser) -> Json<AuthUser> {
    Json(u)
}

#[derive(Deserialize)]
pub struct PasswordReq {
    current: String,
    new: String,
}

pub async fn change_password(
    State(st): State<Arc<AppState>>,
    CurrentUser(u): CurrentUser,
    Json(req): Json<PasswordReq>,
) -> ApiResult<Json<AuthUser>> {
    st.users.change_password(&u.id, &req.current, &req.new)?;
    let user = st.users.get(&u.id).ok_or_else(ApiError::unauthorized)?;
    Ok(Json(user))
}

#[derive(Serialize, Deserialize)]
pub struct Settings {
    max_speakers: usize,
}

pub async fn get_settings(State(st): State<Arc<AppState>>, _: AdminUser) -> Json<serde_json::Value> {
    Json(json!({
        "max_speakers": st.radio.max_speakers(),
        "max_connections": st.cfg.limits.max_connections,
    }))
}

pub async fn put_settings(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(s): Json<Settings>,
) -> ApiResult<StatusCode> {
    st.radio.set_max_speakers(s.max_speakers)?;
    tracing::info!("発言枠を {} に変更 (by {})", s.max_speakers, admin.id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_users(State(st): State<Arc<AppState>>, _: AdminUser) -> Json<Vec<UserView>> {
    Json(st.users.list())
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    name: String,
    #[serde(default)]
    furigana: String,
    #[serde(default)]
    email: String,
}

pub async fn create_user(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<CreateUserReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (id, password) = st.users.create_user(&req.name, &req.furigana, &req.email)?;
    tracing::info!("ID {id} を発行 (by {})", admin.id);
    Ok(Json(json!({ "id": id, "initial_password": password })))
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    admin: Option<bool>,
    disabled: Option<bool>,
}

pub async fn update_user(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> ApiResult<StatusCode> {
    if id == admin.id && (req.admin == Some(false) || req.disabled == Some(true)) {
        return Err(ApiError::bad("自分自身の admin 権限の解除・無効化はできません"));
    }
    st.users.update_flags(&id, req.admin, req.disabled)?;
    if req.disabled == Some(true) {
        // 無効化したユーザのログインセッションを破棄
        st.sessions.lock().unwrap().retain(|_, uid| *uid != id);
    }
    tracing::info!("ID {id} の権限を変更 admin={:?} disabled={:?} (by {})", req.admin, req.disabled, admin.id);
    Ok(StatusCode::NO_CONTENT)
}

/// Caddy の forward_auth 用。admin でログインしていれば 204（File Browser・Grafana へのアクセスを許可）
pub async fn auth_admin(_: AdminUser) -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn ws(State(st): State<Arc<AppState>>, ActiveUser(u): ActiveUser, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| crate::client::run(st, u, socket))
}
