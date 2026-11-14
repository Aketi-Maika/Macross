//! 操作履歴（data/audit/YYYY-MM.csv）
//!
//! 管理者の操作（ID 発行・CSV 登録・削除・無効化・パスワードリセット・権限・名前の変更・イベントなど）を
//! 「いつ・誰が・誰に・何をしたか」の 1 行ずつ追記する。サーバの再起動やコンテナの入れ替えでも消えない。
//! 列: 日時, 操作者ID, 操作者名, 操作者内部ID, 操作, 対象ID, 対象名, 対象内部ID, 内容

use crate::radio::now_jst;
use crate::users::AuthUser;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const HEADER: [&str; 9] = ["日時", "操作者ID", "操作者名", "操作者内部ID", "操作", "対象ID", "対象名", "対象内部ID", "内容"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Entry {
    pub time: String,
    pub actor_id: String,
    pub actor_name: String,
    pub actor_uid: String,
    pub action: String,
    pub target_id: String,
    pub target_name: String,
    pub target_uid: String,
    pub detail: String,
}

/// 操作の対象（内部 ID・ログイン ID・名前）
pub struct Target<'a> {
    pub uid: &'a str,
    pub id: &'a str,
    pub name: &'a str,
}

pub struct Audit {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, lock: Mutex::new(()) })
    }

    /// 1 件追記する（書けなくても操作は止めない。警告だけ出す）
    pub fn log(&self, actor: &AuthUser, action: &str, target: Option<Target>, detail: &str) {
        let now = now_jst();
        let t = target.unwrap_or(Target { uid: "", id: "", name: "" });
        let row = [
            now.format("%Y-%m-%d %H:%M:%S").to_string(),
            actor.display_id.clone(),
            actor.name.clone(),
            actor.id.clone(),
            action.to_string(),
            t.id.to_string(),
            t.name.to_string(),
            t.uid.to_string(),
            detail.to_string(),
        ];
        let path = self.dir.join(format!("{}.csv", now.format("%Y-%m")));
        let _g = self.lock.lock().unwrap();
        let result = (|| -> anyhow::Result<()> {
            let new = !path.exists();
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(vec![]);
            if new {
                f.write_all("\u{feff}".as_bytes())?;
                w.write_record(HEADER)?;
            }
            w.write_record(&row)?;
            f.write_all(&w.into_inner()?)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("操作履歴を書けません: {e:#}");
        }
        tracing::info!("[操作履歴] {} {} ({}) → {} {} {}", action, actor.display_id, actor.name, t.id, t.name, detail);
    }

    /// 記録のある月（新しい順）
    pub fn months(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".csv").map(str::to_string))
            .filter(|m| m.len() == 7)
            .collect();
        v.sort();
        v.reverse();
        v
    }

    pub fn file_of(&self, month: &str) -> Option<PathBuf> {
        let ok = month.len() == 7 && month.chars().enumerate().all(|(i, c)| if i == 4 { c == '-' } else { c.is_ascii_digit() });
        let p = self.dir.join(format!("{month}.csv"));
        (ok && p.exists()).then_some(p)
    }

    /// 新しい順に最大 `limit` 件。`q` は空白区切りの語をすべて含む行だけ（大文字小文字・ハイフンを区別しない）
    pub fn search(&self, q: &str, limit: usize) -> Vec<Entry> {
        let norm = |s: &str| s.to_lowercase().replace('-', "");
        let words: Vec<String> = q.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
        let mut out = vec![];
        for m in self.months() {
            let Ok(text) = std::fs::read_to_string(self.dir.join(format!("{m}.csv"))) else { continue };
            let text = text.trim_start_matches('\u{feff}');
            let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
            let mut rows: Vec<Entry> = rdr
                .records()
                .flatten()
                .map(|r| {
                    let g = |i: usize| r.get(i).unwrap_or("").to_string();
                    Entry {
                        time: g(0),
                        actor_id: g(1),
                        actor_name: g(2),
                        actor_uid: g(3),
                        action: g(4),
                        target_id: g(5),
                        target_name: g(6),
                        target_uid: g(7),
                        detail: g(8),
                    }
                })
                .filter(|e| {
                    if words.is_empty() {
                        return true;
                    }
                    let hay = norm(&[&e.time, &e.actor_id, &e.actor_name, &e.actor_uid, &e.action, &e.target_id, &e.target_name, &e.target_uid, &e.detail]
                        .map(|s| s.as_str())
                        .join(" "));
                    words.iter().all(|w| hay.contains(w))
                })
                .collect();
            rows.reverse();
            for e in rows {
                out.push(e);
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }
}
