#![no_main]
use libfuzzer_sys::fuzz_target;

// grpc-web frames relayed from a Zcash server.
fuzz_target!(|data: &[u8]| {
    let _ = tidvar_chain::transport::decode_frames(data);
});
