use crate::root::{Root, RootError};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidvar_chain::sync::{Snapshot, SyncState};
use tidvar_core::{
    encoding,
    keys::{FirstRound, KeyError, Keys, Role, RoundOne, RoundTwo, SecondRound},
};
use tidvar_seal::{
    SealError, quicknet_public_key,
    seal::{Challenge, Opening, SealedKey, VerifiedSeal, Verifier},
};
use zcash_protocol::consensus::NetworkType;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Debug, Error)]
pub enum PayerError {
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
    pub fn finish(self, peer: RoundTwo) -> Result<UnsealedPayer, PayerError> {
        Ok(UnsealedPayer {
            keys: self.inner.finish(peer, NetworkType::Test)?,
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
        })
    }
}
impl Payer {
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
            NetworkType::Test,
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
        };
        if let Some(record) = backup.record {
            record.verify()?;
            let expected =
                restored.record_body(root, record.body.offer.clone(), record.body.terms.clone())?;
            if expected.canonical()? != record.body.canonical()? {
                return Err(PayerError::Backup);
            }
            restored.record = Some(record);
        }
        Ok(restored)
    }
}
