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

## Deferred to round 4+

- **Binary `.usdc` "Crate" parser.** Pixar publishes no prose
  spec for the wire format; loading a `.usdc` Default Layer
  still surfaces as `Error::Unsupported` with a hint to
  re-package with `usdcat -o foo.usda`.
- **`UsdSkelSkeleton` + `UsdSkelBindingAPI`** skeletal-animation
  skinning.
- **`UsdMediaSpatialAudio`** → `AudioSource` + `AudioEmitter`
  (writer side too — same `raw_storage("zip-stored")`
  pass-through optimisation will apply to audio assets once the
  reader plumbs them in).
- **`UsdGeomSubset`** per-face material-binding subsets.
- **Composition arcs** — sub-layers, references, payloads that
  pull in external USD files. Round 1/2 read+write a single
  self-contained USD layer per archive only.

## Standalone build

`oxideav-core` is gated behind the default-on `registry` cargo
feature. Drop the framework dependency tree entirely with:

```toml
oxideav-usdz = { version = "0.0", default-features = false }
```

The reader still parses USDZ into the same `Scene3D` typed model
and the writer still emits valid USDZ archives — only the
`register()` glue and the `Error` alias change.
