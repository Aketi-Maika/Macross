//! テキスト読み上げ（Open JTalk）
//!
//! テキスト → open_jtalk で WAV → ffmpeg で Ogg Opus → Janus AudioBridge の play_file でチャンネルに流す。
//! 声は ID ごとに「音声モデル × 声の高さ × 話速 × 声質」の組み合わせから重複しないように割り当てる。
//! 読み上げた音声は録音と同じフォルダに `_tts.mp3` として保存する。

use crate::config::TtsCfg;
use crate::metrics::Metrics;
use crate::radio::{now_jst, ServerMsg};
use crate::recorder::TtsRecord;
use crate::AppState;
use anyhow::{bail, Context};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;

const QUEUE: usize = 32;
const PITCH: [f32; 11] = [-4.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
const SPEED: [f32; 5] = [0.9, 0.95, 1.0, 1.05, 1.1];
const ALPHA: [f32; 6] = [0.50, 0.52, 0.54, 0.56, 0.58, 0.60];

pub struct TtsJob {
    pub conn: u64,
    pub user_id: String,
    pub name: String,
    pub channel_id: u32,
    pub text: String,
}

/// ID ごとの声
#[derive(Debug, Clone, Serialize)]
pub struct Voice {
    /// 音声モデルのファイル名（拡張子なし）
    pub model: String,
    #[serde(skip)]
    pub model_path: PathBuf,
    /// 声の高さ（半音）
    pub pitch: f32,
    /// 話速
    pub speed: f32,
    /// 声質（all-pass 定数）
    pub alpha: f32,
}

pub struct Tts {
    cfg: TtsCfg,
    voices: Vec<PathBuf>,
    tx: mpsc::Sender<TtsJob>,
    last: Mutex<HashMap<String, std::time::Instant>>,
}

impl Tts {
    pub fn new(cfg: TtsCfg) -> (Self, mpsc::Receiver<TtsJob>) {
        let voices = find_voices(&cfg);
        if cfg.enabled {
            if voices.is_empty() {
                tracing::warn!("TTS: 音声モデル (.htsvoice) が見つかりません。読み上げは使えません");
            } else {
                tracing::info!("TTS: 音声モデル {} 個（{} 通りの声）", voices.len(), combinations(voices.len()));
            }
        }
        let (tx, rx) = mpsc::channel(QUEUE);
        (Self { cfg, voices, tx, last: Mutex::new(HashMap::new()) }, rx)
    }

    pub fn available(&self) -> bool {
        self.cfg.enabled && !self.voices.is_empty()
    }

    pub fn max_chars(&self) -> usize {
        self.cfg.max_chars
    }

    /// ID に割り当てられた声
    pub fn voice_for(&self, user_id: &str) -> Option<Voice> {
        if self.voices.is_empty() {
            return None;
        }
        let nv = self.voices.len() as u64;
        let total = combinations(self.voices.len());
        // 連番の ID が全て別の組み合わせになるよう、total と互いに素な数で並べ替える
        let n = user_id.parse::<u64>().unwrap_or_else(|_| fnv(user_id));
        let step = [97u64, 89, 83, 79, 1].into_iter().find(|s| gcd(*s, total) == 1).unwrap();
        let mut idx = (n % total) * step % total;
        let v = (idx % nv) as usize;
        idx /= nv;
        let pitch = PITCH[(idx % PITCH.len() as u64) as usize];
        idx /= PITCH.len() as u64;
        let speed = SPEED[(idx % SPEED.len() as u64) as usize];
        idx /= SPEED.len() as u64;
        let alpha = ALPHA[(idx % ALPHA.len() as u64) as usize];
        let path = self.voices[v].clone();
        Some(Voice {
            model: path.file_stem().and_then(|s| s.to_str()).unwrap_or("voice").to_string(),
            model_path: path,
            pitch,
            speed,
            alpha,
        })
    }

    /// 読み上げを受け付ける
    pub fn submit(&self, job: TtsJob) -> Result<(), String> {
        if !self.cfg.enabled {
            return Err("読み上げは無効に設定されています".into());
        }
        if self.voices.is_empty() {
            return Err("読み上げの音声モデルがサーバにありません".into());
        }
        let text = clean(&job.text);
        if text.is_empty() {
            return Err("読み上げるテキストを入力してください".into());
        }
        if text.chars().count() > self.cfg.max_chars {
            return Err(format!("読み上げは{}文字以内にしてください", self.cfg.max_chars));
        }
        {
            let mut last = self.last.lock().unwrap();
            let now = std::time::Instant::now();
            if let Some(t) = last.get(&job.user_id) {
                if now.duration_since(*t) < Duration::from_secs(self.cfg.cooldown_secs) {
                    return Err("少し待ってから送信してください".into());
                }
            }
            last.insert(job.user_id.clone(), now);
        }
        self.tx.try_send(TtsJob { text, ..job }).map_err(|_| "読み上げが混み合っています。少し待ってから送信してください".to_string())
    }
}

fn combinations(n_voices: usize) -> u64 {
    (n_voices * PITCH.len() * SPEED.len() * ALPHA.len()) as u64
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// 改行・制御文字を空白にまとめる
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_voices(cfg: &TtsCfg) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = cfg.voices.iter().filter(|p| p.is_file()).cloned().collect();
    if let Some(dir) = &cfg.voice_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut extra: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("htsvoice"))
                .collect();
            extra.sort();
            for p in extra {
                if !list.contains(&p) {
                    list.push(p);
                }
            }
        }
    }
    list
}

