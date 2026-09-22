# Kraken Daily OHLCV Downloader (Rust)

## Context
Rust CLI that builds a local dataset of **daily OHLCV** candles for **every USD-quoted pair** listed on Kraken (BTC, ETH and all other listed cryptos), with **full history**, stored as CSV for backtesting (e.g. NautilusTrader).

## Data sources (why two)
Kraken's `GET /0/public/OHLC?pair=..&interval=1440` returns only the **latest 720 candles** (~2 years); `since` cannot reach further back. So:

1. **Old history: Kraken bulk OHLCVT download.** Kraken publishes a zip of CSVs (all pairs, from first trade, incl. 1440-min bars) via its support site (hosted on Google Drive, updated periodically). The user downloads it once; the tool imports the `*_1440.csv` files. No trade-by-trade pulling.
2. **Recent history and daily updates: OHLC REST endpoint.** Covers the last 720 days, so it fills the gap after the bulk file's end date and overlaps it for cross-checking.
3. **Fallback only (not v1):** aggregate raw `/Trades` (1000/call, paginated) for a pair missing from the bulk file. Slow (BTC likely hours), so optional.

## Design
Cargo binary `kraken-downloader`. Crates: `tokio`, `reqwest` (rustls), `serde`/`serde_json`, `csv`, `zip`, `chrono`, `clap`, `anyhow`, `tracing`, `governor` (rate limit).

Modules (`src/`):
- `client.rs` – REST client: `asset_pairs()`, `ohlc(pair, since)`. Handles the `error` array, `EGeneral:Too many requests` backoff, retries with exponential delay, rate limiter.
- `pairs.rs` – calls `AssetPairs`, keeps pairs whose quote is `ZUSD`/`USD` and status `online`, skips dark-pool `.d` pairs. Stablecoin/USD pairs (USDT, USDC...) excluded by default; `--include-stablecoins` keeps them. `--pairs` overrides discovery. Maps Kraken legacy names (`XXBTZUSD`) to `altname` (`XBTUSD`), which is also the bulk-file naming.
- `import.rs` – `import --zip <Kraken_OHLCVT.zip>`: reads `<ALTNAME>_1440.csv` (headerless: `unix_ts,open,high,low,close,volume,trades`) for the selected USD pairs and writes/merges into the store. The source file's trades column is read but not stored.
- `update.rs` – fetches OHLC for each pair, merges into the store, drops the still-open current-day candle. Works incrementally; if the stored data ends more than 720 days ago it warns that a newer bulk file is needed.
- `store.rs` – CSV per pair `data/<ALTNAME>_1d.csv`; merge, dedupe by timestamp, ascending sort; on overlap, prefers REST OHLC values and reports differences vs bulk.
- `verify.rs` – local integrity checks (gaps, non-midnight timestamps, invalid OHLC) plus a live comparison against the REST endpoint.
- `main.rs` – clap subcommands: `list-pairs`, `import`, `update`, `verify`.

## CSV format
Header row, one line per completed UTC day, ascending:

```
timestamp,symbol,open,high,low,close,volume
2024-01-01T00:00:00Z,XBTUSD,42283.5,44184.0,42180.0,44179.9,5321.12345678
```
- `timestamp` = UTC day open, ISO 8601 (`--epoch` writes Unix seconds instead).
- `symbol` (Kraken altname) is written on every row so the file is self-describing even if renamed or moved; `load` errors if a file's rows don't match the altname it was opened as.
- Prices and volume kept as exact decimal strings (no float rounding). Volume is base-asset volume.
- `vwap` and trade count are deliberately not stored — dropped at the user's request to keep the CSV to the fields actually needed for OHLCV backtesting.

## Files to create
`Cargo.toml`, `src/{lib,main,client,pairs,import,update,store,verify}.rs`, `README.md`, unit tests (bulk-CSV parsing, merge/dedupe, drop-open-candle, pair filtering, symbol-mismatch detection), integration test with a mocked HTTP server (`wiremock`).

## Verification (real data)
1. `cargo test`.
2. `list-pairs` prints only USD pairs.
3. Download the real Kraken bulk zip, run `import`, then `update`.
4. Overlap check: for the last 720 days, compare bulk-imported vs live REST candles (open/high/low/close/volume) and report mismatches.
5. Sanity checks on all files: sorted, no duplicate timestamps, no missing days between first and last date (or list the gaps), `high >= max(open, close)`, `low <= min(open, close)`.
6. Rerun `update` and confirm it is idempotent (no duplicates, only new rows appended).

## Assumptions to confirm
- "Listed" = currently online USD pairs; delisted pairs are only picked up if they exist in the bulk zip and you ask for them by name.
- User downloads the bulk zip manually from Kraken's support page (link to be recorded in the README after checking the current URL).
- CSV only for now; Parquet can be added later.
