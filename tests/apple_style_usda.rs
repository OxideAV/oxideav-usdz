//! Parser regression tests against Apple `usdzconvert`-style USDA.
//!
//! The CI job `usdz-oracle` runs Apple's converter against our glTF
//! output and feeds the resulting USDA back through our reader. The
//! shapes Apple emits go beyond the round-1 scope sketched in
//! `usda.rs`'s module docs:
//!
//! * Layer metadata holds typed-dictionary literals
//!   (`customLayerData = { string foo = "bar" }`).
//! * Composition arcs (`references`, `payload`) stack an asset path
//!   and an in-asset prim selector (`@file@</Prim>`).
//! * Sub-layer arrays mix asset paths.
//! * Per-attribute metadata blocks carry colour-space / kind /
//!   active flags.
//!
//! Each test below exercises one of those shapes in the smallest
//! self-contained USDA snippet we can hand-author. The contract is
//! "parse-and-discard": the parser must accept the input without
//! erroring, but Round 1 doesn't have to *do* anything with the
//! captured metadata. A future round can plug the extracted values
//! back into `Scene3D::user_extras`.

use oxideav_usdz::usda::{parse, Value};

#[test]
fn layer_metadata_typed_dict_literal() {
    // Mirrors the exact byte layout Apple usdzconvert produced in
    // CI run 25631625721 — `{` lands at offset 34 inside
    // `customLayerData = {` which round 1's parser previously
    // rejected with `unexpected character '{' at offset 34 where
    // value expected`.
    let src = br#"#usda 1.0
(
    customLayerData = {
        string apple_metadata = "value"
        dictionary nested = {
            int count = 3
        }
        double[3] vec = (0, 1, 2)
    }
    defaultPrim = "Asset"
    upAxis = "Y"
)

def Xform "Asset" {
    def Mesh "M" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    }
}
"#;
    let layer = parse(src).expect("parse Apple-style layer metadata");
    // `customLayerData` is a Dict with three top-level entries.
    let custom = layer
        .metadata
        .get("customLayerData")
        .expect("customLayerData survives parse");
    match custom {
        Value::Dict(map) => {
            assert_eq!(map.len(), 3);
            assert!(map.contains_key("apple_metadata"));
            assert!(map.contains_key("nested"));
            assert!(map.contains_key("vec"));
        }
        other => panic!("expected Dict, got {other:?}"),
    }
    // Adjacent layer metadata still parses normally.
    assert_eq!(
        layer.metadata.get("upAxis").and_then(|v| v.as_text()),
        Some("Y")
    );
    assert_eq!(
        layer.metadata.get("defaultPrim").and_then(|v| v.as_text()),
        Some("Asset")
    );
    // Body prims still landed.
    assert_eq!(layer.prims.len(), 1);
    let asset = &layer.prims[0];
    assert_eq!(asset.type_name, "Xform");
    assert_eq!(asset.children.len(), 1);
}

#[test]
fn empty_dict_literal() {
    // Pixar's serialiser emits `customLayerData = {}` when the input
    // had a key but no entries. Verify the empty form parses.
    let src = b"#usda 1.0\n(\n    customLayerData = {}\n    upAxis = \"Y\"\n)\n";
    let layer = parse(src).expect("parse empty dict literal");
    match layer.metadata.get("customLayerData") {
        Some(Value::Dict(map)) => assert!(map.is_empty()),
        other => panic!("expected Dict, got {other:?}"),
    }
}

#[test]
fn references_with_prim_selector() {
    // `references = @file.usd@</Prim>` is the canonical USD
    // composition-arc syntax. The asset path and the prim-path
    // selector must both round-trip into `AssetWithPath`.
    let src = br#"#usda 1.0
def Xform "Root" (
    prepend references = @./asset.usd@</Asset>
) {
}
"#;
    let layer = parse(src).expect("parse references arc");
    let root = &layer.prims[0];
    let refs = root.metadata.get("references").expect("references key");
    match refs {
        Value::AssetWithPath { asset, prim_path } => {
            assert_eq!(asset, "./asset.usd");
            assert_eq!(prim_path, "/Asset");
        }
        other => panic!("expected AssetWithPath, got {other:?}"),
    }
}

#[test]
fn payload_with_prim_selector() {
    // `payload` is the deferred-loading sibling of `references` and
    // shares the same `@asset@</prim>` shape.
    let src = br#"#usda 1.0
def Xform "Root" (
    prepend payload = @./payload.usd@</Inner>
) {
}
"#;
    let layer = parse(src).expect("parse payload arc");
    let root = &layer.prims[0];
    let p = root.metadata.get("payload").expect("payload key");
    match p {
        Value::AssetWithPath { asset, prim_path } => {
            assert_eq!(asset, "./payload.usd");
            assert_eq!(prim_path, "/Inner");
        }
        other => panic!("expected AssetWithPath, got {other:?}"),
    }
}

