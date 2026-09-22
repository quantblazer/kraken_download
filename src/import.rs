//! Import daily candles from Kraken's bulk OHLCVT data (`<PAIR>_1440.csv`, headerless:
//! `unix_ts,open,high,low,close,volume,trades`), either from the zip Kraken publishes or
//! from a directory it was already extracted into.

use crate::store::{merge, Candle, Store, DAY};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

pub fn parse_bulk_csv(reader: impl Read) -> Result<Vec<Candle>> {
    let mut rdr = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_reader(reader);
    let mut out = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.with_context(|| format!("row {}", i + 1))?;
        if rec.len() < 7 {
            bail!("row {}: expected 7 fields, got {}", i + 1, rec.len());
        }
        let ts: i64 = rec[0].trim().parse().with_context(|| format!("row {}: timestamp", i + 1))?;
        out.push(Candle {
            // Daily bars must sit on UTC midnight; normalise defensively.
            ts: ts - ts.rem_euclid(DAY),
            open: rec[1].to_string(),
            high: rec[2].to_string(),
            low: rec[3].to_string(),
            close: rec[4].to_string(),
            volume: rec[5].to_string(),
            // rec[6] is the trades count; the bulk file carries it but we don't store it.
        });
    }
    Ok(out)
}

/// Merge freshly-parsed bulk candles for one pair into the store and log the result.
fn merge_and_save(store: &Store, pair: &str, candles: Vec<Candle>) -> Result<()> {
    // A bulk row for a day that is not finished yet must not be stored.
    let now = chrono::Utc::now().timestamp();
    let candles: Vec<Candle> = candles.into_iter().filter(|c| c.ts + DAY <= now).collect();
    let mut series = store.load(pair)?;
    let stats = merge(&mut series, candles, false);
    store.save(pair, &series)?;
    tracing::info!(
        "{pair}: imported {} new days ({} total, {} overlapping rows differed)",
        stats.added,
        series.len(),
        stats.mismatched
    );
    Ok(())
}

fn warn_missing(wanted: &HashSet<&str>, found: &HashSet<&str>) {
    for w in wanted.iter().filter(|w| !found.contains(**w)) {
        tracing::warn!("{w}: no {w}_1440.csv found (new listing or not in bulk data); use `update`");
    }
}

/// Import from Kraken's bulk zip archive. Returns the number of pairs imported.
pub fn import_zip(zip_path: &Path, wanted: &[String], store: &Store) -> Result<usize> {
    let file = File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("reading zip")?;
    let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();

    let mut entries: Vec<(usize, String)> = Vec::new();
    for i in 0..archive.len() {
        let name = archive.by_index(i)?.name().to_string();
        let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
        if let Some(pair) = base.strip_suffix("_1440.csv") {
            if wanted.contains(pair) {
                entries.push((i, pair.to_string()));
            }
        }
    }
    warn_missing(&wanted, &entries.iter().map(|(_, p)| p.as_str()).collect());

    let mut imported = 0;
    for (idx, pair) in entries {
        let candles = parse_bulk_csv(archive.by_index(idx)?).with_context(|| format!("{pair}_1440.csv"))?;
        merge_and_save(store, &pair, candles)?;
        imported += 1;
    }
    Ok(imported)
}

/// Import from a directory the bulk zip was already extracted into (files may be flat or in
/// subdirectories; only `<PAIR>_1440.csv` files are read). Returns the number of pairs imported.
pub fn import_dir(dir: &Path, wanted: &[String], store: &Store) -> Result<usize> {
    let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    let mut entries: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(pair) = name.strip_suffix("_1440.csv") {
                    if wanted.contains(pair) {
                        entries.push((path.clone(), pair.to_string()));
                    }
                }
            }
        }
    }
    warn_missing(&wanted, &entries.iter().map(|(_, p)| p.as_str()).collect());

    let mut imported = 0;
    for (path, pair) in entries {
        let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let candles = parse_bulk_csv(file).with_context(|| format!("{}", path.display()))?;
        merge_and_save(store, &pair, candles)?;
        imported += 1;
    }
    Ok(imported)
}

/// Import from either a zip file or a directory of already-extracted bulk CSVs, based on
/// what `path` points to.
pub fn import_source(path: &Path, wanted: &[String], store: &Store) -> Result<usize> {
    if path.is_dir() {
        import_dir(path, wanted, store)
    } else {
        import_zip(path, wanted, store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_headerless_bulk_rows() {
        let data = "1381017600,122.0,122.0,122.0,122.0,0.1,1\n1381104000,123.61,124.0,123.0,123.9,5.5,3\n";
        let c = parse_bulk_csv(data.as_bytes()).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].ts, 1381017600);
        assert_eq!(c[1].close, "123.9");
        assert_eq!(c[1].volume, "5.5");
    }

    #[test]
    fn imports_matching_pairs_from_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bulk.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("Kraken_OHLCVT/XBTUSD_1440.csv", opts).unwrap();
            z.write_all(b"1381017600,122.0,122.0,122.0,122.0,0.1,1\n").unwrap();
            z.start_file("Kraken_OHLCVT/XBTUSD_60.csv", opts).unwrap();
            z.write_all(b"1381017600,1,1,1,1,1,1\n").unwrap();
            z.start_file("Kraken_OHLCVT/XBTEUR_1440.csv", opts).unwrap();
            z.write_all(b"1381017600,1,1,1,1,1,1\n").unwrap();
            z.finish().unwrap();
        }
        let store = Store::new(dir.path().join("data"), false);
        let n = import_source(&zip_path, &["XBTUSD".to_string(), "ETHUSD".to_string()], &store).unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.load("XBTUSD").unwrap().len(), 1);
        assert!(!store.exists("XBTEUR"));
        assert!(!store.exists("ETHUSD"));
    }

    #[test]
    fn imports_matching_pairs_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let bulk_dir = dir.path().join("Kraken_OHLCVT");
        fs::create_dir_all(&bulk_dir).unwrap();
        fs::write(bulk_dir.join("XBTUSD_1440.csv"), b"1381017600,122.0,122.0,122.0,122.0,0.1,1\n").unwrap();
        fs::write(bulk_dir.join("XBTUSD_60.csv"), b"1381017600,1,1,1,1,1,1\n").unwrap();
        fs::write(bulk_dir.join("XBTEUR_1440.csv"), b"1381017600,1,1,1,1,1,1\n").unwrap();

        let store = Store::new(dir.path().join("data"), false);
        let n = import_source(&bulk_dir, &["XBTUSD".to_string(), "ETHUSD".to_string()], &store).unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.load("XBTUSD").unwrap().len(), 1);
        assert!(!store.exists("XBTEUR"));
        assert!(!store.exists("ETHUSD"));
    }
}
