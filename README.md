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

let usdz_bytes = std::fs::read("model.usdz")?;
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
  payload offset padded onto a 64-byte boundary by one well-formed
  extra-field record in the local file header (id `0x1986`, zero
  body — present even when no padding is needed), per-entry CRC32,
  the root layer first in both the file and the central directory,
  and the End of Central Directory record last with no comment.
  `tests/usdz_package_rules.rs` re-walks the bytes with its own
  parser and, where the tools are installed, has `usdchecker` pass
  the package, `usdzip` report the same entries at the same data
  offsets and `usdcat` open the root layer through it — for a text
  and for a Crate root layer. A prim that authors a material
  binding applies `MaterialBindingAPI`. The
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
  `spline::Spline` and re-emits as a one-cycle fixed point. A
  two-number text tangent is `(slope, width)` (§16.2.16.5); the
  Crate knot table stores the same fields widths first. One
  spelling the staged specification leaves open is inferred and
  flagged: the Crate 2-bit interpolation code follows the
  §7.4.2.4.2 enumeration order.
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
  Any other stack — Euler rotations, pivots and other suffixed ops,
  a different order, and every *animated* op (`.timeSamples` /
  `.spline`) — is kept as authored and replayed verbatim, op order
  included; the typed node then holds the static part it can read
  (a gprim's carrier node stays at identity so the stack is not
  applied twice).
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

### Unmodelled properties

Every authored property the typed model has no slot for — custom
attributes, `visibility` / `purpose`, tool namespaces, time-sampled
scalars on containers — rides on `Node::extras["usd:attrs"]` (the
tagged `Attr` codec) and replays verbatim; an unknown-schema prim
(`Cube`, `Camera`, a light) keeps its type token, transform, whole
body and children the same way. On gprims the same stash sits on
`Primitive::extras["usd:attrs"]` for what is not topology-indexed
(custom scalars, constant primvars); per-face / per-vertex primvars,
hole / crease / corner tables and `extent` are left out because the
writer emits re-triangulated topology those arrays would no longer
describe. Sampled points (`points.timeSamples` — a point cache) are
indexed by point, which the writer preserves, so they replay as
authored; a mesh that authors `points` *only* through time samples
decodes with its earliest sample as the rest pose.

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

### Layer model: text ↔ Crate without loss

`usda::parse` and `usdc_layer::layer_from_usdc` build the same
layer model, and both writers take it back:
`usda_emit::layer_to_usda(&layer)` prints any layer as `#usda 1.0`
text and `usdc_encode::encode_layer(&layer)` as a Crate file. The
`layer_sweep` suite reads every staged fixture and the in-tree
reference layer (`tests/data/layer-model.usda` and the Crate file a
black-box encoder made from it), writes each as Crate and as text,
reads both again and pins:

- the Crate form is a byte-stable fixed point and re-reads to the
  identical model;
- the text form parses and encodes to the *same Crate bytes*, and
  printing that again gives the same text;
- a text layer and its black-box Crate twin print and encode
  identically;
- where `usdcat` is installed, it prints our Crate and our text
  exactly as it prints the source file.

What the model carries beyond values and time samples:

| Construct | Text form | Model |
|---|---|---|
| layer offsets on sublayers | `@a.usda@ (offset = 10; scale = 2)` | `subLayerOffsets` beside `subLayers` |
| layer offsets / custom data on references and payloads | `@a.usd@</P> (offset = 5; customData = {…})` | `Value::Reference` |
| list-edited relationships and connections | `prepend rel x = …`, `delete float y.connect = …` | `Value::ListOp` on the property |
| the deprecated `add` operator | `add rel x = …` | `ListOp::added` |
| blocked default vs. no default | `int x = None` / `int x` | `Value::Block` / `Value::None` |
| emptied vs. bare relationship | `rel x = None` / `rel x` | empty array / `Value::None` |
| child and property order | `reorder nameChildren`, `reorder properties`, `reorder rootPrims` | `primOrder` / `propertyOrder` |
| typed dictionary members | `double d = 1`, `float f = 1.5`, `string[] s = []` | `Value::Typed` when the value alone would not tell |
| 64-bit integers past 2⁵³ | `int64 id = 9007199254740993` | `Value::Integer` |
| nested variant sets | `variantSet` inside a variant | `Variant::variant_sets` |
| spec comment | a bare string in the metadata block | `comment` |

