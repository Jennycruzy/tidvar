use crate::{
    engine::{self},
    root::Root,
};
use rand_core::OsRng;
use serde::{Serialize, de::DeserializeOwned};
use tidvar_core::encoding::MAX_MESSAGE_BYTES;
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

enum State {
    Fresh,
    First(Box<engine::Setup>),
    Second(Box<engine::SetupSecond>),
    Unsealed(Box<engine::UnsealedPayer>),
    Pending(Box<engine::PendingPayer>),
    Ready(Box<engine::Payer>),
    Failed,
}
#[wasm_bindgen]
pub struct BrowserPayer {
    root: Root,
    state: State,
}
fn error(value: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&value.to_string())
}
fn parse<T: DeserializeOwned>(value: &str) -> Result<T, JsValue> {
    if value.len() > MAX_MESSAGE_BYTES {
        return Err(error("The message is too large."));
    }
    serde_json::from_str(value).map_err(|_| error("The message could not be read."))
}
fn json<T: Serialize>(value: &T) -> Result<String, JsValue> {
    serde_json::to_string(value).map_err(|_| error("The message could not be saved."))
}
fn wrong_state() -> JsValue {
    error("This step is not available. Recover your saved tab to continue.")
}

#[wasm_bindgen]
impl BrowserPayer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            root: Root::generate(&mut OsRng),
            state: State::Fresh,
        }
    }
    pub fn phrase(&self) -> Result<String, JsValue> {
        Ok(self.root.phrase().map_err(error)?.to_string())
    }
    pub fn backup_key(&self) -> Result<Vec<u8>, JsValue> {
        Ok(self.root.encryption_key().map_err(error)?.to_vec())
    }
    pub fn begin(&mut self) -> Result<String, JsValue> {
        if !matches!(self.state, State::Fresh) {
            return Err(wrong_state());
        }
        let (setup, message) = engine::Setup::begin(&mut OsRng).map_err(error)?;
        self.state = State::First(Box::new(setup));
        json(&message)
    }
    pub fn advance(&mut self, peer: &str) -> Result<String, JsValue> {
        let peer = parse(peer)?;
        if !matches!(self.state, State::First(_)) {
            return Err(wrong_state());
        }
        let State::First(first) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(wrong_state());
        };
        let (second, message) = first.advance(peer).map_err(error)?;
        self.state = State::Second(Box::new(second));
        json(&message)
    }
    pub fn finish_keys(&mut self, peer: &str, network: &str) -> Result<String, JsValue> {
        let network = match network {
            "Mainnet" => tidvar_core::record::Network::Mainnet,
            "Testnet" => tidvar_core::record::Network::Testnet,
            _ => return Err(JsValue::from_str("This service is on an unknown network.")),
        };
        let peer = parse(peer)?;
        if !matches!(self.state, State::Second(_)) {
            return Err(wrong_state());
        }
        let State::Second(second) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(wrong_state());
        };
        let unsealed = second.finish(peer, network).map_err(error)?;
        let public = json(unsealed.public_keys())?;
        self.state = State::Unsealed(Box::new(unsealed));
        Ok(public)
    }
    pub fn accept_seal(
        &mut self,
        sealed: &str,
        commitment: &[u8],
        expected_round: u64,
    ) -> Result<String, JsValue> {
        let sealed = parse(sealed)?;
        let commitment = commitment
            .try_into()
            .map_err(|_| error("The sealed key hash has the wrong length."))?;
        if !matches!(self.state, State::Unsealed(_)) {
            return Err(wrong_state());
        }
        let State::Unsealed(unsealed) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(wrong_state());
        };
        let (pending, challenge) = unsealed
            .accept(sealed, commitment, expected_round, &mut OsRng)
            .map_err(error)?;
        self.state = State::Pending(Box::new(pending));
        json(&challenge)
    }
    pub fn verify_seal(&mut self, openings: &str) -> Result<String, JsValue> {
        let openings = parse(openings)?;
        if !matches!(self.state, State::Pending(_)) {
            return Err(wrong_state());
        }
        let State::Pending(pending) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(wrong_state());
        };
        let payer = pending.finish(openings).map_err(error)?;
        let public = json(payer.public_keys())?;
        self.state = State::Ready(Box::new(payer));
        Ok(public)
    }
    pub fn record_body(&self, offer: &str, terms: &str) -> Result<String, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .record_body(&self.root, parse(offer)?, parse(terms)?)
                .map_err(error)?,
        )
    }
    pub fn approve_record(
        &mut self,
        offer: &str,
        terms: &str,
        partial: &str,
    ) -> Result<String, JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .approve_record(
                    &self.root,
                    parse(offer)?,
                    parse(terms)?,
                    parse(partial)?,
                    &mut OsRng,
                )
                .map_err(error)?,
        )
    }
    pub fn signed_record(&self) -> Result<String, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        json(payer.record().ok_or_else(wrong_state)?)
    }
    pub fn funding_request(&self) -> Result<String, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        json(&payer.funding_request().map_err(error)?)
    }
    pub fn backup_plaintext(&self) -> Result<Vec<u8>, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        Ok(payer.backup(&self.root).map_err(error)?.to_vec())
    }
    pub fn restore(phrase: &str, plaintext: &[u8]) -> Result<BrowserPayer, JsValue> {
        let phrase = Zeroizing::new(phrase.to_owned());
        let root = Root::from_phrase(&phrase).map_err(error)?;
        let payer = engine::Payer::restore(&root, plaintext).map_err(error)?;
        Ok(Self {
            root,
            state: State::Ready(Box::new(payer)),
        })
    }
    pub fn start_sync(&mut self, reply: &[u8]) -> Result<(), JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        let tree = tidvar_chain::transport::unary(reply).map_err(error)?;
        payer.start_sync(tree).map_err(error)
    }
    pub fn apply_blocks(&mut self, reply: &[u8]) -> Result<(), JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        payer
            .sync_mut()
            .ok_or_else(wrong_state)?
            .apply_web_range(reply)
            .map_err(error)
    }
    pub fn verify_tip(&self, reply: &[u8]) -> Result<u64, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        let tree = tidvar_chain::transport::unary(reply).map_err(error)?;
        let sync = payer.sync().ok_or_else(wrong_state)?;
        sync.verify_tip(&tree).map_err(error)?;
        sync.value().map_err(error)
    }
}
impl Default for BrowserPayer {
    fn default() -> Self {
        Self::new()
    }
}
#[wasm_bindgen]
pub fn tree_request(height: u64) -> Result<Vec<u8>, JsValue> {
    tidvar_chain::transport::tree_request(height).map_err(error)
}
#[wasm_bindgen]
pub fn range_request(start: u64, end: u64) -> Result<Vec<u8>, JsValue> {
    tidvar_chain::transport::range_request(start, end).map_err(error)
}

