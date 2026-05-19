# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Round 10 (anchored sublayer composition)

- **In-archive sublayer composition.** When the Default Layer
  declares `subLayers = [@./geom.usda@, ...]` and the named
  entries exist in the surrounding USDZ archive, the decoder
  composes their prim trees underneath the local layer's per the
  OpenUSD glossary's LayerStack definition: "the recursive
  gathering of all SubLayers of a Layer, plus the layer itself as
  first and strongest". Sublayers are walked depth-first,
  strength-ordered (first entry strongest), with a cycle-break on
  self-reference. The audit trail of which sublayers were
  composed in lands on
  `Scene3D::extras["usd:composedSubLayers"]` (declaration order,
  depth-first innermost first). Rounds 1..9 ignored the
  `subLayers` opinion entirely on the read path — every prim
  authored in a sublayer was invisible to the `Scene3D`
  translation.
- **Anchored asset-path resolution.** Sublayer entries are
  resolved against the authoring layer's archive directory per
  `spec_usdz.html` §USD Constraints — `./foo.usda` and `foo.usda`
  both anchor to the parent layer's folder; absolute archive
  paths fall back to the archive root if anchored resolution
  misses. Paths containing `://`, `[` (cross-package), or
  starting with `/` are treated as external and silently skipped
  from composition (kept on `scene.extras` for writer
  round-trip).
- **LIVRPS-aligned prim merge.** When local + sublayer both
  declare a prim by the same name, the local opinion wins for
  every metadata key, attr key, variantSet, and same-named child.
  Children present only in the sublayer are appended after local
  children. Empty `type_name` on the local prim inherits from the
  sublayer. The specifier upgrades from `over` / `class` to `def`
  when the sublayer is the defining layer — the prim IS defined
  (by the sublayer); the local layer is overriding opinions.
- **`.usdc` sublayer surfaces as `Error::Unsupported`.** The
  binary Crate parser is still gated on a docs-collaborator trace
  doc; a sublayer with the `.usdc` extension errors out
  consistently with the round-1 Default-Layer policy rather than
  silently dropping the layer.

### Tests

- `tests/sublayer_composition.rs` — 9-case integration suite:
  * `sublayer_def_mesh_lands_in_scene` — basic
    `@./geom.usda@` composition of a `def Mesh`.
  * `local_layer_opinion_wins_over_sublayer` — local 3-vertex Mesh
    beats sublayer 4-vertex Mesh on same prim name.
  * `sublayer_fills_in_missing_def_prims` — local declares empty
    Xform, sublayer contributes two Meshes that surface.
  * `earlier_sublayer_beats_later` — `subLayers = [@a@, @b@]`,
    `a`'s opinions win.
  * `nested_sublayers_compose_recursively` — `a → b → c` chain
    composes the leaf-layer Mesh up to the root.
  * `cyclic_sublayer_does_not_loop` — `a` sublayers itself; we
    don't recurse forever.
  * `external_sublayer_path_is_silently_skipped` — unresolvable
    path keeps the opinion on extras, doesn't error.
  * `usdc_sublayer_surfaces_as_unsupported` — `.usdc` entry
    errors per the docs-gap policy.
  * `over_in_local_upgrades_to_def_from_sublayer` — `over` +
    `def` merge to `def`; the Mesh surfaces.
- Existing 121 tests across 21 integration files still green.

### Docs gap (carried forward from round 9)

- **UsdSkel + UsdGeomSubset still blocked.** Per-schema HTML for
  the `UsdGeom` / `UsdSkel` / `UsdShade` / `UsdPhysics` schema
  families lives behind a different URL pattern (`api_*.html`) on
  openusd.org and isn't staged in the workspace yet.
- **Cross-package composition arcs still single-archive.** Round
  10 composes intra-archive sublayers but does not open
  cross-package `@foo.usdz[path/within.usd]@` references; the
  `ArResolver` syntax mentioned in `spec_usdz.html` §USD
  Constraints stays as a side-channel opinion on
  `scene.extras["usd:layerMetadata"]`.
