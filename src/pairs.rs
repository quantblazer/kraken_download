//! Discover USD-quoted crypto pairs from Kraken's AssetPairs.

use crate::client::{Client, PairInfo};
use anyhow::Result;
use clap::Args;
use std::collections::HashSet;

const FIAT_BASES: &[&str] = &[
    "ZEUR", "ZGBP", "ZAUD", "ZCAD", "ZJPY", "CHF", "EUR", "GBP", "AUD", "CAD", "JPY",
];
/// Fiat-pegged tokens (USD and other currencies). Kraken keeps listing new ones, so check
/// `list-pairs` for near-constant prices after a new listing appears.
const STABLE_BASES: &[&str] = &[
    // USD-pegged
    "USDT", "USDC", "DAI", "PYUSD", "RLUSD", "USD1", "USDG", "USDQ", "USDR", "TUSD", "FDUSD",
    "USDD", "UST", "AUSD", "CASH", "FIDD", "FRNT", "USAT", "USDE", "USDGO", "USDPT", "USDS",
    "USDSM", "USTABLES",
    // other fiat-pegged
    "EURQ", "EURR", "EURT", "EURC", "EUROP", "TGBP", "QCAD", "AUDX", "BRL1", "MXNB", "COPM",
];
/// Commodity-backed tokens (gold, uranium).
const COMMODITY_BASES: &[&str] = &["PAXG", "XAUT", "XU3O8"];
/// Wrapped and liquid-staking tokens: near-duplicates of XBT/ETH/SOL rather than their own
/// market (WBTC tracks BTC to ~0.7%; staking tokens track the underlying plus accrued yield).
const WRAPPED_BASES: &[&str] =
    &["WBTC", "TBTC", "METH", "CMETH", "LSETH", "MSOL", "JITOSOL", "LSSOL"];
/// Quote currencies whose names end in "USD" but are not USD (e.g. `XBTPYUSD` is XBT/PYUSD).
/// Only needed for delisted pairs, where the base/quote split comes from the name alone.
const USD_LIKE_QUOTES: &[&str] = &["PYUSD", "RLUSD", "FDUSD"];

#[derive(Args, Debug, Clone, Default)]
pub struct PairArgs {
    /// Explicit pair altnames (e.g. XBTUSD ETHUSD); skips discovery and all filters.
    #[arg(long, num_args = 1..)]
    pub pairs: Vec<String>,
    /// Include stablecoin/USD pairs (USDT, USDC, ...).
    #[arg(long)]
    pub include_stablecoins: bool,
    /// Include fiat/USD pairs (EUR, GBP, AUD, ...).
    #[arg(long)]
    pub include_fiat: bool,
    /// Include commodity-backed token pairs (PAXG, XAUT, XU3O8).
    #[arg(long)]
    pub include_commodities: bool,
    /// Include wrapped/liquid-staking token pairs (WBTC, TBTC, MSOL, ...).
    #[arg(long)]
    pub include_wrapped: bool,
    /// Only pairs whose status is `online` (excludes cancel_only / post_only).
    #[arg(long)]
    pub online_only: bool,
}

pub fn is_usd_crypto(p: &PairInfo, args: &PairArgs) -> bool {
    if !(p.quote == "ZUSD" || p.quote == "USD") || p.altname.ends_with(".d") {
        return false;
    }
    if excluded_base(&p.base, args) {
        return false;
    }
    if args.online_only && p.status != "online" {
        return false;
    }
    true
}

/// True when `base` belongs to a category the flags leave out (fiat, stablecoin, commodity).
fn excluded_base(base: &str, args: &PairArgs) -> bool {
    (!args.include_fiat && FIAT_BASES.contains(&base))
        || (!args.include_stablecoins && STABLE_BASES.contains(&base))
        || (!args.include_commodities && COMMODITY_BASES.contains(&base))
        || (!args.include_wrapped && WRAPPED_BASES.contains(&base))
}

/// Whether a pair name found only in the bulk data (not in today's AssetPairs, i.e. delisted)
/// should be imported. The base is taken from the name itself (`EOSUSD` -> `EOS`), and the
/// same category filters as for listed pairs apply.
pub fn is_delisted_usd_crypto(altname: &str, listed: &HashSet<String>, args: &PairArgs) -> bool {
    if listed.contains(altname) || USD_LIKE_QUOTES.iter().any(|q| altname.ends_with(q)) {
        return false;
    }
    match altname.strip_suffix("USD") {
        Some(base) if !base.is_empty() => !excluded_base(base, args),
        _ => false,
    }
}

