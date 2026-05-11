//! Round 8: variant *writer* — the encoder re-emits `variantSet`
//! blocks + `variants = {...}` selection metadata captured from the
//! source layer so a USDZ → USDZ round trip preserves every variant
//! body, including the unselected branches that round 7 left invisible
//! to the writer.
//!
//! These tests assert that:
//!
//! 1. The decoder stashes the structured `variant_sets` block on
//!    `Node::extras["usd:variantSets"]`.
//! 2. The encoder reads that extras key back and writes a syntactically
//!    valid `variantSet "name" = { ... }` block with every variant's
//!    body intact.
//! 3. A second decode pass over the re-encoded archive observes the
//!    same variants the original carried (including the *unselected*
//!    Cone / Cube branches in the classic glossary example).

mod common;

use oxideav_usdz::{usda::parse, UsdzDecoder, UsdzEncoder};

fn build_usdz(usda: &str) -> Vec<u8> {
    common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }])
}

fn extract_default_layer(usdz: &[u8]) -> Vec<u8> {
    // The encoder always emits the Default Layer first.  Walk the ZIP
    // central directory to extract it.
    let entries = oxideav_usdz::zip::walk(usdz).expect("zip walk");
    let head = &entries[0];
    usdz[head.payload_offset as usize..(head.payload_offset + head.payload_len) as usize].to_vec()
}

#[test]
fn variant_sets_round_trip_through_encoder() {
    // Classic glossary `simpleVariantSet.usd` example — three shape
    // variants on a single Xform, "Capsule" selected.  After
    // decode → encode → decode, all three variant bodies must still be
    // visible on the parsed prim's `variant_sets` map.
    let src = r#"#usda 1.0
def Xform "Implicits" (
    variants = { string shapeVariant = "Capsule" }
    append variantSets = "shapeVariant"
) {
    variantSet "shapeVariant" = {
        "Capsule" {
            def Xform "Pill" {
            }
        }
        "Cone" {
            def Xform "PartyHat" {
            }
        }
        "Cube" {
            def Xform "Box" {
            }
        }
    }
}
"#;
    let usdz_in = build_usdz(src);
    let scene = UsdzDecoder::new()
        .decode_bytes(&usdz_in)
        .expect("decode ok");
    // The "Implicits" node carries the variantSets stash.
    let implicits = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Implicits"))
        .expect("Implicits node present");
    let stash = implicits
        .extras
        .get("usd:variantSets")
        .expect("usd:variantSets extras present after decode");
    assert!(
        stash.get("shapeVariant").is_some(),
        "shapeVariant set captured: {stash:?}"
    );

    // Encode back to USDZ.
    let usdz_out = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");

    // Walk the encoded layer and verify the variantSet block + each
    // variant name made it into the output text.
    let layer_bytes = extract_default_layer(&usdz_out);
    let layer_text = std::str::from_utf8(&layer_bytes).expect("utf-8 layer");
    assert!(
        layer_text.contains("variantSet \"shapeVariant\""),
        "variantSet block re-emitted: {layer_text}"
    );
    assert!(layer_text.contains("\"Capsule\""), "Capsule variant kept");
    assert!(layer_text.contains("\"Cone\""), "Cone variant kept");
    assert!(layer_text.contains("\"Cube\""), "Cube variant kept");
    assert!(
        layer_text.contains("variants = {"),
        "selection metadata re-emitted"
    );
    assert!(
        layer_text.contains("variantSets = ["),
        "variantSets list re-emitted"
    );

    // Re-parse the emitted layer through the structured parser to
    // confirm syntactic validity (any malformed output trips the
    // tokenizer / prim-body parser here).
    let layer = parse(&layer_bytes).expect("re-parse encoded layer");
    let implicits_prim = layer
        .prims
        .iter()
        .find(|p| p.name == "Implicits")
        .expect("Implicits prim parsed");
    let set = implicits_prim
        .variant_sets
        .get("shapeVariant")
        .expect("shapeVariant set parsed");
    assert_eq!(set.len(), 3, "all three variants survive: {set:?}");
    let capsule = set.get("Capsule").expect("Capsule variant");
    assert_eq!(
        capsule.children.len(),
        1,
        "Capsule's `def Xform Pill` survived"
    );
    assert_eq!(capsule.children[0].name, "Pill");

    // Round-trip through the decoder one more time — the second-pass
    // Scene3D should look identical to the first.
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&usdz_out)
        .expect("re-decode ok");
    let pill_present = scene2
        .nodes
        .iter()
        .any(|n| n.name.as_deref() == Some("Pill"));
    assert!(
        pill_present,
        "Capsule variant still resolves to Pill on re-decode"
    );
    let cone_only = scene2
        .nodes
        .iter()
        .any(|n| n.name.as_deref() == Some("PartyHat"));
    assert!(
        !cone_only,
        "unselected Cone variant doesn't materialise on re-decode"
    );
}