`inherits` is the Crate field `inheritPaths` and `doc` the field
`documentation` on prims *and* properties; a lone unedited payload
is the scalar `Payload` value. Schema families with no typed slot
(`Camera`, the light types) read generically: type name, every
typed attribute, time samples and metadata are preserved and
written back, with no schema semantics applied.

## USDC (binary crate file) — full reader + typed writer

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
  parsed `.usdc` (bootstrap, value region, six section payloads in
  canonical order, tail TOC) byte-exactly on the committed fixture,
  and supports read-modify-write (token interning + field
  authoring).
- **Typed writer** — `usdc_encode::encode_layer` serialises the
  text model (`usda::Layer`) into the §16.3 binary form: the
  §16.3.8.4.5 path tree (prims, properties, `{set=}` /
  `{set=sel}` variant elements, every path a value points at)
  walked with the §16.3.8.4.5.4 jump codes; one spec per authored
  path (Layer / Prim / Attribute / Relationship / VariantSet /
  Variant forms with `specifier`, `typeName`, `primChildren`,
  `properties`, `variantSetNames` / `variantSetChildren` /
  `variantSelection` / `variantChildren`, `default`,
  `timeSamples`, `connectionPaths`, `targetPaths`, `spline`,
  `custom`, `variability`, metadata, `subLayers` +
  `subLayerOffsets`, `documentation`, `layerRelocates`); and every
  §16.3.9 / §16.3.10 value encoding — the inlining rules (4-byte
  scalars, pool indices, int8 vectors, matrix diagonals, empty
  dictionaries), offset scalars, arrays, dictionaries (members
  chained after their `i64` link, as production files lay them
  out), the six-sublist list operations, reference / payload
  records, variant-selection maps, time samples, token / string /
  path vectors, relocates (0.11) and splines (0.12). The attribute
  type token selects the wire type (`point3f[]` → `Vec3f[]`,
  `half3[]` → `Vec3h[]`, …), untyped metadata encodes from its
  shape, and the file is stamped with the lowest §16.3.8.2 row its
  content needs. `UsdzEncoder::new().with_layer_format
  (LayerFormat::Usdc)` packages a `.usdc` root layer; `usdcat`
  reads our output and `usdchecker` passes the package where the
  tools are installed, and the staged fixtures are one-cycle fixed
  points through the Crate form.
- **Array coding and value sharing** — per array the writer
  stores the smallest of the §16.3.9.3 forms: raw (on an 8-byte
  boundary, §16.3.3), compressed integers for `int` / `uint` /
  `int64` / `uint64`, and for scalar `half` / `float` / `double`
  arrays the all-integral (`i`) or lookup-table (`t`) coding;
  dimensioned element types stay raw, as the format requires.
  Arrays shorter than sixteen elements are never compressed.
  Identical values (arrays, offset scalars, list operations,
  time-code vectors, whole time-sample records, dictionaries) are
  stored once, identical field sets share one run, and the
  time-sample record is a single position-independent block. The
  output decodes to the same layer as the raw form, is byte-stable
  through read → write, and a black-box reader prints both forms
  identically. `usdc_encode::encode_layer_with(layer,
  ArrayCoding::Raw)` keeps the uncompressed, unshared layout.
- **Dictionary keys** index the STRINGS section on both the read
  and the write path — what a Crate written by a black-box encoder
  carries (pinned by `tests/data/dictionary-keys.usdc`);
  §16.3.10.19 words the key as a TOKENS index, which neither that
  file nor a black-box reader bears out.
- `timecode` values stamp Crate 0.9.0 and `pathExpression` values
  0.10.0 (§16.3.8.2).

### Crate writer size

Staged fixtures read into the layer model and written back
(`cargo test --release --test usdc_compression -- --nocapture`):

