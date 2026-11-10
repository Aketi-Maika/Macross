//! Prometheus 向けメトリクス（/metrics, 既定 127.0.0.1:9180）

use crate::AppState;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

#[derive(Default)]
pub struct Metrics {
    pub ptt_total: AtomicU64,
    pub ptt_denied_total: AtomicU64,
    pub talk_ms_total: AtomicU64,
    pub recordings_saved_total: AtomicU64,
    pub recordings_failed_total: AtomicU64,
    pub login_success_total: AtomicU64,
    pub login_failure_total: AtomicU64,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}");
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

    let mut out = String::new();
    metric(&mut out, "macross_connected_users", "gauge", "現在接続中のユーザー数（ID の重複を除く）", snap.users);
    metric(&mut out, "macross_connections", "gauge", "現在の接続数", snap.connections);
    metric(&mut out, "macross_max_connections", "gauge", "最大同時接続数", st.cfg.limits.max_connections);
    metric(&mut out, "macross_speakers", "gauge", "発言中の人数", snap.speakers);
    metric(&mut out, "macross_max_speakers", "gauge", "発言枠", snap.max_speakers);
    metric(&mut out, "macross_ptt_total", "counter", "発言（PTT 押下）回数", m.ptt_total.load(Relaxed));
    metric(&mut out, "macross_ptt_denied_total", "counter", "発言枠が埋まっていて拒否された回数", m.ptt_denied_total.load(Relaxed));
    metric(&mut out, "macross_talk_seconds_total", "counter", "発言時間の合計（秒）", m.talk_ms_total.load(Relaxed) as f64 / 1000.0);
    metric(&mut out, "macross_recordings_saved_total", "counter", "保存した録音の数", m.recordings_saved_total.load(Relaxed));
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
