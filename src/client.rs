//! Thin Kraken public REST client with rate limiting and retry/backoff.

use crate::store::{Candle, DAY};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

pub const DEFAULT_BASE: &str = "https://api.kraken.com";
const MAX_ATTEMPTS: u32 = 6;

#[derive(Debug, Clone, Deserialize)]
pub struct PairInfo {
    pub altname: String,
    pub wsname: Option<String>,
    pub base: String,
    pub quote: String,
    #[serde(default)]
    pub status: String,
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    min_interval: Duration,
    last_call: Mutex<Option<Instant>>,
}

impl Client {
    pub fn new(base: impl Into<String>, min_interval: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("kraken-downloader/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            min_interval,
            last_call: Mutex::new(None),
        })
    }

    async fn throttle(&self) {
        let mut last = self.last_call.lock().await;
        if let Some(t) = *last {
            let wait = self.min_interval.saturating_sub(t.elapsed());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
        *last = Some(Instant::now());
    }

    /// GET a public endpoint and return the `result` object, retrying transient failures.
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.throttle().await;
            let outcome: Result<Value, (bool, anyhow::Error)> = async {
                let resp = self
                    .http
                    .get(&url)
                    .query(query)
                    .send()
                    .await
                    .map_err(|e| (true, anyhow!(e)))?;
                let status = resp.status();
                if status.as_u16() == 429 || status.is_server_error() {
                    return Err((true, anyhow!("HTTP {status}")));
                }
                let body: Value = resp.json().await.map_err(|e| (true, anyhow!(e)))?;
                let errors: Vec<String> = body["error"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if !errors.is_empty() {
                    let msg = errors.join("; ");
                    let retry = msg.contains("Too many requests")
                        || msg.contains("Temporary lockout")
                        || msg.contains("Service:Unavailable")
                        || msg.contains("Busy");
                    return Err((retry, anyhow!("Kraken error: {msg}")));
                }
                body.get("result").cloned().ok_or((false, anyhow!("response has no result")))
            }
            .await;

            match outcome {
                Ok(v) => return Ok(v),
                Err((retry, e)) if retry && attempt < MAX_ATTEMPTS => {
                    let delay = Duration::from_secs(2u64.pow(attempt));
                    tracing::warn!("{path}: {e:#}; retry {attempt}/{MAX_ATTEMPTS} in {delay:?}");
                    tokio::time::sleep(delay).await;
                }
                Err((_, e)) => return Err(e.context(format!("GET {path}"))),
            }
        }
    }

    pub async fn asset_pairs(&self) -> Result<Vec<PairInfo>> {
        let result = self.get("/0/public/AssetPairs", &[]).await?;
        let map: HashMap<String, Value> = serde_json::from_value(result)?;
        let mut pairs = Vec::with_capacity(map.len());
        for (key, v) in map {
            match serde_json::from_value::<PairInfo>(v) {
                Ok(p) => pairs.push(p),
                Err(e) => tracing::debug!("skipping pair {key}: {e}"),
            }
        }
        pairs.sort_by(|a, b| a.altname.cmp(&b.altname));
        Ok(pairs)
    }

    /// Daily candles (interval 1440). Kraken returns at most the latest 720 and always
    /// includes the still-forming current day as the final row, which is dropped here.
    pub async fn daily_ohlc(&self, altname: &str, since: Option<i64>) -> Result<Vec<Candle>> {
        let mut q = vec![("pair", altname.to_string()), ("interval", "1440".to_string())];
        if let Some(s) = since {
            q.push(("since", s.to_string()));
        }
        let result = self.get("/0/public/OHLC", &q).await?;
        let obj = result.as_object().context("OHLC result is not an object")?;
        let rows = obj
            .iter()
            .find(|(k, _)| k.as_str() != "last")
            .map(|(_, v)| v)
            .and_then(Value::as_array)
            .with_context(|| format!("OHLC result for {altname} has no candle array"))?;
        let now = chrono::Utc::now().timestamp();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let c = parse_row(row).with_context(|| format!("parsing OHLC row {row}"))?;
            if c.ts + DAY <= now {
                out.push(c);
            }
        }
        Ok(out)
    }
}

/// `[time, open, high, low, close, vwap, volume, count]` — vwap (index 5) and count (index 7)
/// are not kept.
fn parse_row(row: &Value) -> Result<Candle> {
    let a = row.as_array().filter(|a| a.len() >= 8);
    let a = match a {
        Some(a) => a,
        None => bail!("expected 8-element array"),
    };
    let s = |i: usize| -> Result<String> {
        a[i].as_str().map(String::from).with_context(|| format!("field {i} not a string"))
    };
    Ok(Candle {
        ts: a[0].as_i64().context("time")?,
        open: s(1)?,
        high: s(2)?,
        low: s(3)?,
        close: s(4)?,
        volume: s(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kraken_row() {
        let v: Value = serde_json::from_str(
            r#"[1727740800,"63325.0","64100.0","60203.4","60819.1","62147.0","2632.62350634",38700]"#,
        )
        .unwrap();
        let c = parse_row(&v).unwrap();
        assert_eq!(c.ts, 1727740800);
        assert_eq!(c.close, "60819.1");
        assert_eq!(c.volume, "2632.62350634");
    }
}
