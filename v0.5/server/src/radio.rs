//! チャンネル（最大9）・接続者・発言者（発言枠）の管理
//!
//! 1 接続は L / R の 2 つの「受聴口」を持ち、それぞれ別のチャンネルに参加できる（LR受聴）。
//! 発言（PTT）は 1 接続につき同時に 1 つまで。発言枠はチャンネルごと。
//! 1 つの ID で開ける無線ページは 1 つまで（新しく開いたページが優先され、古いページは切断される）。

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

pub const MAX_CHANNELS: u32 = 9;
const NAME_MAX_CHARS: usize = 20;

/// 受聴口（左 / 右）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    L,
    R,
}

impl Side {
    pub const ALL: [Side; 2] = [Side::L, Side::R];

    pub fn idx(self) -> usize {
        match self {
            Side::L => 0,
            Side::R => 1,
        }
    }

    pub fn other(self) -> Side {
        match self {
            Side::L => Side::R,
            Side::R => Side::L,
        }
    }
}

/// サーバ → ブラウザ
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Answer { side: Side, sdp: String },
    State { online: usize, max_connections: usize, channels: Vec<ChannelView> },
    Ptt { side: Side, granted: bool, reason: Option<String> },
    /// 受聴口が切り離された（チャンネル削除・音声切断・参加失敗）
    Left { side: Side, reason: String },
    /// 管理者の割り込みで発言を止められた（client.rs が受けて Janus 側を止め、ブラウザには Ptt で伝える）
    Preempted { side: Side, by: String },
    /// チャンネルを使う権限がなくなった（client.rs が受けて受聴口を外し、ブラウザには Left で伝える）
    Revoked { side: Side },
    /// 読み上げ開始の通知
    Tts { channel: u32, id: String, name: String, text: String, time: String },
    Error { message: String },
    Hangup { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Speaker {
    pub id: String,
    pub name: String,
    pub admin: bool,
    pub superuser: bool,
}

/// ブラウザの GPS で得た位置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    /// 高度 (m)。取れない端末が多い
    #[serde(default)]
    pub alt: Option<f64>,
    /// 誤差 (m)
    #[serde(default)]
    pub accuracy: Option<f64>,
    /// サーバで受け取った時刻（UNIX ミリ秒）
    #[serde(default)]
    pub ts: i64,
}

impl Location {
    pub fn valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
            && !(self.lat == 0.0 && self.lon == 0.0)
    }
}

/// 地図に出す接続中ユーザ
#[derive(Debug, Clone, Serialize)]
pub struct PositionView {
    pub id: String,
    pub name: String,
    pub admin: bool,
    pub superuser: bool,
    /// L / R で聴いているチャンネル名
    pub listening: Vec<String>,
    /// 発言中のチャンネル名
    pub talking: Option<String>,
    pub location: Option<Location>,
}

/// 発信の履歴（地図に出す）
#[derive(Debug, Clone, Serialize)]
pub struct TxEvent {
    pub id: String,
    pub name: String,
    pub channel: String,
    /// PTT / TTS
    pub kind: &'static str,
    pub time: String,
    pub ts: i64,
    pub location: Location,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelView {
    pub id: u32,
    pub name: String,
    pub max_speakers: usize,
    /// このチャンネルを聴いている接続数
    pub listeners: usize,
    pub speakers: Vec<Speaker>,
}

/// data/channels.json に保存するチャンネル定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDef {
    /// 1〜9。Janus のルーム番号は room_base + id - 1
    pub id: u32,
    pub name: String,
    pub max_speakers: usize,
}

/// 発言（PTT 押下 1 回分）
#[derive(Debug, Clone)]
pub struct Talk {
    pub user_id: String,
    pub name: String,
    pub channel_id: u32,
    /// 録音フォルダ名（押した時点のチャンネル名）
    pub channel_name: String,
    pub side: Side,
    pub start: DateTime<FixedOffset>,
    /// Janus に渡した録音ファイル名のベース
    pub mjr_base: String,
    /// 押した時点の位置
    pub location: Option<Location>,
}

pub struct ChannelStat {
    pub name: String,
    pub listeners: usize,
    pub speakers: usize,
    pub max_speakers: usize,
}

pub struct Snapshot {
    pub connections: usize,
    pub users: usize,
    pub channels: Vec<ChannelStat>,
}

