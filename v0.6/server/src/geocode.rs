//! 住所の取得（国土地理院の逆ジオコーダ）
//!
//! 地図で点を押したときに「東京都千代田区丸の内一丁目」のような住所を出す。
//! 外部へ送るのは座標だけ。結果は座標を小数 4 桁（約 10m）に丸めてメモリにキャッシュする。

use crate::config::LocationCfg;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 市区町村コード → 「都道府県＋市区町村」（tools/make_muni.py で作る）
static MUNI_JSON: &str = include_str!("../assets/muni.json");
const CACHE_MAX: usize = 20_000;

pub struct Geocoder {
    enabled: bool,
    url: String,
    client: reqwest::Client,
    cache: Mutex<HashMap<(i64, i64), Option<String>>>,
}

#[derive(Deserialize)]
struct Resp {
    results: Option<Results>,
}

#[derive(Deserialize)]
struct Results {
    #[serde(rename = "muniCd", default)]
    muni_cd: String,
    #[serde(rename = "lv01Nm", default)]
    lv01_nm: String,
}

fn muni_table() -> &'static HashMap<String, String> {
    static T: OnceLock<HashMap<String, String>> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(MUNI_JSON).unwrap_or_default())
}

/// 市区町村コード（先頭の 0 の有無を問わない）と町名から住所を作る
pub fn compose(muni_cd: &str, town: &str) -> Option<String> {
    let code = muni_cd.trim().trim_start_matches('0');
    let muni = muni_table().get(code).cloned().unwrap_or_default();
    let town = if town.trim() == "－" || town.trim() == "-" { "" } else { town.trim() };
    let s = format!("{muni}{town}");
    (!s.is_empty()).then_some(s)
}

impl Geocoder {
    pub fn new(cfg: &LocationCfg) -> Self {
        Self {
            enabled: cfg.enabled && cfg.reverse_geocode && !cfg.geocode_url.trim().is_empty(),
            url: cfg.geocode_url.trim().to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .user_agent("Macross")
                .build()
                .unwrap_or_default(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 住所（取れなければ None。海上・国外・通信エラーなど）
    pub async fn address(&self, lat: f64, lon: f64) -> Option<String> {
        if !self.enabled || !lat.is_finite() || !lon.is_finite() {
            return None;
        }
        let key = ((lat * 1e4).round() as i64, (lon * 1e4).round() as i64);
        if let Some(hit) = self.cache.lock().unwrap().get(&key) {
            return hit.clone();
        }
        let (qlat, qlon) = (key.0 as f64 / 1e4, key.1 as f64 / 1e4);
        let res = self
            .client
            .get(&self.url)
            .query(&[("lat", format!("{qlat:.4}")), ("lon", format!("{qlon:.4}"))])
            .send()
            .await
            .and_then(|r| r.error_for_status());
        let addr = match res {
            Ok(r) => match r.json::<Resp>().await {
                Ok(Resp { results: Some(x) }) => compose(&x.muni_cd, &x.lv01_nm),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!("住所の取得結果を読めません: {e}");
                    return None;
                }
            },
            Err(e) => {
                // 一時的な失敗はキャッシュしない
                tracing::warn!("住所を取得できません: {e}");
                return None;
            }
        };
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, addr.clone());
        addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_address() {
        assert_eq!(compose("13101", "丸の内一丁目").as_deref(), Some("東京都千代田区丸の内一丁目"));
        assert_eq!(compose("01101", "").as_deref(), Some("北海道札幌市中央区"));
        assert_eq!(compose("", ""), None);
    }
}
