use crate::{PayeeError, store::Store};
use rand_core::{CryptoRng, RngCore};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use tidvar_core::{
    frost,
    iou::{self, Checked, FundingNote, Request, TransactionContext},
};

#[derive(Serialize)]
pub struct Closing {
    pub txid: String,
    #[serde(skip_serializing)]
    pub raw: Vec<u8>,
    pub amount: u64,
    pub fee: u64,
    pub returned: u64,
    pub expiry: u32,
    pub height: u64,
}
impl Store {
    pub fn closing_transaction(&self, id: &[u8; 32]) -> Result<Option<Closing>, PayeeError> {
        self.connection.query_row(
            "SELECT txid,raw,amount,fee,returned,expiry,height FROM cooperative_closes WHERE tab_id=?1 ORDER BY sequence DESC LIMIT 1",
            params![id.as_slice()],
            |row| Ok(Closing {txid:row.get(0)?,raw:row.get(1)?,amount:row.get(2)?,fee:row.get(3)?,returned:row.get(4)?,expiry:row.get(5)?,height:row.get(6)?}),
        ).optional().map_err(|_|PayeeError::Storage)
    }
    pub fn save_close(
        &mut self,
        id: &[u8; 32],
        transaction: &zcash_primitives::transaction::Transaction,
        amount: u64,
        fee: u64,
    ) -> Result<Closing, PayeeError> {
        let tab = self.load(id)?.ok_or(PayeeError::Terms)?;
        let returned = tab
            .record
            .body
            .terms
            .funded
            .value()
            .checked_sub(amount)
            .and_then(|value| value.checked_sub(fee))
            .ok_or(PayeeError::Terms)?;
        let mut raw = Vec::new();
        transaction
            .write(&mut raw)
            .map_err(|_| PayeeError::Storage)?;
        self.connection.execute(
            "INSERT INTO cooperative_closes (tab_id,txid,raw,amount,fee,returned,expiry) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(tab_id,txid) DO NOTHING",
            params![id.as_slice(),transaction.txid().to_string(),raw,amount,fee,returned,u32::from(transaction.expiry_height())],
        ).map_err(|_|PayeeError::Storage)?;
        self.closing_transaction(id)?.ok_or(PayeeError::Storage)
    }
    pub fn mark_close_mined(
        &mut self,
        id: &[u8; 32],
        txid: &str,
        height: u64,
    ) -> Result<(), PayeeError> {
        if height == 0 {
            return Err(PayeeError::Terms);
        }
        let changed = self
            .connection
            .execute(
                "UPDATE cooperative_closes SET height=?1 WHERE tab_id=?2 AND txid=?3",
                params![height, id.as_slice(), txid],
            )
            .map_err(|_| PayeeError::Storage)?;
        if changed != 1 {
            return Err(PayeeError::Storage);
        }
        Ok(())
    }
    pub fn sign_close<R: RngCore + CryptoRng>(
        &mut self,
        id: &[u8; 32],
        request: &Request,
        notes: &[FundingNote],
        context: TransactionContext,
        rng: &mut R,
    ) -> Result<Checked, PayeeError> {
        let tab = self.load(id)?.ok_or(PayeeError::Terms)?;
        let observed: Vec<u8> = self
            .connection
            .query_row(
                "SELECT ledger FROM balances WHERE tab_id=?1",
                params![id.as_slice()],
                |row| row.get(0),
            )
            .map_err(|_| PayeeError::Storage)?;
        let ledger: tidvar_core::ledger::Ledger =
            serde_json::from_slice(&observed).map_err(|_| PayeeError::Storage)?;
        ledger.validate(tab.record.body.terms.funded)?;
        let balance = ledger.balance()?;
        if !balance.closing
            || request.amount != balance.used
            || balance.used > balance.paid
            || context.kind != iou::Kind::Cooperative
        {
            return Err(PayeeError::Terms);
        }
        let pczt = pczt::Pczt::parse(&request.pczt).map_err(|_| PayeeError::Proof)?;
        let mut checked = iou::check(pczt, &tab.record, notes, request.amount, context, rng)?;
        if checked.spends.len() != request.messages.len()
            || checked.spends.len() != request.shares.len()
        {
            return Err(PayeeError::Nonce);
        }
        let mut unique = std::collections::BTreeSet::new();
        for ((spend, message), payer) in checked
            .spends
            .iter()
            .zip(&request.messages)
            .zip(&request.shares)
        {
            if message.sighash != checked.sighash
                || message.alpha != spend.alpha
                || !unique.insert(message.service.id)
            {
                return Err(PayeeError::Nonce);
            }
            frost::verify_share(&tab.keys, message, payer)?;
        }
        let mut signer = pczt::roles::signer::Signer::new(checked.pczt.clone())
            .map_err(|_| PayeeError::Proof)?;
        for ((spend, message), payer) in checked
            .spends
            .iter()
            .zip(&request.messages)
            .zip(&request.shares)
        {
            let service = self.complete_nonce(id, message, payer)?;
            let signature = frost::aggregate(&tab.keys, message, payer, &service)?;
            signer
                .apply_ironwood_signature(
                    spend.index,
                    orchard::primitives::redpallas::Signature::from(signature),
                )
                .map_err(|_| PayeeError::Proof)?;
        }
        checked.pczt = signer.finish();
        Ok(checked)
    }
}
