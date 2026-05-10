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
//! Decoder: surfaces an inner `xformOp:transform` (or TRS triple)
//! into the same extras slot.

mod common;

use oxideav_mesh3d::{Indices, Mesh, Node, Primitive, Scene3D, Topology};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

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

    // Round-trip: the decoder lifts the inner xformOp back into
    // Primitive::extras["usd:mesh_transform"]["matrix"].
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let m = &scene2.meshes[0];
    let v = m
        .primitives
        .first()
        .and_then(|p| p.extras.get("usd:mesh_transform"))
        .expect("mesh_transform extras present after round-trip");
    let rows = v
        .get("matrix")
        .and_then(|m| m.as_array())
        .expect("matrix array");
    assert_eq!(rows.len(), 4);
    let last_row = rows[3].as_array().unwrap();
    assert_eq!(last_row[0].as_f64().unwrap(), 10.0);
    assert_eq!(last_row[1].as_f64().unwrap(), 20.0);
    assert_eq!(last_row[2].as_f64().unwrap(), 30.0);
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
    let m = &scene.meshes[0];
    let v = m
        .primitives
        .first()
        .and_then(|p| p.extras.get("usd:mesh_transform"))
        .expect("inner mesh transform surfaced into extras");
    let rows = v.get("matrix").and_then(|m| m.as_array()).unwrap();
    let last = rows[3].as_array().unwrap();
    assert_eq!(last[0].as_f64().unwrap(), 4.0);
    assert_eq!(last[1].as_f64().unwrap(), 5.0);
    assert_eq!(last[2].as_f64().unwrap(), 6.0);
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
    let m = &scene2.meshes[0];
    let v = m
        .primitives
        .first()
        .and_then(|p| p.extras.get("usd:mesh_transform"))
        .expect("mesh_transform extras present after round-trip");
    let trs = v.get("trs").expect("trs sub-object");
    let t = trs.get("translation").and_then(|x| x.as_array()).unwrap();
    assert_eq!(t[0].as_f64().unwrap(), 1.0);
    assert_eq!(t[1].as_f64().unwrap(), 2.0);
    assert_eq!(t[2].as_f64().unwrap(), 3.0);
    let s = trs.get("scale").and_then(|x| x.as_array()).unwrap();
    assert_eq!(s[0].as_f64().unwrap(), 2.0);
}
