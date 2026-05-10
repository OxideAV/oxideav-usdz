# oxideav-usdz

Pure-Rust **USDZ** (Universal Scene Description, zipped) reader.
USDZ is Pixar's container for shipping ready-to-go USD assets — an
uncompressed, 64-byte-aligned ZIP archive whose first entry is a
`.usd` / `.usda` / `.usdc` "Default Layer" referenced by every
companion file (textures, audio, sub-layers). Apple Quick Look on
iOS / macOS uses USDZ as its native AR scene format.

The crate plugs into the
[`oxideav-mesh3d`](https://crates.io/crates/oxideav-mesh3d) typed
model — call `oxideav_usdz::register(&mut Mesh3DRegistry)` and
dispatch by file extension, or invoke `UsdzDecoder::new()` directly
and feed it bytes for a `Scene3D`.

## Round 1 scope

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
  so a future round-2 USDZ writer can pass the inner-file bytes
  through verbatim into the output ZIP without re-buffering.
- **`upAxis` + `metersPerUnit`** map onto `Scene3D::up_axis` and
  `Scene3D::unit` (with the standard preset list — metres /
  centimetres / millimetres / inches / feet / yards).

## Deferred to round 2+

- **USDZ encoder.** Round 1 is read-only. The encoder will
  recognise `AssetSource::raw_storage("zip-stored")` on input and
  copy bytes verbatim into the output archive (no re-encode for
  USDZ → USDZ pipelines).
- **Binary `.usdc` "Crate" parser.** Pixar publishes no prose
  spec for the wire format; loading a `.usdc` Default Layer in r1
  surfaces as `Error::Unsupported` with a hint to re-package with
  `usdcat -o foo.usda`.
- **`UsdSkelSkeleton` + `UsdSkelBindingAPI`** skeletal-animation
  skinning.
- **`UsdMediaSpatialAudio`** → `AudioSource` + `AudioEmitter`.
- **`UsdGeomSubset`** per-face material-binding subsets.
- **Composition arcs** — sub-layers, references, payloads that
  pull in external USD files. Round 1 reads a single
  self-contained USD layer per archive only.

## Standalone build

`oxideav-core` is gated behind the default-on `registry` cargo
feature. Drop the framework dependency tree entirely with:

```toml
oxideav-usdz = { version = "0.0", default-features = false }
```

The decoder still parses USDZ into the same `Scene3D` typed model
— only the `register()` glue and the `Error` alias change.
