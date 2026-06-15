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

## USDC (binary crate file) — partial reader

A clean-room USDC reader is in progress. Implemented so far:

- The §1 version-compatibility dispatch gate (`usdc::Version` with a
  reader-max ceiling; files newer than the ceiling are refused up front).
- The TOC and the outer framing of all six standard sections — TOKENS,
  STRINGS, FIELDS, FIELDSETS, PATHS, SPECS — with typed section accessors
  and canonical-order predicates.
- The §3a compressed-buffer framing and the §3b compressed-integer
  decoder, plus the STRINGS → TOKENS and field-name TOKENS pool
  resolution and the SPECS → FIELDSETS → FIELDS resolved join.

Not yet implemented (so USDC files do not fully decode to a scene yet):

- **LZ4 block decompression** of chunk payloads — the framing exposes
  each chunk's raw bytes, but the block decode awaits a staged
  clean-room trace of the public LZ4 block format.
- **§3b common-value semantic recovery** and the **FIELDS value-rep
  type-code** materialisation that turn the bounded section buffers into
  values.

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
