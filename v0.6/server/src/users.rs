//! ID 管理（users.csv）と認証情報（credentials.json）
//!
//! - users.csv: ID, 名前, ふりがな, メールアドレス, 初期パスワード
//!   ID は 16 桁の [内部 ID 8 桁][イベント ID 4 桁][表示 ID 4 桁]。
//!   - 内部 ID: 16 進数 8 桁の乱数。発行後は変わらない。認証情報・セッション・無線の接続はこれにひもづける
//!   - イベント ID: 10 進数 4 桁。イベントごとに管理画面で登録する（data/events.json）。0000 は既定のイベント
//!   - 表示 ID: 10 進数 4 桁。イベントの中での連番。削除で欠番ができたら「番号を詰める」で付け直す
//!   ログイン ID は [イベント ID][表示 ID] の 8 桁（画面では 0001-0012 のように表示。先頭の 0 は省略可）
//! - credentials.json: 内部 ID → パスワードハッシュ(Argon2)・admin・無効化フラグ
//!   初期パスワードは初回ログイン時の変更後に CSV から消去する。
//! - 0000-0000 はスーパーユーザー（最上位ロール）で固定。起動時に無ければ作る。
//!   admin 権限は常に有効で、無効化・権限の解除・削除・管理画面からのパスワードリセットはできない。
//! - 旧形式（4 桁の ID）の users.csv は起動時に変換する（イベント 0000・表示 ID = 旧番号、内部 ID は新しく発行）

use anyhow::{anyhow, bail, Context};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// スーパーユーザーのログイン ID（画面表示用。固定）
pub const SUPERUSER_ID: &str = "0000-0000";
/// スーパーユーザーの [イベント ID][表示 ID]
const SU_KEY: &str = "00000000";
/// 既定のイベント（既存の ID・スーパーユーザー）
pub const DEFAULT_EVENT: &str = "0000";

/// メモリ上の 1 人分（id = 内部 ID、display = [イベント ID 4 桁][表示 ID 4 桁]）
#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: String,
    pub display: String,
    pub name: String,
    pub furigana: String,
    pub email: String,
    pub initial_password: String,
}

impl UserRow {
    fn superuser(&self) -> bool {
        self.display == SU_KEY
    }
    /// 16 桁の ID（[内部 ID][イベント ID][表示 ID]）
    pub fn uid(&self) -> String {
        format!("{}{}", self.id, self.display)
    }
    fn event(&self) -> &str {
        &self.display[..4]
    }
    fn number(&self) -> &str {
        &self.display[4..]
    }
    /// 画面に出すログイン ID（0001-0012）
    fn shown(&self) -> String {
        show_key(&self.display)
    }
}

/// [イベント ID][表示 ID] の 8 桁を 0001-0012 の形にする
fn show_key(key: &str) -> String {
    if key.len() == 8 { format!("{}-{}", &key[..4], &key[4..]) } else { key.to_string() }
}

/// イベント（data/events.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub name: String,
}

/// 管理画面向けのイベント一覧
#[derive(Debug, Clone, Serialize)]
pub struct EventView {
    pub id: String,
    pub name: String,
    /// 登録されている人数
    pub users: usize,
    /// 表示 ID の欠番の数
    pub gaps: usize,
}

/// users.csv の 1 行
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CsvRow {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "名前")]
    pub name: String,
    #[serde(rename = "ふりがな")]
    pub furigana: String,
    #[serde(rename = "メールアドレス")]
    pub email: String,
    #[serde(rename = "初期パスワード")]
    pub initial_password: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credential {
    #[serde(default)]
    pub password_hash: Option<String>,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub disabled: bool,
    /// 使えるチャンネル（None = すべて）。admin は常にすべて
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<Vec<u32>>,
}

/// ログイン中ユーザの情報
#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    /// 内部 ID（API・接続の識別に使う）
    pub id: String,
    /// ログイン ID（0001-0012。ログイン・画面表示に使う）
    pub display_id: String,
    pub name: String,
    pub admin: bool,
    /// ID 0000（最上位ロール）
    pub superuser: bool,
    /// 初期パスワードのまま（パスワード変更が必要）
    pub must_change: bool,
}

impl AuthUser {
    /// 発言の割り込みの強さ（スーパーユーザー 2 / admin 1 / 一般 0）
    pub fn rank(&self) -> u8 {
        if self.superuser { 2 } else if self.admin { 1 } else { 0 }
    }
}