- **References / payloads composition not yet wired.** Round 10
  walks the `subLayers` arc; the matching engine for
  `references` / `payload` arcs (which target a *single* prim
  inside the named asset rather than the whole layer) is deferred
  pending its own round.
- **Sublayer writer round-trip.** Round 10 evaluates sublayers on
  the read path; the encoder flattens the composed tree back into
  a single layer rather than preserving the multi-file
  LayerStack structure on output.

### Added — Round 9 (composition-arc + layer-metadata round-trip)

- **Layer-metadata round-trip.** The writer now re-emits every
  layer-level `( ... )` metadata key — `defaultPrim`, `subLayers`,
  `customLayerData`, and any unknown / Apple-vendor key — in
  addition to the round-1 `upAxis` + `metersPerUnit` pair. The
  decoder stashes the source layer's full metadata block on
  `Scene3D::extras["usd:layerMetadata"]` using the lossless tagged
  shape from `crate::variant_codec`, so a USDZ → `Scene3D` → USDZ
  round trip preserves each value's USDA type-token discriminant
  (`Token` vs `String` vs `Asset` vs `AssetWithPath` vs `Path`).
  Round 1..8 dropped them silently — every authored `defaultPrim =
  "Root"` re-emitted with only `upAxis` + `metersPerUnit` left.
- **Prim composition-arc metadata round-trip.** Mirror of the
  layer-metadata path on `Node::extras["usd:primMetadata"]`. The
  writer now re-emits every prim-level `( ... )` metadata key —
  `kind`, `active`, `apiSchemas`, `references`, `payload`,
  `inherits`, `specializes`, custom keys — using the LIVRPS
  `prepend` list-edit prefix for the composition-arc keys
  (`references` / `payload` / `inherits` / `specializes` /
  `apiSchemas` / `variantSets`) per OpenUSD authoring convention.
  `references = @file.usd@</Prim>` round-trips with both the asset
  path and the `</Prim>` selector intact via `Value::AssetWithPath`.
- **`format_metadata_value` writer helper.** Inverse of
  `crate::usda::parse_value` — covers every `Value` variant the
  decoder produces from a layer- or prim-metadata block. Used by
  both round-9 round-trip paths plus open to future encoder needs
  for inline value serialisation.

### Tests

- `tests/composition_roundtrip.rs` — 8-case suite covering each
  composition-arc / layer-metadata shape end-to-end (build USDZ
  → decode → write → re-parse → assert): `defaultPrim`,
  `subLayers = [@a@, @b@]`, `prepend references = @./asset@</P>`
  (AssetWithPath form), `prepend payload = @./payload@`, prim
  `kind = "component"`, `customLayerData = { string version =
  "1.0" }`, `prepend apiSchemas = [...]` list-edit form, and an
  unknown vendor-custom layer-metadata key (`customField`).
- Existing 100+ tests across 20 integration files still green;
  the new tagged extras blob is additive (every prior round's
  untagged `usd:metadata` / per-key `usd:<key>` entries remain).

### Docs gap (carried forward from round 8)

- **UsdSkel + UsdGeomSubset still blocked.** `docs/3d/usd/README.md`
  documents that the per-schema HTML for `UsdGeom` / `UsdSkel` /
  `UsdShade` / `UsdPhysics` schema families lives behind a different
  URL pattern (`api_*.html`) on openusd.org and isn't staged in the
  workspace yet. The round-9 brief asked for `UsdGeomSubset` first;
  glossary.html doesn't define it (only TOC links into the
  unfetched `api_usd_geom_subset.html`), so the work fell back to
  the composition-arc push instead.
- **Cross-file composition arcs still single-layer.** Round 9
  surfaces references / payloads / sublayers as round-trip metadata,
  but the Scene3D translator still doesn't open the targeted
  `@file.usd@` and merge its contents — every cross-file arc stays
  as a side-channel opinion on `extras`. Cross-file resolution
  requires per-archive asset traversal that round 9 intentionally
  defers.