/// 合成 → 再生のワーカー。合成は 1 件ずつ、再生はチャンネルごとに順番待ちする
pub fn spawn_worker(st: Arc<AppState>, mut rx: mpsc::Receiver<TtsJob>) {
    let locks: Arc<Mutex<HashMap<u32, Arc<tokio::sync::Mutex<()>>>>> = Default::default();
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let Some(voice) = st.tts.voice_for(&job.user_id) else { continue };
            match synth(&st, &job, &voice).await {
                Ok(audio) => {
                    let lock = locks.lock().unwrap().entry(job.channel_id).or_default().clone();
                    tokio::spawn(play(st.clone(), job, audio, lock));
                }
                Err(e) => {
                    tracing::error!("読み上げの合成に失敗しました: {e:#}");
                    Metrics::inc(&st.metrics.tts_failed_total);
                    st.radio.send(job.conn, ServerMsg::Error { message: "読み上げの音声を作れませんでした".into() });
                }
            }
        }
    });
}

struct Audio {
    wav: PathBuf,
    opus: PathBuf,
    duration: Duration,
}

async fn synth(st: &AppState, job: &TtsJob, voice: &Voice) -> anyhow::Result<Audio> {
    let cfg = &st.cfg.tts;
    let dir = st.cfg.recording.mjr_dir();
    let base = format!("tts_{}_{}_{}", job.user_id, now_jst().timestamp_millis(), crate::users::random_token(4));
    let txt = dir.join(format!("{base}.txt"));
    let wav = dir.join(format!("{base}.wav"));
    let opus = dir.join(format!("{base}.opus"));
    std::fs::write(&txt, &job.text)?;

    let result = async {
        run(Command::new(&cfg.open_jtalk)
            .arg("-x")
            .arg(&cfg.dictionary)
            .arg("-m")
            .arg(&voice.model_path)
            .args(["-fm", &voice.pitch.to_string(), "-r", &voice.speed.to_string(), "-a", &voice.alpha.to_string()])
            .arg("-ow")
            .arg(&wav)
            .arg(&txt))
        .await
        .context("open_jtalk")?;
        let duration = wav_duration(&wav)?;
        // play_file は 48kHz の Ogg Opus を要求する
        run(Command::new(&st.cfg.recording.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&wav)
            .args(["-ac", "1", "-ar", "48000", "-c:a", "libopus", "-b:a", "48k", "-f", "ogg"])
            .arg(&opus))
        .await
        .context("ffmpeg (opus)")?;
        Ok(Audio { wav: wav.clone(), opus: opus.clone(), duration })
    }
    .await;
    let _ = std::fs::remove_file(&txt);
    if result.is_err() {
        let _ = std::fs::remove_file(&wav);
        let _ = std::fs::remove_file(&opus);
    }
    result
}

