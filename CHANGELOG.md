# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
