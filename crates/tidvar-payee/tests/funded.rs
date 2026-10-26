#![cfg(not(target_arch = "wasm32"))]
use orchard::{keys::Scope, note_encryption::IronwoodDomain};
use rand_core::OsRng;
use serde::Deserialize;
use std::{path::PathBuf, time::Instant};
use tidvar_chain::{
    native::{Client, TESTNET_SERVER},
    sync::SyncState,
};
use tidvar_core::{
    iou::{self, Kind, TransactionContext},
    params::Amount,
};
use tidvar_payee::{proof::Proofs, store::Store};
use zcash_protocol::consensus::BranchId;

#[derive(Deserialize)]
struct Prepared {
    #[serde(with = "tidvar_core::encoding::bytes")]
    pczt: Vec<u8>,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    sighash: [u8; 32],
    amount: u64,
    anchor_height: u64,
    cooperative: bool,
}
#[derive(Deserialize)]
struct Capture {
    payment: Prepared,
    close: Prepared,
}
#[tokio::test]
#[ignore = "Requires a real funded browser capture and its service database"]
async fn browser_funding_and_transactions_verify_against_the_actual_chain()
-> Result<(), Box<dyn std::error::Error>> {
    let id: [u8; 32] = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Wrong tab id length")?;
    let path = PathBuf::from(std::env::var("TIDVAR_PREPARED_TX_PATH")?);
    let capture: Capture = serde_json::from_slice(&std::fs::read(path)?)?;
    let data = PathBuf::from(std::env::var("TIDVAR_DATA_DIR")?);
    let store = Store::open(&data.join("payee.sqlite"))?;
    let tab = store
        .load(&id)?
        .ok_or("The accepted service tab was not found")?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let txid = std::env::var("TIDVAR_TXID")?;
    let raw = client.transaction(&txid).await?;
    let tip = client.latest().await?;
    assert!(raw.height > 0 && raw.height <= tip.height);
    let tx = zcash_primitives::transaction::Transaction::read(raw.data.as_slice(), BranchId::Nu7)?;
    assert_eq!(tx.txid().to_string(), txid);
    let view: [u8; 96] = tab.keys.public.viewing_key.as_slice().try_into()?;
    let fvk = orchard::keys::FullViewingKey::from_bytes(&view)
        .ok_or("The service viewing key was not valid")?;
    let incoming = fvk.to_ivk(Scope::External).prepare();
    let mut full = Vec::new();
    for action in tx
        .ironwood_bundle()
        .ok_or("The actual funding has no Ironwood outputs")?
        .actions()
        .iter()
    {
        if let Some((note, address, memo)) = zcash_note_encryption::try_note_decryption(
            &IronwoodDomain::for_action(action),
            &incoming,
            action,
        ) {
            assert_eq!(address, fvk.address_at(0u32, Scope::External));
            assert_eq!(memo, zcash_protocol::memo::MemoBytes::empty().into_bytes());
            full.push(note);
        }
    }
    let anchor = capture.payment.anchor_height;
    assert_eq!(anchor, capture.close.anchor_height);
    assert!(raw.height <= anchor && anchor <= tip.height);
    let before = u64::from(tab.record.body.terms.open_height)
        .checked_sub(1)
        .ok_or("Invalid opening height")?;
    let mut sync = SyncState::new(&view, client.tree(before).await?)?;
    while sync.height < anchor {
        let start = sync.height + 1;
        for block in client.blocks(start, (start + 127).min(anchor)).await? {
            sync.apply(block)?;
        }
    }
    sync.verify_tip(&client.tree(anchor).await?)?;
    let notes = sync.funding_notes()?;
    assert_eq!(full.len(), notes.len());
    for original in &full {
        let compact = notes
            .iter()
            .find(|n| n.note.nullifier(&fvk) == original.nullifier(&fvk))
            .ok_or("The actual compact note was not found")?;
        assert_eq!(compact.note.value(), original.value());
        assert_eq!(compact.note.recipient(), original.recipient());
        assert_eq!(compact.note.rho(), original.rho());
        assert_eq!(compact.note.rseed().as_bytes(), original.rseed().as_bytes());
    }
    assert_eq!(sync.value()?, tab.record.body.terms.funded.value());
    let started = Instant::now();
    let proofs = Proofs::build();
    println!(
        "TESTNET proving key built in{}ms",
        started.elapsed().as_millis()
    );
    for (name, prepared) in [("payment", capture.payment), ("cooperative", capture.close)] {
        let pczt = pczt::Pczt::parse(&prepared.pczt)
            .map_err(|_| "The captured PCZT could not be parsed")?;
        let kind = if prepared.cooperative {
            Kind::Cooperative
        } else {
            Kind::Payment
        };
        let checked = iou::check(
            pczt,
            &tab.record,
            &notes,
            Amount::try_from(prepared.amount)?,
            TransactionContext {
                target: u32::try_from(anchor)?
                    .checked_add(1)
                    .ok_or("Invalid target height")?,
                kind,
                anchor: sync.root(),
            },
            &mut OsRng,
        )?;
        let wrong = pczt::Pczt::parse(&prepared.pczt)
            .map_err(|_| "The captured PCZT could not be parsed")?;
        assert!(
            iou::check(
                wrong,
                &tab.record,
                &notes,
                Amount::try_from(prepared.amount + 1)?,
                TransactionContext {
                    target: u32::try_from(anchor)? + 1,
                    kind,
                    anchor: sync.root()
                },
                &mut OsRng
            )
            .is_err()
        );
        let mut wrong_root = sync.root();
        wrong_root[0] ^= 1;
        let wrong = pczt::Pczt::parse(&prepared.pczt)
            .map_err(|_| "The captured PCZT could not be parsed")?;
        assert!(
            iou::check(
                wrong,
                &tab.record,
                &notes,
                Amount::try_from(prepared.amount)?,
                TransactionContext {
                    target: u32::try_from(anchor)? + 1,
                    kind,
                    anchor: wrong_root
                },
                &mut OsRng
            )
            .is_err()
        );
        assert_eq!(checked.sighash, prepared.sighash);
        assert_eq!(checked.spends.len(), notes.len());
        let started = Instant::now();
        let _proven = proofs.prove(checked, &mut OsRng)?;
        println!(
            "TESTNET browser{}: full/compact notes and outputs agree; proof verified in{}ms; funding{} at{}",
            name,
            started.elapsed().as_millis(),
            txid,
            raw.height
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Requires a fresh real browser transaction capture"]
async fn issue_fresh_nonces_for_the_real_cooperative_close()
-> Result<(), Box<dyn std::error::Error>> {
    let id: [u8; 32] = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid tab id")?;
    let capture: Capture =
        serde_json::from_slice(&std::fs::read(std::env::var("TIDVAR_PREPARED_TX_PATH")?)?)?;
    assert!(capture.close.cooperative);
    assert_eq!(capture.close.amount, 0);
    let pczt =
        pczt::Pczt::parse(&capture.close.pczt).map_err(|_| "Invalid captured transaction")?;
    assert_eq!(
        pczt::roles::signer::Signer::new(pczt)
            .map_err(|_| "Invalid transaction effects")?
            .shielded_sighash(),
        capture.close.sighash
    );
    let mut store =
        Store::open(&PathBuf::from(std::env::var("TIDVAR_DATA_DIR")?).join("payee.sqlite"))?;
    let commitments = store.issue_nonces(&id, 1, Some(capture.close.sighash), &mut OsRng)?;
    let path = PathBuf::from(std::env::var("TIDVAR_NONCE_OUT")?);
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec(&commitments)?)?;
    file.sync_all()?;
    println!("TESTNET fresh service nonce persisted; only its public commitment exported.");
    Ok(())
}

#[derive(Deserialize)]
struct PayerSignatures {
    messages: Vec<tidvar_core::frost::Message>,
    shares: Vec<tidvar_core::frost::Share>,
}
#[tokio::test]
#[ignore = "Requires actual browser signatures and explicit testnet close execution"]
async fn complete_the_real_browser_cooperative_close() -> Result<(), Box<dyn std::error::Error>> {
    let id: [u8; 32] = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid tab id")?;
    let capture: Capture =
        serde_json::from_slice(&std::fs::read(std::env::var("TIDVAR_PREPARED_TX_PATH")?)?)?;
    let signatures: PayerSignatures =
        serde_json::from_slice(&std::fs::read(std::env::var("TIDVAR_SIGNATURES_PATH")?)?)?;
    let data = PathBuf::from(std::env::var("TIDVAR_DATA_DIR")?);
    let mut store = Store::open(&data.join("payee.sqlite"))?;
    let tab = store.load(&id)?.ok_or("Missing accepted tab")?;
    let prepared = capture.close;
    assert!(prepared.cooperative && prepared.amount == 0);
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let before = u64::from(tab.record.body.terms.open_height) - 1;
    let mut sync = SyncState::new(&tab.keys.public.viewing_key, client.tree(before).await?)?;
    while sync.height < prepared.anchor_height {
        let start = sync.height + 1;
        for block in client
            .blocks(start, (start + 127).min(prepared.anchor_height))
            .await?
        {
            sync.apply(block)?;
        }
    }
    sync.verify_tip(&client.tree(prepared.anchor_height).await?)?;
    let pczt = pczt::Pczt::parse(&prepared.pczt).map_err(|_| "Invalid captured transaction")?;
    let mut checked = iou::check(
        pczt,
        &tab.record,
        &sync.funding_notes()?,
        Amount::try_from(prepared.amount)?,
        TransactionContext {
            target: u32::try_from(prepared.anchor_height)? + 1,
            kind: Kind::Cooperative,
            anchor: sync.root(),
        },
        &mut OsRng,
    )?;
    assert_eq!(checked.sighash, prepared.sighash);
    assert_eq!(signatures.messages.len(), checked.spends.len());
    assert_eq!(signatures.shares.len(), checked.spends.len());
    let mut signer = pczt::roles::signer::Signer::new(checked.pczt.clone())
        .map_err(|_| "Invalid signing effects")?;
    for ((spend, message), payer) in checked
        .spends
        .iter()
        .zip(&signatures.messages)
        .zip(&signatures.shares)
    {
        assert_eq!(message.sighash, checked.sighash);
        assert_eq!(message.alpha, spend.alpha);
        let service = store.complete_nonce(&id, message, payer)?;
        drop(store);
        store = Store::open(&data.join("payee.sqlite"))?;
        let repeated = store.complete_nonce(&id, message, payer)?;
        assert!(service.value == repeated.value && service.role == repeated.role);
        let connection = rusqlite::Connection::open_with_flags(
            data.join("payee.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let erased: Option<Vec<u8>> = connection.query_row(
            "SELECT secret FROM signing_nonces WHERE id=?1",
            rusqlite::params![message.service.id.as_slice()],
            |row| row.get(0),
        )?;
        assert!(erased.is_none());
        let signature = tidvar_core::frost::aggregate(&tab.keys, message, payer, &service)?;
        signer
            .apply_ironwood_signature(
                spend.index,
                orchard::primitives::redpallas::Signature::from(signature),
            )
            .map_err(|_| "The SDK rejected the aggregate signature")?;
    }
    checked.pczt = signer.finish();
    let proofs = Proofs::build();
    let proven = proofs.prove(checked, &mut OsRng)?;
    let transaction = proofs.extract(proven, &mut OsRng)?;
    let txid = transaction.txid().to_string();
    let mut raw = Vec::new();
    transaction.write(&mut raw)?;
    let output = PathBuf::from(std::env::var("TIDVAR_CLOSE_RESULT")?);
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    file.write_all(&raw)?;
    file.sync_all()?;
    let sent = client.broadcast(raw).await?;
    assert_eq!(sent, txid);
    println!(
        "TESTNET cooperative close submitted{}; actual browser share accepted; service nonce erased; restart retry identical",
        txid
    );
    Ok(())
}

#[test]
#[ignore = "Requires the real accepted service tab and a new private data directory"]
fn prepare_stored_chain_payment_verification() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    let id: [u8; 32] = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid tab id")?;
    let source =
        Store::open(&PathBuf::from(std::env::var("TIDVAR_DATA_DIR")?).join("payee.sqlite"))?;
    let tab = source.load(&id)?.ok_or("Missing actual accepted tab")?;
    let directory = PathBuf::from(std::env::var("TIDVAR_HISTORY_DATA_DIR")?);
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut store = Store::open(&directory.join("payee.sqlite"))?;
    store.save(&tab)?;
    let commitments = store.issue_nonces(&id, 32, None, &mut OsRng)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("commitments.json"))?;
    file.write_all(&serde_json::to_vec(&commitments)?)?;
    file.sync_all()?;
    println!(
        "TESTNET isolated service store prepared; public commitments only exported; no payment sent."
    );
    Ok(())
}
#[tokio::test]
#[ignore = "Requires actual browser signatures over stored real chain inputs; never broadcasts"]
async fn stored_chain_payments_survive_recovery_and_replace_only_the_old_nonce()
-> Result<(), Box<dyn std::error::Error>> {
    use tidvar_core::iou::Request;
    let id: [u8; 32] = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid tab id")?;
    let directory = PathBuf::from(std::env::var("TIDVAR_HISTORY_DATA_DIR")?);
    let requests: Vec<Request> =
        serde_json::from_slice(&std::fs::read(directory.join("payments.json"))?)?;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].amount < requests[1].amount);
    assert_eq!(requests[0].anchor_height, requests[1].anchor_height);
    let anchor = requests[0].anchor_height;
    let mut store = Store::open(&directory.join("payee.sqlite"))?;
    let tab = store.load(&id)?.ok_or("Missing actual accepted tab")?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let mut sync = SyncState::new(
        &tab.keys.public.viewing_key,
        client
            .tree(u64::from(tab.record.body.terms.open_height) - 1)
            .await?,
    )?;
    while sync.height < anchor {
        let start = sync.height + 1;
        for block in client.blocks(start, (start + 127).min(anchor)).await? {
            sync.apply(block)?;
        }
    }
    sync.verify_tip(&client.tree(anchor).await?)?;
    assert_eq!(sync.value()?, tab.record.body.terms.funded.value());
    let proofs = Proofs::build();
    let target = u32::try_from(anchor)?
        .checked_add(1)
        .ok_or("Invalid target height")?;
    let context = || TransactionContext {
        target,
        kind: Kind::Payment,
        anchor: sync.root(),
    };
    let first = tidvar_payee::payments::verify(
        &tab,
        requests[0].clone(),
        &sync.funding_notes()?,
        context(),
        None,
        &proofs,
        &mut OsRng,
    )?;
    let now = || -> Result<u64, Box<dyn std::error::Error>> {
        Ok(u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        )?)
    };
    let receipt = store.accept_iou(first, now()?)?;
    assert_eq!(receipt.balance.paid, requests[0].amount);
    assert!(!receipt.balance.running);
    drop(store);
    let mut store = Store::open(&directory.join("payee.sqlite"))?;
    let repeated = tidvar_payee::payments::verify(
        &tab,
        requests[0].clone(),
        &sync.funding_notes()?,
        context(),
        store.payment_witness(&id)?,
        &proofs,
        &mut OsRng,
    )?;
    let retry = store.accept_iou(repeated, now()?)?;
    assert_eq!(serde_json::to_vec(&receipt)?, serde_json::to_vec(&retry)?);
    let witness = store.payment_witness(&id)?;
    let second = tidvar_payee::payments::verify(
        &tab,
        requests[1].clone(),
        &sync.funding_notes()?,
        context(),
        witness,
        &proofs,
        &mut OsRng,
    )?;
    let receipt = store.accept_iou(second, now()?)?;
    assert_eq!(receipt.balance.paid, requests[1].amount);
    let current = store.newest_iou(&id)?.ok_or("Missing latest payment")?;
    assert_eq!(current.hash(&id)?, requests[1].hash(&id)?);
    let connection = rusqlite::Connection::open_with_flags(
        directory.join("payee.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    for (index, request) in requests.iter().enumerate() {
        for message in &request.messages {
            let (state, secret, assigned): (String, bool, Option<Vec<u8>>) = connection.query_row(
                "SELECT state,secret IS NOT NULL,message FROM signing_nonces WHERE id=?1",
                rusqlite::params![message.service.id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if index == 0 {
                assert_eq!(state, "discarded");
                assert!(!secret);
            } else {
                assert_eq!(state, "issued");
                assert!(secret);
                assert_eq!(assigned.as_deref(), Some(message.hash()?.as_slice()));
            }
        }
    }
    let older = tidvar_payee::payments::verify(
        &tab,
        requests[0].clone(),
        &sync.funding_notes()?,
        context(),
        store.payment_witness(&id)?,
        &proofs,
        &mut OsRng,
    )?;
    assert!(store.accept_iou(older, now()?).is_err());
    let mut changed = requests[1].clone();
    changed.shares[0].value[0] ^= 1;
    assert!(
        tidvar_payee::payments::verify(
            &tab,
            changed,
            &sync.funding_notes()?,
            context(),
            store.payment_witness(&id)?,
            &proofs,
            &mut OsRng
        )
        .is_err()
    );
    assert_eq!(store.balance(&id, now()?)?.paid, requests[1].amount);
    println!(
        "TESTNET stored-chain payment verification PASS: actual browser shares; identical restart retry; newest nonce retained unsigned; old nonce cleared; lower payment and changed signature refused. No broadcast."
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires a real browser-authenticated payment over stored chain inputs; never broadcasts"]
async fn authenticated_browser_payment_has_a_durable_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let directory = PathBuf::from(std::env::var("TIDVAR_HISTORY_DATA_DIR")?);
    let request: tidvar_core::transport::Authenticated =
        serde_json::from_slice(&std::fs::read(directory.join("authenticated-iou.json"))?)?;
    let id = request.message.tab_id.0;
    let mut store = Store::open(&directory.join("payee.sqlite"))?;
    assert!(store.cached_reply(&request)?.is_none());
    let tab = store.load(&id)?.ok_or("Missing actual accepted tab")?;
    let payment: tidvar_core::iou::Request = serde_json::from_value(
        request
            .message
            .body
            .get("payment")
            .ok_or("Missing actual browser payment")?
            .clone(),
    )?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let mut state = SyncState::new(
        &tab.keys.public.viewing_key,
        client
            .tree(u64::from(tab.record.body.terms.open_height) - 1)
            .await?,
    )?;
    while state.height < payment.anchor_height {
        let start = state.height + 1;
        for block in client
            .blocks(start, (start + 127).min(payment.anchor_height))
            .await?
        {
            state.apply(block)?;
        }
    }
    state.verify_tip(&client.tree(state.height).await?)?;
    let target = u32::try_from(state.height)?
        .checked_add(1)
        .ok_or("Invalid target height")?;
    let proofs = Proofs::build();
    let verified = tidvar_payee::payments::verify(
        &tab,
        payment,
        &state.funding_notes()?,
        TransactionContext {
            target,
            kind: Kind::Payment,
            anchor: state.root(),
        },
        store.payment_witness(&id)?,
        &proofs,
        &mut OsRng,
    )?;
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    let receipt = store.accept_iou(verified, now)?;
    let response = store.save_reply(
        &request,
        serde_json::json!({"step":"accepted","receipt":receipt}),
    )?;
    drop(store);
    let store = Store::open(&directory.join("payee.sqlite"))?;
    assert_eq!(
        store
            .cached_reply(&request)?
            .ok_or("Missing actual receipt")?,
        response
    );
    assert!(store.payment_receipt(&id, &receipt.request)?.is_some());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("authenticated-reply.json"))?;
    file.write_all(&response)?;
    file.sync_all()?;
    println!(
        "TESTNET authenticated historical payment PASS: actual browser root/signature shares; paid and nonce saved; hash-chain receipt identical after restart. No broadcast."
    );
    Ok(())
}
