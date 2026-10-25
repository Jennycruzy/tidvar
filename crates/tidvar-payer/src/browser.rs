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
    pub fn finish_keys(&mut self, peer: &str) -> Result<String, JsValue> {
        let peer = parse(peer)?;
        if !matches!(self.state, State::Second(_)) {
            return Err(wrong_state());
        }
        let State::Second(second) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(wrong_state());
        };
        let unsealed = second.finish(peer).map_err(error)?;
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
pub fn key_from_phrase(phrase: &str) -> Result<Vec<u8>, JsValue> {
    let root = Root::from_phrase(phrase).map_err(error)?;
    Ok(root.encryption_key().map_err(error)?.to_vec())
}
