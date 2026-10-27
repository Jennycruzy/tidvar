//! Attack suite, time-seal part (SPEC 12.2), with a real drand quicknet round.
use drand_core::{beacon::ApiBeacon, chain::ChainInfo};
use tidvar_seal::{verify_beacon, verify_beacon_json};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const INFO: &str = include_str!("../../../evidence/testnet/drand/info.json");
const BEACON: &str = include_str!("../../../evidence/testnet/drand/beacon.json");
const PREVIOUS: &str = include_str!("../../../evidence/testnet/drand/previous.json");

/// AT12: a forged drand round signature never verifies.
#[test]
fn at12_forged_round_signature_fails_bls_verification() -> Result<()> {
    assert!(verify_beacon_json(INFO, BEACON).is_ok());
    let mut beacon: serde_json::Value = serde_json::from_str(BEACON)?;
    let signature = beacon["signature"].as_str().ok_or("signature")?.to_owned();
    let flipped = if signature.ends_with('0') { "1" } else { "0" };
    beacon["signature"] = (signature[..signature.len() - 1].to_owned() + flipped).into();
    assert!(verify_beacon_json(INFO, &beacon.to_string()).is_err());
    Ok(())
}

/// AT1 (local part): opening the seal early means presenting another round's signature as the
/// refund round; drand signatures are bound to their round, so it fails.
#[test]
fn at1_another_rounds_signature_cannot_stand_in_for_the_refund_round() -> Result<()> {
    let info: ChainInfo = serde_json::from_str(INFO)?;
    let previous: ApiBeacon = serde_json::from_str(PREVIOUS)?;
    let current: serde_json::Value = serde_json::from_str(BEACON)?;
    let mut early: serde_json::Value = serde_json::from_str(PREVIOUS)?;
    early["round"] = current["round"].clone();
    assert!(verify_beacon(&info, &previous).is_ok());
    assert!(verify_beacon_json(INFO, &early.to_string()).is_err());
    Ok(())
}
