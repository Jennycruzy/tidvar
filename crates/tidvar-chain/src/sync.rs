use crate::{
    ChainError,
    compact_formats::{CompactBlock, CompactOrchardAction},
    service::TreeState,
    transport,
};
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Address, Note,
    keys::{FullViewingKey, Scope},
    note::{ExtractedNoteCommitment, NoteVersion, Nullifier, RandomSeed, Rho},
    note_encryption::{CompactAction, IronwoodDomain},
    tree::MerkleHashOrchard,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use zcash_note_encryption::{EphemeralKeyBytes, try_compact_note_decryption};
use zcash_primitives::merkle_tree::{
    read_commitment_tree, read_incremental_witness, write_commitment_tree,
    write_incremental_witness,
};

type Tree = CommitmentTree<MerkleHashOrchard, 32>;
type Witness = IncrementalWitness<MerkleHashOrchard, 32>;

#[derive(Clone)]
pub struct OwnedNote {
    pub note: Note,
    pub witness: Witness,
    pub txid: [u8; 32],
    pub height: u64,
    pub spent: bool,
}

#[derive(Serialize, Deserialize)]
pub struct NoteSnapshot {
    #[serde(with = "tidvar_core::encoding::bytes")]
    pub recipient: Vec<u8>,
    pub value: u64,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub rho: [u8; 32],
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub rseed: [u8; 32],
    pub version: u8,
    #[serde(with = "tidvar_core::encoding::bytes")]
    pub witness: Vec<u8>,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub txid: [u8; 32],
    pub height: u64,
    pub spent: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(with = "tidvar_core::encoding::bytes")]
    pub viewing_key: Vec<u8>,
    #[serde(with = "tidvar_core::encoding::bytes")]
    pub tree: Vec<u8>,
    pub height: u64,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub block_hash: [u8; 32],
    pub notes: Vec<NoteSnapshot>,
    pub additional_notes: u64,
}

#[derive(Clone)]
pub struct SyncState {
    viewing: FullViewingKey,
    tree: Tree,
    pub height: u64,
    block_hash: [u8; 32],
    pub notes: Vec<OwnedNote>,
    pub additional_notes: u64,
}

fn fixed<const N: usize>(bytes: &[u8]) -> Result<[u8; N], ChainError> {
    bytes.try_into().map_err(|_| ChainError::Message)
}
fn tree(bytes: &[u8]) -> Result<Tree, ChainError> {
    let mut cursor = Cursor::new(bytes);
    let result = read_commitment_tree(&mut cursor).map_err(|_| ChainError::Tree)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(ChainError::Tree);
    }
    Ok(result)
}
pub fn compact(value: &CompactOrchardAction) -> Result<CompactAction, ChainError> {
    let nf = Option::<Nullifier>::from(Nullifier::from_bytes(&fixed(&value.nullifier)?))
        .ok_or(ChainError::Message)?;
    let cmx = Option::<ExtractedNoteCommitment>::from(ExtractedNoteCommitment::from_bytes(&fixed(
        &value.cmx,
    )?))
    .ok_or(ChainError::Message)?;
    Ok(CompactAction::from_parts(
        nf,
        cmx,
        EphemeralKeyBytes(fixed(&value.ephemeral_key)?),
        fixed(&value.ciphertext)?,
    ))
}

impl SyncState {
    pub fn new(viewing_key: &[u8], before: TreeState) -> Result<Self, ChainError> {
        if before.network != "test" {
            return Err(ChainError::Network);
        }
        let viewing =
            FullViewingKey::from_bytes(&fixed(viewing_key)?).ok_or(ChainError::Message)?;
        let tree = tree(&hex::decode(before.ironwood_tree).map_err(|_| ChainError::Message)?)?;
        let mut hash = hex::decode(before.hash).map_err(|_| ChainError::Message)?;
        hash.reverse();
        Ok(Self {
            viewing,
            tree,
            height: before.height,
            block_hash: fixed(&hash)?,
            notes: Vec::new(),
            additional_notes: 0,
        })
    }

    pub fn viewing_key(&self) -> &FullViewingKey {
        &self.viewing
    }
    pub fn root(&self) -> [u8; 32] {
        self.tree.root().to_bytes()
    }
    pub fn value(&self) -> Result<u64, ChainError> {
        self.notes
            .iter()
            .filter(|n| !n.spent)
            .try_fold(0u64, |sum, n| {
                sum.checked_add(n.note.value().inner())
                    .ok_or(ChainError::Limit)
            })
    }

    pub fn apply(&mut self, block: CompactBlock) -> Result<(), ChainError> {
        let mut next = self.clone();
        next.apply_inner(block)?;
        *self = next;
        Ok(())
    }

