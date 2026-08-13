//! `UsdGeomSubset` per-face material subsets (staged schema
//! `docs/3d/usd/usdgeom-usdshade-schema.md` Part 1): decode-side
//! split of material-bound face subsets into one typed `Primitive`
//! per bound material (§1.1/§1.2), the §1.3 unassigned-elements
//! remainder falling back to the parent binding, §1.4
//! familyType discovery-by-enumeration, verbatim preservation of
//! non-material subsets, out-of-range refusals, and the writer's
//! single-`def Mesh` + `def GeomSubset` re-emission with its
//! one-cycle round-trip fixed point.

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Indices, Mesh3DDecoder, Mesh3DEncoder, Scene3D};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn decode(usda: &str) -> Scene3D {
    let usdz = build_usdz(&[UsdzEntry {
        name: "layer.usda",
        payload: usda.as_bytes(),
    }]);
    UsdzDecoder::new().decode(&usdz).expect("decode")
}

fn encode(scene: &Scene3D) -> Vec<u8> {
    UsdzEncoder::new().encode(scene).expect("encode")
}

fn default_layer_text(usdz: &[u8]) -> String {
    // The encoder's default layer is the first entry; walk the
    // local file headers directly (STORED, so the payload follows
    // the header + name + extra verbatim).
    let name_len = u16::from_le_bytes([usdz[26], usdz[27]]) as usize;
    let extra_len = u16::from_le_bytes([usdz[28], usdz[29]]) as usize;
    let size = u32::from_le_bytes([usdz[18], usdz[19], usdz[20], usdz[21]]) as usize;
    let start = 30 + name_len + extra_len;
    String::from_utf8(usdz[start..start + size].to_vec()).expect("utf8 layer")
}

fn tri_count(p: &oxideav_mesh3d::Primitive) -> usize {
    match &p.indices {
        Some(Indices::U16(v)) => v.len() / 3,
        Some(Indices::U32(v)) => v.len() / 3,
        None => p.positions.len() / 3,
    }
}

fn flat_indices(p: &oxideav_mesh3d::Primitive) -> Vec<u32> {
    match &p.indices {
        Some(Indices::U16(v)) => v.iter().map(|&i| i as u32).collect(),
        Some(Indices::U32(v)) => v.clone(),
        None => (0..p.positions.len() as u32).collect(),
    }
}

/// Two-quad mesh: subset "front" claims face 0 → Red, subset
/// "back" claims face 1 → Green; nothing unassigned.
const TWO_QUAD_PARTITION: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)
def Xform "Root" {
    def Mesh "Plate" {
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 2, 3, 4, 5, 6, 7]
        point3f[] points = [(0,0,0), (1,0,0), (1,1,0), (0,1,0), (0,0,1), (1,0,1), (1,1,1), (0,1,1)]
        uniform token subsetFamily:materialBind:familyType = "partition"
        def GeomSubset "front" {
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [0]
            rel material:binding = </Materials/Red>
        }
        def GeomSubset "back" {
            uniform token elementType = "face"
            uniform token familyName = "materialBind"
            int[] indices = [1]
            rel material:binding = </Materials/Green>
        }
    }
}
def Scope "Materials" {
    def Material "Red" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 0, 0)
            token outputs:surface
        }
    }
    def Material "Green" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0, 1, 0)
            token outputs:surface
        }
    }
}
"#;

