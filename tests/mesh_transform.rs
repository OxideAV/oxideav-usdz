//! Per-Mesh transform on the inner `def Mesh` prim — round-5 work
//! item (c).
//!
//! USD's UsdGeomXformable schema is inherited by `UsdGeomMesh`, so
//! a Mesh prim can carry its own `xformOp:*` opinions independent
//! of its parent Xform. Authoring tools sometimes use this to
//! attach a per-mesh frame without disturbing the surrounding
//! scene-graph transforms.
//!
//! The round-5 mapping uses
//! `Primitive::extras["usd:mesh_transform"]` to carry the
//! per-Mesh transform across the typed model (since
//! `Mesh` doesn't have a Transform field of its own).
//!
//! Encoder: when a Primitive carries the extras entry, the writer
//! emits the matching `xformOp:*` opinions on the inner `def Mesh`
//! itself (not the parent Xform).
//!
//! Decoder: an inner `xformOp:transform` (or TRS triple) lands on
//! the typed model's one transform slot — the mesh-carrier node's
//! `Transform` — never a second time on the extras (a double
//! recording used to apply the transform twice and grow the tree by
//! one Xform per encode → decode cycle). The writer collapses such a
//! carrier back onto the `def Mesh` with the transform inside, so
//! the round trip is a fixed point.

mod common;

use oxideav_mesh3d::{Indices, Mesh, Node, Primitive, Scene3D, Topology, Transform};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

/// Translation of a node transform in either representation (the
/// decoder stores USD's row-vector matrix transposed, so the
/// translation sits in the last column).
fn translation_of(t: &Transform) -> [f32; 3] {
    match t {
        Transform::Trs { translation, .. } => *translation,
        Transform::Matrix(m) => [m[0][3], m[1][3], m[2][3]],
    }
}

/// The mesh-carrier node named `name`.
fn carrier<'a>(scene: &'a Scene3D, name: &str) -> &'a Node {
    scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some(name) && n.mesh.is_some())
        .expect("mesh-carrier node")
}

fn tri() -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    p
}

fn matrix_extras() -> serde_json::Value {
    // Translate by (10, 20, 30) — non-identity 4x4.
    serde_json::json!({
        "matrix": [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [10.0, 20.0, 30.0, 1.0]
        ]
    })
}

#[test]
fn matrix_mesh_transform_round_trips_via_extras() {
    let mut scene = Scene3D::new();
    let mut p = tri();
    p.extras
        .insert("usd:mesh_transform".into(), matrix_extras());
    let mesh = Mesh::new(Some("Body".into())).with_primitive(p);
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    // The xformOp:transform must be emitted INSIDE the def Mesh
    // brace (not on the enclosing Xform). We assert by looking
    // for the matrix4d opinion appearing AFTER the Mesh header.
    let mesh_idx = report
        .usda
        .find("def Mesh \"Body\"")
        .expect("Mesh prim emitted");
    let after_mesh = &report.usda[mesh_idx..];
    assert!(
        after_mesh.contains("matrix4d xformOp:transform"),
        "inner-Mesh transform must emit xformOp:transform after the Mesh header; got:\n{}",
        after_mesh
    );
    assert!(
        after_mesh.contains("xformOpOrder = [\"xformOp:transform\"]"),
        "inner-Mesh transform must emit xformOpOrder; got:\n{}",
        after_mesh
    );

    // Round-trip: the decoder lifts the inner xformOp onto the
    // carrier node's transform (once), and the extras slot is not
    // re-created — the typed model has exactly one transform.
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let body = carrier(&scene2, "Body");
    assert_eq!(translation_of(&body.transform), [10.0, 20.0, 30.0]);
    assert!(
        !scene2.meshes[0].primitives[0]
            .extras
            .contains_key("usd:mesh_transform"),
        "no second copy of the transform on the primitive"
    );
    // Second encode: the carrier collapses back onto the def Mesh
    // with the transform inside — a fixed point, no extra Xform.
    let report2 = UsdzEncoder::new()
        .encode_with_report(&scene2)
        .expect("encode");
    assert_eq!(
        report2.usda.matches("def Xform").count(),
        1,
        "{}",
        report2.usda
    );
    assert_eq!(
        report2.usda.matches("xformOp:transform").count(),
        2,
        "{}",
        report2.usda
    );
    let scene3 = UsdzDecoder::new()
        .decode_bytes(&report2.bytes)
        .expect("decode");
    assert_eq!(
        translation_of(&carrier(&scene3, "Body").transform),
        [10.0, 20.0, 30.0]
    );
    assert_eq!(scene3.nodes.len(), scene2.nodes.len());
    assert_eq!(
        UsdzEncoder::new().encode_with_report(&scene3).unwrap().usda,
        report2.usda
    );
}

