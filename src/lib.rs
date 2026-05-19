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
//! Round 2 adds the **USDZ encoder**:
//!
//! * [`UsdzEncoder`] / [`Mesh3DEncoder`](oxideav_mesh3d::Mesh3DEncoder)
//!   serialises a `Scene3D` back into a USDZ archive. Output is
//!   STORED-only with the spec-mandated 64-byte payload alignment,
//!   and the Default Layer (USDA text) is always the first entry.
//! * Texture pass-through: when a texture's
//!   [`AssetSource::raw_storage`](oxideav_mesh3d::AssetSource::raw_storage)
//!   returns `RawStorage { scheme: "zip-stored", ... }` (which is
//!   exactly what a [`ZipStoredAsset`] from this crate's reader
//!   exposes), the encoder copies the inner-file bytes verbatim
//!   into the output archive. USDZ → USDZ pipelines never re-encode
//!   textures.
//!
//! Round 4 adds the **`UsdMediaSpatialAudio` reader + writer**:
//!
//! * `def SpatialAudio "<name>" { uniform asset filePath = @sound.wav@;
//!   uniform token auralMode = "spatial"; uniform double gain = ...; ... }`
//!   prims decode into `Node::audio_emitter`, populating an
//!   [`AudioEmitter`](oxideav_mesh3d::AudioEmitter) +
//!   [`AudioSource`](oxideav_mesh3d::AudioSource) on the scene.
//!   In-archive `filePath` references resolve through
//!   [`ZipStoredAsset`](crate::ZipStoredAsset) so the writer's
//!   pass-through optimisation extends to audio bytes; external
//!   paths land in
//!   [`AudioData::External`](oxideav_mesh3d::AudioData::External).
//! * The encoder serialises every `Node` carrying `audio_emitter`
//!   back into a `def SpatialAudio` block, embedding the audio
//!   asset bytes alongside textures into the output ZIP.
//!
//! Round 5 adds:
//!
//! * **Strips / fans / lines / points dispatch in the writer.**
//!   Non-`Triangles` primitives are tessellated (strips / fans →
//!   triangle list under `def Mesh`) or emitted as the matching
//!   UsdGeom prim type (`Lines` / `LineStrip` / `LineLoop` →
//!   `def BasisCurves`; `Points` → `def Points`). The original
//!   topology token round-trips on
//!   `Primitive::extras["usd:original_topology"]`.
//! * **`usd:no_fold` extras flag.** When a primitive carries
//!   `extras["usd:no_fold"] = true`, the writer marks the emitted
//!   `def Mesh` prim with the metadata flag so the decoder skips
//!   the round-3 sibling-fold heuristic for it. Mirrors the
//!   authoring-tool convention for intentional `Foo` / `Foo_1`
//!   sibling collisions.
//! * **Per-Mesh transform on the inner `def Mesh`.** A primitive
//!   carrying `extras["usd:mesh_transform"]` (JSON-shaped matrix
//!   or TRS) gets its `xformOp:*` opinions on the inner def Mesh
//!   instead of the parent Xform. Decoder symmetric.
//!
//! Round 6 adds:
//!
//! * **VariantSet structured parser.** `variantSet "name" = {
//!   "variantA" { ... } }` blocks land on
//!   `Prim::variant_sets: BTreeMap<String, BTreeMap<String, Variant>>`
//!   (parse-only; the Scene3D translator continues to ignore them
//!   pending a future composition engine).
//! * **Line + column in parser error messages.** Every `at offset N`
//!   diagnostic now reads `line L:C (offset N)`.
//!
//! Round 7 adds:
//!
//! * **Variant selection composition.** The Scene3D translator now
//!   evaluates `metadata["variants"] = { string SET = "VAR" }`
//!   selections against `Prim::variant_sets` before walking the
//!   prim tree. The matching variant's `attrs` and `children` are
//!   merged into the prim under LIVRPS strength order — Local
//!   opinions beat the variant's. So a `def Mesh` declared inside
//!   a selected variant materialises as a Scene3D Mesh + Node just
//!   like a directly-authored child would. See
//!   [`crate::usda::Prim::resolved_variants`] for the composition
//!   algorithm. Variant `references = @other.usd@` opinions are
//!   not followed (round 7 is single-layer); they're stashed under
//!   `usd:variantReferences:<set>:<var>:references` so the extras
//!   layer surfaces the unresolved arc to the caller.
//!
//! Round 8 adds:
//!
//! * **VariantSet writer round-trip.** The encoder now re-emits
//!   `variantSet "name" = { "variant" ( meta ) { body } }` blocks
//!   from the source layer (including unselected branches), the
//!   `prepend variantSets = [...]` declaration, and the
//!   `variants = { string SET = "VAR" }` selection metadata, so a
//!   USDZ → `Scene3D` → USDZ pipeline preserves every variant body
//!   regardless of which one was selected. The decoder stashes the
//!   structured form under
//!   `Node::extras["usd:variantSets"]` via the
//!   [`crate::variant_codec`] JSON encoding; the writer reads it
//!   back when synthesising the prim's metadata + body.
//!
//! Round 9 adds:
//!
//! * **Composition-arc + layer-metadata round-trip on the writer.**
//!   `defaultPrim`, `subLayers`, `customLayerData`, prim `kind`,
//!   `prepend references = @file@</P>` (AssetWithPath form),
//!   `prepend payload = @file@`, `prepend apiSchemas = [...]`, and
//!   every other layer- or prim-level `( ... )` metadata key now
//!   re-emit from the writer. The decoder stashes the source's
//!   metadata using the lossless tagged shape from
//!   [`crate::variant_codec`] under
//!   [`crate::usda_writer::LAYER_METADATA_EXTRAS_KEY`] /
//!   [`crate::usda_writer::PRIM_METADATA_EXTRAS_KEY`], so a
//!   USDZ → `Scene3D` → USDZ pipeline preserves each value's
//!   USDA type-token discriminant
//!   (`Token` vs `String` vs `Asset` vs `AssetWithPath` vs `Path`).
//!   The list-edit operator (`prepend`) is auto-prefixed on
//!   composition-arc keys per OpenUSD authoring convention.
//!
//! Round 10 adds:
//!
//! * **Anchored in-archive sublayer composition.** When the Default
//!   Layer declares `subLayers = [@./geom.usda@, ...]` and those
//!   entries exist in the surrounding USDZ archive, the decoder now
//!   composes each sublayer's prim tree underneath the local
//!   layer's per the OpenUSD glossary's LayerStack definition: "the
//!   recursive gathering of all SubLayers of a Layer, plus the
//!   layer itself as first and strongest". Local opinions beat
//!   sublayer opinions; `over` + `def` of the same prim name fuse
//!   into a single `def`. `.usdc` sublayer entries surface as
//!   [`Error::Unsupported`](crate::Error) consistent with the
//!   round-1 Default-Layer policy; external / cross-package
//!   sublayer paths are silently skipped from composition. The
//!   audit trail rides on
//!   `Scene3D::extras["usd:composedSubLayers"]`.
//!
//! Deferred to round 11+:
//!
//! * Binary `.usdc` "Crate" parser — Pixar publishes no prose spec
//!   for the wire format (documented gap in `docs/3d/usd/README.md`);
//!   loading a `.usdc` Default Layer surfaces as `Error::Unsupported`.
//! * `UsdSkelSkeleton` + `UsdSkelBindingAPI` skinning — UsdSkel
//!   schema docs are not in our `docs/3d/usd/` (the spec README
//!   notes UsdSkel sits behind a per-schema URL pattern not yet
//!   staged).
//! * `UsdGeomSubset` per-face material binding subsets — same
//!   schema-doc gap as UsdSkel.
//! * Cross-package composition arcs and `references` / `payloads`
//!   arc resolution — round 10 handles intra-archive `subLayers`
//!   only; `@foo.usdz[path/within.usd]@` references and
//!   `references` / `payloads` that target a single prim inside
//!   another file stay as side-channel opinions on
//!   `scene.extras["usd:layerMetadata"]`.
//! * Sublayer round-trip on the writer — round 10 evaluates
//!   sublayers on the read path; the encoder flattens the composed
//!   tree back into a single layer rather than preserving the
//!   multi-file LayerStack structure on output.
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
pub mod encoder;
pub mod error;
pub mod usd_to_scene;
pub mod usda;
pub mod usda_writer;
pub mod variant_codec;
pub mod zip;
pub mod zip_writer;

pub use asset_source::ZipStoredAsset;
pub use decoder::UsdzDecoder;
pub use encoder::{EncodeReport, UsdzEncoder};
pub use error::{Error, Result};

/// Register both the USDZ decoder and encoder against `registry` so
/// callers can dispatch by extension (`.usdz`).
///
/// Only available with the default-on `registry` cargo feature.
#[cfg(feature = "registry")]
pub fn register(registry: &mut oxideav_mesh3d::Mesh3DRegistry) {
    registry.register_decoder("usdz", &["usdz"], Box::new(|| Box::new(UsdzDecoder::new())));
    registry.register_encoder("usdz", &["usdz"], Box::new(|| Box::new(UsdzEncoder::new())));
}