#[test]
fn material_face_subsets_split_into_primitives() {
    let scene = decode(TWO_QUAD_PARTITION);
    assert_eq!(scene.meshes.len(), 1);
    let mesh = &scene.meshes[0];
    // Base (unassigned remainder — empty here) + two subsets.
    assert_eq!(mesh.primitives.len(), 3);
    let base = &mesh.primitives[0];
    assert!(!base.extras.contains_key("usd:subset"));
    assert_eq!(tri_count(base), 0, "partition family leaves no remainder");
    let front = &mesh.primitives[1];
    let back = &mesh.primitives[2];
    // Face 0 (quad) fans into triangles (0,1,2) (0,2,3).
    assert_eq!(flat_indices(front), vec![0, 1, 2, 0, 2, 3]);
    assert_eq!(flat_indices(back), vec![4, 5, 6, 4, 6, 7]);
    // Vertex arrays are shared with the parent.
    assert_eq!(front.positions.len(), 8);
    assert_eq!(back.positions.len(), 8);
    // Each subset resolves its own material (§1.1).
    let front_mat = &scene.materials[front.material.expect("front bound").0 as usize];
    let back_mat = &scene.materials[back.material.expect("back bound").0 as usize];
    assert_eq!(front_mat.name.as_deref(), Some("Red"));
    assert_eq!(back_mat.name.as_deref(), Some("Green"));
    // Subset identity rides on the marker.
    let marker = front.extras.get("usd:subset").expect("marker");
    assert_eq!(marker.get("name").unwrap().as_str(), Some("front"));
    assert_eq!(
        marker.get("familyName").unwrap().as_str(),
        Some("materialBind")
    );
    // §1.4: the familyType property is discovered by enumeration
    // and preserved verbatim.
    let fams = base
        .extras
        .get("usd:subsetFamilies")
        .and_then(|v| v.as_array())
        .expect("familyType stash");
    assert_eq!(fams.len(), 1);
    assert_eq!(
        fams[0].get("name").unwrap().as_str(),
        Some("subsetFamily:materialBind:familyType")
    );
    assert_eq!(fams[0].get("value").unwrap().as_str(), Some("partition"));
    scene.validate().expect("subset scene validates");
}

#[test]
fn unassigned_faces_stay_on_parent_binding() {
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3, 3, 3]
    int[] faceVertexIndices = [0, 1, 2, 3, 4, 5, 6, 7, 8]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0), (2,0,0), (3,0,0), (2,1,0), (4,0,0), (5,0,0), (4,1,0)]
    rel material:binding = </Materials/Base>
    def GeomSubset "hot" {
        uniform token elementType = "face"
        uniform token familyName = "materialBind"
        int[] indices = [1]
        rel material:binding = </Materials/Hot>
    }
}
def Scope "Materials" {
    def Material "Base" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
            token outputs:surface
        }
    }
    def Material "Hot" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 0, 0)
            token outputs:surface
        }
    }
}
"#;
    let scene = decode(usda);
    let mesh = &scene.meshes[0];
    assert_eq!(mesh.primitives.len(), 2);
    let base = &mesh.primitives[0];
    let hot = &mesh.primitives[1];
    // §1.3 unassigned elements: faces 0 and 2 fall back to the
    // parent's own binding.
    assert_eq!(flat_indices(base), vec![0, 1, 2, 6, 7, 8]);
    assert_eq!(
        scene.materials[base.material.expect("parent binding").0 as usize]
            .name
            .as_deref(),
        Some("Base")
    );
    assert_eq!(flat_indices(hot), vec![3, 4, 5]);
    assert_eq!(
        scene.materials[hot.material.expect("subset binding").0 as usize]
            .name
            .as_deref(),
        Some("Hot")
    );
    scene.validate().expect("validates");
}

#[test]
fn overlapping_unrestricted_family_keeps_both_claims() {
    // §1.3: `unrestricted` places no constraint — a face may appear
    // in two subsets, and the declarative constraint tags are
    // assertions, not enforced invariants. Both subsets keep their
    // authored membership.
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3, 3]
    int[] faceVertexIndices = [0, 1, 2, 3, 4, 5]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0), (2,0,0), (3,0,0), (2,1,0)]
    def GeomSubset "a" {
        uniform token familyName = "overlay"
        int[] indices = [0, 1]
        rel material:binding = </Materials/A>
    }
    def GeomSubset "b" {
        uniform token familyName = "overlay"
        int[] indices = [1]
        rel material:binding = </Materials/B>
    }
}
def Scope "Materials" {
    def Material "A" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 1, 0)
            token outputs:surface
        }
    }
    def Material "B" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0, 1, 1)
            token outputs:surface
        }
    }
}
"#;
    let scene = decode(usda);
    let mesh = &scene.meshes[0];
    assert_eq!(mesh.primitives.len(), 3);
    // `elementType` was unauthored — the §1.2 fallback is `face`.
    assert_eq!(flat_indices(&mesh.primitives[1]), vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(flat_indices(&mesh.primitives[2]), vec![3, 4, 5]);
    // No face is unassigned (face 1 is double-claimed, face 0 single).
    assert_eq!(tri_count(&mesh.primitives[0]), 0);
    scene.validate().expect("validates");
}

