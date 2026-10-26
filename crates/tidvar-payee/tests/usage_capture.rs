#![cfg(not(target_arch = "wasm32"))]
use serde::Deserialize;
use std::path::PathBuf;
use tidvar_payee::{store::Store, usage::Limits};

#[derive(Deserialize)]
struct Capture {
    tab_id: String,
    rx: u64,
    tx: u64,
    at_ms: u64,
}

#[test]
#[ignore = "Requires an actual captured WireGuard counter snapshot and its service database"]
fn actual_transfer_capture_is_counted_once_across_store_recovery()
-> Result<(), Box<dyn std::error::Error>> {
    let capture: Capture =
        serde_json::from_slice(&std::fs::read(std::env::var("TIDVAR_USAGE_CAPTURE_PATH")?)?)?;
    let id: [u8; 32] = hex::decode(&capture.tab_id)?
        .try_into()
        .map_err(|_| "Invalid captured tab id")?;
    assert!(capture.rx > 0 && capture.tx > 0);
    let path = PathBuf::from(std::env::var("TIDVAR_DATA_DIR")?).join("payee.sqlite");
    let mut store = Store::open(&path)?;
    store.load(&id)?.ok_or("The actual tab record is missing")?;
    let limits = Limits::default();
    let before = store.usage(&id, capture.at_ms, limits)?;
    let counted = store.record_transfer(&id, capture.rx, capture.tx, capture.at_ms, limits)?;
    assert!(counted.daily_bytes >= before.daily_bytes);
    assert!(counted.monthly_bytes >= before.monthly_bytes);
    drop(store);
    let mut store = Store::open(&path)?;
    let retry = store.record_transfer(&id, capture.rx, capture.tx, capture.at_ms, limits)?;
    assert_eq!(counted.daily_bytes, retry.daily_bytes);
    assert_eq!(counted.monthly_bytes, retry.monthly_bytes);
    assert_eq!(retry.limit, None);
    println!(
        "TESTNET actual WireGuard capture: rx{} tx{}; daily{} monthly{}; identical snapshot retry after restart counted once.",
        capture.rx, capture.tx, retry.daily_bytes, retry.monthly_bytes
    );
    Ok(())
}
