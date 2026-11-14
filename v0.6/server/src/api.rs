//! REST API とログインセッション

use crate::config::IceServer;
use crate::mail::{self, MailUpdate, MailView};
use crate::metrics::Metrics;
use crate::users;
use crate::radio::{ChannelDef, MAX_CHANNELS};
use crate::tts::Voice;
use crate::users::{random_token, AuthUser, LoginError, UserView, SUPERUSER_ID};
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
        let uid = st.sessions.get(&token).ok_or_else(ApiError::unauthorized)?;
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
    /// 位置情報（GPS）を送る
    location: bool,
}

pub async fn config(State(st): State<Arc<AppState>>) -> Json<PublicConfig> {
    Json(PublicConfig {
        ice_servers: st.cfg.janus.ice_servers.clone(),
        tts: st.tts.available(),
        tts_max_chars: st.tts.max_chars(),
        location: st.cfg.location.enabled,
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
    st.sessions.insert(&token, &user.id);
    tracing::info!("ログイン: {} ({})", user.name, user.display_id);
    Ok((jar.add(session_cookie(&st, token)), Json(user)))
}

pub async fn logout(State(st): State<Arc<AppState>>, jar: CookieJar) -> (CookieJar, StatusCode) {
    if let Some(c) = jar.get(COOKIE) {
        st.sessions.remove(c.value());
    }
    (jar.remove(Cookie::build((COOKIE, "")).path("/")), StatusCode::NO_CONTENT)
}

/// ログイン Cookie（Max-Age 付き。ブラウザを閉じても・端末の画面を消しても残る）
fn session_cookie(st: &AppState, token: String) -> Cookie<'static> {
    Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(st.cfg.server.cookie_secure)
        .max_age(time::Duration::seconds(st.sessions.ttl_secs()))
        .build()
}

/// ページを開いたときに呼ばれる。期限が半分を切っていたら延長する（使っている間はログインしたまま）
pub async fn me(State(st): State<Arc<AppState>>, jar: CookieJar, CurrentUser(u): CurrentUser) -> (CookieJar, Json<AuthUser>) {
    let token = jar.get(COOKIE).map(|c| c.value().to_string());
    match token {
        Some(t) if st.sessions.touch(&t) => (jar.add(session_cookie(&st, t)), Json(u)),
        _ => (jar, Json(u)),
    }
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
    st.users.forget_channel(id)?;
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
    let (id, disp, password) = st.users.create_user(&req.name, &req.furigana, &req.email)?;
    tracing::info!("ID {}（内部 {}）を発行 (by {})", disp, id, admin.display_id);
    let mail = mail_password(&st, &disp, req.name.trim(), &req.email, &password, false, req.send_mail).await;
    Ok(Json(json!({ "id": id, "display_id": disp, "initial_password": password, "mail": mail })))
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

/// CSV で一括登録（人数の上限なし）。全行を検証してから、全員を 1 度に登録する。
/// メールは登録のあとに別タスクで順に送る（送信中にブラウザとの通信が切れても、全員に送り終える）
pub async fn import_users(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<ImportReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = users::parse_import(&req.csv)?;
    let created = st.users.create_users(&rows)?;
    tracing::info!(
        "CSV で {} 人を登録: {}〜{} (by {})",
        created.len(),
        created.first().map(|c| c.1.as_str()).unwrap_or(""),
        created.last().map(|c| c.1.as_str()).unwrap_or(""),
        admin.display_id
    );
    let send = req.send_mail;
    let st2 = st.clone();
    let list: Vec<(String, String, String, String, String)> = rows
        .into_iter()
        .zip(created)
        .map(|(r, (id, display, pw))| (id, display, r.name.trim().to_string(), r.email.trim().to_string(), pw))
        .collect();
    let task = tokio::spawn(async move {
        let mut out = vec![];
        for (id, display, name, email, password) in list {
            let mail = mail_password(&st2, &display, &name, &email, &password, false, send).await;
            out.push(json!({ "id": id, "display_id": display, "name": name, "email": email, "initial_password": password, "mail": mail }));
        }
        out
    });
    let out = task.await.map_err(|e| ApiError::from(anyhow::anyhow!("メール送信の処理が異常終了しました: {e}")))?;
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
    if st.users.is_superuser(&id) {
        return Err(ApiError::forbidden(format!("スーパーユーザー（{SUPERUSER_ID}）の権限は変更できません")));
    }
    if req.admin.is_some() && !admin.superuser {
        return Err(ApiError::forbidden(format!("admin 権限の付与・解除はスーパーユーザー（{SUPERUSER_ID}）だけができます")));
    }
    check_target(&st, &admin, &id)?;
    st.users.update_flags(&id, req.admin, req.disabled)?;
    if req.admin.is_some() {
        // admin はすべてのチャンネルを使える。権限の変更を接続中のページに反映する
        // （割り込みの強さ・発言者欄の「管理者」表示・ブラウザのメニューもすぐ切り替える）
        st.radio.set_access(&id, st.users.channels_of(&id));
        if let Some(u) = st.users.get(&id) {
            st.radio.set_role(&id, u.admin, u.superuser);
        }
    }
    if req.disabled == Some(true) {
        // 無効化したユーザのログインセッションを破棄し、無線も切断する
        st.sessions.remove_user(&id);
        st.radio.kick_user(&id, "この ID は管理者により無効化されました");
    }
    tracing::info!("ID {} の権限を変更 admin={:?} disabled={:?} (by {})", st.users.display_of(&id), req.admin, req.disabled, admin.display_id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ProfileReq {
    name: Option<String>,
    furigana: Option<String>,
    email: Option<String>,
}

/// 名前・ふりがな・メールアドレスの変更。自分自身と一般ユーザは admin が、0000 と他の admin はスーパーユーザーが変更できる
pub async fn update_profile(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Path(id): Path<String>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<Json<serde_json::Value>> {
    if id != admin.id {
        if st.users.is_superuser(&id) && !admin.superuser {
            return Err(ApiError::forbidden(format!("スーパーユーザー（{SUPERUSER_ID}）の名前・メールアドレスはスーパーユーザーだけが変更できます")));
        }
        check_target(&st, &admin, &id)?;
    }
    let before = st.users.contact(&id).map(|(_, e)| e).unwrap_or_default();
    let name = st.users.update_profile(&id, req.name.as_deref(), req.furigana.as_deref(), req.email.as_deref())?;
    st.radio.rename(&id, &name);
    let email = st.users.contact(&id).map(|(_, e)| e).unwrap_or_default();
    if email != before {
        tracing::info!("ID {id} のメールアドレスを変更: {before} → {email} (by {})", admin.id);
    }
    tracing::info!("ID {id} の名前を変更: {name} (by {})", admin.id);
    Ok(Json(serde_json::json!({ "id": id, "name": name, "email": email })))
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
    if st.users.is_superuser(&id) {
        return Err(ApiError::forbidden(format!(
            "スーパーユーザー（{SUPERUSER_ID}）のパスワードは管理画面からはリセットできません（README の「0000 のパスワードを忘れたとき」を参照）"
        )));
    }
    check_target(&st, &admin, &id)?;
    let password = st.users.reset_password(&id)?;
    st.sessions.remove_user(&id);
    st.radio.kick_user(&id, "パスワードがリセットされたため、もう一度ログインしてください");
    let disp = st.users.display_of(&id);
    tracing::info!("ID {} のパスワードをリセット (by {})", disp, admin.display_id);
    let (name, email) = st.users.contact(&id).unwrap_or_default();
    let mail = mail_password(&st, &disp, &name, &email, &password, true, req.send_mail).await;
    Ok(Json(json!({ "id": id, "display_id": disp, "initial_password": password, "mail": mail })))
}

/// 他の admin の無効化・パスワードリセットはスーパーユーザーだけができる
fn check_target(st: &AppState, actor: &AuthUser, id: &str) -> ApiResult<()> {
    if actor.superuser {
        return Ok(());
    }
    let target_admin = st.users.list().iter().any(|u| u.id == id && u.admin);
    if target_admin {
        return Err(ApiError::forbidden(format!("他の admin の操作はスーパーユーザー（{SUPERUSER_ID}）だけができます")));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct ChannelsReq {
    /// true ならすべてのチャンネル（今後追加するものも含む）
    all: bool,
    #[serde(default)]
    channels: Vec<u32>,
}

/// ユーザが使えるチャンネルを設定（admin は常にすべて）
pub async fn set_user_channels(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Path(id): Path<String>,
    Json(req): Json<ChannelsReq>,
) -> ApiResult<StatusCode> {
    if st.users.list().iter().any(|u| u.id == id && u.admin) {
        return Err(ApiError::bad("admin はすべてのチャンネルを使えます（制限するには admin を外してください）"));
    }
    let channels = if req.all {
        None
    } else {
        let exists: Vec<u32> = st.radio.channels().iter().map(|c| c.id).collect();
        if let Some(bad) = req.channels.iter().find(|c| !exists.contains(c)) {
            return Err(ApiError::bad(format!("チャンネル {bad} はありません")));
        }
        Some(req.channels.clone())
    };
    st.users.set_channels(&id, channels.clone())?;
    st.radio.set_access(&id, channels.clone());
    tracing::info!("ID {id} の使えるチャンネルを変更 {:?} (by {})", channels, admin.id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct LocationsQuery {
    /// 履歴の期間（時間）。0 = 履歴なし、省略時 24
    hours: Option<i64>,
}

/// 地図: 接続中の人の位置と、位置の記録（GNSS ログ CSV）から読んだ履歴
pub async fn locations(
    State(st): State<Arc<AppState>>,
    _: AdminUser,
    axum::extract::Query(q): axum::extract::Query<LocationsQuery>,
) -> Json<serde_json::Value> {
    let users = st.radio.positions();
    let loc = &st.cfg.location;
    let now = chrono::Utc::now().timestamp_millis();
    let max_hours = i64::from(loc.retention_days.max(1)) * 24;
    let hours = q.hours.unwrap_or(24).clamp(0, max_hours);
    let history = if hours == 0 {
        vec![]
    } else {
        let gnss = st.gnss.clone();
        let since = now - hours * 3_600_000;
        tokio::task::spawn_blocking(move || gnss.read_since(since)).await.unwrap_or_default()
    };
    Json(json!({
        "enabled": loc.enabled,
        "now": now,
        "users": users,
        "hours": hours,
        "history": history,
        "max_points": crate::gnss::MAX_POINTS,
        "dates": st.gnss.dates(),
        "geocode": st.geocoder.enabled(),
        "record_interval_secs": loc.record_interval_secs,
        "map": {
            "base_tiles": loc.base_tiles,
            "photo_tiles": loc.photo_tiles,
            "buildings_tilejson": loc.buildings_tilejson,
            "terrain_tiles": loc.terrain_tiles,
            "glyphs": loc.glyphs,
        },
    }))
}

#[derive(Deserialize)]
pub struct GeocodeQuery {
    lat: f64,
    lon: f64,
}

/// 地図で押した点の住所（国土地理院の逆ジオコーダ。取れなければ address は null）
pub async fn geocode(
    State(st): State<Arc<AppState>>,
    _: AdminUser,
    axum::extract::Query(q): axum::extract::Query<GeocodeQuery>,
) -> Json<serde_json::Value> {
    let address = st.geocoder.address(q.lat, q.lon).await;
    Json(json!({ "address": address }))
}

/// 位置の記録（GNSS ログ）の CSV をダウンロード（/api/admin/gnss/2026-11-14.csv）
pub async fn gnss_csv(State(st): State<Arc<AppState>>, _: AdminUser, Path(file): Path<String>) -> ApiResult<Response> {
    let date = file.strip_suffix(".csv").unwrap_or(&file);
    let path = st.gnss.file_of(date).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "その日の記録はありません".into()))?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| ApiError::bad(e.to_string()))?;
    let name = format!("macross_gnss_{date}.csv");
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        bytes,
    )
        .into_response())
}

/// 管理画面に出すサーバ情報（設定ファイルの場所など）
pub async fn system(State(st): State<Arc<AppState>>, _: AdminUser) -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config_path": st.config_path_display,
    }))
}

