//! ID 管理（users.csv）と認証情報（credentials.json）
//!
//! - users.csv: ID, 名前, ふりがな, メールアドレス, 初期パスワード
//! - credentials.json: パスワードハッシュ(Argon2)・admin・無効化フラグ
//!   初期パスワードは初回ログイン時の変更後に CSV から消去する。
//! - ID 0000 はスーパーユーザー（最上位ロール）で固定。起動時に無ければ作る。
//!   admin 権限は常に有効で、無効化・権限の解除・管理画面からのパスワードリセットはできない。

use anyhow::{anyhow, bail, Context};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// スーパーユーザーの ID（固定）
pub const SUPERUSER_ID: &str = "0000";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRow {
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
    pub id: String,
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
    pub id: String,
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
}

pub struct UserStore {
    csv_path: PathBuf,
    cred_path: PathBuf,
    inner: Mutex<Inner>,
}

pub enum LoginError {
    Invalid,
    Disabled,
}

impl UserStore {
    /// 読み込み。スーパーユーザー（0000）が居なければ作成し、その初期パスワードを返す
    pub fn open(data_dir: &Path, su: &crate::config::SuperuserCfg) -> anyhow::Result<(Self, Option<(String, String)>)> {
        std::fs::create_dir_all(data_dir)?;
        let csv_path = data_dir.join("users.csv");
        let cred_path = data_dir.join("credentials.json");

        let rows = if csv_path.exists() {
            let text = std::fs::read_to_string(&csv_path)?;
            let text = text.trim_start_matches('\u{feff}');
            let mut rdr = csv::Reader::from_reader(text.as_bytes());
            rdr.deserialize::<UserRow>()
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("{} の形式が不正です", csv_path.display()))?
        } else {
            vec![]
        };
        let creds: HashMap<String, Credential> = if cred_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&cred_path)?)
                .with_context(|| format!("{} の形式が不正です", cred_path.display()))?
        } else {
            HashMap::new()
        };

        let store = Self { csv_path, cred_path, inner: Mutex::new(Inner { rows, creds }) };
        let bootstrap = store.ensure_superuser(su)?;
        Ok((store, bootstrap))
    }

    /// 0000 を用意する。Macross.yaml の superuser の名前・ふりがなは 0000 を作るときだけ使う
    /// （その後は管理画面で変更できる）。メールアドレスは設定されていれば起動のたびに反映する
    fn ensure_superuser(&self, su: &crate::config::SuperuserCfg) -> anyhow::Result<Option<(String, String)>> {
        let mut inner = self.inner.lock().unwrap();
        let mut created = None;
        if !inner.rows.iter().any(|r| r.id == SUPERUSER_ID) {
            let password = random_password(10);
            let name = if su.name.trim().is_empty() { "スーパーユーザー".to_string() } else { su.name.trim().to_string() };
            inner.rows.insert(
                0,
                UserRow {
                    id: SUPERUSER_ID.into(),
                    name,
                    furigana: su.furigana.trim().into(),
                    email: su.email.trim().into(),
                    initial_password: password.clone(),
                },
            );
            created = Some((SUPERUSER_ID.to_string(), password));
        }
        let row = inner.rows.iter_mut().find(|r| r.id == SUPERUSER_ID).unwrap();
        if created.is_some() && !su.furigana.trim().is_empty() {
            row.furigana = su.furigana.trim().into();
        }
        if !su.email.trim().is_empty() {
            check_email(&su.email)?;
            row.email = su.email.trim().into();
        }
        let cred = inner.creds.entry(SUPERUSER_ID.into()).or_default();
        cred.admin = true;
        cred.disabled = false;
        cred.channels = None;
        self.save(&inner)?;
        Ok(created)
    }

    fn save(&self, inner: &Inner) -> anyhow::Result<()> {
        let mut wtr = csv::Writer::from_writer(vec![]);
        for r in &inner.rows {
            wtr.serialize(r)?;
        }
        if inner.rows.is_empty() {
            wtr.write_record(["ID", "名前", "ふりがな", "メールアドレス", "初期パスワード"])?;
        }
        let bytes = wtr.into_inner().map_err(|e| anyhow!(e.to_string()))?;
        write_atomic(&self.csv_path, &bytes)?;
        write_atomic(&self.cred_path, serde_json::to_string_pretty(&inner.creds)?.as_bytes())?;
        Ok(())
    }

    fn to_auth(row: &UserRow, cred: &Credential) -> AuthUser {
        let superuser = row.id == SUPERUSER_ID;
        AuthUser {
            id: row.id.clone(),
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

    pub fn authenticate(&self, id: &str, password: &str) -> Result<AuthUser, LoginError> {
        let inner = self.inner.lock().unwrap();
        let row = inner.rows.iter().find(|r| r.id == id).ok_or(LoginError::Invalid)?;
        let cred = inner.creds.get(id).cloned().unwrap_or_default();
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
        if self.authenticate(id, current).is_err() {
            bail!("現在のパスワードが違います");
        }
        let hash = hash_password(new)?;
        let mut inner = self.inner.lock().unwrap();
        inner.creds.entry(id.to_string()).or_default().password_hash = Some(hash);
        if let Some(row) = inner.rows.iter_mut().find(|r| r.id == id) {
            row.initial_password.clear();
        }
        self.save(&inner)
    }

    /// ID を自動発行してユーザを作成。初期パスワードを返す
    pub fn create_user(&self, name: &str, furigana: &str, email: &str) -> anyhow::Result<(String, String)> {
        let name = check_person_name(name)?;
        let name = name.as_str();
        check_email(email)?;
        let mut inner = self.inner.lock().unwrap();
        let next = inner.rows.iter().filter_map(|r| r.id.parse::<u32>().ok()).max().unwrap_or(0) + 1;
        let id = format!("{next:04}");
        let password = random_password(10);
        inner.rows.push(UserRow {
            id: id.clone(),
            name: name.to_string(),
            furigana: furigana.trim().to_string(),
            email: email.trim().to_string(),
            initial_password: password.clone(),
        });
        inner.creds.insert(id.clone(), Credential::default());
        self.save(&inner)?;
        Ok((id, password))
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

    /// 名前・ふりがなを変更する（None の項目はそのまま）。変更後の名前を返す
    pub fn update_profile(&self, id: &str, name: Option<&str>, furigana: Option<&str>) -> anyhow::Result<String> {
        let name = name.map(check_person_name).transpose()?;
        let furigana = furigana.map(|f| f.trim().to_string());
        if let Some(f) = &furigana {
            anyhow::ensure!(f.chars().count() <= NAME_MAX_CHARS, "ふりがなは{NAME_MAX_CHARS}文字以内にしてください");
            anyhow::ensure!(!f.chars().any(char::is_control), "ふりがなに使えない文字が含まれています");
        }
        let mut inner = self.inner.lock().unwrap();
        let row = inner.rows.iter_mut().find(|r| r.id == id).ok_or_else(|| anyhow!("ID {id} は存在しません"))?;
        if let Some(n) = name {
            row.name = n;
        }
        if let Some(f) = furigana {
            row.furigana = f;
        }
        let new_name = row.name.clone();
        self.save(&inner)?;
        Ok(new_name)
    }

    pub fn update_flags(&self, id: &str, admin: Option<bool>, disabled: Option<bool>) -> anyhow::Result<()> {
        if id == SUPERUSER_ID && (admin == Some(false) || disabled == Some(true)) {
            bail!("スーパーユーザー（{SUPERUSER_ID}）の権限は変更できません");
        }
        let mut inner = self.inner.lock().unwrap();
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
                    name: r.name.clone(),
                    furigana: r.furigana.clone(),
                    email: r.email.clone(),
                    admin: c.admin || r.id == SUPERUSER_ID,
                    disabled: c.disabled,
                    superuser: r.id == SUPERUSER_ID,
                    must_change: c.password_hash.is_none(),
                    channels: if c.admin || r.id == SUPERUSER_ID { None } else { c.channels.clone() },
                    initial_password: (c.password_hash.is_none() && !r.initial_password.is_empty())
                        .then(|| r.initial_password.clone()),
                }
            })
            .collect()
    }

    /// 使えるチャンネル（None = すべて。admin・スーパーユーザーは常にすべて）
    pub fn channels_of(&self, id: &str) -> Option<Vec<u32>> {
        if id == SUPERUSER_ID {
            return None;
        }
        let inner = self.inner.lock().unwrap();
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
pub const IMPORT_MAX_ROWS: usize = 300;

pub struct ImportRow {
    pub name: String,
    pub furigana: String,
    pub email: String,
}

fn check_email(email: &str) -> anyhow::Result<()> {
    let e = email.trim();
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
        if name.is_empty() {
            errors.push(format!("{line} 行目: 名前が空です"));
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
    if rows.len() > IMPORT_MAX_ROWS {
        bail!("一度に登録できるのは {IMPORT_MAX_ROWS} 人までです");
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
