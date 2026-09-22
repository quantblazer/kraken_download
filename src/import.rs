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
/// Returns false (and writes nothing) when the file had no completed days.
fn merge_and_save(store: &Store, pair: &str, candles: Vec<Candle>) -> Result<bool> {
    // A bulk row for a day that is not finished yet must not be stored.
    let now = chrono::Utc::now().timestamp();
    let candles: Vec<Candle> = candles.into_iter().filter(|c| c.ts + DAY <= now).collect();
    if candles.is_empty() {
        tracing::warn!("{pair}: bulk file has no completed days; skipped");
        return Ok(false);
    }
    let mut series = store.load(pair)?;
    let stats = merge(&mut series, candles, false);
    store.save(pair, &series)?;
    tracing::info!(
        "{pair}: imported {} new days ({} total, {} overlapping rows differed)",
        stats.added,
        series.len(),
        stats.mismatched
    );
    Ok(true)
}

/// Pair name of a bulk daily file (`.../XBTUSD_1440.csv` -> `XBTUSD`). macOS metadata that
/// often ships inside the zip or the extracted folder (`__MACOSX/`, `._XBTUSD_1440.csv`) is
/// ignored.
fn bulk_pair(path: &str) -> Option<&str> {
    if path.contains("__MACOSX") {
        return None;
    }
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if base.starts_with("._") {
        return None;
    }
    base.strip_suffix("_1440.csv")
}

/// Which bulk files to import: the `listed` pairs (a warning is logged for each one the bulk
/// data lacks) plus any other pair name `extra` accepts (used for delisted pairs).
pub struct Selection<'a> {
    pub listed: &'a [String],
    pub extra: &'a dyn Fn(&str) -> bool,
}

impl Selection<'_> {
    /// Only the given pairs, nothing extra.
    pub fn only(listed: &[String]) -> Selection<'_> {
        Selection { listed, extra: &|_| false }
    }
}

#[derive(Debug, Default)]
pub struct ImportReport {
    /// Listed pairs imported.
    pub listed: usize,
    /// Pairs imported through `Selection::extra` (delisted).
    pub extra: Vec<String>,
}

/// Picks the bulk files to import from `(locator, pair)` candidates and imports them in name
/// order, reading each through `open`.
fn import_entries<L, R: Read>(
    candidates: Vec<(L, String)>,
    sel: &Selection,
    store: &Store,
    mut open: impl FnMut(&L) -> Result<R>,
) -> Result<ImportReport> {
    let listed: HashSet<&str> = sel.listed.iter().map(String::as_str).collect();
    let mut picked: Vec<(L, String, bool)> = candidates
        .into_iter()
        .filter_map(|(loc, pair)| {
            let is_listed = listed.contains(pair.as_str());
            (is_listed || (sel.extra)(&pair)).then_some((loc, pair, is_listed))
        })
        .collect();
    picked.sort_by(|a, b| a.1.cmp(&b.1));
    picked.dedup_by(|a, b| a.1 == b.1);

    let found: HashSet<&str> = picked.iter().map(|(_, p, _)| p.as_str()).collect();
    for w in sel.listed.iter().filter(|w| !found.contains(w.as_str())) {
        tracing::warn!("{w}: no {w}_1440.csv found (new listing or not in bulk data); use `update`");
    }

    let mut report = ImportReport::default();
    for (loc, pair, is_listed) in &picked {
        let candles = parse_bulk_csv(open(loc)?).with_context(|| format!("{pair}_1440.csv"))?;
        if merge_and_save(store, pair, candles)? {
            if *is_listed {
                report.listed += 1;
            } else {
                report.extra.push(pair.clone());
            }
        }
    }
    Ok(report)
}

/// Import from Kraken's bulk zip archive.
pub fn import_zip(zip_path: &Path, sel: &Selection, store: &Store) -> Result<ImportReport> {
    let file = File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("reading zip")?;
    let mut candidates = Vec::new();
    for i in 0..archive.len() {
        let name = archive.by_index(i)?.name().to_string();
        if let Some(pair) = bulk_pair(&name) {
            candidates.push((i, pair.to_string()));
        }
    }
    import_entries(candidates, sel, store, |&i| {
        // Read the entry fully so the archive borrow ends before the next one.
        let mut buf = Vec::new();
        archive.by_index(i)?.read_to_end(&mut buf)?;
        Ok(std::io::Cursor::new(buf))
    })
}

/// Import from a directory the bulk zip was already extracted into (files may be flat or in
/// subdirectories; only `<PAIR>_1440.csv` files are read).
pub fn import_dir(dir: &Path, sel: &Selection, store: &Store) -> Result<ImportReport> {
    let mut candidates = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(pair) = path.to_str().and_then(bulk_pair) {
                let pair = pair.to_string();
                candidates.push((path, pair));
            }
        }
    }
    import_entries(candidates, sel, store, |path| {
        File::open(path).with_context(|| format!("opening {}", path.display()))
    })
}

/// Import from either a zip file or a directory of already-extracted bulk CSVs, based on
/// what `path` points to.
pub fn import_source(path: &Path, sel: &Selection, store: &Store) -> Result<ImportReport> {
    if path.is_dir() {
        import_dir(path, sel, store)
    } else {
        import_zip(path, sel, store)
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
            z.start_file("__MACOSX/Kraken_OHLCVT/._XBTUSD_1440.csv", opts).unwrap();
            z.write_all(b"\x00\x05\x16\x07 binary junk").unwrap();
            z.finish().unwrap();
        }
        let store = Store::new(dir.path().join("data"), false);
        let wanted = ["XBTUSD".to_string(), "ETHUSD".to_string()];
        let r = import_source(&zip_path, &Selection::only(&wanted), &store).unwrap();
        assert_eq!(r.listed, 1);
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
        fs::write(bulk_dir.join("._XBTUSD_1440.csv"), b"\x00\x05\x16\x07 binary junk").unwrap();

        let store = Store::new(dir.path().join("data"), false);
        let wanted = ["XBTUSD".to_string(), "ETHUSD".to_string()];
        let r = import_source(&bulk_dir, &Selection::only(&wanted), &store).unwrap();
        assert_eq!(r.listed, 1);
        assert_eq!(store.load("XBTUSD").unwrap().len(), 1);
        assert!(!store.exists("XBTEUR"));
        assert!(!store.exists("ETHUSD"));
    }

    #[test]
    fn imports_extra_pairs_and_skips_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let bulk_dir = dir.path().join("bulk");
        fs::create_dir_all(&bulk_dir).unwrap();
        fs::write(bulk_dir.join("XBTUSD_1440.csv"), b"1381017600,1,1,1,1,1,1\n").unwrap();
        fs::write(bulk_dir.join("EOSUSD_1440.csv"), b"1381017600,2,2,2,2,2,2\n").unwrap();
        fs::write(bulk_dir.join("CGNUSD_1440.csv"), b"").unwrap();
        fs::write(bulk_dir.join("XBTPYUSD_1440.csv"), b"1381017600,3,3,3,3,3,3\n").unwrap();

        let store = Store::new(dir.path().join("data"), false);
        let wanted = ["XBTUSD".to_string()];
        let extra = |p: &str| p != "XBTPYUSD";
        let r = import_source(&bulk_dir, &Selection { listed: &wanted, extra: &extra }, &store).unwrap();
        assert_eq!(r.listed, 1);
        assert_eq!(r.extra, vec!["EOSUSD".to_string()]);
        assert!(store.exists("EOSUSD"));
        assert!(!store.exists("CGNUSD"), "empty bulk file must not create a CSV");
        assert!(!store.exists("XBTPYUSD"));
    }
}
