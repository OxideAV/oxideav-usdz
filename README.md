# oxideav-usdz

Pure-Rust **USDZ** (Universal Scene Description, zipped) reader **and writer**.
USDZ is USD container for shipping ready-to-go USD assets — an
uncompressed, 64-byte-aligned ZIP archive whose first entry is a
`.usd` / `.usda` / `.usdc` "Default Layer" referenced by every
companion file (textures, audio, sub-layers). Apple Quick Look on
iOS / macOS uses USDZ as its native AR scene format.

The crate plugs into the
[`oxideav-mesh3d`](https://crates.io/crates/oxideav-mesh3d) typed
model — call `oxideav_usdz::register(&mut Mesh3DRegistry)` to wire
both decoder + encoder up for `.usdz` extension dispatch, or
invoke `UsdzDecoder::new()` / `UsdzEncoder::new()` directly.

## Round 1 scope (reader)

- **PKZIP central-directory walk** — STORED-only entries (USDZ
  spec rejects any compression method); every payload offset is
  verified to satisfy the 64-byte alignment requirement.
- **ASCII USDA tokenizer + prim-tree parser** — the `#usda 1.0`
  text format: layer metadata, nested prim defs, attribute /
  relationship statements, asset paths (`@./diffuse.png@`), prim
  paths (`</A/B>`).
- **`UsdGeomMesh` → `Mesh + Primitive`** — `Triangles` topology;
  arbitrary polygon faces (`faceVertexCounts` ≠ 3) are
  fan-triangulated; vertex normals + first UV set are picked up
  when present.
- **`UsdPreviewSurface` → `Material`** — PBR slots
  (`base_color`, `metallic`, `roughness`, `emissive_factor`) plus
  texture connections to sibling `UsdUVTexture` shaders for
  diffuse / normal / emissive / occlusion maps.
- **`UsdUVTexture` → `Texture`** wrapping a `ZipStoredAsset`.
  `ZipStoredAsset::raw_storage()` returns
  `RawStorage { scheme: "zip-stored", bytes, uncompressed_size }`,
  so the round-2 USDZ writer passes inner-file bytes through
  verbatim into the output ZIP without re-buffering.
- **`upAxis` + `metersPerUnit`** map onto `Scene3D::up_axis` and
  `Scene3D::unit` (with the standard preset list — metres /
  centimetres / millimetres / inches / feet / yards).

## Round 2 scope (writer)

- **`UsdzEncoder` / `Mesh3DEncoder`** — writes a `Scene3D` back
  out as a USDZ archive. Output is byte-faithful to the spec:
  STORED entries only, every payload offset is a multiple of 64
  bytes (LFH `extra` field padded with NULs to push the payload
  onto the boundary), CRC32 computed per entry, central directory
  + EOCD assembled correctly.
- **USDZ → USDZ pass-through** — when a texture's
  `AssetSource::raw_storage()` returns
  `RawStorage { scheme: "zip-stored", ... }` (which is exactly
  what `ZipStoredAsset` from this crate's reader exposes), the
  encoder copies the bytes verbatim. A USDZ → `Scene3D` → USDZ
  pipeline therefore never inflates / re-deflates / re-encodes
  texture or audio assets — the inner-file bytes survive
  bit-identical from input archive to output archive.
  `UsdzEncoder::encode_with_report()` returns a per-call
  `EncodeReport` whose `pass_through_textures` / `reencoded_textures`
  counters let callers verify the optimisation actually fires for
  USDZ-sourced scenes.
- **USDA tokenizer-inverse** — `Scene3D` → USDA text, the inverse
  of round 1's USDA reader: layer metadata (`upAxis`,
  `metersPerUnit`), `Xform` nodes containing `def Mesh` children,
  `UsdGeomMesh` with `faceVertexCounts` / `faceVertexIndices` /
  `points` / optional `primvars:normals` / optional `primvars:st`,
  `Material` prims under a synthetic `/Materials` Scope holding
  `UsdPreviewSurface` shader children + `UsdUVTexture` shader
  children for every bound texture map.

## Round 4 scope (UsdMediaSpatialAudio)

- **`UsdMediaSpatialAudio` reader.** `def SpatialAudio "<name>" {
  uniform asset filePath = @sound.wav@; uniform token auralMode =
  "spatial"; uniform double gain = ...; ... }` decodes into a
  `Node::audio_emitter` referencing an `AudioEmitter` + `AudioSource`
  in the scene's audio arenas. In-archive `filePath` references
  resolve through `ZipStoredAsset` so the writer's pass-through
  optimisation extends to audio just like it does for textures;
  external paths land in `AudioData::External { uri }`.
  `auralMode = "spatial"` maps to `AuralMode::SpatialNonAcoustic`
  (USD's default — panning + distance attenuation), `"nonSpatial"`
  to `AuralMode::SpatialAcoustic`. Supplementary fields
  (`startTime`, `endTime`, `mediaOffset`, `fillBufferTime`) survive
  on `AudioSource::extras` / `AudioEmitter::extras` for
  byte-faithful round-trip; the original `auralMode` token spelling
  is also preserved in `emitter.extras["usd:auralMode"]`.
- **`UsdMediaSpatialAudio` writer.** Symmetric to the reader: each
  `Node` carrying `audio_emitter` emits a `def SpatialAudio` block
  inside the parent Xform. In-archive `AudioSource`s land at the
  per-source filename (`{name}.{ext}` with the MIME-derived
  extension) and the bytes are appended to the output ZIP via the
  same STORED + 64-byte alignment path the texture writer uses;
  `AudioSource::raw_storage(scheme = "zip-stored")` wins the
  pass-through optimisation when the source originated from a
  sibling USDZ archive. `EncodeReport` now carries
  `audio_names` / `pass_through_audio` / `reencoded_audio`
  counters mirroring the texture reporting.

## Round 3 scope (per-mesh transforms + multi-primitive)

- **Per-node transform xformOp serialisation.** Both encoder and
  decoder round-trip `Node::transform` through the
  `UsdGeomXformable` opinion schema:
  `Transform::Trs` → `xformOp:translate` + `xformOp:orient`
  (quatf, USD's wxyz layout) + `xformOp:scale`;
  `Transform::Matrix(m)` → `xformOp:transform` (matrix4d);
  identity emits no opinions.
- **Multi-primitive Mesh emission + sibling-fold decode.** A
  Scene3D `Mesh` with N `Primitive`s serialises as N sibling
  `def Mesh` prims (`<MeshName>`, `<MeshName>_1`,
  `<MeshName>_2`, ...) each with its own `material:binding`.
  The decoder folds sibling Mesh prims sharing a name stem
  back into a single Scene3D Mesh with N Primitives.
  Hand-authored sibling Mesh prims whose names don't match
  the stem convention stay unaffected.

## Round 5 scope (topology dispatch + no_fold + per-Mesh transform)

- **Strips / fans / lines / points dispatch.** Non-`Triangles`
  primitives are no longer dropped silently. Strips and fans
  expand into triangle lists in-place (winding-correct per the
  GL convention) and emit as `def Mesh`; the source topology
  token is preserved on `Primitive::extras["usd:original_topology"]`
  for hint round-trip. Lines / LineStrip / LineLoop emit as
  `def BasisCurves` (`type = "linear"`, `wrap` selecting
  periodic / non-periodic, `curveVertexCounts` matching the
  topology shape). Points emit as `def Points`. Decoder
  symmetric: parses BasisCurves and Points prims back into the
  matching `Topology::Lines / LineStrip / LineLoop / Points`.
- **`usd:no_fold` extras flag.** Authoring tools sometimes ship
  intentional sibling-name collisions (`Foo` + `Foo_1` as
  separate Meshes, not folded into one multi-primitive Mesh).
  The decoder honours `(usd:no_fold = 1)` prim metadata as the
  opt-out: marked siblings become independent Scene3D Meshes.
  Encoder symmetric: re-emits the metadata flag whenever a
  `Primitive::extras["usd:no_fold"] = true` indicates the source
  scene wants the opt-out preserved.
- **Per-Mesh transform on the inner `def Mesh`.** UsdGeomMesh
  inherits UsdGeomXformable, so a Mesh prim can carry its own
  transform independent of its parent Xform. The mapping uses
  `Primitive::extras["usd:mesh_transform"]` (a JSON-shaped
  matrix or TRS object) to cross the typed model; the writer
  emits matching `xformOp:*` opinions directly on the inner
  `def Mesh`. Decoder symmetric: lifts an inner-Mesh
  `xformOp:transform` (or TRS triple) back into the same
  extras slot.

## Round 6 scope (variantSet parsing + line/column errors)

- **VariantSet structured parser.** `variantSet "name" = {
  "variantA" ( meta ) { body } "variantB" { body } }` blocks
  inside a prim body now parse into a structured
  `Prim::variant_sets: BTreeMap<String, BTreeMap<String, Variant>>`
  (per the OpenUSD glossary `simpleVariantSet.usd` / 
  `referenceVariantSet` examples). Variants carry their own
  metadata + attrs + child prims, so a future composition engine
  can switch on the prim's `variants = { string name = "..." }`
  metadata. Round 6 is parser-side capture only; semantic
  composition deferred.
- **Line + column in parser error messages.** Every
  `at offset N` diagnostic now reads `line L:C (offset N)`. The
  raw offset stays for byte-precise tooling; the line/column
  makes hand-authored USDA debugging actionable.

## Round 7 scope (variant selection composition)

- **Variant selection composition.** The Scene3D translator now
  evaluates `metadata["variants"] = { string SET = "VAR" }`
  selections against `Prim::variant_sets` before walking the prim
  tree. The matching variant's `attrs` and `children` are merged
  in under **LIVRPS** strength order — Local opinions beat
  Variant opinions. A `def Mesh` declared inside a selected
  variant therefore materialises as a Scene3D Mesh + Node just
  like a directly-authored child would. Round 7 is read-only; the
  writer doesn't yet round-trip `variantSet` blocks (a `Scene3D`
  round-trip flattens the variant content into the resolved
  tree). Public API: `Prim::resolved_variants()` returns the
  composed view of a single prim. External `references = @...@`
  opinions inside a selected variant are NOT followed — they're
  stashed under `usd:variantReferences:<set>:<var>:references` so
  the extras layer surfaces the unresolved arc to the caller.

## Round 10 scope (anchored sublayer composition)

- **In-archive sublayer composition.** When the Default Layer
  declares `subLayers = [@./geom.usda@, @./materials.usda@, ...]`
  and those entries exist in the surrounding USDZ archive, the
  decoder now composes each sublayer's prim tree underneath the
  local layer's, per the OpenUSD glossary's LayerStack definition:
  "the recursive gathering of all SubLayers of a Layer, plus the
  layer itself as first and strongest". Sublayers are walked
  depth-first, strength-ordered (first entry strongest), with a
  cycle-break on self-reference; the audit trail rides on
  `Scene3D::extras["usd:composedSubLayers"]`. Local prim opinions
  beat sublayer opinions on every metadata / attr / child key;
  `over` + `def` of the same prim name fuse into a single `def`
  (the prim IS defined — by the sublayer — and the local layer is
  only overriding opinions).
- **`.usdc` sublayer surfaces as `Error::Unsupported`.** The
  binary Crate parser is still gated on a docs-collaborator trace
  doc, so a sublayer with the `.usdc` extension errors out
  consistently with the round-1 Default-Layer policy. External /
  cross-package sublayer paths (anything not in the archive's
  entries) are silently skipped during composition — they stay on
  `scene.extras["usd:layerMetadata"]` for writer round-trip so a
  higher-level pipeline can still resolve them.

## Round 11 scope (references / payload arc composition)

- **In-archive `references` / `payload` arc composition.** A prim
  carrying `references = @asset.usd@` / `payload = @asset.usd@`
  (optionally with a `</Prim>` selector) that targets an in-archive
  `.usd` / `.usda` layer now has the targeted prim composed
  underneath it, per the OpenUSD glossary's References definition —
  modelled on the glossary's `MarbleCollection` worked example. The
  referencing prim keeps its own name (the glossary's "prim
  name-change"); the target's `type_name`, `kind`, children, and
  attrs ride up. A bare `references = @file@` targets the referenced
  layer's `defaultPrim`; `@file@</Prim>` targets the named prim.
  Local opinions beat referenced opinions; `references` beats
  `payload`; within a `[@a@, @b@]` list the earlier entry is
  stronger. Nested references compose transitively; a
  `(layer, prim-path)` visited set breaks cycles. The audit trail of
  composed arcs lands on `Scene3D::extras["usd:composedReferences"]`.
- **Flatten-on-write + round-trip preservation.** Consumed
  in-archive arcs are stripped so the writer flattens the composed
  tree into a single layer (matching round 10's sublayer policy).
  External / cross-package (`/abs`, `://`, `[...]`) and
  non-`.usd[a]` targets stay authored so the writer round-trips them;
  `.usdc` targets raise `Error::Unsupported` (binary-Crate gap).

## Round 12 scope (`defaultPrim` consistency on the writer)

- **`defaultPrim` token tracks sanitisation.** The writer sanitises
  prim names to USD's `[A-Za-z_][A-Za-z0-9_]*` grammar (`"My Cube"` →
  `def Xform "My_Cube"`). A preserved `defaultPrim` opinion named
  with the pre-sanitisation spelling is now rewritten to match the
  emitted prim so selector-less `references = @./scene.usda@`
  resolution against our output still finds the target.
- **Dangling `defaultPrim` is dropped.** When the preserved opinion
  names a prim that no longer exists in the scene (downstream
  pipeline pruned the root), the writer drops the dangling opinion
  rather than re-emitting it — strict USD validators reject layers
  whose `defaultPrim` resolves to nothing.
- **Missing `defaultPrim` is synthesised from the first root.** A
  freshly-constructed `Scene3D` (no `usd:layerMetadata`) now emits a
  `defaultPrim` opinion naming the first root's sanitised name so
  cross-archive selector-less references downstream of us actually
  resolve. Rootless scenes still emit no opinion.

## Round 13 scope (in-layer `inherits` / `specializes` composition)

- **In-layer class-hierarchy arc composition.** A prim carrying
  `inherits = </Class/Foo>` / `specializes = </Class/Foo>` (or a
  list of such paths) now has the targeted in-layer prim composed
  underneath it per the OpenUSD glossary's Inherits / Specializes
  definitions. Class arcs target a prim path WITHIN the same layer
  (distinct from `references` / `payload` which target external
  asset paths), so the composer walks the local layer's prim tree
  rather than another archive entry. The target's `type_name`,
  `kind`, children, and attrs ride up under the inheriting /
  specialising prim through the same `merge_prim_under` primitive
  that powers the round 10 / 11 sublayer + reference composers —
  local opinions win, earlier list entries beat later ones.
- **LIVRPS-aligned strength order.** Per the OpenUSD glossary —
  L > I > V > R > P > S — `inherits` composes BEFORE
  `references` / `payload` so a referenced opinion can never
  overwrite an inherited one; `specializes` composes AFTER all
  other arcs so it can only fill opinions that every stronger arc
  has left unsourced.
- **Transitive composition + cycle break.** A class prim that
  itself carries class-arc opinions has its own arc composed before
  merging upward (`</A>` inherits `</B>`; `Prop` inherits `</A>`
  pulls every child contributed by `</B>` up through `</A>`). A
  visited-set keyed on the normalised prim path scoped per-recursion
  breaks reference cycles so a self-referential class hierarchy
  doesn't hang.
- **Flatten-on-write.** Successfully-composed in-layer arcs are
  stripped so the writer flattens the composed tree into a single
  layer, matching the round 10 / 11 sublayer + reference
  flatten-on-write policy. An arc whose target prim path doesn't
  resolve in the local layer stays authored so the writer
  round-trips the unresolved opinion verbatim. The audit trail
  rides on `Scene3D::extras["usd:composedInherits"]` and
  `["usd:composedSpecializes"]` as `"<prim-path>|inherits"` /
  `"<prim-path>|specializes"` entries.

## Round 14 scope (CRC-32 integrity verification)

- **STORED payloads are verified against the central-directory
  CRC-32.** USDZ is a PKZIP archive (PKWARE APPNOTE.TXT container)
  with two extra constraints; every entry carries a CRC-32 in the
  same field a compressed entry would. The writer always emitted a
  correct CRC, but the reader never checked it. The ZIP walker now
  reads the CRC-32 each central-directory header records and
  recomputes the CRC-32/ISO-HDLC of the stored payload, rejecting any
  mismatch with `Error::InvalidData`. A corrupted byte (bit-rot, a
  truncated copy, a mismatched STORED pass-through) is caught at the
  container boundary as a clear integrity error instead of surfacing
  downstream as a baffling USDA parse failure or a garbled texture.
  The check reuses the crate's own CRC-32 routine so reader and
  writer can never drift; the zero-length empty-file case verifies
  naturally. The CRC field is plain PKWARE structure — not
  USD-specific.

## Round 194 scope (variantSet-bound Material `usdcat`-flatten oracle)

- **Black-box cross-validation of variantSet + Material PBR.**
  `tests/variant_material_usdcat_oracle.rs` exercises the union of
  the round-7 variant resolver and the round-1 `UsdPreviewSurface`
  PBR mapping against the `usdcat -f` flatten as the oracle. A
  `def Material` declared INSIDE a variantSet body must be
  materialised under the selected branch, indexed by the typed
  Scene3D pass, and bound to the Mesh's `rel material:binding`. The
  test feeds both the original USDA and the `usdcat -f`-flattened
  reference through `UsdzDecoder` (each wrapped via the in-tree
  `common::build_usdz` helper) and asserts a structural fingerprint
  parity — mesh count, primitive vertex count, material count,
  bound `base_color` / `metallic` / `roughness`. A companion test
  flips the selection metadata to `"matte"` and confirms the bound
  material tracks the alternate variant's PBR values. Skip-early
  (NOT `#[ignore]`) when `usdcat` is absent — no `pxr` Python
  module is required, so this oracle runs anywhere Apple USD Tools /
  the USD CLI binary is installed.

## Round 273 scope (USDC §4.5 PATHS three-buffer framing)

- **`usdc::PathsSection` splits the three §3a compressed buffers.**
  The docs collaborator corrected trace doc §4.5 to record that the
  PATHS section's trailing region (after the 16-byte
  `numPaths`+repeat prefix) carries **three** `(int64
  compressedSize, §3a buffer)` triples — the parallel arrays of the
  namespace path tree: path-token indices, element-token indices,
  and sibling/child "jump" offsets. Round 236's opaque `tail_bytes`
  slice (a deliberate hold because the prior single-buffer claim
  didn't exhaust the Elephant's 532 trailing bytes) is replaced by
  `path_tokens_buffer_bytes` / `element_tokens_buffer_bytes` /
  `jumps_buffer_bytes` (+ `*_compressed_size` companions) and the
  matching `path_tokens_buffer` / `element_tokens_buffer` /
  `jumps_buffer` accessors forwarding each bounded slice to
  `CompressedBuffer::parse`, mirroring the §4.3 FIELDS and §4.6
  SPECS multi-buffer framing. `PathsSection::parse` enforces
  `16 + 8 + csize₁ + 8 + csize₂ + 8 + csize₃ == section_size`
  exactly; trailing bytes the trace doesn't authorise are an
  `Error::InvalidData`. Defensive caps (256 MiB per buffer) stop a
  hostile prefix before allocation.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: PATHS at `offset = 0x0cf92b`,
  `size = 548`, `num_paths = 248`, three buffers of
  `csize 266 / 145 / 97`, total footprint
  `16 + 8 + 266 + 8 + 145 + 8 + 97 = 548`; each buffer walks as a
  §3a `CompressedBuffer` envelope. Per-element tree-walk
  reconstruction (the §3b common-value fast path) is a separate
  follow-up — this round lands the framing only.

## Round 265 scope (USDC TOC standard-section table accessors)

- **`usdc::Toc::standard_section_table`** — one-pass classifier
  projecting `Toc::entries` onto a fixed-size
  `[Option<&TocEntry>; 6]` indexed by
  `SectionName::canonical_index`. Each slot at `i` holds
  `Some(entry)` for the first TOC entry whose name classifies as
  the standard section at canonical index `i`, or `None` when no
  such entry is present. Trailing entries with non-standard names
  (per trace doc §2 the TOC name field is open-ended) are
  silently ignored; duplicates of the same standard name keep
  the first occurrence — same contract as `Toc::find`.
- **`usdc::UsdcFile::standard_section_table(file_bytes)`** — the
  bytes-borrowing companion: composes the TOC's
  `standard_section_table` with `TocEntry::slice_in` to borrow
  each present standard section's payload bytes out of
  `file_bytes` in a single walk of the TOC. A slot is `None` if
  the standard section is absent or if `file_bytes` is shorter
  than the entry's recorded range (the same defensive fallback
  `slice_in` provides).
- **Why a bulk accessor in addition to Round 245's per-name
  ones.** `matches_canonical_order` answers "is the TOC
  well-ordered?"; the new accessor answers "for each standard
  section, where is its entry?" — useful when the predicate
  doesn't hold (out-of-order TOC, hand-authored entries) and the
  reader still needs every section located in one walk instead
  of six separate `Toc::find` calls.
- Cross-validated against the in-tree Elephant fixture under
  `docs/3d/usd/fixtures/`: the bulk accessor and the per-name
  `section_bytes` accessor return pointer-identical slices with
  matching lengths for every one of the six standard sections,
  so the new bulk accessor is observationally a one-pass batched
  form of the per-name accessor on real bytes.

## Round 245 scope (USDC TOC canonical-order predicate + section-bytes accessor)

- **`usdc::SectionName::ALL_STANDARD`** — the canonical six
  standard section names in trace doc §2's observed declaration
  order (`TOKENS`, `STRINGS`, `FIELDS`, `FIELDSETS`, `PATHS`,
  `SPECS`). Trace doc §2 grounds this ordering in two
  independent real samples — quoting it directly: "The six names
  appear in this same order in the teapot too."
  `SectionName::canonical_index` gives each variant its zero-based
  position in the sequence so a typed walk has a typed lookup.
- **`usdc::TocEntry::slice_in(file_bytes)`** — borrow a TOC
  entry's payload bytes from a full USDC file slice. The
  `(offset, size)` pair was bounds-checked by `Toc::parse` at
  parse time, so the lookup is a clean borrow into the original
  input. Returns `None` when `file_bytes` is shorter than the
  entry's recorded range as a defensive fallback for callers that
  hold the entry independently of the source slice.
- **`usdc::Toc::matches_canonical_order`** — fast-path predicate
  that returns `true` when the TOC's first six entries classify, in
  declaration order, as `SectionName::ALL_STANDARD`. Trailing
  non-standard entries beyond the canonical six are tolerated (the
  TOC name field is open-ended per the trace doc). A reader for
  which the predicate holds can address each standard section by
  its canonical index directly into `Toc::entries` instead of
  re-running `Toc::find` on every access.
- **`usdc::UsdcFile::section_bytes(name, file_bytes)`** — single-call
  convenience that composes `Toc::find` + `TocEntry::slice_in` so
  callers pull any standard section's bytes out of a parsed file in
  one step.
- Cross-validated against the in-tree Elephant fixture under
  `docs/3d/usd/fixtures/`: the six TOC entries classify in the
  canonical order; `UsdcFile::section_bytes` returns the
  pointer-identity slice at each of the trace doc §2 published
  offsets + sizes — `TOKENS @0x0cebf0` size `1770`,
  `STRINGS @0x0cf2da` size `8`, `FIELDS @0x0cf2e2` size `998`,
  `FIELDSETS @0x0cf6c8` size `611`, `PATHS @0x0cf92b` size `548`,
  `SPECS @0x0cfb4f` size `331`.

## Round 239 scope (USDC §4.6 SPECS section three-buffer framing)

- **`usdc::SpecsHeader` / `usdc::SpecsSection`** — the §4.6 SPECS
  section's outer three-buffer framing. The section is an 8-byte
  little-endian `int64 count` header followed by **three**
  `(int64 compressedSize, §3a buffer)` triples. The three buffers
  carry, in order, `count × i32` **path indices** into the §4.5
  PATHS namespace tree, `count × i32` **field-set indices** into
  the §4.4 FIELDSETS array, and `count × i32` **spec-type** codes
  (a small enum identifying prim / attribute / relationship /
  …). Each spec row is therefore the join
  `(pathIndex, fieldSetIndex, specType)` — "at this namespace
  path, of this kind, here are its fields" — so SPECS is the
  table a reader iterates to materialise the stage.
  `SpecsSection::parse` enforces
  `8 + 8 + csize₁ + 8 + csize₂ + 8 + csize₃ == section_size`
  exactly — trailing bytes the trace doesn't authorise are an
  `Error::InvalidData`. `SpecsSection::paths_buffer` /
  `fieldsets_buffer` / `types_buffer` forward to
  `CompressedBuffer::parse` on each bounded buffer slice for
  callers walking the §3a framing ahead of the LZ4 block decoder.
  Defensive caps on `count` (16 Mi) and on each buffer's
  `compressedSize` (256 MiB) cut off a hostile or corrupted
  header before allocation.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's SPECS entry sits at
  `offset = 0x0cfb4f` with `size = 331`, the section header
  parses to exactly the trace doc's `count = 248`, the three
  buffer prefixes carry `csize₁ = 60`, `csize₂ = 200`,
  `csize₃ = 39`, and the total footprint
  `8 + 8 + 60 + 8 + 200 + 8 + 39 = 331` matches the section size
  on the wire.
- The spec-type enumeration that the third buffer's `i32`s
  eventually resolve into is its own fact-table extraction and
  stays deferred — this primitive lands the framing only.

## Round 236 scope (USDC §4.5 PATHS section leading prefix)

- **`usdc::PathsHeader` / `usdc::PathsSection`** — the §4.5 PATHS
  section's 16-byte leading prefix. Trace doc §4.5 records the
  section opens with `int64 numPaths` immediately followed by a
  second `int64` that repeats the same count (both observed as
  `0xF8` = 248 on the Elephant fixture); the rest of the section
  carries the namespace path tree as one or more §3a compressed
  buffers. `PathsHeader::parse` reads the two int64s, enforces the
  repeat-equals-numPaths invariant the trace doc grounds in the
  bytes, and applies a defensive cap on `numPaths` (16 Mi) to
  bound downstream allocation against a hostile header.
  (Round 273 superseded the original opaque-`tail_bytes` hold once
  the trace doc resolved §4.5 to three §3a buffers — see the
  Round 273 scope above for the full three-buffer split.)
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's PATHS entry sits at
  `offset = 0x0cf92b` with `size = 548`, the section parses to
  exactly the trace doc's `num_paths = 248`.

## Round 229 scope (USDC §4.4 FIELDSETS section framing)

- **`usdc::FieldSetsHeader` / `usdc::FieldSetsSection`** — the §4.4
  FIELDSETS section's outer framing. The section is an 8-byte
  little-endian `int64 count` header followed by a single
  `(int64 compressedSize, §3a buffer)` pair. The decompressed
  buffer, once the LZ4 block decoder lands, feeds `count` `i32`
  values through the §3b integer decoder; the trace doc records
  that the array is the concatenation of per-set field-index runs
  separated by a `-1` (`0xFFFFFFFF`) sentinel. Each spec (§4.6)
  references one such set to get its full property list.
  `FieldSetsSection::parse` enforces
  `8 + 8 + compressedSize == section_size` exactly — trailing
  bytes the trace doesn't authorise are an `Error::InvalidData`.
  `FieldSetsSection::buffer` forwards to `CompressedBuffer::parse`
  on the bounded buffer slice for callers walking the §3a framing
  ahead of the LZ4 block decoder. Defensive caps on `count`
  (16 Mi) and on the buffer's `compressedSize` (256 MiB) cut off a
  hostile or corrupted header before allocation.
- **`usdc::split_field_sets`** — small companion that splits the
  flat post-§3b `i32` array into one `Vec<i32>` per set, dropping
  sentinels. Accepts a trailing run without a sentinel (trace doc
  leaves that case unconstrained), surfaces a leading sentinel as
  an initial empty set, and emits empty sets between consecutive
  sentinels. Lets the run-split semantics get exercised today on
  synthesised input without first wiring the LZ4 block decoder.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's FIELDSETS entry sits at
  `offset = 0x0cf6c8` with `size = 611`, the section header parses
  to exactly the trace doc's `count = 576`, the trailing buffer's
  `compressedSize` is `595`, and the total footprint
  `8 + 8 + 595 = 611` matches the section size on the wire.
- Trace doc §4.4 caveat (the §3b buffer uses a "common value"
  fast path so a naive `decode_int_array` recovers the run
  structure but not the literal field indices) is recorded in the
  module docs; the per-element semantic-recovery step is its own
  follow-up.

## Round 222 scope (USDC §4.3 FIELDS section framing)

- **`usdc::FieldsHeader` / `usdc::FieldsSection`** — the §4.3
  FIELDS section's outer two-buffer framing. The section is an
  8-byte little-endian `int64 numFields` header followed by
  **two** `(int64 compressedSize, §3a buffer)` pairs:
  1. an int-coded array (§3b, once the LZ4 layer is peeled) of
     `numFields` token indices, one per field, giving each field
     its name (an index into the §4.1 TOKENS atom pool);
  2. `numFields × uint64` value-rep entries — a packed
     representation carrying type code, flags (`is-array`,
     `is-inlined`, `is-compressed`), and either an inline value
     or a file offset to the value's bytes elsewhere in the file.
  `FieldsSection::parse` enforces
  `8 + 8 + csize₁ + 8 + csize₂ == section_size` exactly —
  trailing bytes the trace doesn't authorise are an
  `Error::InvalidData`. `FieldsSection::names_buffer` /
  `reps_buffer` forward to `CompressedBuffer::parse` on the
  bounded buffer slices for callers walking the §3a framing
  ahead of the LZ4 block decoder. Defensive caps on `numFields`
  (16 Mi) and on either buffer's `compressedSize` (256 MiB) cut
  off a hostile or corrupted header before allocation.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's FIELDS entry sits at
  `offset = 0x0cf2e2` with `size = 998`, the section header
  parses to exactly the trace doc's `numFields = 157`, the two
  buffer prefixes carry `csize₁ = 141` and `csize₂ = 833`, and
  the total footprint `8 + 8 + 141 + 8 + 833 = 998` matches the
  section size on the wire.

## Round 217 scope (USDC §4.2 STRINGS section parser)

- **`usdc::StringsHeader` / `usdc::StringsSection`** — the §4.2
  STRINGS section: an 8-byte little-endian `int64 count` followed
  by `count × uint32` token indices (raw, NOT LZ4-compressed per
  the trace doc). The STRINGS pool is the subset of TOKENS atoms
  that are used as USDA *string-typed* values (string-valued field
  reps in §4.3 FIELDS reference token indices through this pool).
  `StringsSection::parse` enforces `8 + count * 4 == section_size`
  exactly — trailing bytes the trace doesn't authorise are an
  `Error::InvalidData`. `StringsSection::parse_indices` decodes the
  raw `uint32` array into a `Vec<u32>` for callers that want to
  walk the pool. Defensive cap on `count` (16 Mi) protects against
  runaway allocation from a hostile or corrupted header.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's STRINGS entry sits at
  `offset = 0x0cf2da` with `size = 8`, the section header parses
  to exactly the trace doc's `count = 0`, and the
  `8 + count * 4 = section_size` invariant holds on the wire.

## Round 212 scope (USDC §3a compressed-buffer framing + §4.1 TOKENS section header)

- **`usdc::CompressedBuffer` / `CompressedChunk`** — the §3a outer
  "compressed buffer" framing. Reads the leading chunk-count byte
  and either yields the entire remainder as a single LZ4-block
  chunk (the common `0x00` case the trace doc cites for every
  observed buffer in both samples) or walks the `(int32 LE length,
  bytes)` chunk records that follow. The LZ4 *block* payload inside
  each chunk is kept opaque — the public LZ4 block-format spec is
  not staged under `docs/`, so this module stops at the envelope;
  callers thread the chunk slice into their own block decoder.
  Bounded against truncated length prefixes, chunks running past
  end-of-buffer, and negative declared lengths.
- **`usdc::TokensHeader` / `TokensSection`** — the §4.1 TOKENS
  section header: three little-endian `int64`s (`numTokens`,
  `uncompressedSize`, `compressedSize`) plus the bounded §3a
  compressed-buffer slice that follows. Defensive caps on
  `numTokens` (16 Mi) and `uncompressedSize` (256 MiB) several
  orders of magnitude above the Elephant sample's 192 / 4195
  figures keep a hostile or corrupted header from triggering a
  runaway allocation. `TokensSection::parse` slices exactly
  `compressed_size` bytes after the 24-byte header so callers
  never read past the section bounds.
- **`usdc::split_tokens_blob`** — the cross-section seam for
  callers that have plugged in their own LZ4 decoder for the
  inner block: takes the *decompressed* TOKENS blob plus the
  original `TokensHeader`, verifies the
  `blob.len() == uncompressed_size` equality, NUL-splits into the
  recorded `numTokens` UTF-8 strings (accepting either a trailing
  NUL or a NUL-as-delimiter form), and validates UTF-8 per token.
- Cross-validated against the Elephant fixture under
  `docs/3d/usd/fixtures/`: the TOC's TOKENS entry walks to a
  section whose header parses to exactly the trace doc's
  `numTokens=192 / uncompressedSize=4195 / compressedSize=1746`
  triple, `header(24) + compressed_size = section.size` (1770),
  and the §3a framing decomposes to a single chunk of
  `compressed_size - 1` bytes — i.e. the leading `0x00`
  chunk-count plus the LZ4 block proper.

## Round 206 scope (USDC §3b compressed-integer decoder)

- **`usdc::decode_int_array`** — the §3b "compressed integer"
  decoder. Takes the post-LZ4 bytes of an integer buffer plus the
  expected element count from the surrounding section header and
  returns the reconstructed `Vec<i32>`. Per the trace doc the wire
  shape is: a 2-bit-per-element control stream of `ceil(N/4)` bytes
  (LSB-first within each byte) selecting one of four widths — code 0
  repeats the previous value, codes 1 / 2 encode a signed `i8` /
  `i16` delta from the previous value, code 3 encodes an absolute
  `i32` that also becomes the new "previous". Variable-width payload
  bytes follow in array order. The decoder errors with
  `Error::InvalidData` on a truncated control stream or short
  payload for the declared width. Used by FIELDS' name-index array,
  FIELDSETS' field-index array, and SPECS' three index arrays once
  the LZ4 wrapper lands; sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §3b.

## Round 15 scope (USDC binary-format primitives)

- **`usdc` module — bootstrap + Table-of-Contents primitives.**
  The binary `.usdc` sibling of the USDA text format that the
  rest of this crate parses now has a documented surface for the
  outer parts of the wire format: `Magic` (8-byte `PXR-USDC`
  signature), `Version` (`major.minor.patch` from bootstrap bytes
  `0x08`, `0x09`, `0x0A`), `Bootstrap` (the fixed 88-byte header
  including the absolute `toc_offset`), `TocEntry` /
  `SectionName` / `Toc` (the tail-written directory whose six
  standard sections are `TOKENS`, `STRINGS`, `FIELDS`,
  `FIELDSETS`, `PATHS`, `SPECS`), and a one-call
  `UsdcFile::parse` that returns both. Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md`, the project's own
  clean-room byte-level trace of real `.usdc` samples; cross-
  validated against the trace doc's Elephant fixture (v0.8.0,
  six sections in the documented order, TOC at file offset
  `0x000cfc9a`, section sizes matching the trace doc's decoded
  table down to the byte).
- **USDZ decoder boundary check.** A `.usdc` Default Layer now
  has its bootstrap + TOC validated before the decoder surfaces
  `Error::Unsupported`. Malformed `.usdc` payloads (truncated
  bootstrap, wrong magic, oversized `sectionCount`, section
  region running into the TOC, …) are caught at the container
  boundary as `Error::InvalidData` instead of leaking past
  layer-extension dispatch. The Unsupported message records the
  parsed version + section catalogue so callers can see how far
  the boundary check got. Section-payload decoding (LZ4 +
  delta-int-coding wrappers, plus the six section semantics) is
  the natural next slice and stacks on top of these primitives
  without re-parsing the bootstrap.

## Deferred to round 230+

- **`.usdc` LZ4 *block* decoder.** Round 212 ships the §3a outer
  framing (`CompressedBuffer` walks the chunk count + per-chunk
  length prefixes) and exposes each chunk's raw bytes, but the LZ4
  block-format spec is not staged under `docs/` so the actual
  block decompression is left to the caller. Once a clean-room
  trace of the public LZ4 block format lands, the chunk-payload
  decoder slots in here without changing the framing surface.
- **`.usdc` section-payload semantics.** Rounds 212 / 217 / 222 /
  229 / 236 / 239 / 273 land the outer framing of all six
  standard sections — TOKENS (§4.1), STRINGS (§4.2), the FIELDS
  two-buffer framing (§4.3), the FIELDSETS single-buffer framing
  (§4.4), the PATHS three-buffer framing (§4.5), and the SPECS
  three-buffer spec table (§4.6). What remains is the per-element
  *content* decode (below): the LZ4 block decompression and the
  §3b common-value semantic recovery that turn each section's
  bounded buffers into materialised values.
- **§3b "common value" fast path.** Round 229's §4.4 framing
  exposes the buffer but the trace doc records that FIELDSETS
  (and PATHS) use a common-value compression step on top of
  the §3b shape. `decode_int_array` recovers the structural
  run boundaries but not the literal field indices; the
  per-element semantic-recovery step is a separate decoder
  pass the trace doc leaves as a future fact extraction.
- **FIELDS value-rep type-code enumeration.** A separate
  fact-table extraction (gap tracker's Round B) that the
  bootstrap + TOC primitives don't depend on.
- **`UsdSkelSkeleton` + `UsdSkelBindingAPI`** skeletal-animation
  skinning — blocked on `UsdSkel` schema docs not yet staged in
  `docs/3d/usd/`.
- **`UsdGeomSubset`** per-face material-binding subsets — same
  schema-doc gap as `UsdSkel`.
- **Cross-package `@foo.usdz[path/within.usd]@` references** —
  round 11 resolves `references` / `payload` targets that live as
  their own entries in the *same* archive; package-relative `[...]`
  selectors into a sibling `.usdz` stay as side-channel opinions on
  `scene.extras["usd:layerMetadata"]`.
- **SubLayer / reference / class-arc round-trip on the writer.**
  Rounds 10 / 11 / 13 evaluate sublayers + references + class arcs
  on the read path; the encoder flattens the composed tree into a
  single layer rather than preserving the multi-file structure or
  the original class-hierarchy authoring on output.

## Standalone build

`oxideav-core` is gated behind the default-on `registry` cargo
feature. Drop the framework dependency tree entirely with:

```toml
oxideav-usdz = { version = "0.0", default-features = false }
```

The reader still parses USDZ into the same `Scene3D` typed model
and the writer still emits valid USDZ archives — only the
`register()` glue and the `Error` alias change.