#[test]
fn variant_with_attribute_round_trips() {
    // A variant authoring an attribute on the prim body — verify the
    // attribute survives the JSON encode/decode trip.
    let src = r#"#usda 1.0
def Xform "Implicits" (
    variants = { string v = "a" }
) {
    variantSet "v" = {
        "a" {
            custom string greeting = "hello"
        }
        "b" {
            custom string greeting = "world"
        }
    }
}
"#;
    let usdz_in = build_usdz(src);
    let scene = UsdzDecoder::new().decode_bytes(&usdz_in).unwrap();
    let usdz_out = UsdzEncoder::new().encode_bytes(&scene).expect("encode");
    let layer_bytes = extract_default_layer(&usdz_out);
    let layer = parse(&layer_bytes).expect("re-parse");
    let prim = layer
        .prims
        .iter()
        .find(|p| p.name == "Implicits")
        .expect("Implicits prim");
    let v_set = &prim.variant_sets["v"];
    let a = v_set.get("a").expect("a variant");
    assert_eq!(
        a.attrs.get("greeting").map(|x| x.value.as_text()),
        Some(Some("hello"))
    );
    let b = v_set.get("b").expect("b variant");
    assert_eq!(
        b.attrs.get("greeting").map(|x| x.value.as_text()),
        Some(Some("world"))
    );
}

#[test]
fn no_variants_keeps_writer_output_unchanged_shape() {
    // Sanity check: a node WITHOUT variantSets should NOT pick up a
    // `(...)` metadata block on the writer side — earlier rounds rely
    // on the bare `def Xform "name" {` form.
    let src = r#"#usda 1.0
def Xform "Plain" {
}
"#;
    let usdz_in = build_usdz(src);
    let scene = UsdzDecoder::new().decode_bytes(&usdz_in).unwrap();
    let usdz_out = UsdzEncoder::new().encode_bytes(&scene).expect("encode");
    let layer_bytes = extract_default_layer(&usdz_out);
    let layer_text = std::str::from_utf8(&layer_bytes).unwrap();
    assert!(
        layer_text.contains("def Xform \"Plain\" {"),
        "no metadata block when no variants: {layer_text}"
    );
    assert!(
        !layer_text.contains("variantSet"),
        "no variantSet emission for variantless node"
    );
}

#[test]
fn multiple_variantsets_round_trip() {
    // Two distinct variantSets on the same prim — both must survive
    // the round trip.
    let src = r#"#usda 1.0
def Xform "Asset" (
    variants = {
        string lod = "high"
        string region = "europe"
    }
) {
    variantSet "lod" = {
        "high" {
            def Xform "HighDetail" {
            }
        }
        "low" {
            def Xform "LowDetail" {
            }
        }
    }
    variantSet "region" = {
        "europe" {
            def Xform "EUFlag" {
            }
        }
        "asia" {
            def Xform "JPFlag" {
            }
        }
    }
}
"#;
    let usdz_in = build_usdz(src);
    let scene = UsdzDecoder::new().decode_bytes(&usdz_in).unwrap();
    let usdz_out = UsdzEncoder::new().encode_bytes(&scene).expect("encode");
    let layer_bytes = extract_default_layer(&usdz_out);
    let layer = parse(&layer_bytes).expect("re-parse");
    let prim = layer
        .prims
        .iter()
        .find(|p| p.name == "Asset")
        .expect("Asset prim");
    assert!(prim.variant_sets.contains_key("lod"));
    assert!(prim.variant_sets.contains_key("region"));
    assert_eq!(prim.variant_sets["lod"].len(), 2);
    assert_eq!(prim.variant_sets["region"].len(), 2);

    // Re-decode picks the same variants thanks to round-tripped
    // `variants = {...}` selection.
    let scene2 = UsdzDecoder::new().decode_bytes(&usdz_out).unwrap();
    let names: Vec<&str> = scene2
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(
        names.contains(&"HighDetail"),
        "lod=high resolves: {names:?}"
    );
    assert!(
        names.contains(&"EUFlag"),
        "region=europe resolves: {names:?}"
    );
    assert!(!names.contains(&"LowDetail"), "lod=low not selected");
    assert!(!names.contains(&"JPFlag"), "region=asia not selected");
}