async fn play(st: Arc<AppState>, job: TtsJob, audio: Audio, lock: Arc<tokio::sync::Mutex<()>>) {
    let _guard = lock.lock().await;
    let cleanup = |a: &Audio| {
        let _ = std::fs::remove_file(&a.opus);
    };
    let Some(ch) = st.radio.channel(job.channel_id) else {
        cleanup(&audio);
        let _ = std::fs::remove_file(&audio.wav);
        return;
    };
    let room = st.radio.room_of(ch.id);
    let start = now_jst();
    if let Err(e) = st.janus.play_file(room, &audio.opus.to_string_lossy()).await {
        tracing::error!("読み上げの再生に失敗しました: {e:#}");
        Metrics::inc(&st.metrics.tts_failed_total);
        st.radio.send(job.conn, ServerMsg::Error { message: "読み上げを再生できませんでした".into() });
        cleanup(&audio);
        let _ = std::fs::remove_file(&audio.wav);
        return;
    }
    Metrics::inc(&st.metrics.tts_total);
    st.metrics.inc_channel_tts(&ch.name);
    tracing::info!("読み上げ: {} ({}) → {}: {}", job.name, job.user_id, ch.name, job.text);
    st.radio.send_to_channel(
        ch.id,
        ServerMsg::Tts {
            channel: ch.id,
            id: job.user_id.clone(),
            name: job.name.clone(),
            text: job.text.clone(),
            time: start.format("%H:%M:%S").to_string(),
        },
    );
    // 再生が終わるまで同じチャンネルの次の読み上げを待たせる
    tokio::time::sleep(audio.duration + Duration::from_millis(400)).await;
    cleanup(&audio);
    st.recorder.submit_tts(TtsRecord {
        wav: audio.wav,
        channel_name: ch.name,
        user_id: job.user_id,
        name: job.name,
        text: job.text,
        start,
        end: now_jst(),
    });
}

async fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .context("タイムアウトしました")??;
    if !output.status.success() {
        bail!("終了コード {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// WAV の再生時間
fn wav_duration(path: &Path) -> anyhow::Result<Duration> {
    let b = std::fs::read(path)?;
    anyhow::ensure!(b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE", "WAV ではありません");
    let u32le = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut pos, mut byte_rate, mut data) = (12usize, 0u32, 0u32);
    while pos + 8 <= b.len() {
        let size = u32le(pos + 4);
        match &b[pos..pos + 4] {
            b"fmt " if pos + 20 <= b.len() => byte_rate = u32le(pos + 16),
            b"data" => data = size.min((b.len() - pos - 8) as u32),
            _ => {}
        }
        pos += 8 + size as usize + (size as usize & 1);
    }
    anyhow::ensure!(byte_rate > 0, "WAV の形式を読み取れません");
    Ok(Duration::from_secs_f64(data as f64 / byte_rate as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tts(n: usize) -> Tts {
        let (mut t, _) = Tts::new(TtsCfg { voices: vec![], ..Default::default() });
        t.voices = (0..n).map(|i| PathBuf::from(format!("v{i}.htsvoice"))).collect();
        t
    }

    fn key(v: &Voice) -> String {
        format!("{} {} {} {}", v.model, v.pitch, v.speed, v.alpha)
    }

    #[test]
    fn voices_are_unique_for_sequential_ids() {
        for n in [1, 2, 3] {
            let t = tts(n);
            let total = combinations(n) as usize;
            let keys: std::collections::HashSet<String> =
                (1..=total).map(|i| key(&t.voice_for(&format!("{i:04}")).unwrap())).collect();
            assert_eq!(keys.len(), total, "voices={n}");
        }
    }

    #[test]
    fn clean_text() {
        assert_eq!(clean(" こんにちは\n\t世界 "), "こんにちは 世界");
    }
}
