//! ブラウザ 1 接続ぶんの WebSocket 処理
//!
//! ブラウザ ⇔ (WebSocket) ⇔ Rust ⇔ (HTTP API) ⇔ Janus AudioBridge
//! 音声そのものはブラウザと Janus の間を WebRTC (UDP) で直接流れる。
//!
//! 1 接続 = Janus セッション 1 つ。受聴口 L / R ごとにハンドル（＝ WebRTC 接続）を 1 つ持ち、
//! それぞれ別のチャンネル（AudioBridge ルーム）に参加する。

use crate::janus::transaction;
use crate::metrics::Metrics;
use crate::radio::{Location, Member, ServerMsg, Side};
use crate::tts::TtsJob;
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
    /// 受聴口をチャンネルに参加させる。SDP offer は ICE 候補を含む（trickle なし）
    Join { side: Side, channel: u32, sdp: String },
    Leave { side: Side },
    Ptt { side: Side, on: bool },
    Tts { side: Side, text: String },
    /// ブラウザの GPS（位置情報の送信を許可した場合のみ）
    Location { lat: f64, lon: f64, alt: Option<f64>, accuracy: Option<f64> },
    Ping,
}

struct Leg {
    handle: u64,
    channel: u32,
    joined: bool,
    /// join の応答待ちトランザクション
    pending: Option<String>,
}

struct Ctx {
    st: Arc<AppState>,
    user: AuthUser,
    conn: u64,
    session: Option<(u64, JoinHandle<()>)>,
    legs: [Option<Leg>; 2],
    jev_tx: mpsc::UnboundedSender<Value>,
}

