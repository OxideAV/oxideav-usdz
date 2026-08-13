//! `UsdShadeMaterialBindingAPI` direct-binding resolution (staged
//! schema `docs/3d/usd/usdgeom-usdshade-schema.md` Part 3):
//! purpose-restricted relationship spellings (§3.1/§3.2), the
//! `bindMaterialAs` strength metadata (§3.3), rule-1 namespace
//! inheritance and the rule-3 restricted-over-all-purpose
//! preference (§3.4) — plus verbatim round-trip of every authored
//! spelling.

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Mesh3DEncoder, Scene3D};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn decode(usda: &str) -> Scene3D {
    let usdz = build_usdz(&[UsdzEntry {
        name: "layer.usda",
        payload: usda.as_bytes(),
    }]);
    UsdzDecoder::new().decode(&usdz).expect("decode")
}

fn default_layer_text(usdz: &[u8]) -> String {
    let name_len = u16::from_le_bytes([usdz[26], usdz[27]]) as usize;
    let extra_len = u16::from_le_bytes([usdz[28], usdz[29]]) as usize;
    let size = u32::from_le_bytes([usdz[18], usdz[19], usdz[20], usdz[21]]) as usize;
    let start = 30 + name_len + extra_len;
    String::from_utf8(usdz[start..start + size].to_vec()).expect("utf8 layer")
}

fn materials_block(names: &[(&str, &str)]) -> String {
    let mut out = String::from("def Scope \"Materials\" {\n");
    for (name, color) in names {
        out.push_str(&format!(
            r#"    def Material "{name}" {{
        def Shader "PBRShader" {{
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = {color}
            token outputs:surface
        }}
    }}
"#
        ));
    }
    out.push_str("}\n");
    out
}

fn bound_material_name(scene: &Scene3D) -> Option<String> {
    let prim = scene.meshes.first()?.primitives.first()?;
    let mat = &scene.materials[prim.material?.0 as usize];
    mat.name.clone()
}

const TRI: &str = r#"        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
"#;

