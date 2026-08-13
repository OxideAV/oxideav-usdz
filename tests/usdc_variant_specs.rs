//! Crate variant-spec bridging (AOUSD Core Specification
//! §16.3.8.4.6 forms 10/11, §7.6.6/§7.6.7, §8 variant selectors)
//! against the staged `crate-variant-specs.usdc` fixture — SPECS
//! forms 10 (Variant) and 11 (VariantSet) materialise into the text
//! model's `variant_sets` blocks, selections resolve through the
//! ordinary composition pipeline, and a USDZ whose default layer
//! carries variant specs decodes end-to-end.

mod common;

use oxideav_usdz::usdc_layer::layer_from_usdc;
use oxideav_usdz::UsdzDecoder;

/// Locate the staged variant fixture. `docs/` is a private sibling
/// checkout — skip (don't fail) when absent, mirroring the Elephant
/// fixture tests.
fn fixture_bytes() -> Option<Vec<u8>> {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/crate-variant-specs.usdc");
    if !fixture.exists() {
        return None;
    }
    Some(std::fs::read(&fixture).expect("read variant fixture"))
}

#[test]
fn variant_specs_bridge_into_variant_sets() {
    let Some(bytes) = fixture_bytes() else { return };
    let layer = layer_from_usdc(&bytes).expect("bridge");
    assert_eq!(layer.prims.len(), 1);
    let root = &layer.prims[0];
    assert_eq!(root.name, "VariantFixture");
    // Form 11 declared the set; forms 10 filled the variants.
    let shading = root
        .variant_sets
        .get("shadingVariant")
        .expect("shadingVariant set");
    let mut names: Vec<&str> = shading.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["blue", "green", "red"]);
    // Variant bodies carry their §7.6.7 prim-field opinions: the
    // attribute specs addressed through the {set=sel} selector.
    let red = &shading["red"];
    assert!(red.attrs.contains_key("primvars:displayColor"));
    assert!(red.attrs.contains_key("variantLabel"));
    // §16.3.10.30 variantSelection → the text form's `variants`
    // metadata dict.
    let sel = root.metadata.get("variants").expect("variants selection");
    let rendered = format!("{sel:?}");
    assert!(
        rendered.contains("shadingVariant") && rendered.contains("red"),
        "selection carries shadingVariant=red: {rendered}"
    );

    // The child prim carries two sets at a deeper namespace level,
    // including variants that author child prims.
    let child = &root.children[0];
    assert_eq!(child.name, "Child");
    let lod = child.variant_sets.get("lodVariant").expect("lodVariant");
    let high = &lod["high"];
    assert!(high.attrs.contains_key("subdivisionLevel"));
    assert_eq!(high.children.len(), 1, "variant-authored def Cube");
    assert_eq!(high.children[0].name, "Geom");
    assert_eq!(high.children[0].type_name, "Cube");
    assert!(high.children[0].attrs.contains_key("size"));
    let size = child.variant_sets.get("sizeVariant").expect("sizeVariant");
    assert!(size["small"].attrs.contains_key("xformOp:scale"));
    assert!(size["large"].attrs.contains_key("xformOpOrder"));
}

#[test]
fn variant_selection_resolves_end_to_end() {
    let Some(bytes) = fixture_bytes() else { return };
    // A USDZ whose default layer is the variant-spec Crate file
    // decodes through the ordinary pipeline: the authored
    // selections (shadingVariant=red, lodVariant=high,
    // sizeVariant=small) compose in.
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "fixture.usdc",
        payload: &bytes,
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    scene.validate().expect("validates");
    // The selected lodVariant=high composes its `def Cube "Geom"`
    // child into the scene graph (an unknown-schema node keeping
    // its type token).
    fn find_named<'a>(
        scene: &'a oxideav_mesh3d::Scene3D,
        name: &str,
    ) -> Option<&'a oxideav_mesh3d::Node> {
        scene.nodes.iter().find(|n| n.name.as_deref() == Some(name))
    }
    let geom = find_named(&scene, "Geom").expect("variant-composed Geom node");
    assert_eq!(
        geom.extras.get("usd:type").and_then(|v| v.as_str()),
        Some("Cube")
    );
    // The unselected variants stay available for the writer replay
    // through the variant stash.
    let root_node = find_named(&scene, "VariantFixture").expect("root node");
    assert!(
        root_node
            .extras
            .contains_key(oxideav_usdz::variant_codec::EXTRAS_KEY),
        "variant sets stashed for round-trip"
    );
}
