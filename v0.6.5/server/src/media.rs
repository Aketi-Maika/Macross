//! 音声を WebSocket で運ぶとき（`audio.transport: websocket`）の中継（v0.6.5）
//!
//! ブラウザ ⇔ (WebSocket のバイナリ) ⇔ Rust ⇔ (plain RTP / Opus, 127.0.0.1) ⇔ Janus AudioBridge
//!
//! UDP を外に開けられない環境（Kubernetes の HTTPRoute だけで公開する場合など）でも、
//! 音声が HTTPS の WebSocket だけで届く。ミックス・録音・読み上げは今までどおり Janus が行う。
//!
//! WebSocket のバイナリフレーム（両方向とも同じ形）:
//!   1 バイト目 = 受聴口（0 = L, 1 = R）、続いて 16kHz・20ms（320 サンプル）の G.711 μ-law 320 バイト
//! ブラウザ → サーバは発言中（PTT が許可されている間）だけ、サーバ → ブラウザは無音のフレームを送らない。

use crate::radio::Side;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// ブラウザとの間のサンプリングレート
pub const RATE: u32 = 16_000;
/// 1 フレーム（20ms）のサンプル数
pub const FRAME: usize = 320;
/// Janus に送る Opus の RTP ペイロードタイプ
pub const PAYLOAD_TYPE: u8 = 111;
/// これ未満の振幅しかないフレームは無音とみなしてブラウザに送らない
const SILENCE: u16 = 16;

/// libopus のエンコーダ・デコーダは 1 つのタスクだけが使う（スレッド間で共有しない）ので、
/// タスクがスレッドを移っても問題ない
struct SendBox<T>(T);
unsafe impl<T> Send for SendBox<T> {}

/// Janus と plain RTP をやりとりする UDP ソケット（127.0.0.1 の空いているポート）
pub async fn bind() -> anyhow::Result<(Arc<UdpSocket>, u16)> {
    let sock = UdpSocket::bind("127.0.0.1:0").await?;
    let port = sock.local_addr()?.port();
    Ok((Arc::new(sock), port))
}

/// 受聴口 1 つぶんの中継。drop すると止まる
pub struct Media {
    up_tx: mpsc::Sender<Vec<u8>>,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Media {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Media {
    /// `janus` は AudioBridge が音声を受け付けるアドレス（join の応答の rtp.port）。
    /// `out` にはブラウザへ送るバイナリフレームを入れる
    pub fn start(
        side: Side,
        sock: Arc<UdpSocket>,
        janus: SocketAddr,
        bitrate: u32,
        out: mpsc::Sender<Vec<u8>>,
    ) -> anyhow::Result<Self> {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
        enc.set_bitrate(opus::Bitrate::Bits(bitrate as i32))?;
        let dec = opus::Decoder::new(RATE, opus::Channels::Mono)?;
        let (up_tx, up_rx) = mpsc::channel(25);
        let up = tokio::spawn(uplink(sock.clone(), janus, SendBox(enc), up_rx));
        let down = tokio::spawn(downlink(side, sock, SendBox(dec), out));
        Ok(Self { up_tx, tasks: [up, down] })
    }

    /// ブラウザから届いた μ-law 1 フレーム（320 バイト）。詰まっていたら捨てる
    pub fn push(&self, ulaw: &[u8]) {
        if ulaw.len() == FRAME {
            let _ = self.up_tx.try_send(ulaw.to_vec());
        }
    }
}

/// ブラウザのマイク → Opus → RTP → Janus
async fn uplink(sock: Arc<UdpSocket>, janus: SocketAddr, mut enc: SendBox<opus::Encoder>, mut rx: mpsc::Receiver<Vec<u8>>) {
    let ssrc: u32 = rand::random();
    let mut seq: u16 = rand::random();
    let mut ts: u32 = rand::random();
    let mut last: Option<Instant> = None;
    let mut pcm = [0i16; FRAME];
    let mut buf = [0u8; 1500];
    while let Some(frame) = rx.recv().await {
        for (o, b) in pcm.iter_mut().zip(&frame) {
            *o = ulaw_decode(*b);
        }
        // Opus の RTP タイムスタンプは 48kHz。話し始め（前のフレームから間が空いた）には
        // 空いた時間ぶん進めて marker を立てる
        let now = Instant::now();
        let marker = match last {
            Some(t) if now.duration_since(t) < Duration::from_millis(200) => {
                ts = ts.wrapping_add(960);
                false
            }
            Some(t) => {
                let gap = u32::try_from(now.duration_since(t).as_millis()).unwrap_or(u32::MAX / 48);
                ts = ts.wrapping_add(gap.saturating_mul(48));
                true
            }
            None => true,
        };
        last = Some(now);
        let n = match enc.0.encode(&pcm, &mut buf[12..]) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("Opus エンコード失敗: {e}");
                continue;
            }
        };
        buf[0] = 0x80;
        buf[1] = PAYLOAD_TYPE | if marker { 0x80 } else { 0 };
        buf[2..4].copy_from_slice(&seq.to_be_bytes());
        buf[4..8].copy_from_slice(&ts.to_be_bytes());
        buf[8..12].copy_from_slice(&ssrc.to_be_bytes());
        seq = seq.wrapping_add(1);
        if let Err(e) = sock.send_to(&buf[..12 + n], janus).await {
            tracing::debug!("Janus への RTP 送信失敗: {e}");
        }
    }
}

