//! `usd:no_fold` extras flag — round-5 work item (b).
//!
//! When sibling Mesh prims share a stem (`Foo`, `Foo_1`) the
//! decoder normally folds them into a single Scene3D Mesh with
//! N primitives. Authoring tools sometimes produce intentional
//! sibling collisions where the user does NOT want this fold.
//! The `(usd:no_fold = 1)` prim metadata is the opt-out.
//!
//! Encoder symmetry: when a Scene3D primitive carries
//! `extras["usd:no_fold"] = true` the writer marks the emitted
//! `def Mesh` prim with the metadata flag so a re-decode honours
//! the opt-out.

mod common;

use oxideav_mesh3d::{Indices, Mesh, Node, Primitive, Scene3D, Topology};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn tri_at(offset: f32) -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![
        [offset + 0.0, 0.0, 0.0],
        [offset + 1.0, 0.0, 0.0],
        [offset + 0.0, 1.0, 0.0],
    ];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    p
}

/// Hand-authored USDA with two sibling Mesh prims sharing a stem
/// — both carry `usd:no_fold`. Decoder must NOT fold them.
#[test]
fn hand_authored_no_fold_keeps_siblings_separate() {
    let usda = r#"#usda 1.0
def Xform "Root" {
    def Mesh "Body" (usd:no_fold = 1) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Mesh "Body_1" (usd:no_fold = 1) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(10,0,0), (11,0,0), (10,1,0)]
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        2,
        "no_fold sibling Mesh prims must NOT fold; got {} meshes",
        scene.meshes.len()
    );
    // Both prims surface as separate Scene3D Meshes carrying the
    // hint on Primitive::extras for round-trip propagation.
    for m in &scene.meshes {
        assert_eq!(
            m.primitives[0]
                .extras
                .get("usd:no_fold")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}

/// `true` spelling of the boolean flag must also be honoured.
#[test]
fn no_fold_accepts_true_keyword_spelling() {
    let usda = r#"#usda 1.0
def Xform "Root" {
    def Mesh "Body" (usd:no_fold = true) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Mesh "Body_1" (usd:no_fold = true) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(10,0,0), (11,0,0), (10,1,0)]
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    assert_eq!(scene.meshes.len(), 2);
}

/// Encoder side: a multi-primitive Mesh whose primitives carry
/// `usd:no_fold` must emit the prim-metadata flag on every Mesh
/// prim. A re-decode then preserves the no-fold semantics.
#[test]
fn encoder_emits_no_fold_metadata_and_round_trips() {
    let mut scene = Scene3D::new();
    let mut p0 = tri_at(0.0);
    p0.extras
        .insert("usd:no_fold".into(), serde_json::Value::Bool(true));
    let mut p1 = tri_at(10.0);
    p1.extras
        .insert("usd:no_fold".into(), serde_json::Value::Bool(true));
    let mesh = Mesh::new(Some("Body".into()))
        .with_primitive(p0)
        .with_primitive(p1);
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(
        report.usda.matches("usd:no_fold = 1").count() >= 2,
        "writer must emit no_fold metadata on every prim that carries the flag; got:\n{}",
        report.usda
    );
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    assert_eq!(
        scene2.meshes.len(),
        2,
        "no_fold metadata must prevent re-fold on subsequent decode"
    );
}

/// Regression: a Mesh whose primitives have NO `usd:no_fold` flag
/// must still fold per the round-3 sibling-merge rule. Verifies
/// the new dispatch path doesn't accidentally always-skip the
/// fold heuristic.
#[test]
fn no_fold_absent_falls_back_to_fold() {
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("Body".into()))
        .with_primitive(tri_at(0.0))
        .with_primitive(tri_at(10.0));
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(!report.usda.contains("usd:no_fold"));
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    assert_eq!(
        scene2.meshes.len(),
        1,
        "absent no_fold flag must allow the round-3 fold to apply"
    );
    assert_eq!(scene2.meshes[0].primitives.len(), 2);
}
