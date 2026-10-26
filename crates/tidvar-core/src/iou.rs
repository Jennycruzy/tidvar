use crate::{
    encoding,
    params::{self, Amount},
    record::{self, Network, SignedRecord},
};
use ff::PrimeField;
use orchard::{
    Note,
    keys::{FullViewingKey, Scope},
    note_encryption::IronwoodDomain,
    tree::MerklePath,
};
use pczt::{
    Pczt,
    roles::{creator::Creator, io_finalizer::IoFinalizer, signer::Signer, verifier::Verifier},
};
use rand_core::{CryptoRng, RngCore};
use thiserror::Error;
use zcash_primitives::transaction::{
    builder::{BuildConfig, Builder, BundlePadding, default_tx_expiry_delta},
    fees::zip317::FeeRule,
};
use zcash_protocol::{
    PoolType,
    consensus::{BlockHeight, BranchId, TEST_NETWORK},
    memo::MemoBytes,
    value::Zatoshis,
};

#[derive(Debug, Error)]
pub enum IouError {
    #[error("The payment transaction does not match this tab.")]
    Transaction,
    #[error("The payment needs a confirmed funding note and a valid witness.")]
    Witness,
    #[error("The payment amount exceeds the tab limit.")]
    Amount,
}
#[derive(Clone)]
pub struct FundingNote {
    pub note: Note,
    pub path: MerklePath,
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Kind {
    Payment,
    Cooperative,
}
#[derive(Clone)]
pub struct Spend {
    pub index: usize,
    pub alpha: [u8; 32],
    pub nullifier: [u8; 32],
}
pub struct Checked {
    pub pczt: Pczt,
    pub sighash: [u8; 32],
    pub spends: Vec<Spend>,
    pub witness_hash: [u8; 32],
}
pub struct Random<'a, R: RngCore + CryptoRng>(pub &'a mut R);
impl<R: RngCore + CryptoRng> rand_core_new::TryRng for Random<'_, R> {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(self.0.next_u32())
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0.next_u64())
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.0.fill_bytes(dst);
        Ok(())
    }
}
impl<R: RngCore + CryptoRng> rand_core_new::TryCryptoRng for Random<'_, R> {}
fn viewing(record: &SignedRecord) -> Result<FullViewingKey, IouError> {
    record.verify().map_err(|_| IouError::Transaction)?;
    if record.body.offer.network != Network::Testnet {
        return Err(IouError::Transaction);
    }
    let bytes = record
        .body
        .viewing_key
        .as_slice()
        .try_into()
        .map_err(|_| IouError::Transaction)?;
    FullViewingKey::from_bytes(&bytes).ok_or(IouError::Transaction)
}
fn fee(record: &SignedRecord, notes: &[FundingNote], amount: Amount) -> Result<Amount, IouError> {
    let value = notes
        .iter()
        .try_fold(0u64, |sum, n| sum.checked_add(n.note.value().inner()))
        .ok_or(IouError::Amount)?;
    if value != record.body.terms.funded.value() || amount > record.body.maximum_payment {
        return Err(IouError::Amount);
    }
    params::close_fee(u8::try_from(notes.len()).map_err(|_| IouError::Amount)?)
        .map_err(|_| IouError::Amount)
}
fn expiry(record: &SignedRecord, target: u32, kind: Kind) -> Result<u32, IouError> {
    if BranchId::for_height(&TEST_NETWORK, BlockHeight::from_u32(target)) != BranchId::Nu7 {
        return Err(IouError::Transaction);
    }
    let expiry = match kind {
        Kind::Payment => record.body.expiry,
        Kind::Cooperative => target
            .checked_add(default_tx_expiry_delta(BranchId::Nu7))
            .ok_or(IouError::Transaction)?,
    };
    if expiry <= target || expiry > params::MAXIMUM_EXPIRY {
        return Err(IouError::Transaction);
    }
    Ok(expiry)
}
pub fn build<R: RngCore + CryptoRng>(
    record: &SignedRecord,
    notes: &[FundingNote],
    amount: Amount,
    target: u32,
    kind: Kind,
    rng: &mut R,
) -> Result<Checked, IouError> {
    let fvk = viewing(record)?;
    let cost = fee(record, notes, amount)?;
    if kind == Kind::Payment && amount.value() == 0 {
        return Err(IouError::Amount);
    }
    let first = notes.first().ok_or(IouError::Witness)?;
    let anchor = first.path.root(first.note.commitment().into());
    let mut builder = Builder::new(
        TEST_NETWORK,
        BlockHeight::from_u32(target),
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(anchor),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    )
    .with_expiry_height(BlockHeight::from_u32(expiry(record, target, kind)?));
    for note in notes {
        builder
            .add_ironwood_spend::<std::convert::Infallible>(
                fvk.clone(),
                note.note,
                note.path.clone(),
            )
            .map_err(|_| IouError::Witness)?;
    }
    let service = record::receiver(&record.body.offer.address, Network::Testnet)
        .map_err(|_| IouError::Transaction)?;
    let payer = record::receiver(&record.body.terms.return_address, Network::Testnet)
        .map_err(|_| IouError::Transaction)?;
    if kind == Kind::Payment || amount.value() > 0 {
        builder
            .add_ironwood_output::<std::convert::Infallible>(
                Some(fvk.to_ovk(Scope::External)),
                service,
                Zatoshis::from_u64(amount.value()).map_err(|_| IouError::Amount)?,
                MemoBytes::empty(),
            )
            .map_err(|_| IouError::Transaction)?;
    }
    let returned = record
        .body
        .terms
        .funded
        .value()
        .checked_sub(amount.value())
        .and_then(|n| n.checked_sub(cost.value()))
        .ok_or(IouError::Amount)?;
    builder
        .add_ironwood_output::<std::convert::Infallible>(
            Some(fvk.to_ovk(Scope::External)),
            payer,
            Zatoshis::from_u64(returned).map_err(|_| IouError::Amount)?,
            MemoBytes::empty(),
        )
        .map_err(|_| IouError::Transaction)?;
    if builder
        .get_fee(&FeeRule::standard())
        .map_err(|_| IouError::Transaction)?
        .into_u64()
        != cost.value()
    {
        return Err(IouError::Amount);
    }
    let result = builder
        .build_for_pczt(Random(rng), &FeeRule::standard())
        .map_err(|_| IouError::Transaction)?;
    let pczt = Creator::build_from_parts(result.pczt_parts).ok_or(IouError::Transaction)?;
    let pczt = IoFinalizer::new(pczt)
        .finalize_io(Random(rng))
        .map_err(|_| IouError::Transaction)?;
    check(
        pczt,
        record,
        notes,
        amount,
        TransactionContext {
            target,
            kind,
            anchor: anchor.to_bytes(),
        },
        rng,
    )
}

