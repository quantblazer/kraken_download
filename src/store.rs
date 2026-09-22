//! CSV storage: one file per pair, ascending by day, exact decimal strings.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const DAY: i64 = 86_400;
/// `symbol` is the Kraken altname (e.g. `XBTUSD`), written into every row so a CSV is
/// self-describing even if the file gets renamed, copied elsewhere, or merged with others.
pub const HEADER: [&str; 7] = [
    "timestamp", "symbol", "open", "high", "low", "close", "volume",
];

/// One completed UTC-day candle. Prices/volume are kept as the exact strings Kraken gave us.
#[derive(Clone, Debug, PartialEq)]
pub struct Candle {
    /// Unix seconds of the UTC day open.
    pub ts: i64,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
}

pub type Series = BTreeMap<i64, Candle>;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub added: usize,
    pub replaced: usize,
    /// Overlapping rows whose OHLC/volume differ numerically between old and incoming data.
    pub mismatched: usize,
}

pub fn fmt_ts(ts: i64, epoch: bool) -> String {
    if epoch {
        return ts.to_string();
    }
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| ts.to_string())
}

pub fn parse_ts(s: &str) -> Result<i64> {
    let s = s.trim();
    if let Ok(v) = s.parse::<i64>() {
        return Ok(v);
    }
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("bad timestamp {s:?}"))?
        .timestamp())
}

fn close_enough(a: &str, b: &str, rel: f64) -> bool {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => (x - y).abs() <= rel * x.abs().max(y.abs()).max(f64::MIN_POSITIVE),
        _ => a == b,
    }
}

/// True when two candles for the same day disagree beyond rounding noise.
pub fn differs(a: &Candle, b: &Candle) -> bool {
    !(close_enough(&a.open, &b.open, 1e-9)
        && close_enough(&a.high, &b.high, 1e-9)
        && close_enough(&a.low, &b.low, 1e-9)
        && close_enough(&a.close, &b.close, 1e-9)
        && close_enough(&a.volume, &b.volume, 1e-6))
}

/// Merge `incoming` into `series`. With `prefer_incoming`, overlapping days are overwritten
/// (REST data wins over bulk data); otherwise existing rows are kept and only gaps are filled.
pub fn merge(series: &mut Series, incoming: Vec<Candle>, prefer_incoming: bool) -> MergeStats {
    let mut stats = MergeStats::default();
    for c in incoming {
        match series.get(&c.ts) {
            None => {
                stats.added += 1;
                series.insert(c.ts, c);
            }
            Some(old) => {
                if differs(old, &c) {
                    stats.mismatched += 1;
                }
                if prefer_incoming {
                    if *old != c {
                        stats.replaced += 1;
                    }
                    series.insert(c.ts, c);
                }
            }
        }
    }
    stats
}

pub struct Store {
    dir: PathBuf,
    epoch: bool,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>, epoch: bool) -> Self {
        Self { dir: dir.into(), epoch }
    }

    pub fn path(&self, altname: &str) -> PathBuf {
        self.dir.join(format!("{altname}_1d.csv"))
    }

    pub fn exists(&self, altname: &str) -> bool {
        self.path(altname).exists()
    }

    /// Loads the file expected to hold `altname`. The file's own `symbol` column is the
    /// source of truth: a mismatch (renamed/misplaced file) is an error, not a silent trust
    /// of the filename.
    pub fn load(&self, altname: &str) -> Result<Series> {
        let path = self.path(altname);
        let mut series = Series::new();
        if !path.exists() {
            return Ok(series);
        }
        let mut rdr = csv::ReaderBuilder::new()
            .from_path(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        for (i, rec) in rdr.records().enumerate() {
            let rec = rec.with_context(|| format!("{} row {}", path.display(), i + 2))?;
            if rec.len() != HEADER.len() {
                bail!("{} row {}: expected {} fields, got {}", path.display(), i + 2, HEADER.len(), rec.len());
            }
            if &rec[1] != altname {
                bail!(
                    "{} row {}: symbol column is {:?}, expected {altname:?} (file may be misnamed or mixed up)",
                    path.display(),
                    i + 2,
                    &rec[1]
                );
            }
            let c = Candle {
                ts: parse_ts(&rec[0])?,
                open: rec[2].to_string(),
                high: rec[3].to_string(),
                low: rec[4].to_string(),
                close: rec[5].to_string(),
                volume: rec[6].to_string(),
            };
            series.insert(c.ts, c);
        }
        Ok(series)
    }

    /// Write atomically (temp file + rename) so an interrupted run never leaves a half-written CSV.
    pub fn save(&self, altname: &str, series: &Series) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(altname);
        let tmp = path.with_extension("csv.tmp");
        {
            let mut w = csv::Writer::from_path(&tmp)?;
            w.write_record(HEADER)?;
            for c in series.values() {
                w.write_record([
                    &fmt_ts(c.ts, self.epoch),
                    altname,
                    &c.open,
                    &c.high,
                    &c.low,
                    &c.close,
                    &c.volume,
                ])?;
            }
            w.flush()?;
        }
        replace_file(&tmp, &path)
    }
}

