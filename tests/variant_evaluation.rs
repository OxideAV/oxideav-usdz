//! Round 7: variant selection composition.
//!
//! Round 6 captured `variant_sets` on `Prim` as a parser-side
//! structured field but the Scene3D translator ignored them. Round
//! 7 wires the selection metadata through: `metadata["variants"] =
//! { string SET = "VAR" }` brings the matching
//! `variant_sets[SET][VAR]`'s `attrs` + `children` into the
//! prim body before the translator builds the Scene3D node tree.
//!
//! These tests cover the composition rules per the OpenUSD
//! glossary's `simpleVariantSet.usd` + `referenceVariantSet`
//! examples plus the LIVRPS strength-order tail (Local opinions
//! beat variant opinions when both author the same key).

mod common;

use oxideav_usdz::usda::{parse, Prim, Value};
use oxideav_usdz::UsdzDecoder;

/// Build a USDZ archive carrying a single USDA layer.
fn usdz(usda: &str) -> Vec<u8> {
    common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }])
}

#[test]
fn variant_selection_composes_child_def_into_node_tree() {
    // The classic glossary example: a variantSet declaring three
    // shape variants, each contributing a different `def` child,
    // selected via the `variants = { string shapeVariant = "..." }`
    // metadata on the enclosing prim.
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
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    // Root Xform "Implicits" + composed child "Pill" — the Cone /
    // Cube variants are NOT selected and don't materialise.
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(names.contains(&"Implicits"), "root node present");
    assert!(
        names.contains(&"Pill"),
        "Capsule variant child surfaced: got {names:?}"
    );
    assert!(
        !names.contains(&"PartyHat"),
        "Cone variant should not materialise"
    );
    assert!(
        !names.contains(&"Box"),
        "Cube variant should not materialise"
    );
}

#[test]
fn variant_selection_changing_picks_different_child() {
    // Same variantSet shape, different selection — exercises that
    // the resolver keys off the metadata value, not just the first
    // variant.
    let src = r#"#usda 1.0
def Xform "Implicits" (
    variants = { string shapeVariant = "Cone" }
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
    }
}
"#;
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(names.contains(&"PartyHat"), "Cone variant materialises");
    assert!(!names.contains(&"Pill"), "Capsule variant skipped");
}

#[test]
fn no_selection_metadata_leaves_variants_uncomposed() {
    // variantSet declared but no `variants = {...}` selection on
    // the prim → the variant body's children stay invisible (per
    // USD's "variant selections are not required" rule, the
    // fallback when no selection is authored is to ignore the set
    // until a stronger layer authors a selection).
    let src = r#"#usda 1.0
def Xform "Implicits" {
    variantSet "shapeVariant" = {
        "Capsule" {
            def Xform "Pill" {
            }
        }
    }
}
"#;
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(names.contains(&"Implicits"));
    assert!(
        !names.contains(&"Pill"),
        "no selection → no variant child surfaced: got {names:?}"
    );
}

#[test]
fn unknown_variant_name_silently_ignored() {
    // Per the glossary: "Variant selections are not required" —
    // selecting a variant name that doesn't exist in the set is
    // tolerated; the prim falls back to its locally-authored body
    // (no children here, just the bare Xform).
    let src = r#"#usda 1.0
def Xform "Implicits" (
    variants = { string shapeVariant = "DoesNotExist" }
) {
    variantSet "shapeVariant" = {
        "Capsule" {
            def Xform "Pill" {
            }
        }
    }
}
"#;
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(names.contains(&"Implicits"));
    assert!(!names.contains(&"Pill"), "no matching variant → no child");
}

#[test]
fn local_child_wins_over_variant_child_with_same_name() {
    // LIVRPS: Local > Variant. When both author a child named
    // "Pill", the local `def` wins. We use distinct types to
    // disambiguate which one materialised.
    let src = r#"#usda 1.0
def Xform "Implicits" (
    variants = { string shapeVariant = "Capsule" }
) {
    def Xform "Pill" {
        # Local Pill carries no children — distinguishable from the
        # variant version below by its empty children list.
    }

    variantSet "shapeVariant" = {
        "Capsule" {
            def Xform "Pill" {
                def Xform "VariantOnlyChild" {
                }
            }
        }
    }
}
"#;
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    // Find the "Pill" node; look at its child count.
    let pill = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Pill"))
        .expect("Pill node present");
    assert_eq!(
        pill.children.len(),
        0,
        "local Pill (empty children) won over variant Pill (1 child)"
    );
    // The variant-only "VariantOnlyChild" must NOT be in the scene.
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(
        !names.contains(&"VariantOnlyChild"),
        "variant child collided with local Pill — local won, variant child suppressed: got {names:?}"
    );
}

#[test]
fn multiple_variantsets_compose_independently() {
    // Two distinct variantSets, each contributing a different child.
    // Both selections are honoured.
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
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(names.contains(&"HighDetail"));
    assert!(names.contains(&"EUFlag"));
    assert!(!names.contains(&"LowDetail"));
    assert!(!names.contains(&"JPFlag"));
}

