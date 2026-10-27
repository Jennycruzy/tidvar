#![no_main]
use libfuzzer_sys::fuzz_target;

// A signed tab record (terms, deadlines, keys) from the other party or a backup.
fuzz_target!(|data: &[u8]| {
    if let Ok(record) = serde_json::from_slice::<tidvar_core::record::SignedRecord>(data) {
        let _ = record.verify();
        let _ = record.body.terms.validate(&record.body.offer);
    }
});
