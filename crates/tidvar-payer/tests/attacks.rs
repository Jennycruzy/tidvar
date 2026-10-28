use rand_core::OsRng;
use tidvar_core::keys::{FirstRound, Role};
use tidvar_payer::engine::Setup;
use tidvar_seal::{quicknet_public_key, seal::SecretSeal};
use zcash_protocol::consensus::NetworkType;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn at6_engine_refuses_a_repeated_service_commitment() -> Result<()> {
    let (payer, p1) = Setup::begin(&mut OsRng)?;
    let (service, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (payer, p2) = payer.advance(s1)?;
    let (service, s2) = service.advance(p1)?;
    let payer = payer.finish(s2, tidvar_core::record::Network::Testnet)?;
    let service = service.finish(p2, NetworkType::Test)?;
    let beacon: serde_json::Value =
        serde_json::from_str(include_str!("../../../evidence/testnet/drand/beacon.json"))?;
    let round = beacon["round"].as_u64().ok_or("Missing recorded round")?;
    let mut seal = SecretSeal::create(
        &*service.service_share()?,
        &quicknet_public_key()?,
        round,
        &mut OsRng,
    )?;
    let (pending, challenge) = payer.accept(
        seal.sealed.clone(),
        seal.sealed.commitment()?,
        round,
        &mut OsRng,
    )?;
    let mut payer = pending.finish(seal.answer(&challenge)?)?;
    let nonce = tidvar_core::frost::Nonce::generate(&service, &mut OsRng)?;
    let bytes = nonce.public().data.clone();
    payer.consume_commitment(&bytes)?;
    let error = payer
        .consume_commitment(&bytes)
        .err()
        .ok_or("Repeated commitment accepted")?;
    assert_eq!(
        error.to_string(),
        "This payment commitment has already been used."
    );
    println!("AT6 payer refusal: {error}");
    Ok(())
}
