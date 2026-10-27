#![no_main]
use libfuzzer_sys::fuzz_target;

// The service's sealed key and its openings, checked before any money moves.
fuzz_target!(|data: &[u8]| {
    if let Ok(sealed) = serde_json::from_slice::<tidvar_seal::seal::SealedKey>(data) {
        let _ = sealed.commitment();
    }
    let _ = serde_json::from_slice::<Vec<tidvar_seal::seal::Opening>>(data);
    let _ = serde_json::from_slice::<tidvar_seal::seal::Challenge>(data);
});