pub async fn discover(client: &Client, args: &PairArgs) -> Result<Vec<PairInfo>> {
    let all = client.asset_pairs().await?;
    Ok(all.into_iter().filter(|p| is_usd_crypto(p, args)).collect())
}

/// Altnames to process: the explicit list if given, otherwise discovery.
pub async fn resolve(client: &Client, args: &PairArgs) -> Result<Vec<String>> {
    if !args.pairs.is_empty() {
        return Ok(args.pairs.iter().map(|p| p.to_uppercase()).collect());
    }
    Ok(discover(client, args).await?.into_iter().map(|p| p.altname).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(alt: &str, base: &str, quote: &str, status: &str) -> PairInfo {
        PairInfo {
            altname: alt.into(),
            wsname: None,
            base: base.into(),
            quote: quote.into(),
            status: status.into(),
        }
    }

    #[test]
    fn filters_usd_crypto_only() {
        let a = PairArgs::default();
        assert!(is_usd_crypto(&p("XBTUSD", "XXBT", "ZUSD", "online"), &a));
        assert!(is_usd_crypto(&p("SOLUSD", "SOL", "ZUSD", "cancel_only"), &a));
        assert!(!is_usd_crypto(&p("XBTEUR", "XXBT", "ZEUR", "online"), &a));
        assert!(!is_usd_crypto(&p("XBTUSDT", "XXBT", "USDT", "online"), &a));
        assert!(!is_usd_crypto(&p("USDTZUSD", "USDT", "ZUSD", "online"), &a));
        assert!(!is_usd_crypto(&p("EURUSD", "ZEUR", "ZUSD", "online"), &a));
        assert!(!is_usd_crypto(&p("XBTUSD.d", "XXBT", "ZUSD", "online"), &a));
    }

    #[test]
    fn flags_widen_and_narrow() {
        let stable = p("USDCUSD", "USDC", "ZUSD", "online");
        let fiat = p("EURUSD", "ZEUR", "ZUSD", "online");
        let gold = p("PAXGUSD", "PAXG", "ZUSD", "online");
        let wrapped = p("WBTCUSD", "WBTC", "ZUSD", "online");
        assert!(!is_usd_crypto(&wrapped, &PairArgs::default()));
        let w = PairArgs { include_wrapped: true, ..Default::default() };
        assert!(is_usd_crypto(&wrapped, &w));
        let post = p("FOOUSD", "FOO", "ZUSD", "post_only");
        let d = PairArgs::default();
        assert!(!is_usd_crypto(&gold, &d) && !is_usd_crypto(&p("EURCUSD", "EURC", "ZUSD", "online"), &d));
        let a = PairArgs {
            include_stablecoins: true,
            include_fiat: true,
            include_commodities: true,
            ..Default::default()
        };
        assert!(is_usd_crypto(&stable, &a) && is_usd_crypto(&fiat, &a) && is_usd_crypto(&gold, &a));
        let a = PairArgs { online_only: true, ..Default::default() };
        assert!(!is_usd_crypto(&post, &a));
    }

    #[test]
    fn delisted_candidates() {
        let listed: HashSet<String> = ["XBTUSD", "USDCUSD"].iter().map(|s| s.to_string()).collect();
        let a = PairArgs::default();
        assert!(is_delisted_usd_crypto("EOSUSD", &listed, &a));
        assert!(is_delisted_usd_crypto("LUNA2USD", &listed, &a));
        assert!(!is_delisted_usd_crypto("XBTUSD", &listed, &a), "still listed");
        assert!(!is_delisted_usd_crypto("USDCUSD", &listed, &a), "listed but filtered");
        assert!(!is_delisted_usd_crypto("XBTPYUSD", &listed, &a), "PYUSD quote");
        assert!(!is_delisted_usd_crypto("XRPRLUSD", &listed, &a), "RLUSD quote");
        assert!(!is_delisted_usd_crypto("XBTEUR", &listed, &a));
        assert!(!is_delisted_usd_crypto("USD", &listed, &a));
        for stable_or_fiat in ["USDTUSD", "DAIUSD", "EURTUSD", "EURUSD", "GBPUSD", "TUSDUSD"] {
            assert!(!is_delisted_usd_crypto(stable_or_fiat, &listed, &a), "{stable_or_fiat}");
        }
        let wide = PairArgs { include_fiat: true, ..Default::default() };
        assert!(is_delisted_usd_crypto("EURUSD", &listed, &wide));
    }
}
