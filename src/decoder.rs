//! [`UsdzDecoder`] — implements
//! [`Mesh3DDecoder`](oxideav_mesh3d::Mesh3DDecoder) for the USDZ
//! container.
//!
//! The decode pipeline is:
//!
//! 1. Walk the ZIP central directory ([`zip::walk`](crate::zip::walk))
//!    to enumerate entries with verified 64-byte alignment.
//! 2. Pick the Default Layer — the first entry whose extension is
//!    `.usd` / `.usda` / `.usdc` (a generic `.usd` dispatches on the
//!    header byte run per spec §16.1).
//! 3. Parse the layer — the ASCII text form via
//!    [`usda::parse`](crate::usda::parse), the binary Crate form via
//!    [`usdc_layer::layer_from_usdc`](crate::usdc_layer::layer_from_usdc).
//! 4. Translate the prim tree into a
//!    [`Scene3D`](oxideav_mesh3d::Scene3D) via
//!    [`usd_to_scene::translate`](crate::usd_to_scene::translate).

use std::sync::Arc;

use oxideav_mesh3d::{Mesh3DDecoder, Scene3D};

use crate::error::unsupported;
use crate::{usd_to_scene, usda, usdc_layer, zip, Result};

/// USDZ decoder. Constructed via [`UsdzDecoder::new`] (state is
/// reset on every `decode()` call so a single instance can be
/// reused across files).
#[derive(Debug, Default)]
pub struct UsdzDecoder {
    _private: (),
}

impl UsdzDecoder {
    /// Construct a fresh decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow-based decode that doesn't go through the trait — used
    /// internally by tests + by callers that want the raw archive
    /// path (e.g. holding the `Arc<Vec<u8>>` themselves).
    pub fn decode_bytes(&self, bytes: &[u8]) -> Result<Scene3D> {
        let entries = zip::walk(bytes)?;
        let default = pick_default_layer(&entries)?;
        let layer_name = default.name.clone();
        let extension = layer_name
            .rsplit('.')
            .next()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let archive = Arc::new(bytes.to_vec());
        let payload_start = default.payload_offset as usize;
        let payload_end = payload_start + default.payload_len as usize;
        let payload = &archive[payload_start..payload_end];
        // §16.1: the generic `.usd` extension is dispatched on the
        // header byte run — `PXR-USDC` magic selects the Crate binary
        // format, a `#usda` banner the text format — while `.usda` /
        // `.usdc` assert their format directly.
        let is_crate = match extension.as_str() {
            "usda" => false,
            "usdc" => true,
            "usd" => usdc_layer::is_usdc_magic(payload),
            other => {
                return Err(unsupported(format!(
                    "unrecognised USDZ default-layer extension `{other}` (expected .usd / .usda / .usdc)"
                )))
            }
        };
        let layer = if is_crate {
            usdc_layer::layer_from_usdc(payload)?
        } else {
            usda::parse(payload)?
        };
        usd_to_scene::translate_with_root(&layer, archive.clone(), &entries, Some(&layer_name))
    }
}

impl Mesh3DDecoder for UsdzDecoder {
    fn decode(&mut self, bytes: &[u8]) -> Result<Scene3D> {
        self.decode_bytes(bytes)
    }
}

/// Per the USDZ spec the Default Layer is the *first* entry. We
/// allow a small relaxation: the first entry whose extension is
/// one of `.usd` / `.usda` / `.usdc`. This tolerates packaging
/// tools that prepend a non-USD bookkeeping file but still want
/// the archive to be loadable as a USD asset.
pub fn pick_default_layer(entries: &[zip::ZipEntry]) -> Result<&zip::ZipEntry> {
    for entry in entries {
        let lower = entry.name.to_ascii_lowercase();
        if lower.ends_with(".usd") || lower.ends_with(".usda") || lower.ends_with(".usdc") {
            return Ok(entry);
        }
    }
    Err(crate::error::invalid(
        "USDZ archive contains no .usd / .usda / .usdc default layer",
    ))
}
