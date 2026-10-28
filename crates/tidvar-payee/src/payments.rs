use crate::{PayeeError, opening::Accepted, proof::Proofs};
use rand_core::{CryptoRng, RngCore};
use tidvar_core::{
    frost,
    iou::{self, FundingNote, Request, TransactionContext},
};

pub struct VerifiedIou {
    pub(crate) tab_id: [u8; 32],
    pub(crate) request: Request,
    pub(crate) hash: [u8; 32],
    pub(crate) witness: [u8; 32],
    pub(crate) value: tidvar_core::params::Amount,
}
pub fn verify<R: RngCore + CryptoRng>(
    tab: &Accepted,
    request: Request,
    notes: &[FundingNote],
    context: TransactionContext,
    previous_witness: Option<[u8; 32]>,
    proofs: &Proofs,
    rng: &mut R,
) -> Result<VerifiedIou, PayeeError> {
    if context.kind != iou::Kind::Payment || u64::from(context.target) <= request.anchor_height {
        return Err(PayeeError::Terms);
    }
    let pczt = pczt::Pczt::parse(&request.pczt).map_err(|_| PayeeError::Proof)?;
    let checked = iou::check(pczt, &tab.record, notes, request.amount, context, rng)?;
    if checked.spends.len() != request.messages.len()
        || checked.spends.len() != request.shares.len()
    {
        return Err(PayeeError::Nonce);
    }
    let witness = checked.witness_hash;
    let sighash = checked.sighash;
    let spends = checked.spends.clone();
    if previous_witness != Some(witness) {
        proofs.prove(checked, rng)?;
    }
    let mut service_ids = std::collections::BTreeSet::new();
    for ((spend, message), payer) in spends.iter().zip(&request.messages).zip(&request.shares) {
        if message.sighash != sighash
            || message.alpha != spend.alpha
            || payer.role != tidvar_core::keys::Role::Payer
            || !service_ids.insert(message.service.id)
        {
            return Err(PayeeError::Nonce);
        }
        frost::verify_share(&tab.keys, message, payer)?;
    }
    let hash = request.hash(&tab.record.body.tab_id)?;
    Ok(VerifiedIou {
        tab_id: tab.record.body.tab_id,
        request,
        hash,
        witness,
        value: iou::notes_value(notes)?,
    })
}
