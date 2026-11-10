//! ブラウザ 1 接続ぶんの WebSocket 処理
//!
//! ブラウザ ⇔ (WebSocket) ⇔ Rust ⇔ (HTTP API) ⇔ Janus AudioBridge
//! 音声そのものはブラウザと Janus の間を WebRTC (UDP) で直接流れる。

use crate::janus::transaction;
use crate::metrics::Metrics;
use crate::radio::ServerMsg;
use crate::users::AuthUser;
use crate::AppState;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// ブラウザ → サーバ
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    /// SDP offer（ICE 候補を含む。trickle なし）
    Join { sdp: String },
    Ptt { on: bool },
    Ping,
}

struct JanusConn {
    session: u64,
    handle: u64,
    poller: JoinHandle<()>,
    joined: bool,
}

struct Ctx {
    st: Arc<AppState>,
    user: AuthUser,
    conn: u64,
    janus: Option<JanusConn>,
    pending_join: Option<String>,
    jev_tx: mpsc::UnboundedSender<Value>,
}

pub async fn run(st: Arc<AppState>, user: AuthUser, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();

    let Some(conn) = st.radio.register(&user.id, &user.name, tx) else {
        let msg = ServerMsg::Error { message: "接続数の上限に達しています".into() };
        let _ = ws_tx.send(Message::Text(serde_json::to_string(&msg).unwrap())).await;
        return;
    };
    tracing::info!("接続: {} ({}) conn={conn}", user.name, user.id);
    st.radio.broadcast_state();

    let (jev_tx, mut jev_rx) = mpsc::unbounded_channel::<Value>();
    let mut ctx = Ctx { st: st.clone(), user, conn, janus: None, pending_join: None, jev_tx };
    let mut keepalive = tokio::time::interval(Duration::from_secs(25));

    loop {
        tokio::select! {
            msg = ws_rx.next() => match msg {
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMsg>(&t) {
                    Ok(m) => ctx.on_client(m).await,
                    Err(e) => tracing::debug!("不明なメッセージ: {e}"),
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            Some(out) = rx.recv() => {
                let text = serde_json::to_string(&out).unwrap();
                if ws_tx.send(Message::Text(text)).await.is_err() {
                    break;
                }
            },
            Some(ev) = jev_rx.recv() => ctx.on_janus(ev).await,
            _ = keepalive.tick() => {
                if let Some(j) = &ctx.janus {
                    if let Err(e) = ctx.st.janus.keepalive(j.session).await {
                        tracing::warn!("Janus keepalive 失敗: {e:#}");
                    }
                }
            },
        }
    }

    ctx.cleanup().await;
}

impl Ctx {
    fn send(&self, msg: ServerMsg) {
        self.st.radio.send(self.conn, msg);
    }

    async fn on_client(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Join { sdp } => {
                if let Err(e) = self.join(sdp).await {
                    tracing::error!("Janus への参加に失敗: {e:#}");
                    self.send(ServerMsg::Error { message: format!("音声サーバに接続できません: {e}") });
                }
            }
            ClientMsg::Ptt { on: true } => self.ptt_on().await,
            ClientMsg::Ptt { on: false } => self.ptt_off().await,
            ClientMsg::Ping => {}
        }
    }

    async fn join(&mut self, sdp: String) -> anyhow::Result<()> {
        anyhow::ensure!(self.janus.is_none(), "既に接続しています");
        let janus = &self.st.janus;
        let session = janus.create_session().await?;
        let handle = match janus.attach(session).await {
            Ok(h) => h,
            Err(e) => {
                let _ = janus.destroy(session).await;
                return Err(e);
            }
        };

        // Janus からのイベントを受け取るロングポーリング
        let poller = {
            let janus = janus.clone();
            let tx = self.jev_tx.clone();
            tokio::spawn(async move {
                let mut failures = 0;
                loop {
                    match janus.poll(session).await {
                        Ok(evs) => {
                            failures = 0;
                            for ev in evs {
                                if tx.send(ev).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            tracing::warn!("Janus ポーリング失敗 ({failures}): {e:#}");
                            if failures >= 3 {
                                let _ = tx.send(json!({ "janus": "macross_poll_error" }));
                                return;
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            })
        };
        self.janus = Some(JanusConn { session, handle, poller, joined: false });

        let txn = transaction();
        let cfg = &self.st.cfg;
        let body = json!({
            "request": "join",
            "room": cfg.channel.room_id,
            "display": format!("{} ({})", self.user.name, self.user.id),
            "muted": true,
            "bitrate": cfg.audio.bitrate,
        });
        let jsep = json!({ "type": "offer", "sdp": sdp, "trickle": false });
        self.pending_join = Some(txn.clone());
        janus.message(session, handle, body, Some(jsep), &txn).await?;
        Ok(())
    }

    async fn on_janus(&mut self, ev: Value) {
        match ev["janus"].as_str() {
            Some("event") => {
                let data = &ev["plugindata"]["data"];
                let is_join = self.pending_join.is_some() && ev["transaction"].as_str() == self.pending_join.as_deref();
                if is_join {
                    self.pending_join = None;
                    if let Some(err) = data["error"].as_str() {
                        self.send(ServerMsg::Error { message: format!("参加できません: {err}") });
                    } else if let Some(sdp) = ev["jsep"]["sdp"].as_str() {
                        if let Some(j) = &mut self.janus {
                            j.joined = true;
                        }
                        self.send(ServerMsg::Answer { sdp: sdp.to_string() });
                    } else {
                        self.send(ServerMsg::Error { message: "音声サーバから応答がありません".into() });
                    }
                } else if let Some(err) = data["error"].as_str() {
                    tracing::warn!("AudioBridge エラー: {err}");
                }
            }
            Some("webrtcup") => tracing::info!("WebRTC 接続確立: {} conn={}", self.user.id, self.conn),
            Some("hangup") | Some("detached") => {
                self.release_talk().await;
                self.send(ServerMsg::Hangup { reason: "音声接続が切断されました".into() });
            }
            Some("macross_poll_error") => {
                self.release_talk().await;
                self.send(ServerMsg::Hangup { reason: "音声サーバとの通信が途絶えました".into() });
            }
            _ => {}
        }
    }

    async fn configure(&self, body: Value) -> anyhow::Result<()> {
        let j = self.janus.as_ref().ok_or_else(|| anyhow::anyhow!("未接続"))?;
        self.st.janus.message(j.session, j.handle, body, None, &transaction()).await.map(|_| ())
    }

    async fn ptt_on(&mut self) {
        if !self.janus.as_ref().is_some_and(|j| j.joined) {
            self.send(ServerMsg::Ptt { granted: false, reason: Some("音声サーバに未接続です".into()) });
            return;
        }
        let talk = match self.st.radio.try_start_talk(self.conn) {
            Ok(t) => t,
            Err(reason) => {
                Metrics::inc(&self.st.metrics.ptt_denied_total);
                self.send(ServerMsg::Ptt { granted: false, reason: Some(reason) });
                return;
            }
        };
        let filename = self.st.cfg.recording.mjr_dir().join(&talk.mjr_base);
        let body = json!({
            "request": "configure",
            "muted": false,
            "record": true,
            "filename": filename.to_string_lossy(),
        });
        match self.configure(body).await {
            Ok(()) => {
                Metrics::inc(&self.st.metrics.ptt_total);
                self.send(ServerMsg::Ptt { granted: true, reason: None });
                self.st.radio.broadcast_state();
            }
            Err(e) => {
                tracing::error!("PTT 開始に失敗: {e:#}");
                self.st.radio.stop_talk(self.conn);
                self.send(ServerMsg::Ptt { granted: false, reason: Some("音声サーバでエラーが発生しました".into()) });
            }
        }
    }

    async fn ptt_off(&mut self) {
        self.release_talk().await;
    }

    /// 発言を終了し、録音を変換キューへ
    async fn release_talk(&mut self) {
        let Some(talk) = self.st.radio.stop_talk(self.conn) else { return };
        if let Err(e) = self.configure(json!({ "request": "configure", "muted": true, "record": false })).await {
            tracing::warn!("PTT 終了の通知に失敗: {e:#}");
        }
        self.st.recorder.submit(talk);
        self.st.radio.broadcast_state();
    }

    async fn cleanup(mut self) {
        if let Some(talk) = self.st.radio.unregister(self.conn) {
            // 発言中に切断された
            let _ = self.configure(json!({ "request": "configure", "muted": true, "record": false })).await;
            self.st.recorder.submit(talk);
        }
        if let Some(j) = self.janus.take() {
            j.poller.abort();
            let _ = self.st.janus.destroy(j.session).await;
        }
        tracing::info!("切断: {} ({}) conn={}", self.user.name, self.user.id, self.conn);
        self.st.radio.broadcast_state();
    }
}
