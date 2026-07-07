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

- **PKZIP central-directory walk** — STORED-only entries (the USDZ spec
  rejects any compression method); every payload offset is verified
  against the 64-byte alignment requirement. **ZIP64 is rejected at the
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
- **Materials** — `UsdPreviewSurface` → `Material` (base color, metallic,
  roughness, emissive) with `UsdUVTexture` connections for diffuse /
  normal / emissive / occlusion maps.
- **Transforms** — per-node `UsdGeomXformable` xformOps round-trip
  (`translate` + `orient` quatf + `scale`, or a `matrix4d` transform).
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

### Composition

The read path evaluates USD composition arcs:

- variantSets and variant selection,
- anchored sublayer composition (LayerStack),
- `references` / `payload` arcs (including targets that live as their own
  entries in the same archive),
- in-layer `inherits` / `specializes`,

resolved in the documented strength order. The writer flattens the
composed tree into a single output layer rather than preserving the
multi-file or class-hierarchy authoring.

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

## USDC (binary crate file) — partial reader + structural writer

A clean-room USDC reader is in progress, with a matching **structural
writer** (`usdc_writer::CrateImage`). Implemented so far:

- The §1 version-compatibility dispatch gate (`usdc::Version` with a
  reader-max ceiling; files newer than the ceiling are refused up front).
- The TOC and the outer framing of all six standard sections — TOKENS,
  STRINGS, FIELDS, FIELDSETS, PATHS, SPECS — with typed section accessors
  and canonical-order predicates.
- The §3a compressed-buffer framing with the public LZ4 block decode,
  and the §3b compressed-integer decoder including the 4-byte
  common-delta preamble. The int-coded decoder enforces the §3b
  "zero leftover bytes" invariant: a buffer whose control stream and
  payload disagree on the array's byte length (trailing unconsumed
  payload after the last element) is rejected rather than silently
  truncated.
- End-to-end section-content decode chaining §3a → LZ4 → §3b: the TOKENS
  atom pool, the FIELDS `(nameIndex, valueRep)` pairs, the FIELDSETS flat
  index array, and the three PATHS buffers — each decoding exactly on the
  committed Elephant fixture (192 tokens, 157 fields, 576 field-set
  indices, 248 paths). Each FIELDS value-rep word is split into a typed
  `ValueRep` — the high-16-bit type-code + flags word and the low-48-bit
  inline-or-offset payload (lossless: `ValueRep::raw()` rebuilds the
  original `u64`). The flag bits are **observer-grounded from the fixture
  bytes**: `is_array` (bit 63), `is_inline` (bit 62) and `is_compressed`
  (bit 61) — across all 157 Elephant reps the flag byte only ever takes
  `0x00/0x40/0x80/0xa0`, array and inline are mutually exclusive, and
  compressed only rides on array. `UsdcFile::value_region` then resolves
  a rep to its on-disk value: an inline payload (bit-cast via
  `as_f32`/`as_i32`/`as_u32`/`as_bool`), a `[u64 count][elements]`
  uncompressed array (`array_elements_exact(width)` trims to
  `count × width`), a compressed-array `(count, offset)`, or a scalar
  offset — all bounds-checked against the value region `[0x58,
  toc_offset)`. The fixture's inline `0x4009` rep decodes to the `float`
  `60.0` and its `0x0f` `double[]` array reads back with `1.0` as element
  0. For the **integer** compressed-array class,
  `UsdcFile::decode_compressed_int_array` runs the documented §3a → LZ4 →
  §3b path on the compressed region (bounded by `int_coded_max_len`) and
  returns the `count` recovered `i32`s; `ValueRegion::array_count` /
  `compressed_region_offset` expose the array shape uniformly. The
  compressed float/double/half element coding is a *separate* (deferred)
  encoding — a real-fixture test pins that the Elephant's compressed
  arrays are not the §3b integer form, and `decode_compressed_int_array`
  fails cleanly on them rather than fabricating integers (the caller must
  establish integrality first). Only the *naming* of the raw
  `type_code()` byte (which code is `float`, which is `int[]`) stays
  deferred — that enumeration is GAP-TRACKER Round B.
