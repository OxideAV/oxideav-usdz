# oxideav-usdz

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
  against the 64-byte alignment requirement.
- **Writer** emits a byte-faithful archive: STORED entries only, every
  payload offset padded (via the LFH `extra` field) onto a 64-byte
  boundary, per-entry CRC32, correct central directory + EOCD.
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
  0. Only the *naming* of the raw `type_code()` byte (which code is
  `float`, which is `int[]`) stays deferred — that enumeration is
  GAP-TRACKER Round B.
- The STRINGS → TOKENS and field-name TOKENS pool resolution and the
  full §5 SPECS → FIELDSETS → FIELDS join (`decode_specs` /
  `decode_named_specs`), which materialises all 248 Elephant spec rows
  with their field names resolved to strings. Each spec row's `pathIndex`
  is bounds-checked against the §4.5 PATHS `numPaths`, so a corrupt
  path-index column referencing a non-existent namespace path is rejected
  rather than producing a dangling `SdfPath` reference.
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
  (path-token indices, element-token indices, jump offsets) decode to
  their raw integer streams, but the jump-walk that turns them into
  `SdfPath` strings is not yet documented (the per-element semantics and
  the jump-offset encoding are not pinned by the staged trace).

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