/// 接続を登録するときの情報
pub struct Member<'a> {
    pub user_id: &'a str,
    pub name: &'a str,
    pub admin: bool,
    pub superuser: bool,
    /// 使えるチャンネル（None = すべて）
    pub allowed: Option<Vec<u32>>,
}

struct Client {
    user_id: String,
    name: String,
    admin: bool,
    superuser: bool,
    allowed: Option<HashSet<u32>>,
    location: Option<Location>,
    tx: UnboundedSender<ServerMsg>,
    /// L / R それぞれが参加しているチャンネル
    legs: [Option<u32>; 2],
    talk: Option<Talk>,
}

struct Inner {
    next_conn: u64,
    clients: HashMap<u64, Client>,
    channels: BTreeMap<u32, ChannelDef>,
    /// チャンネル → 発言中の接続（押した順）
    speakers: HashMap<u32, Vec<u64>>,
    /// 発信の履歴（位置が分かっているものだけ）
    history: VecDeque<TxEvent>,
}

impl Client {
    fn rank(&self) -> u8 {
        if self.superuser {
            2
        } else if self.admin {
            1
        } else {
            0
        }
    }

    fn can_use(&self, ch: u32) -> bool {
        self.allowed.as_ref().map_or(true, |a| a.contains(&ch))
    }
}

pub struct Radio {
    room_base: u64,
    max_connections: usize,
    history_max: usize,
    path: PathBuf,
    inner: Mutex<Inner>,
}

/// 日本時間
pub fn now_jst() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&FixedOffset::east_opt(9 * 3600).unwrap())
}

