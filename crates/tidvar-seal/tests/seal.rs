use drand_core::{beacon::ApiBeacon, chain::ChainInfo};
use rand_core::{OsRng, RngCore};
use tidvar_core::keys::{FirstRound, Keys, Role};
use tidvar_seal::{
    ibe::{decrypt_exact, encrypt_with_sigma},
    seal::{SecretSeal, Verifier},
    verify_beacon,
};
use zcash_protocol::consensus::NetworkType;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn beacon() -> Result<(Vec<u8>, tidvar_seal::VerifiedBeacon, Vec<u8>)> {
    let info: ChainInfo =
        serde_json::from_slice(include_bytes!("../../../evidence/testnet/drand/info.json"))?;
    let beacon: ApiBeacon = serde_json::from_slice(include_bytes!(
        "../../../evidence/testnet/drand/beacon.json"
    ))?;
    let previous: ApiBeacon = serde_json::from_slice(include_bytes!(
        "../../../evidence/testnet/drand/previous.json"
    ))?;
    let verified = verify_beacon(&info, &beacon)?;
    let previous = verify_beacon(&info, &previous)?;
    assert_ne!(verified.round(), previous.round());
    Ok((info.public_key(), verified, previous.signature().to_vec()))
}
fn service_keys() -> Result<Keys> {
    let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
    let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (p, p2) = p.advance(s1)?;
    let (s, s2) = s.advance(p1)?;
    let payer = p.finish(s2, NetworkType::Test)?;
    let service = s.finish(p2, NetworkType::Test)?;
    assert_eq!(payer.public.group, service.public.group);
    Ok(service)
}

#[test]
fn original_tlock_and_exact_encryption_are_compatible() -> Result<()> {
    let (public, beacon, wrong) = beacon()?;
    for kind in 0..3 {
        let mut key = [0; 16];
        let mut sigma = [0; 16];
        OsRng.fill_bytes(&mut key);
        OsRng.fill_bytes(&mut sigma);
        match kind {
            1 => key[15] = 0,
            2 => key.fill(0),
            _ => {}
        }
        let ciphertext = encrypt_with_sigma(&public, beacon.round(), &key, &sigma)?;
        assert_eq!(
            decrypt_exact(beacon.signature(), &ciphertext)?,
            (key, sigma)
        );
        let mut original = Vec::new();
        tlock::decrypt(&mut original, &ciphertext[..], beacon.signature())?;
        assert!(original.len() <= 16);
        original.resize(16, 0);
        assert_eq!(original, key);
        let mut encrypted = Vec::new();
        tlock::encrypt(&mut encrypted, &key[..], &public, beacon.round())?;
        let (recovered, sigma) = decrypt_exact(beacon.signature(), &encrypted)?;
        assert_eq!(recovered, key);
        assert_eq!(
            encrypt_with_sigma(&public, beacon.round(), &recovered, &sigma)?,
            encrypted
        );
        assert!(decrypt_exact(&wrong, &ciphertext).is_err());
        let mut altered = ciphertext.clone();
        altered[127] ^= 1;
        assert!(decrypt_exact(beacon.signature(), &altered).is_err());
    }
    Ok(())
}

#[test]
fn full_seal_checks_thirty_two_openings_and_recovers_service_share() -> Result<()> {
    let (public, beacon, _) = beacon()?;
    let service = service_keys()?;
    let secret = service.service_share()?;
    let mut seal = SecretSeal::create(&secret, &public, beacon.round(), &mut OsRng)?;
    let verifier = Verifier::accept(
        seal.sealed.clone(),
        seal.sealed.commitment()?,
        &service.public.service,
        beacon.round(),
        &public,
    )?;
    let challenge = verifier.challenge(&mut OsRng);
    let opened = seal.answer(&challenge)?;
    assert_eq!(seal.answer(&challenge)?.len(), 32);
    let verified = verifier.finish(challenge, opened)?;
    assert_eq!(*verified.open(&beacon)?, *secret);
    Ok(())
}

#[test]
fn a_second_challenge_cannot_reveal_another_half() -> Result<()> {
    let (public, beacon, _) = beacon()?;
    let service = service_keys()?;
    let mut seal = SecretSeal::create(
        &*service.service_share()?,
        &public,
        beacon.round(),
        &mut OsRng,
    )?;
    let verifier = Verifier::accept(
        seal.sealed.clone(),
        seal.sealed.commitment()?,
        &service.public.service,
        beacon.round(),
        &public,
    )?;
    let first = verifier.challenge(&mut OsRng);
    seal.answer(&first)?;
    let mut second = verifier.challenge(&mut OsRng);
    while second.indices == first.indices {
        second = verifier.challenge(&mut OsRng);
    }
    assert!(seal.answer(&second).is_err());
    Ok(())
}

#[test]
fn changed_opening_and_changed_service_point_are_refused() -> Result<()> {
    let (public, beacon, _) = beacon()?;
    let service = service_keys()?;
    let mut seal = SecretSeal::create(
        &*service.service_share()?,
        &public,
        beacon.round(),
        &mut OsRng,
    )?;
    assert!(
        Verifier::accept(
            seal.sealed.clone(),
            seal.sealed.commitment()?,
            &service.public.payer,
            beacon.round(),
            &public
        )
        .is_err()
    );
    let verifier = Verifier::accept(
        seal.sealed.clone(),
        seal.sealed.commitment()?,
        &service.public.service,
        beacon.round(),
        &public,
    )?;
    let challenge = verifier.challenge(&mut OsRng);
    let mut opened = seal.answer(&challenge)?;
    opened[0].sigma[0] ^= 1;
    assert!(verifier.finish(challenge, opened).is_err());
    Ok(())
}
