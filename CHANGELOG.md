# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
