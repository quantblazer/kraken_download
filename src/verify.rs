//! Integrity checks on stored CSVs, plus a live comparison with the OHLC endpoint.

use crate::client::Client;
use crate::store::{differs, fmt_ts, Series, Store, DAY};
use anyhow::Result;

#[derive(Debug, Default)]
pub struct LocalReport {
    pub rows: usize,
    pub bad_ohlc: Vec<String>,
    pub not_midnight: usize,
    /// Days with no candle between first and last date (legitimate if there were no trades).
    pub gap_days: usize,
    pub first_gaps: Vec<String>,
}

pub fn check_local(series: &Series) -> LocalReport {
    let mut r = LocalReport { rows: series.len(), ..Default::default() };
    let mut prev: Option<i64> = None;
    for c in series.values() {
        if c.ts % DAY != 0 {
            r.not_midnight += 1;
        }
        let f = |s: &str| s.parse::<f64>().ok();
        match (f(&c.open), f(&c.high), f(&c.low), f(&c.close), f(&c.volume)) {
            (Some(o), Some(h), Some(l), Some(cl), Some(v)) => {
                if h < o.max(cl) || l > o.min(cl) || h < l || v < 0.0 {
                    r.bad_ohlc.push(fmt_ts(c.ts, false));
                }
            }
            _ => r.bad_ohlc.push(fmt_ts(c.ts, false)),
        }
        if let Some(p) = prev {
            let missing = ((c.ts - p) / DAY - 1).max(0) as usize;
            if missing > 0 {
                r.gap_days += missing;
                if r.first_gaps.len() < 5 {
                    r.first_gaps.push(format!("{} (+{missing}d)", fmt_ts(p + DAY, false)));
                }
            }
        }
        prev = Some(c.ts);
    }
    r
}

pub struct LiveReport {
    pub compared: usize,
    pub mismatched: Vec<String>,
    pub missing_locally: usize,
}

pub async fn compare_live(client: &Client, series: &Series, pair: &str) -> Result<LiveReport> {
    let live = client.daily_ohlc(pair, None).await?;
    let mut r = LiveReport { compared: 0, mismatched: Vec::new(), missing_locally: 0 };
    for c in live {
        match series.get(&c.ts) {
            None => r.missing_locally += 1,
            Some(mine) => {
                r.compared += 1;
                if differs(mine, &c) {
                    r.mismatched.push(format!(
                        "{}: stored o={} h={} l={} c={} v={} | live o={} h={} l={} c={} v={}",
                        fmt_ts(c.ts, false),
                        mine.open, mine.high, mine.low, mine.close, mine.volume,
                        c.open, c.high, c.low, c.close, c.volume
                    ));
                }
            }
        }
    }
    Ok(r)
}

/// Returns true when every checked pair is clean.
pub async fn verify_pairs(client: &Client, store: &Store, pairs: &[String], live: bool) -> bool {
    let mut all_ok = true;
    for pair in pairs {
        let series = match store.load(pair) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => {
                println!("{pair}: no data file");
                all_ok = false;
                continue;
            }
            Err(e) => {
                println!("{pair}: unreadable: {e:#}");
                all_ok = false;
                continue;
            }
        };
        let l = check_local(&series);
        let first = series.keys().next().map(|t| fmt_ts(*t, false)).unwrap_or_default();
        let last = series.keys().next_back().map(|t| fmt_ts(*t, false)).unwrap_or_default();
        let mut ok = l.bad_ohlc.is_empty() && l.not_midnight == 0;
        println!(
            "{pair}: {} rows {first} .. {last}; gaps {} days{}; bad OHLC {}; non-midnight {}",
            l.rows,
            l.gap_days,
            if l.first_gaps.is_empty() { String::new() } else { format!(" [{}]", l.first_gaps.join(", ")) },
            l.bad_ohlc.len(),
            l.not_midnight
        );
        if live {
            match compare_live(client, &series, pair).await {
                Ok(r) => {
                    println!(
                        "    live: {} days compared, {} mismatched, {} live days missing locally",
                        r.compared, r.mismatched.len(), r.missing_locally
                    );
                    for m in r.mismatched.iter().take(5) {
                        println!("      {m}");
                    }
                    if !r.mismatched.is_empty() {
                        ok = false;
                    }
                }
                Err(e) => {
                    println!("    live: failed: {e:#}");
                    ok = false;
                }
            }
        }
        all_ok &= ok;
    }
    all_ok
}
