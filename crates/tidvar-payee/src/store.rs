use crate::{PayeeError, opening::Accepted};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
use tidvar_core::{
    keys::{Keys, Role},
    record::SignedRecord,
};
use tidvar_seal::seal::SealedKey;
use zeroize::Zeroizing;

pub struct Store {
    pub(crate) connection: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self, PayeeError> {
        if fs::symlink_metadata(path).is_ok_and(|info| info.file_type().is_symlink()) {
            return Err(PayeeError::Storage);
        }
        // Closing any descriptor for the database releases this process's SQLite locks,
        // so the permission handle must be gone before the connection opens.
        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(|_| PayeeError::Storage)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| PayeeError::Storage)?;
        }
        let connection = Connection::open(path).map_err(|_| PayeeError::Storage)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS tabs (
                id BLOB PRIMARY KEY CHECK(length(id)=32),
                record BLOB NOT NULL,
                share BLOB NOT NULL,
                public_package BLOB NOT NULL,
                viewing_key BLOB NOT NULL CHECK(length(viewing_key)=96),
                seal BLOB NOT NULL,
                paid INTEGER NOT NULL DEFAULT 0 CHECK(paid>=0),
                used_ms INTEGER NOT NULL DEFAULT 0 CHECK(used_ms>=0)
            );
            CREATE TABLE IF NOT EXISTS signing_nonces (
                id BLOB PRIMARY KEY CHECK(length(id)=32),
                tab_id BLOB NOT NULL REFERENCES tabs(id),
                commitment BLOB NOT NULL,
                secret BLOB,
                intent BLOB CHECK(intent IS NULL OR length(intent)=32),
                message BLOB CHECK(message IS NULL OR length(message)=32),
                state TEXT NOT NULL CHECK(state IN ('issued','used','discarded')),
                signature BLOB
            );
            CREATE TABLE IF NOT EXISTS balances (
                tab_id BLOB PRIMARY KEY REFERENCES tabs(id),
                ledger BLOB NOT NULL,
                newest BLOB,
                request BLOB CHECK(request IS NULL OR length(request)=32),
                witness BLOB CHECK(witness IS NULL OR length(witness)=32),
                receipt BLOB
            );
            CREATE TABLE IF NOT EXISTS message_exchanges (
                tab_id BLOB PRIMARY KEY REFERENCES tabs(id),
                next_k INTEGER NOT NULL CHECK(next_k>0),
                previous BLOB NOT NULL CHECK(length(previous)=32),
                last_request BLOB NOT NULL CHECK(length(last_request)=32),
                reply BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS funding (
                tab_id BLOB PRIMARY KEY REFERENCES tabs(id),
                snapshot BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS peers (
                slot INTEGER PRIMARY KEY AUTOINCREMENT CHECK(slot>0 AND slot<65534),
                tab_id BLOB NOT NULL UNIQUE REFERENCES tabs(id),
                public_key BLOB NOT NULL UNIQUE CHECK(length(public_key)=32)
            );
            CREATE TABLE IF NOT EXISTS watch_packs (
                tab_id BLOB PRIMARY KEY CHECK(length(tab_id)=32),
                txid TEXT NOT NULL,
                raw BLOB NOT NULL,
                amount INTEGER NOT NULL,
                watch_ms INTEGER NOT NULL,
                expiry INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cooperative_closes (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                tab_id BLOB NOT NULL REFERENCES tabs(id),
                txid TEXT NOT NULL,
                raw BLOB NOT NULL,
                amount INTEGER NOT NULL CHECK(amount>=0),
                fee INTEGER NOT NULL CHECK(fee>0),
                returned INTEGER NOT NULL CHECK(returned>=0),
                expiry INTEGER NOT NULL CHECK(expiry>0),
                height INTEGER NOT NULL DEFAULT 0 CHECK(height>=0),
                UNIQUE(tab_id,txid)
            );
            CREATE TABLE IF NOT EXISTS transfer_counters (
                tab_id BLOB PRIMARY KEY REFERENCES tabs(id),
                rx INTEGER NOT NULL CHECK(rx>=0),
                tx INTEGER NOT NULL CHECK(tx>=0),
                sampled_ms INTEGER NOT NULL CHECK(sampled_ms>=0)
            );
            CREATE TABLE IF NOT EXISTS daily_traffic (
                tab_id BLOB NOT NULL REFERENCES tabs(id),
                day INTEGER NOT NULL,
                bytes INTEGER NOT NULL CHECK(bytes>=0),
                PRIMARY KEY(tab_id,day)
            );
            CREATE TABLE IF NOT EXISTS monthly_traffic (
                month TEXT PRIMARY KEY,
                bytes INTEGER NOT NULL CHECK(bytes>=0)
            );",
            )
            .map_err(|_| PayeeError::Storage)?;
        connection
            .busy_timeout(std::time::Duration::from_millis(2000))
            .map_err(|_| PayeeError::Storage)?;
        Ok(Self { connection })
    }
    pub fn save(&mut self, tab: &Accepted) -> Result<(), PayeeError> {
        tab.verify()?;
        let id = tab.record.body.tab_id;
        let record = serde_json::to_vec(&tab.record).map_err(|_| PayeeError::Storage)?;
        let sealed = serde_json::to_vec(&tab.sealed).map_err(|_| PayeeError::Storage)?;
        let share = tab.keys.secret_bytes()?;
        let public = tab.keys.public_bytes()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        let previous: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT record FROM tabs WHERE id=?1",
                params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| PayeeError::Storage)?;
        if let Some(previous) = previous {
            if previous != record {
                return Err(PayeeError::Terms);
            }
        } else {
            transaction.execute("INSERT INTO tabs (id,record,share,public_package,viewing_key,seal) VALUES (?1,?2,?3,?4,?5,?6)",params![id.as_slice(),record,share.as_slice(),public,tab.keys.public.viewing_key,sealed]).map_err(|_|PayeeError::Storage)?;
        }
        transaction.commit().map_err(|_| PayeeError::Storage)
    }
    pub fn load(&self, id: &[u8; 32]) -> Result<Option<Accepted>, PayeeError> {
        self.connection
            .query_row(
                "SELECT record,share,public_package,viewing_key,seal FROM tabs WHERE id=?1",
                params![id.as_slice()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        Zeroizing::new(row.get::<_, Vec<u8>>(1)?),
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| PayeeError::Storage)?
            .map(|(record, share, public, viewing, sealed)| {
                let record: SignedRecord =
                    serde_json::from_slice(&record).map_err(|_| PayeeError::Storage)?;
                let sealed: SealedKey =
                    serde_json::from_slice(&sealed).map_err(|_| PayeeError::Storage)?;
                let keys = Keys::restore(
                    Role::Service,
                    &share,
                    &public,
                    &viewing,
                    record.body.offer.network.kind(),
                )?;
                if record.body.tab_id != *id {
                    return Err(PayeeError::Terms);
                }
                let tab = Accepted {
                    keys,
                    record,
                    sealed,
                };
                tab.verify()?;
                Ok(tab)
            })
            .transpose()
    }
}