- **`NodeGraphNodeAPI` / `UsdShadeNodeGraph` composition** — the
  staged glossary only mentions it in TOC links to the unfetched
  per-schema HTML. Same staging gap as UsdGeomSubset.

### Added — Round 8 (variant writer round-trip)

- **VariantSet writer.** The encoder now re-emits `variantSet "name"
  = { "variant" ( meta ) { body } ... }` blocks captured from the
  source layer, plus the `variants = { string SET = "VAR" ... }`
  selection metadata and the `prepend variantSets = [...]` list, so
  a USDZ → USDZ round trip preserves every variant body, including
  the *unselected* branches that round 7 left invisible to the
  writer (round 7 evaluated the selected variant and flattened it
  into the resolved tree, dropping the others).
- **`crate::variant_codec` module.** Defines the JSON shape used
  for the `Node::extras["usd:variantSets"]` round-trip stash —
  every `Variant`'s `metadata` / `attrs` / recursive prim
  `children` are encoded with full type discriminants so the
  writer can reconstruct the original USDA spelling (`Token` vs
  `String` vs `Asset` etc. all round-trip distinctly). `Prim`
  trees inside a variant body are encoded recursively, including
  nested `variant_sets` declarations.
- **Decoder side: stash on Node extras.** `usd_to_scene::stash_extras`
  now also encodes the prim's `variant_sets` block via
  `variant_codec::encode_variant_sets()` and writes it under the
  `usd:variantSets` extras key on the synthesised `Node`.

### Tests

- `tests/variant_writer.rs` — 4-case suite: the full
  decode→encode→re-decode→re-parse loop on the glossary's classic
  three-variant `simpleVariantSet.usd` shape (asserts every variant
  body survives + selection metadata picks the same Capsule on
  re-decode); per-variant attribute round-trip; bare `def Xform`
  without variants emits no `(...)` metadata block (preserves r1/r2
  output shape); two distinct variantSets on the same prim survive
  + their selections still resolve on re-decode.

### Docs gap

- **UsdSkel + UsdGeomSubset still blocked.** `docs/3d/usd/README.md`
  documents that the per-schema HTML for the `UsdGeom` / `UsdSkel`
  / `UsdShade` schema families lives behind a different URL pattern
  (`api_*.html`) on openusd.org and isn't staged in the workspace
  yet. Both candidate features (skeletal animation, per-face
  material subsets) wait on that staging round.
- **Composition arcs (sublayers / references / payloads pulling in
  *external* USD files) still single-layer only.** Round 8 closes
  the variant-selection round-trip gap; cross-file composition
  remains a future round.

### Added — Round 7 (variant selection composition)

- **Variant selection composition.** The Scene3D translator now
  evaluates `metadata["variants"] = { string SET = "VAR" }`
  against `Prim::variant_sets` before walking the prim tree. The
  matching variant's `attrs` and `children` are merged into the
  prim per LIVRPS strength order — Local opinions beat the
  variant's. The headline behaviour: a `def Mesh` declared inside
  a selected variant materialises as a Scene3D `Mesh` + `Node`
  just like a directly-authored child would. The composition is
  recursive — variant selections deep in the prim tree are
  resolved on the same pass.
- **`Prim::resolved_variants()` helper.** Public API on
  [`crate::usda::Prim`] that returns the composed view of a single
  prim. Tests + Scene3D translator both use it. Variant
  `references = @other.usd@` opinions inside a selected variant
  are NOT followed (round 7 is single-layer evaluation only) —
  they're recorded on the resolved prim's metadata under
  `usd:variantReferences:<set>:<var>:references` so the extras
  layer surfaces the unresolved arc to the caller. Variant
  `metadata` keys other than `references` / `payload` lose to
  local opinions per LIVRPS.

### Tests

