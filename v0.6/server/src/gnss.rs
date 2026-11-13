//! 位置の記録（GNSS ログ）
//!
//! `<data_dir>/gnss/YYYY-MM-DD.csv`（日本時間の日付ごと・BOM 付き UTF-8）に追記する。
//! 地図の履歴はこの CSV から読むので、サーバを再起動しても消えない。
//!
//! - 位置: 1 人あたり `record_interval_secs` ごと、または `record_min_move_m` 以上動いたときに記録
//! - PTT・読み上げ: 押した（送った）時点の位置を毎回記録
//! - `retention_days` を過ぎた日のファイルは 1 日 1 回削除

use crate::config::LocationCfg;
use crate::radio::{now_jst, Location};
use chrono::{Duration as ChronoDuration, FixedOffset, NaiveDate, TimeZone};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const HEADER: [&str; 10] = ["日時", "UNIXミリ秒", "ID", "名前", "種別", "チャンネル", "緯度", "経度", "高度", "誤差"];

/// 1 回の取得で返す最大件数（超えたら位置の点を間引く。PTT・読み上げは残す）
pub const MAX_POINTS: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Kind {
    /// 位置（定期記録）
    #[serde(rename = "POS")]
    Pos,
    #[serde(rename = "PTT")]
    Ptt,
    #[serde(rename = "TTS")]
    Tts,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Pos => "位置",
            Kind::Ptt => "PTT",
            Kind::Tts => "TTS",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "位置" => Some(Kind::Pos),
            "PTT" => Some(Kind::Ptt),
            "TTS" => Some(Kind::Tts),
            _ => None,
        }
    }
}

/// 記録 1 件（地図 API でもこの形で返す）
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub channel: String,
    /// 日本時間 "YYYY-MM-DD HH:MM:SS"
    pub time: String,
    /// UNIX ミリ秒
    pub ts: i64,
    pub location: Location,
}

impl Point {
    pub fn new(id: &str, name: &str, kind: Kind, channel: &str, location: Location) -> Self {
        let now = now_jst();
        let ts = now.timestamp_millis();
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            channel: channel.into(),
            time: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            ts,
            location: Location { ts, ..location },
        }
    }
}

#[derive(Clone)]
pub struct GnssLog {
    dir: PathBuf,
    tx: Option<Sender<Point>>,
}

impl GnssLog {
    /// 記録用のスレッドを起動する（`cfg.enabled` が false なら何も記録しない）
    pub fn start(dir: PathBuf, cfg: &LocationCfg) -> anyhow::Result<Self> {
        if !cfg.enabled {
            return Ok(Self { dir, tx: None });
        }
        std::fs::create_dir_all(&dir)?;
        let (tx, rx) = channel::<Point>();
        let wdir = dir.clone();
        let interval_ms = i64::from(cfg.record_interval_secs.max(1)) * 1000;
        let min_move = cfg.record_min_move_m.max(0.0);
        let retention = cfg.retention_days;
        std::thread::Builder::new().name("gnss-log".into()).spawn(move || {
            // ID → 最後に記録した (時刻, 緯度, 経度)
            let mut last: HashMap<String, (i64, f64, f64)> = HashMap::new();
            let mut next_cleanup = Instant::now();
            loop {
                if Instant::now() >= next_cleanup {
                    cleanup(&wdir, retention);
                    next_cleanup = Instant::now() + Duration::from_secs(24 * 3600);
                }
                let first = match rx.recv_timeout(Duration::from_secs(600)) {
                    Ok(p) => p,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                let mut batch = vec![first];
                batch.extend(rx.try_iter());
                let keep: Vec<Point> = batch
                    .into_iter()
                    .filter(|p| {
                        let l = &p.location;
                        let record = match (p.kind, last.get(&p.id)) {
                            (Kind::Pos, Some(&(t, lat, lon))) => {
                                p.ts - t >= interval_ms || distance_m(lat, lon, l.lat, l.lon) >= min_move
                            }
                            _ => true,
                        };
                        if record {
                            last.insert(p.id.clone(), (p.ts, l.lat, l.lon));
                        }
                        record
                    })
                    .collect();
                if let Err(e) = append(&wdir, &keep) {
                    tracing::warn!("位置の記録（GNSS ログ）を書き込めません: {e:#}");
                }
            }
        })?;
        Ok(Self { dir, tx: Some(tx) })
    }

    pub fn record(&self, p: Point) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(p);
        }
    }

    /// 日付ごとの CSV ファイル（ダウンロード用）。日付の形式が不正・ファイルが無ければ None
    pub fn file_of(&self, date: &str) -> Option<PathBuf> {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        let p = self.dir.join(format!("{}.csv", d.format("%Y-%m-%d")));
        p.is_file().then_some(p)
    }

    /// CSV がある日付の一覧（新しい順）
    pub fn dates(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let d = name.strip_suffix(".csv")?;
                        NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().map(|_| d.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v
    }

    /// `since_ms` 以降の記録を CSV から読む（古い順。多すぎるときは位置の点を間引く）
    pub fn read_since(&self, since_ms: i64) -> Vec<Point> {
        let tz = jst();
        let today = now_jst().date_naive();
        let first = tz.timestamp_millis_opt(since_ms).single().map(|d| d.date_naive()).unwrap_or(today).max(today - ChronoDuration::days(400));
        let mut out = Vec::new();
        let mut day = first;
        while day <= today {
            let path = self.dir.join(format!("{}.csv", day.format("%Y-%m-%d")));
            if path.is_file() {
                if let Err(e) = read_file(&path, since_ms, &mut out) {
                    tracing::warn!("{} を読めません: {e:#}", path.display());
                }
            }
            day += ChronoDuration::days(1);
        }
        out.sort_by_key(|p| p.ts);
        thin(out, MAX_POINTS)
    }
}

