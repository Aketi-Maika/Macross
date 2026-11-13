//! ログインセッション（data/sessions.json に保存。サーバを再起動してもログインしたまま）
//!
//! ファイルにはトークンそのものではなく SHA-256 のハッシュだけを書く
//! （ファイルを読まれても、そのままではログインに使えない）。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    user: String,
    /// 期限（UNIX 秒）
    expires: i64,
}

pub struct Sessions {
    path: PathBuf,
    ttl_secs: i64,
    inner: Mutex<HashMap<String, Entry>>,
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Sessions {
    pub fn open(path: PathBuf, days: u32) -> Self {
        let mut map: HashMap<String, Entry> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let t = now();
        map.retain(|_, e| e.expires > t);
        let s = Self { path, ttl_secs: i64::from(days.max(1)) * 86_400, inner: Mutex::new(map) };
        let n = s.inner.lock().unwrap().len();
        if n > 0 {
            tracing::info!("ログインセッションを {n} 件読み込みました");
        }
        s.save(&s.inner.lock().unwrap());
        s
    }

    /// Cookie の Max-Age（秒）
    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    pub fn insert(&self, token: &str, user: &str) {
        let mut m = self.inner.lock().unwrap();
        let t = now();
        m.retain(|_, e| e.expires > t);
        m.insert(hash(token), Entry { user: user.into(), expires: t + self.ttl_secs });
        self.save(&m);
    }

    /// トークンのユーザ ID（期限切れ・不明なら None）
    pub fn get(&self, token: &str) -> Option<String> {
        let m = self.inner.lock().unwrap();
        m.get(&hash(token)).filter(|e| e.expires > now()).map(|e| e.user.clone())
    }

    /// 残りが半分を切っていたら期限を延ばす（延ばしたら true。呼び出し側で Cookie も出し直す）
    pub fn touch(&self, token: &str) -> bool {
        let mut m = self.inner.lock().unwrap();
        let t = now();
        let ttl = self.ttl_secs;
        let Some(e) = m.get_mut(&hash(token)) else { return false };
        if e.expires <= t || e.expires - t > ttl / 2 {
            return false;
        }
        e.expires = t + ttl;
        self.save(&m);
        true
    }

    pub fn remove(&self, token: &str) {
        let mut m = self.inner.lock().unwrap();
        if m.remove(&hash(token)).is_some() {
            self.save(&m);
        }
    }

    /// そのユーザのセッションをすべて破棄（無効化・パスワードリセット時）
    pub fn remove_user(&self, user: &str) {
        let mut m = self.inner.lock().unwrap();
        let before = m.len();
        m.retain(|_, e| e.user != user);
        if m.len() != before {
            self.save(&m);
        }
    }

    fn save(&self, m: &HashMap<String, Entry>) {
        let result = serde_json::to_vec(m).map_err(anyhow::Error::from).and_then(|bytes| {
            let tmp = self.path.with_extension("tmp");
            std::fs::write(&tmp, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        });
        if let Err(e) = result {
            tracing::warn!("{} を保存できません: {e:#}", self.path.display());
        }
    }
}