| Fixture | Source file | Raw writer | Smallest coding |
|---|---:|---:|---:|
| `SoC-ElephantWithMonochord.usdc` | 851 298 B | 2 932 917 B | **736 462 B** (0.25× raw, 0.87× source) |
| `crate-variant-specs.usdc` | 1 836 B | 1 943 B | **1 764 B** |

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

## Read / write speed

`USDZ_PERF_SIDE=1000 cargo test --release --test perf_report -- --nocapture --test-threads=1`
times every stage of both pipelines and reports the peak heap growth
of one run (a counting allocator in the test). The synthetic layer is
a 1000 × 1000 point grid — 1 M points, 1 M quads, normals, a UV set,
three time samples of the points and a 240-sample transform — and the
fixture is the staged production Crate. Medians (5 runs for the
grid, 7 for the fixture; the slower of two invocations) on a machine
shared with other builds, before → after this round's work:

| 1 M-point grid | time before | time after | peak heap before | peak heap after |
|---|---:|---:|---:|---:|
| Crate → layer model | 219.7 ms | **68.2 ms** | 1281 MiB | 1281 MiB |
| layer model → Crate (smallest coding, 56.0 MB) | 576.5 ms | **95.2 ms** | 252 MiB | 199 MiB |
| layer model → Crate (raw, 88.0 MB) | 554.7 ms | **81.9 ms** | 359 MiB | 320 MiB |
| layer model → text (163.6 MB) | 1777.3 ms | **604.6 ms** | 434 MiB | 464 MiB |
| text → layer model | 674.1 ms | 618.0 ms | 1627 MiB | **1284 MiB** |
| package (Crate root) → scene | 2047.1 ms | **231.7 ms** | 7741 MiB | **1415 MiB** |
| scene → package (text root, 105.6 MB) | 1093.6 ms | **370.9 ms** | 462 MiB | 462 MiB |
| scene → package (Crate root, 32.0 MB) | 1572.9 ms | **941.5 ms** | 1679 MiB | **1136 MiB** |

| `SoC-ElephantWithMonochord.usdc` (851 kB) | time before | time after | peak heap after |
|---|---:|---:|---:|
| Crate → layer model | 7.7 ms | **3.6 ms** | 39 MiB |
| layer model → Crate (smallest coding) | 20.1 ms | **6.2 ms** | 3 MiB |
| layer model → text (9.2 MB) | 90.6 ms | **45.0 ms** | 24 MiB |
| text → layer model | 28.2 ms | 31.6 ms | 40 MiB |
| package (Crate root) → scene | — | 21.9 ms | 62 MiB |
| scene → package (text root) | — | 62.5 ms | 40 MiB |
| scene → package (Crate root) | — | 99.0 ms | 52 MiB |

What changed: the decoder hands the layer to composition instead of
copying it (five full copies of the layer model during a decode
became one — the 7.7 GiB peak), the inherit / specialize snapshots
are taken only when a prim authors those arcs, variant resolution
moves prims instead of cloning them, array elements are read and
written without a heap allocation each, the Crate writer borrows
arrays instead of cloning them, the text writers format numbers
straight into the output, the parser scans numbers and ASCII
without per-character decoding, tuples keep exactly the room they
use, the compressed-integer coder tallies deltas in a flat table,
and the package CRC-32 folds eight bytes per step (it runs over
every entry on both read and write). Output is byte-identical
throughout.

The layer model itself — one tagged value per number — is the
remaining cost: about 224 bytes per point, which is what the
1.28 GiB of the grid's four point arrays, normals, UVs and
topology is.

## Not yet supported

- A typed mapping for `UsdGeomCamera` / `UsdLux` prims: they are
  read and written generically (every attribute, time sample and
  metadata field preserved) with no schema semantics applied.
- `UnregisteredValueListOp` Crate payloads (refused with a precise
  message), and layer offsets on the *typed scene* — the layer
  model keeps them, the scene has no slot for retimed arcs.
- The deprecated `add` / `reorder` list operators compose as plain
  additions / no-ops; both formats carry them through unchanged.

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