#[test]
fn asset_only_reference() {
    // Reference without a `<...>` selector — the whole layer is
    // pulled in. Stays as plain `Asset`.
    let src = br#"#usda 1.0
def Xform "Root" (
    prepend references = @./whole.usd@
) {
}
"#;
    let layer = parse(src).expect("parse asset-only reference");
    let root = &layer.prims[0];
    let refs = root.metadata.get("references").expect("references key");
    match refs {
        Value::Asset(s) => assert_eq!(s, "./whole.usd"),
        other => panic!("expected Asset, got {other:?}"),
    }
}

#[test]
fn sublayers_array() {
    // `subLayers = [@a.usd@, @b.usd@]` — already covered by the
    // generic array parser, but lock it down with an explicit test
    // since Apple emits this in some pipelines.
    let src = br#"#usda 1.0
(
    subLayers = [
        @./layer1.usd@,
        @./layer2.usd@
    ]
)
"#;
    let layer = parse(src).expect("parse subLayers array");
    let subs = layer.metadata.get("subLayers").expect("subLayers key");
    let seq = subs.as_seq().expect("array seq");
    assert_eq!(seq.len(), 2);
    assert_eq!(seq[0].as_text(), Some("./layer1.usd"));
    assert_eq!(seq[1].as_text(), Some("./layer2.usd"));
}

#[test]
fn per_attribute_metadata_with_colorspace() {
    // Apple emits `colorSpace = "raw"` per-attribute metadata on
    // texture inputs. The trailing `( ... )` must parse.
    let src = br#"#usda 1.0
def Shader "S" {
    asset inputs:file = @./tex.png@ (
        colorSpace = "sRGB"
    )
}
"#;
    let layer = parse(src).expect("parse per-attribute metadata");
    let s = &layer.prims[0];
    let attr = s.attrs.get("inputs:file").expect("inputs:file");
    assert_eq!(attr.metadata.len(), 1);
    assert_eq!(
        attr.metadata.get("colorSpace").and_then(|v| v.as_text()),
        Some("sRGB")
    );
}

#[test]
fn prim_metadata_kind_and_active() {
    // `kind = "component"` + `active = false` are common Apple
    // outputs in prim metadata position.
    let src = br#"#usda 1.0
def Xform "Root" (
    kind = "component"
    active = true
) {
}
"#;
    let layer = parse(src).expect("parse prim metadata");
    let root = &layer.prims[0];
    assert_eq!(
        root.metadata.get("kind").and_then(|v| v.as_text()),
        Some("component")
    );
    match root.metadata.get("active") {
        Some(Value::Bool(true)) => {}
        other => panic!("expected Bool(true), got {other:?}"),
    }
}

#[test]
fn over_and_class_specs() {
    // `over` and `class` are valid prim specifiers alongside `def`.
    // Already covered by the spec-token list, but lock them down
    // explicitly so a future regression on the keyword set fails
    // loudly.
    let src = br#"#usda 1.0
class "Template" {
}

over "Existing" {
}

def Xform "Real" {
}
"#;
    let layer = parse(src).expect("parse over+class specs");
    assert_eq!(layer.prims.len(), 3);
    assert_eq!(layer.prims[0].spec, "class");
    assert_eq!(layer.prims[1].spec, "over");
    assert_eq!(layer.prims[2].spec, "def");
}

#[test]
fn pixar_serialiser_brace_layout() {
    // Pixar's serialiser emits `def Xform "Asset"\n{` — the opening
    // brace lands on its own line. `skip_trivia` already eats the
    // newline, but verify behaviourally.
    let src = b"#usda 1.0\ndef Xform \"Asset\"\n{\n}\n";
    let layer = parse(src).expect("parse Pixar brace layout");
    assert_eq!(layer.prims.len(), 1);
    assert_eq!(layer.prims[0].name, "Asset");
}