/// 管理画面向けの一覧表示用
#[derive(Debug, Clone, Serialize)]
pub struct UserView {
    /// 内部 ID
    pub id: String,
    /// ログイン ID（0001-0012）
    pub display_id: String,
    /// イベント ID
    pub event_id: String,
    /// 表示 ID（イベントの中での番号）
    pub number: String,
    /// 16 桁の ID（[内部 ID][イベント ID][表示 ID]）
    pub uid: String,
    pub name: String,
    pub furigana: String,
    pub email: String,
    pub admin: bool,
    pub disabled: bool,
    pub superuser: bool,
    pub must_change: bool,
    /// 使えるチャンネル（None = すべて）
    pub channels: Option<Vec<u32>>,
    /// 初期パスワード（本人が変更するまで管理画面で確認できる）
    pub initial_password: Option<String>,
}

struct Inner {
    rows: Vec<UserRow>,
    creds: HashMap<String, Credential>,
    events: Vec<Event>,
}

pub struct UserStore {
    csv_path: PathBuf,
    cred_path: PathBuf,
    events_path: PathBuf,
    inner: Mutex<Inner>,
}

pub enum LoginError {
    Invalid,
    Disabled,
}

impl UserStore {
    /// 読み込み。スーパーユーザー（0000-0000）が居なければ作成し、その初期パスワードを返す。
    /// 旧形式の ID を変換したときは (旧 ID, 内部用 ID) の一覧も返す（ログインセッションの付け替えに使う）
    pub fn open(
        data_dir: &Path,
        su: &crate::config::SuperuserCfg,
    ) -> anyhow::Result<(Self, Option<(String, String)>, Vec<(String, String)>)> {
        std::fs::create_dir_all(data_dir)?;
        let csv_path = data_dir.join("users.csv");
        let cred_path = data_dir.join("credentials.json");
        let events_path = data_dir.join("events.json");
        let mut events: Vec<Event> = if events_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&events_path)?)
                .with_context(|| format!("{} の形式が不正です", events_path.display()))?
        } else {
            vec![]
        };

        let rows = if csv_path.exists() {
            let text = std::fs::read_to_string(&csv_path)?;
            let text = text.trim_start_matches('\u{feff}');
            let mut rdr = csv::Reader::from_reader(text.as_bytes());
            rdr.deserialize::<CsvRow>()
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("{} の形式が不正です", csv_path.display()))?
        } else {
            vec![]
        };
        let mut creds: HashMap<String, Credential> = if cred_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&cred_path)?)
                .with_context(|| format!("{} の形式が不正です", cred_path.display()))?
        } else {
            HashMap::new()
        };

        // 16 桁の ID はそのまま分ける。旧形式（4 桁）はイベント 0000・表示 ID = 旧番号、内部 ID = 新しく発行
        let mut migrated = vec![];
        let mut out: Vec<UserRow> = vec![];
        for r in rows {
            let id = r.id.trim().to_string();
            let (internal, display) = if id.len() == 16 && id.is_ascii() && is_internal_id(&id[..8]) && is_key(&id[8..]) {
                (id[..8].to_lowercase(), id[8..].to_string())
            } else {
                let n: u64 = id.parse().with_context(|| format!("users.csv の ID「{id}」を変換できません"))?;
                anyhow::ensure!(n <= 9_999, "users.csv の ID「{id}」が大きすぎます（表示 ID は 4 桁まで）");
                let internal = new_internal_id(&out);
                migrated.push((id.clone(), internal.clone()));
                (internal, format!("{DEFAULT_EVENT}{n:04}"))
            };
            anyhow::ensure!(!out.iter().any(|o| o.id == internal || o.display == display), "users.csv の ID「{id}」が重複しています");
            out.push(UserRow {
                id: internal,
                display,
                name: r.name,
                furigana: r.furigana,
                email: r.email,
                initial_password: r.initial_password,
            });
        }
        if !migrated.is_empty() {
            // 認証情報を新しい内部用 ID で作り直す（旧 ID と新 ID の文字列が偶然同じでも混ざらないように）
            let mut renamed = HashMap::new();
            for row in &out {
                let old = migrated.iter().find(|(_, n)| *n == row.id).map(|(o, _)| o.as_str()).unwrap_or(&row.id);
                if let Some(c) = creds.get(old) {
                    renamed.insert(row.id.clone(), c.clone());
                }
            }
            creds = renamed;
        }
        out.sort_by(|a, b| a.display.cmp(&b.display));
        if !migrated.is_empty() {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            let _ = std::fs::copy(&csv_path, data_dir.join(format!("users.csv.before-id16-{stamp}")));
            let _ = std::fs::copy(&cred_path, data_dir.join(format!("credentials.json.before-id16-{stamp}")));
            tracing::warn!("ID を 16 桁（[内部 ID][イベント ID][表示 ID]）に変換しました: {} 人（元のファイルは *.before-id16-{stamp}）", migrated.len());
        }
        // 既定のイベント 0000 と、ユーザーが使っているイベントを必ず用意する
        if !events.iter().any(|e| e.id == DEFAULT_EVENT) {
            events.insert(0, Event { id: DEFAULT_EVENT.into(), name: "既定".into() });
        }
        for r in &out {
            if !events.iter().any(|e| e.id == r.event()) {
                events.push(Event { id: r.event().to_string(), name: format!("イベント {}", r.event()) });
            }
        }
        events.sort_by(|a, b| a.id.cmp(&b.id));

        let store = Self { csv_path, cred_path, events_path, inner: Mutex::new(Inner { rows: out, creds, events }) };
        let bootstrap = store.ensure_superuser(su)?;
        Ok((store, bootstrap, migrated))
    }

    /// 0000-0000 を用意する。Macross.yaml の superuser の名前・ふりがな・メールアドレスは作るときだけ使う
    /// （その後は管理画面で変更できる）
    fn ensure_superuser(&self, su: &crate::config::SuperuserCfg) -> anyhow::Result<Option<(String, String)>> {
        let mut inner = self.inner.lock().unwrap();
        let mut created = None;
        if !inner.rows.iter().any(|r| r.superuser()) {
            let password = random_password(10);
            let name = if su.name.trim().is_empty() { "スーパーユーザー".to_string() } else { su.name.trim().to_string() };
            let id = new_internal_id(&inner.rows);
            inner.rows.insert(
                0,
                UserRow {
                    id,
                    display: SU_KEY.into(),
                    name,
                    furigana: su.furigana.trim().into(),
                    email: su.email.trim().into(),
                    initial_password: password.clone(),
                },
            );
            created = Some((SUPERUSER_ID.to_string(), password));
        }
        let row = inner.rows.iter_mut().find(|r| r.superuser()).unwrap();
        if created.is_some() && !su.furigana.trim().is_empty() {
            row.furigana = su.furigana.trim().into();
        }
        if created.is_some() && !su.email.trim().is_empty() {
            check_email(&su.email)?;
            row.email = su.email.trim().into();
        }
        let su_id = row.id.clone();
        let cred = inner.creds.entry(su_id).or_default();
        cred.admin = true;
        cred.disabled = false;
        cred.channels = None;
        self.save(&inner)?;
        Ok(created)
    }

    fn save(&self, inner: &Inner) -> anyhow::Result<()> {
        let mut wtr = csv::Writer::from_writer(vec![]);
        for r in &inner.rows {
            wtr.serialize(CsvRow {
                id: r.uid(),
                name: r.name.clone(),
                furigana: r.furigana.clone(),
                email: r.email.clone(),
                initial_password: r.initial_password.clone(),
            })?;
        }
        if inner.rows.is_empty() {
            wtr.write_record(["ID", "名前", "ふりがな", "メールアドレス", "初期パスワード"])?;
        }
        let bytes = wtr.into_inner().map_err(|e| anyhow!(e.to_string()))?;
        write_atomic(&self.csv_path, &bytes)?;
        write_atomic(&self.cred_path, serde_json::to_string_pretty(&inner.creds)?.as_bytes())?;
        write_atomic(&self.events_path, serde_json::to_string_pretty(&inner.events)?.as_bytes())?;
        Ok(())
    }

    /// イベントの一覧（人数・欠番の数つき）
    pub fn events(&self) -> Vec<EventView> {
        let inner = self.inner.lock().unwrap();
        inner
            .events
            .iter()
            .map(|e| {
                let nums: Vec<u32> = inner
                    .rows
                    .iter()
                    .filter(|r| r.event() == e.id && !r.superuser())
                    .filter_map(|r| r.number().parse().ok())
                    .collect();
                let max = nums.iter().copied().max().unwrap_or(0) as usize;
                EventView {
                    id: e.id.clone(),
                    name: e.name.clone(),
                    users: inner.rows.iter().filter(|r| r.event() == e.id).count(),
                    gaps: max.saturating_sub(nums.len()),
                }
            })
            .collect()
    }

    /// イベントを登録する。ID を省略（空）すると、いまの最大 + 1 を使う
    pub fn create_event(&self, id: &str, name: &str) -> anyhow::Result<Event> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "イベント名を入力してください");
        anyhow::ensure!(name.chars().count() <= NAME_MAX_CHARS, "イベント名は{NAME_MAX_CHARS}文字以内にしてください");
        anyhow::ensure!(!name.chars().any(char::is_control), "イベント名に使えない文字が含まれています");
        let mut inner = self.inner.lock().unwrap();
        let id = match id.trim() {
            "" => {
                let max = inner.events.iter().filter_map(|e| e.id.parse::<u32>().ok()).max().unwrap_or(0);
                anyhow::ensure!(max < 9999, "イベント ID の上限に達しました");
                format!("{:04}", max + 1)
            }
            s => {
                anyhow::ensure!(s.len() <= 4 && s.chars().all(|c| c.is_ascii_digit()), "イベント ID は 4 桁の数字にしてください");
                format!("{:04}", s.parse::<u32>()?)
            }
        };
        anyhow::ensure!(!inner.events.iter().any(|e| e.id == id), "イベント ID {id} はすでに登録されています");
        let ev = Event { id, name: name.to_string() };
        inner.events.push(ev.clone());
        inner.events.sort_by(|a, b| a.id.cmp(&b.id));
        self.save(&inner)?;
        Ok(ev)
    }

    /// イベント名を変える
    pub fn rename_event(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "イベント名を入力してください");
        anyhow::ensure!(name.chars().count() <= NAME_MAX_CHARS, "イベント名は{NAME_MAX_CHARS}文字以内にしてください");
        let mut inner = self.inner.lock().unwrap();
        let ev = inner.events.iter_mut().find(|e| e.id == id).ok_or_else(|| anyhow!("イベント {id} は存在しません"))?;
        ev.name = name.to_string();
        self.save(&inner)
    }

    /// イベントを削除する（登録されている人がいない場合のみ。0000 は削除できない）
    pub fn delete_event(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(id != DEFAULT_EVENT, "既定のイベント（{DEFAULT_EVENT}）は削除できません");
        let mut inner = self.inner.lock().unwrap();
        let n = inner.rows.iter().filter(|r| r.event() == id).count();
        anyhow::ensure!(n == 0, "イベント {id} には {n} 人が登録されています（先にユーザーを削除してください）");
        let before = inner.events.len();
        inner.events.retain(|e| e.id != id);
        anyhow::ensure!(inner.events.len() != before, "イベント {id} は存在しません");
        self.save(&inner)
    }

    fn check_event(inner: &Inner, event: &str) -> anyhow::Result<String> {
        let e = event.trim();
        let e = if e.is_empty() { DEFAULT_EVENT.to_string() } else { format!("{:04}", e.parse::<u32>().map_err(|_| anyhow!("イベント ID が不正です"))?) };
        anyhow::ensure!(inner.events.iter().any(|x| x.id == e), "イベント {e} は登録されていません（管理画面の「イベント」で登録してください）");
        Ok(e)
    }

    fn to_auth(row: &UserRow, cred: &Credential) -> AuthUser {
        let superuser = row.superuser();
        AuthUser {
            id: row.id.clone(),
            display_id: row.shown(),
            name: row.name.clone(),
            admin: cred.admin || superuser,
            superuser,
            must_change: cred.password_hash.is_none(),
        }
    }

    pub fn get(&self, id: &str) -> Option<AuthUser> {
        let inner = self.inner.lock().unwrap();
        let row = inner.rows.iter().find(|r| r.id == id)?;
        let cred = inner.creds.get(id).cloned().unwrap_or_default();
        if cred.disabled {
            return None;
        }
        Some(Self::to_auth(row, &cred))
    }

    /// ログイン。ID は表示用（先頭の 0 は省略可。例: 12 = 00000012）または 12 桁
    pub fn authenticate(&self, login_id: &str, password: &str) -> Result<AuthUser, LoginError> {
        let inner = self.inner.lock().unwrap();
        let row = resolve_login(&inner.rows, login_id).ok_or(LoginError::Invalid)?;
        Self::check_password(&inner, row, password)
    }

    fn check_password(inner: &Inner, row: &UserRow, password: &str) -> Result<AuthUser, LoginError> {
        let cred = inner.creds.get(&row.id).cloned().unwrap_or_default();
        let ok = match &cred.password_hash {
            Some(h) => verify_password(h, password),
            None => !row.initial_password.is_empty() && row.initial_password == password,
        };
        if !ok {
            return Err(LoginError::Invalid);
        }
        if cred.disabled {
            return Err(LoginError::Disabled);
        }
        Ok(Self::to_auth(row, &cred))
    }

    pub fn change_password(&self, id: &str, current: &str, new: &str) -> anyhow::Result<()> {
        if new.chars().count() < 8 {
            bail!("パスワードは8文字以上にしてください");
        }
        {
            let inner = self.inner.lock().unwrap();
            let row = inner.rows.iter().find(|r| r.id == id).ok_or_else(|| anyhow!("ユーザーが存在しません"))?;
            if Self::check_password(&inner, row, current).is_err() {
                bail!("現在のパスワードが違います");
            }
        }
        let hash = hash_password(new)?;
        let mut inner = self.inner.lock().unwrap();
        inner.creds.entry(id.to_string()).or_default().password_hash = Some(hash);
        if let Some(row) = inner.rows.iter_mut().find(|r| r.id == id) {
            row.initial_password.clear();
        }
        self.save(&inner)
    }

    /// イベントの中で ID を自動発行してユーザを作成。(内部 ID, ログイン ID, 初期パスワード) を返す
    pub fn create_user(&self, event: &str, name: &str, furigana: &str, email: &str) -> anyhow::Result<(String, String, String)> {
        let name = check_person_name(name)?;
        let name = name.as_str();
        check_email(email)?;
        let mut inner = self.inner.lock().unwrap();
        let event = Self::check_event(&inner, event)?;
        let display = next_display(&inner.rows, &event)?;
        let id = new_internal_id(&inner.rows);
        let password = random_password(10);
        inner.rows.push(UserRow {
            id: id.clone(),
            display: display.clone(),
            name: name.to_string(),
            furigana: furigana.trim().to_string(),
            email: email.trim().to_string(),
            initial_password: password.clone(),
        });
        inner.creds.insert(id.clone(), Credential::default());
        self.save(&inner)?;
        Ok((id, show_key(&display), password))
    }

    /// 複数のユーザをまとめて作成（CSV 一括登録）。先に全員を検証し、1 回の保存で全員を登録する
    /// （途中で止まって一部だけ登録される、ということが起きない）。(内部 ID, ログイン ID, 初期パスワード) を行の順に返す
    pub fn create_users(&self, event: &str, rows: &[ImportRow]) -> anyhow::Result<Vec<(String, String, String)>> {
        let mut checked = Vec::with_capacity(rows.len());
        let mut errors = vec![];
        for (i, r) in rows.iter().enumerate() {
            match check_person_name(&r.name).and_then(|n| check_email(&r.email).map(|_| n)) {
                Ok(n) => checked.push(n),
                Err(e) => errors.push(format!("{} 人目（{}）: {e}", i + 1, r.name)),
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join(" / "));
        }
        let mut inner = self.inner.lock().unwrap();
        let event = Self::check_event(&inner, event)?;
        let free = 9999 - inner.rows.iter().filter(|r| r.event() == event).filter_map(|r| r.number().parse::<usize>().ok()).max().unwrap_or(0);
        anyhow::ensure!(rows.len() <= free, "イベント {event} に登録できるのはあと {free} 人です（表示 ID は 4 桁まで）");
        let mut out = Vec::with_capacity(rows.len());
        for (r, name) in rows.iter().zip(checked) {
            let display = next_display(&inner.rows, &event)?;
            let id = new_internal_id(&inner.rows);
            let password = random_password(10);
            inner.rows.push(UserRow {
                id: id.clone(),
                display: display.clone(),
                name,
                furigana: r.furigana.trim().to_string(),
                email: r.email.trim().to_string(),
                initial_password: password.clone(),
            });
            inner.creds.insert(id.clone(), Credential::default());
            out.push((id, show_key(&display), password));
        }
        self.save(&inner)?;
        Ok(out)
    }

    /// パスワードをリセットし、新しい初期パスワードを返す（次回ログイン時に変更を求める）
    pub fn reset_password(&self, id: &str) -> anyhow::Result<String> {
        let mut inner = self.inner.lock().unwrap();
        let password = random_password(10);
        let row = inner.rows.iter_mut().find(|r| r.id == id).ok_or_else(|| anyhow!("ID {id} は存在しません"))?;
        row.initial_password = password.clone();
        inner.creds.entry(id.to_string()).or_default().password_hash = None;
        self.save(&inner)?;
        Ok(password)
    }

    /// 名前とメールアドレス
    pub fn contact(&self, id: &str) -> Option<(String, String)> {
        let inner = self.inner.lock().unwrap();
        inner.rows.iter().find(|r| r.id == id).map(|r| (r.name.clone(), r.email.clone()))
    }

    /// 表示用 ID（無ければ内部用 ID をそのまま返す）
    pub fn display_of(&self, id: &str) -> String {
        let inner = self.inner.lock().unwrap();
        inner.rows.iter().find(|r| r.id == id).map(|r| r.shown()).unwrap_or_else(|| id.to_string())
    }

    /// スーパーユーザーか（内部用 ID で）
    pub fn is_superuser(&self, id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.rows.iter().any(|r| r.id == id && r.superuser())
    }

    /// スーパーユーザーの内部用 ID
    pub fn superuser_id(&self) -> String {
        let inner = self.inner.lock().unwrap();
        inner.rows.iter().find(|r| r.superuser()).map(|r| r.id.clone()).unwrap_or_default()
    }

    /// ユーザーを削除する（スーパーユーザーは削除できない）。削除した人の (内部用 ID, 表示用 ID, 名前) を返す。
    /// 表示用 ID は詰めない（欠番は「番号を詰める」で付け直す）
    pub fn delete_users(&self, ids: &[String]) -> anyhow::Result<Vec<(String, String, String)>> {
        let mut inner = self.inner.lock().unwrap();
        anyhow::ensure!(
            !inner.rows.iter().any(|r| r.superuser() && ids.contains(&r.id)),
            "スーパーユーザーは削除できません"
        );
        let mut out = vec![];
        for id in ids {
            let Some(pos) = inner.rows.iter().position(|r| &r.id == id) else { continue };
            let r = inner.rows.remove(pos);
            inner.creds.remove(&r.id);
            out.push((r.id.clone(), r.shown(), r.name));
        }
        if !out.is_empty() {
            self.save(&inner)?;
        }
        Ok(out)
    }

    /// 欠番の数（全イベントの合計）
    pub fn gaps(&self) -> usize {
        self.events().iter().map(|e| e.gaps).sum()
    }

    /// イベントの中で表示 ID を 0001, 0002… の連番に付け直す（スーパーユーザーは 0000-0000 のまま。いまの番号の順を保つ）。
    /// 変わった人の (内部 ID, 旧ログイン ID, 新ログイン ID) を返す
    pub fn renumber(&self, event: &str) -> anyhow::Result<Vec<(String, String, String)>> {
        let mut inner = self.inner.lock().unwrap();
        let event = Self::check_event(&inner, event)?;
        inner.rows.sort_by(|a, b| a.display.cmp(&b.display));
        let mut changed = vec![];
        let mut n = 0u32;
        for r in inner.rows.iter_mut() {
            if r.superuser() || r.event() != event {
                continue;
            }
            n += 1;
            let d = format!("{event}{n:04}");
            if r.display != d {
                let old = std::mem::replace(&mut r.display, d.clone());
                changed.push((r.id.clone(), show_key(&old), show_key(&d)));
            }
        }
        if !changed.is_empty() {
            self.save(&inner)?;
        }
        Ok(changed)
    }

    /// 名前・ふりがなを変更する（None の項目はそのまま）。変更後の名前を返す
    /// 名前・ふりがな・メールアドレスの変更（None の項目はそのまま。メールアドレスは空にもできる）
    pub fn update_profile(&self, id: &str, name: Option<&str>, furigana: Option<&str>, email: Option<&str>) -> anyhow::Result<String> {
        let name = name.map(check_person_name).transpose()?;
        let furigana = furigana.map(|f| f.trim().to_string());
        if let Some(f) = &furigana {
            anyhow::ensure!(f.chars().count() <= NAME_MAX_CHARS, "ふりがなは{NAME_MAX_CHARS}文字以内にしてください");
            anyhow::ensure!(!f.chars().any(char::is_control), "ふりがなに使えない文字が含まれています");
        }
        if let Some(e) = email {
            check_email(e)?;
        }
        let mut inner = self.inner.lock().unwrap();
        let row = inner.rows.iter_mut().find(|r| r.id == id).ok_or_else(|| anyhow!("ID {id} は存在しません"))?;
        if let Some(n) = name {
            row.name = n;
        }
        if let Some(f) = furigana {
            row.furigana = f;
        }
        if let Some(e) = email {
            row.email = e.trim().to_string();
        }
        let new_name = row.name.clone();
        self.save(&inner)?;
        Ok(new_name)
    }

    pub fn update_flags(&self, id: &str, admin: Option<bool>, disabled: Option<bool>) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.rows.iter().any(|r| r.id == id && r.superuser()) && (admin == Some(false) || disabled == Some(true)) {
            bail!("スーパーユーザー（{SUPERUSER_ID}）の権限は変更できません");
        }
        if !inner.rows.iter().any(|r| r.id == id) {
            bail!("ID {id} は存在しません");
        }
        let cred = inner.creds.entry(id.to_string()).or_default();
        if let Some(a) = admin {
            cred.admin = a;
        }
        if let Some(d) = disabled {
            cred.disabled = d;
        }
        self.save(&inner)
    }

    pub fn list(&self) -> Vec<UserView> {
        let inner = self.inner.lock().unwrap();
        inner
            .rows
            .iter()
            .map(|r| {
                let c = inner.creds.get(&r.id).cloned().unwrap_or_default();
                UserView {
                    id: r.id.clone(),
                    display_id: r.shown(),
                    event_id: r.event().to_string(),
                    number: r.number().to_string(),
                    uid: r.uid(),
                    name: r.name.clone(),
                    furigana: r.furigana.clone(),
                    email: r.email.clone(),
                    admin: c.admin || r.superuser(),
                    disabled: c.disabled,
                    superuser: r.superuser(),
                    must_change: c.password_hash.is_none(),
                    channels: if c.admin || r.superuser() { None } else { c.channels.clone() },
                    initial_password: (c.password_hash.is_none() && !r.initial_password.is_empty())
                        .then(|| r.initial_password.clone()),
                }
            })
            .collect()
    }

    /// 使えるチャンネル（None = すべて。admin・スーパーユーザーは常にすべて）
    pub fn channels_of(&self, id: &str) -> Option<Vec<u32>> {
        let inner = self.inner.lock().unwrap();
        if inner.rows.iter().any(|r| r.id == id && r.superuser()) {
            return None;
        }
        let c = inner.creds.get(id)?;
        if c.admin { None } else { c.channels.clone() }
    }

    /// 使えるチャンネルを設定（None = すべて）
    pub fn set_channels(&self, id: &str, channels: Option<Vec<u32>>) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.rows.iter().any(|r| r.id == id) {
            bail!("ID {id} は存在しません");
        }
        let channels = channels.map(|mut v| {
            v.sort_unstable();
            v.dedup();
            v
        });
        inner.creds.entry(id.to_string()).or_default().channels = channels;
        self.save(&inner)
    }

    /// 削除したチャンネルを全員の設定から外す（同じ番号で作り直したチャンネルに権限が残らないように）
    pub fn forget_channel(&self, channel: u32) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut changed = false;
        for c in inner.creds.values_mut() {
            if let Some(list) = &mut c.channels {
                let before = list.len();
                list.retain(|x| *x != channel);
                changed |= list.len() != before;
            }
        }
        if changed {
            self.save(&inner)?;
        }
        Ok(())
    }
}

