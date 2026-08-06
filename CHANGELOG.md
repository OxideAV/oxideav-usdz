# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **usdc: §16.3.8.4.5.4 path construction — full `SdfPath`
  reconstruction (`usdc::construct_paths` /
  `UsdcFile::decode_paths`).** The AOUSD spec publishes the PATHS
  tree-walk algorithm the trace doc deferred: entry 0 seeds the
  absolute root `/`, each entry appends its element token to the
  parent path with the delimiter selected by the element-token word's
  sign (positive = prim `/`, negative = property `.`), and the jump
  word drives descent (`-2` leaf, `-1` child-only, `0` sibling-only,
  positive = child at the next entry + sibling subtree `jump` entries
  ahead). The walk is implemented iteratively with a sibling stack
  and full corruption guards — out-of-table jumps, cycles, duplicate
  or unfilled output slots, and the spec-forbidden zero token index
  are each precise `InvalidData` refusals. On the committed Elephant
  fixture all 248 paths reconstruct (`/`,
  `/SoC_ElephantWithMonochord`, …,
  `/SoC_ElephantWithMonochord.xformOp:transform`), pairwise
  distinct, parent-closed, and each ending in its spec's leaf token.

- **usdc: §16.3.9/§16.3.10 typed value decoding
  (`usdc_values::ValueDecoder`) — every Crate field value now decodes
  to a concrete `usda::Value`.** The decoder implements the spec's
  value-representation rules end to end: the §16.3.9.1 inlining rules
  (4-byte payload limit; zero payload = default; direct casts for the
  one-dimension types; `Vec2h` component packing; the int8 component
  fallback for wider vectors and the int8 matrix diagonal — pinned by
  the fixture's `0x01010101` identity matrix4d; `double`-as-`float`,
  `(u)int64`-as-`(u)int32`, token/string/asset by pool index), offset
  scalars (double/TimeCode, all dimensioned vec/quat/matrix widths,
  row-major matrices per §16.3.10.24, imaginary-first quaternion
  storage re-ordered to the text form's real-first per §16.3.10.22),
  uncompressed arrays for every Supports-Array type, §16.3.9.3.1
  compressed integral arrays (`u64 compressedSize` + §3a/§3b stream;
  32- and 64-bit widths), §16.3.9.3.2 compressed floating-point
  arrays (both the `i` all-integral coding and the `t` lookup-table
  coding: `u32 lutCount` + values + int-coded indices), the
  §16.3.10.26 index vectors (Token/String/Path/Double/LayerOffset),
  §16.3.10.19 dictionaries (relative-offset members, recursion
  bounded), §16.3.10.25 list operations (bitmask header + the six
  ordered sublists mapped onto `usda::ListOp`; only-Make-Explicit =
  explicit empty list), §16.3.10.30 variant-selection maps,
  §16.3.10.31 time samples (the double-indirect timecodes DoubleVector
  + per-sample rep table, offset bases pinned on the fixture's two
  shared-timeline tracks), §16.3.10.21 references/payloads,
  §16.3.10.17 indirect values with a recursion guard, §16.3.10.18
  unregistered values (inner-type restriction enforced), ValueBlock →
  `Value::None`, and the §16.3.10.27–.29 specifier / permission /
  variability enums (out-of-range codes refused). Relocates (0.11.0)
  and Splines (0.12.0) refuse with a precise `Unsupported`. A
  spec-conventions IEEE-754 half decoder (`half_to_f32`) covers
  normals, denormals, infinities and NaN. **Fixture proof:** all 157
  Elephant field reps decode; pinned values include
  `defaultPrim`/`upAxis` tokens, the inline-60.0 / offset-0.01 layer
  doubles, `primChildren` TokenVectors, `xformOpOrder` token arrays,
  inline + offset matrix4d, unit-norm real-first quatf[] rest
  rotations, prepend-authored `apiSchemas`, absolute-path
  connection/target list ops, both `i`- and `t`-coded compressed
  float arrays (values within [0,1]), and the 2540/3023-sample
  animation tracks. `usda::Value` and `usda::ListOp` now derive
  `PartialEq` to support value-level assertions.

- **usdc: `decode_compressed_int_array` reads the §16.3.7.2.3
  `u64 compressedSize` prefix.** The value-region compressed-array
  stream opens with its size word (right after the element count);
  the old code treated the size byte as the §3a chunk-count byte,
  which is why the fixture's compressed `int[]` reps "failed
  cleanly" — they now decode (element counts recovered exactly), and
  the fixture test asserts both that and that compressed `float[]`
  reps still refuse the integral decoder.

- **usdc: the §16.3.10.1 value-type table (`usdc::ValueType`).** The
  AOUSD *USD Core Specification* 1.0.1 (staged in `docs/3d/usd/`)
  publishes the Crate value-type enumeration — IDs 1–59 with their
  per-type *Supports Array* column — closing what the gap tracker
  called the "Round B" blocker. `ValueType` carries the table verbatim
  (discriminant = on-disk type-code byte), with `from_id` / `id` /
  `name` / `supports_array`, and `ValueRep::value_type()` resolves a
  rep's raw `type_code()` byte to the named type (unknown / future IDs
  resolve to `None` rather than failing the file). Cross-checked
  against the committed Elephant fixture: all 157 field reps resolve,
  every array-flagged rep uses a Supports-Array type, and the
  fixture's empirical type roster (bool / int / float / double /
  token / asset / matrix4d / quatf / float2 / float3 / half3 /
  float4 / TokenListOp / PathListOp / TokenVector / Specifier /
  Variability / TimeSamples) is pinned by test. The previously
  observer-guessed "`0x0f` = `double[]`" reading is corrected: `0x0f`
  is **matrix4d** (its first element still reads `1.0` — the identity
  diagonal).

### Fixed

- **usdc: §16.3.7.2.2 full-width codepoint is a *difference*, not an
  absolute value.** The int-coded decoder treated control code `3`
  (full-width int32) as the element's absolute value — a trace-doc-era
  guess that no fixture buffer exercises (the encoder mirrored it, so
  round trips masked it). The spec's worked example pins the
  difference model for every codepoint (`int32(100000)` encodes the
  element whose absolute value is `100125`), so decode is now
  `prev + delta` for code `3` too, and `encode_int_coded` emits the
  delta. New tests carry the spec's worked example end-to-end
  (decode of the published bytes + byte-exact re-encode) and a
  discriminating prev≠0 case. A 64-bit variant
  (`decode_int_array64` / `encode_int_coded64`) lands alongside for
  the §16.3.9.3.1 compressed `int64[]` / `uint64[]` value arrays
  (int64 preamble; int16 / int32 / int64 delta widths).

- **usdc: PATHS element-token mapping corrected to the normative
  §16.3.8.4.5.2 rule.** The observer-era decode read the element
  token index as `abs(word) >> 1` (guessing the low bit was a flag);
  the spec pins it as **`abs(word)`** with the **sign** as the
  prim-vs-property delimiter selector — under the old mapping every
  fixture element still landed in pool range, which is why the guess
  survived, but the resolved names were wrong (e.g. the root prim's
  element resolved as `defaultPrim`). `PathElement::element_token_index`
  now carries the spec mapping, `PathElement::is_property()` exposes
  the sign, a zero word is rejected per the spec, and the
  fixture-grounded tests now pin the true names
  (`SoC_ElephantWithMonochord`, `CharacterAudioSource`, …).
  `decode_spec_leaf_names` inherits the correction.

### Changed

- **`UsdPreviewSurface` extras shims migrated onto the typed
  `oxideav-mesh3d` material-extension surface (published 0.0.4).**
  The parked extras/HashMap side-channels for the expanded §2.1
  inputs now land on `Material::ext`:
  - `inputs:ior` → `MaterialExt::ior` (`Option`-shaped, so an
    authored opinion — including an explicit `1.5` — stays
    distinguishable from absence; `usd:inputs:ior` extras retired).
  - `inputs:clearcoat` / `inputs:clearcoatRoughness` + their texture
    connections → `MaterialExt::clearcoat` (the typed `Clearcoat`
    lobe: factor / roughness / factor_texture / roughness_texture).
    The lobe materialises only when at least one clearcoat opinion is
    authored; unauthored inputs inside it evaluate to the schema §2.1
    defaults (`0` / `0.01`) so consumers read the value the shader
    would use, and the writer skips default-valued fields so no
    synthetic opinion lands in the output file. The
    `usd:inputs:clearcoat*` and `usd:tex:clearcoat*` extras are
    retired.
  - `useSpecularWorkflow = 1` + `specularColor` (+ its texture
    connection) → `MaterialExt::specular` (the typed `Specular`
    slot): `Some` is exactly "the specular workflow is active", the
    authored color (or the schema default black) is the F0
    `color_factor`, and `specularColor.connect` lands on
    `color_texture`. A `specularColor` authored while the workflow is
    off is inert per schema §2.1 and keeps riding on extras so it
    re-emits without activating the workflow. The
    `usd:useSpecularWorkflow` extras key is retired;
    `usd:inputs:specularColor` / `usd:tex:specularColor` survive for
    the inert case only.

### Fixed

- **`matrix4d` convention boundary: USD row-vector literals now
  transpose into the typed model's column-vector `Transform::Matrix`
  (and back on write).** USD's `matrix4d` is row-vector maths
  (`p' = p · M`, translation in the last *row*); the typed model
  documents column-vector maths (`p' = M · p`, translation in the
  last *column*, matching its glTF-layout `node.matrix` contract).
  The decoder previously stored the USD rows verbatim — internally
  self-consistent (the writer replayed them verbatim), but every
  matrix handed to or taken from the typed model was transposed:
  `Skeleton::inverse_bind_matrices` carried translation in the
  fourth row (failing `Scene3D::validate`'s glTF §5.28.1 affine
  last-row check), joint rest / `xformOp:transform` node matrices
  were transposed for any typed-model consumer, and a scene authored
  through the typed model emitted a *projective* `matrix4d` literal
  instead of a translation. All USD-literal ↔ typed-model crossings
  now transpose (`transpose4`, its own inverse): skeleton
  bind/rest, `xformOp:transform` decode, `write_node_transform`,
  the skel writer's bind/rest re-emission, and the
  `usd:mesh_transform` extras replay (the extras stash itself stays
  in USD literal layout, documented). Round trips remain
  byte-identical; the emitted USD for typed-model scenes is now
  semantically correct. A new `decode_validates` suite pins every
  decoded scene (expanded materials with ext-slot textures, skel
  scenes) through `Scene3D::validate()` on both first decode and
  repack.

