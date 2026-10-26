#![cfg(not(target_arch = "wasm32"))]
use prost::Message;
use rand_core::OsRng;
use std::collections::BTreeSet;
use tidvar_chain::{
    native::{Client, TESTNET_SERVER},
    service,
    sync::SyncState,
    transport,
};
use tidvar_core::keys::{FirstRound, Role};
use zcash_protocol::consensus::NetworkType;

#[tokio::test]
#[ignore = "Requires the public Zcash testnet server"]
async fn actual_tip_compact_block_and_tree_are_consistent() -> Result<(), Box<dyn std::error::Error>>
{
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let tip = client.latest().await?;
    let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
    let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (p, _) = p.advance(s1)?;
    let (_, s2) = s.advance(p1)?;
    let payer = p.finish(s2, NetworkType::Test)?;
    let before = client.tree(tip.height - 1).await?;
    let mut state = SyncState::new(&payer.public.viewing_key, before)?;
    let original = serde_json::to_vec(&state.snapshot()?)?;
    let blocks = client.blocks(tip.height, tip.height).await?;
    let block = blocks.first().ok_or("No testnet block returned")?;
    let mut invalid = block.clone();
    invalid.prev_hash.reverse();
    invalid.prev_hash.push(0);
    assert!(state.apply(invalid).is_err());
    assert_eq!(original, serde_json::to_vec(&state.snapshot()?)?);
    let mut partial = transport::request(block)?;
    partial.extend(transport::request(block)?);
    assert!(
        state
            .apply_web_range(&transport::successful(partial))
            .is_err()
    );
    assert_eq!(original, serde_json::to_vec(&state.snapshot()?)?);
    let frames = transport::decode_frames(&transport::range_request(tip.height, tip.height)?);
    assert!(frames.is_err());
    let request = service::BlockRange {
        start: Some(service::BlockId {
            height: tip.height,
            hash: Vec::new(),
        }),
        end: Some(service::BlockId {
            height: tip.height,
            hash: Vec::new(),
        }),
        pool_types: Vec::new(),
    };
    let reply = client
        .relay("GetBlockRange", &request.encode_to_vec(), &BTreeSet::new())
        .await?;
    state.apply_web_range(&reply)?;
    assert!(transport::unary::<service::BlockId>(&reply[..reply.len() - 1]).is_err());
    assert!(
        client
            .relay("GetMempoolStream", &[], &BTreeSet::new())
            .await
            .is_err()
    );
    state.verify_tip(&client.tree(tip.height).await?)?;
    let restored = SyncState::restore(state.snapshot()?)?;
    assert_eq!(restored.root(), state.root());
    assert_eq!(state.value()?, 0);
    println!(
        "TESTNET height{}: actual compact block, root and persisted frontier agree",
        tip.height
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires a supplied real testnet transaction"]
async fn supplied_transaction_has_the_expected_hash_and_mining_height()
-> Result<(), Box<dyn std::error::Error>> {
    let txid = std::env::var("TIDVAR_TXID")?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let tip = client.latest().await?;
    println!("TESTNET live tip{}", tip.height);
    let raw = client.transaction(&txid).await?;
    let tx = zcash_primitives::transaction::Transaction::read(
        raw.data.as_slice(),
        zcash_protocol::consensus::BranchId::Nu7,
    )?;
    assert_eq!(tx.txid().to_string(), txid);
    let tip = client.latest().await?;
    println!(
        "TESTNET transaction{} height{} tip{} bytes{}",
        txid,
        raw.height,
        tip.height,
        raw.data.len()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires a supplied real transaction file"]
async fn supplied_expired_transaction_cannot_be_mined_again()
-> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("TIDVAR_RAW_TX")?;
    let raw = std::fs::read(path)?;
    let tx = zcash_primitives::transaction::Transaction::read(
        raw.as_slice(),
        zcash_protocol::consensus::BranchId::Nu7,
    )?;
    let expiry = u32::from(tx.expiry_height());
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let tip = client.latest().await?;
    assert!(expiry > 0 && u64::from(expiry) <= tip.height);
    assert!(matches!(
        tidvar_chain::native::validate_broadcast(&raw, tip.height),
        Err(tidvar_chain::ChainError::Expired)
    ));
    println!(
        "TESTNET original{} expired at{}; current tip{}",
        tx.txid(),
        expiry,
        tip.height
    );
    Ok(())
}
