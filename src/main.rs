use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use kraken_downloader::client::{Client, DEFAULT_BASE};
use kraken_downloader::pairs::{self, PairArgs};
use kraken_downloader::store::Store;
use kraken_downloader::{import, update, verify};
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
        Cmd::Import { path, pairs: args } => {
            let wanted = pairs::resolve(&client, &args).await?;
            let n = import::import_source(&path, &wanted, &store)?;
            println!("imported {n} of {} pairs into {}", wanted.len(), cli.data_dir.display());
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
            let wanted = if args.pairs.is_empty() {
                // Verify what is on disk rather than what is currently listed.
                let discovered = pairs::resolve(&client, &args).await?;
                discovered.into_iter().filter(|p| store.exists(p)).collect()
            } else {
                pairs::resolve(&client, &args).await?
            };
            if !verify::verify_pairs(&client, &store, &wanted, !no_live).await {
                bail!("verification found problems");
            }
            println!("all {} pairs OK", wanted.len());
        }
    }
    Ok(())
}