#[test]
fn no_mesh_transform_extras_emits_no_inner_xformop() {
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("Body".into())).with_primitive(tri());
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    // Without the extras flag the writer must fall back to the
    // pre-r5 behaviour: only the parent Xform carries xformOps
    // (and an identity Xform emits none either).
    let mesh_idx = report
        .usda
        .find("def Mesh \"Body\"")
        .expect("Mesh prim emitted");
    let after_mesh = &report.usda[mesh_idx..];
    let close_brace = after_mesh.find("}").unwrap();
    let mesh_body = &after_mesh[..close_brace];
    assert!(
        !mesh_body.contains("xformOp:"),
        "inner Mesh must not emit any xformOp without the extras hint; got:\n{}",
        mesh_body
    );
}

#[test]
fn decoder_lifts_hand_authored_inner_mesh_xformop() {
    // Hand-authored USD with a Mesh-level xformOp:transform —
    // verifies the decoder picks it up even when the encoder
    // wasn't in the loop.
    let usda = r#"#usda 1.0
def Xform "Root" {
    def Mesh "Body" {
        matrix4d xformOp:transform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (4, 5, 6, 1))
        uniform token[] xformOpOrder = ["xformOp:transform"]
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    let body = carrier(&scene, "Body");
    assert_eq!(translation_of(&body.transform), [4.0, 5.0, 6.0]);
    assert!(!scene.meshes[0].primitives[0]
        .extras
        .contains_key("usd:mesh_transform"));
    // And the shape is a fixed point: def Xform "Root" > def Mesh
    // "Body" with the transform inside, cycle after cycle.
    let r1 = UsdzEncoder::new().encode_with_report(&scene).unwrap();
    assert_eq!(r1.usda.matches("def Xform").count(), 1, "{}", r1.usda);
    let s2 = UsdzDecoder::new().decode_bytes(&r1.bytes).unwrap();
    let r2 = UsdzEncoder::new().encode_with_report(&s2).unwrap();
    assert_eq!(r1.usda, r2.usda);
}

#[test]
fn trs_mesh_transform_round_trips_via_extras() {
    // TRS-shaped extras → encoder emits translate / orient /
    // scale xformOp:* on the inner Mesh; decoder reads them back
    // into a `usd:mesh_transform.trs` blob (matrix path is also
    // accepted on encode).
    let mut scene = Scene3D::new();
    let mut p = tri();
    p.extras.insert(
        "usd:mesh_transform".into(),
        serde_json::json!({
            "trs": {
                "translation": [1.0, 2.0, 3.0],
                // identity quaternion in xyzw.
                "rotation": [0.0, 0.0, 0.0, 1.0],
                "scale": [2.0, 2.0, 2.0]
            }
        }),
    );
    let mesh = Mesh::new(Some("Body".into())).with_primitive(p);
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    let mesh_idx = report
        .usda
        .find("def Mesh \"Body\"")
        .expect("Mesh prim emitted");
    let after_mesh = &report.usda[mesh_idx..];
    assert!(after_mesh.contains("xformOp:translate = (1, 2, 3)"));
    assert!(after_mesh.contains("xformOp:scale = (2, 2, 2)"));

    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let body = carrier(&scene2, "Body");
    let Transform::Trs {
        translation, scale, ..
    } = body.transform
    else {
        panic!("TRS carrier transform, got {:?}", body.transform);
    };
    assert_eq!(translation, [1.0, 2.0, 3.0]);
    assert_eq!(scale, [2.0, 2.0, 2.0]);
    let t = [
        translation[0] as f64,
        translation[1] as f64,
        translation[2] as f64,
    ];
    assert_eq!(t[0], 1.0);
    let s = [scale[0] as f64, scale[1] as f64, scale[2] as f64];
    assert_eq!(s[0], 2.0);
    assert_eq!(t[2], 3.0);
}
