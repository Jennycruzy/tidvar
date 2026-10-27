#![no_main]
use libfuzzer_sys::fuzz_target;

// An IOU: the payment request and the transaction it carries.
fuzz_target!(|data: &[u8]| {
    if let Ok(request) = serde_json::from_slice::<tidvar_core::iou::Request>(data) {
        let _ = request.hash(&[1; 32]);
        let _ = pczt::Pczt::parse(&request.pczt);
    }
    let _ = pczt::Pczt::parse(data);
});
