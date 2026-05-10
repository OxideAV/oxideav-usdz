//! Error type for `oxideav-usdz`.
//!
//! Re-exports `oxideav_mesh3d::{Error, Result}` so the decoder's
//! return type lines up with whatever the consumer's `oxideav-mesh3d`
//! is using (the framework `oxideav_core::Error` under the
//! `registry` feature, the crate-local enum without it).

pub use oxideav_mesh3d::{Error, Result};

/// Build an `InvalidData` error with the given message.
pub fn invalid(msg: impl Into<String>) -> Error {
    Error::invalid(msg.into())
}

/// Build an `Unsupported` error with the given message.
pub fn unsupported(msg: impl Into<String>) -> Error {
    Error::unsupported(msg.into())
}
