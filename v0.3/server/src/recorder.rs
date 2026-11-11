//! 押下ごとの録音・読み上げ音声を MP3 で保存し、保持期間を過ぎたものを削除する
//!
//! PTT: Janus AudioBridge が発言者ごとに書き出した .mjr を janus-pp-rec で .opus に、ffmpeg で .mp3 に変換する。
//! TTS: 合成した .wav を ffmpeg で .mp3 に変換する。
//! 保存先は `<recording.dir>/<チャンネル名>/`。同じフォルダの index.csv に一覧を追記する。

use crate::config::RecordingCfg;
use crate::metrics::Metrics;
use crate::radio::{now_jst, Talk};
use anyhow::{bail, Context};
use chrono::{DateTime, FixedOffset};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

/// 読み上げ 1 回分
pub struct TtsRecord {
    pub wav: PathBuf,
    pub channel_name: String,
    pub user_id: String,
    pub name: String,
    pub text: String,
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
}

enum Job {
    Ptt { talk: Talk, end: DateTime<FixedOffset> },
    Tts(TtsRecord),
}

struct Entry<'a> {
    channel: &'a str,
    user_id: &'a str,
    name: &'a str,
    kind: &'a str,
    text: &'a str,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
}

#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::UnboundedSender<Job>,
}

