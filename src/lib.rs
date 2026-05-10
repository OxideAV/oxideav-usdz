//! Pure-Rust USDZ (Universal Scene Description, zipped) reader.
//!
//! USDZ is Pixar's container for ship-ready USD assets — an
//! uncompressed ZIP archive whose first entry is a `.usd` / `.usda`
//! / `.usdc` "Default Layer" referenced by every other file in the
//! package (textures, audio, sub-layers). Apple Quick Look on iOS /
//! macOS uses USDZ as its native AR scene format.
//!
//! Round 1 implements:
//!
//! * A minimal PKZIP central-directory walker (STORED-only entries
//!   per the USDZ spec — no compression allowed in the wire format)
//!   that enforces the 64-byte alignment requirement on every
//!   inner-file payload.
//! * An ASCII USDA tokenizer + prim-tree parser covering the prim
//!   shapes the round-1 Scene3D mapping needs: `Xform`, `Mesh`,
//!   `Scope`, `Material`, `Shader`. Only the
//!   `(prim_path, prim_def)` view consumed by `usd_to_scene` is
//!   exposed.
//! * USD → [`Scene3D`](oxideav_mesh3d::Scene3D) translation:
//!   - `UsdGeomMesh` → `Mesh + Primitive { topology: Triangles, ... }`,
//!     fan-triangulating any non-triangle face arities described
//!     by `faceVertexCounts`.
//!   - `UsdShade` `info:id == "UsdPreviewSurface"` → `Material`
//!     with PBR slots populated.
//!   - `info:id == "UsdUVTexture"` → `Texture` whose `ImageData`
//!     wraps a [`ZipStoredAsset`](crate::asset_source::ZipStoredAsset)
//!     so the inner ZIP-stored bytes can later pass through into a
//!     USDZ → USDZ writer without re-compression.
//!   - `upAxis = "Y" | "Z"` and `metersPerUnit` map onto
//!     [`Scene3D::up_axis`] + [`Scene3D::unit`].
//!
//! Deferred to round 2+:
//!
//! * USDZ encoder (writing back out, with raw_storage("zip-stored")
//!   pass-through optimisation when the scene was loaded from USDZ).
//! * Binary `.usdc` "Crate" parser — Pixar publishes no prose spec
//!   for the wire format; loading a `.usdc` Default Layer in r1
//!   surfaces as `Error::Unsupported`.
//! * `UsdSkelSkeleton` + `UsdSkelBindingAPI` skinning.
//! * `UsdMediaSpatialAudio` → `AudioSource` + `AudioEmitter`.
//! * `UsdGeomSubset` per-face material binding subsets.
//! * Composition arcs (sub-layers, references, payloads) that pull
//!   in external USD files; r1 handles a single self-contained USD
//!   layer per archive only.
//!
//! ## Standalone build
//!
//! `oxideav-core` is gated behind the default-on `registry` cargo
//! feature. Drop the framework dependency tree entirely with:
//!
//! ```toml
//! oxideav-usdz = { version = "0.0", default-features = false }
//! ```
//!
//! The decoder still parses USDZ into the same
//! [`Scene3D`](oxideav_mesh3d::Scene3D) typed model — only the
//! `register()` glue and the `Error` alias change.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod asset_source;
pub mod decoder;
pub mod error;
pub mod usd_to_scene;
pub mod usda;
pub mod zip;

pub use asset_source::ZipStoredAsset;
pub use decoder::UsdzDecoder;
pub use error::{Error, Result};

/// Register the USDZ decoder against `registry` so callers can
/// dispatch by extension (`.usdz`).
///
/// Only available with the default-on `registry` cargo feature.
#[cfg(feature = "registry")]
pub fn register(registry: &mut oxideav_mesh3d::Mesh3DRegistry) {
    registry.register_decoder("usdz", &["usdz"], Box::new(|| Box::new(UsdzDecoder::new())));
}