/// 一括登録用 CSV（テンプレートと同じ列）
pub const IMPORT_HEADER: [&str; 3] = ["名前", "ふりがな", "メールアドレス"];
/// テンプレートの記入例の行（この文字で始まる名前の行は登録しない）
pub const IMPORT_EXAMPLE_PREFIX: &str = "（例）";

pub struct ImportRow {
    pub name: String,
    pub furigana: String,
    pub email: String,
}

fn check_email(email: &str) -> anyhow::Result<()> {
    let e = email.trim();
    if e.chars().count() > 254 {
        bail!("メールアドレスが長すぎます");
    }
    if !e.is_empty() && e.parse::<lettre::Address>().is_err() {
        bail!("メールアドレスの形式が不正です: {e}");
    }
    Ok(())
}

/// CSV を読み、全行を検証してから返す（1 行でも不正なら何も登録しない）
pub fn parse_import(text: &str) -> anyhow::Result<Vec<ImportRow>> {
    let text = text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new().flexible(true).trim(csv::Trim::All).from_reader(text.as_bytes());
    let headers = rdr.headers()?.clone();
    let col = |names: &[&str]| headers.iter().position(|h| names.contains(&h));
    let name_col = col(&["名前", "name"]).ok_or_else(|| anyhow!("「名前」列がありません（テンプレートを使ってください）"))?;
    let furi_col = col(&["ふりがな", "furigana"]);
    let mail_col = col(&["メールアドレス", "email"]);
    let mut rows = vec![];
    let mut errors = vec![];
    for (i, rec) in rdr.records().enumerate() {
        let line = i + 2;
        let rec = rec.with_context(|| format!("{line} 行目を読めません"))?;
        let get = |c: Option<usize>| c.and_then(|c| rec.get(c)).unwrap_or("").to_string();
        let name = get(Some(name_col));
        if rec.iter().all(|f| f.is_empty()) || name.starts_with(IMPORT_EXAMPLE_PREFIX) {
            continue;
        }
        let email = get(mail_col);
        if let Err(e) = check_person_name(&name) {
            errors.push(format!("{line} 行目: {e}"));
        }
        if let Err(e) = check_email(&email) {
            errors.push(format!("{line} 行目: {e}"));
        }
        rows.push(ImportRow { name, furigana: get(furi_col), email });
    }
    if !errors.is_empty() {
        bail!("{}", errors.join(" / "));
    }
    if rows.is_empty() {
        bail!("登録する行がありません");
    }
    Ok(rows)
}

fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("パスワードのハッシュ化に失敗: {e}"))
}

fn verify_password(hash: &str, pw: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// 内部 ID（16 進数 8 桁）か
fn is_internal_id(s: &str) -> bool {
    s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// [イベント ID 4 桁][表示 ID 4 桁]（10 進数 8 桁）か
fn is_key(s: &str) -> bool {
    s.len() == 8 && s.chars().all(|c| c.is_ascii_digit())
}

/// 内部 ID を発行（16 進数 8 桁の乱数・重複なし）
fn new_internal_id(rows: &[UserRow]) -> String {
    let mut rng = rand::thread_rng();
    loop {
        let id = format!("{:08x}", rng.gen::<u32>());
        if !rows.iter().any(|r| r.id == id) {
            return id;
        }
    }
}

/// イベントの中の次の [イベント ID][表示 ID]（いまの最大 + 1）
fn next_display(rows: &[UserRow], event: &str) -> anyhow::Result<String> {
    let max = rows.iter().filter(|r| r.event() == event).filter_map(|r| r.number().parse::<u32>().ok()).max().unwrap_or(0);
    anyhow::ensure!(max < 9999, "イベント {event} の表示 ID の上限（9999）に達しました");
    Ok(format!("{event}{:04}", max + 1))
}

/// ログイン ID から利用者を探す: [イベント ID][表示 ID]（0001-0012 / 00010012。先頭の 0 は省略可）
/// または 16 桁（[内部 ID][イベント ID][表示 ID]）
fn resolve_login<'a>(rows: &'a [UserRow], input: &str) -> Option<&'a UserRow> {
    let s: String = input.trim().chars().filter(|c| !matches!(c, '-' | ' ' | '－' | 'ー')).collect();
    let s = s.as_str();
    if s.len() == 16 && s.is_ascii() {
        let (i, d) = s.split_at(8);
        return rows.iter().find(|r| r.id.eq_ignore_ascii_case(i) && r.display == d);
    }
    if s.is_empty() || s.len() > 8 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let d = format!("{:08}", s.parse::<u64>().ok()?);
    rows.iter().find(|r| r.display == d)
}

/// 紛らわしい文字（0/O, 1/l/I）を除いた英数字
fn random_password(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..len).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

/// 名前・ふりがなの最大文字数
pub const NAME_MAX_CHARS: usize = 40;

/// 人の名前の確認（空・長すぎ・制御文字を弾く）
fn check_person_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "名前を入力してください");
    anyhow::ensure!(name.chars().count() <= NAME_MAX_CHARS, "名前は{NAME_MAX_CHARS}文字以内にしてください");
    anyhow::ensure!(!name.chars().any(char::is_control), "名前に使えない文字が含まれています");
    Ok(name.to_string())
}

pub fn random_token(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..len).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
