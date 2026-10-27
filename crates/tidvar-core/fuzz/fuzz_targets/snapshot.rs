#![no_main]
use libfuzzer_sys::fuzz_target;

// A saved chain snapshot restored from a backup.
fuzz_target!(|data: &[u8]| {
    if let Ok(snapshot) = serde_json::from_slice::<tidvar_chain::sync::Snapshot>(data) {
        let _ = tidvar_chain::sync::SyncState::restore(snapshot);
    }
});
