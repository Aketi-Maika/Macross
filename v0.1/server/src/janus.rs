//! Janus Gateway HTTP API クライアント（AudioBridge プラグイン用）

use anyhow::{anyhow, bail};
use serde_json::{json, Value};
use std::time::Duration;

pub const PLUGIN: &str = "janus.plugin.audiobridge";

#[derive(Clone)]
pub struct Janus {
    http: reqwest::Client,
    base: String,
}

pub fn transaction() -> String {
    crate::users::random_token(12)
}

impl Janus {
    pub fn new(base: &str) -> Self {
        Self { http: reqwest::Client::new(), base: base.trim_end_matches('/').to_string() }
    }

    async fn post(&self, url: &str, mut body: Value) -> anyhow::Result<Value> {
        if body.get("transaction").is_none() {
            body["transaction"] = Value::String(transaction());
        }
        let v: Value = self
            .http
            .post(url)
            .timeout(Duration::from_secs(10))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if v["janus"] == "error" {
            bail!("Janus エラー: {}", v["error"]["reason"].as_str().unwrap_or("不明"));
        }
        Ok(v)
    }

    pub async fn create_session(&self) -> anyhow::Result<u64> {
        let v = self.post(&self.base, json!({ "janus": "create" })).await?;
        v["data"]["id"].as_u64().ok_or_else(|| anyhow!("Janus セッション ID を取得できません"))
    }

    pub async fn attach(&self, session: u64) -> anyhow::Result<u64> {
        let url = format!("{}/{}", self.base, session);
        let v = self.post(&url, json!({ "janus": "attach", "plugin": PLUGIN })).await?;
        v["data"]["id"].as_u64().ok_or_else(|| anyhow!("Janus ハンドル ID を取得できません"))
    }

    /// プラグインへのメッセージ。非同期リクエスト（join / configure）は "ack" が返り、
    /// 結果は poll() のイベントとして同じ transaction で届く
    pub async fn message(
        &self,
        session: u64,
        handle: u64,
        body: Value,
        jsep: Option<Value>,
        transaction: &str,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/{}/{}", self.base, session, handle);
        let mut req = json!({ "janus": "message", "body": body, "transaction": transaction });
        if let Some(j) = jsep {
            req["jsep"] = j;
        }
        self.post(&url, req).await
    }

    pub async fn keepalive(&self, session: u64) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.base, session);
        self.post(&url, json!({ "janus": "keepalive" })).await.map(|_| ())
    }

    pub async fn destroy(&self, session: u64) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.base, session);
        self.post(&url, json!({ "janus": "destroy" })).await.map(|_| ())
    }

    /// ロングポーリング（最大 30 秒待つ）
    pub async fn poll(&self, session: u64) -> anyhow::Result<Vec<Value>> {
        let url = format!("{}/{}?maxev=5", self.base, session);
        let v: Value = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(45))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match v {
            Value::Array(a) => Ok(a),
            other => {
                if other["janus"] == "error" {
                    bail!("Janus エラー: {}", other["error"]["reason"].as_str().unwrap_or("不明"));
                }
                Ok(vec![other])
            }
        }
    }
}
