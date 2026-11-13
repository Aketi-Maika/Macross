//! Macross v0.4 サーバ（9チャンネル・LR受聴・TTS・位置情報）
//!
//! 使い方: macross [--config Macross.yaml]
//!         macross [--config Macross.yaml] reset-superuser   … 0000 のパスワードをリセット（サーバ停止中に実行）

mod api;
mod client;
mod config;
mod janus;
mod mail;
mod metrics;
mod radio;
mod recorder;
mod tts;
mod users;

use anyhow::Context;
use axum::routing::{get, patch, post, put};
use axum::Router;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_http::services::{ServeDir, ServeFile};

pub struct AppState {
    pub cfg: config::Config,
    /// 管理画面に表示する設定ファイルの場所（ホスト側のパス）
    pub config_path_display: String,
    pub users: users::UserStore,
    /// セッショントークン → ユーザ ID（α版はメモリ保持。再起動で全員ログアウト）
    pub sessions: Mutex<HashMap<String, String>>,
    pub radio: radio::Radio,
    pub janus: janus::Janus,
    pub recorder: recorder::Recorder,
    pub tts: tts::Tts,
    pub mailer: mail::Mailer,
    pub metrics: Arc<metrics::Metrics>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_path = config_path_from_args();
    let cfg = config::load(&config_path)?;
    tracing::info!("設定ファイル: {}", config_path.display());
    // コンテナ内のパスではなく、ホスト側で編集するファイルの場所を表示する（docker-compose.yml で指定）
    let config_path_display =
        std::env::var("MACROSS_CONFIG_HOST").unwrap_or_else(|_| config_path.display().to_string());

    if std::env::args().any(|a| a == "reset-superuser") {
        return reset_superuser(&cfg);
    }

    std::fs::create_dir_all(&cfg.server.data_dir)?;
    for dir in [cfg.recording.dir.clone(), cfg.recording.mjr_dir()] {
        std::fs::create_dir_all(&dir).with_context(|| format!("フォルダを作成できません: {}", dir.display()))?;
    }

    let (users, bootstrap) = users::UserStore::open(&cfg.server.data_dir, &cfg.superuser)?;
    if let Some((id, pw)) = bootstrap {
        tracing::warn!("スーパーユーザーを作成しました ID={id} 初期パスワード={pw}（初回ログイン時に変更してください）");
    }

    let initial: Vec<(String, usize)> =
        cfg.channels.initial.iter().map(|c| (c.name.clone(), c.max_speakers)).collect();
    let radio = radio::Radio::new(
        cfg.channels.room_base,
        cfg.limits.max_connections,
        cfg.location.history_max,
        cfg.server.data_dir.join("channels.json"),
        &initial,
    )?;
    let metrics = Arc::new(metrics::Metrics::default());
    let recorder = recorder::Recorder::start(cfg.recording.clone(), metrics.clone());
    let (tts, tts_rx) = tts::Tts::new(cfg.tts.clone());
    let janus = janus::Janus::new(&cfg.janus.url);

    let web_dir = cfg.server.web_dir.clone();
    let data_dir = cfg.server.data_dir.clone();
    let listen = cfg.server.listen.clone();
    let metrics_listen = cfg.server.metrics_listen.clone();
    let mailer = mail::Mailer::open(data_dir.join("mail.json"), &cfg.mail.default_from, &cfg.mail.default_from_name);
    let state = Arc::new(AppState {
        cfg,
        config_path_display,
        users,
        sessions: Mutex::new(HashMap::new()),
        radio,
        janus,
        recorder,
        tts,
        mailer,
        metrics,
    });
    tts::spawn_worker(state.clone(), tts_rx);
    spawn_room_setup(state.clone());

    if !metrics_listen.is_empty() {
        let app = Router::new().route("/metrics", get(metrics::handler)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(&metrics_listen)
            .await
            .with_context(|| format!("{metrics_listen} で待ち受けできません（metrics）"))?;
        tracing::info!("メトリクス: http://{metrics_listen}/metrics");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("メトリクスサーバが停止しました: {e}");
            }
        });
    }

    let app = Router::new()
        .route("/api/config", get(api::config))
        .route("/api/login", post(api::login))
        .route("/api/logout", post(api::logout))
        .route("/api/me", get(api::me))
        .route("/api/password", post(api::change_password))
        .route("/api/tts/voice", get(api::my_voice))
        .route("/api/admin/channels", get(api::list_channels).post(api::create_channel))
        .route("/api/admin/channels/:id", patch(api::update_channel).delete(api::delete_channel))
        .route("/api/admin/users", get(api::list_users).post(api::create_user))
        .route("/api/admin/users/template.csv", get(api::users_template))
        .route("/api/admin/users/import", post(api::import_users))
        .route("/api/admin/mail", get(api::get_mail).put(api::put_mail))
        .route("/api/admin/mail/test", post(api::test_mail))
        .route("/api/admin/users/:id", patch(api::update_user))
        .route("/api/admin/users/:id/reset-password", post(api::reset_password))
        .route("/api/admin/users/:id/channels", put(api::set_user_channels))
        .route("/api/admin/locations", get(api::locations))
        .route("/api/admin/system", get(api::system))
        .route("/api/auth/admin", get(api::auth_admin))
        .route("/ws", get(api::ws))
        .fallback_service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(web_dir.join("index.html"))))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("{listen} で待ち受けできません"))?;
    tracing::info!("Macross v0.4 を起動しました: http://{listen}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 起動時に全チャンネルの AudioBridge ルームを作る（Janus の起動を待って再試行）
fn spawn_room_setup(st: Arc<AppState>) {
    tokio::spawn(async move {
        for attempt in 1.. {
            let mut failed = None;
            for c in st.radio.channels() {
                let room = st.radio.room_of(c.id);
                if let Err(e) = st.janus.ensure_room(room, &c.name, st.cfg.audio.sampling_rate).await {
                    failed = Some(e);
                    break;
                }
            }
            match failed {
                None => {
                    tracing::info!("Janus のルームを準備しました（{} チャンネル）", st.radio.channels().len());
                    return;
                }
                Some(e) if attempt == 1 || attempt % 20 == 0 => {
                    tracing::warn!("Janus のルームを準備できません。再試行します: {e:#}")
                }
                Some(_) => {}
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });
}

/// 0000 のパスワードを忘れたとき用。新しい初期パスワードを表示する
/// （サーバが動いているとメモリ上の古い状態で上書きされるため、停止中に実行する）
fn reset_superuser(cfg: &config::Config) -> anyhow::Result<()> {
    let (users, _) = users::UserStore::open(&cfg.server.data_dir, &cfg.superuser)?;
    let pw = users.reset_password(users::SUPERUSER_ID)?;
    println!("スーパーユーザー（{}）の新しい初期パスワード: {pw}", users::SUPERUSER_ID);
    println!("ログイン後にパスワードの変更を求められます。");
    Ok(())
}

fn config_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--config" || a == "-c")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(|| std::env::var("MACROSS_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("Macross.yaml"))
}
