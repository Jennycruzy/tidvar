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