- `tests/variant_evaluation.rs` — 11-case suite covering: the
  glossary `simpleVariantSet.usd` shape (single variantSet, one
  variant selected, child `def Xform` materialises); changing the
  selection picks a different child; no-selection metadata leaves
  the variant invisible; selecting a non-existent variant name is
  silently tolerated (per "selections are not required"); local
  child wins over variant child sharing the same name; multiple
  variantSets compose independently; variant-authored `def Mesh`
  pulls through into a Scene3D mesh with positions; deeply-nested
  variantSet (on a non-root prim) is composed; variant
  `references = @...@` opinions land in the
  `usd:variantReferences:<set>:<var>:references` side channel;
  variant attribute opinion loses to local; `variant_sets` are
  preserved on the resolved prim for future writer round-trip.

### Docs gap

- **UsdSkel + UsdGeomSubset blocked.** `docs/3d/usd/README.md`
  documents that the per-schema HTML for the `UsdGeom` /
  `UsdSkel` / `UsdShade` schema families lives behind a different
  URL pattern (`api_*.html`) on openusd.org and isn't staged in
  the workspace yet. Both candidate features (skeletal animation,
  per-face material subsets) wait on that staging round.
- **Round 7 is read-only on the writer.** Composition fires on
  the decode path; the encoder doesn't yet round-trip
  `variantSet` blocks from the source layer's structured form
  (the round 6 changelog entry's "round-trip writer re-emits the
  block verbatim" claim was aspirational and remains a gap).
  After a `Scene3D` round-trip the variant content is flattened
  into the resolved tree — selection metadata is lost, the
  `variantSet` block isn't re-emitted.

## [0.0.1](https://github.com/OxideAV/oxideav-usdz/compare/v0.0.0...v0.0.1) - 2026-05-10

### Other

- Round 6: variantSet structured parsing + line/column errors
- USDA parser: accept Apple-emitted layer-metadata syntax
- Round 5: topology dispatch + no_fold flag + per-Mesh transform
- Round 4: UsdMediaSpatialAudio reader + writer
- Round 3: multi-primitive Mesh emission + sibling-fold decode
- Round 3: per-node transform xformOp serialisation
- Round 2: USDZ encoder + USDZ → USDZ pass-through optimisation

### Added — Round 6 (variantSet parsing + line/column errors)

- **VariantSet structured parser.** `variantSet "name" = {
  "variantA" ( meta ) { body } "variantB" { body } }` blocks
  inside a prim body now parse into a structured
  `Prim::variant_sets: BTreeMap<String, BTreeMap<String, Variant>>`
  rather than landing on the `unexpected character '{' at offset N`
  error path. Each `Variant` carries its own `metadata`, `attrs`
  and `children`, mirroring [`Prim`](crate::usda::Prim)'s shape so
  a future composition engine can switch on the prim's
  `variants = { string name = "..." }` selection metadata. Coverage
  matches OpenUSD glossary's `simpleVariantSet.usd` and
  `referenceVariantSet` examples — variants with composition-arc
  metadata (`prepend references = @...@`) round-trip too. Empty
  `variantSet "foo" = {}` blocks (Pixar's tooling emits these when
  a variant is removed but the declaration stub remains) parse to
  an empty inner map. The Scene3D translator continues to ignore
  variant_sets — round 6 is parser-side capture only; semantic
  composition deferred to a later round.
- **Line + column in parser error messages.** Every `at offset N`
  diagnostic in `usda.rs` now renders as `line L:C (offset N)`
  using a `Tokenizer::pos_display()` helper. The original byte
  offset stays in the message so callers comparing exact
  `at offset` substrings keep working; new consumers get an
  actionable line/column for hand-authored USDA debugging. Affects
  the identifier / attribute-name / numeric-literal / value-dispatch
  / prim-spec / prim-name / prim-body / type-token error paths
  (10 sites total).

### Tests

- `tests/apple_style_usda.rs` gains four cases:
  `variant_set_parses_into_structured_form` (the canonical
  glossary example), `variant_set_with_per_variant_metadata`
  (per-variant `prepend references = @...@`),
  `empty_variant_set_block` (the stripped-stub case),
  `parse_error_reports_line_and_column` (locks the new error
  message shape).

### Docs gap

- **USDC binary "Crate" format.** OpenUSD publishes no prose spec
  for the wire format — `docs/3d/usd/README.md` documents this
  explicitly. The C++ source (`pxr/usd/usd/crateFile.{h,cpp}`)
  is barred under the workspace's no-external-libs rule.
  Loading a `.usdc` Default Layer continues to surface as
  `Error::Unsupported` with a `usdcat -o foo.usda` hint.

### Added — Apple oracle parser unblock (CI run 25631625721)

- **Typed-dictionary value literals** (`Value::Dict`). The USDA
  value parser now recognises `{ TYPE NAME = VALUE; ... }` blocks
  in metadata position, which Apple's `usdzconvert` emits as
  `customLayerData = { string apple_metadata = "..." }`. Each
  entry's value is parsed recursively (so nested `dictionary X = {
  ... }` works); the type token is consumed but discarded since
  Round 1's Scene3D mapping doesn't act on layer-custom data.
  Previously the parser failed with `unexpected character '{' at
  offset 34 where value expected` on every Apple-emitted USDZ.
- **Composition-arc target with prim selector** (`Value::AssetWithPath`).
  Asset paths immediately followed by a `</Prim>` selector — the
  shape Apple uses for `references = @file.usd@</Prim>` and
  `payload = @file.usd@</Prim>` — now parse into a structured
  variant rather than erroring on the unexpected `<` after the
  closing `@`. Plain `@file@` references still parse as
  `Value::Asset`.
- **Sized vector type suffix** (`double[3]`, `int[2]`). The
  attribute-prefix parser previously only recognised `[]` for
  unbounded arrays; sized suffixes (used by Apple inside dict
  literals for vector metadata) now parse as part of the type
  token without erroring at the `[`.

### Tests

- `tests/apple_style_usda.rs` — 11-case regression suite mirroring
  the smallest viable Apple-shaped USDA snippets: typed-dict
  metadata, references with prim selectors, sub-layer arrays,
  per-attribute `colorSpace` blocks, prim `kind`/`active`
  metadata, `over`/`class` specs, Pixar's same-line-or-next-line
  brace layout, triple-quoted `doc` metadata.
- `tests/apple_oracle_local.rs` — opportunistic smoke test that
  builds an Apple-style USDZ via the system `usdcat` + Pixar's
  `pxr.UsdUtils.CreateNewUsdzPackage`, then decodes it through
  our `UsdzDecoder`. Skips with an `eprintln!` notice when either
  binary is absent (per workspace rule "no `#[ignore]`").

### Added — Round 5 (topology dispatch + no_fold + per-Mesh transform)

- **Strips / fans / lines / points dispatch in the writer.** Non-
  `Triangles` primitives are now serialised correctly instead of
  silently dropped:
  - `Topology::TriangleStrip` / `TriangleFan` → expanded in-place
    into a triangle list (alternating winding for strips, shared-
    anchor fan for fans) and emitted as a `def Mesh`. The source
    topology token survives in
    `Primitive::extras["usd:original_topology"]` so a downstream
    consumer can recover the hint.
  - `Topology::Lines` → `def BasisCurves` with `type = "linear"`,
    `wrap = "nonperiodic"`, and one `2` per index pair in
    `curveVertexCounts`.
  - `Topology::LineStrip` → `def BasisCurves` with `wrap =
    "nonperiodic"` and a single `curveVertexCounts` entry equal
    to the index count.
  - `Topology::LineLoop` → `def BasisCurves` with `wrap =
    "periodic"` (the closing segment is implicit per the
    UsdGeomBasisCurves periodic-wrap rule).
  - `Topology::Points` → `def Points` carrying the position
    array.
  Decoder symmetric: parses `BasisCurves` back into
  `Lines / LineStrip / LineLoop` topologies (driven by `wrap`
  + `curveVertexCounts` shape) and `Points` back into
  `Topology::Points`.
- **`usd:no_fold` extras flag.** When a `Primitive` carries
  `extras["usd:no_fold"] = true`, the writer marks the emitted
  `def Mesh` prim with the `(usd:no_fold = 1)` metadata flag so a
  subsequent decoder run skips the round-3 sibling-Mesh fold
  heuristic for that prim group. Decoder symmetric: any
  `def Mesh` prim with the metadata flag becomes its own
  Scene3D Mesh + Node, even when sibling stems would otherwise
  group it. Mirrors the authoring-tool convention for
  intentional `Foo` / `Foo_1` sibling collisions.
- **Per-Mesh transform on the inner `def Mesh`.** UsdGeomMesh
  inherits UsdGeomXformable, so a Mesh prim can carry its own
  `xformOp:*` opinions independent of the parent Xform. The
  round-5 mapping uses
  `Primitive::extras["usd:mesh_transform"]` to cross the typed
  model (since `Mesh` doesn't have a Transform field of its
  own); the encoder picks up the extras entry and emits the
  matching `xformOp:transform` (or `translate / orient / scale`
  triple) directly on the inner `def Mesh`. Decoder symmetric:
  surfaces an inner-Mesh `xformOp:transform` (or TRS triple)
  back into the same extras slot as a JSON-shaped value.

### Added — Round 4 (UsdMediaSpatialAudio reader + writer)

- **`UsdMediaSpatialAudio` reader.** `def SpatialAudio "<name>" {
  ... }` prims now decode into `Node::audio_emitter` referencing
  an `AudioEmitter` + `AudioSource` registered in the scene's
  audio arenas. `filePath = @path@` resolves against in-archive
  ZIP entries (wrapped in `ZipStoredAsset` so the writer's
  `raw_storage("zip-stored")` pass-through path applies to
  audio just like it does to textures); external paths land in
  `AudioData::External { uri }`. The `auralMode` token maps to
  `AuralMode::SpatialNonAcoustic` (`"spatial"`, USD's positional
  default) or `AuralMode::SpatialAcoustic` (`"nonSpatial"`); the
  raw token is also stashed into
  `emitter.extras["usd:auralMode"]` so the writer can preserve
  the exact spelling on round-trip. `gain` lands on
  `AudioEmitter::gain`; `startTime` / `endTime` / `mediaOffset`
  ride along on `AudioSource::extras`; `fillBufferTime` on
  `AudioEmitter::extras`.
- **`UsdMediaSpatialAudio` writer.** Each `Node` carrying
  `audio_emitter` emits a `def SpatialAudio` block inside its
  parent `Xform`. The new
  `usda_writer::collect_audio_assets()` mirrors
  `collect_texture_assets()`: `AudioSource`s with
  `raw_storage(scheme = "zip-stored")` flow into the output ZIP
  byte-identical (the USDZ → USDZ pass-through optimisation
  extended to audio); `AudioData::External` URIs round-trip the
  raw `filePath` string but are NOT packed into the archive.
  `EncodeReport` gains `audio_names` / `pass_through_audio` /
  `reencoded_audio` mirroring the existing texture counters.

### Added — Round 3 (per-mesh transforms + multi-primitive)

- **Multi-primitive Mesh emission + sibling-fold decode.** A
  Scene3D `Mesh` carrying N `Primitive`s now serialises as N
  sibling `def Mesh` prims under the parent Xform, named
  `<MeshName>` for the first and `<MeshName>_<i>` for
  subsequent ones (i = 1..N). Each carries its own
  `material:binding`. The decoder folds sibling Mesh prims
  whose names share a `<stem>` / `<stem>_<digits>` pattern
  back into a single Scene3D `Mesh` with N `Primitive`s,
  preserving full round-trip fidelity. Hand-authored sibling
  Mesh prims with unrelated names (`HeadGeo`, `BodyGeo`) stay
  unaffected — each becomes its own Scene3D Mesh.
- **Per-node transform serialisation.** Both encoder and decoder
  now round-trip `Node::transform` through the
  `UsdGeomXformable` opinion schema:
  - `Transform::Trs { translation, rotation, scale }` →
    `xformOp:translate` (double3) + `xformOp:orient` (quatf,
    USD's `(w, x, y, z)` layout) + `xformOp:scale` (float3),
    driven by
    `xformOpOrder = ["xformOp:translate", "xformOp:orient", "xformOp:scale"]`.
  - `Transform::Matrix(m)` → `xformOp:transform` (matrix4d)
    driven by `xformOpOrder = ["xformOp:transform"]`.
  - `Transform::identity()` emits no opinions (keeps r1/r2
    output minimal); decoder treats absent `xformOpOrder` as
    identity.

### Added — Round 2 (USDZ writer)

- `UsdzEncoder` implementing
  `oxideav_mesh3d::Mesh3DEncoder` for the `.usdz` container.
  `register()` now wires both decoder + encoder against a
  `Mesh3DRegistry`.
- USDZ-conforming PKZIP writer (`zip_writer::Writer`): STORED
  entries only, every payload offset padded to a multiple of 64
  bytes via the LFH `extra` field per the USDZ spec, CRC32
  computed per entry, central directory + EOCD assembled
  correctly.
- USDA tokenizer-inverse (`usda_writer::write_layer`): serialises
  `Scene3D` → `#usda 1.0` text. Coverage mirrors r1's reader —
  `Xform`/`Scope` nodes, `UsdGeomMesh` (Triangles, positions +
  optional normals + first-UV-set + material binding),
  `UsdPreviewSurface` materials with `UsdUVTexture` shader
  children for diffuse / normal / emissive / occlusion maps,
  `upAxis` + `metersPerUnit` layer metadata.
- **USDZ → USDZ pass-through optimisation.** When a texture's
  `AssetSource::raw_storage()` returns
  `RawStorage { scheme: "zip-stored", ... }` (which is what
  `ZipStoredAsset` from this crate's reader exposes), the encoder
  copies the inner-file bytes verbatim into the output ZIP — no
  inflate / decode / re-encode cycle. A USDZ → `Scene3D` → USDZ
  pipeline now preserves texture bytes bit-identical from input
  to output. Verified by a roundtrip integration test
  (`tests/roundtrip_encoder.rs`) + the new
  `EncodeReport { pass_through_textures, reencoded_textures, ... }`
  return shape from `UsdzEncoder::encode_with_report()`.

### Added — Round 1 (USDZ reader scaffold)

- `UsdzDecoder` implementing
  `oxideav_mesh3d::Mesh3DDecoder` for the `.usdz` container.
- PKZIP central-directory walker covering STORED entries with
  64-byte payload-alignment validation per the USDZ spec; every
  other PKZIP feature (compression, encryption, ZIP64,
  multi-volume) is rejected at the boundary.
- Hand-rolled ASCII USDA tokenizer + prim-tree parser. Supports
  layer metadata, `def`/`over`/`class` prim specs, attribute and
  relationship statements, prim/property paths (`</A.attr>`),
  asset paths (`@uri@` and `@@@uri@@@`), nested arrays/tuples,
  numeric/bool/string/token/path/asset values.
- USD → `Scene3D` translator:
  - `UsdGeomMesh` → `Mesh + Primitive { Triangles, ... }` with
    fan-triangulation of arbitrary polygon faces.
  - `UsdPreviewSurface` → `Material` (PBR base colour, metallic,
    roughness, emissive, opacity, normal/diffuse/emissive/occlusion
    texture connects).
  - `UsdUVTexture` → `Texture` whose `ImageData::Source` wraps a
    `ZipStoredAsset` — implements `AssetSource::raw_storage()`
    with `scheme = "zip-stored"` so a round-2 USDZ writer can
    pass inner-file bytes through verbatim.
  - `upAxis` + `metersPerUnit` map onto `Scene3D::up_axis` /
    `Scene3D::unit`.
- `.usdc` (binary Crate) Default Layer surfaces a clear
  `Error::Unsupported` pointing at `usdcat -o foo.usda` for
  conversion (binary parser deferred to round 2+).
- Default-on `registry` cargo feature exposes
  `oxideav_usdz::register(&mut Mesh3DRegistry)`; standalone build
  (`default-features = false`) still parses to the same `Scene3D`
  typed model.