#[wasm_bindgen]
pub fn phrase_from_entropy(entropy: &[u8]) -> Result<String, JsValue> {
    let entropy: [u8; 32] = entropy
        .try_into()
        .map_err(|_| JsValue::from_str("This judge code is not complete."))?;
    Ok(Root::from_entropy(entropy)
        .phrase()
        .map_err(error)?
        .to_string())
}

#[wasm_bindgen]
pub fn key_from_phrase(phrase: &str) -> Result<Vec<u8>, JsValue> {
    let root = Root::from_phrase(phrase).map_err(error)?;
    Ok(root.encryption_key().map_err(error)?.to_vec())
}

#[wasm_bindgen]
pub fn message_hash(message: &str) -> Result<String, JsValue> {
    let value: tidvar_core::transport::Message<serde_json::Value> = parse(message)?;
    json(&value.hash().map_err(error)?)
}

#[wasm_bindgen]
pub fn new_open_id() -> Result<String, JsValue> {
    use rand_core::RngCore;
    let mut id = [0; 32];
    OsRng.fill_bytes(&mut id);
    json(&tidvar_core::transport::Id(id))
}
#[wasm_bindgen]
pub fn round_for_terms(terms: &str) -> Result<u64, JsValue> {
    let terms: tidvar_core::record::Terms = parse(terms)?;
    let times = tidvar_core::params::Deadlines::new(terms.open_ms, terms.lifetime.seconds() * 1000)
        .map_err(error)?;
    tidvar_seal::round_at(times.refund_ms / 1000).map_err(error)
}
#[wasm_bindgen]
pub fn latest_request() -> Result<Vec<u8>, JsValue> {
    tidvar_chain::transport::request(&tidvar_chain::service::ChainSpec {}).map_err(error)
}
#[wasm_bindgen]
pub fn transaction_request(txid: &str) -> Result<Vec<u8>, JsValue> {
    let mut hash =
        hex::decode(txid).map_err(|_| JsValue::from_str("This is not a transaction id."))?;
    hash.reverse();
    tidvar_chain::transport::request(&tidvar_chain::service::TxFilter {
        block: None,
        index: 0,
        hash,
    })
    .map_err(error)
}
/// Parses the server's bytes and recomputes the transaction id from them.
#[wasm_bindgen]
pub fn transaction_check(reply: &[u8]) -> Result<String, JsValue> {
    let raw: tidvar_chain::service::RawTransaction =
        tidvar_chain::transport::unary(reply).map_err(error)?;
    let parsed = zcash_primitives::transaction::Transaction::read(
        raw.data.as_slice(),
        zcash_protocol::consensus::BranchId::Nu7,
    )
    .map_err(|_| JsValue::from_str("The server returned bytes that are not a transaction."))?;
    json(
        &serde_json::json!({"txid": parsed.txid().to_string(), "height": raw.height, "bytes": raw.data.len()}),
    )
}
#[wasm_bindgen]
pub fn info_request() -> Result<Vec<u8>, JsValue> {
    tidvar_chain::transport::request(&tidvar_chain::service::Empty {}).map_err(error)
}
/// Lets this browser switch to NU7 transactions once the chain reports it.
#[wasm_bindgen]
pub fn observe_info(reply: &[u8]) -> Result<String, JsValue> {
    let info: tidvar_chain::service::LightdInfo =
        tidvar_chain::transport::unary(reply).map_err(error)?;
    let network = match info.chain_name.as_str() {
        "main" => tidvar_core::record::Network::Mainnet,
        "test" => tidvar_core::record::Network::Testnet,
        _ => {
            return Err(JsValue::from_str(
                "The Zcash server is on an unknown network.",
            ));
        }
    };
    tidvar_core::record::observe_branch(network, &info.consensus_branch_id, info.block_height);
    Ok(info.consensus_branch_id)
}
#[wasm_bindgen]
pub fn latest_height(reply: &[u8]) -> Result<u64, JsValue> {
    let value: tidvar_chain::service::BlockId =
        tidvar_chain::transport::unary(reply).map_err(error)?;
    Ok(value.height)
}
#[wasm_bindgen]
impl BrowserPayer {
    pub fn synced_height(&self) -> Result<u64, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        Ok(payer.sync().ok_or_else(wrong_state)?.height)
    }
}

