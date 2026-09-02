# oxideav-usdz

[![CI](https://github.com/OxideAV/oxideav-usdz/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-usdz/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-usdz.svg)](https://crates.io/crates/oxideav-usdz) [![docs.rs](https://docs.rs/oxideav-usdz/badge.svg)](https://docs.rs/oxideav-usdz) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust **USDZ** (Universal Scene Description, zipped) reader **and
writer**. USDZ is the USD container for shipping ready-to-go assets — an
uncompressed, 64-byte-aligned ZIP archive whose first entry is a
`.usd` / `.usda` / `.usdc` "Default Layer" referenced by every companion
file (textures, audio, sub-layers). It is the native AR scene format for
Apple Quick Look on iOS / macOS.

The crate plugs into the
[`oxideav-mesh3d`](https://crates.io/crates/oxideav-mesh3d) typed model.
Call `oxideav_usdz::register(&mut Mesh3DRegistry)` to wire both decoder
and encoder for `.usdz` extension dispatch, or use `UsdzDecoder::new()` /
`UsdzEncoder::new()` directly.

```rust
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};
use oxideav_mesh3d::{Mesh3DDecoder, Mesh3DEncoder};

let scene = UsdzDecoder::new().decode(&usdz_bytes)?;
let out   = UsdzEncoder::new().encode(&scene)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Container

- **PKZIP central-directory walk** — STORED-only entries (spec
  §16.4.1.1 rejects any compression method and any encryption); the
  §16.4.1.3 64-byte rule is enforced under both its readings (an
  entry passes when its file *header* or its *payload* sits on the
  boundary), and the §16.4.1.4 End-of-Central-Directory restrictions
  are checked — zero disk numbers, a single central directory
  (agreeing entry counts). **ZIP64 is rejected at the
  boundary** — the EOCD sentinels (`0xFFFF` entry count, `0xFFFFFFFF`
  central-directory size/offset) surface a precise *"USDZ forbids
  ZIP64"* error instead of dereferencing a sentinel as a literal offset,
  and the count of central-directory records walked is validated against
  the EOCD's declared total.
- **Writer** emits a byte-faithful archive: STORED entries only, every
  payload offset padded (via the LFH `extra` field) onto a 64-byte
  boundary, per-entry CRC32, correct central directory + EOCD. The
  fallible `Writer::try_add_stored` / `try_finish` surface returns
  `Unsupported` when an entry or the assembled archive would cross the
  ZIP64 boundary USDZ forbids (a `≥ 2³²`-byte payload/offset or `> 2¹⁶-1`
  entries) rather than silently truncating to a corrupt archive; the
  encoder routes through it.
- **CRC-32 integrity verification** on read.
- **USDZ → `Scene3D` → USDZ pass-through** — texture and audio assets
  whose source is a `zip-stored` `RawStorage` (exactly what this crate's
  reader exposes) are copied verbatim, so inner-file bytes survive
  bit-identical with no re-encode. `UsdzEncoder::encode_with_report()`
  returns an `EncodeReport` with pass-through / re-encode counters.

## USDA (text layer)

Full reader and symmetric writer for the `#usda 1.0` text format:

- **Geometry** — `UsdGeomMesh` → `Mesh + Primitive`. Triangles plus
  strips / fans / lines / points topology dispatch; arbitrary polygons
  are fan-triangulated; vertex normals and the first UV set are picked
  up. Multi-primitive meshes serialise as sibling `def Mesh` prims and
  fold back on decode.
- **Cubic splines** — the §16.2.16.5 `<attr>.spline = { … }`
  statement parses under the §16.2.13 grammar (curve type, `pre:` /
  `post:` extrapolation incl. `sloped(s)` and `loop repeat|reset|
  oscillate`, `loop: (…)`, knots with `pre & post` dual values, pre /
  post tangents, per-segment interpolation, custom-data
  dictionaries; last-authored-wins per §16.2.16.5) into a typed
  `spline::Spline` and re-emits as a one-cycle fixed point. Two
  spellings the staged specification leaves open are inferred and
  flagged: the text tangent pair is read as `(width, slope)` (the
  §16.3.10.33.1 knot-table order) and the Crate 2-bit interpolation
  code follows the §7.4.2.4.2 enumeration order.
- **Per-face material subsets (`UsdGeomSubset`)** — face-element
  subsets with material bindings split a mesh into one typed
  `Primitive` per bound material (shared vertex arrays, the subset's
  faces' triangles); unassigned faces keep the parent binding, the
  `familyType` parent property is discovered by enumeration (its
  spelling is unpublished) and preserved verbatim, non-face /
  unbound subsets round-trip losslessly, and the writer re-emits a
  single `def Mesh` + `def GeomSubset` children as a one-cycle
  fixed point.
- **Material binding resolution** — the full `MaterialBindingAPI`
  surface: purpose-restricted spellings (`material:binding:preview`
  preferred for the typed slot, `:full` as last resort, arbitrary
  purposes preserved), `bindMaterialAs` strength, rule-1 namespace
  inheritance from enclosing containers, and **collection
  bindings** evaluated through the Core Specification §15
  CollectionAPI (includes / excludes / expansionRule / includeRoot,
  referenced collections, orphaned excludes inert) under the §3.4
  ordered rules — collection-beats-direct at one prim, normative
  property order (name-sorted + `propertyOrder`) among collections,
  membership-not-specificity. Authored relationship sets and
  CollectionAPI properties replay verbatim.
- **Materials** — the full `UsdPreviewSurface` input set → `Material`:
  base color, metallic, roughness, emissive, opacity +
  `opacityThreshold` cutout (`AlphaMode::Mask` / `Blend`), occlusion
  multiplier, plus the typed `MaterialExt` extension slots — the
  specular workflow (`useSpecularWorkflow` + `specularColor` →
  `MaterialExt::specular`; an inert color authored with the workflow
  off stays on extras), the clearcoat lobe
  (`clearcoat` / `clearcoatRoughness` → `MaterialExt::clearcoat`), and
  IOR (`MaterialExt::ior`). Displacement and a constant normal are
  preserved per-input on extras. Texture connections resolve for
  every input — diffuse / normal / emissive / occlusion into the
  typed core slots, metallic + roughness into the packed slot with
  the authored-input record, the specular-F0 and clearcoat maps into
  the typed extension slots, the rest via `usd:tex:*` extras.
- **Texture network** — `UsdUVTexture` `wrapS`/`wrapT` map to sampler
  wrap modes (filters decode as *undefined* — the Option-shaped
  sampler state — since §2.2 authors no filter inputs);
  `scale` / `bias` / `fallback` / `sourceColorSpace`
  round-trip; the `st` connection is followed to its
  `UsdPrimvarReader_float2` so `varname` selects the UV set
  (`st` → 0, `st<N>` → N). A typed per-reference UV transform
  (`TextureRef::transform`) **bakes on encode** — one
  pre-transformed `primvars:st<N>` channel per distinct
  (source channel, transform) pair, shared across every primitive
  drawing with the material, references retargeted and the
  transform consumed (`texCoord`-only overrides fold into the UV
  set; identities are no-ops) — the sampled coordinates are
  preserved exactly and the baked output is a one-cycle round-trip
  fixed point. A `file` path carrying the `<UDIM>` token
  (a tile set — no single archive entry) decodes to an
  `ImageData::External` reference and re-emits the URI verbatim.
  Material inputs connected to a `UsdPrimvarReader_<T>` (any of the
  ten §2.3 typed variants — e.g. `diffuseColor` reading
  `displayColor`) round-trip through `extras["usd:primvar:<input>"]`,
  reader prim included. Meshes read/write multi-UV
  `primvars:st1..N`, `doubleSided`, and `displayColor` /
  `displayOpacity` vertex colours.
- **Skeletal animation (UsdSkel)** — `SkelRoot` / `Skeleton` /
  `SkelAnimation` / `BlendShape` + `SkelBindingAPI`: joint token
  trees become joint nodes + a typed `Skeleton` (inverted
  `bindTransforms`), `jointIndices` / `jointWeights` decode under
  their `interpolation` / `elementSize` layouts into per-vertex
  quads (with `skel:joints` remap), animations become per-joint
  TRS channels (timeCodes → seconds) and blend-shape weight
  channels, and blend shapes become `MorphTarget`s (sparse
  `pointIndices` scattered dense). A **static** blend state
  (`blendShapeWeights` authored as a plain default value) lands on
  `Node::weights` — the per-instance morph-weight override —
  instead of a fabricated one-keyframe channel;
  `skel:animationSource` scopes which SkelAnimation drives which
  geometry (divergent states over identical rosters stay apart);
  the writer re-emits the state in default-value form and
  synthesizes a root-level `BlendState_<id>` SkelAnimation for a
  typed-model override with no carrier — so two nodes sharing one
  mesh with different static blend states survive the round trip.
  Channel names are the typed `Mesh::target_names`; **inbetween
  shapes** (§1.4.1) are typed `MorphTarget::inbetweens` stations
  (named, `weight`-pinned, `pointIndices`-scattered dense) that the
  typed model resolves at sample time, and sampled
  `blendShapeWeights` become a `MorphWeights` channel built through
  `AnimationSampler::morph_weights` — scalar channel weights stored
  verbatim, read back losslessly on encode. The writer reconstructs
  the full prim network from the typed model (a typed-model
  `MorphWeights` animation with no carrier gets a synthesized
  `BlendAnim_<idx>` SkelAnimation); round trips are a one-cycle
  fixed point.
- **Transforms** — per-node `UsdGeomXformable` xformOps round-trip
  (`translate` + `orient` quatf + `scale`, or a `matrix4d` transform).
  `matrix4d` literals (row-vector, translation in the last row)
  transpose into the typed model's column-vector convention and back,
  so `Skeleton::inverse_bind_matrices` and node matrices satisfy the
  model's affine contract (`Scene3D::validate`-clean; guarded by the
  `decode_validates` suite).
- **Spatial audio** — `UsdMediaSpatialAudio` read and write, mapping to
  `AudioEmitter` / `AudioSource` with `auralMode`, gain, and timing
  fields preserved for byte-faithful round-trip.
- **Scene metadata** — `upAxis`, `metersPerUnit` (metres / centimetres /
  millimetres / inches / feet / yards), and `defaultPrim`.
- **Diagnostics** — line/column error reporting from the tokenizer.

The reader/writer pair is a **round-trip fixed point**: encode → decode
→ encode is stable (a decoded scene re-serialises identically), with no
node-tree drift. The `roundtrip_fidelity` test suite measures this over
a matrix of base colour, metallic/roughness, emissive, normals + UVs,
texture binding, transforms, up-axis/unit, and deep hierarchy — 8/8
channels round-trip cleanly — and guards two subtle asymmetries: the
top-level `def Scope "Materials"` no longer decodes into a phantom empty
root, and a node that directly carries a same-named mesh no longer gains
a redundant `def Xform` wrapper on each cycle.

### Point instancing (`UsdGeomPointInstancer`)

The staged schema Part 2 surface, both directions: `rel prototypes`
(target order = prototype index), `protoIndices`, `positions`,
`orientations` (`quath[]`) / `orientationsf` (`quatf[]`), `scales`,
`ids`, `invisibleIds`, `velocities` / `angularVelocities` /
`accelerations` — each as its default value **and** its
`.timeSamples` — plus the list-edited `inactiveIds` prim metadata,
ride on the carrier node as a typed `PointInstancer` record
(`Node::extras["usd:pointInstancer"]`); prototype subtrees are
ordinary child nodes. The writer re-emits the prim symmetrically
(one-cycle fixed point; `usdcat` / `usdchecker` accept the output
where installed). `point_instancer::expand` is the lossy lift onto
plain nodes: one child per unmasked instance with the §2.3 TRS
(scale → orientation → position, velocities scaled by
`1 / timeCodesPerSecond` and suppressing interpolation, the
prototype's own root transform kept underneath), geometry shared
by `MeshId`, `inactiveIds` ∪ `invisibleIds` masked by id (or array
position when no `ids` are authored). `angularVelocities` are
carried but not applied — the staged digest does not state their
angular unit.

### Composition

The read path evaluates USD composition arcs:

- variantSets and variant selection,
- anchored sublayer composition (LayerStack),
- `references` / `payload` arcs (including targets that live as their own
  entries in the same archive, and — through the §16.4 package-relative
  selector `@pkg.usdz[path/within.usd]@` or a bare `@pkg.usdz@` for the
  package's default layer — targets inside a sibling `.usdz` entry, whose
  own sublayers, references and textures anchor at the package),
- in-layer `inherits` / `specializes`,
- layer `relocates` (§16.2.18.5 text map / §16.3.10.15 Crate value):
  each `{ </src> : </dst> }` entry moves the composed source prim to
  its target path under the §10.3.2.6 restrictions (root-level,
  nested, duplicate and self entries are ignored as composition
  errors); a local `over` at the target keeps its opinions,

resolved in the documented strength order, with the §10.5 namespace
mapping applied to each arc's target subtree (relationship targets
and `.connect` sources authored inside a referenced asset follow it
to its new prim path) and every in-archive asset path rebased from
the authoring layer's directory. Arc targets may be `.usda`,
`.usdc` or generic `.usd` layers.

#### Writer: flatten or preserve

The decoder also records a **typed opinion model** — a
`CompositionRecord` on `Scene3D::extras["usd:composition"]`: the
root layer's prim-tree skeleton as authored (arcs, `class` /
`over` prims), every in-archive layer the composition consumed
(with the assets those layers reference), and the root's entry
name. The encoder chooses what to do with it:

- `CompositionMode::Flatten` (default) emits one self-contained
  layer with every composed opinion flattened in; consumed arcs
  are not re-authored and folded-in `subLayers` entries are
  pruned rather than left dangling.
- `UsdzEncoder::new().with_composition(CompositionMode::Preserve)`
  re-authors the source structure: sublayer / reference / payload
  / inherits / specializes opinions return on their prims exactly
  as written (list-edit operators included), prims an arc or a
  variant selection contributed are left to that arc, `class` /
  `over` prims replay verbatim, the consumed layer entries and
  their assets are copied byte-for-byte into the package, and the
  root layer keeps its entry name so anchored paths still resolve.
  Both modes are one-cycle round-trip fixed points; preserved
  packages pass `usdchecker` where it is installed.

#### List-edit operators

Composition-arc and list-valued metadata fields (`references`,
`payload`, `inherits`, `specializes`, `apiSchemas`, `subLayers`, …) are
**list-edited**: the parser captures the `prepend` / `append` /
`delete` / `add` / `reorder` operator rather than discarding it, and
several authored statements of the same field on one prim merge
losslessly into a single `SdfListOp`-style value with separate
*prepended* / *appended* / *deleted* / *explicit* / *reordered*
sublists. Composition flattens the additive sublists in LIVRPS strength
order (*prepended* → *explicit* → *appended*) and then removes any
`delete`-authored target, so `delete references = @x@` is an actual
removal rather than being treated as an add. The writer re-emits each
authored operator on its own line.

### Round-trip fidelity

`tests/fixture_fixed_point.rs` runs every staged fixture under
`docs/3d/usd/fixtures/` (the production Elephant Crate, the
variant-spec Crate, the alignment package) through decode → encode →
decode in both composition modes and pins a **one-cycle fixed
point**: byte-identical second package, identical typed-model digest
(node / mesh / primitive / vertex / index / UV / normal / joint /
morph / material / texture / skeleton / skin / animation / channel /
keyframe / audio counts and names), exact positions, indices,
normals, UVs and keyframes. The channels that used to degrade are
closed: a Mesh prim's own `xformOp` lives once on the carrier node
(the writer collapses the carrier back onto the `def Mesh`, metadata
and all); a `SpatialAudio` carrier re-emits as that prim; each
SkelAnimation array is written on its own keyframe timeline (a
static `scales` no longer resamples onto the `rotations` timeline,
and a joint's value is held, never defaulted); authored
`bindTransforms` replay verbatim while they still invert onto the
typed matrices; `f32` data prints the shortest round-trip digits and
`f64` values (timeCodes included) the §16.2.5 double-precision
spelling, `-0` normalised.

## USDC (binary crate file) — full reader + structural writer

A clean-room Crate reader implemented from the AOUSD *USD Core
Specification* 1.0.1 §16.3 (staged in `docs/3d/usd/`), cross-checked
against a committed real-production fixture. **A `.usdz` whose default
layer is a `.usdc` now decodes end-to-end to a `Scene3D`**, and a
generic `.usd` layer dispatches on its header byte run (§16.1:
`PXR-USDC` magic = Crate, `#usda` banner = text).

- **Preamble + sections** — bootstrap, version gate (reader ceiling
  Crate 0.12.0 — the last row of the §16.3.8.2 version table), the
  tail TOC, and all six standard
  sections (TOKENS, STRINGS, FIELDS, FIELDSETS, PATHS, SPECS) with
  the §16.3.7 compression stack: chunked LZ4 buffers and the
  compressed-integer coding (2-bit control stream + common-delta
  preamble, 32- and 64-bit widths, difference model per the spec's
  worked example — which is carried verbatim as a test).
- **Paths** — the §16.3.8.4.5.4 path-construction algorithm rebuilds
  every `SdfPath` from the three parallel PATHS arrays (jump codes
  `-2` leaf / `-1` child / `0` sibling / positive child+sibling; the
  element-token sign selects the prim `/` vs property `.` delimiter),
  with corruption guards for cycles, out-of-table jumps, and
  duplicate/unfilled slots.
- **Typed values** — `usdc::ValueType` carries the §16.3.10.1 type
  table (IDs 1–59, per-type Supports-Array), and
  `usdc_values::ValueDecoder` resolves any value representation to a
  concrete `usda::Value`: the §16.3.9.1 inlining rules (4-byte
  payload limit, int8 vector/matrix-diagonal packing,
  `double`-as-`float`, pool-index token/string/asset), offset
  scalars for every dimensioned width (row-major matrices,
  imaginary-first quaternions re-ordered to the text form's
  real-first), uncompressed arrays, compressed integral arrays, the
  §16.3.9.3.2 compressed floating-point codings (`i` all-integral
  and `t` lookup-table), index vectors, dictionaries, the
  six-sublist list operations, variant-selection maps, time samples,
  references/payloads, recursion-guarded indirect values, the
  specifier / permission / variability enums, the 0.11.0
  `Relocates` map (§16.3.10.15 path-index pairs; the
  `layerRelocates` layer field surfaces under the text form's
  `relocates` key), and the 0.12.0 `Splines` value (§16.3.10.33:
  the flag bytes, sloped / loop parameters, the typed knot run in
  double / float / half, and the per-time custom-data
  dictionaries) — an attribute's `spline` field becomes the text
  form's `<attr>.spline` companion statement. On the committed
  fixture **all 157 field values decode**, pinned down to exact
  layer metadata, mesh arrays, and 3023-sample animation tracks.
- **Scene bridge** — `usdc_layer::layer_from_usdc` materialises the
  §16.3.8.4.6 spec table (path + form + field set) into the same
  `usda::Layer` prim-tree model the text parser produces: prim specs
  with specifier / typeName / `primChildren` ordering, attribute
  specs with their `custom` / `uniform` spelling plus
  `.timeSamples` / `.connect` companion statements, relationship
  specs as `rel` statements. The existing `usd_to_scene` pipeline
  then builds the typed scene — the fixture yields its full
  UsdPreviewSurface networks, skinned meshes, 27-joint skeleton,
  sampled animation, and spatial audio, `Scene3D::validate()`-clean.
- **Structural writer** — `usdc_writer::CrateImage` re-emits a
  parsed `.usdc` (bootstrap, six section payloads in canonical
  order, tail TOC) byte-exactly on the committed fixture, and
  supports read-modify-write (token interning + field authoring).
  Value reps ride through the writer losslessly as raw words — a
  typed value *writer* is not implemented yet.

Variant / VariantSet spec forms (10/11) bridge into the text
model's `variantSet` blocks — the `{set=sel}` selector paths fold
each Variant's prim-field opinions (attributes, relationships,
child prims) into `variant_sets`, `variantSelection` maps become
`variants` metadata, and selections resolve through the ordinary
composition pipeline (pinned against the staged
`crate-variant-specs.usdc` fixture: 7 Variant + 3 VariantSet specs
across two namespace levels). Not yet implemented on the Crate
path: `UnregisteredValueListOp` payloads refuse with a precise
message.

## Not yet supported

- `UsdGeomCamera` / `UsdLux` light schemas — unstaged in
  `docs/3d/usd/` (GAP-TRACKER Round F: on demand).

## Standalone build

`oxideav-core` is gated behind the default-on `registry` cargo feature.
Drop the framework dependency tree with:

```toml
oxideav-usdz = { version = "0.0", default-features = false }
```

The reader still parses USDZ into the same `Scene3D` typed model and the
writer still emits valid USDZ archives — only the `register()` glue and
the `Error` alias change.

## License

MIT. See [LICENSE](LICENSE).