- **UsdSkel §1.6 cross-primvar consistency enforced.** The schema
  requires `interpolation` and `elementSize` to be identical between
  `primvars:skel:jointIndices` and `primvars:skel:jointWeights`; the
  decoder silently resolved a mismatch in favour of `jointIndices`,
  pairing arrays that describe different layouts (fabricated
  influences). Both knobs are now cross-validated when authored on
  both primvars, and a mismatch is a precise `InvalidData` refusal.

- **texture asset filename drift across repack.** A decoded texture
  was named after its `UsdUVTexture` shader prim; since the writer
  derives both the output archive entry name and the `inputs:file`
  reference from `Texture::name`, a decode → encode repack renamed
  `cc.png` to `<ShaderPrim>.png` on the first cycle and to
  `Texture_<id>.png` on the second — a two-cycle drift breaking the
  one-cycle round-trip fixed point for textured materials. Textures
  are now named after the archive entry's basename stem, so the
  original asset filename survives the repack verbatim and the text
  round trip is a fixed point after one cycle.

### Added

- **`<UDIM>` tile-set texture references (staged schema §2.2).** A
  `UsdUVTexture` whose `file` input carries the `<UDIM>` token names
  a UDIM *tile set* — no single archive entry can back it, so the
  decode previously failed with *"not present in the USDZ archive"*.
  Such a texture now decodes to `ImageData::External` with the
  authored URI preserved verbatim (MIME inferred from the extension;
  sampler/wrap/stash decode unchanged), and the writer re-emits the
  URI unchanged with no phantom archive entry — a one-cycle
  round-trip fixed point. Only the `<UDIM>` token opts into the
  external fallback: a plain missing path is still a self-contained-
  container violation with the precise diagnostic.

- **`UsdPrimvarReader_<T>`-connected preview-surface inputs (staged
  schema §2.3).** A material input connected to a
  `UsdPrimvarReader_<T>` (e.g. `diffuseColor` reading the
  `displayColor` primvar off the bound geometry) previously killed
  the whole decode with *"only `UsdUVTexture` is supported"*. The
  connection resolver now recognises all ten §2.3 typed reader
  variants and preserves the reader's authored inputs — variant
  type, `varname` (with its `string`/`token` type spelling), and
  `fallback` as an exact USDA literal — on
  `Material::extras["usd:primvar:<input>"]`. The writer re-emits the
  connection plus a `UsdPrimvarReader_<T>` shader prim per input
  (correct `outputs:result` element type per the §2.3 table), and
  the encode → decode → encode cycle is a fixed point. Unknown
  shader ids and non-§2.3 reader variants still refuse precisely.

- **USDA `timeSamples` parsing.** The parser now understands the
  time-sampled attribute value form
  `<attr>.timeSamples = { 0: VALUE, 10: VALUE, ... }` — the shape
  UsdSkel animations author their `translations` / `rotations` /
  `scales` / `blendShapeWeights` in. Samples land on the new
  `usda::Value::TimeSamples(Vec<(f64, Value)>)` variant in authoring
  order (negative timeCodes and trailing commas tolerated). The
  braces syntax is disambiguated from `{ TYPE NAME = VALUE }` typed
  dictionaries on the first non-trivia character after `{` (numeric
  lead-in = timeSamples). The writer emits the map back with the
  same literal shape, the extras JSON view flattens it to an object
  keyed by the timeCode's decimal string, and `variant_codec`
  round-trips it losslessly (order + f64 keys preserved).

- **Expanded `UsdPreviewSurface` input coverage (staged schema §2.1).**
  The material translator now reads the full shader input set:
  `occlusion` maps onto `occlusion_strength`, and
  `opacityThreshold > 0` selects cutout (`AlphaMode::Mask`) with a
  sub-1 `opacity` otherwise selecting `AlphaMode::Blend`.
  `useSpecularWorkflow` + `specularColor`, `clearcoat`,
  `clearcoatRoughness`, `ior`, `displacement`, and a non-default
  constant `normal` are preserved per-input on
  `extras["usd:inputs:*"]` / `extras["usd:useSpecularWorkflow"]`, so
  only authored opinions re-emit. (Mapping the specular workflow /
  clearcoat / IOR onto the typed material-extension slots is deferred
  until the published `oxideav-mesh3d` release carries that extension
  surface.) `metallic.connect` / `roughness.connect` resolve into the
  packed metallic-roughness texture slot with the authored-input
  record on `extras["usd:mr_connect"]`; texture connections without a
  typed slot (opacity, displacement, specularColor, clearcoat maps, a
  roughness map differing from the metallic map) round-trip through
  `extras["usd:tex:<input>"]`. The writer re-emits every expanded
  input and connection, and the encode → decode → encode cycle is a
  fixed point (covered by `tests/preview_surface_expanded.rs`).

- **Expanded `UsdUVTexture` + `UsdPrimvarReader` + gprim display
  attributes (staged schema §2.2 / §2.3 / §2.5).** `wrapS` / `wrapT`
  map onto the typed sampler wrap modes (`clamp` / `mirror` /
  `repeat`; `black` and `useMetadata` keep the default and their
  authored spelling). `scale`, `bias`, `fallback`, and
  `sourceColorSpace` are preserved on
  `Scene3D::extras["usd:uvtexture:<id>"]` and replayed by the writer.
  A texture's `st` connection is followed to its
  `UsdPrimvarReader_float2` and the reader's `varname` selects the
  UV set (`st` → 0, `st<N>` → N; non-standard primvar names are
  preserved); the writer emits the reader back for any non-zero UV
  set. Meshes now read/write multi-UV `primvars:st1`, `st2`, ... into
  `Primitive::uvs[N]`, `doubleSided` lands on the primitive extras +
  the bound material's `double_sided`, and per-vertex
  `primvars:displayColor` / `displayOpacity` land on the first
  vertex-colour set (constant/uniform interpolations preserved on
  extras). Covered by `tests/uvtexture_primvar_reader.rs`.

- **UsdSkel core — `SkelRoot` / `Skeleton` / BindingAPI (staged
  schema §1.1–§1.6).** A `def Skeleton` now materialises into the
  typed model: one scene-graph node per `joints` token (tree
  topology from the path-prefix rule, local rest transform from
  `restTransforms`) plus a `Skeleton` resource whose
  `inverse_bind_matrices` are the inverted world-space
  `bindTransforms` (Gauss-Jordan, f64). The carrier node marks
  itself with `extras["usd:skeleton"]` so the writer re-emits a
  `def Skeleton` reconstructed from the typed data (joint tokens
  from the node tree, `bindTransforms` re-inverted, `restTransforms`
  from the joint nodes' local transforms); `jointNames` round-trips
  via the carrier stash. BindingAPI: `rel skel:skeleton` binds
  geometry (inheritance down namespace per §1.5 — `skel:skeleton` /
  `skel:animationSource` / `skel:joints` propagate to descendants),
  `primvars:skel:jointIndices` / `jointWeights` decode under their
  `interpolation` (`constant` = rigid, replicated per point;
  `vertex` = per point) and `elementSize` knobs into the typed
  4-wide joint/weight quads (over-4 influence sets keep the 4
  strongest; weights stay as-authored per §1.6), with `skel:joints`
  overrides remapped into the Skeleton's canonical joint order by
  token match. `geomBindTransform` + `skinningMethod` ride on
  `Primitive::extras["usd:skel:*"]`. Skinned nodes get a shared
  `Skin` (explicit root = the skeleton's root joint) and the writer
  emits the full binding back (`SkelBindingAPI` apiSchemas
  declaration, skeleton rel, canonical `vertex`/`elementSize = 4`
  influence primvars). `def SkelRoot` survives as the container's
  schema token via `extras["usd:type"]` (unknown-schema containers
  now re-emit their token generally). The attribute-metadata parser
  additionally tolerates `,` / `;` entry separators
  (`(elementSize = 4, interpolation = "vertex")`). 13 new
  integration tests in `tests/usdskel_binding.rs`, including
  write → re-read fidelity and the one-cycle fixed point.

- **UsdSkel SkelAnimation (staged schema §1.3).** A
  `def SkelAnimation` (or the deprecated `PackedJointAnimation`)
  decodes into one typed `Animation`: per-joint Translation /
  Rotation / Scale channels targeting the joint nodes of the
  skeleton its `joints` tokens name (subset / reordered token lists
  remap by path-token match; the parallel-array lengths are
  validated per sample). timeCodes map to seconds through the
  layer's `timeCodesPerSecond` (fallback `framesPerSecond`, default
  24); a non-sampled default array is a single keyframe at `t = 0`;
  `quatf` literals convert from USD's `(w, x, y, z)` to the model's
  xyzw. The carrier node marks `usd:skelAnimation` so the writer
  re-emits a `def SkelAnimation` reconstructed from the typed
  channels (joint tokens rebuilt from the skeleton carriers,
  per-property `.timeSamples` maps on the shared timeline, seconds x
  tcps back to timeCodes) and skinned geometry re-authors
  `rel skel:animationSource` pointing at the animation that drives
  its bound skeleton. 8 integration tests in
  `tests/usdskel_animation.rs`, including round-trip fidelity and
  the one-cycle fixed point.

- **UsdSkel blend shapes (staged schema §1.4 / §1.5 / §1.8).**
  `def BlendShape` prims resolve through the geometry's positional
  `skel:blendShapes` / `skel:blendShapeTargets` pair into typed
  `MorphTarget`s — dense `offsets` / `normalOffsets` verbatim, a
  sparse shape (`pointIndices`) scattered into zero-filled per-point
  delta buffers. The channel-name roster rides on
  `Primitive::extras["usd:skel:blendShapes"]`. SkelAnimation
  `blendShapes` + `blendShapeWeights` decode into one `MorphWeights`
  channel per bound mesh node, with per-frame weights remapped from
  the animation's channel order into the mesh's target order by
  name (§1.8 step 1; blend-only animations — no `joints` — are
  supported). The writer re-emits `def BlendShape` children on the
  mesh prim, the positional binding pair, and the animation's
  `blendShapes` / `blendShapeWeights.timeSamples`. Inbetween shapes
  (`inbetweens:` namespace) are not yet modelled. 7 integration
  tests in `tests/usdskel_blendshapes.rs`; additionally,
  `tests/skel_fixture_tokens.rs` pins all 19 staged UsdSkel schema
  token spellings against the committed real-production Crate
  fixture's TOKENS pool.

### Fixed

- **round-trip drift: bare-mesh-carrier collapse.** A `Scene3D` whose
  geometry is attached directly to a node (`Node::mesh`) round-tripped
  through USDA into a *monotonically growing* tree: the writer emitted
  the node as `def Xform "N" { def Mesh "M" }`, and the reader then
  re-externalised that inner `def Mesh` into a *standalone* mesh-carrier
  child node — so the next encode wrapped it in yet another `def Xform`,
  adding one namespace level (~58 bytes) on *every* encode→decode cycle
  without bound. The writer now recognises the exact shape the reader
  produces — a node that is a pure mesh carrier (mesh set, identity
  transform, no children / camera / light / skin / audio / variant /
  composition metadata) *whose own name already equals its mesh's prim
  name* — and emits the mesh prim(s) directly at that level instead of
  re-wrapping them in a redundant `def Xform`. The round-trip is now a
  fixed point (stable after at most one cycle). Nodes whose name differs
  from the mesh (a genuine Xform locator over a differently-named mesh)
  keep the Xform, so hand-authored layers still round-trip byte-for-byte.
  The same drift affected `BasisCurves` / `Points` and the strip/fan
  topologies: the writer's own geometry-recovery hints
  (`usd:original_topology`, `usd:no_fold`) were being mirrored onto the
  carrier node's extras — as well as onto the geometry prim they belong
  on — which left the node's prim-metadata non-empty (defeating the
  collapse) and double-emitted the hint on the enclosing Xform. The
  decoder no longer stashes those geometry-only hints on the node; they
  continue to round-trip via `Primitive::extras`, which is where the
  mesh writer reads them.