use tidvar_core::iou::Prepared;

#[wasm_bindgen]
impl BrowserPayer {
    pub fn prepare_transaction(&self, amount: u64, cooperative: bool) -> Result<String, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        let checked = payer.transaction(amount, cooperative).map_err(error)?;
        let height = payer.sync().ok_or_else(wrong_state)?.height;
        json(&Prepared {
            pczt: checked
                .pczt
                .serialize()
                .map_err(|_| error("The transaction could not be saved."))?,
            sighash: checked.sighash,
            amount,
            anchor_height: height,
            cooperative,
        })
    }
}

#[wasm_bindgen]
impl BrowserPayer {
    pub fn sign_transaction(
        &mut self,
        prepared: &str,
        services: &str,
        tree: &[u8],
    ) -> Result<String, JsValue> {
        let prepared: Prepared = parse(prepared)?;
        let services = parse(services)?;
        let tree = tidvar_chain::transport::unary(tree).map_err(error)?;
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .sign_transaction(
                    &prepared.pczt,
                    prepared.amount,
                    prepared.anchor_height,
                    prepared.cooperative,
                    tree,
                    services,
                )
                .map_err(error)?,
        )
    }
}

#[wasm_bindgen]
impl BrowserPayer {
    /// The signed refund (a PCZT without proofs), once drand has published the refund round.
    pub fn refund_transaction(&self, info: &str, beacon: &str) -> Result<Vec<u8>, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        let beacon = tidvar_seal::verify_beacon_json(info, beacon).map_err(error)?;
        payer.refund_transaction(&beacon).map_err(error)
    }
    pub fn prepare_request(&mut self, body: &str) -> Result<String, JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .prepare_request(&self.root, parse(body)?)
                .map_err(error)?,
        )
    }
    pub fn pending_request(&self) -> Result<String, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        json(&payer.pending_request())
    }
    pub fn accept_reply(&mut self, reply: &str, now_ms: u64) -> Result<String, JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .accept_reply_at(parse(reply)?, now_ms)
                .map_err(error)?,
        )
    }
    pub fn signed_amount(&self) -> Result<u64, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        Ok(payer.signed_amount())
    }
}

#[wasm_bindgen]
impl BrowserPayer {
    pub fn prepare_iou_request(
        &mut self,
        prepared: &str,
        services: &str,
        tree: &[u8],
    ) -> Result<String, JsValue> {
        let prepared: tidvar_core::iou::Prepared = parse(prepared)?;
        let services = parse(services)?;
        let tree = tidvar_chain::transport::unary(tree).map_err(error)?;
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .prepare_iou_request(&self.root, &prepared, services, tree)
                .map_err(error)?,
        )
    }
    pub fn prepare_close_request(
        &mut self,
        prepared: &str,
        services: &str,
        tree: &[u8],
    ) -> Result<String, JsValue> {
        let prepared: tidvar_core::iou::Prepared = parse(prepared)?;
        let services = parse(services)?;
        let tree = tidvar_chain::transport::unary(tree).map_err(error)?;
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(
            &payer
                .prepare_close_request(&self.root, &prepared, services, tree)
                .map_err(error)?,
        )
    }
    pub fn commitment_unused(&self, commitment: &str) -> Result<bool, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        payer.commitment_unused(&parse(commitment)?).map_err(error)
    }
    pub fn funding_note_count(&self) -> Result<u32, JsValue> {
        let State::Ready(payer) = &self.state else {
            return Err(wrong_state());
        };
        let notes = payer
            .sync()
            .ok_or_else(wrong_state)?
            .funding_notes()
            .map_err(error)?;
        u32::try_from(notes.len()).map_err(error)
    }
    pub fn payment_target(
        &mut self,
        now_ms: u64,
        hidden: bool,
        service_used: u64,
    ) -> Result<u64, JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        payer
            .payment_target(now_ms, hidden, service_used)
            .map_err(error)
    }
    pub fn authorization_balance(&mut self, now_ms: u64) -> Result<String, JsValue> {
        let State::Ready(payer) = &mut self.state else {
            return Err(wrong_state());
        };
        json(&payer.authorization_balance(now_ms).map_err(error)?)
    }
}