#[test]
fn variant_with_mesh_def_materialises_into_scene_mesh() {
    // The variant brings in a `def Mesh` — verify the full Scene3D
    // mapping (mesh + primitive + positions) fires after variant
    // composition. This is the round's headline behaviour: variants
    // aren't just metadata, they pull through into the typed model.
    let src = r#"#usda 1.0
def Xform "Asset" (
    variants = { string shape = "tri" }
) {
    variantSet "shape" = {
        "tri" {
            def Mesh "Tri" {
                int[] faceVertexCounts = [3]
                int[] faceVertexIndices = [0, 1, 2]
                point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
            }
        }
    }
}
"#;
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    assert_eq!(scene.meshes.len(), 1, "variant-authored Mesh materialised");
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.positions.len(), 3);
    assert_eq!(prim.triangle_count(), 1);
}

#[test]
fn variant_selection_deep_in_tree_composes() {
    // The variantSet lives on a non-root prim; verify the recursive
    // walk reaches it.
    let src = r#"#usda 1.0
def Xform "Root" {
    def Xform "Inner" (
        variants = { string v = "a" }
    ) {
        variantSet "v" = {
            "a" {
                def Xform "DeepChild" {
                }
            }
        }
    }
}
"#;
    let scene = UsdzDecoder::new().decode_bytes(&usdz(src)).unwrap();
    let names: Vec<&str> = scene
        .nodes
        .iter()
        .filter_map(|n| n.name.as_deref())
        .collect();
    assert!(
        names.contains(&"DeepChild"),
        "deeply-nested variant resolved: got {names:?}"
    );
}

#[test]
fn variant_references_recorded_in_metadata_for_caller() {
    // External `references = @other.usd@` inside a selected
    // variant aren't followed (round 7 is single-layer evaluation),
    // but they're recorded under
    // `metadata["usd:variantReferences:<set>:<var>:references"]` so
    // the Scene3D extras stash exposes the unresolved arc to the
    // caller. Test against the structured-parse layer rather than
    // the Scene3D translation since the translator doesn't surface
    // this key (only the prim's own metadata is stashed).
    let src = b"#usda 1.0
def Xform \"Implicits\" (
    variants = { string v = \"a\" }
) {
    variantSet \"v\" = {
        \"a\" (
            prepend references = @./other.usda@
        ) {
        }
    }
}
";
    let layer = parse(src).expect("parse ok");
    let resolved = layer.prims[0].resolved_variants();
    let key = "usd:variantReferences:v:a:references";
    let v = resolved
        .metadata
        .get(key)
        .unwrap_or_else(|| panic!("expected key {key}; got {:?}", resolved.metadata.keys()));
    // `prepend references` is preserved as a list-op; the authored
    // asset lives in the `prepended` sublist.
    let inner = match v {
        Value::ListOp(list) => list.prepended.as_ref().expect("prepended sublist"),
        other => other,
    };
    match inner {
        Value::Asset(s) => assert_eq!(s, "./other.usda"),
        other => panic!("expected Value::Asset, got {other:?}"),
    }
}

#[test]
fn variant_attribute_loses_to_local_attribute() {
    // LIVRPS for attributes: when both the prim body and the
    // selected variant author the same attribute key, the local
    // value wins. We use a `kind` token (parsed-and-stashed as
    // metadata, not a real attribute) — the cleanest demonstration
    // is at the Prim layer: build a Prim manually, ask for its
    // resolved view, check the attrs map.
    let src = b"#usda 1.0
def Xform \"Implicits\" (
    variants = { string v = \"a\" }
) {
    custom string localKey = \"FROM_LOCAL\"
    variantSet \"v\" = {
        \"a\" {
            custom string localKey = \"FROM_VARIANT\"
            custom string variantOnlyKey = \"FROM_VARIANT\"
        }
    }
}
";
    let layer = parse(src).expect("parse ok");
    let resolved = layer.prims[0].resolved_variants();
    let local = resolved
        .attrs
        .get("localKey")
        .expect("localKey present after compose");
    assert_eq!(
        local.value.as_text(),
        Some("FROM_LOCAL"),
        "Local opinion wins over variant"
    );
    let variant_only = resolved
        .attrs
        .get("variantOnlyKey")
        .expect("variantOnlyKey present after compose");
    assert_eq!(
        variant_only.value.as_text(),
        Some("FROM_VARIANT"),
        "variant-only key surfaces"
    );
}

#[test]
fn variant_sets_preserved_for_writer_round_trip() {
    // After resolution the original `variant_sets` block stays on
    // the resolved prim so a future writer can re-emit it. (The
    // current writer doesn't yet emit variantSets — that's a
    // separate slice — but the parser-side data model is preserved
    // either way.)
    let src = b"#usda 1.0
def Xform \"Implicits\" (
    variants = { string v = \"a\" }
) {
    variantSet \"v\" = {
        \"a\" {
            def Xform \"Pill\" {
            }
        }
        \"b\" {
            def Xform \"Cone\" {
            }
        }
    }
}
";
    let layer = parse(src).expect("parse ok");
    let resolved: Prim = layer.prims[0].resolved_variants();
    assert!(
        resolved.variant_sets.contains_key("v"),
        "variant_sets retained on resolved prim"
    );
    assert_eq!(resolved.variant_sets["v"].len(), 2);
    // The selection metadata also survives.
    assert!(resolved.metadata.contains_key("variants"));
}
