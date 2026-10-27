#![no_main]
use libfuzzer_sys::fuzz_target;

// A drand beacon and chain description from any relay.
fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (info, beacon) = text.split_once('\n').unwrap_or((&text, ""));
    let _ = tidvar_seal::verify_beacon_json(info, beacon);
});
