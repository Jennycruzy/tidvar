#![forbid(unsafe_code)]
pub mod opening;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum PayeeError {
    #[error("The VPN usage settings or counters could not be verified. Your prepaid time is kept.")]
    Usage,
    #[error("The VPN is full. Keep your prepaid time and try again later.")]
    Capacity,
    #[error("This request does not follow the saved exchange. Retry the same request.")]
    Exchange,
    #[error(transparent)]
    Transaction(#[from] tidvar_core::iou::IouError),
    #[error(transparent)]
    Amount(#[from] tidvar_core::params::AmountError),
    #[error(transparent)]
    Signing(#[from] tidvar_core::frost::SigningError),
    #[error("This signing value has already been assigned to another transaction.")]
    Nonce,
    #[error("The payment proof failed verification. No payment was accepted.")]
    Proof,
    #[error("The tab terms changed during setup. No money moved.")]
    Terms,
    #[error("The tab could not be saved. Do not send money yet.")]
    Storage,
    #[error(transparent)]
    Keys(#[from] tidvar_core::keys::KeyError),
    #[error(transparent)]
    Record(#[from] tidvar_core::record::RecordError),
    #[error(transparent)]
    Seal(#[from] tidvar_seal::SealError),
}

#[cfg(not(target_arch = "wasm32"))]
pub mod proof;

#[cfg(not(target_arch = "wasm32"))]
pub mod payments;

#[cfg(not(target_arch = "wasm32"))]
pub mod usage;
