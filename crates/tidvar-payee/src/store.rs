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
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS tabs (
                id BLOB PRIMARY KEY CHECK(length(id)=32),
                record BLOB NOT NULL,
                share BLOB NOT NULL,
                public_package BLOB NOT NULL,
                viewing_key BLOB NOT NULL CHECK(length(viewing_key)=96),
                seal BLOB NOT NULL,
                paid INTEGER NOT NULL DEFAULT 0 CHECK(paid>=0),
                used_ms INTEGER NOT NULL DEFAULT 0 CHECK(used_ms>=0)
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