#[test]
fn ancestor_binding_inherits_down_namespace() {
    // §3.4 rule 1: a binding on the enclosing Xform reaches an
    // unbound descendant gprim.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    rel material:binding = </Materials/Red>
    def Xform "Mid" {{
        def Mesh "M" {{
{TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Red"));
}

#[test]
fn leaf_binding_beats_weaker_ancestor() {
    // §3.4 rule 1: closer to the leaf wins when the ancestor is
    // unmarked (default weakerThanDescendants).
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    rel material:binding = </Materials/Red>
    def Mesh "M" {{
{TRI}        rel material:binding = </Materials/Green>
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Green"));
}

#[test]
fn stronger_than_descendants_ancestor_wins() {
    // §3.3: bindMaterialAs = "strongerThanDescendants" reverses
    // rule 1.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    rel material:binding = </Materials/Red> (
        bindMaterialAs = "strongerThanDescendants"
    )
    def Mesh "M" {{
{TRI}        rel material:binding = </Materials/Green>
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Red"));
}

#[test]
fn restricted_preview_preferred_over_all_purpose() {
    // §3.4 rule 3: at one prim, the requested-purpose restricted
    // binding beats the all-purpose one. The typed slot requests
    // `preview` (the crate's material model is UsdPreviewSurface).
    let usda = format!(
        r#"#usda 1.0
def Mesh "M" {{
{TRI}    rel material:binding = </Materials/Red>
    rel material:binding:preview = </Materials/Green>
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Green"));
}

#[test]
fn full_purpose_binds_only_as_last_resort() {
    // A full-only asset still materialises a bound mesh…
    let usda_full_only = format!(
        r#"#usda 1.0
def Mesh "M" {{
{TRI}    rel material:binding:full = </Materials/Red>
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda_full_only);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Red"));

    // …but never beats an all-purpose or preview opinion.
    let usda_both = format!(
        r#"#usda 1.0
def Mesh "M" {{
{TRI}    rel material:binding:full = </Materials/Red>
    rel material:binding = </Materials/Green>
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene = decode(&usda_both);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Green"));
}

#[test]
fn arbitrary_purpose_tokens_never_bind_but_survive() {
    // §3.1: the purpose position is not a closed enumeration — an
    // arbitrary purpose never binds the typed slot but round-trips.
    let usda = format!(
        r#"#usda 1.0
def Mesh "M" {{
{TRI}    rel material:binding:shadow = </Materials/Red>
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene), None);
    let out = UsdzEncoder::new().encode(&scene).expect("encode");
    let text = default_layer_text(&out);
    assert!(
        text.contains("rel material:binding:shadow = </Materials/Red>"),
        "arbitrary-purpose binding preserved: {text}"
    );
}

#[test]
fn authored_spellings_roundtrip_verbatim() {
    // Purpose forms + strength metadata survive encode → decode →
    // encode as a fixed point, with the strength metadata intact.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    rel material:binding = </Materials/Red> (
        bindMaterialAs = "strongerThanDescendants"
    )
    def Mesh "M" {{
{TRI}        rel material:binding:preview = </Materials/Green>
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene1 = decode(&usda);
    // strongerThanDescendants ancestor wins even over the leaf's
    // preview-restricted opinion.
    assert_eq!(bound_material_name(&scene1).as_deref(), Some("Red"));
    let out1 = UsdzEncoder::new().encode(&scene1).expect("encode 1");
    let text1 = default_layer_text(&out1);
    assert!(
        text1.contains("bindMaterialAs = \"strongerThanDescendants\""),
        "strength metadata replays: {text1}"
    );
    assert!(text1.contains("rel material:binding:preview = </Materials/Green>"));
    // No synthetic mesh-level all-purpose binding was invented.
    assert!(
        !text1.contains("rel material:binding = </Materials/Red>\n            "),
        "no synthetic gprim binding inside the mesh prim"
    );
    let scene2 = UsdzDecoder::new().decode(&out1).expect("decode 2");
    assert_eq!(bound_material_name(&scene2).as_deref(), Some("Red"));
    let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
    assert_eq!(text1, default_layer_text(&out2), "fixed point");
}

#[test]
fn ancestor_resolved_binding_adds_no_gprim_relationship() {
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    rel material:binding = </Materials/Red>
    def Mesh "M" {{
{TRI}    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Red"));
    let out = UsdzEncoder::new().encode(&scene).expect("encode");
    let text = default_layer_text(&out);
    // Exactly one binding relationship — the ancestor's.
    assert_eq!(
        text.matches("rel material:binding").count(),
        1,
        "no synthetic duplicate on the gprim: {text}"
    );
    // Fixed point.
    let scene2 = UsdzDecoder::new().decode(&out).expect("decode 2");
    let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
    assert_eq!(text, default_layer_text(&out2));
}

#[test]
fn subset_purpose_restricted_binding_selects_split_material() {
    // A GeomSubset authoring only material:binding:preview still
    // participates in the per-face material split (Part 1 ∩ Part 3).
    let usda = format!(
        r#"#usda 1.0
def Mesh "M" {{
    int[] faceVertexCounts = [3, 3]
    int[] faceVertexIndices = [0, 1, 2, 3, 4, 5]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0), (2,0,0), (3,0,0), (2,1,0)]
    rel material:binding = </Materials/Red>
    def GeomSubset "pv" {{
        uniform token familyName = "materialBind"
        int[] indices = [1]
        rel material:binding:preview = </Materials/Green>
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene = decode(&usda);
    let mesh = &scene.meshes[0];
    assert_eq!(mesh.primitives.len(), 2);
    let sub = mesh
        .primitives
        .iter()
        .find(|p| p.extras.contains_key("usd:subset"))
        .expect("subset primitive");
    assert_eq!(
        scene.materials[sub.material.unwrap().0 as usize]
            .name
            .as_deref(),
        Some("Green")
    );
    // The authored preview spelling replays verbatim, and the cycle
    // is a fixed point.
    let out1 = UsdzEncoder::new().encode(&scene).expect("encode");
    let text1 = default_layer_text(&out1);
    assert!(
        text1.contains("rel material:binding:preview = </Materials/Green>"),
        "{text1}"
    );
    let scene2 = UsdzDecoder::new().decode(&out1).expect("decode 2");
    let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
    assert_eq!(text1, default_layer_text(&out2));
}

#[test]
fn collection_forms_are_preserved_verbatim() {
    // §3.2 four/five-token collection forms: not yet evaluated into
    // the typed slot, but the authored relationships survive
    // round-trip untouched.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    rel material:binding:collection:metalBits = [
        </Root.collection:metalBits>,
        </Materials/Red>
    ]
    def Mesh "M" {{
{TRI}        rel material:binding = </Materials/Green>
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Green", "(0, 1, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(bound_material_name(&scene).as_deref(), Some("Green"));
    let out = UsdzEncoder::new().encode(&scene).expect("encode");
    let text = default_layer_text(&out);
    assert!(
        text.contains("material:binding:collection:metalBits"),
        "collection binding preserved: {text}"
    );
    assert!(text.contains("</Root.collection:metalBits>"));
}
