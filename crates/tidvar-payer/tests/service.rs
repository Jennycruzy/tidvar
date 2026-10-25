#![cfg(not(target_arch = "wasm32"))]
use rand_core::{OsRng, RngCore};
use std::os::unix::fs::PermissionsExt;
use tidvar_core::{
    keys::{FirstRound, Role},
    params::{Amount, Lifetime},
    record::{Network, Offer, Terms},
};
use tidvar_payee::{opening, store::Store};
use tidvar_payer::{engine::Setup, root::Root};
use zcash_protocol::consensus::NetworkType;

fn fresh_address() -> Result<String, Box<dyn std::error::Error>> {
    let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
    let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (p, _) = p.advance(s1)?;
    let (_, s2) = s.advance(p1)?;
    Ok(p.finish(s2, NetworkType::Test)?.public.address)
}
#[test]
fn separate_service_saves_only_its_own_share_after_both_signatures()
-> Result<(), Box<dyn std::error::Error>> {
    let root = Root::generate(&mut OsRng);
    let key = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::new(OsRng);
    let offer = Offer {
        version: 1,
        network: Network::Testnet,
        service_name: "Tidvar VPN".to_owned(),
        address: fresh_address()?,
        maximum_lifetime_ms: 30 * 86_400_000,
        vpn_ms_per_zat: 6000,
        service_key: reddsa::VerificationKey::from(&key).into(),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        * 1000;
    let terms = Terms {
        funded: Amount::try_from(100_000)?,
        open_ms: now,
        lifetime: Lifetime::Days3,
        open_height: 4_541_167,
        return_address: fresh_address()?,
    };
    let (p, p1) = Setup::begin(&mut OsRng)?;
    let (s, s1, s2) = opening::Setup::begin(offer.clone(), terms.clone(), p1, &mut OsRng)?;
    let (p, p2) = p.advance(s1)?;
    let p = p.finish(s2)?;
    let mut s = s.finish(p2, &mut OsRng)?;
    let sealed = s.sealed().clone();
    let (p, challenge) = p.accept(
        sealed.clone(),
        sealed.commitment()?,
        sealed.round,
        &mut OsRng,
    )?;
    let mut p = p.finish(s.answer(challenge)?)?;
    let body = p.record_body(&root, offer.clone(), terms.clone())?;
    let partial = s.sign(body, &key, &mut OsRng)?;
    let signed = p.approve_record(&root, offer, terms, partial, &mut OsRng)?;
    let accepted = s.accept(signed)?;
    let request = p.funding_request()?;
    assert_eq!(request.amount, "0.001");
    let decoded = zip321::TransactionRequest::from_uri(&request.uri)?;
    assert_eq!(decoded.payments().len(), 1);
    let payment = decoded
        .payments()
        .values()
        .next()
        .ok_or("No payment request")?;
    assert_eq!(
        payment.amount().ok_or("No amount")?.into_u64(),
        request.amount_zat
    );
    assert_eq!(payment.recipient_address().to_string(), request.address);
    assert!(payment.memo().is_none());
    let mut bytes = [0; 16];
    OsRng.fill_bytes(&mut bytes);
    let path = std::env::temp_dir().join(format!("tidvar-{}.sqlite", hex::encode(bytes)));
    {
        let mut store = Store::open(&path)?;
        store.save(&accepted)?;
        store.save(&accepted)?;
        let saved = store
            .load(&accepted.record.body.tab_id)?
            .ok_or("The accepted tab was not saved")?;
        assert_eq!(saved.keys.role, Role::Service);
        assert_eq!(saved.keys.public.address, p.public_keys().address);
        assert_eq!(*saved.keys.secret_bytes()?, *accepted.keys.secret_bytes()?);
        saved.record.verify()?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        for suffix in ["-wal", "-shm"] {
            let auxiliary = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            if auxiliary.exists() {
                assert_eq!(
                    std::fs::metadata(auxiliary)?.permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }
    {
        let store = Store::open(&path)?;
        assert!(store.load(&accepted.record.body.tab_id)?.is_some());
    }
    std::fs::remove_file(path)?;
    Ok(())
}
