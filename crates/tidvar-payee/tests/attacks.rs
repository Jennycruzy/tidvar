#![cfg(not(target_arch = "wasm32"))]
use rand_core::OsRng;
use std::{
    error::Error,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tidvar_chain::{
    native::{Client, TESTNET_SERVER},
    sync::SyncState,
};
use tidvar_core::iou::{Kind, TransactionContext};
use tidvar_payee::{proof::Proofs, store::Store};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[tokio::test]
#[ignore = "Requires a genuine funded testnet tab in the isolated service; never broadcasts"]
async fn at3_at4_at15_finish_the_current_signed_iou() -> Result<()> {
    let id: [u8; 32] = hex::decode(std::env::var("AT_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid identifier")?;
    let directory = PathBuf::from(std::env::var("AT_OUTPUT_DIR")?);
    std::fs::create_dir_all(&directory)?;
    let mut store = Store::open(&PathBuf::from("/root/tidvar-data-at/payee.sqlite"))?;
    let tab = store.load(&id)?.ok_or("Missing isolated tab")?;
    assert_eq!(
        tab.record.body.offer.network,
        tidvar_core::record::Network::Testnet
    );
    let request = store
        .newest_iou(&id)?
        .ok_or("Missing actual accepted IOU")?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let before = u64::from(tab.record.body.terms.open_height) - 1;
    let mut sync = SyncState::new(&tab.keys.public.viewing_key, client.tree(before).await?)?;
    while sync.height < request.anchor_height {
        let start = sync.height + 1;
        for block in client
            .blocks(start, (start + 127).min(request.anchor_height))
            .await?
        {
            sync.apply(block)?;
        }
    }
    sync.verify_tip(&client.tree(request.anchor_height).await?)?;
    let target = u32::try_from(client.latest().await?.height)? + 1;
    let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let (checked, signed) = store.sign_forced(
        &id,
        &sync.funding_notes()?,
        TransactionContext {
            target,
            kind: Kind::Payment,
            anchor: sync.root(),
        },
        now,
        false,
        &mut OsRng,
    )?;
    let proofs = Proofs::build();
    let transaction = proofs.extract(proofs.prove(checked, &mut OsRng)?, &mut OsRng)?;
    let (txid, raw, watch_ms, expiry) =
        store.save_pack(&id, &transaction, signed.amount.value())?;
    let notes = sync.funding_notes()?;
    let value = tidvar_core::iou::notes_value(&notes)?.value();
    let fee = tidvar_core::params::close_fee(u8::try_from(notes.len())?)?.value();
    let result = serde_json::json!({"network":"TESTNET","tab":hex::encode(id),"txid":txid,"amount":signed.amount.value(),"returned":value-signed.amount.value()-fee,"fee":fee,"expiry":expiry,"broadcast_after_ms":watch_ms});
    std::fs::write(directory.join("transaction.raw"), &raw)?;
    std::fs::write(
        directory.join("pack.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"network":"Testnet","txid":txid,"raw":hex::encode(&raw),"broadcast_after_ms":watch_ms,"expiry":expiry}),
        )?,
    )?;
    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}

#[tokio::test]
#[ignore = "Requires the mined transaction of a real isolated testnet tab; read-only"]
async fn at3_at15_mined_outputs_equal_the_signed_amounts() -> Result<()> {
    use orchard::{keys::Scope, note_encryption::IronwoodDomain};
    let id: [u8; 32] = hex::decode(std::env::var("AT_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid identifier")?;
    let directory = PathBuf::from(std::env::var("AT_OUTPUT_DIR")?);
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("result.json"))?)?;
    let store = Store::open(&PathBuf::from("/root/tidvar-data-at/payee.sqlite"))?;
    let tab = store.load(&id)?.ok_or("Missing isolated tab")?;
    let view: [u8; 96] = tab.keys.public.viewing_key.as_slice().try_into()?;
    let fvk = orchard::keys::FullViewingKey::from_bytes(&view).ok_or("Invalid viewing key")?;
    let txid = expected["txid"].as_str().ok_or("Missing txid")?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let found = client.transaction(txid).await?;
    assert!(found.height > 0);
    assert_eq!(
        found.data,
        std::fs::read(directory.join("transaction.raw"))?
    );
    let tx = zcash_primitives::transaction::Transaction::read(
        found.data.as_slice(),
        zcash_protocol::consensus::BranchId::Nu7,
    )?;
    let mut values = Vec::new();
    let mut recipients = Vec::new();
    for action in tx
        .ironwood_bundle()
        .ok_or("Missing Ironwood bundle")?
        .actions()
        .iter()
    {
        if let Some((note, recipient, _)) = zcash_note_encryption::try_output_recovery_with_ovk(
            &IronwoodDomain::for_action(action),
            &fvk.to_ovk(Scope::External),
            action,
            action.cv_net(),
            &action.encrypted_note().out_ciphertext,
        ) && note.value().inner() > 0
        {
            values.push(note.value().inner());
            recipients.push((recipient, note.value().inner()));
        }
    }
    values.sort_unstable();
    let mut wanted = vec![
        expected["amount"]
            .as_u64()
            .ok_or("Missing service amount")?,
        expected["returned"]
            .as_u64()
            .ok_or("Missing payer amount")?,
    ];
    wanted.retain(|v| *v > 0);
    wanted.sort_unstable();
    assert_eq!(values, wanted);
    let network = tab.record.body.offer.network;
    let service = tidvar_core::record::receiver(&tab.record.body.offer.address, network)?;
    let payer = tidvar_core::record::receiver(&tab.record.body.terms.return_address, network)?;
    assert!(
        recipients.contains(&(
            service,
            expected["amount"]
                .as_u64()
                .ok_or("Missing service amount")?
        ))
    );
    assert!(
        recipients.contains(&(
            payer,
            expected["returned"]
                .as_u64()
                .ok_or("Missing payer amount")?
        ))
    );
    let result = serde_json::json!({"network":"TESTNET","txid":txid,"height":found.height,"exact_bytes_match":true,"recipients_match_signed_record":true,"recovered_output_values":values});
    std::fs::write(
        directory.join("mined-outputs.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
