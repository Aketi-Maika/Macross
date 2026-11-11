//! REST API とログインセッション

use crate::config::IceServer;
use crate::mail::{self, MailUpdate, MailView};
use crate::metrics::Metrics;
use crate::users;
use crate::radio::{ChannelDef, MAX_CHANNELS};
use crate::tts::Voice;
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
    ice_servers: Vec<IceServer>,
    tts: bool,
    tts_max_chars: usize,
}

pub async fn config(State(st): State<Arc<AppState>>) -> Json<PublicConfig> {
    Json(PublicConfig {
        ice_servers: st.cfg.janus.ice_servers.clone(),
        tts: st.tts.available(),
        tts_max_chars: st.tts.max_chars(),
    })
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

/// 自分の読み上げの声
pub async fn my_voice(State(st): State<Arc<AppState>>, ActiveUser(u): ActiveUser) -> Json<Option<Voice>> {
    Json(st.tts.voice_for(&u.id))
}

pub async fn list_channels(State(st): State<Arc<AppState>>, _: AdminUser) -> Json<serde_json::Value> {
    let channels: Vec<_> = st
        .radio
        .channels()
        .into_iter()
        .map(|c| json!({ "id": c.id, "name": c.name, "max_speakers": c.max_speakers, "room": st.radio.room_of(c.id) }))
        .collect();
    Json(json!({
        "channels": channels,
        "max_channels": MAX_CHANNELS,
        "max_connections": st.cfg.limits.max_connections,
    }))
}

#[derive(Deserialize)]
pub struct CreateChannelReq {
    name: String,
    #[serde(default = "one")]
    max_speakers: usize,
}

fn one() -> usize {
    1
}

pub async fn create_channel(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<CreateChannelReq>,
) -> ApiResult<Json<ChannelDef>> {
    let def = st.radio.create_channel(&req.name, req.max_speakers)?;
    tracing::info!("チャンネル {} ({}) を作成 (by {})", def.name, def.id, admin.id);
    // Janus のルームは参加時にも作るので、ここでの失敗は警告のみ
    let room = st.radio.room_of(def.id);
    if let Err(e) = st.janus.ensure_room(room, &def.name, st.cfg.audio.sampling_rate).await {
        tracing::warn!("ルーム {room} の作成に失敗: {e:#}");
    }
    Ok(Json(def))
}

#[derive(Deserialize)]
pub struct UpdateChannelReq {
    name: Option<String>,
    max_speakers: Option<usize>,
}

pub async fn update_channel(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Path(id): Path<u32>,
    Json(req): Json<UpdateChannelReq>,
) -> ApiResult<Json<ChannelDef>> {
    let def = st.radio.update_channel(id, req.name.as_deref(), req.max_speakers)?;
    tracing::info!("チャンネル {id} を変更 name={:?} max_speakers={:?} (by {})", req.name, req.max_speakers, admin.id);
    Ok(Json(def))
}

pub async fn delete_channel(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Path(id): Path<u32>,
) -> ApiResult<StatusCode> {
    let def = st.radio.delete_channel(id)?;
    tracing::info!("チャンネル {} ({id}) を削除 (by {})", def.name, admin.id);
    // ルームを破棄すると参加者に destroyed イベントが届き、各接続が受聴口を外す
    let room = st.radio.room_of(id);
    if let Err(e) = st.janus.destroy_room(room).await {
        tracing::warn!("ルーム {room} の削除に失敗: {e:#}");
    }
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
    /// 初期パスワードをメールで送る
    #[serde(default)]
    send_mail: bool,
}

/// 初期パスワードのメールを送り、結果を "sent" / "skipped" / エラー文で返す
async fn mail_password(st: &AppState, id: &str, name: &str, email: &str, password: &str, reset: bool, send: bool) -> String {
    if !send {
        return "skipped".into();
    }
    if email.trim().is_empty() {
        return "メールアドレスが未登録のため送信していません".into();
    }
    let (subject, body) = mail::initial_password_mail(name, id, password, &mail::login_url(&st.cfg.server.domain), reset);
    match st.mailer.send(email, &subject, body).await {
        Ok(()) => {
            tracing::info!("ID {id} に初期パスワードをメールで送信しました");
            "sent".into()
        }
        Err(e) => {
            tracing::warn!("ID {id} へのメール送信に失敗: {e:#}");
            format!("{e}")
        }
    }
}

pub async fn create_user(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<CreateUserReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let (id, password) = st.users.create_user(&req.name, &req.furigana, &req.email)?;
    tracing::info!("ID {id} を発行 (by {})", admin.id);
    let mail = mail_password(&st, &id, req.name.trim(), &req.email, &password, false, req.send_mail).await;
    Ok(Json(json!({ "id": id, "initial_password": password, "mail": mail })))
}

/// 一括登録用 CSV のテンプレート（Excel で開けるよう BOM 付き UTF-8）
pub async fn users_template(_: AdminUser) -> Response {
    let body = format!(
        "\u{feff}{}\r\n{}山田 太郎,やまだ たろう,taro@example.com\r\n",
        users::IMPORT_HEADER.join(","),
        users::IMPORT_EXAMPLE_PREFIX
    );
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"macross_users_template.csv\""),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct ImportReq {
    csv: String,
    #[serde(default)]
    send_mail: bool,
}

/// CSV で一括登録。全行を検証してから登録する
pub async fn import_users(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<ImportReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = users::parse_import(&req.csv)?;
    let mut out = vec![];
    for r in rows {
        let (id, password) = st.users.create_user(&r.name, &r.furigana, &r.email)?;
        let mail = mail_password(&st, &id, &r.name, &r.email, &password, false, req.send_mail).await;
        out.push(json!({ "id": id, "name": r.name, "email": r.email, "initial_password": password, "mail": mail }));
    }
    tracing::info!("CSV で {} 人を登録 (by {})", out.len(), admin.id);
    Ok(Json(json!({ "users": out })))
}

pub async fn get_mail(State(st): State<Arc<AppState>>, _: AdminUser) -> Json<MailView> {
    Json(st.mailer.view())
}

pub async fn put_mail(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<MailUpdate>,
) -> ApiResult<StatusCode> {
    st.mailer.update(req)?;
    tracing::info!("メール設定を変更 (by {})", admin.id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct MailTestReq {
    to: String,
}

pub async fn test_mail(
    State(st): State<Arc<AppState>>,
    _: AdminUser,
    Json(req): Json<MailTestReq>,
) -> ApiResult<StatusCode> {
    let body = format!(
        "Macross からのテストメールです。\nこのメールが届いていれば、メール設定は正しく動作しています。\n\nURL: {}\n",
        mail::login_url(&st.cfg.server.domain)
    );
    st.mailer.send(&req.to, "【Macross】テストメール", body).await?;
    Ok(StatusCode::NO_CONTENT)
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

/// パスワードリセット。新しい初期パスワードを発行し、そのユーザのログインセッションを破棄する
#[derive(Deserialize, Default)]
pub struct ResetReq {
    #[serde(default)]
    send_mail: bool,
}

pub async fn reset_password(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Path(id): Path<String>,
    req: Option<Json<ResetReq>>,
) -> ApiResult<Json<serde_json::Value>> {
    let req = req.map(|j| j.0).unwrap_or_default();
    if id == admin.id {
        return Err(ApiError::bad("自分のパスワードは「パスワード変更」から変更してください"));
    }
    let password = st.users.reset_password(&id)?;
    st.sessions.lock().unwrap().retain(|_, uid| *uid != id);
    tracing::info!("ID {id} のパスワードをリセット (by {})", admin.id);
    let (name, email) = st.users.contact(&id).unwrap_or_default();
    let mail = mail_password(&st, &id, &name, &email, &password, true, req.send_mail).await;
    Ok(Json(json!({ "id": id, "initial_password": password, "mail": mail })))
}

/// Caddy の forward_auth 用。admin でログインしていれば 204（File Browser・Grafana へのアクセスを許可）
pub async fn auth_admin(_: AdminUser) -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn ws(State(st): State<Arc<AppState>>, ActiveUser(u): ActiveUser, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| crate::client::run(st, u, socket))
}
