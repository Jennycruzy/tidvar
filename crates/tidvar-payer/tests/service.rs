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
        let id = accepted.record.body.tab_id;
        let body = serde_json::json!({"step":"pause"});
        let request = p.prepare_request(&root, body.clone())?;
        request.verify(saved.record.body.payer_record_key)?;
        let mut changed = request.clone();
        changed.message.body = serde_json::json!({"step":"resume"});
        assert!(changed.verify(saved.record.body.payer_record_key).is_err());
        p = tidvar_payer::engine::Payer::restore(&root, &p.backup(&root)?)?;
        let retry = p.prepare_request(&root, body.clone())?;
        assert_eq!(serde_json::to_vec(&request)?, serde_json::to_vec(&retry)?);
        assert!(store.cached_reply(&request)?.is_none());
        let balance = store.pause_vpn(&id, now)?;
        let reply = store.save_reply(&request, serde_json::to_value(balance)?)?;
        assert_eq!(
            store.cached_reply(&request)?.ok_or("Missing receipt")?,
            reply
        );
        p.accept_reply(serde_json::from_slice(&reply)?)?;
        p = tidvar_payer::engine::Payer::restore(&root, &p.backup(&root)?)?;
        let next = p.prepare_request(&root, body)?;
        assert_eq!(next.message.k, request.message.k + 2);
        let balance = store.pause_vpn(&id, now)?;
        let reply = store.save_reply(&next, serde_json::to_value(balance)?)?;
        p.accept_reply(serde_json::from_slice(&reply)?)?;
        assert!(store.cached_reply(&request).is_err());
        let limits = tidvar_payee::usage::Limits::default();
        let usage = store.record_transfer(&id, 4096, 8192, now, limits)?;
        assert_eq!(usage.daily_bytes, 12288);
        assert_eq!(
            store
                .record_transfer(&id, 4096, 8192, now, limits)?
                .daily_bytes,
            12288
        );
        store.reset_transfer_counter(&id, now)?;
        assert_eq!(
            store
                .record_transfer(&id, 256, 512, now + 1, limits)?
                .daily_bytes,
            13056
        );
        let next_midnight = (now / 86_400_000 + 1) * 86_400_000;
        assert_eq!(store.usage(&id, next_midnight, limits)?.daily_bytes, 0);
        assert_eq!(store.usage(&id, now, limits)?.daily_bytes, 13056);
        assert_eq!(
            store
                .usage(
                    &id,
                    now,
                    tidvar_payee::usage::Limits {
                        daily: 13056,
                        monthly: limits.monthly
                    }
                )?
                .limit,
            Some(tidvar_payee::usage::Limit::Daily)
        );
        assert_eq!(
            store
                .usage(
                    &id,
                    now,
                    tidvar_payee::usage::Limits {
                        daily: limits.daily,
                        monthly: 13056
                    }
                )?
                .limit,
            Some(tidvar_payee::usage::Limit::Monthly)
        );

        let mut wireguard = [0; 32];
        OsRng.fill_bytes(&mut wireguard);
        let peer = store.register_peer(&id, wireguard)?;
        assert_eq!(peer.address()?, std::net::Ipv4Addr::new(10, 77, 0, 2));
        assert_eq!(store.register_peer(&id, wireguard)?.slot, peer.slot);
        assert!(store.register_peer(&id, [7; 32]).is_err());
        assert!(store.register_peer(&[9; 32], [8; 32]).is_err());
        assert_eq!(store.peers()?.len(), 1);
        let pause = p.prepare_request(&root, serde_json::json!({"step":"vpn_pause"}))?;
        let balance = store.pause_vpn(&id, now + 1)?;
        let reply = store.save_reply(
            &pause,
            serde_json::json!({"step":"vpn_paused","balance":balance}),
        )?;
        p.accept_reply_at(serde_json::from_slice(&reply)?, now + 1)?;
        assert!(p.authorization_balance(now + 2)?.paused);
        assert!(store.balance(&id, now + 2)?.paused);
        let resume = p.prepare_request(&root, serde_json::json!({"step":"vpn_resume"}))?;
        let balance = store.request_vpn_resume(&id, now + 3)?;
        assert!(!balance.paused && !balance.running);
        let reply = store.save_reply(
            &resume,
            serde_json::json!({"step":"vpn_resumed","balance":balance}),
        )?;
        p.accept_reply_at(serde_json::from_slice(&reply)?, now + 3)?;
        assert!(!p.authorization_balance(now + 4)?.paused);
        assert!(store.resume_vpn(&id, now + 4).is_err());
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
