//! Pure-Rust USDZ (Universal Scene Description, zipped) reader.
//!
//! USDZ is USD container for ship-ready USD assets — an
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
//! Round 11 adds:
//!
//! * **In-archive `references` / `payload` arc composition.** A prim
//!   carrying `references = @asset.usd@` / `payload = @asset.usd@`
//!   (optionally with a `</Prim>` selector) that targets an
//!   in-archive `.usd` / `.usda` layer now has the targeted prim
//!   composed underneath it per the OpenUSD glossary's References
//!   definition: the referencing prim keeps its own name (the
//!   "prim name-change" the glossary's `MarbleCollection` example
//!   describes), while the target's `type_name`, `kind`, children,
//!   and attrs ride up. Local opinions beat referenced opinions, and
//!   among arcs `references` beats `payload` with earlier list
//!   entries stronger than later ones (USD list order = strength
//!   order). A bare `references = @file@` targets the referenced
//!   layer's `defaultPrim`; `@file@</Prim>` targets the named prim.
//!   Nested references compose transitively; cycles are broken by a
//!   `(layer, prim-path)` visited guard. `.usdc` targets surface as
//!   [`Error::Unsupported`](crate::Error) (binary-Crate gap);
//!   external / cross-package / non-`.usd[a]` targets stay authored
//!   so the writer round-trips them. Consumed in-archive arcs are
//!   stripped so the writer flattens the composed tree (matching the
//!   round-10 sublayer flatten-on-write policy). The audit trail
//!   rides on `Scene3D::extras["usd:composedReferences"]`. See
//!   [`crate::usd_to_scene`]'s `compose_references`.
//!
//! Round 12 — `defaultPrim` writer consistency:
//!
//! * **Token tracks prim-name sanitisation.** The writer rewrites a
//!   preserved `defaultPrim = "My Cube"` to `"My_Cube"` so it still
//!   names the actual emitted prim (`def Xform "My_Cube"`). Without
//!   the rewrite a selector-less `references = @./scene.usda@`
//!   resolving to `defaultPrim` would silently miss its target.
//! * **Dangling opinions are dropped.** A preserved `defaultPrim`
//!   that names no surviving root is omitted rather than re-emitted
//!   verbatim (strict USD validators reject dangling defaults).
//! * **Synthesised when absent.** A `Scene3D` carrying no
//!   `usd:layerMetadata` now emits a `defaultPrim` opinion naming
//!   the first root's sanitised prim name; without it, downstream
//!   selector-less references against our output produce nothing.
//!   Rootless scenes still emit no opinion.
//!
//! Round 13 — in-layer `inherits` / `specializes` class-hierarchy
//! arc composition:
//!
//! * **In-layer class-arc composition.** A prim carrying `inherits
//!   = </Class/Foo>` or `specializes = </Class/Foo>` (or a list of
//!   such paths) now has the targeted in-layer prim composed
//!   underneath it per the OpenUSD glossary's Inherits / Specializes
//!   definitions. Class arcs target a prim path *within the same
//!   layer* (distinct from `references` / `payload` which target
//!   external asset paths), so the composer walks the local layer's
//!   prim tree rather than another archive entry. The target's
//!   `type_name`, `kind`, children, and attrs ride up via the same
//!   `merge_prim_under` primitive that powers rounds 10 / 11 —
//!   local opinions win; earlier list entries beat later ones.
//! * **LIVRPS-aligned strength order.** L > I > V > R > P > S, so
//!   `inherits` composes BEFORE `references` / `payload` and
//!   `specializes` composes AFTER them; the read-pass call order in
//!   [`crate::usd_to_scene::translate`] reflects this directly.
//! * **Transitive composition + cycle break.** A class prim that
//!   itself carries class arcs has its own arc composed before
//!   merging upward; a `(prim-path)` visited set scoped per-recursion
//!   breaks reference cycles.
//! * **Flatten-on-write.** Consumed in-layer arcs are stripped so
//!   the writer flattens the composed tree into a single layer,
//!   matching the rounds 10 / 11 sublayer + reference policy.
//!   Unresolved arcs (paths that don't name any local prim) stay
//!   authored so the writer round-trips them verbatim. The audit
//!   trail lands on `Scene3D::extras["usd:composedInherits"]` and
//!   `["usd:composedSpecializes"]`. See
//!   [`crate::usd_to_scene`]'s `compose_class_arcs`.
//!
//! Round 14 — CRC-32 integrity verification on the reader:
//!
//! * **STORED payloads are verified against the central-directory
//!   CRC-32.** USDZ is a PKZIP archive (PKWARE APPNOTE.TXT
//!   container); every entry carries a CRC-32. The writer always
//!   emitted a correct one, but the reader never checked it. The ZIP
//!   walker now recomputes the CRC-32/ISO-HDLC of each STORED payload
//!   and rejects a mismatch with [`Error::InvalidData`](crate::Error),
//!   catching a corrupted byte at the container boundary instead of
//!   downstream as a USDA parse failure or a garbled texture. Reuses
//!   the crate's own [`crate::zip_writer::crc32`] so reader and writer
//!   can't drift. The CRC field is plain PKWARE structure, not
//!   USD-specific.
//!
//! Round 15 — USDC ("Crate") binary-format primitives:
//!
//! * [`usdc`] module — bootstrap-header + Table-of-Contents
//!   primitives for the binary `.usdc` sibling of the USDA text
//!   format ([`usdc::Magic`], [`usdc::Version`],
//!   [`usdc::Bootstrap`], [`usdc::TocEntry`], [`usdc::SectionName`],
//!   [`usdc::Toc`], [`usdc::UsdcFile::parse`]). Sourced exclusively
//!   from `docs/3d/usd/usdc-crate-format-trace.md`, the project's
//!   own clean-room byte-level trace of real `.usdc` samples; cross-
//!   validated against the trace doc's Elephant fixture (v0.8.0,
//!   six sections in `TOKENS / STRINGS / FIELDS / FIELDSETS /
//!   PATHS / SPECS` order, TOC at file offset `0x000cfc9a`).
//! * **Decoder boundary check.** The USDZ decoder now parses the
//!   bootstrap + TOC of a `.usdc` Default Layer before surfacing
//!   `Error::Unsupported` — malformed `.usdc` payloads (truncated
//!   bootstrap, wrong magic, oversized TOC, section running into
//!   the TOC, …) are caught at the container boundary as
//!   `Error::InvalidData` instead of leaking past
//!   layer-extension dispatch. The Unsupported message records the
//!   parsed version + section catalogue so callers can see how far
//!   the boundary check got.
//!
//! Round 206 — USDC §3b compressed-integer decoder:
//!
//! * [`usdc::decode_int_array`] decodes the §3b "compressed
//!   integer" wire shape: a 2-bit-per-element control stream
//!   (`ceil(N/4)` bytes, LSB-first) selecting one of four widths
//!   per element (`0` = repeat previous, `1` = `i8` delta, `2` =
//!   `i16` delta, `3` = absolute `i32`), followed by
//!   variable-width payload bytes. Used by the index arrays
//!   inside FIELDS / FIELDSETS / SPECS once the §3a LZ4 wrapper
//!   lands. Sourced exclusively from `docs/3d/usd/usdc-crate-format-trace.md`
//!   §3b.
//!
//! Round 217 — USDC §4.2 STRINGS section parser:
//!
//! * [`usdc::StringsHeader`] / [`usdc::StringsSection`] — parse
//!   the §4.2 STRINGS section: an 8-byte little-endian `int64`
//!   count followed by `count * 4` bytes of raw (NOT
//!   LZ4-compressed) little-endian `uint32` token indices. The
//!   STRINGS pool is the subset of TOKENS atoms used as
//!   USDA *string-typed* values, and §4.3 FIELDS string-valued
//!   reps index into it. [`usdc::StringsSection::parse_indices`]
//!   materialises the wire `uint32` array as a `Vec<u32>`.
//!   Defensive cap on `count` (16 Mi) plus a strict
//!   `8 + count * 4 == section size` check rejects trailing
//!   bytes the trace doesn't authorise. Cross-validated against
//!   the Elephant fixture (`count = 0`, section size = 8).
//!
//! Round 229 — USDC §4.4 FIELDSETS section framing parser:
//!
//! * [`usdc::FieldSetsHeader`] / [`usdc::FieldSetsSection`] — parse
//!   the §4.4 FIELDSETS section's outer framing: an 8-byte
//!   little-endian `int64 count` header followed by a single
//!   `(int64 compressedSize, §3a buffer)` pair. The decompressed
//!   buffer, once the LZ4 block decoder lands, feeds `count` `i32`
//!   values through the §3b integer decoder; the trace doc records
//!   that the array is the concatenation of per-set field-index
//!   runs separated by a `-1` (`0xFFFFFFFF`) sentinel — each spec
//!   (§4.6) references one such set to get its full property list.
//!   [`usdc::FieldSetsSection::buffer`] forwards to
//!   [`usdc::CompressedBuffer::parse`] on the bounded buffer slice
//!   ready for §3a / §3b chained decoding once the LZ4 block
//!   decoder lands. Defensive caps on `count` (16 Mi) and on the
//!   buffer's `compressedSize` (256 MiB) plus a strict
//!   `8 + 8 + compressedSize == section size` check reject trailing
//!   bytes the trace doesn't authorise. Cross-validated against the
//!   Elephant fixture (`count = 576`, `compressedSize = 595`,
//!   section size = 611). The trace doc's §4.4 caveat — that the
//!   buffer uses a "common value" fast path on top of §3b so a
//!   naive [`usdc::decode_int_array`] recovers the run structure
//!   but not the literal field indices — is recorded in the module
//!   docs; the per-element semantic-recovery step is its own
//!   follow-up.
//! * [`usdc::split_field_sets`] — companion helper that splits the
//!   flat post-§3b `i32` array into one `Vec<i32>` per field set,
//!   dropping the `-1` sentinels. Accepts a trailing run without a
//!   sentinel (trace doc leaves the final terminator
//!   unconstrained), surfaces a leading sentinel as an initial
//!   empty set, and emits empty inner sets between consecutive
//!   sentinels.
//!
//! Round 239 — USDC §4.6 SPECS section three-buffer framing parser:
//!
//! * [`usdc::SpecsHeader`] / [`usdc::SpecsSection`] — parse the
//!   §4.6 SPECS section's outer three-buffer framing: an 8-byte
//!   little-endian `int64 count` header followed by three
//!   `(int64 compressedSize, §3a buffer)` triples. The three
//!   decompressed buffers carry `count × i32` path indices (into
//!   §4.5 PATHS), `count × i32` field-set indices (into §4.4
//!   FIELDSETS), and `count × i32` spec-type codes (prim /
//!   attribute / relationship / …); each row is the join
//!   `(pathIndex, fieldSetIndex, specType)` so SPECS is the table
//!   a reader iterates to materialise the stage.
//!   [`usdc::SpecsSection::paths_buffer`] /
//!   [`usdc::SpecsSection::fieldsets_buffer`] /
//!   [`usdc::SpecsSection::types_buffer`] forward to
//!   [`usdc::CompressedBuffer::parse`] on each bounded buffer
//!   slice ready for §3a / §3b chained decoding once the LZ4
//!   block decoder lands. Defensive caps on `count` (16 Mi) and
//!   on each buffer's `compressedSize` (256 MiB) plus a strict
//!   `8 + 3*(8 + compressedSize) == section_size` check reject
//!   trailing bytes the trace doesn't authorise. Cross-validated
//!   against the Elephant fixture (`count = 248`,
//!   `compressedSizes = 60 / 200 / 39`, section size = 331).
//!
//! Round 222 — USDC §4.3 FIELDS section framing parser:
//!
//! * [`usdc::FieldsHeader`] / [`usdc::FieldsSection`] — parse the
//!   §4.3 FIELDS section's outer two-buffer framing: an 8-byte
//!   little-endian `int64 numFields` header followed by two
//!   `(int64 compressedSize, §3a buffer)` pairs. The first
//!   buffer's decompressed form is an int-coded array (§3b) of
//!   `numFields` token indices giving each field its name; the
//!   second buffer's decompressed form is `numFields × uint64`
//!   packed value-rep words (type code + flags + inline / offset
//!   value). [`usdc::FieldsSection::names_buffer`] and
//!   [`usdc::FieldsSection::reps_buffer`] forward to
//!   [`usdc::CompressedBuffer::parse`] on the bounded buffer
//!   slices ready for §3a / §3b chained decoding once the LZ4
//!   block decoder lands. Defensive caps on `numFields` (16 Mi)
//!   and on either buffer's `compressedSize` (256 MiB) plus a
//!   strict `8 + 8 + csize₁ + 8 + csize₂ == section size` check
//!   reject trailing bytes the trace doesn't authorise.
//!   Cross-validated against the Elephant fixture
//!   (`numFields = 157`, `csize₁ = 141`, `csize₂ = 833`, section
//!   size = 998).
//!
//! Deferred to round 230+:
//!
//! * **`.usdc` §3a LZ4 wrapper.** The outer per-buffer wrapper
//!   that feeds bytes into §3b. The remaining primitive needed
//!   before any index section can be decoded end-to-end.
//! * **`.usdc` section-payload semantics.** Rounds 212 / 217 / 222 /
//!   229 / 236 / 239 land the TOKENS string-atom pool header
//!   (§4.1), the STRINGS index table (§4.2), the FIELDS two-buffer
//!   framing (§4.3), the FIELDSETS single-buffer framing (§4.4),
//!   the PATHS 16-byte leading prefix (§4.5) and the SPECS
//!   three-buffer framing (§4.6). The remaining payload-semantic
//!   step is the PATHS namespace tree's compressed-buffer
//!   decomposition (trace doc caveat: a recursive child/sibling
//!   jump encoding rather than three flat §3b arrays — see
//!   #1463) layered on top of §3a + §3b.
//! * **§3b "common value" fast path.** Round 229's §4.4 framing
//!   exposes the buffer but the trace doc records that FIELDSETS
//!   (and PATHS) layer a common-value compression step on top of
//!   the §3b shape. [`usdc::decode_int_array`] recovers the
//!   structural run boundaries but not the literal field indices;
//!   the per-element semantic-recovery step is a separate decoder
//!   pass the trace doc leaves as a future fact extraction.
//! * **FIELDS value-rep type-code enumeration.** A separate
//!   fact-table extraction (gap tracker's Round B) that the
//!   [`usdc`] primitives don't depend on.
//! * `UsdSkelSkeleton` + `UsdSkelBindingAPI` skinning — UsdSkel
//!   schema docs are not in our `docs/3d/usd/` (the spec README
//!   notes UsdSkel sits behind a per-schema URL pattern not yet
//!   staged).
//! * `UsdGeomSubset` per-face material binding subsets — same
//!   schema-doc gap as UsdSkel.
//! * Cross-package `@foo.usdz[path/within.usd]@` package-relative
//!   references — round 11 resolves `references` / `payload` targets
//!   that live as their own entries in the *same* archive; nested
//!   `[...]` package selectors into a sibling `.usdz` stay as
//!   side-channel opinions on `scene.extras["usd:layerMetadata"]`.
//! * SubLayer / reference / class-arc round-trip on the writer —
//!   rounds 10 / 11 / 13 evaluate sublayers + references + class
//!   arcs on the read path; the encoder flattens the composed tree
//!   into a single layer rather than preserving the multi-file or
//!   class-hierarchy authoring on output.
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
pub mod usdc;
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
