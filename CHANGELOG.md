# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
