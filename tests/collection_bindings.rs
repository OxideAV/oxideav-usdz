//! Collection material bindings: AOUSD Core Specification §15
//! (CollectionAPI — includes / excludes / expansionRule /
//! includeRoot / referenced collections) evaluated through the
//! staged schema's §3.2 collection relationship forms and the §3.4
//! resolution rules 2, 4, 5 and 6, plus verbatim round-trip of the
//! authored collection properties.

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

const TRI: &str = r#"int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
"#;

/// Material bound to the mesh node named `mesh_node`.
fn material_of(scene: &Scene3D, mesh_node: &str) -> Option<String> {
    let mesh = scene
        .meshes
        .iter()
        .find(|m| m.name.as_deref() == Some(mesh_node))?;
    let mat = &scene.materials[mesh.primitives.first()?.material?.0 as usize];
    mat.name.clone()
}

#[test]
fn expand_prims_membership_binds_descendants() {
    // §15.2: expandPrims includes the specified prim and all its
    // descendants; the sibling outside the collection keeps the
    // direct fallback.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:rooms"]
) {{
    rel collection:rooms:includes = [</Root/Office>]
    rel material:binding:collection:rooms = [
        </Root.collection:rooms>,
        </Materials/Red>
    ]
    rel material:binding = </Materials/Grey>
    def Xform "Office" {{
        def Mesh "Desk" {{
            {TRI}        }}
    }}
    def Xform "Garden" {{
        def Mesh "Tree" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Grey", "(0.5, 0.5, 0.5)")])
    );
    let scene = decode(&usda);
    // Desk is a descendant of the included /Root/Office → collection
    // binding wins (rule 4 also: it beats the direct binding on the
    // same prim for members).
    assert_eq!(material_of(&scene, "Desk").as_deref(), Some("Red"));
    // Tree is not a member → the direct fallback applies.
    assert_eq!(material_of(&scene, "Tree").as_deref(), Some("Grey"));
}

#[test]
fn excludes_subtract_a_subtree() {
    // §15.2: excluded prims (and their descendants) drop out of the
    // included group.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:rooms"]
) {{
    rel collection:rooms:includes = [</Root/Office>]
    rel collection:rooms:excludes = [</Root/Office/Bookshelf>]
    rel material:binding:collection:rooms = [
        </Root.collection:rooms>,
        </Materials/Red>
    ]
    rel material:binding = </Materials/Grey>
    def Xform "Office" {{
        def Mesh "Desk" {{
            {TRI}        }}
        def Xform "Bookshelf" {{
            def Mesh "Shelf" {{
                {TRI}            }}
        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Grey", "(0.5, 0.5, 0.5)")])
    );
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "Desk").as_deref(), Some("Red"));
    assert_eq!(material_of(&scene, "Shelf").as_deref(), Some("Grey"));
}

#[test]
fn explicit_only_matches_exact_paths() {
    // §15.2 explicitOnly: only the literally listed paths are
    // members — descendants are not.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:picks"]
) {{
    uniform token collection:picks:expansionRule = "explicitOnly"
    rel collection:picks:includes = [</Root/Office/Desk>]
    rel material:binding:collection:picks = [
        </Root.collection:picks>,
        </Materials/Red>
    ]
    rel material:binding = </Materials/Grey>
    def Xform "Office" {{
        def Mesh "Desk" {{
            {TRI}        }}
        def Mesh "Chair" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Grey", "(0.5, 0.5, 0.5)")])
    );
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "Desk").as_deref(), Some("Red"));
    assert_eq!(material_of(&scene, "Chair").as_deref(), Some("Grey"));
}

#[test]
fn referenced_collection_members_are_included() {
    // §15.2: an include targeting another collection's property
    // path contributes that collection's members (recursively).
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:combo"]
) {{
    rel collection:combo:includes = [</Root/Other.collection:inner>]
    rel material:binding:collection:combo = [
        </Root.collection:combo>,
        </Materials/Red>
    ]
    def Xform "Other" (
        prepend apiSchemas = ["CollectionAPI:inner"]
    ) {{
        rel collection:inner:includes = [</Root/Extra>]
    }}
    def Xform "Extra" {{
        def Mesh "M" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "M").as_deref(), Some("Red"));
}

#[test]
fn include_root_covers_everything() {
    // §15.1.1.2: includeRoot includes the absolute root — every
    // prim — because relationships cannot target `/` directly.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:world"]
) {{
    uniform bool collection:world:includeRoot = 1
    rel material:binding:collection:world = [
        </Root.collection:world>,
        </Materials/Red>
    ]
    def Xform "Anywhere" {{
        def Mesh "M" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "M").as_deref(), Some("Red"));
}

#[test]
fn rule6_earlier_collection_wins_over_deeper_specificity() {
    // §3.4 rule 6 (the schema doc's Chair example): among collection
    // bindings on one prim, property order decides — the earlier
    // collection wins even though the later one names the prim more
    // specifically. Name-sorted property order per §11.3.2:
    // metalBits < plasticBits.
    let usda = format!(
        r#"#usda 1.0
def Xform "Chair" (
    prepend apiSchemas = ["CollectionAPI:metalBits", "CollectionAPI:plasticBits"]
) {{
    rel collection:metalBits:includes = [</Chair/Back>]
    rel collection:plasticBits:includes = [</Chair/Back/Brace/Rivet>]
    rel material:binding:collection:metalBits = [
        </Chair.collection:metalBits>,
        </Materials/Metal>
    ]
    rel material:binding:collection:plasticBits = [
        </Chair.collection:plasticBits>,
        </Materials/Plastic>
    ]
    def Xform "Back" {{
        def Xform "Brace" {{
            def Xform "Rivet" {{
                def Mesh "M" {{
                    {TRI}                }}
            }}
        }}
    }}
}}
{}"#,
        materials_block(&[("Metal", "(0.8, 0.8, 0.8)"), ("Plastic", "(0.1, 0.1, 0.9)")])
    );
    let scene = decode(&usda);
    // /Chair/Back/Brace/Rivet/M is a member of BOTH collections;
    // the earlier property (metalBits) wins.
    assert_eq!(material_of(&scene, "M").as_deref(), Some("Metal"));
}

#[test]
fn rule5_property_order_reorders_by_strongest_property_order() {
    // §11.3.2: the strongest authored `propertyOrder` reorders the
    // name-sorted properties — flipping the rule-6 outcome.
    let usda = format!(
        r#"#usda 1.0
def Xform "Chair" (
    prepend apiSchemas = ["CollectionAPI:metalBits", "CollectionAPI:plasticBits"]
    propertyOrder = ["material:binding:collection:plasticBits"]
) {{
    rel collection:metalBits:includes = [</Chair/Back>]
    rel collection:plasticBits:includes = [</Chair/Back/Brace/Rivet>]
    rel material:binding:collection:metalBits = [
        </Chair.collection:metalBits>,
        </Materials/Metal>
    ]
    rel material:binding:collection:plasticBits = [
        </Chair.collection:plasticBits>,
        </Materials/Plastic>
    ]
    def Xform "Back" {{
        def Xform "Brace" {{
            def Xform "Rivet" {{
                def Mesh "M" {{
                    {TRI}                }}
            }}
        }}
    }}
}}
{}"#,
        materials_block(&[("Metal", "(0.8, 0.8, 0.8)"), ("Plastic", "(0.1, 0.1, 0.9)")])
    );
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "M").as_deref(), Some("Plastic"));
}

#[test]
fn orphaned_excludes_are_inert() {
    // §15.2: an exclude that is not a subset of the included paths
    // is treated as inert.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:rooms"]
) {{
    rel collection:rooms:includes = [</Root/Office>]
    rel collection:rooms:excludes = [</Root/Garden>]
    rel material:binding:collection:rooms = [
        </Root.collection:rooms>,
        </Materials/Red>
    ]
    def Xform "Office" {{
        def Mesh "Desk" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "Desk").as_deref(), Some("Red"));
}

#[test]
fn collection_roundtrip_is_a_fixed_point() {
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:rooms"]
) {{
    uniform token collection:rooms:expansionRule = "expandPrims"
    rel collection:rooms:includes = [</Root/Office>]
    rel collection:rooms:excludes = [</Root/Office/Bookshelf>]
    rel material:binding:collection:rooms = [
        </Root.collection:rooms>,
        </Materials/Red>
    ]
    rel material:binding = </Materials/Grey>
    def Xform "Office" {{
        def Mesh "Desk" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)"), ("Grey", "(0.5, 0.5, 0.5)")])
    );
    let scene1 = decode(&usda);
    assert_eq!(material_of(&scene1, "Desk").as_deref(), Some("Red"));
    let out1 = UsdzEncoder::new().encode(&scene1).expect("encode 1");
    let text1 = default_layer_text(&out1);
    // Every authored collection property + the binding relationships
    // replay.
    assert!(
        text1.contains("rel collection:rooms:includes = [</Root/Office>]"),
        "{text1}"
    );
    assert!(
        text1.contains("rel collection:rooms:excludes = [</Root/Office/Bookshelf>]"),
        "{text1}"
    );
    assert!(
        text1.contains("uniform token collection:rooms:expansionRule = \"expandPrims\""),
        "{text1}"
    );
    assert!(
        text1.contains("material:binding:collection:rooms"),
        "{text1}"
    );
    let scene2 = UsdzDecoder::new().decode(&out1).expect("decode 2");
    assert_eq!(material_of(&scene2, "Desk").as_deref(), Some("Red"));
    let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
    assert_eq!(text1, default_layer_text(&out2), "fixed point");
}

#[test]
fn collection_include_cycle_terminates() {
    // Two collections including each other must not loop; neither
    // contributes prim members through the cycle.
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" (
    prepend apiSchemas = ["CollectionAPI:a", "CollectionAPI:b"]
) {{
    rel collection:a:includes = [</Root.collection:b>]
    rel collection:b:includes = [</Root.collection:a>, </Root/Office>]
    rel material:binding:collection:a = [
        </Root.collection:a>,
        </Materials/Red>
    ]
    def Xform "Office" {{
        def Mesh "Desk" {{
            {TRI}        }}
    }}
}}
{}"#,
        materials_block(&[("Red", "(1, 0, 0)")])
    );
    // a includes b, b includes a (cycle, ignored) and /Root/Office
    // (real member) → Desk resolves through a → b → Office.
    let scene = decode(&usda);
    assert_eq!(material_of(&scene, "Desk").as_deref(), Some("Red"));
}