impl Store {
    pub fn issue_nonces<R: rand_core::RngCore + rand_core::CryptoRng>(
        &mut self,
        id: &[u8; 32],
        count: usize,
        intent: Option<[u8; 32]>,
        rng: &mut R,
    ) -> Result<Vec<tidvar_core::frost::Commitment>, PayeeError> {
        use tidvar_core::frost::Nonce;
        if count == 0 || count > 32 {
            return Err(PayeeError::Nonce);
        }
        let tab = self.load(id)?.ok_or(PayeeError::Terms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        let mut public = Vec::with_capacity(count);
        for _ in 0..count {
            let nonce = Nonce::generate(&tab.keys, rng)?;
            let commitment = nonce.public().clone();
            let encoded = serde_json::to_vec(&commitment).map_err(|_| PayeeError::Storage)?;
            let secret = nonce.secret_bytes()?;
            transaction.execute("INSERT INTO signing_nonces (id,tab_id,commitment,secret,intent,state) VALUES (?1,?2,?3,?4,?5,'issued')",params![commitment.id.as_slice(),id.as_slice(),encoded,secret.as_slice(),intent.map(|value|value.to_vec())]).map_err(|_|PayeeError::Storage)?;
            public.push(commitment);
        }
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(public)
    }
    pub fn complete_nonce(
        &mut self,
        id: &[u8; 32],
        message: &tidvar_core::frost::Message,
        payer: &tidvar_core::frost::Share,
    ) -> Result<tidvar_core::frost::Share, PayeeError> {
        use tidvar_core::frost::{self, Commitment, Nonce, Share};
        if payer.role != Role::Payer {
            return Err(PayeeError::Nonce);
        }
        let tab = self.load(id)?.ok_or(PayeeError::Terms)?;
        frost::verify_share(&tab.keys, message, payer)?;
        let hash = message.hash()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        let row=transaction.query_row("SELECT commitment,secret,intent,message,state,signature FROM signing_nonces WHERE id=?1 AND tab_id=?2",params![message.service.id.as_slice(),id.as_slice()],|row|Ok((row.get::<_,Vec<u8>>(0)?,row.get::<_,Option<Vec<u8>>>(1)?.map(Zeroizing::new),row.get::<_,Option<Vec<u8>>>(2)?,row.get::<_,Option<Vec<u8>>>(3)?,row.get::<_,String>(4)?,row.get::<_,Option<Vec<u8>>>(5)?))).optional().map_err(|_|PayeeError::Storage)?.ok_or(PayeeError::Nonce)?;
        let (encoded, secret, intent, previous, state, signature) = row;
        let commitment: Commitment =
            serde_json::from_slice(&encoded).map_err(|_| PayeeError::Storage)?;
        if commitment.data != message.service.data
            || intent
                .as_ref()
                .is_some_and(|value| value.as_slice() != message.sighash)
            || previous
                .as_ref()
                .is_some_and(|value| value.as_slice() != hash)
        {
            return Err(PayeeError::Nonce);
        }
        if state == "used" {
            if previous.as_deref() != Some(hash.as_slice()) {
                return Err(PayeeError::Nonce);
            }
            let share: Share = serde_json::from_slice(&signature.ok_or(PayeeError::Storage)?)
                .map_err(|_| PayeeError::Storage)?;
            transaction.commit().map_err(|_| PayeeError::Storage)?;
            return Ok(share);
        }
        if state != "issued" {
            return Err(PayeeError::Nonce);
        }
        let secret = secret.ok_or(PayeeError::Nonce)?;
        let nonce = Nonce::restore(&secret, commitment)?;
        let share = nonce.sign(&tab.keys, message)?;
        let signature = serde_json::to_vec(&share).map_err(|_| PayeeError::Storage)?;
        let changed=transaction.execute("UPDATE signing_nonces SET state='used',secret=NULL,message=?1,signature=?2 WHERE id=?3 AND tab_id=?4 AND state='issued'",params![hash.as_slice(),signature,message.service.id.as_slice(),id.as_slice()]).map_err(|_|PayeeError::Storage)?;
        if changed != 1 {
            return Err(PayeeError::Nonce);
        }
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(share)
    }
    pub fn discard_nonces(&mut self, id: &[u8; 32], ids: &[[u8; 32]]) -> Result<(), PayeeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        for nonce in ids {
            transaction.execute("UPDATE signing_nonces SET state='discarded',secret=NULL WHERE id=?1 AND tab_id=?2 AND state='issued' AND message IS NULL",params![nonce.as_slice(),id.as_slice()]).map_err(|_|PayeeError::Storage)?;
        }
        transaction.commit().map_err(|_| PayeeError::Storage)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PaymentReceipt {
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub request: [u8; 32],
    pub balance: tidvar_core::ledger::Balance,
}
type RuntimeRow = (
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);
fn runtime(connection: &Connection, id: &[u8; 32]) -> Result<Option<RuntimeRow>, PayeeError> {
    connection
        .query_row(
            "SELECT ledger,newest,request,witness,receipt FROM balances WHERE tab_id=?1",
            params![id.as_slice()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|_| PayeeError::Storage)
}
fn ledger(row: Option<&RuntimeRow>) -> Result<tidvar_core::ledger::Ledger, PayeeError> {
    row.map(|row| serde_json::from_slice(&row.0).map_err(|_| PayeeError::Storage))
        .unwrap_or_else(|| Ok(tidvar_core::ledger::Ledger::default()))
}
fn json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, PayeeError> {
    serde_json::to_vec(value).map_err(|_| PayeeError::Storage)
}
impl Store {
    pub fn payment_witness(&self, id: &[u8; 32]) -> Result<Option<[u8; 32]>, PayeeError> {
        runtime(&self.connection, id)?
            .and_then(|row| row.3)
            .map(|bytes| bytes.try_into().map_err(|_| PayeeError::Storage))
            .transpose()
    }
    pub fn newest_iou(
        &self,
        id: &[u8; 32],
    ) -> Result<Option<tidvar_core::iou::Request>, PayeeError> {
        runtime(&self.connection, id)?
            .and_then(|row| row.1)
            .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| PayeeError::Storage))
            .transpose()
    }
    pub fn accept_iou(
        &mut self,
        payment: crate::payments::VerifiedIou,
        now_ms: u64,
    ) -> Result<PaymentReceipt, PayeeError> {
        use tidvar_core::{frost::Commitment, iou::Request};
        let tab = self.load(&payment.tab_id)?.ok_or(PayeeError::Terms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        let previous = runtime(&transaction, &payment.tab_id)?;
        if let Some(row) = &previous
            && row.2.as_deref() == Some(payment.hash.as_slice())
        {
            return serde_json::from_slice(row.4.as_deref().ok_or(PayeeError::Storage)?)
                .map_err(|_| PayeeError::Storage);
        }
        let mut balance = ledger(previous.as_ref())?;
        balance.validate(tab.record.body.terms.funded)?;
        balance.accept(
            tab.record.body.terms.funded,
            payment.value,
            payment.request.amount,
            balance.observed(now_ms)?,
        )?;
        for message in &payment.request.messages {
            let (encoded, intent): (Vec<u8>, Option<Vec<u8>>) = transaction.query_row(
                "SELECT commitment,intent FROM signing_nonces WHERE id=?1 AND tab_id=?2 AND state='issued' AND secret IS NOT NULL AND message IS NULL",
                params![message.service.id.as_slice(),payment.tab_id.as_slice()],
                |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional().map_err(|_| PayeeError::Storage)?.ok_or(PayeeError::Nonce)?;
            let commitment: Commitment =
                serde_json::from_slice(&encoded).map_err(|_| PayeeError::Storage)?;
            if commitment.id != message.service.id
                || commitment.data != message.service.data
                || intent
                    .as_ref()
                    .is_some_and(|value| value.as_slice() != message.sighash)
            {
                return Err(PayeeError::Nonce);
            }
            let hash = message.hash()?;
            let changed = transaction.execute(
                "UPDATE signing_nonces SET message=?1 WHERE id=?2 AND tab_id=?3 AND state='issued' AND message IS NULL",
                params![hash.as_slice(),message.service.id.as_slice(),payment.tab_id.as_slice()],
            ).map_err(|_| PayeeError::Storage)?;
            if changed != 1 {
                return Err(PayeeError::Nonce);
            }
        }
        if let Some(encoded) = previous.as_ref().and_then(|row| row.1.as_deref()) {
            let old: Request = serde_json::from_slice(encoded).map_err(|_| PayeeError::Storage)?;
            for message in old.messages {
                let changed = transaction.execute(
                    "UPDATE signing_nonces SET state='discarded',secret=NULL WHERE id=?1 AND tab_id=?2 AND state='issued'",
                    params![message.service.id.as_slice(),payment.tab_id.as_slice()],
                ).map_err(|_| PayeeError::Storage)?;
                // A payment already finished for the watchtower keeps its used nonce.
                let packed = changed == 0
                    && transaction
                        .query_row(
                            "SELECT 1 FROM signing_nonces WHERE id=?1 AND tab_id=?2 AND state='used'",
                            params![message.service.id.as_slice(), payment.tab_id.as_slice()],
                            |_| Ok(()),
                        )
                        .optional()
                        .map_err(|_| PayeeError::Storage)?
                        .is_some();
                if changed != 1 && !packed {
                    return Err(PayeeError::Nonce);
                }
            }
        }
        let receipt = PaymentReceipt {
            request: payment.hash,
            balance: balance.balance()?,
        };
        transaction.execute(
            "INSERT INTO balances (tab_id,ledger,newest,request,witness,receipt) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(tab_id) DO UPDATE SET ledger=excluded.ledger,newest=excluded.newest,request=excluded.request,witness=excluded.witness,receipt=excluded.receipt",
            params![payment.tab_id.as_slice(),json(&balance)?,json(&payment.request)?,payment.hash.as_slice(),payment.witness.as_slice(),json(&receipt)?],
        ).map_err(|_| PayeeError::Storage)?;
        transaction
            .execute(
                "UPDATE tabs SET paid=?1,used_ms=?2 WHERE id=?3",
                params![
                    receipt.balance.paid.value(),
                    receipt.balance.vpn_ms,
                    payment.tab_id.as_slice()
                ],
            )
            .map_err(|_| PayeeError::Storage)?;
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(receipt)
    }
    pub fn balance(
        &mut self,
        id: &[u8; 32],
        now_ms: u64,
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        self.change_ledger(id, |ledger| {
            ledger.tick(ledger.observed(now_ms)?)?;
            Ok(())
        })
    }
    pub fn resume_vpn(
        &mut self,
        id: &[u8; 32],
        now_ms: u64,
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        self.change_ledger(id, |ledger| ledger.resume(ledger.observed(now_ms)?))
    }
    pub fn pause_vpn(
        &mut self,
        id: &[u8; 32],
        now_ms: u64,
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        self.change_ledger(id, |ledger| ledger.pause(ledger.observed(now_ms)?))
    }
    pub fn close_ledger(
        &mut self,
        id: &[u8; 32],
        now_ms: u64,
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        self.change_ledger(id, |ledger| {
            ledger.close(ledger.observed(now_ms)?)?;
            Ok(())
        })
    }
    /// Charges a per-request product and records the signed reply atomically, so a
    /// retried request is answered from the cache and never charged twice.
    pub fn charge_and_reply(
        &mut self,
        request: &tidvar_core::transport::Authenticated,
        price: u64,
        now_ms: u64,
        body: impl FnOnce(&tidvar_core::ledger::Balance) -> serde_json::Value,
    ) -> Result<Vec<u8>, PayeeError> {
        if let Some(reply) = self.cached_reply(request)? {
            return Ok(reply);
        }
        let id = request.message.tab_id.0;
        let tab = self.load(&id)?.ok_or(PayeeError::Terms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        if let Some(reply) = request_cursor(&transaction, request)? {
            return Ok(reply);
        }
        let row = runtime(&transaction, &id)?;
        let mut ledger = ledger(row.as_ref())?;
        ledger.validate(tab.record.body.terms.funded)?;
        ledger.charge(ledger.observed(now_ms)?, price)?;
        let balance = ledger.balance()?;
        transaction.execute(
            "INSERT INTO balances (tab_id,ledger) VALUES (?1,?2) ON CONFLICT(tab_id) DO UPDATE SET ledger=excluded.ledger",
            params![id.as_slice(),json(&ledger)?],
        ).map_err(|_| PayeeError::Storage)?;
        let hash = request.message.hash().map_err(|_| PayeeError::Exchange)?;
        let reply = tidvar_core::transport::Message {
            tab_id: request.message.tab_id,
            k: request
                .message
                .k
                .checked_add(1)
                .ok_or(PayeeError::Exchange)?,
            previous: Some(hash),
            body: body(&balance),
        };
        let encoded = json(&reply)?;
        let previous = reply.hash().map_err(|_| PayeeError::Exchange)?;
        transaction.execute(
            "INSERT INTO message_exchanges (tab_id,next_k,previous,last_request,reply) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(tab_id) DO UPDATE SET next_k=excluded.next_k,previous=excluded.previous,last_request=excluded.last_request,reply=excluded.reply",
            params![reply.tab_id.0.as_slice(),reply.k.checked_add(1).ok_or(PayeeError::Exchange)?,previous.0.as_slice(),hash.0.as_slice(),encoded],
        ).map_err(|_|PayeeError::Storage)?;
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(encoded)
    }
    fn change_ledger(
        &mut self,
        id: &[u8; 32],
        action: impl FnOnce(
            &mut tidvar_core::ledger::Ledger,
        ) -> Result<(), tidvar_core::params::AmountError>,
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        let tab = self.load(id)?.ok_or(PayeeError::Terms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        let row = runtime(&transaction, id)?;
        let mut ledger = ledger(row.as_ref())?;
        ledger.validate(tab.record.body.terms.funded)?;
        action(&mut ledger)?;
        let result = ledger.balance()?;
        transaction.execute(
            "INSERT INTO balances (tab_id,ledger) VALUES (?1,?2) ON CONFLICT(tab_id) DO UPDATE SET ledger=excluded.ledger",
            params![id.as_slice(),json(&ledger)?],
        ).map_err(|_| PayeeError::Storage)?;
        transaction
            .execute(
                "UPDATE tabs SET used_ms=?1 WHERE id=?2",
                params![result.vpn_ms, id.as_slice()],
            )
            .map_err(|_| PayeeError::Storage)?;
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(result)
    }
}

type ExchangeRow = (u64, Vec<u8>, Vec<u8>, Vec<u8>);
fn request_cursor(
    connection: &Connection,
    request: &tidvar_core::transport::Authenticated,
) -> Result<Option<Vec<u8>>, PayeeError> {
    let message = &request.message;
    let hash = message.hash().map_err(|_| PayeeError::Exchange)?;
    let previous: Option<ExchangeRow> = connection
        .query_row(
            "SELECT next_k,previous,last_request,reply FROM message_exchanges WHERE tab_id=?1",
            params![message.tab_id.0.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|_| PayeeError::Storage)?;
    if let Some((next_k, previous, last, reply)) = previous {
        if last == hash.0 {
            return Ok(Some(reply));
        }
        let previous: [u8; 32] = previous.try_into().map_err(|_| PayeeError::Storage)?;
        if !message.follows(
            message.tab_id,
            next_k,
            Some(tidvar_core::transport::Id(previous)),
        ) {
            return Err(PayeeError::Exchange);
        }
    } else if message.k != 1 || message.previous.is_some() {
        return Err(PayeeError::Exchange);
    }
    Ok(None)
}
impl Store {
    pub fn cached_reply(
        &self,
        request: &tidvar_core::transport::Authenticated,
    ) -> Result<Option<Vec<u8>>, PayeeError> {
        let tab = self
            .load(&request.message.tab_id.0)?
            .ok_or(PayeeError::Terms)?;
        request
            .verify(tab.record.body.payer_record_key)
            .map_err(|_| PayeeError::Exchange)?;
        request_cursor(&self.connection, request)
    }
    pub fn save_reply(
        &mut self,
        request: &tidvar_core::transport::Authenticated,
        body: serde_json::Value,
    ) -> Result<Vec<u8>, PayeeError> {
        if let Some(reply) = self.cached_reply(request)? {
            return Ok(reply);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        if let Some(reply) = request_cursor(&transaction, request)? {
            return Ok(reply);
        }
        let hash = request.message.hash().map_err(|_| PayeeError::Exchange)?;
        let reply = tidvar_core::transport::Message {
            tab_id: request.message.tab_id,
            k: request
                .message
                .k
                .checked_add(1)
                .ok_or(PayeeError::Exchange)?,
            previous: Some(hash),
            body,
        };
        let encoded = json(&reply)?;
        let previous = reply.hash().map_err(|_| PayeeError::Exchange)?;
        transaction.execute(
            "INSERT INTO message_exchanges (tab_id,next_k,previous,last_request,reply) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(tab_id) DO UPDATE SET next_k=excluded.next_k,previous=excluded.previous,last_request=excluded.last_request,reply=excluded.reply",
            params![reply.tab_id.0.as_slice(),reply.k.checked_add(1).ok_or(PayeeError::Exchange)?,previous.0.as_slice(),hash.0.as_slice(),encoded],
        ).map_err(|_|PayeeError::Storage)?;
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(encoded)
    }
    pub fn available_nonces<R: rand_core::RngCore + rand_core::CryptoRng>(
        &mut self,
        id: &[u8; 32],
        rng: &mut R,
    ) -> Result<Vec<tidvar_core::frost::Commitment>, PayeeError> {
        let mut statement=self.connection.prepare(
            "SELECT commitment FROM signing_nonces WHERE tab_id=?1 AND state='issued' AND message IS NULL AND intent IS NULL ORDER BY id LIMIT 32"
        ).map_err(|_|PayeeError::Storage)?;
        let encoded = statement
            .query_map(params![id.as_slice()], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|_| PayeeError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| PayeeError::Storage)?;
        drop(statement);
        let mut public = encoded
            .into_iter()
            .map(|bytes| {
                serde_json::from_slice::<tidvar_core::frost::Commitment>(&bytes)
                    .map_err(|_| PayeeError::Storage)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for commitment in &public {
            commitment.verify()?;
        }
        if public.len() < 32 {
            public.extend(self.issue_nonces(id, 32 - public.len(), None, rng)?);
        }
        public.sort_by_key(|commitment| commitment.id);
        Ok(public)
    }
    pub fn funding_snapshot(&self, id: &[u8; 32]) -> Result<Option<Vec<u8>>, PayeeError> {
        self.connection
            .query_row(
                "SELECT snapshot FROM funding WHERE tab_id=?1",
                params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| PayeeError::Storage)
    }
    pub fn save_funding_snapshot(
        &mut self,
        id: &[u8; 32],
        snapshot: &[u8],
    ) -> Result<(), PayeeError> {
        self.connection.execute("INSERT INTO funding (tab_id,snapshot) VALUES (?1,?2) ON CONFLICT(tab_id) DO UPDATE SET snapshot=excluded.snapshot",
            params![id.as_slice(),snapshot]).map_err(|_|PayeeError::Storage)?;
        Ok(())
    }
}

impl Store {
    pub fn payment_receipt(
        &self,
        id: &[u8; 32],
        hash: &[u8; 32],
    ) -> Result<Option<PaymentReceipt>, PayeeError> {
        if let Some(row) = runtime(&self.connection, id)?
            && row.2.as_deref() == Some(hash.as_slice())
        {
            return serde_json::from_slice(row.4.as_deref().ok_or(PayeeError::Storage)?)
                .map(Some)
                .map_err(|_| PayeeError::Storage);
        }
        Ok(None)
    }
}

#[derive(Clone, serde::Serialize)]
pub struct Peer {
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub tab_id: [u8; 32],
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub public_key: [u8; 32],
    pub slot: u32,
}
/// The second octet of this VPN's 10.x.0.0/16 network (77 unless `TIDVAR_VPN_NET` says otherwise), so a
/// mainnet and a testnet VPN can run side by side.
pub fn vpn_net() -> u8 {
    std::env::var("TIDVAR_VPN_NET")
        .ok()
        .and_then(|net| net.parse().ok())
        .unwrap_or(77)
}
impl Peer {
    pub fn address(&self) -> Result<std::net::Ipv4Addr, PayeeError> {
        let host = self
            .slot
            .checked_add(1)
            .filter(|host| *host < 65535)
            .ok_or(PayeeError::Terms)?;
        Ok(std::net::Ipv4Addr::new(
            10,
            vpn_net(),
            u8::try_from(host / 256).map_err(|_| PayeeError::Terms)?,
            u8::try_from(host % 256).map_err(|_| PayeeError::Terms)?,
        ))
    }
}
impl Store {
    pub fn peers(&self) -> Result<Vec<Peer>, PayeeError> {
        let mut statement = self
            .connection
            .prepare("SELECT tab_id,public_key,slot FROM peers ORDER BY slot")
            .map_err(|_| PayeeError::Storage)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            })
            .map_err(|_| PayeeError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| PayeeError::Storage)?;
        rows.into_iter()
            .map(|(id, key, slot)| {
                Ok(Peer {
                    tab_id: id.try_into().map_err(|_| PayeeError::Storage)?,
                    public_key: key.try_into().map_err(|_| PayeeError::Storage)?,
                    slot,
                })
            })
            .collect()
    }
    pub fn register_peer(
        &mut self,
        id: &[u8; 32],
        public_key: [u8; 32],
    ) -> Result<Peer, PayeeError> {
        self.load(id)?.ok_or(PayeeError::Terms)?;
        if public_key == [0; 32] {
            return Err(PayeeError::Terms);
        }
        let peers = self.peers()?;
        if let Some(peer) = peers.iter().find(|peer| peer.tab_id == *id) {
            if peer.public_key != public_key {
                return Err(PayeeError::Terms);
            }
            return Ok(peer.clone());
        }
        if peers.len() >= 50 {
            return Err(PayeeError::Capacity);
        }
        self.connection
            .execute(
                "INSERT INTO peers (tab_id,public_key) VALUES (?1,?2)",
                params![id.as_slice(), public_key.as_slice()],
            )
            .map_err(|_| PayeeError::Storage)?;
        self.peers()?
            .into_iter()
            .find(|peer| peer.tab_id == *id)
            .ok_or(PayeeError::Storage)
    }
    pub fn request_vpn_resume(
        &mut self,
        id: &[u8; 32],
        now_ms: u64,
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        self.change_ledger(id, |ledger| ledger.unpause(ledger.observed(now_ms)?))
    }
    pub fn recover_vpn(
        &mut self,
        id: &[u8; 32],
    ) -> Result<tidvar_core::ledger::Balance, PayeeError> {
        self.change_ledger(id, |ledger| {
            ledger.recover();
            Ok(())
        })
    }
}
