#![no_main]
use libfuzzer_sys::fuzz_target;

// A signing commitment (FROST round one) from the other party.
fuzz_target!(|data: &[u8]| {
    if let Ok(commitment) = serde_json::from_slice::<tidvar_core::frost::Commitment>(data) {
        let _ = commitment.verify();
    }
    if let Ok(message) = serde_json::from_slice::<tidvar_core::frost::Message>(data) {
        let _ = message.hash();
        let _ = message.package();
    }
});
