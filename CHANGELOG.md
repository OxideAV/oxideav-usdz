# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Round 217 (USDC §4.2 STRINGS section parser)

- **`usdc::StringsHeader` / `usdc::StringsSection`** — the §4.2
  STRINGS section: an 8-byte little-endian `int64 count` header
  followed by `count × uint32` (raw, NOT LZ4-compressed per the
  trace doc) little-endian token indices. The STRINGS pool is the
  subset of TOKENS atoms used as USDA *string-typed* values —
  §4.3 FIELDS string-valued reps index into this table by `uint32`
  position. `StringsHeader::parse` reads the 8-byte count;
  `StringsSection::parse` enforces `8 + count * 4 == section_size`
  strictly so any trailing byte the trace doesn't authorise is an
  `Error::InvalidData`; `StringsSection::parse_indices` decodes
  the raw `uint32` array into an owned `Vec<u32>` for callers that
  want to walk the pool. Defensive cap on `count` (16 Mi, several
  orders of magnitude above the teapot trace's `count = 15`)
  protects against runaway allocation from a hostile or corrupted
  header. Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §4.2.
- **8 new unit tests** in `usdc::tests` cover the §4.2 header
  parsing for `count = 0` (Elephant's empty case) and the
  truncated-input error path, the oversized-count rejection
  against the defensive cap, the empty-section happy path, the
  trace's teapot-shape worked example (15 sequential `uint32`s
  walked through `parse_indices`), short-index-array and
  trailing-bytes error paths exercising the strict
  `header(8) + count * 4 == section size` invariant, and a
  short-header propagation case where `StringsSection::parse`
  bubbles up `StringsHeader::parse`'s truncation error.
- **Real-fixture cross-validation** —
  `real_fixture_strings_section_parses_zero_count` reads
  `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc` through
  `UsdcFile::parse`, locates the STRINGS TOC entry, and asserts
  the trace-doc-published `offset = 0x0cf2da` / `size = 8` facts
  hold on the wire — then parses the section and confirms
  `count = 0` (matching the trace's §4.2 worked example), no
  index bytes, and `parse_indices()` yields an empty vector.
  Skips silently when the fixture is absent so CI remains green
  in submodule-less checkouts.

### Added — Round 212 (USDC §3a compressed-buffer framing + §4.1 TOKENS section header)

- **`usdc::CompressedBuffer` / `CompressedChunk`** — the §3a
  outer "compressed buffer" framing: a leading byte that
  declares "extra chunks" (so `total = extra + 1`) followed
  either by the entire remainder as a single LZ4-block chunk
  (the common `0x00` case the trace doc cites for every
  observed buffer in both samples) or by `(int32 LE length,
  bytes)` chunk records. The LZ4 *block* payload inside each
  chunk is left opaque — the public LZ4 block-format spec is
  not staged under `docs/`, so this module stops at the
  envelope. Borrows from the input slice; bounded against
  truncated length prefixes, chunks running past end-of-buffer,
  and negative declared lengths. `as_single_chunk()` shortcut
  for the common path.
- **`usdc::TokensHeader` / `TokensSection`** — the §4.1 TOKENS
  section header: three little-endian `int64`s (`numTokens`,
  `uncompressedSize`, `compressedSize`) plus the bounded §3a
  compressed-buffer slice that follows it. Defensive caps on
  `numTokens` (16 Mi) and `uncompressedSize` (256 MiB) protect
  against runaway allocation from a hostile or corrupted
  header — both are several orders of magnitude above the
  Elephant sample's 192 / 4195 figures published in the trace
  doc. `TokensSection::parse` slices exactly
  `compressed_size` bytes after the 24-byte header so a
  caller never reads past the section bounds, and the
  convenience `buffer()` method forwards into
  `CompressedBuffer::parse`.
- **`usdc::split_tokens_blob`** — the cross-section seam for
  callers that have plugged in their own LZ4 decoder for the
  inner block: takes the *decompressed* TOKENS blob plus the
  original `TokensHeader`, verifies the
  `blob.len() == uncompressed_size` equality, NUL-splits into
  the recorded `numTokens` UTF-8 strings (accepting either a
  trailing NUL or a NUL-as-delimiter encoding — the trace
  doesn't constrain the trailing byte), and validates UTF-8
  per token. Returns `Vec<String>`.
- **17 new unit tests** in `usdc::tests` cover §3a empty-input
  rejection, the single-chunk `0x00` form, an empty
  single-chunk buffer, two- and three-chunk forms walking
  each `(int32 length, bytes)` record, three error paths
  (truncated length prefix, chunk overrun, negative declared
  length), the §4.1 header parsing the Elephant's
  `numTokens=192 / uncompressedSize=4195 / compressedSize=1746`
  triple from the trace doc, three header error paths
  (truncated buffer, oversize `numTokens`, oversize
  `uncompressedSize`), section-bounds clamping at
  `compressed_size`, a truncated-buffer error path, two
  `split_tokens_blob` happy paths (trailing-NUL and
  delimiter-only forms), three error paths (size mismatch,
  count mismatch, non-UTF-8 byte), and a 14-token round-trip
  exercising the §4.1 trace-doc-published string set
  (`defaultPrim`, `endTimeCode`, `xformOp:transform`, …).
- **Real-fixture cross-validation** — `real_fixture_tokens_section_header_parses`
  reads `docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc`
  through `UsdcFile::parse`, locates the TOKENS TOC entry,
  parses the section header, and asserts the
  `numTokens=192 / uncompressedSize=4195 / compressedSize=1746`
  trace-doc facts hold on the real bytes — plus
  `header(24) + compressed_size = section.size` (1770) and
  that the §3a framing parses to a single chunk whose payload
  is exactly `compressed_size - 1` bytes long (the LZ4 block
  proper). Skips silently when the fixture is absent so CI
  remains green in submodule-less checkouts.

### Added — Round 206 (USDC §3b compressed-integer decoder)

- **`usdc::decode_int_array`** — decodes the §3b "compressed
  integer" wire shape: a 2-bit-per-element control stream
  (`ceil(N/4)` bytes, LSB-first) selecting one of four widths
  per element, then the variable-width payload bytes. Code 0
  repeats the previous value; codes 1 / 2 encode signed `i8` /
  `i16` deltas from the previous value; code 3 encodes an
  absolute `i32` that becomes the new "previous". Returns
  `Vec<i32>`; rejects a truncated control stream or short
  payload for the declared width with `Error::InvalidData`.
  This is the primitive consumed by FIELDS' name-index array,
  FIELDSETS' field-index array, and SPECS' three index arrays
  once the §3a LZ4 wrapper lands. Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md` §3b.
- **`usdc::encode_int_array_for_tests`** — test-only
  companion encoder so the §3b decoder can be exercised
  end-to-end against synthesised integer sequences (varied
  zero-delta / i8-delta / i16-delta / absolute-i32 patterns)
  without first committing a corpus of real `.usdc` byte
  buffers. Not part of the on-disk writer surface — it picks
  per-element widths greedily, which exercises every decode
  path but isn't necessarily byte-identical to what Pixar's
  writer would produce.
- **13 new unit tests** in `usdc::tests` cover the empty
  input, all-zero-delta packing, multi-element LSB-first
  control-byte layout, single `i8` / `i16` / `i32` elements,
  the "absolute resets prev" rule, negative-delta wrapping, a
  five-element sequence forcing two control bytes, a
  monotonic-token-index pattern mirroring the trace's
  decoded FIELDS name-index opening, and four error paths
  (short control stream, short `i8` / `i16` / `i32` payload).

### Added — Round 15 (USDC binary-format primitives: bootstrap + TOC)

- **`usdc` module** — bootstrap-header and Table-of-Contents
  primitives for the binary `.usdc` sibling of the USDA text
  format. Public surface: `Magic` (8-byte `PXR-USDC` signature),
  `Version` (`major.minor.patch` from bootstrap bytes
  `0x08`/`0x09`/`0x0A`, with reserved-bytes-zero validation),
  `Bootstrap` (the fixed 88-byte header including the absolute
  `toc_offset`), `SectionName` (enum over the six standard
  section names `TOKENS` / `STRINGS` / `FIELDS` / `FIELDSETS` /
  `PATHS` / `SPECS` observed in real samples), `TocEntry`
  (`(name, offset, size)` row in the tail TOC), `Toc::parse`
  (walks the TOC and validates that each declared section region
  lives inside the file, doesn't overlap the bootstrap header,
  and doesn't run into the TOC itself), and a one-call
  `UsdcFile::parse` that returns both bootstrap and TOC.
  Sourced exclusively from
  `docs/3d/usd/usdc-crate-format-trace.md`, the project's own
  clean-room byte-level trace of real `.usdc` samples.
- **Decoder boundary check.** A `.usdc` Default Layer now has
  its bootstrap + TOC validated before the decoder surfaces
  `Error::Unsupported` — malformed `.usdc` payloads (truncated
  bootstrap, wrong magic, oversized `sectionCount`, section
  region running into the TOC, …) are caught at the container
  boundary as `Error::InvalidData` instead of leaking past
  layer-extension dispatch. The `Unsupported` message now also
  records the parsed version and section catalogue so callers
  can see how far the boundary check got.
- **Real-fixture cross-check.** `tests/usdc_unsupported.rs`'s
  new `binary_usdc_real_fixture_reports_trace_doc_facts` test
  parses the trace doc's Elephant fixture
  (`docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc`) and
  asserts the every-byte facts: version 0.8.0, six sections in
  the documented order, TOC at file offset `0x000cfc9a`, section
  sizes matching the trace doc's decoded table
  (`TOKENS=1770`, `STRINGS=8`, `FIELDS=998`, `FIELDSETS=611`,
  `PATHS=548`, `SPECS=331`).

### Added — Round 194 (variantSet-bound Material `usdcat`-flatten oracle)

- **`tests/variant_material_usdcat_oracle.rs`** — black-box cross-validation
  of the round-7 variantSet resolver against Pixar's `usdcat -f` flatten
  command on a feature surface combining variant resolution with the
  round-1 `UsdPreviewSurface` PBR mapping. A `def Material` declared
  inside a variantSet body (the "metallic" branch of a `finish` variant
  set) has to be materialised by the resolver, then the typed Scene3D
  pass has to pick up `inputs:diffuseColor` / `inputs:metallic` /
  `inputs:roughness` and bind the Mesh's `rel material:binding` to the
  resulting `MaterialId`. The test feeds the original USDA + the
  `usdcat -f`-flattened reference through `UsdzDecoder` (each wrapped in
  USDZ via the in-tree `common::build_usdz` helper) and asserts
  fingerprint parity: mesh count, primitive vertex count, material count,
  bound `base_color` / `metallic` / `roughness`. A second test flips the
  selection metadata to `"matte"` and verifies the bound material's PBR
  slots track the alternate variant body. Skip-early (NOT `#[ignore]`)
  when `usdcat` is absent — no `pxr` Python module is needed, so the
  oracle runs anywhere Apple USD Tools / Pixar's CLI is installed.

## [0.0.2](https://github.com/OxideAV/oxideav-usdz/compare/v0.0.1...v0.0.2) - 2026-05-29

### Other

- Round 14: CRC-32 integrity verification on the USDZ reader
- Round 13: in-layer inherits / specializes class-arc composition
- Round 12: defaultPrim consistency on the USDA writer
- round 11 — in-archive references / payload arc composition
- Round 10: anchored in-archive sublayer composition
- Round 9: composition-arc + layer-metadata round-trip on the writer
- Round 8: variant writer round-trip
- Round 7: variant selection composition

### Added — Round 14 (CRC-32 integrity verification on the reader)

- **STORED payloads are verified against the central-directory
  CRC-32.** The ZIP walker now reads the CRC-32 field each
  central-directory file header records (offset +16) and recomputes
  the CRC-32/ISO-HDLC of the stored payload, rejecting any mismatch
  with `Error::InvalidData` (`"USDZ entry '<name>' failed CRC-32
  check (stored 0x…, computed 0x…)"`). Until now the writer computed
  and emitted a correct CRC for every entry but the reader never
  checked it, so a single corrupted byte (bit-rot, a truncated copy,
  a mismatched STORED pass-through) would surface as a baffling USDA
  parse failure or a garbled texture rather than a clear
  container-integrity error. The check reuses the crate's own
  CRC-32 routine (`zip_writer::crc32`) so reader and writer can never
  drift, and the zero-length empty-file case (`crc32(b"") == 0`)
  verifies naturally. This is plain PKWARE-format structure — the
  CRC field is not USD-specific.
- **`zip::walk` unit tests** — `rejects_crc_mismatch` (a central
  directory advertising the wrong CRC for an otherwise-valid payload
  is rejected) and `accepts_empty_payload_crc` (a zero-length entry's
  all-zero CRC field passes). `tests/cube_usdz.rs::corrupted_payload_is_rejected_by_crc`
  flips a payload byte of a valid archive and asserts the public
  `UsdzDecoder` surfaces the CRC failure rather than parsing garbage.
- **Stale `spec_usdz.html` source citations removed.** The
  project-shipped `spec_usdz.html` was purged from `docs/3d/usd/` on
  2026-05-24 (clean-room policy — see `docs/3d/usd/README.md`). The
  five lingering `docs/3d/usd/spec_usdz.html` citations in `zip.rs`,
  `zip_writer.rs`, and `usd_to_scene.rs` are re-grounded to the
  surviving `docs/3d/usd/GAP-TRACKER.md` §3 (USDZ container) and to
  the neutral PKWARE-format / USD relative-asset-path descriptions;
  no behaviour change.

### Added — Round 13 (in-layer `inherits` / `specializes` composition)

- **In-layer class-hierarchy arc composition.** A prim carrying
  `inherits = </Class/Foo>` / `specializes = </Class/Foo>` (or a
  list of such paths) now has the targeted in-layer prim composed
  underneath it per the OpenUSD glossary's Inherits / Specializes
  definitions. The class-arc target is a prim path WITHIN the same
  layer (as distinct from `references` / `payload` which target
  external assets), so the composer walks the local layer's prim
  tree rather than opening another archive entry. The targeted
  prim's `type_name`, `kind`, children, and attrs ride up under the
  inheriting / specialising prim via the same `merge_prim_under`
  primitive that powers rounds 10 / 11 — local opinions win, then
  earlier list entries win over later ones (USD list order =
  strength order).
- **LIVRPS-aligned strength order.** Per the OpenUSD glossary
  LIVRPS ordering — L > I > V > R > P > S — `inherits` is composed
  BEFORE `references` / `payload` so a referenced opinion can never
  overwrite an inherited one; `specializes` is composed AFTER all
  other arcs so it can only fill in opinions that every stronger
  arc has left unsourced. Confirmed by paired test fixtures: a prim
  with `inherits` + `references` resolves to the inherited child;
  the same shape with `specializes` + `references` resolves to the
  referenced child.
- **Transitive composition.** A class prim that itself carries
  `inherits` / `specializes` opinions has its own arc composed
  before merging upward. So `</A> inherits </B>; Prop inherits </A>`
  results in Prop carrying every child contributed by `</B>` (per
  the glossary's "implied" inherited opinion chain).
- **Cycle break.** A `(prim-path)` visited set keyed on the
  normalised path scopes per-recursion so a class-arc cycle (`</A>`
  inherits `</B>` which inherits `</A>` again) terminates without
  looping. The cycle break still flips the consumed flag so the
  writer doesn't re-author an arc that resolves into a cycle.
- **Flatten-on-write + round-trip preservation.** A successfully-
  composed in-layer arc is stripped from the prim's metadata so the
  writer flattens the composed tree into a single layer, matching
  the round 10 / 11 sublayer + reference flatten-on-write policy.
  An arc whose target prim path doesn't resolve in the local layer
  stays authored so the writer round-trips the unresolved opinion
  verbatim (matching the external-/cross-package round-trip path
  for `references`). The audit trail of composed arcs lands on
  `Scene3D::extras["usd:composedInherits"]` and
  `Scene3D::extras["usd:composedSpecializes"]` as
  `"<prim-path>|inherits"` / `"<prim-path>|specializes"` entries.
- **`tests/class_hierarchy_composition.rs`** — 10-case suite:
  basic class inheritance pulls children up + audit stamp;
  local-child-wins-over-inherited; multi-target `inherits = [</A>,
  </B>]` strength order; inherits-beats-references; specializes-
  loses-to-references; transitive `</A>` → `</B>` chain; cycle
  termination; external (unresolvable) target round-trips; consumed
  arc flattens away on the writer; non-`Path` opinion value (a raw
  string for `inherits`) doesn't crash the composer.

### Fixed — Round 12 (`defaultPrim` consistency on the writer)

- **`defaultPrim` token tracks prim-name sanitisation.** The USDA
  writer sanitises prim names to the `[A-Za-z_][A-Za-z0-9_]*` USD
  grammar (e.g. `"My Cube"` → `def Xform "My_Cube"`), but the
  `defaultPrim` opinion preserved through `usd:layerMetadata` was
  emitted verbatim. A downstream resolver looking up the
  `defaultPrim` token then missed the only prim in the layer,
  silently breaking `references = @./scene.usda@` (selector-less)
  composition against our output. The writer now rewrites the token
  to the sanitised spelling when the raw form names a real root.
- **Dangling `defaultPrim` opinions are dropped.** When the
  preserved `defaultPrim` names a prim that no longer exists in the
  scene (downstream pipeline pruned it), the writer used to re-emit
  the dangling token. Strict USD validators reject layers whose
  `defaultPrim` resolves to nothing — we now drop the opinion (and
  fall back to synthesising one from the first surviving root, if
  any).
- **Missing `defaultPrim` is synthesised from the first root.** A
  freshly-constructed `Scene3D` (no `usd:layerMetadata`) emitted no
  `defaultPrim` at all, so cross-archive `references = @./scene.usda@`
  resolution against our output produced nothing. The writer now
  synthesises a `defaultPrim` opinion naming the first root's
  sanitised prim name whenever no preserved one survives. A
  genuinely rootless scene still emits no opinion (USD's `defaultPrim`
  is optional, and we don't invent a target out of thin air).

### Added — Round 11 (references / payload arc composition)

- **In-archive `references` / `payload` arc composition.** A prim
  carrying `references = @asset.usd@` / `payload = @asset.usd@`
  (optionally with a `</Prim>` selector) that targets an in-archive
  `.usd` / `.usda` layer now has the targeted prim composed
  underneath it per the OpenUSD glossary's References definition.
  The referencing prim keeps its own name — the "prim name-change"
  the glossary's `MarbleCollection` example describes — while the
  target's `type_name`, `kind`, children, and attrs ride up. A bare
  `references = @file@` targets the referenced layer's `defaultPrim`;
  `@file@</Prim>` targets the named prim (descending nested paths).
  Rounds 1..10 ignored both arcs on the read path: a prim authored
  purely as `def "Inst" ( references = @Marble.usd@ )` produced an
  empty node.
- **LIVRPS-aligned strength order.** Local opinions beat referenced
  opinions (`merge_prim_under`'s local-wins merge); `references`
  beats `payload` ("Payloads are weaker than references"); within a
  `[@a@, @b@]` list the earlier entry is stronger (USD list order =
  strength order). Composed strongest-first into the local prim so
  every weaker arc only fills gaps.
- **Transitive + cyclic composition.** A referenced layer that
  itself references composes transitively (its sub-references resolve
  against *its own* archive location). A `(layer-path, in-asset
  prim-path)` visited set breaks reference cycles so decode always
  terminates.
- **Flatten-on-write + round-trip preservation.** Consumed
  in-archive arcs are stripped from prim metadata so the writer
  flattens the composed tree into a single layer (matching round
  10's sublayer flatten-on-write policy). External / cross-package
  (`/abs`, `://`, `[...]`) and non-`.usd[a]` targets stay authored so
  the writer round-trips the unresolved opinion verbatim; a partly
  external list keeps the whole opinion. `.usdc` targets raise
  `Error::Unsupported` (binary-Crate docs gap). The audit trail of
  composed arcs lands on
  `Scene3D::extras["usd:composedReferences"]` as
  `"<layer>|references"` / `"<layer>|payload"` entries.
- **`tests/reference_composition.rs`** — 10 tests modelled on the
  glossary's `MarbleCollection` worked example plus boundary cases
  (defaultPrim targeting, `</Prim>` selector, references-beat-payload,
  list strength order, transitive nesting, external round-trip,
  `.usdc` rejection, cycle termination, USDZ→Scene3D→USDZ flatten).

### Added — Round 10 (anchored sublayer composition)

- **In-archive sublayer composition.** When the Default Layer
  declares `subLayers = [@./geom.usda@, ...]` and the named
  entries exist in the surrounding USDZ archive, the decoder
  composes their prim trees underneath the local layer's per the
  OpenUSD glossary's LayerStack definition: "the recursive
  gathering of all SubLayers of a Layer, plus the layer itself as
  first and strongest". Sublayers are walked depth-first,
  strength-ordered (first entry strongest), with a cycle-break on
  self-reference. The audit trail of which sublayers were
  composed in lands on
  `Scene3D::extras["usd:composedSubLayers"]` (declaration order,
  depth-first innermost first). Rounds 1..9 ignored the
  `subLayers` opinion entirely on the read path — every prim
  authored in a sublayer was invisible to the `Scene3D`
  translation.
- **Anchored asset-path resolution.** Sublayer entries are
  resolved against the authoring layer's archive directory per
  `spec_usdz.html` §USD Constraints — `./foo.usda` and `foo.usda`
  both anchor to the parent layer's folder; absolute archive
  paths fall back to the archive root if anchored resolution
  misses. Paths containing `://`, `[` (cross-package), or
  starting with `/` are treated as external and silently skipped
  from composition (kept on `scene.extras` for writer
  round-trip).
- **LIVRPS-aligned prim merge.** When local + sublayer both
  declare a prim by the same name, the local opinion wins for
  every metadata key, attr key, variantSet, and same-named child.
  Children present only in the sublayer are appended after local
  children. Empty `type_name` on the local prim inherits from the
  sublayer. The specifier upgrades from `over` / `class` to `def`
  when the sublayer is the defining layer — the prim IS defined
  (by the sublayer); the local layer is overriding opinions.
- **`.usdc` sublayer surfaces as `Error::Unsupported`.** The
  binary Crate parser is still gated on a docs-collaborator trace
  doc; a sublayer with the `.usdc` extension errors out
  consistently with the round-1 Default-Layer policy rather than
  silently dropping the layer.

### Tests

- `tests/sublayer_composition.rs` — 9-case integration suite:
  * `sublayer_def_mesh_lands_in_scene` — basic
    `@./geom.usda@` composition of a `def Mesh`.
  * `local_layer_opinion_wins_over_sublayer` — local 3-vertex Mesh
    beats sublayer 4-vertex Mesh on same prim name.
  * `sublayer_fills_in_missing_def_prims` — local declares empty
    Xform, sublayer contributes two Meshes that surface.
  * `earlier_sublayer_beats_later` — `subLayers = [@a@, @b@]`,
    `a`'s opinions win.
  * `nested_sublayers_compose_recursively` — `a → b → c` chain
    composes the leaf-layer Mesh up to the root.
  * `cyclic_sublayer_does_not_loop` — `a` sublayers itself; we
    don't recurse forever.
  * `external_sublayer_path_is_silently_skipped` — unresolvable
    path keeps the opinion on extras, doesn't error.
  * `usdc_sublayer_surfaces_as_unsupported` — `.usdc` entry
    errors per the docs-gap policy.
  * `over_in_local_upgrades_to_def_from_sublayer` — `over` +
    `def` merge to `def`; the Mesh surfaces.
- Existing 121 tests across 21 integration files still green.

### Docs gap (carried forward from round 9)

- **UsdSkel + UsdGeomSubset still blocked.** Per-schema HTML for
  the `UsdGeom` / `UsdSkel` / `UsdShade` / `UsdPhysics` schema
  families lives behind a different URL pattern (`api_*.html`) on
  openusd.org and isn't staged in the workspace yet.
- **Cross-package composition arcs still single-archive.** Round
  10 composes intra-archive sublayers but does not open
  cross-package `@foo.usdz[path/within.usd]@` references; the
  `ArResolver` syntax mentioned in `spec_usdz.html` §USD
  Constraints stays as a side-channel opinion on
  `scene.extras["usd:layerMetadata"]`.
- **References / payloads composition not yet wired.** Round 10
  walks the `subLayers` arc; the matching engine for
  `references` / `payload` arcs (which target a *single* prim
  inside the named asset rather than the whole layer) is deferred
  pending its own round.
- **Sublayer writer round-trip.** Round 10 evaluates sublayers on
  the read path; the encoder flattens the composed tree back into
  a single layer rather than preserving the multi-file
  LayerStack structure on output.

### Added — Round 9 (composition-arc + layer-metadata round-trip)

- **Layer-metadata round-trip.** The writer now re-emits every
  layer-level `( ... )` metadata key — `defaultPrim`, `subLayers`,
  `customLayerData`, and any unknown / Apple-vendor key — in
  addition to the round-1 `upAxis` + `metersPerUnit` pair. The
  decoder stashes the source layer's full metadata block on
  `Scene3D::extras["usd:layerMetadata"]` using the lossless tagged
  shape from `crate::variant_codec`, so a USDZ → `Scene3D` → USDZ
  round trip preserves each value's USDA type-token discriminant
  (`Token` vs `String` vs `Asset` vs `AssetWithPath` vs `Path`).
  Round 1..8 dropped them silently — every authored `defaultPrim =
  "Root"` re-emitted with only `upAxis` + `metersPerUnit` left.
- **Prim composition-arc metadata round-trip.** Mirror of the
  layer-metadata path on `Node::extras["usd:primMetadata"]`. The
  writer now re-emits every prim-level `( ... )` metadata key —
  `kind`, `active`, `apiSchemas`, `references`, `payload`,
  `inherits`, `specializes`, custom keys — using the LIVRPS
  `prepend` list-edit prefix for the composition-arc keys
  (`references` / `payload` / `inherits` / `specializes` /
  `apiSchemas` / `variantSets`) per OpenUSD authoring convention.
  `references = @file.usd@</Prim>` round-trips with both the asset
  path and the `</Prim>` selector intact via `Value::AssetWithPath`.
- **`format_metadata_value` writer helper.** Inverse of
  `crate::usda::parse_value` — covers every `Value` variant the
  decoder produces from a layer- or prim-metadata block. Used by
  both round-9 round-trip paths plus open to future encoder needs
  for inline value serialisation.

### Tests

- `tests/composition_roundtrip.rs` — 8-case suite covering each
  composition-arc / layer-metadata shape end-to-end (build USDZ
  → decode → write → re-parse → assert): `defaultPrim`,
  `subLayers = [@a@, @b@]`, `prepend references = @./asset@</P>`
  (AssetWithPath form), `prepend payload = @./payload@`, prim
  `kind = "component"`, `customLayerData = { string version =
  "1.0" }`, `prepend apiSchemas = [...]` list-edit form, and an
  unknown vendor-custom layer-metadata key (`customField`).
- Existing 100+ tests across 20 integration files still green;
  the new tagged extras blob is additive (every prior round's
  untagged `usd:metadata` / per-key `usd:<key>` entries remain).

### Docs gap (carried forward from round 8)

- **UsdSkel + UsdGeomSubset still blocked.** `docs/3d/usd/README.md`
  documents that the per-schema HTML for `UsdGeom` / `UsdSkel` /
  `UsdShade` / `UsdPhysics` schema families lives behind a different
  URL pattern (`api_*.html`) on openusd.org and isn't staged in the
  workspace yet. The round-9 brief asked for `UsdGeomSubset` first;
  glossary.html doesn't define it (only TOC links into the
  unfetched `api_usd_geom_subset.html`), so the work fell back to
  the composition-arc push instead.
- **Cross-file composition arcs still single-layer.** Round 9
  surfaces references / payloads / sublayers as round-trip metadata,
  but the Scene3D translator still doesn't open the targeted
  `@file.usd@` and merge its contents — every cross-file arc stays
  as a side-channel opinion on `extras`. Cross-file resolution
  requires per-archive asset traversal that round 9 intentionally
  defers.
- **`NodeGraphNodeAPI` / `UsdShadeNodeGraph` composition** — the
  staged glossary only mentions it in TOC links to the unfetched
  per-schema HTML. Same staging gap as UsdGeomSubset.

### Added — Round 8 (variant writer round-trip)

- **VariantSet writer.** The encoder now re-emits `variantSet "name"
  = { "variant" ( meta ) { body } ... }` blocks captured from the
  source layer, plus the `variants = { string SET = "VAR" ... }`
  selection metadata and the `prepend variantSets = [...]` list, so
  a USDZ → USDZ round trip preserves every variant body, including
  the *unselected* branches that round 7 left invisible to the
  writer (round 7 evaluated the selected variant and flattened it
  into the resolved tree, dropping the others).
- **`crate::variant_codec` module.** Defines the JSON shape used
  for the `Node::extras["usd:variantSets"]` round-trip stash —
  every `Variant`'s `metadata` / `attrs` / recursive prim
  `children` are encoded with full type discriminants so the
  writer can reconstruct the original USDA spelling (`Token` vs
  `String` vs `Asset` etc. all round-trip distinctly). `Prim`
  trees inside a variant body are encoded recursively, including
  nested `variant_sets` declarations.
- **Decoder side: stash on Node extras.** `usd_to_scene::stash_extras`
  now also encodes the prim's `variant_sets` block via
  `variant_codec::encode_variant_sets()` and writes it under the
  `usd:variantSets` extras key on the synthesised `Node`.

### Tests

- `tests/variant_writer.rs` — 4-case suite: the full
  decode→encode→re-decode→re-parse loop on the glossary's classic
  three-variant `simpleVariantSet.usd` shape (asserts every variant
  body survives + selection metadata picks the same Capsule on
  re-decode); per-variant attribute round-trip; bare `def Xform`
  without variants emits no `(...)` metadata block (preserves r1/r2
  output shape); two distinct variantSets on the same prim survive
  + their selections still resolve on re-decode.

### Docs gap

- **UsdSkel + UsdGeomSubset still blocked.** `docs/3d/usd/README.md`
  documents that the per-schema HTML for the `UsdGeom` / `UsdSkel`
  / `UsdShade` schema families lives behind a different URL pattern
  (`api_*.html`) on openusd.org and isn't staged in the workspace
  yet. Both candidate features (skeletal animation, per-face
  material subsets) wait on that staging round.
- **Composition arcs (sublayers / references / payloads pulling in
  *external* USD files) still single-layer only.** Round 8 closes
  the variant-selection round-trip gap; cross-file composition
  remains a future round.

### Added — Round 7 (variant selection composition)

- **Variant selection composition.** The Scene3D translator now
  evaluates `metadata["variants"] = { string SET = "VAR" }`
  against `Prim::variant_sets` before walking the prim tree. The
  matching variant's `attrs` and `children` are merged into the
  prim per LIVRPS strength order — Local opinions beat the
  variant's. The headline behaviour: a `def Mesh` declared inside
  a selected variant materialises as a Scene3D `Mesh` + `Node`
  just like a directly-authored child would. The composition is
  recursive — variant selections deep in the prim tree are
  resolved on the same pass.
- **`Prim::resolved_variants()` helper.** Public API on
  [`crate::usda::Prim`] that returns the composed view of a single
  prim. Tests + Scene3D translator both use it. Variant
  `references = @other.usd@` opinions inside a selected variant
  are NOT followed (round 7 is single-layer evaluation only) —
  they're recorded on the resolved prim's metadata under
  `usd:variantReferences:<set>:<var>:references` so the extras
  layer surfaces the unresolved arc to the caller. Variant
  `metadata` keys other than `references` / `payload` lose to
  local opinions per LIVRPS.

### Tests

- `tests/variant_evaluation.rs` — 11-case suite covering: the
  glossary `simpleVariantSet.usd` shape (single variantSet, one
  variant selected, child `def Xform` materialises); changing the
  selection picks a different child; no-selection metadata leaves
  the variant invisible; selecting a non-existent variant name is
  silently tolerated (per "selections are not required"); local
  child wins over variant child sharing the same name; multiple
  variantSets compose independently; variant-authored `def Mesh`
  pulls through into a Scene3D mesh with positions; deeply-nested
  variantSet (on a non-root prim) is composed; variant
  `references = @...@` opinions land in the
  `usd:variantReferences:<set>:<var>:references` side channel;
  variant attribute opinion loses to local; `variant_sets` are
  preserved on the resolved prim for future writer round-trip.

### Docs gap

- **UsdSkel + UsdGeomSubset blocked.** `docs/3d/usd/README.md`
  documents that the per-schema HTML for the `UsdGeom` /
  `UsdSkel` / `UsdShade` schema families lives behind a different
  URL pattern (`api_*.html`) on openusd.org and isn't staged in
  the workspace yet. Both candidate features (skeletal animation,
  per-face material subsets) wait on that staging round.
- **Round 7 is read-only on the writer.** Composition fires on
  the decode path; the encoder doesn't yet round-trip
  `variantSet` blocks from the source layer's structured form
  (the round 6 changelog entry's "round-trip writer re-emits the
  block verbatim" claim was aspirational and remains a gap).
  After a `Scene3D` round-trip the variant content is flattened
  into the resolved tree — selection metadata is lost, the
  `variantSet` block isn't re-emitted.

## [0.0.1](https://github.com/OxideAV/oxideav-usdz/compare/v0.0.0...v0.0.1) - 2026-05-10

### Other

- Round 6: variantSet structured parsing + line/column errors
- USDA parser: accept Apple-emitted layer-metadata syntax
- Round 5: topology dispatch + no_fold flag + per-Mesh transform
- Round 4: UsdMediaSpatialAudio reader + writer
- Round 3: multi-primitive Mesh emission + sibling-fold decode
- Round 3: per-node transform xformOp serialisation
- Round 2: USDZ encoder + USDZ → USDZ pass-through optimisation

### Added — Round 6 (variantSet parsing + line/column errors)

- **VariantSet structured parser.** `variantSet "name" = {
  "variantA" ( meta ) { body } "variantB" { body } }` blocks
  inside a prim body now parse into a structured
  `Prim::variant_sets: BTreeMap<String, BTreeMap<String, Variant>>`
  rather than landing on the `unexpected character '{' at offset N`
  error path. Each `Variant` carries its own `metadata`, `attrs`
  and `children`, mirroring [`Prim`](crate::usda::Prim)'s shape so
  a future composition engine can switch on the prim's
  `variants = { string name = "..." }` selection metadata. Coverage
  matches OpenUSD glossary's `simpleVariantSet.usd` and
  `referenceVariantSet` examples — variants with composition-arc
  metadata (`prepend references = @...@`) round-trip too. Empty
  `variantSet "foo" = {}` blocks (Pixar's tooling emits these when
  a variant is removed but the declaration stub remains) parse to
  an empty inner map. The Scene3D translator continues to ignore
  variant_sets — round 6 is parser-side capture only; semantic
  composition deferred to a later round.
- **Line + column in parser error messages.** Every `at offset N`
  diagnostic in `usda.rs` now renders as `line L:C (offset N)`
  using a `Tokenizer::pos_display()` helper. The original byte
  offset stays in the message so callers comparing exact
  `at offset` substrings keep working; new consumers get an
  actionable line/column for hand-authored USDA debugging. Affects
  the identifier / attribute-name / numeric-literal / value-dispatch
  / prim-spec / prim-name / prim-body / type-token error paths
  (10 sites total).

### Tests

- `tests/apple_style_usda.rs` gains four cases:
  `variant_set_parses_into_structured_form` (the canonical
  glossary example), `variant_set_with_per_variant_metadata`
  (per-variant `prepend references = @...@`),
  `empty_variant_set_block` (the stripped-stub case),
  `parse_error_reports_line_and_column` (locks the new error
  message shape).

### Docs gap

- **USDC binary "Crate" format.** OpenUSD publishes no prose spec
  for the wire format — `docs/3d/usd/README.md` documents this
  explicitly. The C++ source (`pxr/usd/usd/crateFile.{h,cpp}`)
  is barred under the workspace's no-external-libs rule.
  Loading a `.usdc` Default Layer continues to surface as
  `Error::Unsupported` with a `usdcat -o foo.usda` hint.

### Added — Apple oracle parser unblock (CI run 25631625721)

- **Typed-dictionary value literals** (`Value::Dict`). The USDA
  value parser now recognises `{ TYPE NAME = VALUE; ... }` blocks
  in metadata position, which Apple's `usdzconvert` emits as
  `customLayerData = { string apple_metadata = "..." }`. Each
  entry's value is parsed recursively (so nested `dictionary X = {
  ... }` works); the type token is consumed but discarded since
  Round 1's Scene3D mapping doesn't act on layer-custom data.
  Previously the parser failed with `unexpected character '{' at
  offset 34 where value expected` on every Apple-emitted USDZ.
- **Composition-arc target with prim selector** (`Value::AssetWithPath`).
  Asset paths immediately followed by a `</Prim>` selector — the
  shape Apple uses for `references = @file.usd@</Prim>` and
  `payload = @file.usd@</Prim>` — now parse into a structured
  variant rather than erroring on the unexpected `<` after the
  closing `@`. Plain `@file@` references still parse as
  `Value::Asset`.
- **Sized vector type suffix** (`double[3]`, `int[2]`). The
  attribute-prefix parser previously only recognised `[]` for
  unbounded arrays; sized suffixes (used by Apple inside dict
  literals for vector metadata) now parse as part of the type
  token without erroring at the `[`.

### Tests

- `tests/apple_style_usda.rs` — 11-case regression suite mirroring
  the smallest viable Apple-shaped USDA snippets: typed-dict
  metadata, references with prim selectors, sub-layer arrays,
  per-attribute `colorSpace` blocks, prim `kind`/`active`
  metadata, `over`/`class` specs, Pixar's same-line-or-next-line
  brace layout, triple-quoted `doc` metadata.
- `tests/apple_oracle_local.rs` — opportunistic smoke test that
  builds an Apple-style USDZ via the system `usdcat` + Pixar's
  `pxr.UsdUtils.CreateNewUsdzPackage`, then decodes it through
  our `UsdzDecoder`. Skips with an `eprintln!` notice when either
  binary is absent (per workspace rule "no `#[ignore]`").

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