#[test]
fn non_face_and_unbound_subsets_preserved_verbatim() {
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    def GeomSubset "corners" {
        uniform token elementType = "point"
        uniform token familyName = "pins"
        int[] indices = [0, 2]
    }
}
"#;
    let scene = decode(usda);
    let mesh = &scene.meshes[0];
    // No material face subset → no split; the subset is preserved.
    assert_eq!(mesh.primitives.len(), 1);
    let stash = mesh.primitives[0]
        .extras
        .get("usd:geomSubsets")
        .and_then(|v| v.as_array())
        .expect("verbatim stash");
    assert_eq!(stash.len(), 1);
    assert_eq!(stash[0].get("name").unwrap().as_str(), Some("corners"));

    // The writer replays it as an authored `def GeomSubset` child.
    let out = encode(&scene);
    let text = default_layer_text(&out);
    assert!(
        text.contains("def GeomSubset \"corners\""),
        "verbatim subset re-emitted: {text}"
    );
    assert!(text.contains("uniform token elementType = \"point\""));
    assert!(text.contains("int[] indices = [0, 2]"));

    // And the replay round-trips: decode the writer output again.
    let scene2 = UsdzDecoder::new().decode(&out).expect("second decode");
    let stash2 = scene2.meshes[0].primitives[0]
        .extras
        .get("usd:geomSubsets")
        .and_then(|v| v.as_array())
        .expect("verbatim stash survives");
    assert_eq!(stash2.len(), 1);
}

#[test]
fn out_of_range_face_index_refuses() {
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    def GeomSubset "bad" {
        int[] indices = [7]
        rel material:binding = </Materials/A>
    }
}
def Scope "Materials" {
    def Material "A" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 0, 0)
            token outputs:surface
        }
    }
}
"#;
    let usdz = build_usdz(&[UsdzEntry {
        name: "layer.usda",
        payload: usda.as_bytes(),
    }]);
    let err = UsdzDecoder::new().decode(&usdz).expect_err("refuses");
    let msg = format!("{err}");
    assert!(
        msg.contains("out of range") && msg.contains("bad"),
        "precise diagnostic, got: {msg}"
    );
}

#[test]
fn writer_emits_single_mesh_with_subset_children() {
    let scene = decode(TWO_QUAD_PARTITION);
    let out = encode(&scene);
    let text = default_layer_text(&out);
    // ONE def Mesh (not sibling `Plate_1` prims).
    assert_eq!(text.matches("def Mesh ").count(), 1, "{text}");
    assert!(!text.contains("def Mesh \"Plate_1\""));
    assert!(text.contains("def GeomSubset \"front\""));
    assert!(text.contains("def GeomSubset \"back\""));
    // §1.4 familyType property replays verbatim on the parent prim.
    assert!(
        text.contains("uniform token subsetFamily:materialBind:familyType = \"partition\""),
        "{text}"
    );
    // Contiguous face runs over the concatenated (all-triangle)
    // topology: the base is empty and each authored quad fanned
    // into two triangles, so front = faces 0-1, back = faces 2-3.
    assert!(text.contains("int[] indices = [0, 1]"), "{text}");
    assert!(text.contains("int[] indices = [2, 3]"), "{text}");
    // Subset-scoped bindings.
    assert!(text.contains("rel material:binding = </Materials/Red>"));
    assert!(text.contains("rel material:binding = </Materials/Green>"));
}

