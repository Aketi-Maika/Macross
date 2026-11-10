//! Macross α サーバ
//!
//! 使い方: macross [--config Macross.yaml]

mod api;
mod client;
mod config;
mod janus;
mod metrics;
mod radio;
mod recorder;
mod users;

use anyhow::Context;
use axum::routing::{get, patch, post};
use axum::Router;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_http::services::{ServeDir, ServeFile};

pub struct AppState {
    pub cfg: config::Config,
    pub users: users::UserStore,
    /// セッショントークン → ユーザ ID（α版はメモリ保持。再起動で全員ログアウト）
    pub sessions: Mutex<HashMap<String, String>>,
    pub radio: radio::Radio,
    pub janus: janus::Janus,
    pub recorder: recorder::Recorder,
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

    std::fs::create_dir_all(&cfg.server.data_dir)?;
    for dir in [cfg.recording.dir.clone(), cfg.recording.mjr_dir()] {
        std::fs::create_dir_all(&dir).with_context(|| format!("フォルダを作成できません: {}", dir.display()))?;
    }

    let (users, bootstrap) = users::UserStore::open(&cfg.server.data_dir)?;
    if let Some((id, pw)) = bootstrap {
        tracing::warn!("初期管理者を作成しました ID={id} 初期パスワード={pw}（初回ログイン時に変更してください）");
    }

    let radio = radio::Radio::new(
        cfg.channel.name.clone(),
        cfg.limits.max_connections,
        cfg.channel.max_speakers,
        cfg.server.data_dir.join("settings.json"),
    );
    let metrics = Arc::new(metrics::Metrics::default());
    let recorder = recorder::Recorder::start(cfg.recording.clone(), cfg.channel.clone(), metrics.clone());
    let janus = janus::Janus::new(&cfg.janus.url);

    let web_dir = cfg.server.web_dir.clone();
    let listen = cfg.server.listen.clone();
    let metrics_listen = cfg.server.metrics_listen.clone();
    let state = Arc::new(AppState {
        cfg,
        users,
        sessions: Mutex::new(HashMap::new()),
        radio,
        janus,
        recorder,
        metrics,
    });

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
        .route("/api/admin/settings", get(api::get_settings).put(api::put_settings))
        .route("/api/admin/users", get(api::list_users).post(api::create_user))
        .route("/api/admin/users/:id", patch(api::update_user))
        .route("/api/auth/admin", get(api::auth_admin))
        .route("/ws", get(api::ws))
        .fallback_service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(web_dir.join("index.html"))))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("{listen} で待ち受けできません"))?;
    tracing::info!("Macross α を起動しました: http://{listen}");
    axum::serve(listener, app).await?;
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
