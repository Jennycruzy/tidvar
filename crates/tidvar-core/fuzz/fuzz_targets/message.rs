#![no_main]
use libfuzzer_sys::fuzz_target;

// A payer or service message as it arrives over HTTP.
fuzz_target!(|data: &[u8]| {
    if let Ok(signed) = serde_json::from_slice::<tidvar_core::transport::Authenticated>(data) {
        let _ = signed.message.hash();
        let _ = signed.verify([7; 32]);
        let _ = signed.message.follows(signed.message.tab_id, 1, None);
    }
});