pub async fn run(st: Arc<AppState>, user: AuthUser, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();

    let member = Member {
        user_id: &user.id,
        name: &user.name,
        admin: user.admin,
        superuser: user.superuser,
        allowed: st.users.channels_of(&user.id),
    };
    let Some(conn) = st.radio.register(member, tx) else {
        let msg = ServerMsg::Error { message: "接続数の上限に達しています".into() };
        let _ = ws_tx.send(Message::Text(serde_json::to_string(&msg).unwrap())).await;
        return;
    };
    tracing::info!("接続: {} ({}) conn={conn}", user.name, user.id);
    st.radio.broadcast_state();

    let (jev_tx, mut jev_rx) = mpsc::unbounded_channel::<Value>();
    let mut ctx = Ctx { st: st.clone(), user, conn, session: None, legs: [None, None], jev_tx };
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
                let out = match out {
                    ServerMsg::Preempted { side, by } => {
                        ctx.preempted(side, &by).await;
                        continue;
                    }
                    ServerMsg::Revoked { side } => {
                        ctx.drop_leg(side, true).await;
                        ctx.send(ServerMsg::Left { side, reason: "このチャンネルを使う権限がなくなりました".into() });
                        ctx.st.radio.broadcast_state();
                        continue;
                    }
                    other => other,
                };
                let hangup = matches!(out, ServerMsg::Hangup { .. });
                let text = serde_json::to_string(&out).unwrap();
                if ws_tx.send(Message::Text(text)).await.is_err() || hangup {
                    // Hangup（同じ ID の別ページからの接続など）を伝えたらこの接続は終わり
                    break;
                }
            },
            Some(ev) = jev_rx.recv() => ctx.on_janus(ev).await,
            _ = keepalive.tick() => {
                if let Some((session, _)) = &ctx.session {
                    if let Err(e) = ctx.st.janus.keepalive(*session).await {
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
            ClientMsg::Join { side, channel, sdp } => {
                if let Err(e) = self.join(side, channel, sdp).await {
                    tracing::error!("チャンネル {channel} への参加に失敗: {e:#}");
                    self.drop_leg(side, true).await;
                    self.send(ServerMsg::Left { side, reason: format!("参加できません: {e}") });
                    self.st.radio.broadcast_state();
                }
            }
            ClientMsg::Leave { side } => {
                self.drop_leg(side, true).await;
                self.st.radio.broadcast_state();
            }
            ClientMsg::Ptt { side, on: true } => self.ptt_on(side).await,
            ClientMsg::Ptt { on: false, .. } => self.release_talk().await,
            ClientMsg::Tts { side, text } => self.tts(side, text),
            ClientMsg::Location { lat, lon, alt, accuracy } => {
                if self.st.cfg.location.enabled {
                    self.st.radio.set_location(self.conn, Location { lat, lon, alt, accuracy, ts: 0 });
                }
            }
            ClientMsg::Ping => self.send(ServerMsg::Pong),
        }
    }

    /// Janus セッションとイベント受信（ロングポーリング）を用意する
    async fn ensure_session(&mut self) -> anyhow::Result<u64> {
        if let Some((s, _)) = &self.session {
            return Ok(*s);
        }
        let janus = self.st.janus.clone();
        let session = janus.create_session().await?;
        let tx = self.jev_tx.clone();
        let poller = tokio::spawn(async move {
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
        });
        self.session = Some((session, poller));
        Ok(session)
    }

    async fn join(&mut self, side: Side, channel: u32, sdp: String) -> anyhow::Result<()> {
        // 同じ受聴口で別のチャンネルに入っていれば抜ける
        self.drop_leg(side, true).await;
        self.st.radio.set_leg(self.conn, side, Some(channel)).map_err(|e| anyhow::anyhow!(e))?;
        let def = self.st.radio.channel(channel).ok_or_else(|| anyhow::anyhow!("チャンネルが見つかりません"))?;
        let room = self.st.radio.room_of(channel);
        let st = self.st.clone();
        let cfg = &st.cfg;
        self.st.janus.ensure_room(room, &def.name, cfg.audio.sampling_rate).await?;

        let session = self.ensure_session().await?;
        let handle = self.st.janus.attach(session).await?;
        let txn = transaction();
        self.legs[side.idx()] = Some(Leg { handle, channel, joined: false, pending: Some(txn.clone()) });

        let body = json!({
            "request": "join",
            "room": room,
            "display": format!("{} ({})", self.user.name, self.user.id),
            "muted": true,
            "bitrate": cfg.audio.bitrate,
        });
        let jsep = json!({ "type": "offer", "sdp": sdp, "trickle": false });
        self.st.janus.message(session, handle, body, Some(jsep), &txn).await?;
        self.st.radio.broadcast_state();
        Ok(())
    }

    /// 受聴口を外す。その受聴口で発言中なら発言を終える
    async fn drop_leg(&mut self, side: Side, detach: bool) {
        self.release_talk_on(Some(side)).await;
        let _ = self.st.radio.set_leg(self.conn, side, None);
        if let Some(leg) = self.legs[side.idx()].take() {
            if detach {
                if let Some((session, _)) = &self.session {
                    let _ = self.st.janus.detach(*session, leg.handle).await;
                }
            }
        }
    }

    fn side_of(&self, handle: u64) -> Option<Side> {
        Side::ALL.into_iter().find(|s| self.legs[s.idx()].as_ref().is_some_and(|l| l.handle == handle))
    }

    async fn on_janus(&mut self, ev: Value) {
        if ev["janus"] == "macross_poll_error" {
            self.release_talk().await;
            self.send(ServerMsg::Hangup { reason: "音声サーバとの通信が途絶えました".into(), retry: true });
            return;
        }
        let Some(side) = ev["sender"].as_u64().and_then(|h| self.side_of(h)) else { return };
        match ev["janus"].as_str() {
            Some("event") => {
                let data = &ev["plugindata"]["data"];
                let leg = self.legs[side.idx()].as_mut().unwrap();
                let is_join = leg.pending.is_some() && ev["transaction"].as_str() == leg.pending.as_deref();
                if is_join {
                    leg.pending = None;
                    if let Some(err) = data["error"].as_str() {
                        let reason = format!("参加できません: {err}");
                        self.drop_leg(side, true).await;
                        self.send(ServerMsg::Left { side, reason });
                        self.st.radio.broadcast_state();
                    } else if let Some(sdp) = ev["jsep"]["sdp"].as_str() {
                        leg.joined = true;
                        self.send(ServerMsg::Answer { side, sdp: sdp.to_string() });
                    } else {
                        self.drop_leg(side, true).await;
                        self.send(ServerMsg::Left { side, reason: "音声サーバから応答がありません".into() });
                        self.st.radio.broadcast_state();
                    }
                } else if data["audiobridge"] == "destroyed" {
                    self.drop_leg(side, true).await;
                    self.send(ServerMsg::Left { side, reason: "チャンネルが削除されました".into() });
                    self.st.radio.broadcast_state();
                } else if let Some(err) = data["error"].as_str() {
                    tracing::warn!("AudioBridge エラー: {err}");
                }
            }
            Some("webrtcup") => {
                let ch = self.legs[side.idx()].as_ref().map(|l| l.channel).unwrap_or_default();
                tracing::info!("WebRTC 接続確立: {} conn={} {side:?}=ch{ch}", self.user.id, self.conn);
            }
            Some(kind @ ("hangup" | "detached")) => {
                self.drop_leg(side, kind == "hangup").await;
                self.send(ServerMsg::Left { side, reason: "音声接続が切断されました".into() });
                self.st.radio.broadcast_state();
            }
            _ => {}
        }
    }

    async fn configure(&self, side: Side, body: Value) -> anyhow::Result<()> {
        let leg = self.legs[side.idx()].as_ref().ok_or_else(|| anyhow::anyhow!("未接続"))?;
        let (session, _) = self.session.as_ref().ok_or_else(|| anyhow::anyhow!("未接続"))?;
        self.st.janus.message(*session, leg.handle, body, None, &transaction()).await.map(|_| ())
    }

    async fn ptt_on(&mut self, side: Side) {
        if !self.legs[side.idx()].as_ref().is_some_and(|l| l.joined) {
            self.send(ServerMsg::Ptt { side, granted: false, reason: Some("音声サーバに未接続です".into()) });
            return;
        }
        let talk = match self.st.radio.try_start_talk(self.conn, side) {
            Ok(t) => t,
            Err(reason) => {
                Metrics::inc(&self.st.metrics.ptt_denied_total);
                self.send(ServerMsg::Ptt { side, granted: false, reason: Some(reason) });
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
        match self.configure(side, body).await {
            Ok(()) => {
                Metrics::inc(&self.st.metrics.ptt_total);
                self.st.metrics.inc_channel_ptt(&talk.channel_name);
                self.send(ServerMsg::Ptt { side, granted: true, reason: None });
                self.st.radio.broadcast_state();
            }
            Err(e) => {
                tracing::error!("PTT 開始に失敗: {e:#}");
                self.st.radio.stop_talk(self.conn);
                self.send(ServerMsg::Ptt { side, granted: false, reason: Some("音声サーバでエラーが発生しました".into()) });
            }
        }
    }

    async fn release_talk(&mut self) {
        self.release_talk_on(None).await;
    }

    /// 発言を終了し、録音を変換キューへ（only を指定するとその受聴口での発言のみ）
    async fn release_talk_on(&mut self, only: Option<Side>) {
        let Some(side) = self.st.radio.talk_side(self.conn) else { return };
        if only.is_some_and(|s| s != side) {
            return;
        }
        if let Some(talk) = self.st.radio.stop_talk(self.conn) {
            self.finish(talk).await;
        }
    }

    async fn finish(&self, talk: crate::radio::Talk) {
        if let Err(e) = self.configure(talk.side, json!({ "request": "configure", "muted": true, "record": false })).await {
            tracing::debug!("PTT 終了の通知に失敗: {e:#}");
        }
        self.send(ServerMsg::Ptt { side: talk.side, granted: false, reason: None });
        self.st.recorder.submit(talk);
        self.st.radio.broadcast_state();
    }

    /// 管理者の割り込みで発言を止められた
    async fn preempted(&mut self, side: Side, by: &str) {
        if self.st.radio.talk_side(self.conn) != Some(side) {
            return;
        }
        tracing::info!("発言を停止（管理者 {by} の割り込み）: {} conn={}", self.user.id, self.conn);
        self.send(ServerMsg::Ptt { side, granted: false, reason: Some(format!("管理者（{by}）が発言するため停止されました")) });
        self.release_talk().await;
    }

    fn tts(&self, side: Side, text: String) {
        let Some(leg) = &self.legs[side.idx()] else {
            self.send(ServerMsg::Error { message: "チャンネルに参加してから送信してください".into() });
            return;
        };
        let channel = leg.channel;
        let job = TtsJob {
            conn: self.conn,
            user_id: self.user.id.clone(),
            // 接続中に管理画面で名前が変わっていても、いまの名前で記録する
            name: self.st.users.get(&self.user.id).map(|u| u.name).unwrap_or_else(|| self.user.name.clone()),
            channel_id: leg.channel,
            text,
        };
        match self.st.tts.submit(job) {
            Ok(()) => self.st.radio.log_tts(self.conn, channel),
            Err(message) => self.send(ServerMsg::Error { message }),
        }
    }

    async fn cleanup(mut self) {
        if let Some(talk) = self.st.radio.unregister(self.conn) {
            // 発言中に切断された
            let _ = self.configure(talk.side, json!({ "request": "configure", "muted": true, "record": false })).await;
            self.st.recorder.submit(talk);
        }
        self.legs = [None, None];
        if let Some((session, poller)) = self.session.take() {
            poller.abort();
            let _ = self.st.janus.destroy(session).await;
        }
        tracing::info!("切断: {} ({}) conn={}", self.user.name, self.user.id, self.conn);
        self.st.radio.broadcast_state();
    }
}
