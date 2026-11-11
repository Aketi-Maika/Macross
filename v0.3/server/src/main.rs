//! Macross v0.3 サーバ（9チャンネル・LR受聴・TTS）
//!
//! 使い方: macross [--config Macross.yaml]

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

    std::fs::create_dir_all(&cfg.server.data_dir)?;
    for dir in [cfg.recording.dir.clone(), cfg.recording.mjr_dir()] {
        std::fs::create_dir_all(&dir).with_context(|| format!("フォルダを作成できません: {}", dir.display()))?;
    }

    let (users, bootstrap) = users::UserStore::open(&cfg.server.data_dir)?;
    if let Some((id, pw)) = bootstrap {
        tracing::warn!("初期管理者を作成しました ID={id} 初期パスワード={pw}（初回ログイン時に変更してください）");
    }

    let initial: Vec<(String, usize)> =
        cfg.channels.initial.iter().map(|c| (c.name.clone(), c.max_speakers)).collect();
    let radio = radio::Radio::new(
        cfg.channels.room_base,
        cfg.limits.max_connections,
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
    let state = Arc::new(AppState {
        cfg,
        users,
        sessions: Mutex::new(HashMap::new()),
        radio,
        janus,
        recorder,
        tts,
        mailer: mail::Mailer::open(data_dir.join("mail.json")),
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
        .route("/api/auth/admin", get(api::auth_admin))
        .route("/ws", get(api::ws))
        .fallback_service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(web_dir.join("index.html"))))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("{listen} で待ち受けできません"))?;
    tracing::info!("Macross v0.3 を起動しました: http://{listen}");
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

fn config_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == "--config" || a == "-c")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(|| std::env::var("MACROSS_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("Macross.yaml"))
}