impl Recorder {
    pub fn start(rec: RecordingCfg, metrics: Arc<Metrics>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let rec2 = rec.clone();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let (label, result) = match &job {
                    Job::Ptt { talk, end } => {
                        let ms = (*end - talk.start).num_milliseconds().max(0) as u64;
                        metrics.talk_ms_total.fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
                        (talk.mjr_base.clone(), convert_ptt(&rec2, talk, *end).await)
                    }
                    Job::Tts(t) => (t.wav.display().to_string(), convert_tts(&rec2, t).await),
                };
                match result {
                    Ok(()) => Metrics::inc(&metrics.recordings_saved_total),
                    Err(e) => {
                        Metrics::inc(&metrics.recordings_failed_total);
                        tracing::error!("録音の変換に失敗しました ({label}): {e:#}");
                    }
                }
            }
        });
        tokio::spawn(async move {
            loop {
                if let Err(e) = cleanup(&rec) {
                    tracing::warn!("古い録音の削除に失敗しました: {e:#}");
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
        Self { tx }
    }

    pub fn submit(&self, talk: Talk) {
        let _ = self.tx.send(Job::Ptt { talk, end: now_jst() });
    }

    pub fn submit_tts(&self, rec: TtsRecord) {
        let _ = self.tx.send(Job::Tts(rec));
    }
}

async fn convert_ptt(rec: &RecordingCfg, talk: &Talk, end: DateTime<FixedOffset>) -> anyhow::Result<()> {
    // Janus がファイルを閉じるのを待つ
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mjr_dir = rec.mjr_dir();
    let mjr = find_mjr(&mjr_dir, &talk.mjr_base)?;
    let opus = mjr_dir.join(format!("{}.opus", talk.mjr_base));

    let out_dir = rec.dir.join(&talk.channel_name);
    std::fs::create_dir_all(&out_dir)?;
    let out = unique_path(&out_dir, &talk.start, &talk.user_id, "");

    run(Command::new(&rec.janus_pp_rec).arg(&mjr).arg(&opus)).await.context("janus-pp-rec")?;
    let r = to_mp3(rec, &opus, &out).await;
    let _ = std::fs::remove_file(&opus);
    r?;
    let _ = std::fs::remove_file(&mjr);

    let entry = Entry {
        channel: &talk.channel_name,
        user_id: &talk.user_id,
        name: &talk.name,
        kind: "PTT",
        text: "",
        start: talk.start,
        end,
    };
    append_index(&out_dir, &entry, &out)?;
    tracing::info!("録音を保存しました: {}", out.display());
    Ok(())
}

async fn convert_tts(rec: &RecordingCfg, t: &TtsRecord) -> anyhow::Result<()> {
    let out_dir = rec.dir.join(&t.channel_name);
    std::fs::create_dir_all(&out_dir)?;
    let out = unique_path(&out_dir, &t.start, &t.user_id, "_tts");
    let r = to_mp3(rec, &t.wav, &out).await;
    let _ = std::fs::remove_file(&t.wav);
    r?;
    let entry = Entry {
        channel: &t.channel_name,
        user_id: &t.user_id,
        name: &t.name,
        kind: "TTS",
        text: &t.text,
        start: t.start,
        end: t.end,
    };
    append_index(&out_dir, &entry, &out)?;
    tracing::info!("読み上げを保存しました: {}", out.display());
    Ok(())
}

async fn to_mp3(rec: &RecordingCfg, input: &Path, out: &Path) -> anyhow::Result<()> {
    run(Command::new(&rec.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args(["-vn", "-codec:a", "libmp3lame", "-b:a", &rec.mp3_bitrate])
        .arg(out))
    .await
    .context("ffmpeg")
}

async fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let output = cmd.output().await?;
    if !output.status.success() {
        bail!("終了コード {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn find_mjr(dir: &Path, base: &str) -> anyhow::Result<PathBuf> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("{} を読めません", dir.display()))? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with(base) && name.ends_with(".mjr") {
            return Ok(path);
        }
    }
    bail!("{} に {base}*.mjr が見つかりません", dir.display())
}

/// `yyyy-mm-dd_HHMM_<ID><suffix>.mp3`。同じ分に同じ人が複数回押した場合は `_2`, `_3` … を付ける
fn unique_path(dir: &Path, start: &DateTime<FixedOffset>, user_id: &str, suffix: &str) -> PathBuf {
    let stem = format!("{}_{}{}", start.format("%Y-%m-%d_%H%M"), user_id, suffix);
    let first = dir.join(format!("{stem}.mp3"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}_{n}.mp3")))
        .find(|p| !p.exists())
        .unwrap()
}

/// 録音ごとのタイムスタンプを index.csv に追記（Excel で開けるよう BOM 付き UTF-8）
fn append_index(dir: &Path, e: &Entry, file: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    let path = dir.join("index.csv");
    let new = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if new {
        f.write_all("\u{feff}".as_bytes())?;
    }
    let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(f);
    if new {
        w.write_record(["開始", "終了", "チャンネル", "ID", "名前", "種別", "ファイル", "読み上げテキスト"])?;
    }
    w.write_record([
        e.start.format("%Y-%m-%d %H:%M:%S").to_string().as_str(),
        e.end.format("%Y-%m-%d %H:%M:%S").to_string().as_str(),
        e.channel,
        e.user_id,
        e.name,
        e.kind,
        file.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        e.text,
    ])?;
    w.flush()?;
    Ok(())
}

/// 保持期間を過ぎた MP3（全チャンネルのフォルダ）と、取り残された作業ファイルを削除
fn cleanup(rec: &RecordingCfg) -> anyhow::Result<()> {
    let now = SystemTime::now();
    let keep = Duration::from_secs(rec.retention_days as u64 * 86400);
    let older = |path: &Path, d: Duration| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > d)
            .unwrap_or(false)
    };
    if rec.dir.exists() {
        for ch in std::fs::read_dir(&rec.dir)?.flatten() {
            let ch = ch.path();
            if !ch.is_dir() || ch == rec.mjr_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&ch)?.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("mp3") && older(&path, keep) {
                    std::fs::remove_file(&path)?;
                    tracing::info!("保持期間切れの録音を削除しました: {}", path.display());
                }
            }
        }
    }
    let work = rec.mjr_dir();
    if work.exists() {
        for entry in std::fs::read_dir(&work)?.flatten() {
            let path = entry.path();
            if path.is_file() && older(&path, Duration::from_secs(86400)) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(())
}
