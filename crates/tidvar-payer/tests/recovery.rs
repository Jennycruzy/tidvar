use rand_core::OsRng;
use tidvar_core::keys::{FirstRound, Role};
use tidvar_payer::{
    engine::{Payer, Setup},
    root::Root,
};
use tidvar_seal::{quicknet_public_key, round_at, seal::SecretSeal};
use zcash_protocol::consensus::NetworkType;

#[test]
fn complete_setup_and_recovery_keep_keys_seal_and_consumed_nonces()
-> Result<(), Box<dyn std::error::Error>> {
    let root = Root::generate(&mut OsRng);
    let (payer, p1) = Setup::begin(&mut OsRng)?;
    let (service, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (payer, p2) = payer.advance(s1)?;
    let (service, s2) = service.advance(p1)?;
    let payer = payer.finish(s2)?;
    let service = service.finish(p2, NetworkType::Test)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let round = round_at(now + 72 * 3600)?;
    let mut secret = SecretSeal::create(
        &*service.service_share()?,
        &quicknet_public_key()?,
        round,
        &mut OsRng,
    )?;
    let (pending, challenge) = payer.accept(
        secret.sealed.clone(),
        secret.sealed.commitment()?,
        round,
        &mut OsRng,
    )?;
    let mut payer = pending.finish(secret.answer(&challenge)?)?;
    assert_eq!(payer.public_keys().address, service.public.address);
    let signing_share =
        reddsa::frost::redpallas::keys::SigningShare::deserialize(&*service.service_share()?)?;
    let (_, commitment) = reddsa::frost::redpallas::round1::commit(&signing_share, &mut OsRng);
    let encoded = commitment.serialize()?;
    payer.consume_commitment(&encoded)?;
    assert!(payer.consume_commitment(&encoded).is_err());
    let service_key = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::new(OsRng);
    let offer = tidvar_core::record::Offer {
        version: 1,
        network: tidvar_core::record::Network::Testnet,
        service_name: "Tidvar VPN".to_owned(),
        address: service.public.address.clone(),
        maximum_lifetime_ms: 30 * 86_400_000,
        vpn_ms_per_zat: 6000,
        service_key: reddsa::VerificationKey::from(&service_key).into(),
    };
    let terms = tidvar_core::record::Terms {
        funded: tidvar_core::params::Amount::try_from(100_000)?,
        open_ms: now * 1000,
        lifetime: tidvar_core::params::Lifetime::Days3,
        open_height: 4_541_167,
        return_address: service.public.address.clone(),
    };
    let body = payer.record_body(&root, offer.clone(), terms.clone())?;
    let partial = tidvar_core::record::ServiceSigned::sign(body, &service_key, &mut OsRng)?;
    let record = payer.approve_record(&root, offer, terms, partial, &mut OsRng)?;
    record.verify()?;
    let mut changed = record.clone();
    changed.body.terms.funded = tidvar_core::params::Amount::try_from(100_001)?;
    assert!(changed.verify().is_err());
    let backup = payer.backup(&root)?;
    let phrase = root.phrase()?;
    let restored_root = Root::from_phrase(&phrase)?;
    let mut restored = Payer::restore(&restored_root, &backup)?;
    assert_eq!(payer.public_keys().address, restored.public_keys().address);
    assert_eq!(
        payer.seal().sealed().commitment()?,
        restored.seal().sealed().commitment()?
    );
    assert!(restored.consume_commitment(&encoded).is_err());
    assert!(Payer::restore(&Root::generate(&mut OsRng), &backup).is_err());
    Ok(())
}
