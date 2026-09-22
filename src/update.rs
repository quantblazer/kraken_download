//! Incremental update from the OHLC REST endpoint (latest 720 daily candles).

use crate::client::Client;
use crate::store::{merge, Store, DAY};
use anyhow::Result;

pub struct UpdateReport {
    pub failed: Vec<(String, String)>,
}

pub async fn update_pairs(client: &Client, store: &Store, pairs: &[String]) -> UpdateReport {
    let mut failed = Vec::new();
    for (i, pair) in pairs.iter().enumerate() {
        match update_one(client, store, pair).await {
            Ok(msg) => tracing::info!("[{}/{}] {pair}: {msg}", i + 1, pairs.len()),
            Err(e) => {
                tracing::error!("[{}/{}] {pair}: {e:#}", i + 1, pairs.len());
                failed.push((pair.clone(), format!("{e:#}")));
            }
        }
    }
    UpdateReport { failed }
}

async fn update_one(client: &Client, store: &Store, pair: &str) -> Result<String> {
    let mut series = store.load(pair)?;
    // Re-fetch the last two stored days so a previously partial/late-corrected day is refreshed.
    let since = series.keys().next_back().map(|last| last - 2 * DAY);
    let candles = client.daily_ohlc(pair, since).await?;
    if candles.is_empty() {
        return Ok("no completed candles returned".into());
    }

    let mut gap_note = String::new();
    if let (Some(last), Some(first_new)) = (series.keys().next_back(), candles.first().map(|c| c.ts)) {
        if first_new > last + DAY && since.is_some() {
            gap_note = format!(
                " WARNING: gap between stored data (ends {}) and REST data (starts {}); import a newer bulk zip",
                crate::store::fmt_ts(*last, false),
                crate::store::fmt_ts(first_new, false)
            );
        }
    }

    let stats = merge(&mut series, candles, true);
    store.save(pair, &series)?;
    Ok(format!(
        "+{} new, {} refreshed, {} differed from existing; {} days total{gap_note}",
        stats.added,
        stats.replaced,
        stats.mismatched,
        series.len()
    ))
}
