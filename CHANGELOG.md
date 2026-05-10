# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
