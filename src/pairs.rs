//! Discover USD-quoted crypto pairs from Kraken's AssetPairs.

use crate::client::{Client, PairInfo};
use anyhow::Result;
use clap::Args;

const FIAT_BASES: &[&str] = &[
    "ZEUR", "ZGBP", "ZAUD", "ZCAD", "ZJPY", "CHF", "EUR", "GBP", "AUD", "CAD", "JPY",
];
const STABLE_BASES: &[&str] = &[
    "USDT", "USDC", "DAI", "PYUSD", "RLUSD", "USD1", "USDG", "USDQ", "USDR", "EURQ", "EURR",
    "TUSD", "FDUSD", "USDD", "UST",
];

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
    /// Only pairs whose status is `online` (excludes cancel_only / post_only).
    #[arg(long)]
    pub online_only: bool,
}

pub fn is_usd_crypto(p: &PairInfo, args: &PairArgs) -> bool {
    if !(p.quote == "ZUSD" || p.quote == "USD") || p.altname.ends_with(".d") {
        return false;
    }
    if !args.include_fiat && FIAT_BASES.contains(&p.base.as_str()) {
        return false;
    }
    if !args.include_stablecoins && STABLE_BASES.contains(&p.base.as_str()) {
        return false;
    }
    if args.online_only && p.status != "online" {
        return false;
    }
    true
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
        let post = p("FOOUSD", "FOO", "ZUSD", "post_only");
        let a = PairArgs { include_stablecoins: true, include_fiat: true, ..Default::default() };
        assert!(is_usd_crypto(&stable, &a) && is_usd_crypto(&fiat, &a));
        let a = PairArgs { online_only: true, ..Default::default() };
        assert!(!is_usd_crypto(&post, &a));
    }
}
