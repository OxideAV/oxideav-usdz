# oxideav-usdz

Pure-Rust **USDZ** (Universal Scene Description, zipped) reader **and writer**.
USDZ is Pixar's container for shipping ready-to-go USD assets — an
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

## Deferred to round 15+

- **Binary `.usdc` "Crate" parser.** Pixar publishes no prose
  spec for the wire format; loading a `.usdc` Default Layer
  still surfaces as `Error::Unsupported` with a hint to
  re-package with `usdcat -o foo.usda`.
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
