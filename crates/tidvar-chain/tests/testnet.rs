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
