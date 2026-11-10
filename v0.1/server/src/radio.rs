//! チャンネルの接続者・発言者（発言枠）の管理

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// サーバ → ブラウザ
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Answer { sdp: String },
    State { channel: String, online: usize, max_connections: usize, max_speakers: usize, speakers: Vec<Speaker> },
    Ptt { granted: bool, reason: Option<String> },
    Error { message: String },
    Hangup { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Speaker {
    pub id: String,
    pub name: String,
}

/// 発言（PTT 押下 1 回分）
#[derive(Debug, Clone)]
pub struct Talk {
    pub user_id: String,
    pub name: String,
    pub start: DateTime<FixedOffset>,
    /// Janus に渡した録音ファイル名のベース
    pub mjr_base: String,
}

pub struct Snapshot {
    pub connections: usize,
    pub users: usize,
    pub speakers: usize,
    pub max_speakers: usize,
}

struct Client {
    user_id: String,
    name: String,
    tx: UnboundedSender<ServerMsg>,
    talk: Option<Talk>,
}

struct Inner {
    next_conn: u64,
    clients: HashMap<u64, Client>,
    /// 発言中の接続（押した順）
    speakers: Vec<u64>,
    max_speakers: usize,
}

#[derive(Serialize, Deserialize)]
struct Settings {
    max_speakers: usize,
}

pub struct Radio {
    channel: String,
    max_connections: usize,
    settings_path: PathBuf,
    inner: Mutex<Inner>,
}

/// 日本時間
pub fn now_jst() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&FixedOffset::east_opt(9 * 3600).unwrap())
}

impl Radio {
    pub fn new(channel: String, max_connections: usize, default_max_speakers: usize, settings_path: PathBuf) -> Self {
        let max_speakers = std::fs::read_to_string(&settings_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .map(|s| s.max_speakers)
            .filter(|n| *n > 0)
            .unwrap_or(default_max_speakers);
        Self {
            channel,
            max_connections,
            settings_path,
            inner: Mutex::new(Inner { next_conn: 1, clients: HashMap::new(), speakers: vec![], max_speakers }),
        }
    }

    /// 接続を登録。上限に達していれば None
    pub fn register(&self, user_id: &str, name: &str, tx: UnboundedSender<ServerMsg>) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap();
        if inner.clients.len() >= self.max_connections {
            return None;
        }
        let conn = inner.next_conn;
        inner.next_conn += 1;
        inner.clients.insert(conn, Client { user_id: user_id.into(), name: name.into(), tx, talk: None });
        Some(conn)
    }

    /// 接続を削除。発言中だった場合はその発言を返す
    pub fn unregister(&self, conn: u64) -> Option<Talk> {
        let mut inner = self.inner.lock().unwrap();
        inner.speakers.retain(|c| *c != conn);
        inner.clients.remove(&conn).and_then(|c| c.talk)
    }

    pub fn try_start_talk(&self, conn: u64) -> Result<Talk, String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.speakers.contains(&conn) {
            return Err("既に発言中です".into());
        }
        if inner.speakers.len() >= inner.max_speakers {
            return Err(format!("発言枠（{}人）が埋まっています", inner.max_speakers));
        }
        let client = inner.clients.get_mut(&conn).ok_or("未接続です")?;
        let start = now_jst();
        let talk = Talk {
            user_id: client.user_id.clone(),
            name: client.name.clone(),
            start,
            mjr_base: format!("{}_{}_{}", client.user_id, conn, start.timestamp_millis()),
        };
        client.talk = Some(talk.clone());
        inner.speakers.push(conn);
        Ok(talk)
    }

    pub fn stop_talk(&self, conn: u64) -> Option<Talk> {
        let mut inner = self.inner.lock().unwrap();
        inner.speakers.retain(|c| *c != conn);
        inner.clients.get_mut(&conn).and_then(|c| c.talk.take())
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        let users: std::collections::HashSet<&str> = inner.clients.values().map(|c| c.user_id.as_str()).collect();
        Snapshot {
            connections: inner.clients.len(),
            users: users.len(),
            speakers: inner.speakers.len(),
            max_speakers: inner.max_speakers,
        }
    }

    pub fn max_speakers(&self) -> usize {
        self.inner.lock().unwrap().max_speakers
    }

    pub fn set_max_speakers(&self, n: usize) -> anyhow::Result<()> {
        anyhow::ensure!((1..=self.max_connections).contains(&n), "発言枠は1〜{}の範囲で指定してください", self.max_connections);
        self.inner.lock().unwrap().max_speakers = n;
        std::fs::write(&self.settings_path, serde_json::to_string_pretty(&Settings { max_speakers: n })?)?;
        self.broadcast_state();
        Ok(())
    }

    pub fn send(&self, conn: u64, msg: ServerMsg) {
        if let Some(c) = self.inner.lock().unwrap().clients.get(&conn) {
            let _ = c.tx.send(msg);
        }
    }

    pub fn broadcast_state(&self) {
        let inner = self.inner.lock().unwrap();
        let speakers: Vec<Speaker> = inner
            .speakers
            .iter()
            .filter_map(|c| inner.clients.get(c))
            .map(|c| Speaker { id: c.user_id.clone(), name: c.name.clone() })
            .collect();
        let msg = ServerMsg::State {
            channel: self.channel.clone(),
            online: inner.clients.len(),
            max_connections: self.max_connections,
            max_speakers: inner.max_speakers,
            speakers,
        };
        for c in inner.clients.values() {
            let _ = c.tx.send(msg.clone());
        }
    }
}
