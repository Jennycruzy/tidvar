#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod sync;
pub mod transport;
use thiserror::Error;
pub use zcash_client_backend::proto::{compact_formats, service};

#[derive(Debug, Error)]
pub enum ChainError {
    #[error("The transaction has expired. Keep your tab and prepare a fresh transaction.")]
    Expired,
    #[error("The transaction was not found in the mempool or current chain.")]
    TransactionMissing,
    #[error("We can't reach the Zcash server right now. Retrying.")]
    Unavailable,
    #[error("The Zcash server returned data for a different network.")]
    Network,
    #[error("The Zcash server returned an invalid message.")]
    Message,
    #[error("The Zcash tree or note failed verification.")]
    Tree,
    #[error("The requested range is too large.")]
    Limit,
}
