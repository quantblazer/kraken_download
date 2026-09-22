# kraken-downloader

Builds a local CSV dataset of **daily OHLCV** candles (UTC days, closed at 00:00 UTC) for every **USD-quoted crypto pair** on Kraken. Only public endpoints are used, so **no API keys are needed**.

## Usage

```
cargo run --release -- list-pairs                       # USD pairs that would be processed
cargo run --release -- import --path Kraken_OHLCVT.zip  # full history from Kraken's bulk download
cargo run --release -- update                            # last 720 days + anything new (run after 00:00 UTC)
cargo run --release -- verify                            # integrity checks + live comparison
```

Common flags: `--data-dir data`, `--pairs XBTUSD ETHUSD`, `--epoch` (Unix-second timestamps), `--include-stablecoins`, `--include-fiat`, `--include-commodities`, `--online-only`, `--rate-ms 1100`.

## Why two sources
The REST OHLC endpoint returns only the latest 720 candles (~2 years). Older history comes from Kraken's bulk OHLCVT zip (linked from Kraken's support site: "Downloadable historical OHLCVT data"). Download it once and run `import --path` on the zip (or on the folder you extracted it into); then `update` fills the recent window and keeps it current.

## CSV format
`data/<PAIR>.csv` (e.g. `data/XBTUSD.csv`), ascending, one row per completed UTC day:

```
timestamp,symbol,open,high,low,close,volume
2024-10-01T00:00:00Z,XBTUSD,63325.0,64100.0,60203.4,60819.1,2632.62350634
```

The `symbol` column (Kraken altname) is written on every row so the file is self-describing even if renamed or moved — the filename is not the source of truth. `load` checks it and errors if a file's rows don't match the altname it was opened as.

Prices and volume are the exact decimal strings from Kraken. The still-forming current day is never written. `vwap` and trade count are not stored (Kraken's bulk files don't provide vwap either).

## API keys
Not used. If private endpoints are ever added, read keys from environment variables (`KRAKEN_API_KEY`, `KRAKEN_API_SECRET`) or a git-ignored `.env`; never store them in the repo or the data files.

## Notes
- TLS uses the OS certificate store (`rustls-tls-native-roots`), which is needed behind antivirus/proxy HTTPS inspection.
- Days with no trades have no candle, so `verify` can report gaps for illiquid pairs; that is expected.
