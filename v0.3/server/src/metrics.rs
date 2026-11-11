//! Prometheus 向けメトリクス（/metrics, 既定 127.0.0.1:9180）

use crate::AppState;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Metrics {
    pub ptt_total: AtomicU64,
    pub ptt_denied_total: AtomicU64,
    pub talk_ms_total: AtomicU64,
    pub tts_total: AtomicU64,
    pub tts_failed_total: AtomicU64,
    pub recordings_saved_total: AtomicU64,
    pub recordings_failed_total: AtomicU64,
    pub login_success_total: AtomicU64,
    pub login_failure_total: AtomicU64,
    /// チャンネル名 → (PTT 回数, 読み上げ回数)
    per_channel: Mutex<BTreeMap<String, (u64, u64)>>,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }

    pub fn inc_channel_ptt(&self, channel: &str) {
        self.per_channel.lock().unwrap().entry(channel.to_string()).or_default().0 += 1;
    }

    pub fn inc_channel_tts(&self, channel: &str) {
        self.per_channel.lock().unwrap().entry(channel.to_string()).or_default().1 += 1;
    }
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}");
}

fn header(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

/// ラベル値のエスケープ
fn label(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

pub async fn handler(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let m = &st.metrics;
    let snap = st.radio.snapshot();
    let rec_dir = st.cfg.recording.dir.clone();
    let rec_bytes = tokio::task::spawn_blocking(move || dir_size(&rec_dir)).await.unwrap_or(0);
    let speakers: usize = snap.channels.iter().map(|c| c.speakers).sum();

    let mut out = String::new();
    metric(&mut out, "macross_connected_users", "gauge", "現在接続中のユーザー数（ID の重複を除く）", snap.users);
    metric(&mut out, "macross_connections", "gauge", "現在の接続数", snap.connections);
    metric(&mut out, "macross_max_connections", "gauge", "最大同時接続数", st.cfg.limits.max_connections);
    metric(&mut out, "macross_channels", "gauge", "チャンネル数", snap.channels.len());
    metric(&mut out, "macross_speakers", "gauge", "発言中の人数（全チャンネル）", speakers);

    header(&mut out, "macross_channel_listeners", "gauge", "チャンネルを聴いている接続数");
    for c in &snap.channels {
        let _ = writeln!(out, "macross_channel_listeners{{channel=\"{}\"}} {}", label(&c.name), c.listeners);
    }
    header(&mut out, "macross_channel_speakers", "gauge", "チャンネルで発言中の人数");
    for c in &snap.channels {
        let _ = writeln!(out, "macross_channel_speakers{{channel=\"{}\"}} {}", label(&c.name), c.speakers);
    }
    header(&mut out, "macross_channel_max_speakers", "gauge", "チャンネルの発言枠");
    for c in &snap.channels {
        let _ = writeln!(out, "macross_channel_max_speakers{{channel=\"{}\"}} {}", label(&c.name), c.max_speakers);
    }
    {
        let per = m.per_channel.lock().unwrap();
        header(&mut out, "macross_channel_ptt_total", "counter", "チャンネルごとの発言（PTT 押下）回数");
        for (name, (ptt, _)) in per.iter() {
            let _ = writeln!(out, "macross_channel_ptt_total{{channel=\"{}\"}} {ptt}", label(name));
        }
        header(&mut out, "macross_channel_tts_total", "counter", "チャンネルごとの読み上げ回数");
        for (name, (_, tts)) in per.iter() {
            let _ = writeln!(out, "macross_channel_tts_total{{channel=\"{}\"}} {tts}", label(name));
        }
    }

    metric(&mut out, "macross_ptt_total", "counter", "発言（PTT 押下）回数", m.ptt_total.load(Relaxed));
    metric(&mut out, "macross_ptt_denied_total", "counter", "発言枠が埋まっていて拒否された回数", m.ptt_denied_total.load(Relaxed));
    metric(&mut out, "macross_talk_seconds_total", "counter", "発言時間の合計（秒）", m.talk_ms_total.load(Relaxed) as f64 / 1000.0);
    metric(&mut out, "macross_tts_total", "counter", "読み上げ回数", m.tts_total.load(Relaxed));
    metric(&mut out, "macross_tts_failed_total", "counter", "読み上げに失敗した回数", m.tts_failed_total.load(Relaxed));
    metric(&mut out, "macross_recordings_saved_total", "counter", "保存した録音の数（読み上げを含む）", m.recordings_saved_total.load(Relaxed));
    metric(&mut out, "macross_recordings_failed_total", "counter", "変換に失敗した録音の数", m.recordings_failed_total.load(Relaxed));
    metric(&mut out, "macross_recordings_bytes", "gauge", "録音フォルダの使用量（バイト）", rec_bytes);
    let _ = writeln!(
        out,
        "# HELP macross_logins_total ログイン試行回数\n# TYPE macross_logins_total counter\nmacross_logins_total{{result=\"success\"}} {}\nmacross_logins_total{{result=\"failure\"}} {}",
        m.login_success_total.load(Relaxed),
        m.login_failure_total.load(Relaxed)
    );
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], out)
}