- **spurious empty `Materials` root on every material round-trip.** The
  writer emits materials under a top-level `def Scope "Materials"`; on
  decode that scope became an empty, childless scene-graph node and was
  pushed onto `Scene3D::roots` — so a one-root scene came back with *two*
  roots (the geometry root plus a phantom `Materials` node). A USD
  `Scope` is a non-transformable namespace container; when it contributes
  no scene-graph children (its `Material` children are indexed separately
  into `Scene3D::materials`) it is now dropped rather than emitted as a
  node. Empty `Xform`s are still kept — an Xform carries a transform and
  may be a meaningful locator/anchor.

### Added

- tests: **`roundtrip_fidelity` harness** — a decode→encode→decode
  matrix over base-colour, metallic/roughness, emissive, normals+UVs,
  texture binding, transforms, up-axis/unit, and deep hierarchy that
  reports a measured pass-count (8/8 channels), plus fixed-point /
  single-root regression guards for the two fixes above.

- zip: **fallible writer surface guarding the ZIP64 boundary**. The USDZ
  writer gains `Writer::try_add_stored` and `Writer::try_finish`, which
  return `Error::Unsupported` when an entry or the assembled archive
  would cross the ZIP64 boundary USDZ forbids (`GAP-TRACKER.md` §3): a
  payload of `2^32-1` bytes or more, a local-header / central-directory
  offset at or past `2^32-1`, or more than `2^16-1` entries. Previously
  those cases silently truncated `payload.len() as u32` /
  `records.len() as u16` / `offset as u32` and emitted a corrupt
  archive. The infallible `add_stored` / `finish` remain (delegating to
  the `try_*` forms and documented to panic on the boundary — a
  conforming USDZ never reaches it); `UsdzEncoder::encode_with_report`
  now routes through the `try_*` surface so an oversized scene surfaces
  cleanly as an error. Three new tests: the `try_*` round-trip, the
  `2^16`-entry rejection, and the infallible wrapper for conforming
  sizes.