pub struct TransactionContext {
    pub target: u32,
    pub kind: Kind,
    pub anchor: [u8; 32],
}

pub fn check(
    mut pczt: Pczt,
    record: &SignedRecord,
    notes: &[FundingNote],
    amount: Amount,
    context: TransactionContext,
    rng: &mut (impl RngCore + CryptoRng),
) -> Result<Checked, IouError> {
    let TransactionContext {
        target,
        kind,
        anchor,
    } = context;
    let fvk = viewing(record)?;
    let cost = fee(record, notes, amount)?;
    if kind == Kind::Payment && amount.value() == 0 {
        return Err(IouError::Amount);
    }
    pczt.resolve_fields().map_err(|_| IouError::Transaction)?;
    let global = pczt.global();
    if *global.tx_version() != 6
        || *global.consensus_branch_id() != u32::from(BranchId::Nu7)
        || *global.expiry_height() != expiry(record, target, kind)?
        || *global.coin_type() != 1
        || *global.tx_modifiable() != 0
        || *global.fallback_lock_time() != Some(0)
    {
        return Err(IouError::Transaction);
    }
    if [PoolType::TRANSPARENT, PoolType::SAPLING, PoolType::ORCHARD]
        .iter()
        .any(|pool| pczt.has_data_in_pool(*pool))
    {
        return Err(IouError::Transaction);
    }
    let service = record::receiver(&record.body.offer.address, Network::Testnet)
        .map_err(|_| IouError::Transaction)?;
    let payer = record::receiver(&record.body.terms.return_address, Network::Testnet)
        .map_err(|_| IouError::Transaction)?;
    let returned = record
        .body
        .terms
        .funded
        .value()
        .checked_sub(amount.value())
        .and_then(|n| n.checked_sub(cost.value()))
        .ok_or(IouError::Amount)?;
    let mut outputs = vec![(payer, returned)];
    if kind == Kind::Payment || amount.value() > 0 {
        outputs.push((service, amount.value()));
    }
    let sighash = Signer::new(pczt.clone())
        .map_err(|_| IouError::Transaction)?
        .shielded_sighash();
    let mut expected: Vec<_> = notes
        .iter()
        .map(|n| n.note.nullifier(&fvk).to_bytes())
        .collect();
    if expected
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != notes.len()
    {
        return Err(IouError::Witness);
    }
    let mut spends = Vec::new();
    let mut witness_bytes = anchor.to_vec();
    let pczt = Verifier::new(pczt)
        .with_ironwood::<IouError, _>(|bundle| {
            use pczt::roles::verifier::OrchardError;
            let bad = || OrchardError::Custom(IouError::Transaction);
            bundle.verify_cross_address_restriction()?;
            if bundle.anchor().to_bytes() != anchor
                || bundle.actions().len() != usize::max(2, notes.len())
                || i64::try_from(*bundle.value_sum()).ok() != i64::try_from(cost.value()).ok()
                || bundle.zkproof().is_some()
            {
                return Err(bad());
            }
            for (index, action) in bundle.actions().iter().enumerate() {
                action.verify_cv_net()?;
                let spend = action.spend();
                let output = action.output();
                output.verify_note_commitment(spend)?;
                let value = spend.value().ok_or_else(bad)?.inner();
                if let Some(position) = expected
                    .iter()
                    .position(|nf| *nf == spend.nullifier().to_bytes())
                {
                    let funding = notes
                        .iter()
                        .find(|n| n.note.nullifier(&fvk) == *spend.nullifier())
                        .ok_or_else(bad)?;
                    spend.verify_nullifier(Some(&fvk))?;
                    spend.verify_rk(Some(&fvk))?;
                    if spend.dummy_sk().is_some()
                        || spend.spend_auth_sig().is_some()
                        || value != funding.note.value().inner()
                        || spend.recipient() != &Some(funding.note.recipient())
                        || spend.note_version() != &funding.note.version()
                        || spend.rho() != &Some(funding.note.rho())
                        || spend.rseed().as_ref().map(|r| r.as_bytes())
                            != Some(funding.note.rseed().as_bytes())
                    {
                        return Err(bad());
                    }
                    let path = spend.witness().as_ref().ok_or_else(bad)?;
                    if path.root(funding.note.commitment().into()).to_bytes() != anchor {
                        return Err(bad());
                    }
                    witness_bytes.extend(spend.nullifier().to_bytes());
                    witness_bytes.extend(path.position().to_le_bytes());
                    for hash in path.auth_path() {
                        witness_bytes.extend(hash.to_bytes());
                    }
                    spends.push(Spend {
                        index,
                        alpha: spend.alpha().ok_or_else(bad)?.to_repr(),
                        nullifier: spend.nullifier().to_bytes(),
                    });
                    expected.remove(position);
                } else {
                    if value != 0 || spend.dummy_sk().is_some() {
                        return Err(bad());
                    }
                    spend.verify_nullifier(None)?;
                    spend.verify_rk(None)?;
                    let signature = spend.spend_auth_sig().as_ref().ok_or_else(bad)?;
                    spend.rk().verify(&sighash, signature).map_err(|_| bad())?;
                }
                let recipient = output.recipient().ok_or_else(bad)?;
                let value = output.value().ok_or_else(bad)?.inner();
                if let Some(position) = outputs
                    .iter()
                    .position(|(address, amount)| address == &recipient && *amount == value)
                {
                    let (note, address, memo) =
                        zcash_note_encryption::try_output_recovery_with_ovk(
                            &IronwoodDomain::for_pczt_action(action),
                            &fvk.to_ovk(Scope::External),
                            action,
                            action.cv_net(),
                            &output.encrypted_note().out_ciphertext,
                        )
                        .ok_or_else(bad)?;
                    if note.value().inner() != value
                        || address != recipient
                        || memo != MemoBytes::empty().into_bytes()
                    {
                        return Err(bad());
                    }
                    outputs.remove(position);
                } else if value != 0 {
                    return Err(bad());
                }
            }
            if !expected.is_empty() || !outputs.is_empty() {
                return Err(bad());
            }
            Ok(())
        })
        .map_err(|_| IouError::Transaction)?
        .finish();
    let pczt = IoFinalizer::new(pczt)
        .finalize_io(Random(rng))
        .map_err(|_| IouError::Transaction)?;
    if Signer::new(pczt.clone())
        .map_err(|_| IouError::Transaction)?
        .shielded_sighash()
        != sighash
    {
        return Err(IouError::Transaction);
    }
    let witness_hash =
        encoding::hash(b"TidvarWitness", &witness_bytes).map_err(|_| IouError::Witness)?;
    Ok(Checked {
        pczt,
        sighash,
        spends,
        witness_hash,
    })
}
