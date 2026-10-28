//! Attack suite, time-seal part, with a real drand quicknet round.
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

use chacha20::cipher::{KeyIvInit, StreamCipher};
use ff::PrimeField;
use pasta_curves::pallas::Scalar;
use rand_core::{OsRng, RngCore};
use tidvar_seal::{
    ibe::decrypt_exact,
    seal::{Challenge, Opening, SecretSeal, Verifier},
};

fn dishonest_half() -> Result<(SecretSeal, Vec<u8>, tidvar_seal::VerifiedBeacon)> {
    let info: ChainInfo = serde_json::from_str(INFO)?;
    let beacon = verify_beacon_json(INFO, BEACON)?;
    let mut secret = [0; 32];
    loop {
        OsRng.fill_bytes(&mut secret);
        if Option::<Scalar>::from(Scalar::from_repr(secret)).is_some() {
            break;
        }
    }
    let mut seal = SecretSeal::create(&secret, &info.public_key(), beacon.round(), &mut OsRng)?;
    for share in &mut seal.sealed.shares[32..] {
        share.encrypted_share[0] ^= 1;
    }
    Ok((seal, info.public_key(), beacon))
}
fn verifier(seal: &SecretSeal, public: &[u8]) -> Result<Verifier> {
    Ok(Verifier::accept(
        seal.sealed.clone(),
        seal.sealed.commitment()?,
        &seal.sealed.coefficients[0].0,
        seal.sealed.round,
        public,
    )?)
}
fn first_half(seal: &SecretSeal) -> Result<Challenge> {
    Ok(Challenge {
        commitment: seal.sealed.commitment()?,
        indices: (1..=32).collect(),
    })
}

#[test]
fn at5_one_corrupt_opened_share_refuses_funding() -> Result<()> {
    let (mut seal, public, _) = dishonest_half()?;
    seal.sealed.shares[0].encrypted_share[0] ^= 1;
    let check = verifier(&seal, &public)?;
    let challenge = first_half(&seal)?;
    let opened = seal.answer(&challenge)?;
    let error = check
        .finish(challenge, opened)
        .err()
        .ok_or("Corrupt opening accepted")?;
    assert_eq!(
        error.to_string(),
        "The service's sealed key failed our check, so we didn't open the tab. No money moved."
    );
    println!("AT5 opened-share refusal: {error}");
    Ok(())
}

#[test]
fn at5_corrupt_unopened_half_cannot_release_a_refund_key() -> Result<()> {
    let (mut seal, public, beacon) = dishonest_half()?;
    let check = verifier(&seal, &public)?;
    let challenge = first_half(&seal)?;
    let opened = seal.answer(&challenge)?;
    // Conditional on the one challenge that selects all valid shares, funding checks pass.
    let accepted = check.finish(challenge, opened)?;
    let error = accepted
        .open(&beacon)
        .err()
        .ok_or("Invalid refund key released")?;
    println!("AT5 unopened-share refusal after actual beacon decryption: {error}");
    Ok(())
}

#[test]
fn at5_exactly_thirty_two_valid_shares_over_ten_thousand_challenges() -> Result<()> {
    let (seal, public, beacon) = dishonest_half()?;
    let check = verifier(&seal, &public)?;
    let mut valid = Vec::new();
    // Verify each actual encrypted share once, then reuse that deterministic result while
    // exercising the production CSPRNG challenge sampler ten thousand times.
    for (offset, share) in seal.sealed.shares.iter().enumerate() {
        let (key, sigma) = decrypt_exact(beacon.signature(), &share.time_key)?;
        let derived = tidvar_core::encoding::hash(
            b"TidvarSealKey",
            &tidvar_core::encoding::encode_bytes(&[&key])?,
        )?;
        let mut value = share.encrypted_share;
        let mut cipher = chacha20::ChaCha20::new((&derived).into(), (&[0; 12]).into());
        cipher.apply_keystream(&mut value);
        let opening = Opening {
            index: (offset + 1) as u8,
            share: value,
            key,
            sigma,
        };
        valid.push(check.verify_opening(&opening).is_ok());
    }
    assert_eq!(valid.iter().filter(|&&v| v).count(), 32);
    let trials = 10_000;
    let mut accepted = 0;
    for _ in 0..trials {
        let challenge = check.challenge(&mut OsRng);
        let unique: std::collections::BTreeSet<_> = challenge.indices.iter().collect();
        assert_eq!(unique.len(), 32);
        accepted += usize::from(challenge.indices.iter().all(|i| valid[usize::from(*i) - 1]));
    }
    let mut combinations = 1u128;
    for k in 1..=32u128 {
        combinations = combinations * (65 - k) / k;
    }
    assert_eq!(combinations, 1_832_624_140_942_590_534);
    let probability = 1.0 / combinations as f64;
    println!(
        "valid | challenges | accepted | observed rate | exact probability | expected accepts"
    );
    println!(
        "32 | {trials} | {accepted} | {:.9} | {probability:.19e} | {:.19e}",
        accepted as f64 / f64::from(trials),
        probability * f64::from(trials)
    );
    println!(
        "Zero observations cannot resolve a probability this small; the exact probability is combinatorial, not an empirical confidence claim."
    );
    Ok(())
}