impl Radio {
    /// `path`（channels.json）が無ければ `initial` から作る
    pub fn new(
        room_base: u64,
        max_connections: usize,
        history_max: usize,
        path: PathBuf,
        initial: &[(String, usize)],
    ) -> anyhow::Result<Self> {
        let channels: Vec<ChannelDef> = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("{} の形式が不正です: {e}", path.display()))?,
            Err(_) => initial
                .iter()
                .take(MAX_CHANNELS as usize)
                .enumerate()
                .map(|(i, (name, max))| ChannelDef { id: i as u32 + 1, name: name.clone(), max_speakers: (*max).max(1) })
                .collect(),
        };
        let mut map = BTreeMap::new();
        for c in channels {
            if (1..=MAX_CHANNELS).contains(&c.id) {
                map.insert(c.id, c);
            }
        }
        if map.is_empty() {
            map.insert(1, ChannelDef { id: 1, name: "ch1".into(), max_speakers: 1 });
        }
        let radio = Self {
            room_base,
            max_connections,
            history_max,
            path,
            inner: Mutex::new(Inner {
                next_conn: 1,
                clients: HashMap::new(),
                channels: map,
                speakers: HashMap::new(),
                history: VecDeque::new(),
            }),
        };
        radio.save(&radio.inner.lock().unwrap())?;
        Ok(radio)
    }

    fn save(&self, inner: &Inner) -> anyhow::Result<()> {
        let list: Vec<&ChannelDef> = inner.channels.values().collect();
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&list)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn room_of(&self, channel: u32) -> u64 {
        self.room_base + channel as u64 - 1
    }

    pub fn channels(&self) -> Vec<ChannelDef> {
        self.inner.lock().unwrap().channels.values().cloned().collect()
    }

    pub fn channel(&self, id: u32) -> Option<ChannelDef> {
        self.inner.lock().unwrap().channels.get(&id).cloned()
    }

    fn check_name(inner: &Inner, name: &str, except: Option<u32>) -> anyhow::Result<String> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "チャンネル名を入力してください");
        anyhow::ensure!(name.chars().count() <= NAME_MAX_CHARS, "チャンネル名は{NAME_MAX_CHARS}文字以内にしてください");
        anyhow::ensure!(
            !name.starts_with('.') && !name.chars().any(|c| c.is_control() || r#"/\:*?"<>|"#.contains(c)),
            "チャンネル名に / \\ : * ? \" < > | や先頭の . は使えません（録音フォルダ名になります）"
        );
        anyhow::ensure!(
            !inner.channels.values().any(|c| Some(c.id) != except && c.name == name),
            "同じ名前のチャンネルがあります"
        );
        Ok(name.to_string())
    }

    fn check_speakers(&self, n: usize) -> anyhow::Result<()> {
        anyhow::ensure!((1..=self.max_connections).contains(&n), "発言枠は1〜{}の範囲で指定してください", self.max_connections);
        Ok(())
    }

    pub fn create_channel(&self, name: &str, max_speakers: usize) -> anyhow::Result<ChannelDef> {
        self.check_speakers(max_speakers)?;
        let def = {
            let mut inner = self.inner.lock().unwrap();
            let name = Self::check_name(&inner, name, None)?;
            let id = (1..=MAX_CHANNELS)
                .find(|i| !inner.channels.contains_key(i))
                .ok_or_else(|| anyhow::anyhow!("チャンネルは最大{MAX_CHANNELS}つまでです"))?;
            let def = ChannelDef { id, name, max_speakers };
            inner.channels.insert(id, def.clone());
            self.save(&inner)?;
            def
        };
        self.broadcast_state();
        Ok(def)
    }

    pub fn update_channel(&self, id: u32, name: Option<&str>, max_speakers: Option<usize>) -> anyhow::Result<ChannelDef> {
        if let Some(n) = max_speakers {
            self.check_speakers(n)?;
        }
        let def = {
            let mut inner = self.inner.lock().unwrap();
            let name = name.map(|n| Self::check_name(&inner, n, Some(id))).transpose()?;
            let c = inner.channels.get_mut(&id).ok_or_else(|| anyhow::anyhow!("チャンネルが見つかりません"))?;
            if let Some(n) = name {
                c.name = n;
            }
            if let Some(n) = max_speakers {
                c.max_speakers = n;
            }
            let def = c.clone();
            self.save(&inner)?;
            def
        };
        self.broadcast_state();
        Ok(def)
    }

    /// チャンネルを削除する。参加中の接続の受聴口は外す
    /// （Janus のルームを破棄すると各接続に destroyed イベントが届き、発言・録音はそちらで片付ける）
    pub fn delete_channel(&self, id: u32) -> anyhow::Result<ChannelDef> {
        let def = {
            let mut inner = self.inner.lock().unwrap();
            anyhow::ensure!(inner.channels.contains_key(&id), "チャンネルが見つかりません");
            anyhow::ensure!(inner.channels.len() > 1, "最後のチャンネルは削除できません");
            let def = inner.channels.remove(&id).unwrap();
            inner.speakers.remove(&id);
            for c in inner.clients.values_mut() {
                for leg in c.legs.iter_mut() {
                    if *leg == Some(id) {
                        *leg = None;
                    }
                }
                if let Some(a) = &mut c.allowed {
                    a.remove(&id);
                }
            }
            self.save(&inner)?;
            def
        };
        self.broadcast_state();
        Ok(def)
    }

    /// 接続を登録。上限に達していれば None。
    /// 同じ ID の接続（別のページ・別の端末）があれば切断させる（1 ID につき無線ページは 1 つまで）
    pub fn register(&self, m: Member, tx: UnboundedSender<ServerMsg>) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap();
        let others = inner.clients.values().filter(|c| c.user_id != m.user_id).count();
        if others >= self.max_connections {
            return None;
        }
        for c in inner.clients.values().filter(|c| c.user_id == m.user_id) {
            let _ = c.tx.send(ServerMsg::Hangup {
                reason: "同じ ID で別のページ（または別の端末）から接続したため、このページの接続を切りました".into(),
            });
        }
        let conn = inner.next_conn;
        inner.next_conn += 1;
        inner.clients.insert(
            conn,
            Client {
                user_id: m.user_id.into(),
                name: m.name.into(),
                admin: m.admin,
                superuser: m.superuser,
                allowed: m.allowed.map(|v| v.into_iter().collect()),
                location: None,
                tx,
                legs: [None, None],
                talk: None,
            },
        );
        Some(conn)
    }

    /// ユーザの使えるチャンネルを変更。使えなくなったチャンネルを聴いている受聴口は外させる
    pub fn set_access(&self, user_id: &str, allowed: Option<Vec<u32>>) {
        {
            let mut inner = self.inner.lock().unwrap();
            let allowed: Option<HashSet<u32>> = allowed.map(|v| v.into_iter().collect());
            for c in inner.clients.values_mut().filter(|c| c.user_id == user_id) {
                c.allowed = allowed.clone();
                for side in Side::ALL {
                    if c.legs[side.idx()].is_some_and(|ch| !c.can_use(ch)) {
                        let _ = c.tx.send(ServerMsg::Revoked { side });
                    }
                }
            }
        }
        self.broadcast_state();
    }

    /// 名前の変更を接続中のページ（発言者の表示など）に反映する
    pub fn rename(&self, user_id: &str, name: &str) {
        {
            let mut inner = self.inner.lock().unwrap();
            for c in inner.clients.values_mut().filter(|c| c.user_id == user_id) {
                c.name = name.to_string();
            }
        }
        self.broadcast_state();
    }

    /// 位置を更新
    pub fn set_location(&self, conn: u64, mut loc: Location) -> bool {
        if !loc.valid() {
            return false;
        }
        loc.ts = Utc::now().timestamp_millis();
        loc.alt = loc.alt.filter(|a| a.is_finite());
        loc.accuracy = loc.accuracy.filter(|a| a.is_finite() && *a >= 0.0);
        let mut inner = self.inner.lock().unwrap();
        match inner.clients.get_mut(&conn) {
            Some(c) => {
                c.location = Some(loc);
                true
            }
            None => false,
        }
    }

    pub fn location_of(&self, conn: u64) -> Option<Location> {
        self.inner.lock().unwrap().clients.get(&conn).and_then(|c| c.location.clone())
    }

    /// 発信を履歴に残す（位置が分かっている場合のみ）
    fn push_history(&self, inner: &mut Inner, conn: u64, channel: &str, kind: &'static str) {
        let Some(c) = inner.clients.get(&conn) else { return };
        let Some(location) = c.location.clone() else { return };
        let now = now_jst();
        let ev = TxEvent {
            id: c.user_id.clone(),
            name: c.name.clone(),
            channel: channel.to_string(),
            kind,
            time: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            ts: now.timestamp_millis(),
            location,
        };
        inner.history.push_back(ev);
        while inner.history.len() > self.history_max {
            inner.history.pop_front();
        }
    }

    /// 読み上げを発信として履歴に残す
    pub fn log_tts(&self, conn: u64, channel: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(name) = inner.channels.get(&channel).map(|c| c.name.clone()) else { return };
        self.push_history(&mut inner, conn, &name, "TTS");
    }

    /// 地図用: 接続中のユーザと発信履歴
    pub fn positions(&self) -> (Vec<PositionView>, Vec<TxEvent>) {
        let inner = self.inner.lock().unwrap();
        let ch_name = |id: &u32| inner.channels.get(id).map(|c| c.name.clone()).unwrap_or_default();
        let mut users: Vec<PositionView> = inner
            .clients
            .values()
            .map(|c| PositionView {
                id: c.user_id.clone(),
                name: c.name.clone(),
                admin: c.admin,
                superuser: c.superuser,
                listening: c.legs.iter().flatten().map(&ch_name).collect(),
                talking: c.talk.as_ref().map(|t| t.channel_name.clone()),
                location: c.location.clone(),
            })
            .collect();
        users.sort_by(|a, b| a.id.cmp(&b.id));
        (users, inner.history.iter().cloned().collect())
    }

    /// 接続を削除。発言中だった場合はその発言を返す
    pub fn unregister(&self, conn: u64) -> Option<Talk> {
        let mut inner = self.inner.lock().unwrap();
        for list in inner.speakers.values_mut() {
            list.retain(|c| *c != conn);
        }
        inner.clients.remove(&conn).and_then(|c| c.talk)
    }

    /// 受聴口のチャンネルを設定（None で外す）
    pub fn set_leg(&self, conn: u64, side: Side, channel: Option<u32>) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(ch) = channel {
            if !inner.channels.contains_key(&ch) {
                return Err("チャンネルが見つかりません".into());
            }
        }
        let client = inner.clients.get_mut(&conn).ok_or("未接続です")?;
        if channel.is_some_and(|ch| !client.can_use(ch)) {
            return Err("このチャンネルを使う権限がありません".into());
        }
        if channel.is_some() && client.legs[side.other().idx()] == channel {
            return Err("L と R に同じチャンネルは選べません".into());
        }
        client.legs[side.idx()] = channel;
        Ok(())
    }

    pub fn try_start_talk(&self, conn: u64, side: Side) -> Result<Talk, String> {
        let mut inner = self.inner.lock().unwrap();
        let client = inner.clients.get(&conn).ok_or("未接続です")?;
        if client.talk.is_some() {
            return Err("既に発言中です".into());
        }
        let ch = client.legs[side.idx()].ok_or("チャンネルに参加していません")?;
        if !client.can_use(ch) {
            return Err("このチャンネルを使う権限がありません".into());
        }
        let def = inner.channels.get(&ch).ok_or("チャンネルが見つかりません")?.clone();
        let count = inner.speakers.get(&ch).map_or(0, |v| v.len());
        if count >= def.max_speakers {
            // 上位のロールは下位の人の発言（いちばん長く話している人）を止めて発言できる
            // スーパーユーザー > 管理者 > 一般
            let rank = client.rank();
            let by = client.name.clone();
            let victim = inner
                .speakers
                .get(&ch)
                .into_iter()
                .flatten()
                .copied()
                .find(|c| inner.clients.get(c).is_some_and(|cl| cl.rank() < rank));
            let Some(victim) = victim else {
                return Err(format!("{} の発言枠（{}人）が埋まっています", def.name, def.max_speakers));
            };
            inner.speakers.get_mut(&ch).unwrap().retain(|c| *c != victim);
            if let Some(v) = inner.clients.get(&victim) {
                if let Some(t) = &v.talk {
                    let _ = v.tx.send(ServerMsg::Preempted { side: t.side, by });
                }
            }
        }
        let client = inner.clients.get_mut(&conn).unwrap();
        let start = now_jst();
        let talk = Talk {
            user_id: client.user_id.clone(),
            name: client.name.clone(),
            channel_id: ch,
            channel_name: def.name,
            side,
            start,
            mjr_base: format!("{}_{}_{}", client.user_id, conn, start.timestamp_millis()),
            location: client.location.clone(),
        };
        client.talk = Some(talk.clone());
        inner.speakers.entry(ch).or_default().push(conn);
        let name = talk.channel_name.clone();
        self.push_history(&mut inner, conn, &name, "PTT");
        Ok(talk)
    }

    /// 発言中ならその受聴口
    pub fn talk_side(&self, conn: u64) -> Option<Side> {
        self.inner.lock().unwrap().clients.get(&conn).and_then(|c| c.talk.as_ref().map(|t| t.side))
    }

    pub fn stop_talk(&self, conn: u64) -> Option<Talk> {
        let mut inner = self.inner.lock().unwrap();
        let talk = inner.clients.get_mut(&conn).and_then(|c| c.talk.take())?;
        if let Some(list) = inner.speakers.get_mut(&talk.channel_id) {
            list.retain(|c| *c != conn);
        }
        Some(talk)
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        let users: HashSet<&str> = inner.clients.values().map(|c| c.user_id.as_str()).collect();
        let channels = inner
            .channels
            .values()
            .map(|c| ChannelStat {
                name: c.name.clone(),
                listeners: Self::listeners(&inner, c.id),
                speakers: inner.speakers.get(&c.id).map_or(0, |v| v.len()),
                max_speakers: c.max_speakers,
            })
            .collect();
        Snapshot { connections: inner.clients.len(), users: users.len(), channels }
    }

    fn listeners(inner: &Inner, ch: u32) -> usize {
        inner.clients.values().filter(|c| c.legs.contains(&Some(ch))).count()
    }

    pub fn send(&self, conn: u64, msg: ServerMsg) {
        if let Some(c) = self.inner.lock().unwrap().clients.get(&conn) {
            let _ = c.tx.send(msg);
        }
    }

    /// チャンネルを聴いている接続へ送る
    pub fn send_to_channel(&self, ch: u32, msg: ServerMsg) {
        let inner = self.inner.lock().unwrap();
        for c in inner.clients.values().filter(|c| c.legs.contains(&Some(ch))) {
            let _ = c.tx.send(msg.clone());
        }
    }

    /// 状態を全接続へ送る。各接続には使えるチャンネルだけを見せる
    pub fn broadcast_state(&self) {
        let inner = self.inner.lock().unwrap();
        let channels: Vec<ChannelView> = inner
            .channels
            .values()
            .map(|c| ChannelView {
                id: c.id,
                name: c.name.clone(),
                max_speakers: c.max_speakers,
                listeners: Self::listeners(&inner, c.id),
                speakers: inner
                    .speakers
                    .get(&c.id)
                    .into_iter()
                    .flatten()
                    .filter_map(|conn| inner.clients.get(conn))
                    .map(|cl| Speaker {
                        id: cl.user_id.clone(),
                        name: cl.name.clone(),
                        admin: cl.admin,
                        superuser: cl.superuser,
                    })
                    .collect(),
            })
            .collect();
        for c in inner.clients.values() {
            let visible = channels.iter().filter(|v| c.can_use(v.id)).cloned().collect();
            let msg = ServerMsg::State { online: inner.clients.len(), max_connections: self.max_connections, channels: visible };
            let _ = c.tx.send(msg);
        }
    }
}