/// Janus のミックス → RTP → Opus を復号 → μ-law → ブラウザ
async fn downlink(side: Side, sock: Arc<UdpSocket>, mut dec: SendBox<opus::Decoder>, out: mpsc::Sender<Vec<u8>>) {
    let tag = side.idx() as u8;
    let mut buf = [0u8; 2048];
    // Opus の 1 パケットは最大 120ms（16kHz で 1920 サンプル）
    let mut pcm = vec![0i16; 1920];
    loop {
        let n = match sock.recv(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Janus からの RTP 受信失敗: {e}");
                return;
            }
        };
        let Some(payload) = rtp_payload(&buf[..n]) else { continue };
        if payload.is_empty() {
            continue;
        }
        let samples = match dec.0.decode(payload, &mut pcm, false) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Opus デコード失敗: {e}");
                continue;
            }
        };
        for chunk in pcm[..samples].chunks(FRAME) {
            if chunk.iter().all(|s| s.unsigned_abs() < SILENCE) {
                continue;
            }
            let mut f = Vec::with_capacity(1 + FRAME);
            f.push(tag);
            f.extend(chunk.iter().map(|s| ulaw_encode(*s)));
            f.resize(1 + FRAME, ulaw_encode(0));
            // ブラウザが受け取りきれないときは古いものを溜めずに捨てる（遅れて聞こえるより途切れる方がよい）
            let _ = out.try_send(f);
        }
    }
}

/// RTP パケットからペイロードを取り出す（CSRC・拡張ヘッダ・パディングを読み飛ばす）
fn rtp_payload(p: &[u8]) -> Option<&[u8]> {
    if p.len() < 12 || p[0] >> 6 != 2 {
        return None;
    }
    let cc = (p[0] & 0x0f) as usize;
    let mut off = 12 + cc * 4;
    if p[0] & 0x10 != 0 {
        if p.len() < off + 4 {
            return None;
        }
        let words = u16::from_be_bytes([p[off + 2], p[off + 3]]) as usize;
        off += 4 + words * 4;
    }
    let mut end = p.len();
    if p[0] & 0x20 != 0 {
        let pad = *p.last()? as usize;
        end = end.checked_sub(pad)?;
    }
    if off > end {
        return None;
    }
    Some(&p[off..end])
}

/// G.711 μ-law 符号化
pub fn ulaw_encode(s: i16) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32635;
    let mut x = s as i32;
    let sign = if x < 0 {
        x = -x;
        0x80
    } else {
        0
    };
    x = x.min(CLIP) + BIAS;
    let high = 31 - (x as u32).leading_zeros() as i32;
    let exp = (high - 7).clamp(0, 7);
    let mantissa = (x >> (exp + 3)) & 0x0f;
    !((sign | (exp << 4) | mantissa) as u8)
}

/// G.711 μ-law 復号
pub fn ulaw_decode(u: u8) -> i16 {
    let u = !u;
    let exp = ((u >> 4) & 0x07) as i32;
    let mantissa = (u & 0x0f) as i32;
    let x = ((((mantissa << 3) + 0x84) << exp) - 0x84) as i16;
    if u & 0x80 != 0 {
        -x
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_roundtrip_is_close() {
        for s in (-32000i32..32000).step_by(37) {
            let s = s as i16;
            let d = ulaw_decode(ulaw_encode(s));
            let err = (d as i32 - s as i32).abs();
            // μ-law の量子化誤差は振幅のおよそ 1/16 以内
            assert!(err <= (s as i32).abs() / 16 + 8, "s={s} d={d}");
        }
        assert_eq!(ulaw_encode(0), 0xff);
    }

    #[test]
    fn rtp_payload_skips_header() {
        let mut p = vec![0x80, 111, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        p.extend([1, 2, 3]);
        assert_eq!(rtp_payload(&p), Some(&[1u8, 2, 3][..]));
        // 拡張ヘッダ付き（1 ワード）
        let mut q = vec![0x90, 111, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0xbe, 0xde, 0, 1, 9, 9, 9, 9];
        q.extend([4, 5]);
        assert_eq!(rtp_payload(&q), Some(&[4u8, 5][..]));
    }
}