#[test]
fn variant_set_parses_into_structured_form() {
    // Mirrors the OpenUSD glossary `simpleVariantSet.usd` example:
    // a variantSet declared inside a prim body, with each variant
    // carrying a nested `def` prim. The parser surfaces the
    // structure on `Prim::variant_sets` so a future composition
    // engine can switch on `variants = { string foo = "bar" }`.
    let src = br#"#usda 1.0
def Xform "Implicits" (
    append variantSets = "shapeVariant"
) {
    variantSet "shapeVariant" = {
        "Capsule" {
            def Capsule "Pill" {
            }
        }
        "Cone" {
            def Cone "PartyHat" {
            }
        }
        "Cube" {
            def Cube "Box" {
            }
        }
    }
}
"#;
    let layer = parse(src).expect("parse simpleVariantSet.usd");
    let prim = &layer.prims[0];
    assert_eq!(prim.name, "Implicits");

    // The variantSets metadata key still surfaces in `metadata`.
    assert!(prim.metadata.contains_key("variantSets"));

    // The variantSet block lands in `variant_sets`.
    let vsets = &prim.variant_sets;
    assert_eq!(vsets.len(), 1, "one variantSet declared");
    let shape = vsets.get("shapeVariant").expect("shapeVariant key");
    assert_eq!(shape.len(), 3, "three variants");
    assert!(shape.contains_key("Capsule"));
    assert!(shape.contains_key("Cone"));
    assert!(shape.contains_key("Cube"));

    // Each variant body holds the nested `def` as a child prim.
    let capsule_v = shape.get("Capsule").unwrap();
    assert_eq!(capsule_v.children.len(), 1);
    assert_eq!(capsule_v.children[0].type_name, "Capsule");
    assert_eq!(capsule_v.children[0].name, "Pill");
    assert!(capsule_v.metadata.is_empty());
    assert!(capsule_v.attrs.is_empty());

    let cube_v = shape.get("Cube").unwrap();
    assert_eq!(cube_v.children[0].name, "Box");
}

#[test]
fn variant_set_with_per_variant_metadata() {
    // Per the glossary's `referenceVariantSet` example each variant
    // can carry its own `( ... )` metadata block — typically a
    // composition arc bringing in another asset.
    let src = br#"#usda 1.0
over "Model" {
    variantSet "referenceVariantSet" = {
        "asset1" (
            prepend references = @./Asset1.usda@
        ) {
        }
        "asset2" (
            prepend references = @./Asset2.usda@
        ) {
        }
    }
}
"#;
    let layer = parse(src).expect("parse variantSet with per-variant metadata");
    let prim = &layer.prims[0];
    let vset = prim
        .variant_sets
        .get("referenceVariantSet")
        .expect("variantSet present");
    let asset1 = vset.get("asset1").expect("asset1 variant");
    let refs = asset1
        .metadata
        .get("references")
        .expect("references metadata on asset1");
    match refs {
        Value::Asset(s) => assert_eq!(s, "./Asset1.usda"),
        other => panic!("expected Asset, got {other:?}"),
    }
    let asset2 = vset.get("asset2").expect("asset2 variant");
    match asset2.metadata.get("references") {
        Some(Value::Asset(s)) => assert_eq!(s, "./Asset2.usda"),
        other => panic!("expected Asset, got {other:?}"),
    }
}

#[test]
fn empty_variant_set_block() {
    // A variantSet declared but with no variants — Pixar's tooling
    // sometimes emits this when a variant is removed but the
    // declaration stub is left behind.
    let src = b"#usda 1.0\ndef Xform \"X\" {\n    variantSet \"foo\" = {\n    }\n}\n";
    let layer = parse(src).expect("parse empty variantSet");
    let prim = &layer.prims[0];
    let vset = prim.variant_sets.get("foo").expect("foo variantSet");
    assert!(vset.is_empty());
}

#[test]
fn parse_error_reports_line_and_column() {
    // Round 6: error messages render `line L:C (offset N)` instead
    // of the bare offset. Plant a stray garbage character at a
    // known line and verify the error mentions both `line` and the
    // 1-indexed column.
    //
    // Line 1: `#usda 1.0`
    // Line 2: `def Xform "Root" {`
    // Line 3: `    %foo = 1`   ← `%` is invalid as type-token start
    // Line 4: `}`
    let src = b"#usda 1.0\ndef Xform \"Root\" {\n    %foo = 1\n}\n";
    let err = parse(src).expect_err("parse should fail on stray `%`");
    // `Error::InvalidData(String)` under both registry and
    // standalone feature configurations of oxideav-mesh3d, so we
    // match `Display` rather than the variant directly.
    let msg = format!("{err}");
    assert!(
        msg.contains("line 3"),
        "error should mention the line number, got: {msg}"
    );
    // Column is 5 (1-indexed; four spaces of indent + the `%` itself).
    assert!(
        msg.contains(":5"),
        "error should mention the column, got: {msg}"
    );
    // Offset stays in the message for callers that prefer raw bytes.
    assert!(
        msg.contains("offset"),
        "error should still carry the offset, got: {msg}"
    );
}

#[test]
fn doc_triple_quoted_string_in_metadata() {
    // Pixar's serialiser emits a `doc = """..."""` field when it
    // adds a "generated from..." header during flattening. Triple
    // strings already work via `read_quoted_string`, but lock down
    // its use in metadata position.
    let src = b"#usda 1.0\n(\n    doc = \"\"\"Generated from /tmp/foo.usda\"\"\"\n    upAxis = \"Y\"\n)\n";
    let layer = parse(src).expect("parse triple-quoted doc");
    assert_eq!(
        layer
            .metadata
            .get("doc")
            .and_then(|v| v.as_text())
            .map(str::trim),
        Some("Generated from /tmp/foo.usda")
    );
}
