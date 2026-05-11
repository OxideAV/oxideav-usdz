//! Round 9 — Composition-arc + layer-metadata round-trip on the
//! writer.
//!
//! The decoder side already parses `defaultPrim`, `subLayers`,
//! `customLayerData`, `references`, `payload`, `kind`, `apiSchemas`
//! and similar entries — they ride on `Scene3D::extras` /
//! `Node::extras`. Round 1..8 dropped them silently when serialising,
//! so a USDZ → `Scene3D` → USDZ round trip lost every composition
//! opinion. Round 9 closes that gap on the writer: the new
//! lossless tagged blobs at `usd:layerMetadata` and `usd:primMetadata`
//! preserve every value's USDA type-token discriminant
//! (`Token` vs `String` vs `Asset` vs `AssetWithPath` vs `Path`)
//! through the round trip.
//!
//! Coverage in this file:
//! * Layer-level `defaultPrim` round-trips with its `Token`-shaped
//!   discriminant intact.
//! * Layer-level `subLayers = [@a@, @b@]` round-trips and re-decodes
//!   into the same `Value::Array` of `Value::Asset`s.
//! * Prim-level `prepend references = @./asset.usd@</Asset>`
//!   round-trips as an `AssetWithPath` opinion on the same prim.
//! * Prim-level `prepend payload = @./payload.usd@` round-trips with
//!   the `prepend` list-edit operator and the `Asset` discriminant.
//! * Prim-level `kind = "component"` survives.
//! * `customLayerData = { string version = "1.0" }` survives.
//!
//! Trace: each assertion checks the *decoded re-decoded* USDA tree
//! (parse → translate → write_layer → parse again) so we exercise
//! the whole pipeline, not just the writer's text shape.

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_usdz::usda::{parse, Value};
use oxideav_usdz::UsdzDecoder;

use oxideav_mesh3d::Mesh3DDecoder;

fn decode_roundtrip_emit(usda: &str) -> String {
    let archive = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&archive).expect("decode round-trip");
    // The writer is private but `UsdzEncoder` re-exports its output
    // shape — we call `write_layer` directly through the public
    // module so the test stays close to the unit-under-test.
    oxideav_usdz::usda_writer::write_layer(&scene)
}

#[test]
fn default_prim_round_trips() {
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    defaultPrim = "Root"
)

def Xform "Root" {
}
"#;
    let written = decode_roundtrip_emit(src);
    assert!(
        written.contains("defaultPrim = \"Root\""),
        "expected defaultPrim line in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    match re.metadata.get("defaultPrim") {
        Some(Value::Token(s)) | Some(Value::String(s)) => assert_eq!(s, "Root"),
        other => panic!("expected defaultPrim to round-trip as token/string, got {other:?}"),
    }
}

#[test]
fn sublayers_array_round_trips() {
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    subLayers = [
        @./layer1.usd@,
        @./layer2.usd@
    ]
)

def Xform "Root" {
}
"#;
    let written = decode_roundtrip_emit(src);
    assert!(
        written.contains("subLayers ="),
        "expected subLayers in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    let subs = re
        .metadata
        .get("subLayers")
        .expect("subLayers key on re-parse");
    let seq = subs.as_seq().expect("subLayers is an array");
    assert_eq!(seq.len(), 2);
    // Round-trip preserves `Asset` (`@uri@`) discriminant — not
    // a generic `Token` / `String`.
    match &seq[0] {
        Value::Asset(s) => assert_eq!(s, "./layer1.usd"),
        other => panic!("expected Asset, got {other:?}"),
    }
    match &seq[1] {
        Value::Asset(s) => assert_eq!(s, "./layer2.usd"),
        other => panic!("expected Asset, got {other:?}"),
    }
}

