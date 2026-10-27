use crate::root::{Root, RootError};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidvar_chain::sync::{Snapshot, SyncState};
use tidvar_core::record::Network;
use tidvar_core::{
    encoding,
    keys::{FirstRound, KeyError, Keys, Role, RoundOne, RoundTwo, SecondRound},
};
use tidvar_seal::{
    SealError, quicknet_public_key,
    seal::{Challenge, Opening, SealedKey, VerifiedSeal, Verifier},
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Debug, Error)]
pub enum PayerError {
    #[error("A request is waiting for its receipt. Retry it before starting another step.")]
    Pending,
    #[error("The saved tab could not be verified. Keep your backup and try again.")]
    Backup,
    #[error(transparent)]
    Transaction(#[from] tidvar_core::iou::IouError),
    #[error("This payment commitment has already been used.")]
    ReusedCommitment,
    #[error(transparent)]
    Record(#[from] tidvar_core::record::RecordError),
    #[error(transparent)]
    Keys(#[from] KeyError),
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error(transparent)]
    Root(#[from] RootError),
    #[error(transparent)]
    Chain(#[from] tidvar_chain::ChainError),
}

#[derive(Serialize)]
pub struct FundingRequest {
    pub address: String,
    pub amount_zat: u64,
    pub amount: String,
    pub uri: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SignedTransaction {
    #[serde(with = "tidvar_core::encoding::fixed32")]
    request: [u8; 32],
    pub messages: Vec<tidvar_core::frost::Message>,
    pub shares: Vec<tidvar_core::frost::Share>,
}

pub struct Setup {
    inner: FirstRound,
}
pub struct SetupSecond {
    inner: SecondRound,
}
pub struct UnsealedPayer {
    keys: Keys,
}
pub struct PendingPayer {
    keys: Keys,
    verifier: Verifier,
    challenge: Challenge,
}
pub struct Payer {
    keys: Keys,
    seal: VerifiedSeal,
    sync: Option<SyncState>,
    consumed: Vec<[u8; 32]>,
    record: Option<tidvar_core::record::SignedRecord>,
    pending: Option<SignedTransaction>,
    paid: u64,
    channel: Channel,
    authorizer: tidvar_core::ledger::Ledger,
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SecretBytes(#[serde(with = "tidvar_core::encoding::bytes")] Vec<u8>);
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SavedOpenings(Vec<Opening>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backup {
    version: u32,
    package: SecretBytes,
    #[serde(with = "tidvar_core::encoding::bytes")]
    public_package: Vec<u8>,
    #[serde(with = "tidvar_core::encoding::bytes")]
    viewing_key: Vec<u8>,
    sealed: SealedKey,
    challenge: Challenge,
    openings: SavedOpenings,
    sync: Option<Snapshot>,
    consumed: Vec<[u8; 32]>,
    record: Option<tidvar_core::record::SignedRecord>,
    pending: Option<SignedTransaction>,
    #[serde(default)]
    paid: u64,
    #[serde(default)]
    channel: Channel,
    #[serde(default)]
    authorizer: tidvar_core::ledger::Ledger,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    owner: [u8; 32],
}

impl Setup {
    pub fn begin<R: RngCore + CryptoRng>(rng: &mut R) -> Result<(Self, RoundOne), PayerError> {
        let (inner, message) = FirstRound::begin(Role::Payer, rng)?;
        Ok((Self { inner }, message))
    }
    pub fn advance(self, peer: RoundOne) -> Result<(SetupSecond, RoundTwo), PayerError> {
        let (inner, message) = self.inner.advance(peer)?;
        Ok((SetupSecond { inner }, message))
    }
}
impl SetupSecond {
    pub fn finish(self, peer: RoundTwo, network: Network) -> Result<UnsealedPayer, PayerError> {
        Ok(UnsealedPayer {
            keys: self.inner.finish(peer, network.kind())?,
        })
    }
}
impl UnsealedPayer {
    pub fn public_keys(&self) -> &tidvar_core::keys::PublicKeys {
        &self.keys.public
    }
    pub fn accept<R: RngCore + CryptoRng>(
        self,
        sealed: SealedKey,
        commitment: [u8; 32],
        round: u64,
        rng: &mut R,
    ) -> Result<(PendingPayer, Challenge), PayerError> {
        let verifier = Verifier::accept(
            sealed,
            commitment,
            &self.keys.public.service,
            round,
            &quicknet_public_key()?,
        )?;
        let challenge = verifier.challenge(rng);
        Ok((
            PendingPayer {
                keys: self.keys,
                verifier,
                challenge: challenge.clone(),
            },
            challenge,
        ))
    }
}
impl PendingPayer {
    pub fn finish(self, openings: Vec<Opening>) -> Result<Payer, PayerError> {
        let seal = self.verifier.finish(self.challenge, openings)?;
        Ok(Payer {
            keys: self.keys,
            seal,
            sync: None,
            consumed: Vec::new(),
            record: None,
            pending: None,
            paid: 0,
            channel: Channel::default(),
            authorizer: tidvar_core::ledger::Ledger::default(),
        })
    }
}
impl Payer {
    pub fn sign_transaction(
        &mut self,
        raw: &[u8],
        amount: u64,
        anchor_height: u64,
        cooperative: bool,
        tree: tidvar_chain::service::TreeState,
        services: Vec<tidvar_core::frost::Commitment>,
    ) -> Result<SignedTransaction, PayerError> {
        use tidvar_core::{frost, iou};
        if tree.height != anchor_height {
            return Err(PayerError::Backup);
        }
        let record = self.record.as_ref().ok_or(PayerError::Backup)?;
        let sync = self.sync.as_ref().ok_or(PayerError::Backup)?;
        if anchor_height > sync.height {
            return Err(PayerError::Backup);
        }
        let anchor =
            tidvar_chain::sync::SyncState::new(&self.keys.public.viewing_key, tree)?.root();
        let pczt = pczt::Pczt::parse(raw).map_err(|_| PayerError::Backup)?;
        let checked = iou::check(
            pczt,
            record,
            &sync.funding_notes()?,
            tidvar_core::params::Amount::try_from(amount).map_err(|_| PayerError::Backup)?,
            iou::TransactionContext {
                target: u32::try_from(anchor_height)
                    .map_err(|_| PayerError::Backup)?
                    .checked_add(1)
                    .ok_or(PayerError::Backup)?,
                kind: if cooperative {
                    iou::Kind::Cooperative
                } else {
                    iou::Kind::Payment
                },
                anchor,
            },
            &mut rand_core::OsRng,
        )?;
        if services.len() != checked.spends.len() {
            return Err(PayerError::Backup);
        }
        for service in &services {
            service.verify().map_err(|_| PayerError::Backup)?;
        }
        let encoded = serde_json::to_vec(&services).map_err(|_| PayerError::Backup)?;
        let request = encoding::hash(
            b"TidvarPayReq",
            &encoding::encode_bytes(&[raw, &encoded]).map_err(|_| PayerError::Backup)?,
        )
        .map_err(|_| PayerError::Backup)?;
        if let Some(pending) = &self.pending
            && pending.request == request
        {
            return Ok(pending.clone());
        }
        if cooperative {
            if amount > self.paid {
                return Err(PayerError::Backup);
            }
        } else {
            tidvar_core::params::check_payment(
                record.body.terms.funded,
                tidvar_core::params::Amount::try_from(self.paid).map_err(|_| PayerError::Backup)?,
                tidvar_core::params::Amount::try_from(amount).map_err(|_| PayerError::Backup)?,
            )
            .map_err(|_| PayerError::Backup)?;
        }
        let mut messages = Vec::new();
        let mut shares = Vec::new();
        for (spend, service) in checked.spends.iter().zip(services) {
            self.consume_commitment(&service.data)?;
            let nonce = frost::Nonce::generate(&self.keys, &mut rand_core::OsRng)
                .map_err(|_| PayerError::Backup)?;
            let message = frost::Message {
                sighash: checked.sighash,
                alpha: spend.alpha,
                payer: nonce.public().clone(),
                service,
            };
            let share = nonce
                .sign(&self.keys, &message)
                .map_err(|_| PayerError::Backup)?;
            messages.push(message);
            shares.push(share);
        }
        let signed = SignedTransaction {
            request,
            messages,
            shares,
        };
        if !cooperative {
            self.paid = amount;
        }
        self.pending = Some(signed.clone());
        Ok(signed)
    }
    pub fn transaction(
        &self,
        amount: u64,
        cooperative: bool,
    ) -> Result<tidvar_core::iou::Checked, PayerError> {
        let record = self.record.as_ref().ok_or(PayerError::Backup)?;
        let sync = self.sync.as_ref().ok_or(PayerError::Backup)?;
        let notes = sync.funding_notes()?;
        let kind = if cooperative {
            tidvar_core::iou::Kind::Cooperative
        } else {
            tidvar_core::iou::Kind::Payment
        };
        let amount =
            tidvar_core::params::Amount::try_from(amount).map_err(|_| PayerError::Backup)?;
        let target = u32::try_from(sync.height)
            .map_err(|_| PayerError::Backup)?
            .checked_add(1)
            .ok_or(PayerError::Backup)?;
        Ok(tidvar_core::iou::build(
            record,
            &notes,
            amount,
            target,
            kind,
            &mut rand_core::OsRng,
        )?)
    }
    pub fn funding_request(&self) -> Result<FundingRequest, PayerError> {
        let record = self.record.as_ref().ok_or(PayerError::Backup)?;
        record.verify()?;
        let address = self.keys.public.address.clone();
        let recipient = zcash_address::ZcashAddress::try_from_encoded(&address)
            .map_err(|_| PayerError::Backup)?;
        let amount_zat = record.body.terms.funded.value();
        let amount = zcash_protocol::value::Zatoshis::from_u64(amount_zat)
            .map_err(|_| PayerError::Backup)?;
        let uri =
            zip321::TransactionRequest::new(vec![zip321::Payment::without_memo(recipient, amount)])
                .map_err(|_| PayerError::Backup)?
                .to_uri();
        let decimal = format!(
            "{}.{:08}",
            amount_zat / 100_000_000,
            amount_zat % 100_000_000
        );
        let amount = decimal
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned();
        Ok(FundingRequest {
            address,
            amount_zat,
            amount,
            uri,
        })
    }
    pub fn record(&self) -> Option<&tidvar_core::record::SignedRecord> {
        self.record.as_ref()
    }
    pub fn record_body(
        &self,
        root: &Root,
        offer: tidvar_core::record::Offer,
        terms: tidvar_core::record::Terms,
    ) -> Result<tidvar_core::record::RecordBody, PayerError> {
        let commitment = self.seal.sealed().commitment()?;
        let id = tidvar_core::record::tab_id(&self.keys.public, &offer, &terms, &commitment)?;
        let public = reddsa::VerificationKey::from(&root.signing_key(&id)?).into();
        let body = tidvar_core::record::RecordBody::new(
            offer,
            terms,
            &self.keys.public,
            commitment,
            self.seal.challenge().indices.clone(),
            public,
        )?;
        if body.refund_round != self.seal.sealed().round {
            return Err(PayerError::Backup);
        }
        Ok(body)
    }
    pub fn approve_record<R: RngCore + CryptoRng>(
        &mut self,
        root: &Root,
        offer: tidvar_core::record::Offer,
        terms: tidvar_core::record::Terms,
        partial: tidvar_core::record::ServiceSigned,
        rng: &mut R,
    ) -> Result<tidvar_core::record::SignedRecord, PayerError> {
        partial.verify()?;
        let expected = self.record_body(root, offer, terms)?;
        if expected.canonical()? != partial.body.canonical()? {
            return Err(PayerError::Backup);
        }
        if let Some(record) = &self.record {
            if record.body.canonical()? != expected.canonical()? {
                return Err(PayerError::Backup);
            }
            return Ok(record.clone());
        }
        let record = partial.countersign(&root.signing_key(&expected.tab_id)?, rng)?;
        self.record = Some(record.clone());
        Ok(record)
    }
    pub fn public_keys(&self) -> &tidvar_core::keys::PublicKeys {
        &self.keys.public
    }
    pub fn seal(&self) -> &VerifiedSeal {
        &self.seal
    }
    pub fn sync(&self) -> Option<&SyncState> {
        self.sync.as_ref()
    }
    pub fn sync_mut(&mut self) -> Option<&mut SyncState> {
        self.sync.as_mut()
    }
    pub fn start_sync(
        &mut self,
        before: tidvar_chain::service::TreeState,
    ) -> Result<(), PayerError> {
        if self.sync.is_some() {
            return Err(PayerError::Backup);
        }
        self.sync = Some(SyncState::new(&self.keys.public.viewing_key, before)?);
        Ok(())
    }
    pub fn consume_commitment(&mut self, bytes: &[u8]) -> Result<(), PayerError> {
        let parsed = reddsa::frost::redpallas::round1::SigningCommitments::deserialize(bytes)
            .map_err(|_| PayerError::Backup)?;
        let canonical = parsed.serialize().map_err(|_| PayerError::Backup)?;
        let hash = encoding::hash(
            b"TidvarUsedNonce",
            &encoding::encode_bytes(&[&canonical]).map_err(|_| PayerError::Backup)?,
        )
        .map_err(|_| PayerError::Backup)?;
        if self.consumed.contains(&hash) {
            return Err(PayerError::ReusedCommitment);
        }
        self.consumed.push(hash);
        Ok(())
    }
    pub fn backup(&self, root: &Root) -> Result<Zeroizing<Vec<u8>>, PayerError> {
        let record_key = root.signing_key(&self.seal.sealed().commitment()?)?;
        let backup = Backup {
            version: 1,
            package: SecretBytes(self.keys.secret_bytes()?.to_vec()),
            public_package: self.keys.public_bytes()?,
            viewing_key: self.keys.public.viewing_key.clone(),
            sealed: self.seal.sealed().clone(),
            challenge: self.seal.challenge().clone(),
            openings: SavedOpenings(self.seal.openings().to_vec()),
            sync: self.sync.as_ref().map(SyncState::snapshot).transpose()?,
            consumed: self.consumed.clone(),
            record: self.record.clone(),
            pending: self.pending.clone(),
            paid: self.paid,
            channel: self.channel.clone(),
            authorizer: self.authorizer.clone(),
            owner: reddsa::VerificationKey::from(&record_key).into(),
        };
        Ok(Zeroizing::new(
            serde_json::to_vec(&backup).map_err(|_| PayerError::Backup)?,
        ))
    }
    pub fn restore(root: &Root, plaintext: &[u8]) -> Result<Self, PayerError> {
        if plaintext.len() > 16 * 1024 * 1024 {
            return Err(PayerError::Backup);
        }
        let backup: Backup = serde_json::from_slice(plaintext).map_err(|_| PayerError::Backup)?;
        if backup.version != 1 {
            return Err(PayerError::Backup);
        }
        let keys = Keys::restore(
            Role::Payer,
            &backup.package.0,
            &backup.public_package,
            &backup.viewing_key,
            backup
                .record
                .as_ref()
                .map_or(Network::Testnet, |record| record.body.offer.network)
                .kind(),
        )?;
        let commitment = backup.sealed.commitment()?;
        let owner: [u8; 32] = reddsa::VerificationKey::from(&root.signing_key(&commitment)?).into();
        if owner != backup.owner {
            return Err(PayerError::Backup);
        }
        let verifier = Verifier::accept(
            backup.sealed.clone(),
            commitment,
            &keys.public.service,
            backup.sealed.round,
            &quicknet_public_key()?,
        )?;
        let seal = verifier.finish(backup.challenge, backup.openings.0.clone())?;
        let sync = backup.sync.map(SyncState::restore).transpose()?;
        if sync.as_ref().is_some_and(|state| {
            state.viewing_key().to_bytes().as_slice() != keys.public.viewing_key
        }) {
            return Err(PayerError::Backup);
        }
        let unique: std::collections::BTreeSet<_> = backup.consumed.iter().collect();
        if unique.len() != backup.consumed.len() {
            return Err(PayerError::Backup);
        }
        let mut restored = Self {
            keys,
            seal,
            sync,
            consumed: backup.consumed,
            record: None,
            pending: backup.pending,
            paid: backup.paid,
            channel: backup.channel,
            authorizer: backup.authorizer,
        };
        if let Some(record) = backup.record {
            record.verify()?;
            let expected =
                restored.record_body(root, record.body.offer.clone(), record.body.terms.clone())?;
            if expected.canonical()? != record.body.canonical()? {
                return Err(PayerError::Backup);
            }
            restored
                .authorizer
                .validate(record.body.terms.funded)
                .map_err(|_| PayerError::Backup)?;
            if restored
                .authorizer
                .balance()
                .map_err(|_| PayerError::Backup)?
                .paid
                .value()
                > restored.paid
            {
                return Err(PayerError::Backup);
            }
            if restored.paid > record.body.maximum_payment.value() {
                return Err(PayerError::Backup);
            }
            restored.record = Some(record);
        }
        if restored.record.is_none()
            && (restored.paid != 0
                || restored.channel.next_k != 1
                || restored.channel.pending.is_some())
        {
            return Err(PayerError::Backup);
        }
        if restored.channel.next_k == 0
            || restored.channel.next_k.is_multiple_of(2)
            || (restored.channel.next_k == 1) != restored.channel.previous.is_none()
        {
            return Err(PayerError::Backup);
        }
        if restored
            .channel
            .pending_paid
            .is_some_and(|paid| paid > restored.paid)
            || (restored.channel.pending.is_none() && restored.channel.pending_paid.is_some())
        {
            return Err(PayerError::Backup);
        }
        if let Some(pending) = &restored.channel.pending {
            let record = restored.record.as_ref().ok_or(PayerError::Backup)?;
            if !pending.message.follows(
                tidvar_core::transport::Id(record.body.tab_id),
                restored.channel.next_k,
                restored.channel.previous,
            ) {
                return Err(PayerError::Backup);
            }
            pending
                .verify(record.body.payer_record_key)
                .map_err(|_| PayerError::Backup)?;
        }
        Ok(restored)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Channel {
    next_k: u64,
    previous: Option<tidvar_core::transport::Id>,
    pending: Option<tidvar_core::transport::Authenticated>,
    #[serde(default)]
    pending_paid: Option<u64>,
}
impl Default for Channel {
    fn default() -> Self {
        Self {
            next_k: 1,
            previous: None,
            pending: None,
            pending_paid: None,
        }
    }
}
impl Payer {
    /// After the refund time: opens the service's sealed share with the public drand beacon
    /// and signs the refund alone. Returns the signed PCZT; it holds no secrets and only
    /// needs its proofs before broadcast.
    pub fn refund_transaction(
        &self,
        beacon: &tidvar_seal::VerifiedBeacon,
    ) -> Result<Vec<u8>, PayerError> {
        let record = self.record.as_ref().ok_or(PayerError::Backup)?;
        let sync = self.sync.as_ref().ok_or(PayerError::Backup)?;
        if beacon.round() != record.body.refund_round {
            return Err(PayerError::Seal(SealError::Beacon));
        }
        let share = self.seal.open(beacon)?;
        let service = Keys::from_service_share(
            &share,
            &self.keys.public_bytes()?,
            &self.keys.public.viewing_key,
            record.body.offer.network.kind(),
        )?;
        let notes = sync.funding_notes()?;
        let target =
            u32::try_from(sync.height.saturating_add(1)).map_err(|_| PayerError::Backup)?;
        let pczt = tidvar_core::iou::refund(
            record,
            &notes,
            target,
            &self.keys,
            &service,
            &mut rand_core::OsRng,
        )?;
        pczt.serialize().map_err(|_| PayerError::Backup)
    }
    pub fn prepare_request(
        &mut self,
        root: &Root,
        body: serde_json::Value,
    ) -> Result<tidvar_core::transport::Authenticated, PayerError> {
        if let Some(pending) = &self.channel.pending {
            if pending.message.body != body {
                return Err(PayerError::Pending);
            }
            return Ok(pending.clone());
        }
        let record = self.record.as_ref().ok_or(PayerError::Backup)?;
        let message = tidvar_core::transport::Message {
            tab_id: tidvar_core::transport::Id(record.body.tab_id),
            k: self.channel.next_k,
            previous: self.channel.previous,
            body,
        };
        let signed = tidvar_core::transport::Authenticated::sign(
            message,
            &root.signing_key(&record.body.tab_id)?,
            &mut rand_core::OsRng,
        )
        .map_err(|_| PayerError::Backup)?;
        self.channel.pending = Some(signed.clone());
        Ok(signed)
    }
    pub fn pending_request(&self) -> Option<&tidvar_core::transport::Authenticated> {
        self.channel.pending.as_ref()
    }
    pub fn accept_reply(
        &mut self,
        reply: tidvar_core::transport::Message<serde_json::Value>,
    ) -> Result<serde_json::Value, PayerError> {
        let pending = self.channel.pending.as_ref().ok_or(PayerError::Backup)?;
        let hash = pending.message.hash().map_err(|_| PayerError::Backup)?;
        if !reply.follows(
            pending.message.tab_id,
            pending.message.k.checked_add(1).ok_or(PayerError::Backup)?,
            Some(hash),
        ) {
            return Err(PayerError::Backup);
        }
        let previous = reply.hash().map_err(|_| PayerError::Backup)?;
        if let Some(previous) = self.channel.pending_paid {
            match reply.body.get("step").and_then(|value| value.as_str()) {
                Some("refused") => {
                    self.paid = previous;
                    self.pending = None;
                }
                Some("accepted") => {}
                _ => return Err(PayerError::Backup),
            }
        }
        self.channel.pending_paid = None;
        self.channel.next_k = reply.k.checked_add(1).ok_or(PayerError::Backup)?;
        self.channel.previous = Some(previous);
        self.channel.pending = None;
        Ok(reply.body)
    }
    pub fn signed_amount(&self) -> u64 {
        self.paid
    }
}

impl Payer {
    pub fn prepare_iou_request(
        &mut self,
        root: &Root,
        prepared: &tidvar_core::iou::Prepared,
        services: Vec<tidvar_core::frost::Commitment>,
        tree: tidvar_chain::service::TreeState,
    ) -> Result<tidvar_core::transport::Authenticated, PayerError> {
        if self.channel.pending.is_some() {
            return Err(PayerError::Pending);
        }
        if prepared.cooperative {
            return Err(PayerError::Backup);
        }
        let previous = self.paid;
        let signed = self.sign_transaction(
            &prepared.pczt,
            prepared.amount,
            prepared.anchor_height,
            false,
            tree,
            services,
        )?;
        let payment = tidvar_core::iou::Request {
            pczt: prepared.pczt.clone(),
            amount: tidvar_core::params::Amount::try_from(prepared.amount)
                .map_err(|_| PayerError::Backup)?,
            anchor_height: prepared.anchor_height,
            messages: signed.messages,
            shares: signed.shares,
        };
        let request =
            self.prepare_request(root, serde_json::json!({"step":"iou","payment":payment}))?;
        self.channel.pending_paid = Some(previous);
        Ok(request)
    }
    pub fn prepare_close_request(
        &mut self,
        root: &Root,
        prepared: &tidvar_core::iou::Prepared,
        services: Vec<tidvar_core::frost::Commitment>,
        tree: tidvar_chain::service::TreeState,
    ) -> Result<tidvar_core::transport::Authenticated, PayerError> {
        if self.channel.pending.is_some() {
            return Err(PayerError::Pending);
        }
        if !prepared.cooperative
            || !self
                .authorizer
                .balance()
                .map_err(|_| PayerError::Backup)?
                .closing
        {
            return Err(PayerError::Backup);
        }
        let signed = self.sign_transaction(
            &prepared.pczt,
            prepared.amount,
            prepared.anchor_height,
            true,
            tree,
            services,
        )?;
        let payment = tidvar_core::iou::Request {
            pczt: prepared.pczt.clone(),
            amount: tidvar_core::params::Amount::try_from(prepared.amount)
                .map_err(|_| PayerError::Backup)?,
            anchor_height: prepared.anchor_height,
            messages: signed.messages,
            shares: signed.shares,
        };
        self.prepare_request(
            root,
            serde_json::json!({"step":"close_submit","payment":payment}),
        )
    }
    pub fn commitment_unused(
        &self,
        commitment: &tidvar_core::frost::Commitment,
    ) -> Result<bool, PayerError> {
        commitment.verify().map_err(|_| PayerError::Backup)?;
        let hash = encoding::hash(
            b"TidvarUsedNonce",
            &encoding::encode_bytes(&[&commitment.data]).map_err(|_| PayerError::Backup)?,
        )
        .map_err(|_| PayerError::Backup)?;
        Ok(!self.consumed.contains(&hash))
    }
}

impl Payer {
    pub fn accept_reply_at(
        &mut self,
        reply: tidvar_core::transport::Message<serde_json::Value>,
        now_ms: u64,
    ) -> Result<serde_json::Value, PayerError> {
        let mut authorizer = self.authorizer.clone();
        let pending = self.channel.pending.as_ref().ok_or(PayerError::Backup)?;
        if pending
            .message
            .body
            .get("step")
            .and_then(|value| value.as_str())
            == Some("iou")
            && reply.body.get("step").and_then(|value| value.as_str()) == Some("accepted")
        {
            let amount = pending
                .message
                .body
                .get("payment")
                .and_then(|payment| payment.get("amount"))
                .and_then(|amount| amount.as_u64())
                .ok_or(PayerError::Backup)?;
            let funded = self
                .record
                .as_ref()
                .ok_or(PayerError::Backup)?
                .body
                .terms
                .funded;
            authorizer
                .accept(
                    funded,
                    tidvar_core::params::Amount::try_from(amount)
                        .map_err(|_| PayerError::Backup)?,
                    now_ms,
                )
                .map_err(|_| PayerError::Backup)?;
            let balance = authorizer.balance().map_err(|_| PayerError::Backup)?;
            if !balance.paused
                && !balance.running
                && tidvar_core::params::can_start_vpn(balance.paid, balance.used)
            {
                authorizer.resume(now_ms).map_err(|_| PayerError::Backup)?;
            }
        }
        let sent = pending
            .message
            .body
            .get("step")
            .and_then(|value| value.as_str());
        match (
            sent,
            reply.body.get("step").and_then(|value| value.as_str()),
        ) {
            (Some("close_start"), Some("close_ready")) => {
                authorizer.close(now_ms).map_err(|_| PayerError::Backup)?;
            }
            (Some("vpn_pause"), Some("vpn_paused")) => {
                authorizer.pause(now_ms).map_err(|_| PayerError::Backup)?;
            }
            (Some("vpn_peer"), Some("vpn_peer")) | (Some("vpn_resume"), Some("vpn_resumed")) => {
                authorizer.unpause(now_ms).map_err(|_| PayerError::Backup)?;
            }
            _ => {}
        }
        let body = self.accept_reply(reply)?;
        self.authorizer = authorizer;
        Ok(body)
    }
    pub fn authorization_balance(
        &mut self,
        now_ms: u64,
    ) -> Result<tidvar_core::ledger::Balance, PayerError> {
        self.authorizer
            .tick(now_ms)
            .map_err(|_| PayerError::Backup)?;
        self.authorizer.balance().map_err(|_| PayerError::Backup)
    }
    pub fn payment_target(
        &mut self,
        now_ms: u64,
        hidden: bool,
        service_used: u64,
    ) -> Result<u64, PayerError> {
        let funded = self
            .record
            .as_ref()
            .ok_or(PayerError::Backup)?
            .body
            .terms
            .funded;
        let balance = self.authorization_balance(now_ms)?;
        if balance.paused || balance.closing {
            return Err(PayerError::Backup);
        }
        let used = balance.used.min(
            tidvar_core::params::Amount::try_from(service_used).map_err(|_| PayerError::Backup)?,
        );
        tidvar_core::params::payment_target(funded, used, hidden)
            .map(|amount| amount.value())
            .map_err(|_| PayerError::Backup)
    }
}
