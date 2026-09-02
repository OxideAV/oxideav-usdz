//! [`ZipStoredAsset`] — `AssetSource` impl that points at an
//! inner-file slice of a USDZ archive.
//!
//! Why the type exists: USDZ entries are STORED (uncompressed) by
//! spec, so a future USDZ writer can copy the bytes verbatim from
//! the input archive into the output archive without re-deflating
//! or even re-buffering. The pass-through path is the
//! `oxideav_mesh3d::AssetSource::raw_storage()` hook, which
//! [`ZipStoredAsset`] implements with `scheme = "zip-stored"` per
//! the conventions documented in `oxideav_mesh3d::asset`.
//!
//! Construction is `Arc`-based: the entire archive bytes get
//! wrapped in `Arc<Vec<u8>>` once (or backed by mmap; either works
//! given this type only takes `Arc<Vec<u8>>` for r1) and every
//! [`ZipStoredAsset`] in the resulting `Scene3D` shares that one
//! buffer. No texture is copied into a per-asset `Vec` until a
//! consumer actually opens it.

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use oxideav_mesh3d::asset::{AssetSource, RawStorage, ReadSeek};

/// Lazy reference into a slice of a USDZ archive.
///
/// `archive` is the entire ZIP bytes (Arc'd so multiple
/// `AssetSource`s can share without copying); `offset` and `length`
/// pin the inner-file payload.
pub struct ZipStoredAsset {
    /// Reference to the entire USDZ archive bytes.
    pub archive: Arc<Vec<u8>>,
    /// Offset into archive where this inner file's data begins
    /// (already past the local file header — points at raw stored
    /// bytes).
    pub offset: u64,
    /// Length of inner-file data in bytes.
    pub length: u64,
    /// MIME hint inferred from the inner filename's extension
    /// (e.g. `image/png`, `image/jpeg`, `audio/mp4`).
    pub mime: Option<String>,
}

impl ZipStoredAsset {
    /// Construct from an Arc'd archive buffer + payload window.
    ///
    /// `mime` should be a best-effort MIME inferred from the inner
    /// filename's extension; pass `None` when the extension is
    /// unrecognised and let the consumer sniff.
    pub fn new(archive: Arc<Vec<u8>>, offset: u64, length: u64, mime: Option<String>) -> Self {
        Self {
            archive,
            offset,
            length,
            mime,
        }
    }

    /// Borrow the raw inner-file bytes.
    pub fn bytes(&self) -> &[u8] {
        let start = self.offset as usize;
        let end = start + self.length as usize;
        &self.archive[start..end]
    }
}

impl fmt::Debug for ZipStoredAsset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZipStoredAsset")
            .field("offset", &self.offset)
            .field("length", &self.length)
            .field("mime", &self.mime)
            .finish()
    }
}

impl AssetSource for ZipStoredAsset {
    fn mime(&self) -> Option<&str> {
        self.mime.as_deref()
    }

    fn size_hint(&self) -> Option<u64> {
        Some(self.length)
    }

    fn open(&self) -> std::io::Result<Box<dyn ReadSeek + Send>> {
        // Cursor over an owned copy of the inner-file bytes — the
        // `ReadSeek` trait wants an owning, positionable reader and
        // the trait contract gives every caller their own cursor,
        // so two concurrent `open()`s must each get an independent
        // one. The pass-through optimization for USDZ→USDZ goes
        // through `raw_storage()` instead and avoids this clone
        // entirely.
        Ok(Box::new(Cursor::new(self.bytes().to_vec())))
    }

    fn raw_storage(&self) -> Option<RawStorage<'_>> {
        Some(RawStorage {
            scheme: "zip-stored",
            bytes: self.bytes(),
            uncompressed_size: Some(self.length),
        })
    }
}

/// Best-effort MIME guess from a filename's extension. Returns
/// `None` when nothing is recognised; the consumer can then fall
/// back to magic-byte sniffing.
pub fn mime_from_filename(name: &str) -> Option<String> {
    // `pkg.usdz[inner/tex.png]` — the extension is the inner file's.
    let name = match name.rsplit_once('[') {
        Some((_, inner)) => inner.strip_suffix(']').unwrap_or(inner),
        None => name,
    };
    let dot = name.rfind('.')?;
    let ext = &name[dot + 1..];
    let lc = ext.to_ascii_lowercase();
    let mime = match lc.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "exr" => "image/x-exr",
        "hdr" => "image/vnd.radiance",
        "tif" | "tiff" => "image/tiff",
        "ktx" | "ktx2" => "image/ktx2",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "usd" | "usda" => "model/vnd.usda",
        "usdc" => "model/vnd.usdc",
        _ => return None,
    };
    Some(mime.to_owned())
}