- The STRINGS → TOKENS and field-name TOKENS pool resolution and the
  full §5 SPECS → FIELDSETS → FIELDS join (`decode_specs` /
  `decode_named_specs`), which materialises all 248 Elephant spec rows
  with their field names resolved to strings. Each spec row's `pathIndex`
  is bounds-checked against the §4.5 PATHS `numPaths`, so a corrupt
  path-index column referencing a non-existent namespace path is rejected
  rather than producing a dangling `SdfPath` reference.
- The §4.5 PATHS structural join (`decode_path_elements`) — the three
  parallel buffers are joined into one typed `PathElement` per tree-walk
  slot. Parsing the fixture bytes pins the per-element buffer roles
  tighter than the trace doc's column header: buffer 1 is the
  **target-slot permutation** (an exact permutation of `0..numPaths`),
  buffer 2 is the **element-token word** whose `abs(word) >> 1` is an
  in-range §4.1 TOKENS index for all 248 elements (resolving to `Xform` /
  `Material` / `xformOp:transform` / … via `element_token`), and buffer 3
  is the **jump** offset. The element-token sign + low bit and the
  jump-walk are preserved verbatim and left to the deferred tree
  reconstruction below.
- The §4.5↔§4.6 PATHS↔SPECS join naming each spec's leaf component.
  `decode_path_elements_by_slot` puts the path elements into
  `target_index` order (the index space a SPECS `path_index` addresses,
  validated as a permutation), and `decode_spec_leaf_names` joins the
  SPECS table to it so each spec row gets its own path-component token
  (the pseudo-root spec carries the stage metadata, prim specs get their
  type/name token — `Xform` / `Materials` / …). This names *what each
  spec is*; the full ancestor `SdfPath` still needs the deferred
  jump-walk.
- **Structural writer** — `usdc_writer::CrateImage` is the inverse of
  the reader at the documented structural layer. `from_bytes` /
  `from_file` decode a `.usdc` into a content image (the six sections'
  decoded arrays); `to_bytes` re-emits the bootstrap, the six section
  payloads in canonical order, and the tail TOC, using the §3a
  single-chunk LZ4 framing and §3b integer coding. The committed
  Elephant fixture round-trips structurally (all 192 tokens / 157
  fields / 576 field-set indices / 248 paths / 248 specs reproduced),
  and supports **read-modify-write**: load, intern tokens / `add_field`
  authored properties, and re-emit a valid file. The §4.3 value-rep
  words ride through opaque (the type-code enumeration is not staged),
  so authored values are preserved losslessly rather than interpreted.
  The public §3a/§3b encoders (`encode_compressed_buffer`,
  `encode_int_coded`) are the byte-exact inverses of the read path.

Not yet implemented (so USDC files do not fully decode to a scene yet):

- **FIELDS value-rep type-code naming** — the value-rep word's flag
  bits (array / inline / compressed) and its on-disk value region (inline
  payload, uncompressed/compressed array, scalar offset) are now decoded,
  so a caller that knows an element is a 4-byte `float` can read it. What
  remains is the **mapping from the raw `type_code()` byte to a named USD
  value type** (`0x09` → `float`, `0x0f` → `double`, `0x29` → `int[]`,
  …). That enumeration lives only in the off-limits reference-implementation source and is
  not yet staged in `docs/3d/usd/` — GAP-TRACKER Round B. Without it the
  reader cannot auto-select element widths or build a typed scene from a
  `.usdc` value section.
- **PATHS namespace-tree reconstruction** — the three parallel buffers
  now decode to a typed `PathElement` join (target-slot permutation,
  element-token index + name, raw element-token word, jump), but the
  **jump-walk** that turns those triples into full `SdfPath` strings is
  not yet documented: how the absolute root is seeded, what the `jump`
  sign cases (`-1` / `-2` / `0` / positive) mean for has-child /
  has-sibling descent, and what the element-token sign + low bit
  distinguish are not pinned by the staged trace (GAP-TRACKER §1 /
  Round B). The decode stops at the verified parallel-array join so no
  un-grounded path string is fabricated.

## Not yet supported

- `UsdSkelSkeleton` / `UsdSkelBindingAPI` skeletal-animation skinning,
  and `UsdGeomSubset` per-face material subsets — both blocked on
  `UsdSkel` schema docs not yet staged in `docs/3d/usd/`.
- Cross-package `@foo.usdz[path/within.usd]@` selectors into a sibling
  archive — preserved as side-channel opinions rather than resolved.
- SubLayer / reference / class-arc structure preservation on the writer
  (the encoder flattens; see Composition above).

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
