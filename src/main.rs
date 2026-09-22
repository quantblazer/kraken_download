use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use kraken_downloader::client::{Client, DEFAULT_BASE};
use kraken_downloader::pairs::{self, PairArgs};
use kraken_downloader::store::Store;
use kraken_downloader::import::Selection;
use kraken_downloader::{import, update, verify};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "kraken-downloader", version, about = "Daily OHLCV CSVs for Kraken USD pairs")]
struct Cli {
    /// Directory for the CSV files.
    #[arg(long, global = true, default_value = "data")]
    data_dir: PathBuf,
    /// Minimum milliseconds between REST calls (Kraken public limit is about 1/s).
    #[arg(long, global = true, default_value_t = 1100)]
    rate_ms: u64,
    /// Write timestamps as Unix seconds instead of ISO 8601.
    #[arg(long, global = true)]
    epoch: bool,
    /// API base URL (for testing).
    #[arg(long, global = true, default_value = DEFAULT_BASE, hide = true)]
    base_url: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the USD-quoted pairs that would be processed.
    ListPairs(PairArgs),
    /// Import daily history from Kraken's bulk OHLCVT data (a zip file or an extracted folder).
    Import {
        /// Path to the downloaded Kraken OHLCVT zip, or a folder it was extracted into.
        #[arg(long)]
        path: PathBuf,
        /// Only import currently listed pairs. By default, USD pairs that are in the bulk data
        /// but no longer listed (delisted) are imported too, to avoid survivorship bias.
        #[arg(long)]
        skip_delisted: bool,
        #[command(flatten)]
        pairs: PairArgs,
    },
    /// Fetch the latest 720 daily candles (and everything new since the last stored day).
    Update(PairArgs),
    /// Check stored CSVs and compare the last 720 days against the live OHLC endpoint.
    Verify {
        /// Skip the live comparison (local checks only).
        #[arg(long)]
        no_live: bool,
        #[command(flatten)]
        pairs: PairArgs,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let client = Client::new(&cli.base_url, Duration::from_millis(cli.rate_ms))?;
    let store = Store::new(&cli.data_dir, cli.epoch);

    match cli.cmd {
        Cmd::ListPairs(args) => {
            let found = pairs::discover(&client, &args).await?;
            for p in &found {
                println!(
                    "{:<14} {:<14} base={:<8} status={}",
                    p.altname,
                    p.wsname.as_deref().unwrap_or("-"),
                    p.base,
                    p.status
                );
            }
            println!("{} pairs", found.len());
        }
        Cmd::Import { path, skip_delisted, pairs: args } => {
            if skip_delisted || !args.pairs.is_empty() {
                let wanted = pairs::resolve(&client, &args).await?;
                let r = import::import_source(&path, &Selection::only(&wanted), &store)?;
                println!("imported {} of {} listed pairs", r.listed, wanted.len());
            } else {
                let all = client.asset_pairs().await?;
                let listed: HashSet<String> = all.iter().map(|p| p.altname.clone()).collect();
                let wanted: Vec<String> = all
                    .into_iter()
                    .filter(|p| pairs::is_usd_crypto(p, &args))
                    .map(|p| p.altname)
                    .collect();
                let delisted = |p: &str| pairs::is_delisted_usd_crypto(p, &listed, &args);
                let sel = Selection { listed: &wanted, extra: &delisted };
                let r = import::import_source(&path, &sel, &store)?;
                println!("imported {} of {} listed pairs", r.listed, wanted.len());
                println!("imported {} delisted pairs: {}", r.extra.len(), r.extra.join(" "));
            }
        }
        Cmd::Update(args) => {
            let wanted = pairs::resolve(&client, &args).await?;
            let report = update::update_pairs(&client, &store, &wanted).await;
            println!("updated {} of {} pairs", wanted.len() - report.failed.len(), wanted.len());
            if !report.failed.is_empty() {
                for (p, e) in &report.failed {
                    eprintln!("FAILED {p}: {e}");
                }
                bail!("{} pairs failed", report.failed.len());
            }
        }
        Cmd::Verify { no_live, pairs: args } => {
            // Verify what is on disk: listed pairs get the live comparison, files for pairs that
            // are no longer listed (delisted) get local checks only.
            let (wanted, delisted) = if args.pairs.is_empty() {
                let all = client.asset_pairs().await?;
                let listed: HashSet<String> = all.iter().map(|p| p.altname.clone()).collect();
                let wanted: Vec<String> = all
                    .into_iter()
                    .filter(|p| pairs::is_usd_crypto(p, &args) && store.exists(&p.altname))
                    .map(|p| p.altname)
                    .collect();
                let delisted: Vec<String> =
                    store.list()?.into_iter().filter(|p| !listed.contains(p)).collect();
                (wanted, delisted)
            } else {
                (pairs::resolve(&client, &args).await?, Vec::new())
            };
            let mut ok = verify::verify_pairs(&client, &store, &wanted, !no_live).await;
            ok &= verify::verify_pairs(&client, &store, &delisted, false).await;
            if !ok {
                bail!("verification found problems");
            }
            println!("all {} pairs OK ({} delisted, local checks only)", wanted.len() + delisted.len(), delisted.len());
        }
    }
    Ok(())
}