#[test]
fn subset_roundtrip_is_a_fixed_point() {
    for src in [TWO_QUAD_PARTITION] {
        let scene1 = decode(src);
        let out1 = encode(&scene1);
        let scene2 = UsdzDecoder::new().decode(&out1).expect("decode 2");
        let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
        assert_eq!(
            default_layer_text(&out1),
            default_layer_text(&out2),
            "encode → decode → encode must be a fixed point"
        );
        // Typed-model equivalence across the cycle.
        assert_eq!(scene1.meshes.len(), scene2.meshes.len());
        let m1 = &scene1.meshes[0];
        let m2 = &scene2.meshes[0];
        assert_eq!(m1.primitives.len(), m2.primitives.len());
        for (p1, p2) in m1.primitives.iter().zip(&m2.primitives) {
            assert_eq!(flat_indices(p1), flat_indices(p2));
            assert_eq!(p1.positions, p2.positions);
            assert_eq!(p1.material.is_some(), p2.material.is_some());
            assert_eq!(p1.extras.get("usd:subset"), p2.extras.get("usd:subset"));
        }
        scene2.validate().expect("cycle output validates");
    }
}

#[test]
fn remainder_roundtrip_is_a_fixed_point() {
    // A mesh with a real remainder + parent binding: the emitted
    // topology reorders into [remainder | subset] once, then holds.
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3, 3, 3]
    int[] faceVertexIndices = [0, 1, 2, 3, 4, 5, 6, 7, 8]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0), (2,0,0), (3,0,0), (2,1,0), (4,0,0), (5,0,0), (4,1,0)]
    rel material:binding = </Materials/Base>
    def GeomSubset "hot" {
        uniform token familyName = "materialBind"
        int[] indices = [1]
        rel material:binding = </Materials/Hot>
    }
}
def Scope "Materials" {
    def Material "Base" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
            token outputs:surface
        }
    }
    def Material "Hot" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 0, 0)
            token outputs:surface
        }
    }
}
"#;
    let scene1 = decode(usda);
    let out1 = encode(&scene1);
    let scene2 = UsdzDecoder::new().decode(&out1).expect("decode 2");
    let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
    assert_eq!(default_layer_text(&out1), default_layer_text(&out2));
    // The subset's faces stay bound to Hot across the cycle.
    let hot2 = scene2.meshes[0]
        .primitives
        .iter()
        .find(|p| p.extras.contains_key("usd:subset"))
        .expect("subset primitive survives");
    assert_eq!(
        scene2.materials[hot2.material.unwrap().0 as usize]
            .name
            .as_deref(),
        Some("Hot")
    );
    assert_eq!(tri_count(hot2), 1);
}

#[test]
fn subset_extra_opinions_ride_the_rest_slot() {
    // Unconsumed attrs on a subset prim (here a custom token and a
    // prim metadata note) survive the split and re-emit.
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    def GeomSubset "tagged" (
        customLabel = "keep-me"
    ) {
        uniform token familyName = "materialBind"
        int[] indices = [0]
        rel material:binding = </Materials/A>
        uniform token myapp:zone = "hazard"
    }
}
def Scope "Materials" {
    def Material "A" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 0, 0)
            token outputs:surface
        }
    }
}
"#;
    let scene = decode(usda);
    let out = encode(&scene);
    let text = default_layer_text(&out);
    assert!(
        text.contains("uniform token myapp:zone = \"hazard\""),
        "extra subset attr re-emitted: {text}"
    );
    assert!(
        text.contains("customLabel = \"keep-me\""),
        "subset prim metadata re-emitted: {text}"
    );
    // Fixed point.
    let scene2 = UsdzDecoder::new().decode(&out).expect("decode 2");
    let out2 = UsdzEncoder::new().encode(&scene2).expect("encode 2");
    assert_eq!(text, default_layer_text(&out2));
}

#[test]
fn double_sided_mesh_propagates_to_subset_materials() {
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    uniform bool doubleSided = 1
    def GeomSubset "s" {
        int[] indices = [0]
        rel material:binding = </Materials/A>
    }
}
def Scope "Materials" {
    def Material "A" {
        def Shader "PBRShader" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (1, 0, 0)
            token outputs:surface
        }
    }
}
"#;
    let scene = decode(usda);
    let sub = scene.meshes[0]
        .primitives
        .iter()
        .find(|p| p.extras.contains_key("usd:subset"))
        .expect("subset primitive");
    assert!(
        scene.materials[sub.material.unwrap().0 as usize].double_sided,
        "gprim-wide doubleSided reaches the subset-bound material"
    );
}