    fn apply_inner(&mut self, block: CompactBlock) -> Result<(), ChainError> {
        if block.height != self.height.checked_add(1).ok_or(ChainError::Limit)?
            || block.prev_hash != self.block_hash
        {
            return Err(ChainError::Tree);
        }
        let next_hash = fixed(&block.hash)?;
        let recipient = self.viewing.address_at(0u32, Scope::External);
        let incoming = self.viewing.to_ivk(Scope::External).prepare();
        for transaction in block.vtx {
            let txid = fixed(&transaction.txid)?;
            for action in transaction.ironwood_actions {
                let action = compact(&action)?;
                for owned in &mut self.notes {
                    if owned.note.nullifier(&self.viewing) == action.nullifier() {
                        owned.spent = true;
                    }
                }
                let leaf = MerkleHashOrchard::from_cmx(&action.cmx());
                self.tree.append(leaf).map_err(|_| ChainError::Tree)?;
                for owned in &mut self.notes {
                    owned.witness.append(leaf).map_err(|_| ChainError::Tree)?;
                }
                if let Some((note, address)) = try_compact_note_decryption(
                    &IronwoodDomain::for_compact_action(&action),
                    &incoming,
                    &action,
                ) && address == recipient
                {
                    tidvar_core::params::Amount::try_from(note.value().inner())
                        .map_err(|_| ChainError::Message)?;
                    if self.notes.len() < 4 {
                        let witness =
                            Witness::from_tree(self.tree.clone()).ok_or(ChainError::Tree)?;
                        self.notes.push(OwnedNote {
                            note,
                            witness,
                            txid,
                            height: block.height,
                            spent: false,
                        });
                    } else {
                        self.additional_notes = self
                            .additional_notes
                            .checked_add(1)
                            .ok_or(ChainError::Limit)?;
                    }
                }
            }
        }
        self.height = block.height;
        self.block_hash = next_hash;
        Ok(())
    }

    pub fn apply_web_range(&mut self, bytes: &[u8]) -> Result<(), ChainError> {
        let frames = transport::decode_frames(bytes)?;
        if frames.len() > transport::MAX_RANGE_BLOCKS as usize {
            return Err(ChainError::Limit);
        }
        let mut next = self.clone();
        for frame in frames {
            next.apply_inner(
                CompactBlock::decode(frame.as_slice()).map_err(|_| ChainError::Message)?,
            )?;
        }
        *self = next;
        Ok(())
    }

    pub fn verify_tip(&self, state: &TreeState) -> Result<(), ChainError> {
        if state.network != "test" || state.height != self.height {
            return Err(ChainError::Network);
        }
        let expected = tree(&hex::decode(&state.ironwood_tree).map_err(|_| ChainError::Message)?)?;
        if expected.root() != self.tree.root() || expected.size() != self.tree.size() {
            return Err(ChainError::Tree);
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Snapshot, ChainError> {
        let mut tree = Vec::new();
        write_commitment_tree(&self.tree, &mut tree).map_err(|_| ChainError::Tree)?;
        let notes = self
            .notes
            .iter()
            .map(|owned| {
                let mut witness = Vec::new();
                write_incremental_witness(&owned.witness, &mut witness)
                    .map_err(|_| ChainError::Tree)?;
                Ok(NoteSnapshot {
                    recipient: owned.note.recipient().to_raw_address_bytes().to_vec(),
                    value: owned.note.value().inner(),
                    rho: owned.note.rho().to_bytes(),
                    rseed: *owned.note.rseed().as_bytes(),
                    version: match owned.note.version() {
                        NoteVersion::V2 => 2,
                        NoteVersion::V3 => 3,
                    },
                    witness,
                    txid: owned.txid,
                    height: owned.height,
                    spent: owned.spent,
                })
            })
            .collect::<Result<_, ChainError>>()?;
        Ok(Snapshot {
            viewing_key: self.viewing.to_bytes().to_vec(),
            tree,
            height: self.height,
            block_hash: self.block_hash,
            notes,
            additional_notes: self.additional_notes,
        })
    }

    pub fn restore(snapshot: Snapshot) -> Result<Self, ChainError> {
        if snapshot.notes.len() > 4 {
            return Err(ChainError::Limit);
        }
        let viewing = FullViewingKey::from_bytes(&fixed(&snapshot.viewing_key)?)
            .ok_or(ChainError::Message)?;
        let expected = viewing.address_at(0u32, Scope::External);
        let tree = tree(&snapshot.tree)?;
        let mut notes = Vec::new();
        for stored in snapshot.notes {
            let recipient = Option::<Address>::from(Address::from_raw_address_bytes(&fixed(
                &stored.recipient,
            )?))
            .ok_or(ChainError::Message)?;
            if recipient != expected {
                return Err(ChainError::Tree);
            }
            tidvar_core::params::Amount::try_from(stored.value).map_err(|_| ChainError::Message)?;
            let rho =
                Option::<Rho>::from(Rho::from_bytes(&stored.rho)).ok_or(ChainError::Message)?;
            let seed = Option::<RandomSeed>::from(RandomSeed::from_bytes(stored.rseed, &rho))
                .ok_or(ChainError::Message)?;
            let version = match stored.version {
                2 => NoteVersion::V2,
                3 => NoteVersion::V3,
                _ => return Err(ChainError::Message),
            };
            let note = Option::<Note>::from(Note::from_parts(
                recipient,
                orchard::value::NoteValue::from_raw(stored.value),
                rho,
                seed,
                version,
            ))
            .ok_or(ChainError::Message)?;
            let mut cursor = Cursor::new(&stored.witness);
            let witness: Witness =
                read_incremental_witness(&mut cursor).map_err(|_| ChainError::Tree)?;
            if cursor.position() != stored.witness.len() as u64
                || witness.root() != tree.root()
                || stored.height > snapshot.height
            {
                return Err(ChainError::Tree);
            }
            let path = witness.path().ok_or(ChainError::Tree)?;
            if path.root(MerkleHashOrchard::from_cmx(&note.commitment().into())) != tree.root() {
                return Err(ChainError::Tree);
            }
            notes.push(OwnedNote {
                note,
                witness,
                txid: stored.txid,
                height: stored.height,
                spent: stored.spent,
            });
        }
        Ok(Self {
            viewing,
            tree,
            height: snapshot.height,
            block_hash: snapshot.block_hash,
            notes,
            additional_notes: snapshot.additional_notes,
        })
    }
}