- zip: **ZIP64-sentinel rejection + central-directory entry-count
  validation**. The USDZ container walker now detects the ZIP64
  sentinels in the End-of-Central-Directory record (`0xFFFF` total-entry
  count, `0xFFFFFFFF` central-directory size/offset — APPNOTE.TXT
  §4.3.14/§4.4) and rejects the archive with a precise *"USDZ forbids
  ZIP64"* diagnostic instead of dereferencing the sentinel as a literal
  offset (which previously failed with a baffling downstream "extends
  past EOF"). It also validates that the number of central-directory
  records actually walked equals the EOCD's declared total-entry count,
  catching a truncated directory or a mis-sized variable-length field at
  the container boundary rather than silently returning a short entry
  list. Three new tests cover the entry-count sentinel, the
  central-directory-offset sentinel, and the count-mismatch case.

- usdc §4.3/§3b: **compressed-integer array materialisation**. A
  `ValueRep` whose `is_compressed` flag rides on `is_array` resolves
  (via `value_region`) to a `ValueRegion::CompressedArray { count,
  region_offset }`; the new `UsdcFile::decode_compressed_int_array`
  runs the documented §3a → LZ4 → §3b path on that region (reusing
  `int_coded_max_len` as the decompression-bomb budget and
  `decode_int_array` for the control/payload walk) and returns the
  `count` recovered `i32`s. This closes the previously surfaced-but-
  unmaterialised compressed-array half of the value region for the
  **integer** element classes the trace doc §3b grounds (indices,
  jumps, counts); compressed float/double/half element codings stay
  deferred (the type code that distinguishes them is Round B), so the
  method documents the caller's obligation to establish integrality and
  fails cleanly — never panics — on a non-integer region. New
  `ValueRegion::array_count` / `compressed_region_offset` accessors
  expose the array shape uniformly. Four new tests round-trip a
  synthesised §3a/§3b region end-to-end through `value_region` +
  `decode_compressed_int_array`, plus the `count == 0`, non-compressed-
  region-rejected, and accessor cases. A real-fixture test against the
  committed Elephant `.usdc` pins the grounded limit: its
  compressed-array reps are **not** the §3b integer form (the value-
  region compressed float/double element coding is a separate, deferred
  Round-B encoding the trace does not document), and
  `decode_compressed_int_array` correctly *fails cleanly* on them rather
  than fabricating integers — the empirical backing for the method's
  caller-obligation contract.

- usda: **list-edit-operator-aware composition arcs**. The USDA parser
  now captures the `prepend` / `append` / `delete` / `add` / `reorder`
  list-edit operator on composition-arc and list-valued metadata
  fields (`references`, `payload`, `inherits`, `specializes`,
  `apiSchemas`, `subLayers`, …) instead of discarding it. Each
  list-edited field parses into a new `Value::ListOp` that keeps the
  *prepended* / *appended* / *deleted* / *explicit* / *reordered*
  sublists separate (mirroring USD's `SdfListOp`), so several authored
  statements of the same field on one prim merge losslessly. The
  composition engine flattens the additive sublists in LIVRPS strength
  order — *prepended* → *explicit* → *appended* — and then removes any
  `delete`-authored target. A `delete references = @x@` is now an actual
  **removal** rather than being silently treated as an add, and the
  writer re-emits the operator the author wrote (multiple operators on
  one field round-trip as separate lines) rather than forcing every arc
  to `prepend`. The variant side-channel stash and the JSON extras view
  preserve the full per-operator structure. A new
  `tests/listop_composition.rs` pins the operator capture/merge, the
  `delete`-cancels-`prepend` composition behaviour, and the writer's
  per-operator re-emission end-to-end.

- usdc §4.5/§5: **PATHS↔SPECS namespace join** giving every spec its
  leaf component name. `UsdcFile::decode_path_elements_by_slot` reorders
  the §4.5 path elements into `target_index` (= slot) order — the same
  index space a §4.6 SPECS row's `path_index` addresses — and validates
  the permutation (no gap/collision). `UsdcFile::decode_spec_leaf_names`
  then joins the SPECS table with that slot view: each spec's
  `path_index` selects the `PathElement` whose `element_token` is the
  spec's own path component, resolved against the §4.1 TOKENS pool. On
  the Elephant fixture the pseudo-root spec (`path_index 0`,
  `spec_type 7`) carries the stage metadata fields, and each prim spec
  (`spec_type 6`) gets its type/name token (`Xform` / `Materials` /
  `CharacterAudioSource` / …) as its leaf — all asserted by a new
  fixture test. This names *what each spec is* (leaf + fields) on
  grounded footing; the full ancestor path (`/Foo/Bar/leaf`) still needs
  the deferred jump-walk (gap-tracker §1 / Round B).

- usdc §4.5: **observer-grounded `PATHS` structural join** —
  `PathsSection::decode_path_elements` (+ the file-level
  `UsdcFile::decode_path_elements` convenience) decode the section's
  three §3b integer buffers into one typed `PathElement` per tree-walk
  slot. Empirically parsing the committed Elephant fixture's buffers
  pins their per-element roles, tighter than the trace doc's "holds"
  column: buffer 1 is the **target-slot permutation** (an exact
  permutation of `0..numPaths`, *not* token indices — that was the
  source of the old "values exceed the TOKENS pool" caveat), buffer 2 is
  the **element-token word** (`abs(word) >> 1` is an in-range §4.1
  TOKENS index for all 248 elements, resolving to `Xform` / `Material` /
  `xformOp:transform` / …, with the sign + low bit preserved verbatim as
  deferred flags), and buffer 3 is the **jump** offset. `PathElement`
  exposes `target_index`, `element_token_index`, `element_token_word`,
  `jump`, and an `element_token(&tokens)` resolver. The decode stops at
  the verified parallel-array join — the tree **walk** that reconstructs
  full `SdfPath` strings stays deferred (gap-tracker §1 / Round B) since
  the trace doc does not pin the descent arithmetic, so no un-grounded
  path string is fabricated. New fixture test asserts 248 elements, the
  `target_index` permutation, every `element_token_index` in pool range,
  and three spot-checked element names + jump words.

- usdc §4.3: **typed `ValueRegion` accessors** — `as_f32` / `as_i32` /
  `as_u32` / `as_bool` bit-cast an inline payload, and
  `array_elements_exact(width)` trims an uncompressed array's element
  bytes to `count × width` (the caller supplies the type-code-dependent
  width). The Elephant fixture's inline `0x4009` rep decodes to the
  `float` `60.0` and its `0x0f` `double[]` array reads back with `1.0`
  as the first element — both asserted by new tests.
- usdc §4.3: **value-region resolution** — `UsdcFile::value_region`
  turns a `ValueRep` into a `ValueRegion`: `Inline(payload)` for inline
  scalars, `ScalarOffset` for offset scalars, `Array { count, elements }`
  for uncompressed arrays (parses the leading `u64` count and borrows
  the raw element bytes — element *width* stays type-code dependent), and
  `CompressedArray { count, region_offset }` for compressed arrays
  (count + offset; full decode needs the deferred type-code width).
  Array offsets and count headers are bounds-checked against the value
  region `[0x58, toc_offset)`. Grounded by a synthetic round-trip plus a
  fixture test that resolves every Elephant FIELDS rep.
- usdc §4.3: observer-grounded **ValueRep flag decomposition**. The
  rep word's top byte (bits 5/6/7) is now decoded into
  `ValueRep::is_array` (bit 63), `is_inline` (bit 62) and
  `is_compressed` (bit 61), and the low byte is surfaced as the raw
  `type_code()` (its *naming* stays GAP-TRACKER Round B). Bit
  positions are pinned from the Elephant fixture: across all 157
  reps the flag byte only ever takes `0x00/0x40/0x80/0xa0`, array and
  inline are mutually exclusive, compressed only rides on array, and
  every array rep addresses an in-file `[u64 count][elements]` region
  — all asserted by a new fixture-grounded invariant test.
- usdc writer: new `usdc_writer::CrateImage` — a clean-room **USDC
  "Crate" structural writer**. `CrateImage::from_file` decodes the six
  standard sections of a parsed `.usdc` into a content image;
  `CrateImage::to_bytes` re-serialises it (bootstrap + six section
  payloads in canonical order + tail TOC, §3a single-chunk LZ4 + §3b
  integer coding) so the result re-decodes byte-exactly through the
  reader. The committed Elephant fixture round-trips structurally
  (192 tokens / 157 fields / 576 field-set indices / 248 paths / 248
  specs all reproduced), with the §4.3 value-rep words riding through
  opaque (the type-code enumeration is not staged — GAP-TRACKER Round
  B). Synthetic and empty images round-trip too; cross-array length
  mismatches are rejected before any bytes are produced.
- usdc writer authoring surface: `CrateImage::from_bytes` (parse +
  decode in one call), `intern_token` (single-interning per trace doc
  §4.1), `add_field` (intern name + append `(name, valueRep)`), and
  `field_name` (resolve a field's name back through the token pool).
  These enable **read-modify-write**: the committed Elephant fixture
  loads, gains an authored field, and re-emits to a valid file where
  the new field survives alongside the original 157 untouched.
- usdc §3a/§3b: production `encode_compressed_buffer` (single-chunk
  LZ4 framing) and `encode_int_coded` (the inverse of
  `decode_int_array`, including the 4-byte common-delta preamble) are
  now public, the byte-exact inverses of the read path.
  `encode_int_array_for_tests` is now a thin alias of `encode_int_coded`.
- usdc §4.3: `ValueRep` + `FieldsSection::decode_value_reps` split each
  packed `uint64` FIELDS value-rep word into its documented two-part
  shape — the high-16-bit type-enum + flags word and the low-48-bit
  inline-or-offset payload — the byte boundary read straight off the
  trace doc's §4.3 hex excerpt. `ValueRep::from_raw` / `ValueRep::raw`
  are exact inverses (lossless relative to `decode_reps`). The
  per-value type-code enumeration (which type-enum, which flag bits,
  inline-vs-offset) stays deferred to the gap tracker's Round B
  fact-table extraction. Cross-validated against the real Elephant
  fixture's first eight rep words.

### Other

- usdc §3b: `decode_int_array` now rejects an int-coded buffer that
  leaves payload bytes unconsumed after the last element. The trace
  doc §3b records every buffer as consuming its variable-width payload
  exactly ("zero leftover bytes"); trailing slack signals a mis-framed
  or corrupt buffer (e.g. a `compressedSize` that over-reads into the
  next section, or a control stream that under-counts wide codes) and
  now fails with a clear `InvalidData` instead of silently dropping
  the extra bytes. All eight real Elephant-fixture buffers continue to
  decode (they consume their payload exactly, as documented).
- usdc §5: bounds-check each spec row's `pathIndex` against the §4.5
  PATHS `numPaths` in `decode_specs` — a corrupt §4.6 path-index column
  referencing a non-existent namespace path now fails with a clear
  `InvalidData` rather than producing a dangling `SdfPath` reference.
- usdc: end-to-end §3a → LZ4 → §3b section-content regression test
  against the committed Elephant fixture (192 tokens / 157 fields /
  576 field-set indices / 248 paths / 248 specs); correct the
  `decode_specs` doc-comment (`path_index` is a permutation of
  `0..248`, identity for the first 36 rows, not the full identity map).

## [0.0.3](https://github.com/OxideAV/oxideav-usdz/compare/v0.0.2...v0.0.3) - 2026-06-15

### Other

- usdc §5 step 3: STRINGS → TOKENS pool resolution (decode_strings)
- usdc r303: §5 step 3+7 field-name TOKENS resolution (decode_named_specs / NamedSpec)
- §1 version-compatibility dispatch gate
- usdc §5 step 7: SPECS → FIELDSETS → FIELDS resolved-spec join
- usdc §3a LZ4 block layer + §3b common-delta preamble + end-to-end typed section decoders
- usdc §4.5 PATHS: split the three §3a compressed buffers
- Round 265: USDC TOC standard-section table accessors
- drop release-plz.toml — use release-plz defaults across the workspace
- Round 245: USDC TOC canonical-order predicate + section-bytes accessor
- Round 239: USDC §4.6 SPECS section three-buffer framing parser
- Round 236: USDC §4.5 PATHS section leading-prefix parser
- replace possessive vendor references with neutral language
- Round 229: USDC §4.4 FIELDSETS section framing parser
- Round 222: USDC §4.3 FIELDS section framing parser
- Round 217: USDC §4.2 STRINGS section parser
- Round 212: USDC §3a compressed-buffer framing + §4.1 TOKENS section header
- Round 206: USDC §3b compressed-integer decoder
- Round 15: USDC binary-format primitives — bootstrap + Table of Contents

### Added — Round 312 (USDC §5 step 3 — STRINGS → TOKENS pool resolution)

- **`usdc::UsdcFile::decode_strings(file_bytes)`** — completes the
  trace doc §5 step-3 "load STRINGS indices" join to its
  string-resolved form. The §4.2 `STRINGS` section is a flat
  `count × uint32` array of token indices; per §4.2 the pool is the
  subset of §4.1 `TOKENS` atoms used as string-typed values. The
  method composes the two indirections (`STRINGS[i]` is a token index,
  `TOKENS[that]` is the UTF-8 atom) and returns the resolved strings in
  pool order — the same `index → string` lookup `decode_named_specs`
  performs for field names. `TOKENS` and `STRINGS` are each decoded
  once. The Elephant fixture's documented zero-count `STRINGS` section
  resolves to an empty vector without a `TOKENS` decode; an absent
  `STRINGS`/`TOKENS` section, or an index past the `TOKENS` pool, is an
  `Error::InvalidData`.
- Cross-validated on the committed Elephant fixture (`STRINGS`
  count = 0 → empty pool) plus synthetic populated, out-of-range, and
  missing-section cases.

### Added — Round 303 (USDC §5 step 3 + 7 — field-name `TOKENS` resolution)

- **`usdc::UsdcFile::decode_named_specs(file_bytes)`** — carries the
  trace doc §5 reader pipeline one step past `decode_specs`: loads the
  §4.1 `TOKENS` atom pool (step 3) and rewrites each resolved spec
  row's `(fieldNameTokenIndex, valueRep)` pairs into
  `(fieldName, valueRep)` by indexing the pool, returning the new
  **`usdc::NamedSpec`** struct. `TOKENS` / `FIELDS` / `FIELDSETS` /
  `SPECS` are each decoded once. Only field names are humanised;
  `value_rep`, `path_index` and `spec_type` stay raw (type-code
  enumerations remain gap-tracker Round B). An absent `TOKENS` section,
  or a field-name token index out of pool range, is an
  `Error::InvalidData`.
- Cross-validated on the committed Elephant fixture: 248 named specs,
  row structure identical to `decode_specs`, every named field equal to
  its `TOKENS` pool entry, and the root prim (path 0) naming its eight
  metadata fields in field order.

### Added — Round 297 (USDC §1 version-compatibility dispatch gate)

- **`usdc::Version::READER_MAX`** — the highest `(major, minor)` this
  reader has behaviour for, set to the trace doc's only observed
  version `0.8.0`.
- **`usdc::Version::is_readable_by` / `usdc::Version::is_readable`** —
  the trace doc §1 dispatch rule ("the version is the **only** dispatch
  key — a reader compares `(major, minor)` and refuses files it is too
  old to understand"). A file is readable iff its `(major, minor)`
  does not exceed the reader's; patch is excluded from the gate (not
  part of the dispatch key). `(major, minor)` is compared
  lexicographically, so a major bump dominates any minor.
- **`usdc::UsdcFile::parse`** now refuses a forward-incompatible file
  up front (before TOC parse) with an `Unsupported` error rather than
  a downstream structural error, when the file version's dispatch key
  exceeds `READER_MAX`. Equal/older `(major, minor)` parse unchanged;
  the committed `0.8.0` Elephant fixture is unaffected.

### Added — Round 290 (USDC §5 step 7 SPECS → FIELDSETS → FIELDS resolved join)

- **`usdc::UsdcFile::decode_specs`** — materialises the trace doc §5
  "how a reader uses it" pipeline into one **`usdc::ResolvedSpec`**
  per spec row: each §4.6 SPECS row's
  `(pathIndex, fieldSetOffset, specType)` triple is joined and the
  row's field set expanded into its concrete
  `(fieldNameTokenIndex, valueRep)` property list via §4.4 FIELDSETS
  → §4.3 FIELDS. The §4.6 middle buffer's field-set index is a
  **flat offset into the concatenated FIELDSETS array** (the run
  start), not an ordinal "Nth set" number — confirmed against the
  committed Elephant fixture (all 248 rows' indices land on a run
  boundary; the largest, 570, exceeds the 113 distinct sets).
- **`usdc::field_set_at`** — reads one field-set run from a flat
  FIELDSETS offset up to the next `-1` sentinel (companion to
  `usdc::split_field_sets`, which splits the whole array).
- Cross-validated on the Elephant fixture: 248 resolved specs,
  `path_index` the identity permutation `0..248`, spec-type codes
  `{1, 6, 7, 8}`, every resolved `(name, rep)` pair traceable to the
  FIELDS table and every name token a valid TOKENS-pool index.

### Added — Round 282 (USDC §3a LZ4 layer + §3b common-delta preamble + typed section decoders)

- **`usdc::CompressedBuffer::decompress` / `decompress_exact`** —
  the §3a wrapper's inner LZ4 *block* layer, delegated to the new
  `compcol` dependency (Karpelès Lab's compression collection,
  `lz4` feature only) under a caller-supplied output bound
  (decompression-bomb guard). Multi-chunk buffers concatenate in
  declaration order. `usdc::int_coded_max_len` supplies the §3b
  arithmetic budget.
- **End-to-end typed section decoders** chaining §3a → LZ4 → §3b:
  `TokensSection::decode`, `FieldsSection::decode_name_indices` /
  `decode_reps`, `FieldSetsSection::decode_flat_indices` /
  `decode_field_sets`, `SpecsSection::decode_path_indices` /
  `decode_fieldset_indices` / `decode_spec_types`, and the three
  PATHS raw-stream decoders (`decode_path_token_ints` /
  `decode_element_token_ints` / `decode_jump_ints`). Validated
  end-to-end against the committed Elephant fixture: 192 tokens,
  157 field (name, rep) pairs with the trace doc's §4.3 rep-word
  hex excerpt matched verbatim, 576 sentinel-separated field-set
  indices, 248 spec rows whose path indices form an exact
  permutation of `0..numPaths`.

### Changed — Round 282

- **`usdc::decode_int_array` now consumes the §3b stream's leading
  `int32` common-delta preamble**; control code `0` decodes to
  *previous + common delta* (the previously-implemented form is the
  `commonDelta = 0` special case). This resolves the trace doc's
  §4.4/§4.5 "common value" caveat empirically: all eight int-coded
  buffers of the Elephant fixture decode exactly (zero leftover
  bytes) and yield semantically coherent indices under the
  corrected model, where the preamble-less reading yields
  out-of-range/negative values. `usdc::encode_int_array_for_tests`
  emits the preamble (picking the most frequent delta as the
  common delta).

### Added — Round 273 (USDC §4.5 PATHS three-buffer framing)

- **`usdc::PathsSection` now splits the three §3a compressed
  buffers.** Round 236 landed the §4.5 PATHS section's 16-byte
  leading prefix (`int64 numPaths` + repeated count) but surfaced
  the trailing region as a single opaque `tail_bytes` slice,
  because the trace doc's then-stated single-buffer count did not
  exhaust the Elephant fixture's 532 trailing bytes. The docs
  collaborator has since corrected §4.5 to record **three**
  `(int64 compressedSize, §3a buffer)` triples — the parallel
  arrays of the namespace path tree: path-token indices,
  element-token indices, and sibling/child "jump" offsets.
  `PathsSection::parse` now reads all three, enforcing
  `16 + 8 + csize₁ + 8 + csize₂ + 8 + csize₃ == section_size`
  exactly — trailing bytes the trace doesn't authorise are an
  `Error::InvalidData`. The opaque `tail_bytes` field is replaced
  by `path_tokens_buffer_bytes` / `element_tokens_buffer_bytes` /
  `jumps_buffer_bytes` (plus their `*_compressed_size` companions)
  and the `path_tokens_buffer` / `element_tokens_buffer` /
  `jumps_buffer` accessors forwarding each bounded slice to
  `CompressedBuffer::parse`, mirroring the §4.3 FIELDS and §4.6
  SPECS multi-buffer framing. Defensive caps on the buffer sizes
  (256 MiB) cut off a hostile or corrupted prefix before
  allocation.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's PATHS entry sits at
  `offset = 0x0cf92b` with `size = 548`, the header parses to
  `num_paths = 248`, the three buffer prefixes carry
  `csize₁ = 266`, `csize₂ = 145`, `csize₃ = 97`, and the total
  footprint `16 + 8 + 266 + 8 + 145 + 8 + 97 = 548` matches the
  section size on the wire; each buffer is walkable as a §3a
  `CompressedBuffer` envelope.
- The §3b per-element tree-walk reconstruction (the buffers go
  through the common-value fast path) remains a separate
  follow-up; this round lands the framing only.

### Added — Round 265 (USDC TOC standard-section table accessors)

- **`usdc::Toc::standard_section_table`** — one-pass classifier
  projecting `Toc::entries` onto a fixed-size
  `[Option<&TocEntry>; 6]` indexed by
  `SectionName::canonical_index`. Each slot at `i =
  name.canonical_index()` holds `Some(entry)` when the TOC
  carries a section of that name (first occurrence wins, matching
  `Toc::find`'s contract) or `None` when absent. Trailing entries
  with non-standard names (per trace doc §2 the TOC name field is
  open-ended) are silently ignored.
- **`usdc::UsdcFile::standard_section_table(file_bytes)`** — the
  bytes-borrowing companion: composes the TOC's
  `standard_section_table` with `TocEntry::slice_in` in a single
  pass, returning `[Option<&[u8]>; 6]` borrowing each present
  standard section's payload bytes out of `file_bytes`. A slot is
  `None` if the standard section is absent from the TOC, or if
  `file_bytes` is shorter than the entry's recorded range (the
  same defensive fallback `slice_in` provides on its own).
- **Why a bulk accessor in addition to Round 245's per-name
  ones.** `Toc::matches_canonical_order` answers "is the TOC
  well-ordered?"; the new `standard_section_table` answers "for
  each standard section, where is its entry?" — useful when the
  predicate doesn't hold (a future writer reordering entries, or
  a hand-authored file with non-standard names interleaved) and
  the reader still needs every standard section located in one
  walk of the TOC instead of six separate `Toc::find` calls.
- **7 new unit tests** in `usdc::tests` cover the canonical-six
  filled-every-slot case (with pointer-identity witnesses for the
  zero-clone borrow), the skip-unknown-names path on a TOC mixing
  out-of-order standard names with non-standard ones,
  duplicate-name first-wins semantics, the all-`None`
  empty-TOC case, the `UsdcFile` bytes accessor borrowing every
  payload (again pointer-identity-verified), the missing-sections
  partial-`None` projection, the truncated-source-yields-`None`
  defensive fallback for entries the shorter slice can't cover,
  and a fixture-backed cross-validation against the in-tree
  Elephant fixture confirming the bulk accessor and the per-name
  `section_bytes` are observationally identical on real bytes
  (pointer identity + length parity for all six standard
  sections). Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §2 (TOC layout, six
  standard section names, canonical ordering, and Elephant
  per-section offsets + sizes).

### Added — Round 245 (USDC TOC canonical-order predicate + section-bytes accessor)

- **`usdc::SectionName::ALL_STANDARD`** — the canonical six standard
  section names in trace doc §2's observed declaration order
  (`TOKENS`, `STRINGS`, `FIELDS`, `FIELDSETS`, `PATHS`, `SPECS`).
  The trace doc grounds this ordering in two independent real
  samples — quoting §2: "The six names appear in this same order
  in the teapot too." Companion
  **`usdc::SectionName::canonical_index`** returns the zero-based
  position of each variant in that sequence, giving a reader
  walking the six standard sections by canonical index a typed
  lookup.
- **`usdc::TocEntry::slice_in(file_bytes)`** — borrow a TOC entry's
  payload bytes from a full USDC file slice. The `(offset, size)`
  pair was bounds-checked by `Toc::parse` against the file length
  and the TOC offset at parse time, so this lookup returns the
  clean borrow into the original input. Returns `None` when
  `file_bytes` is shorter than the entry's recorded range (defensive
  fallback for callers holding the entry independently of the
  source slice).
- **`usdc::Toc::matches_canonical_order`** — fast-path predicate:
  returns `true` when the TOC's first six entries classify, in
  declaration order, as the six canonical variants
  `SectionName::ALL_STANDARD`. Trailing non-standard entries beyond
  the canonical six are tolerated (the TOC name field is open-ended
  per the trace doc). When the predicate holds, a reader can address
  each standard section by its canonical index directly into
  `Toc::entries` without re-running `Toc::find` per access.
- **`usdc::UsdcFile::section_bytes(name, file_bytes)`** — single-call
  convenience that composes `Toc::find` + `TocEntry::slice_in` so
  callers can pull any standard section's bytes out of a parsed
  file in one step. Returns `None` when the requested standard
  section is absent from the TOC or when `file_bytes` is shorter
  than the entry's range.
- **11 new unit tests** in `usdc::tests` cover the canonical
  ordering against `ALL_STANDARD`, the `canonical_index` /
  `ALL_STANDARD` round-trip on every variant, the
  `matches_canonical_order` predicate on a synthesised six-section
  USDC file, the same predicate's rejection of a FIELDS / FIELDSETS
  swap and of a five-entry truncated TOC, the trailing-extras
  tolerance with a synthesised `EXTRA`-named seventh entry,
  `TocEntry::slice_in` borrowing the right payload from each
  standard entry and returning `None` on a truncated re-read,
  `UsdcFile::section_bytes` round-tripping each canonical name on
  the synthesised input, the same accessor returning `None` for an
  absent SPECS, and two fixture-backed cross-validations against
  the in-tree Elephant fixture: (a)
  `Toc::matches_canonical_order` holds on the real file's six TOC
  entries and (b) `UsdcFile::section_bytes` returns the right
  pointer-identity slice at each of the trace doc's six published
  `(offset, size)` pairs (TOKENS @0x0cebf0 size 1770, STRINGS
  @0x0cf2da size 8, FIELDS @0x0cf2e2 size 998, FIELDSETS @0x0cf6c8
  size 611, PATHS @0x0cf92b size 548, SPECS @0x0cfb4f size 331).
  Sourced exclusively from `docs/3d/usd/usdc-crate-format-trace.md`
  §2 (TOC layout, canonical ordering of the six standard sections
  in both real samples, and the per-section offsets + sizes).

### Added — Round 239 (USDC §4.6 SPECS section three-buffer framing)

- **`usdc::SpecsHeader` / `usdc::SpecsSection`** — the §4.6 SPECS
  section's outer three-buffer framing. Trace doc §4.6 records the
  section opens with an 8-byte little-endian `int64 count`
  immediately followed by **three** `(int64 compressedSize, §3a
  buffer)` triples. The three decompressed buffers carry, in
  order, `count × i32` path indices (into the §4.5 PATHS namespace
  tree), `count × i32` field-set indices (into the §4.4 FIELDSETS
  array), and `count × i32` spec-type codes (a small enum
  identifying prim / attribute / relationship / …). Each spec row
  is therefore the join `(pathIndex, fieldSetIndex, specType)`, so
  the SPECS section is the table a reader iterates to materialise
  the stage. `SpecsHeader::parse` reads the 8-byte count and
  bounds it under a defensive cap (16 Mi — several orders of
  magnitude above the Elephant fixture's 248); `SpecsSection::parse`
  slices each `(compressedSize, bytes)` triple under a strict
  `8 + 3*(8 + compressedSize) == section_size` invariant.
  `SpecsSection::paths_buffer` / `fieldsets_buffer` /
  `types_buffer` forward to `CompressedBuffer::parse` on each
  bounded buffer slice ready for §3a / §3b chained decoding once
  the LZ4 block decoder lands. The spec-type enumeration that the
  third buffer's `i32`s eventually resolve into is its own
  fact-table extraction and stays deferred. Sourced exclusively
  from `docs/3d/usd/usdc-crate-format-trace.md` §4.6 (header
  layout + Elephant numbers) and §2 (TOC entry
  `offset = 0x0cfb4f`, `size = 331`).
- **10 new unit tests** in `usdc::tests` cover §4.6 header parsing
  on the Elephant `count = 248` worked example, header truncation,
  the oversized-count rejection against the defensive cap, the
  synthesised Elephant-shape section
  (`8 + 8 + 60 + 8 + 200 + 8 + 39 = 331`), the zero-count minimal
  framing (`8 + 3 × 8` total), truncated-csize-prefix error on the
  second buffer, the third buffer overrunning the section,
  trailing-bytes rejection, header-truncation propagation through
  `SpecsSection::parse`, and the §3a single-chunk round-trip on
  each of the three buffer forwarders. An additional
  `real_fixture_specs_section_parses` test cross-validates against
  the in-tree Elephant fixture: the trace doc's `count = 248` and
  the three `compressedSize` values 60 / 200 / 39 are recovered
  from the wire bytes; the three buffer slices are non-overlapping
  borrows into the section at the documented offsets.

### Added — Round 236 (USDC §4.5 PATHS section leading-prefix framing)

- **`usdc::PathsHeader` / `usdc::PathsSection`** — the §4.5 PATHS
  section's 16-byte leading prefix. Trace doc §4.5 records the
  section opens with `int64 numPaths` immediately followed by a
  second `int64` that repeats the same count (both observed as
  `0x000000F8` = 248 on the Elephant fixture); the rest of the
  section holds the namespace path tree as one or more §3a
  compressed buffers. `PathsHeader::parse` reads the two int64s,
  enforces the repeat-equals-numPaths invariant the trace doc
  grounds in the bytes, and applies a defensive cap on `numPaths`
  (16 Mi — several orders of magnitude above the Elephant's 248)
  to bound downstream allocation against a hostile or corrupted
  header. `PathsSection::parse` surfaces the trailing region after
  the 16-byte prefix as an opaque `tail_bytes` slice through
  [`PathsSection::tail_bytes`] so a future round can layer the
  compressed-buffer decomposition on top once the trace doc
  resolves the buffer-count question (see the docs-gap note
  below). Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §4.5 (header layout +
  Elephant `numPaths = 248`) and §2 (TOC entry `offset =
  0x0cf92b`, `size = 548`).
- **8 new unit tests** in `usdc::tests` cover §4.5 header parsing
  on the Elephant `num_paths = 248` worked example with the
  matching repeat int64, the short-buffer error path, the
  oversized-numPaths rejection against the defensive cap, a
  deliberate repeat-mismatch (numPaths = 248 / repeat = 247) that
  exercises the trace doc's invariant, the synthesised Elephant-
  shape section (16-byte header + 532 opaque trailing bytes = 548
  total), the zero-count minimal section, header-truncation
  propagation through `PathsSection::parse`, and repeat-mismatch
  propagation through `PathsSection::parse`.
- **Real-fixture cross-validation** —
  `real_fixture_paths_section_parses` reads
  `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc` through
  `UsdcFile::parse`, finds the PATHS TOC entry (offset
  `0x0cf92b`, size `548`), parses the section, and asserts the
  trace doc's published numbers verbatim — `num_paths = 248`,
  trailing byte count = `548 - 16 = 532`, plus a borrow-identity
  check that `tail_bytes` is a sub-slice of the input section.
  Skips silently when the fixture is absent so CI remains green
  in submodule-less checkouts.

### Docs gap

- **PATHS section trailing-buffer count is undetermined.** The
  trace doc §4.5 narrative records "one compressed buffer" after
  the 16-byte header, but the Elephant fixture's 532 trailing
  bytes do not match a single `(int64 compressedSize, body)` pair
  whose `compressedSize` reads `266` at offset `+16` (`24 + 266 =
  290` consumed, well short of `548`). An empirical scan of the
  trailing bytes finds two further `(int64 size, body)` framings
  at the expected boundaries that together close the section,
  consistent with the three-buffer pattern the trace doc records
  for §4.6 SPECS (path indices, element-token indices,
  sibling/child jump offsets — the three parallel arrays
  referenced in the §4.5 narrative). The framing landing in this
  round therefore stops at the 16-byte leading prefix; the §3a
  envelope is NOT applied to `tail_bytes` until the trace doc
  reconciles the buffer-count claim with the bytes. Recommended
  trace-doc patch: change §4.5 to record "three §3a compressed
  buffers" (mirroring §4.6's table form) so a follow-up round
  can layer `(buffer, header.num_paths)` decompression + §3b
  decoding on the three arrays.

### Added — Round 229 (USDC §4.4 FIELDSETS section framing + sentinel-split helper)

- **`usdc::FieldSetsHeader` / `usdc::FieldSetsSection`** — the §4.4
  FIELDSETS section's outer framing. The section is an 8-byte
  little-endian `int64 count` header followed by a single
  `(int64 compressedSize, §3a buffer)` pair. The decompressed
  buffer, once the LZ4 block decoder lands, feeds `count` `i32`
  values through the §3b integer decoder; the trace doc records
  that the array is the concatenation of per-set field-index runs
  separated by a `-1` (`0xFFFFFFFF`) sentinel — each spec (§4.6)
  references one such set to get its full property list.
  `FieldSetsHeader::parse` reads the 8-byte count;
  `FieldSetsSection::parse` enforces
  `8 + 8 + compressedSize == section_size` strictly so any trailing
  byte the trace doesn't authorise is an `Error::InvalidData`;
  `FieldSetsSection::buffer` forwards to `CompressedBuffer::parse`
  on the bounded buffer slice for callers walking the §3a framing
  ahead of the LZ4 block decoder. Defensive caps on `count`
  (16 Mi) and on the buffer's `compressedSize` (256 MiB) protect
  against runaway allocation from a hostile or corrupted header.
  The trace doc's §4.4 caveat — that a naive §3b decode of the
  buffer recovers the run structure but the literal values need a
  separate "common value" step — is recorded in the module docs;
  this round deliberately stops at the framing.
- **`usdc::split_field_sets`** — small companion helper that splits
  a flat `&[i32]` (the eventual output of the §3b decoder on the
  decompressed buffer) into one `Vec<i32>` per field set, dropping
  the `-1` sentinels. Accepts a trailing run without a sentinel
  (trace doc leaves that case unconstrained) and surfaces a
  leading sentinel as an initial empty set; consecutive sentinels
  produce empty sets between non-empty ones. Lets the run-split
  semantics get exercised today on synthesised input without
  first wiring the LZ4 block decoder.
- **15 new unit tests** in `usdc::tests` cover §4.4 header parsing
  on the trace's Elephant `count = 576` worked example, the
  truncated-input error path, the oversized-count rejection
  against the defensive cap, the synthesised Elephant-shape
  section (count = 576, compressedSize = 595, section size = 611),
  the zero-count minimal section, end-to-end §3a forwarding into
  `CompressedBuffer::parse` on a two-chunk buffer, the
  truncated-prefix error path, the oversize-csize error path, the
  short-body error path, the strict trailing-byte rejection,
  header-truncation propagation through `FieldSetsSection::parse`,
  the over-cap defensive `compressedSize` rejection, plus five
  `split_field_sets` cases (empty input, two sentinel-terminated
  runs, unterminated trailing run, leading sentinel as empty set,
  consecutive sentinels as inner empty sets).
- **Real-fixture cross-validation** —
  `real_fixture_field_sets_section_parses` reads
  `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc` through
  `UsdcFile::parse`, finds the `FIELDSETS` TOC entry (offset
  `0x0cf6c8`, size `611`), parses the section, and asserts the
  trace doc's published numbers verbatim — `count = 576`,
  `compressedSize = 595`, plus the `8 + 8 + 595 = 611`
  total-footprint invariant on the wire. Walks the §3a framing to
  confirm the buffer has at least one chunk. Sourced exclusively
  from `docs/3d/usd/usdc-crate-format-trace.md` §4.4.

### Added — Round 222 (USDC §4.3 FIELDS section framing)

- **`usdc::FieldsHeader` / `usdc::FieldsSection`** — the §4.3
  FIELDS section's outer two-buffer framing. The section is an
  8-byte little-endian `int64 numFields` header followed by two
  `(int64 compressedSize, §3a buffer)` pairs: the first buffer's
  decompressed form is an int-coded array (§3b) of `numFields`
  token indices giving each field its name (an index into the
  §4.1 TOKENS atom pool); the second buffer's decompressed form
  is `numFields × uint64` packed value-rep entries — a packed
  representation carrying type code, flags (`is-array`,
  `is-inlined`, `is-compressed`), and either an inline value or
  a file offset to the value's bytes elsewhere in the file.
  `FieldsHeader::parse` reads the 8-byte numFields;
  `FieldsSection::parse` enforces
  `8 + 8 + csize₁ + 8 + csize₂ == section_size` strictly so any
  trailing byte the trace doesn't authorise is an
  `Error::InvalidData`; `FieldsSection::names_buffer` /
  `reps_buffer` forward to `CompressedBuffer::parse` on the
  bounded buffer slices for callers walking the §3a framing
  ahead of the LZ4 block decoder. Defensive caps on `numFields`
  (16 Mi) and on either buffer's `compressedSize` (256 MiB)
  protect against runaway allocation from a hostile or corrupted
  header. Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §4.3.
- **12 new unit tests** in `usdc::tests` cover the §4.3 header
  parsing on the trace's Elephant `numFields = 157` worked
  example, the truncated-input error path, the
  oversized-numFields rejection against the defensive cap, the
  zero-fields synthetic minimal section, end-to-end section
  parsing on the trace's three documented `(numFields, csize₁,
  csize₂)` values, §3a forward-to-`CompressedBuffer` on both
  buffers, truncated-prefix error paths for both buffers,
  oversize-csize error paths for both buffers, the strict
  trailing-byte rejection, header-truncation propagation through
  `FieldsSection::parse`, and the over-cap defensive
  `compressedSize` rejection.
- **Real-fixture cross-validation** —
  `real_fixture_fields_section_parses` reads
  `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc` through
  `UsdcFile::parse`, finds the `FIELDS` TOC entry (offset
  `0x0cf2e2`, size `998`), parses the section, and asserts the
  trace doc's published numbers verbatim — `numFields = 157`,
  `csize₁ = 141`, `csize₂ = 833`, plus the `8 + 8 + 141 + 8 +
  833 = 998` total-footprint invariant on the wire. Walks the §3a
  framing on both buffers to confirm each compressed buffer has
  at least one chunk.

### Added — Round 217 (USDC §4.2 STRINGS section parser)

- **`usdc::StringsHeader` / `usdc::StringsSection`** — the §4.2
  STRINGS section: an 8-byte little-endian `int64 count` header
  followed by `count × uint32` (raw, NOT LZ4-compressed per the
  trace doc) little-endian token indices. The STRINGS pool is the
  subset of TOKENS atoms used as USDA *string-typed* values —
  §4.3 FIELDS string-valued reps index into this table by `uint32`
  position. `StringsHeader::parse` reads the 8-byte count;
  `StringsSection::parse` enforces `8 + count * 4 == section_size`
  strictly so any trailing byte the trace doesn't authorise is an
  `Error::InvalidData`; `StringsSection::parse_indices` decodes
  the raw `uint32` array into an owned `Vec<u32>` for callers that
  want to walk the pool. Defensive cap on `count` (16 Mi, several
  orders of magnitude above the teapot trace's `count = 15`)
  protects against runaway allocation from a hostile or corrupted
  header. Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §4.2.
- **8 new unit tests** in `usdc::tests` cover the §4.2 header
  parsing for `count = 0` (Elephant's empty case) and the
  truncated-input error path, the oversized-count rejection
  against the defensive cap, the empty-section happy path, the
  trace's teapot-shape worked example (15 sequential `uint32`s
  walked through `parse_indices`), short-index-array and
  trailing-bytes error paths exercising the strict
  `header(8) + count * 4 == section size` invariant, and a
  short-header propagation case where `StringsSection::parse`
  bubbles up `StringsHeader::parse`'s truncation error.
- **Real-fixture cross-validation** —
  `real_fixture_strings_section_parses_zero_count` reads
  `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc` through
  `UsdcFile::parse`, locates the STRINGS TOC entry, and asserts
  the trace-doc-published `offset = 0x0cf2da` / `size = 8` facts
  hold on the wire — then parses the section and confirms
  `count = 0` (matching the trace's §4.2 worked example), no
  index bytes, and `parse_indices()` yields an empty vector.
  Skips silently when the fixture is absent so CI remains green
  in submodule-less checkouts.

### Added — Round 212 (USDC §3a compressed-buffer framing + §4.1 TOKENS section header)

- **`usdc::CompressedBuffer` / `CompressedChunk`** — the §3a
  outer "compressed buffer" framing: a leading byte that
  declares "extra chunks" (so `total = extra + 1`) followed
  either by the entire remainder as a single LZ4-block chunk
  (the common `0x00` case the trace doc cites for every
  observed buffer in both samples) or by `(int32 LE length,
  bytes)` chunk records. The LZ4 *block* payload inside each
  chunk is left opaque — the public LZ4 block-format spec is
  not staged under `docs/`, so this module stops at the
  envelope. Borrows from the input slice; bounded against
  truncated length prefixes, chunks running past end-of-buffer,
  and negative declared lengths. `as_single_chunk()` shortcut
  for the common path.
- **`usdc::TokensHeader` / `TokensSection`** — the §4.1 TOKENS
  section header: three little-endian `int64`s (`numTokens`,
  `uncompressedSize`, `compressedSize`) plus the bounded §3a
  compressed-buffer slice that follows it. Defensive caps on
  `numTokens` (16 Mi) and `uncompressedSize` (256 MiB) protect
  against runaway allocation from a hostile or corrupted
  header — both are several orders of magnitude above the
  Elephant sample's 192 / 4195 figures published in the trace
  doc. `TokensSection::parse` slices exactly
  `compressed_size` bytes after the 24-byte header so a
  caller never reads past the section bounds, and the
  convenience `buffer()` method forwards into
  `CompressedBuffer::parse`.
- **`usdc::split_tokens_blob`** — the cross-section seam for
  callers that have plugged in their own LZ4 decoder for the
  inner block: takes the *decompressed* TOKENS blob plus the
  original `TokensHeader`, verifies the
  `blob.len() == uncompressed_size` equality, NUL-splits into
  the recorded `numTokens` UTF-8 strings (accepting either a
  trailing NUL or a NUL-as-delimiter encoding — the trace
  doesn't constrain the trailing byte), and validates UTF-8
  per token. Returns `Vec<String>`.
- **17 new unit tests** in `usdc::tests` cover §3a empty-input
  rejection, the single-chunk `0x00` form, an empty
  single-chunk buffer, two- and three-chunk forms walking
  each `(int32 length, bytes)` record, three error paths
  (truncated length prefix, chunk overrun, negative declared
  length), the §4.1 header parsing the Elephant's
  `numTokens=192 / uncompressedSize=4195 / compressedSize=1746`
  triple from the trace doc, three header error paths
  (truncated buffer, oversize `numTokens`, oversize
  `uncompressedSize`), section-bounds clamping at
  `compressed_size`, a truncated-buffer error path, two
  `split_tokens_blob` happy paths (trailing-NUL and
  delimiter-only forms), three error paths (size mismatch,
  count mismatch, non-UTF-8 byte), and a 14-token round-trip
  exercising the §4.1 trace-doc-published string set
  (`defaultPrim`, `endTimeCode`, `xformOp:transform`, …).
- **Real-fixture cross-validation** — `real_fixture_tokens_section_header_parses`
  reads `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc`
  through `UsdcFile::parse`, locates the TOKENS TOC entry,
  parses the section header, and asserts the
  `numTokens=192 / uncompressedSize=4195 / compressedSize=1746`
  trace-doc facts hold on the real bytes — plus
  `header(24) + compressed_size = section.size` (1770) and
  that the §3a framing parses to a single chunk whose payload
  is exactly `compressed_size - 1` bytes long (the LZ4 block
  proper). Skips silently when the fixture is absent so CI
  remains green in submodule-less checkouts.

### Added — Round 206 (USDC §3b compressed-integer decoder)

- **`usdc::decode_int_array`** — decodes the §3b "compressed
  integer" wire shape: a 2-bit-per-element control stream
  (`ceil(N/4)` bytes, LSB-first) selecting one of four widths
  per element, then the variable-width payload bytes. Code 0
  repeats the previous value; codes 1 / 2 encode signed `i8` /
  `i16` deltas from the previous value; code 3 encodes an
  absolute `i32` that becomes the new "previous". Returns
  `Vec<i32>`; rejects a truncated control stream or short
  payload for the declared width with `Error::InvalidData`.
  This is the primitive consumed by FIELDS' name-index array,
  FIELDSETS' field-index array, and SPECS' three index arrays
  once the §3a LZ4 wrapper lands. Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §3b.
- **`usdc::encode_int_array_for_tests`** — test-only
  companion encoder so the §3b decoder can be exercised
  end-to-end against synthesised integer sequences (varied
  zero-delta / i8-delta / i16-delta / absolute-i32 patterns)
  without first committing a corpus of real `.usdc` byte
  buffers. Not part of the on-disk writer surface — it picks
  per-element widths greedily, which exercises every decode
  path but isn't necessarily byte-identical to what a reference
  writer would produce.
- **13 new unit tests** in `usdc::tests` cover the empty
  input, all-zero-delta packing, multi-element LSB-first
  control-byte layout, single `i8` / `i16` / `i32` elements,
  the "absolute resets prev" rule, negative-delta wrapping, a
  five-element sequence forcing two control bytes, a
  monotonic-token-index pattern mirroring the trace's
  decoded FIELDS name-index opening, and four error paths
  (short control stream, short `i8` / `i16` / `i32` payload).

### Added — Round 15 (USDC binary-format primitives: bootstrap + TOC)

- **`usdc` module** — bootstrap-header and Table-of-Contents
  primitives for the binary `.usdc` sibling of the USDA text
  format. Public surface: `Magic` (8-byte `PXR-USDC` signature),
  `Version` (`major.minor.patch` from bootstrap bytes
  `0x08`/`0x09`/`0x0A`, with reserved-bytes-zero validation),
  `Bootstrap` (the fixed 88-byte header including the absolute
  `toc_offset`), `SectionName` (enum over the six standard
  section names `TOKENS` / `STRINGS` / `FIELDS` / `FIELDSETS` /
  `PATHS` / `SPECS` observed in real samples), `TocEntry`
  (`(name, offset, size)` row in the tail TOC), `Toc::parse`
  (walks the TOC and validates that each declared section region
  lives inside the file, doesn't overlap the bootstrap header,
  and doesn't run into the TOC itself), and a one-call
  `UsdcFile::parse` that returns both bootstrap and TOC.
  Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md`, the project's own
  clean-room byte-level trace of real `.usdc` samples.
- **Decoder boundary check.** A `.usdc` Default Layer now has
  its bootstrap + TOC validated before the decoder surfaces
  `Error::Unsupported` — malformed `.usdc` payloads (truncated
  bootstrap, wrong magic, oversized `sectionCount`, section
  region running into the TOC, …) are caught at the container
  boundary as `Error::InvalidData` instead of leaking past
  layer-extension dispatch. The `Unsupported` message now also
  records the parsed version and section catalogue so callers
  can see how far the boundary check got.
- **Real-fixture cross-check.** `tests/usdc_unsupported.rs`'s
  new `binary_usdc_real_fixture_reports_trace_doc_facts` test
  parses the trace doc's Elephant fixture
  (`docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc`) and
  asserts the every-byte facts: version 0.8.0, six sections in
  the documented order, TOC at file offset `0x000cfc9a`, section
  sizes matching the trace doc's decoded table
  (`TOKENS=1770`, `STRINGS=8`, `FIELDS=998`, `FIELDSETS=611`,
  `PATHS=548`, `SPECS=331`).

### Added — Round 194 (variantSet-bound Material `usdcat`-flatten oracle)

- **`tests/variant_material_usdcat_oracle.rs`** — black-box cross-validation
  of the round-7 variantSet resolver against the `usdcat -f` flatten
  command on a feature surface combining variant resolution with the
  round-1 `UsdPreviewSurface` PBR mapping. A `def Material` declared
  inside a variantSet body (the "metallic" branch of a `finish` variant
  set) has to be materialised by the resolver, then the typed Scene3D
  pass has to pick up `inputs:diffuseColor` / `inputs:metallic` /
  `inputs:roughness` and bind the Mesh's `rel material:binding` to the
  resulting `MaterialId`. The test feeds the original USDA + the
  `usdcat -f`-flattened reference through `UsdzDecoder` (each wrapped in
  USDZ via the in-tree `common::build_usdz` helper) and asserts
  fingerprint parity: mesh count, primitive vertex count, material count,
  bound `base_color` / `metallic` / `roughness`. A second test flips the
  selection metadata to `"matte"` and verifies the bound material's PBR
  slots track the alternate variant body. Skip-early (NOT `#[ignore]`)
  when `usdcat` is absent — no `pxr` Python module is needed, so the
  oracle runs anywhere Apple USD Tools / the USD CLI is installed.

## [0.0.2](https://github.com/OxideAV/oxideav-usdz/compare/v0.0.1...v0.0.2) - 2026-05-29

### Other

- Round 14: CRC-32 integrity verification on the USDZ reader
- Round 13: in-layer inherits / specializes class-arc composition
- Round 12: defaultPrim consistency on the USDA writer
- round 11 — in-archive references / payload arc composition
- Round 10: anchored in-archive sublayer composition
- Round 9: composition-arc + layer-metadata round-trip on the writer
- Round 8: variant writer round-trip
- Round 7: variant selection composition

### Added — Round 14 (CRC-32 integrity verification on the reader)

- **STORED payloads are verified against the central-directory
  CRC-32.** The ZIP walker now reads the CRC-32 field each
  central-directory file header records (offset +16) and recomputes
  the CRC-32/ISO-HDLC of the stored payload, rejecting any mismatch
  with `Error::InvalidData` (`"USDZ entry '<name>' failed CRC-32
  check (stored 0x…, computed 0x…)"`). Until now the writer computed
  and emitted a correct CRC for every entry but the reader never
  checked it, so a single corrupted byte (bit-rot, a truncated copy,
  a mismatched STORED pass-through) would surface as a baffling USDA
  parse failure or a garbled texture rather than a clear
  container-integrity error. The check reuses the crate's own
  CRC-32 routine (`zip_writer::crc32`) so reader and writer can never
  drift, and the zero-length empty-file case (`crc32(b"") == 0`)
  verifies naturally. This is plain PKWARE-format structure — the
  CRC field is not USD-specific.
- **`zip::walk` unit tests** — `rejects_crc_mismatch` (a central
  directory advertising the wrong CRC for an otherwise-valid payload
  is rejected) and `accepts_empty_payload_crc` (a zero-length entry's
  all-zero CRC field passes). `tests/cube_usdz.rs::corrupted_payload_is_rejected_by_crc`
  flips a payload byte of a valid archive and asserts the public
  `UsdzDecoder` surfaces the CRC failure rather than parsing garbage.
- **Stale `spec_usdz.html` source citations removed.** The
  project-shipped `spec_usdz.html` was purged from `docs/3d/usd/` on
  2026-05-24 (clean-room policy — see `docs/3d/usd/README.md`). The
  five lingering `docs/3d/usd/spec_usdz.html` citations in `zip.rs`,
  `zip_writer.rs`, and `usd_to_scene.rs` are re-grounded to the
  surviving `docs/3d/usd/GAP-TRACKER.md` §3 (USDZ container) and to
  the neutral PKWARE-format / USD relative-asset-path descriptions;
  no behaviour change.

### Added — Round 13 (in-layer `inherits` / `specializes` composition)

- **In-layer class-hierarchy arc composition.** A prim carrying
  `inherits = </Class/Foo>` / `specializes = </Class/Foo>` (or a
  list of such paths) now has the targeted in-layer prim composed
  underneath it per the OpenUSD glossary's Inherits / Specializes
  definitions. The class-arc target is a prim path WITHIN the same
  layer (as distinct from `references` / `payload` which target
  external assets), so the composer walks the local layer's prim
  tree rather than opening another archive entry. The targeted
  prim's `type_name`, `kind`, children, and attrs ride up under the
  inheriting / specialising prim via the same `merge_prim_under`
  primitive that powers rounds 10 / 11 — local opinions win, then
  earlier list entries win over later ones (USD list order =
  strength order).
- **LIVRPS-aligned strength order.** Per the OpenUSD glossary
  LIVRPS ordering — L > I > V > R > P > S — `inherits` is composed
  BEFORE `references` / `payload` so a referenced opinion can never
  overwrite an inherited one; `specializes` is composed AFTER all
  other arcs so it can only fill in opinions that every stronger
  arc has left unsourced. Confirmed by paired test fixtures: a prim
  with `inherits` + `references` resolves to the inherited child;
  the same shape with `specializes` + `references` resolves to the
  referenced child.
- **Transitive composition.** A class prim that itself carries
  `inherits` / `specializes` opinions has its own arc composed
  before merging upward. So `</A> inherits </B>; Prop inherits </A>`
  results in Prop carrying every child contributed by `</B>` (per
  the glossary's "implied" inherited opinion chain).
- **Cycle break.** A `(prim-path)` visited set keyed on the
  normalised path scopes per-recursion so a class-arc cycle (`</A>`
  inherits `</B>` which inherits `</A>` again) terminates without
  looping. The cycle break still flips the consumed flag so the
  writer doesn't re-author an arc that resolves into a cycle.
- **Flatten-on-write + round-trip preservation.** A successfully-
  composed in-layer arc is stripped from the prim's metadata so the
  writer flattens the composed tree into a single layer, matching
  the round 10 / 11 sublayer + reference flatten-on-write policy.
  An arc whose target prim path doesn't resolve in the local layer
  stays authored so the writer round-trips the unresolved opinion
  verbatim (matching the external-/cross-package round-trip path
  for `references`). The audit trail of composed arcs lands on
  `Scene3D::extras["usd:composedInherits"]` and
  `Scene3D::extras["usd:composedSpecializes"]` as
  `"<prim-path>|inherits"` / `"<prim-path>|specializes"` entries.
- **`tests/class_hierarchy_composition.rs`** — 10-case suite:
  basic class inheritance pulls children up + audit stamp;
  local-child-wins-over-inherited; multi-target `inherits = [</A>,
  </B>]` strength order; inherits-beats-references; specializes-
  loses-to-references; transitive `</A>` → `</B>` chain; cycle
  termination; external (unresolvable) target round-trips; consumed
  arc flattens away on the writer; non-`Path` opinion value (a raw
  string for `inherits`) doesn't crash the composer.

### Fixed — Round 12 (`defaultPrim` consistency on the writer)

- **`defaultPrim` token tracks prim-name sanitisation.** The USDA
  writer sanitises prim names to the `[A-Za-z_][A-Za-z0-9_]*` USD
  grammar (e.g. `"My Cube"` → `def Xform "My_Cube"`), but the
  `defaultPrim` opinion preserved through `usd:layerMetadata` was
  emitted verbatim. A downstream resolver looking up the
  `defaultPrim` token then missed the only prim in the layer,
  silently breaking `references = @./scene.usda@` (selector-less)
  composition against our output. The writer now rewrites the token
  to the sanitised spelling when the raw form names a real root.
- **Dangling `defaultPrim` opinions are dropped.** When the
  preserved `defaultPrim` names a prim that no longer exists in the
  scene (downstream pipeline pruned it), the writer used to re-emit
  the dangling token. Strict USD validators reject layers whose
  `defaultPrim` resolves to nothing — we now drop the opinion (and
  fall back to synthesising one from the first surviving root, if
  any).
- **Missing `defaultPrim` is synthesised from the first root.** A
  freshly-constructed `Scene3D` (no `usd:layerMetadata`) emitted no
  `defaultPrim` at all, so cross-archive `references = @./scene.usda@`
  resolution against our output produced nothing. The writer now
  synthesises a `defaultPrim` opinion naming the first root's
  sanitised prim name whenever no preserved one survives. A
  genuinely rootless scene still emits no opinion (USD's `defaultPrim`
  is optional, and we don't invent a target out of thin air).

### Added — Round 11 (references / payload arc composition)

- **In-archive `references` / `payload` arc composition.** A prim
  carrying `references = @asset.usd@` / `payload = @asset.usd@`
  (optionally with a `</Prim>` selector) that targets an in-archive
  `.usd` / `.usda` layer now has the targeted prim composed
  underneath it per the OpenUSD glossary's References definition.
  The referencing prim keeps its own name — the "prim name-change"
  the glossary's `MarbleCollection` example describes — while the
  target's `type_name`, `kind`, children, and attrs ride up. A bare
  `references = @file@` targets the referenced layer's `defaultPrim`;
  `@file@</Prim>` targets the named prim (descending nested paths).
  Rounds 1..10 ignored both arcs on the read path: a prim authored
  purely as `def "Inst" ( references = @Marble.usd@ )` produced an
  empty node.
- **LIVRPS-aligned strength order.** Local opinions beat referenced
  opinions (`merge_prim_under`'s local-wins merge); `references`
  beats `payload` ("Payloads are weaker than references"); within a
  `[@a@, @b@]` list the earlier entry is stronger (USD list order =
  strength order). Composed strongest-first into the local prim so
  every weaker arc only fills gaps.
- **Transitive + cyclic composition.** A referenced layer that
  itself references composes transitively (its sub-references resolve
  against *its own* archive location). A `(layer-path, in-asset
  prim-path)` visited set breaks reference cycles so decode always
  terminates.
- **Flatten-on-write + round-trip preservation.** Consumed
  in-archive arcs are stripped from prim metadata so the writer
  flattens the composed tree into a single layer (matching round
  10's sublayer flatten-on-write policy). External / cross-package
  (`/abs`, `://`, `[...]`) and non-`.usd[a]` targets stay authored so
  the writer round-trips the unresolved opinion verbatim; a partly
  external list keeps the whole opinion. `.usdc` targets raise
  `Error::Unsupported` (binary-Crate docs gap). The audit trail of
  composed arcs lands on
  `Scene3D::extras["usd:composedReferences"]` as
  `"<layer>|references"` / `"<layer>|payload"` entries.
- **`tests/reference_composition.rs`** — 10 tests modelled on the
  glossary's `MarbleCollection` worked example plus boundary cases
  (defaultPrim targeting, `</Prim>` selector, references-beat-payload,
  list strength order, transitive nesting, external round-trip,
  `.usdc` rejection, cycle termination, USDZ→Scene3D→USDZ flatten).

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
  `variantSet "foo" = {}` blocks (the standard tooling emits these when
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
  explicitly. The reference-implementation C++ source
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
  metadata, `over`/`class` specs, the same-line-or-next-line
  brace layout, triple-quoted `doc` metadata.
- `tests/apple_oracle_local.rs` — opportunistic smoke test that
  builds an Apple-style USDZ via the system `usdcat` +
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