fn jst() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).unwrap()
}

/// 2 点間の距離（m、近距離用の近似）
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0_f64;
    let x = (lon2 - lon1).to_radians() * ((lat1 + lat2) / 2.0).to_radians().cos();
    let y = (lat2 - lat1).to_radians();
    (x * x + y * y).sqrt() * r
}

fn append(dir: &Path, points: &[Point]) -> anyhow::Result<()> {
    let mut by_day: HashMap<String, Vec<&Point>> = HashMap::new();
    for p in points {
        by_day.entry(p.time[..10].to_string()).or_default().push(p);
    }
    for (day, ps) in by_day {
        let path = dir.join(format!("{day}.csv"));
        let new = !path.exists();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let mut w = csv::WriterBuilder::new().from_writer(vec![]);
        if new {
            f.write_all(b"\xEF\xBB\xBF")?;
            w.write_record(HEADER)?;
        }
        for p in ps {
            let l = &p.location;
            let opt = |v: Option<f64>, digits: usize| v.map(|x| format!("{x:.digits$}")).unwrap_or_default();
            w.write_record([
                p.time.as_str(),
                &p.ts.to_string(),
                &p.id,
                &p.name,
                p.kind.label(),
                &p.channel,
                &format!("{:.6}", l.lat),
                &format!("{:.6}", l.lon),
                &opt(l.alt, 1),
                &opt(l.accuracy, 1),
            ])?;
        }
        f.write_all(&w.into_inner()?)?;
    }
    Ok(())
}

fn read_file(path: &Path, since_ms: i64, out: &mut Vec<Point>) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let text = text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
    for rec in rdr.records() {
        let Ok(r) = rec else { continue };
        let get = |i: usize| r.get(i).unwrap_or("").trim();
        let Ok(ts) = get(1).parse::<i64>() else { continue };
        if ts < since_ms {
            continue;
        }
        let (Ok(lat), Ok(lon)) = (get(6).parse::<f64>(), get(7).parse::<f64>()) else { continue };
        let Some(kind) = Kind::parse(get(4)) else { continue };
        out.push(Point {
            time: get(0).into(),
            ts,
            id: get(2).into(),
            name: get(3).into(),
            kind,
            channel: get(5).into(),
            location: Location { lat, lon, alt: get(8).parse().ok(), accuracy: get(9).parse().ok(), ts },
        });
    }
    Ok(())
}

/// 多すぎるときは位置の点を等間隔に間引く（PTT・読み上げは全部残す）
fn thin(points: Vec<Point>, max: usize) -> Vec<Point> {
    if points.len() <= max {
        return points;
    }
    let events = points.iter().filter(|p| p.kind != Kind::Pos).count();
    let pos = points.len() - events;
    let room = max.saturating_sub(events).max(1);
    let step = pos.div_ceil(room).max(1);
    let mut i = 0usize;
    points
        .into_iter()
        .filter(|p| {
            if p.kind != Kind::Pos {
                return true;
            }
            i += 1;
            (i - 1) % step == 0
        })
        .collect()
}

fn cleanup(dir: &Path, retention_days: u32) {
    if retention_days == 0 {
        return;
    }
    let limit = now_jst().date_naive() - ChronoDuration::days(i64::from(retention_days));
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(d) = name.strip_suffix(".csv").and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) else { continue };
        if d < limit {
            match std::fs::remove_file(e.path()) {
                Ok(()) => tracing::info!("保存期間を過ぎた位置の記録を削除: {name}"),
                Err(err) => tracing::warn!("{name} を削除できません: {err}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(kind: Kind, ts: i64) -> Point {
        Point {
            id: "0001".into(),
            name: "a".into(),
            kind,
            channel: String::new(),
            time: "2026-11-14 00:00:00".into(),
            ts,
            location: Location { lat: 35.0, lon: 139.0, alt: None, accuracy: None, ts },
        }
    }

    #[test]
    fn thin_keeps_events() {
        let mut v: Vec<Point> = (0..100).map(|i| pt(Kind::Pos, i)).collect();
        v.push(pt(Kind::Ptt, 1000));
        let t = thin(v, 11);
        assert!(t.len() <= 11);
        assert!(t.iter().any(|p| p.kind == Kind::Ptt));
    }

    #[test]
    fn distance() {
        let d = distance_m(35.0, 139.0, 35.001, 139.0);
        assert!((d - 111.2).abs() < 1.0, "{d}");
    }

    #[test]
    fn write_and_read() {
        let dir = std::env::temp_dir().join(format!("mx-gnss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = Point::new("0001", "山田, 太郎", Kind::Ptt, "ch1", Location { lat: 35.68, lon: 139.76, alt: Some(3.0), accuracy: Some(8.0), ts: 0 });
        append(&dir, &[p.clone()]).unwrap();
        append(&dir, &[p.clone()]).unwrap();
        let log = GnssLog { dir: dir.clone(), tx: None };
        let got = log.read_since(0);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "山田, 太郎");
        assert_eq!(got[0].kind, Kind::Ptt);
        assert_eq!(got[0].location.accuracy, Some(8.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