/// Caddy の forward_auth 用。admin でログインしていれば 204（File Browser・Grafana へのアクセスを許可）
#[derive(Deserialize)]
pub struct BulkReq {
    /// 内部用 ID
    ids: Vec<String>,
    /// delete / disable / enable / reset / admin_on / admin_off / channels
    action: String,
    /// reset: 新しい初期パスワードをメールで送る
    #[serde(default)]
    send_mail: bool,
    /// channels: 使えるチャンネル（None = すべて）
    #[serde(default)]
    channels: Option<Vec<u32>>,
}

/// 選んだユーザーへの一括操作。操作できない人は理由を付けて飛ばし、ほかの人には実行する
pub async fn bulk_users(
    State(st): State<Arc<AppState>>,
    AdminUser(admin): AdminUser,
    Json(req): Json<BulkReq>,
) -> ApiResult<Json<serde_json::Value>> {
    const ACTIONS: [&str; 7] = ["delete", "disable", "enable", "reset", "admin_on", "admin_off", "channels"];
    if !ACTIONS.contains(&req.action.as_str()) {
        return Err(ApiError::bad("不明な操作です"));
    }
    if req.ids.is_empty() {
        return Err(ApiError::bad("ユーザーを選んでください"));
    }
    if matches!(req.action.as_str(), "admin_on" | "admin_off") && !admin.superuser {
        return Err(ApiError::forbidden(format!("admin 権限の付与・解除はスーパーユーザー（{SUPERUSER_ID}）だけができます")));
    }
    if req.action == "channels" {
        if let Some(list) = &req.channels {
            let valid: Vec<u32> = st.radio.channels().iter().map(|c| c.id).collect();
            if list.iter().any(|c| !valid.contains(c)) {
                return Err(ApiError::bad("存在しないチャンネルが含まれています"));
            }
        }
    }
    let all = st.users.list();
    let mut results = vec![];
    let mut targets = vec![];
    let mut seen = std::collections::HashSet::new();
    for id in &req.ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(u) = all.iter().find(|u| &u.id == id) else {
            results.push(json!({ "id": id, "display_id": id, "name": "", "result": "存在しません" }));
            continue;
        };
        let skip = if u.superuser {
            Some("スーパーユーザーは対象外です")
        } else if u.id == admin.id && req.action != "enable" {
            Some("自分自身は対象外です")
        } else if u.admin && !admin.superuser {
            Some("他の admin はスーパーユーザーだけが操作できます")
        } else if req.action == "channels" && u.admin {
            Some("admin は常にすべてのチャンネルを使えます")
        } else {
            None
        };
        match skip {
            Some(why) => results.push(json!({ "id": u.id, "display_id": u.display_id, "name": u.name, "result": why })),
            None => targets.push(u.clone()),
        }
    }

    let mut mails = vec![];
    match req.action.as_str() {
        "delete" => {
            for u in &targets {
                st.sessions.remove_user(&u.id);
                st.radio.kick_user(&u.id, "この ID は管理者により削除されました");
            }
            let ids: Vec<String> = targets.iter().map(|u| u.id.clone()).collect();
            st.users.delete_users(&ids)?;
            for u in &targets {
                results.push(json!({ "id": u.id, "display_id": u.display_id, "name": u.name, "result": "ok" }));
            }
        }
        "disable" | "enable" => {
            let disable = req.action == "disable";
            for u in &targets {
                st.users.update_flags(&u.id, None, Some(disable))?;
                if disable {
                    st.sessions.remove_user(&u.id);
                    st.radio.kick_user(&u.id, "この ID は管理者により無効化されました");
                }
                results.push(json!({ "id": u.id, "display_id": u.display_id, "name": u.name, "result": "ok" }));
            }
        }
        "admin_on" | "admin_off" => {
            let on = req.action == "admin_on";
            for u in &targets {
                st.users.update_flags(&u.id, Some(on), None)?;
                st.radio.set_access(&u.id, st.users.channels_of(&u.id));
                st.radio.set_role(&u.id, on, false);
                results.push(json!({ "id": u.id, "display_id": u.display_id, "name": u.name, "result": "ok" }));
            }
        }
        "channels" => {
            for u in &targets {
                st.users.set_channels(&u.id, req.channels.clone())?;
                st.radio.set_access(&u.id, st.users.channels_of(&u.id));
                results.push(json!({ "id": u.id, "display_id": u.display_id, "name": u.name, "result": "ok" }));
            }
        }
        _ => {
            // reset: パスワードを発行してログアウトさせ、メールはあとで順に送る
            for u in &targets {
                let password = st.users.reset_password(&u.id)?;
                st.sessions.remove_user(&u.id);
                st.radio.kick_user(&u.id, "パスワードがリセットされたため、もう一度ログインしてください");
                mails.push((u.id.clone(), u.display_id.clone(), u.name.clone(), u.email.clone(), password));
            }
        }
    }
    if !mails.is_empty() {
        let st2 = st.clone();
        let send = req.send_mail;
        let task = tokio::spawn(async move {
            let mut out = vec![];
            for (id, display, name, email, password) in mails {
                let mail = mail_password(&st2, &display, &name, &email, &password, true, send).await;
                out.push(json!({ "id": id, "display_id": display, "name": name, "result": "ok", "initial_password": password, "mail": mail }));
            }
            out
        });
        results.extend(task.await.map_err(|e| ApiError::from(anyhow::anyhow!("メール送信の処理が異常終了しました: {e}")))?);
    }
    let done = results.iter().filter(|r| r["result"] == "ok").count();
    tracing::info!("一括操作 {}: {done} 人 / 選択 {} 人 (by {})", req.action, req.ids.len(), admin.display_id);
    Ok(Json(json!({ "action": req.action, "done": done, "results": results, "gaps": st.users.gaps() })))
}

/// 表示用 ID を 1, 2, 3… の連番に付け直す（削除で欠番ができたとき）。ログイン ID が変わった人の一覧を返す
pub async fn renumber_users(State(st): State<Arc<AppState>>, AdminUser(admin): AdminUser) -> ApiResult<Json<serde_json::Value>> {
    let changed = st.users.renumber()?;
    for (id, _, new) in &changed {
        st.radio.set_display(id, new);
    }
    tracing::info!("表示用 ID を詰め直し: {} 人の ID が変更 (by {})", changed.len(), admin.display_id);
    let list: Vec<_> = changed
        .iter()
        .map(|(id, old, new)| {
            let name = st.users.contact(id).map(|c| c.0).unwrap_or_default();
            json!({ "id": id, "name": name, "old": old, "new": new })
        })
        .collect();
    Ok(Json(json!({ "changed": list })))
}

pub async fn auth_admin(_: AdminUser) -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn ws(State(st): State<Arc<AppState>>, ActiveUser(u): ActiveUser, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| crate::client::run(st, u, socket))
}
