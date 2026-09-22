use kraken_downloader::client::Client;
use kraken_downloader::store::{Store, DAY};
use kraken_downloader::update::update_pairs;
use std::time::Duration;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn row(ts: i64, close: &str) -> serde_json::Value {
    serde_json::json!([ts, "1.0", "3.0", "0.5", close, "2.0", "10.5", 7])
}

#[tokio::test]
async fn update_writes_completed_days_only_and_is_idempotent() {
    let now = chrono::Utc::now().timestamp();
    let today = now - now.rem_euclid(DAY);
    let (d3, d2, d1) = (today - 3 * DAY, today - 2 * DAY, today - DAY);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/0/public/OHLC"))
        .and(query_param("interval", "1440"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": [],
            "result": {
                "XXBTZUSD": [row(d3, "2.0"), row(d2, "2.5"), row(d1, "2.7"), row(today, "9.9")],
                "last": d1
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path(), false);
    let client = Client::new(server.uri(), Duration::from_millis(0)).unwrap();
    let pairs = vec!["XBTUSD".to_string()];

    let report = update_pairs(&client, &store, &pairs).await;
    assert!(report.failed.is_empty());
    let s = store.load("XBTUSD").unwrap();
    assert_eq!(s.keys().copied().collect::<Vec<_>>(), vec![d3, d2, d1], "open day must be dropped");
    assert_eq!(s[&d1].close, "2.7");

    // Second run: no duplicates, same content.
    let report = update_pairs(&client, &store, &pairs).await;
    assert!(report.failed.is_empty());
    assert_eq!(store.load("XBTUSD").unwrap(), s);
}

#[tokio::test]
async fn kraken_error_is_reported_per_pair() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/0/public/OHLC"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": ["EQuery:Unknown asset pair"]
        })))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path(), false);
    let client = Client::new(server.uri(), Duration::from_millis(0)).unwrap();
    let report = update_pairs(&client, &store, &["NOPEUSD".to_string()]).await;
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].1.contains("Unknown asset pair"));
    assert!(!store.exists("NOPEUSD"));
}