fn replace_file(tmp: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_file(dest).with_context(|| format!("removing {}", dest.display()))?;
    }
    fs::rename(tmp, dest).with_context(|| format!("renaming to {}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(ts: i64, close: &str) -> Candle {
        Candle {
            ts,
            open: "1".into(),
            high: "3".into(),
            low: "1".into(),
            close: close.into(),
            volume: "10".into(),
        }
    }

    #[test]
    fn merge_fills_gaps_and_keeps_existing() {
        let mut s = Series::new();
        s.insert(0, c(0, "2"));
        let st = merge(&mut s, vec![c(0, "2.5"), c(DAY, "2")], false);
        assert_eq!(st, MergeStats { added: 1, replaced: 0, mismatched: 1 });
        assert_eq!(s[&0].close, "2");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn merge_prefers_incoming() {
        let mut s = Series::new();
        s.insert(0, c(0, "2"));
        let st = merge(&mut s, vec![c(0, "2.5")], true);
        assert_eq!(st.mismatched, 1);
        assert_eq!(st.replaced, 1);
        assert_eq!(s[&0].close, "2.5");
    }

    #[test]
    fn merge_dedupes_repeated_input() {
        let mut s = Series::new();
        merge(&mut s, vec![c(DAY, "2"), c(DAY, "2"), c(0, "2")], true);
        assert_eq!(s.keys().copied().collect::<Vec<_>>(), vec![0, DAY]);
    }

    #[test]
    fn roundtrip_iso_and_epoch() {
        let dir = tempfile::tempdir().unwrap();
        for epoch in [false, true] {
            let store = Store::new(dir.path(), epoch);
            let mut s = Series::new();
            s.insert(1_704_067_200, c(1_704_067_200, "42.5"));
            store.save("XBTUSD", &s).unwrap();
            let text = fs::read_to_string(store.path("XBTUSD")).unwrap();
            assert!(text.starts_with("timestamp,symbol,open,high,low,close,volume\n"));
            if epoch {
                assert!(text.contains("\n1704067200,XBTUSD,"));
            } else {
                assert!(text.contains("\n2024-01-01T00:00:00Z,XBTUSD,"));
            }
            assert_eq!(store.load("XBTUSD").unwrap(), s);
        }
    }

    #[test]
    fn load_rejects_mismatched_symbol_column() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), false);
        let mut s = Series::new();
        s.insert(0, c(0, "2"));
        store.save("XBTUSD", &s).unwrap();
        // Simulate a renamed/misplaced file: load it under a different expected altname.
        fs::rename(store.path("XBTUSD"), store.path("ETHUSD")).unwrap();
        let err = store.load("ETHUSD").unwrap_err();
        assert!(err.to_string().contains("expected \"ETHUSD\""), "{err}");
    }

    #[test]
    fn numeric_tolerance() {
        assert!(!differs(&c(0, "2"), &c(0, "2.0")));
        assert!(differs(&c(0, "2"), &c(0, "2.1")));
    }
}
