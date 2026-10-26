use crate::{PayeeError, opening::Accepted};
use rusqlite::{Connection, OptionalExtension, params};
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
use zcash_protocol::consensus::NetworkType;
use zeroize::Zeroizing;

pub struct Store {
    connection: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self, PayeeError> {
        if fs::symlink_metadata(path).is_ok_and(|info| info.file_type().is_symlink()) {
            return Err(PayeeError::Storage);
        }
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
            );",
            )
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
            .transaction()
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
                let keys =
                    Keys::restore(Role::Service, &share, &public, &viewing, NetworkType::Test)?;
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
            .transaction()
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
            .transaction()
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
            .transaction()
            .map_err(|_| PayeeError::Storage)?;
        for nonce in ids {
            transaction.execute("UPDATE signing_nonces SET state='discarded',secret=NULL WHERE id=?1 AND tab_id=?2 AND state='issued'",params![nonce.as_slice(),id.as_slice()]).map_err(|_|PayeeError::Storage)?;
        }
        transaction.commit().map_err(|_| PayeeError::Storage)
    }
}