#[test]
fn references_with_selector_round_trips() {
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def Xform "Root" (
    prepend references = @./asset.usd@</Asset>
) {
}
"#;
    let written = decode_roundtrip_emit(src);
    // Composition arcs ride with `prepend` per LIVRPS authoring
    // convention — the same shape every USD tool emits.
    assert!(
        written.contains("prepend references = @./asset.usd@<"),
        "expected prepend references arc in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    let root = re
        .prims
        .iter()
        .find(|p| p.name == "Root")
        .expect("Root prim");
    let refs = root
        .metadata
        .get("references")
        .expect("references opinion on re-parse");
    match refs {
        Value::AssetWithPath { asset, prim_path } => {
            assert_eq!(asset, "./asset.usd");
            assert_eq!(prim_path, "/Asset");
        }
        other => panic!("expected AssetWithPath, got {other:?}"),
    }
}

#[test]
fn payload_arc_round_trips() {
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def Xform "Root" (
    prepend payload = @./payload.usd@
) {
}
"#;
    let written = decode_roundtrip_emit(src);
    assert!(
        written.contains("prepend payload = @./payload.usd@"),
        "expected payload arc in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    let root = re
        .prims
        .iter()
        .find(|p| p.name == "Root")
        .expect("Root prim");
    match root.metadata.get("payload") {
        Some(Value::Asset(s)) => assert_eq!(s, "./payload.usd"),
        other => panic!("expected Asset, got {other:?}"),
    }
}

#[test]
fn prim_kind_round_trips() {
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def Xform "Root" (
    kind = "component"
) {
}
"#;
    let written = decode_roundtrip_emit(src);
    assert!(
        written.contains("kind = \"component\""),
        "expected kind opinion in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    let root = re
        .prims
        .iter()
        .find(|p| p.name == "Root")
        .expect("Root prim");
    match root.metadata.get("kind") {
        Some(Value::Token(s)) | Some(Value::String(s)) => assert_eq!(s, "component"),
        other => panic!("expected kind to round-trip as token/string, got {other:?}"),
    }
}

#[test]
fn customlayerdata_round_trips() {
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    customLayerData = {
        string version = "1.0"
    }
)

def Xform "Root" {
}
"#;
    let written = decode_roundtrip_emit(src);
    // The customLayerData block should re-appear (regardless of
    // the exact internal shape — the parser preserves the key).
    assert!(
        written.contains("customLayerData"),
        "expected customLayerData in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    assert!(re.metadata.contains_key("customLayerData"));
}

#[test]
fn apischemas_list_round_trips() {
    // `prepend apiSchemas = ["MaterialBindingAPI"]` is the standard
    // way to apply an API schema to a prim; round-9 surfaces it back
    // to the writer with its list-edit operator intact.
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def Xform "Root" (
    prepend apiSchemas = ["MaterialBindingAPI"]
) {
}
"#;
    let written = decode_roundtrip_emit(src);
    assert!(
        written.contains("prepend apiSchemas = [\"MaterialBindingAPI\"]"),
        "expected prepend apiSchemas list in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    let root = re
        .prims
        .iter()
        .find(|p| p.name == "Root")
        .expect("Root prim");
    let api = root
        .metadata
        .get("apiSchemas")
        .expect("apiSchemas opinion on re-parse");
    let seq = api.as_seq().expect("apiSchemas is an array");
    assert_eq!(seq.len(), 1);
    match &seq[0] {
        Value::String(s) => assert_eq!(s, "MaterialBindingAPI"),
        Value::Token(s) => assert_eq!(s, "MaterialBindingAPI"),
        other => panic!("expected string/token entry, got {other:?}"),
    }
}

#[test]
fn unknown_layer_metadata_key_round_trips() {
    // Apple's `usdzconvert` writes its own metadata keys
    // (`apple_metadata`, ...) outside the canonical USD set. The
    // round-trip should preserve them verbatim — the writer can't
    // know the original type token, but the key must survive.
    let src = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    customField = "hello"
)

def Xform "Root" {
}
"#;
    let written = decode_roundtrip_emit(src);
    assert!(
        written.contains("customField"),
        "expected customField in re-emitted USDA, got:\n{written}"
    );
    let re = parse(written.as_bytes()).expect("re-parse round-tripped USDA");
    match re.metadata.get("customField") {
        Some(Value::String(s)) => assert_eq!(s, "hello"),
        other => panic!("expected customField to be a String, got {other:?}"),
    }
}
