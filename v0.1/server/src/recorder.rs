//! 押下ごとの録音を MP3 に変換・保存し、保持期間を過ぎたものを削除する
//!
//! Janus AudioBridge が発言者ごとに書き出した .mjr を
//! janus-pp-rec で .opus に、ffmpeg で .mp3 に変換する。

use crate::config::{ChannelCfg, RecordingCfg};
use crate::metrics::Metrics;
use std::sync::Arc;
use crate::radio::{now_jst, Talk};
use anyhow::{bail, Context};
use chrono::{DateTime, FixedOffset};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct Job {
    pub talk: Talk,
    pub end: DateTime<FixedOffset>,
}

#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::UnboundedSender<Job>,
}

impl Recorder {
    pub fn start(rec: RecordingCfg, channel: ChannelCfg, metrics: Arc<Metrics>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let rec2 = rec.clone();
        let ch2 = channel.name.clone();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let ms = (job.end - job.talk.start).num_milliseconds().max(0) as u64;
                metrics.talk_ms_total.fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
                match convert(&rec2, &ch2, &job).await {
                    Ok(()) => Metrics::inc(&metrics.recordings_saved_total),
                    Err(e) => {
                        Metrics::inc(&metrics.recordings_failed_total);
                        tracing::error!("録音の変換に失敗しました ({}): {e:#}", job.talk.mjr_base);
                    }
                }
            }
        });
        tokio::spawn(async move {
            loop {
                if let Err(e) = cleanup(&rec, &channel.name) {
                    tracing::warn!("古い録音の削除に失敗しました: {e:#}");
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
        Self { tx }
    }

    pub fn submit(&self, talk: Talk) {
        let _ = self.tx.send(Job { talk, end: now_jst() });
    }
}

async fn convert(rec: &RecordingCfg, channel: &str, job: &Job) -> anyhow::Result<()> {
    // Janus がファイルを閉じるのを待つ
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mjr_dir = rec.mjr_dir();
    let mjr = find_mjr(&mjr_dir, &job.talk.mjr_base)?;

    let out_dir = rec.dir.join(channel);
    std::fs::create_dir_all(&out_dir)?;
    let out = unique_path(&out_dir, &job.talk.start, &job.talk.user_id);
    let opus = mjr_dir.join(format!("{}.opus", job.talk.mjr_base));

    run(Command::new(&rec.janus_pp_rec).arg(&mjr).arg(&opus)).await.context("janus-pp-rec")?;
    run(Command::new(&rec.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&opus)
        .args(["-vn", "-codec:a", "libmp3lame", "-b:a", &rec.mp3_bitrate])
        .arg(&out))
    .await
    .context("ffmpeg")?;

    let _ = std::fs::remove_file(&mjr);
    let _ = std::fs::remove_file(&opus);
    append_index(&out_dir, job, &out)?;
    tracing::info!("録音を保存しました: {}", out.display());
    Ok(())
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

/// `yyyy-mm-dd_HHMM_<ID>.mp3`。同じ分に同じ人が複数回押した場合は `_2`, `_3` … を付ける
fn unique_path(dir: &Path, start: &DateTime<FixedOffset>, user_id: &str) -> PathBuf {
    let stem = format!("{}_{}", start.format("%Y-%m-%d_%H%M"), user_id);
    let first = dir.join(format!("{stem}.mp3"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}_{n}.mp3")))
        .find(|p| !p.exists())
        .unwrap()
}

/// 録音ごとのタイムスタンプを index.csv に追記
fn append_index(dir: &Path, job: &Job, file: &Path) -> anyhow::Result<()> {
    let path = dir.join("index.csv");
    let new = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if new {
        writeln!(f, "開始,終了,ID,名前,ファイル")?;
    }
    writeln!(
        f,
        "{},{},{},{},{}",
        job.talk.start.format("%Y-%m-%d %H:%M:%S"),
        job.end.format("%Y-%m-%d %H:%M:%S"),
        job.talk.user_id,
        job.talk.name.replace(',', " "),
        file.file_name().and_then(|n| n.to_str()).unwrap_or_default()
    )?;
    Ok(())
}

/// 保持期間を過ぎた MP3 と、取り残された作業ファイルを削除
fn cleanup(rec: &RecordingCfg, channel: &str) -> anyhow::Result<()> {
    let now = SystemTime::now();
    let keep = Duration::from_secs(rec.retention_days as u64 * 86400);
    let dir = rec.dir.join(channel);
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp3") {
                continue;
            }
            let modified = std::fs::metadata(&path)?.modified()?;
            if now.duration_since(modified).unwrap_or_default() > keep {
                std::fs::remove_file(&path)?;
                tracing::info!("保持期間切れの録音を削除しました: {}", path.display());
            }
        }
    }
    let mjr_dir = rec.mjr_dir();
    if mjr_dir.exists() {
        for entry in std::fs::read_dir(&mjr_dir)? {
            let path = entry?.path();
            let modified = std::fs::metadata(&path)?.modified()?;
            if now.duration_since(modified).unwrap_or_default() > Duration::from_secs(86400) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(())
}
