//! Macross.yaml の読み込み

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerCfg,
    pub limits: LimitsCfg,
    pub channel: ChannelCfg,
    pub audio: AudioCfg,
    pub recording: RecordingCfg,
    pub janus: JanusCfg,
    pub external_api: ExternalApiCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerCfg {
    /// 待ち受けアドレス
    pub listen: String,
    /// ビルド済みフロントエンド（web/dist）の場所
    pub web_dir: PathBuf,
    /// users.csv / credentials.json / settings.json の保存先
    pub data_dir: PathBuf,
    /// HTTPS 配下で運用する場合は true（Cookie に Secure 属性を付ける）
    pub cookie_secure: bool,
    /// Prometheus 用 /metrics の待ち受けアドレス（空なら無効）。外部に公開しないこと
    pub metrics_listen: String,
}

impl Default for ServerCfg {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".into(),
            web_dir: "./web/dist".into(),
            data_dir: "./data".into(),
            cookie_secure: false,
            metrics_listen: "127.0.0.1:9180".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LimitsCfg {
    /// 最大同時接続数
    pub max_connections: usize,
}

impl Default for LimitsCfg {
    fn default() -> Self {
        Self { max_connections: 30 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ChannelCfg {
    /// Janus AudioBridge のルーム番号
    pub room_id: u64,
    /// チャンネル名（録音フォルダ名にも使う）
    pub name: String,
    /// 発言枠の初期値（管理画面から変更すると data/settings.json が優先される）
    pub max_speakers: usize,
}

impl Default for ChannelCfg {
    fn default() -> Self {
        Self { room_id: 1001, name: "ch1".into(), max_speakers: 1 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AudioCfg {
    pub sampling_rate: u32,
    /// Opus ビットレート (bps)
    pub bitrate: u32,
}

impl Default for AudioCfg {
    fn default() -> Self {
        Self { sampling_rate: 48000, bitrate: 32000 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RecordingCfg {
    /// MP3 の保存先
    pub dir: PathBuf,
    /// Janus が押下ごとの生データ (.mjr) を書き出す作業フォルダ。
    /// 省略時は `<dir>/.mjr`。File Browser に見せないよう録音先の外に置くのを推奨
    pub work_dir: Option<PathBuf>,
    /// 保持日数（既定 90 日 ≒ 3ヶ月）
    pub retention_days: u32,
    pub mp3_bitrate: String,
    pub janus_pp_rec: String,
    pub ffmpeg: String,
}

impl Default for RecordingCfg {
    fn default() -> Self {
        Self {
            dir: "./recordings".into(),
            work_dir: None,
            retention_days: 90,
            mp3_bitrate: "64k".into(),
            janus_pp_rec: "janus-pp-rec".into(),
            ffmpeg: "ffmpeg".into(),
        }
    }
}

impl RecordingCfg {
    /// Janus が押下ごとの生データ (.mjr) を書き出す作業フォルダ
    pub fn mjr_dir(&self) -> PathBuf {
        self.work_dir.clone().unwrap_or_else(|| self.dir.join(".mjr"))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JanusCfg {
    /// Janus HTTP API の URL
    pub url: String,
    /// サーバのグローバル IP（Janus の nat_1_1_mapping）
    pub public_ip: String,
    /// WebRTC 用 UDP ポート範囲
    pub rtp_port_range: String,
    /// ブラウザに渡す ICE サーバ
    pub ice_servers: Vec<IceServer>,
}

impl Default for JanusCfg {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8088/janus".into(),
            public_ip: String::new(),
            rtp_port_range: "20000-20200".into(),
            ice_servers: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IceServer {
    pub urls: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ExternalApiCfg {
    /// 外部 API（文字起こし等）。α版では未実装、false 固定で運用
    pub enabled: bool,
}

pub fn load(path: &Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("設定ファイルを読み込めません: {}", path.display()))?;
    let mut cfg: Config = serde_yaml::from_str(&text)
        .with_context(|| format!("設定ファイルの形式が不正です: {}", path.display()))?;
    if let Ok(dir) = std::env::var("MACROSS_WEB_DIR") {
        cfg.server.web_dir = dir.into();
    }
    if cfg.channel.max_speakers == 0 {
        cfg.channel.max_speakers = 1;
    }
    Ok(cfg)
}
