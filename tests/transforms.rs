//! Per-node transform serialisation roundtrip — verify that a
//! `Scene3D` whose root nodes carry non-identity
//! `Transform::Trs` / `Transform::Matrix` values survives a
//! USDZ → `Scene3D` → USDZ → `Scene3D` cycle with the transform
//! recovered.
//!
//! The decoder side accepts the standard UsdGeomXformable opinion
//! sets emitted by the writer:
//!
//! * `xformOp:translate` + `xformOp:orient` + `xformOp:scale`
//!   driven by `xformOpOrder = [translate, orient, scale]`
//!   → `Transform::Trs`.
//! * `xformOp:transform` driven by
//!   `xformOpOrder = [transform]` → `Transform::Matrix`.
//!
//! Identity transforms emit no opinions (matching r1/r2 output)
//! and decode to `Transform::identity()`.

mod common;

use oxideav_mesh3d::{Mesh, Node, Primitive, Scene3D, Topology, Transform};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn unit_tri() -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    p.indices = Some(oxideav_mesh3d::Indices::U16(vec![0, 1, 2]));
    p
}

fn scene_with_node_transform(t: Transform) -> Scene3D {
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("M".into())).with_primitive(unit_tri());
    let mid = scene.add_mesh(mesh);
    let node = Node::new()
        .with_name("Root")
        .with_mesh(mid)
        .with_transform(t);
    let id = scene.add_node(node);
    scene.add_root(id);
    scene
}

/// Locate a freshly-decoded node by name — the encoder produces
/// `Xform "Root" { def Mesh "M" {...} }` so `Scene3D::nodes` ends
/// up with both, in child-first order. Test asserts on the Root
/// transform regardless of its arena index.
fn find_node_by_name<'a>(scene: &'a Scene3D, name: &str) -> &'a Node {
    scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no node named `{name}`"))
}

#[test]
fn trs_translate_only_roundtrips() {
    let t = Transform::Trs {
        translation: [3.0, -1.5, 2.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0, 1.0, 1.0],
    };
    let scene = scene_with_node_transform(t);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode");
    let scene2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode");
    let recovered = find_node_by_name(&scene2, "Root").transform;
    let Transform::Trs {
        translation,
        rotation,
        scale,
    } = recovered
    else {
        panic!("expected Trs after roundtrip, got {recovered:?}");
    };
    assert_eq!(translation, [3.0, -1.5, 2.0]);
    assert_eq!(rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(scale, [1.0, 1.0, 1.0]);
}

#[test]
fn trs_full_roundtrips() {
    // 90° rotation about Y → quat (xyzw) = (0, sin(45°), 0, cos(45°)).
    let s2 = (std::f32::consts::FRAC_PI_4).sin();
    let c2 = (std::f32::consts::FRAC_PI_4).cos();
    let t = Transform::Trs {
        translation: [10.0, 20.0, 30.0],
        rotation: [0.0, s2, 0.0, c2],
        scale: [2.0, 0.5, 4.0],
    };
    let scene = scene_with_node_transform(t);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode");
    let scene2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode");
    let Transform::Trs {
        translation,
        rotation,
        scale,
    } = find_node_by_name(&scene2, "Root").transform
    else {
        panic!("expected Trs");
    };
    assert_eq!(translation, [10.0, 20.0, 30.0]);
    // Quaternion roundtrip — exact since float text formatting is
    // canonical and we use 6-digit precision.
    let approx_eq = |a: f32, b: f32| (a - b).abs() < 1e-5;
    assert!(approx_eq(rotation[0], 0.0));
    assert!(approx_eq(rotation[1], s2));
    assert!(approx_eq(rotation[2], 0.0));
    assert!(approx_eq(rotation[3], c2));
    assert_eq!(scale, [2.0, 0.5, 4.0]);
}

#[test]
fn matrix_transform_roundtrips() {
    let m = [
        [1.0, 0.0, 0.0, 5.0],
        [0.0, 2.0, 0.0, 6.0],
        [0.0, 0.0, 3.0, 7.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let scene = scene_with_node_transform(Transform::Matrix(m));
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode");
    let scene2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode");
    let recovered = find_node_by_name(&scene2, "Root").transform;
    let Transform::Matrix(out) = recovered else {
        panic!("expected Matrix, got {recovered:?}");
    };
    assert_eq!(out, m);
}

#[test]
fn identity_transform_emits_no_xformop() {
    // Sanity: when the node carries Transform::identity() we must
    // NOT emit xformOp opinions (keeps the writer output minimal
    // and avoids regressing r1/r2 test assertions).
    let scene = scene_with_node_transform(Transform::identity());
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(
        !report.usda.contains("xformOp:"),
        "identity transform should not emit any xformOp opinions; got:\n{}",
        report.usda
    );
    assert!(
        !report.usda.contains("xformOpOrder"),
        "identity transform should not emit xformOpOrder; got:\n{}",
        report.usda
    );
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    let Transform::Trs {
        translation,
        rotation,
        scale,
    } = find_node_by_name(&scene2, "Root").transform
    else {
        panic!("expected identity Trs");
    };
    assert_eq!(translation, [0.0, 0.0, 0.0]);
    assert_eq!(rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(scale, [1.0, 1.0, 1.0]);
}

/// Sanity-check that the writer emits the spec'd attribute names
/// (renderers grep for these literally) by inspecting the USDA
/// text directly. The decoder roundtrip tests above already cover
/// the value side.
#[test]
fn writer_emits_canonical_attribute_names() {
    let t = Transform::Trs {
        translation: [1.0, 2.0, 3.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0, 1.0, 1.0],
    };
    let scene = scene_with_node_transform(t);
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report
        .usda
        .contains("double3 xformOp:translate = (1, 2, 3)"));
    assert!(report.usda.contains("quatf xformOp:orient = "));
    assert!(report.usda.contains("float3 xformOp:scale = (1, 1, 1)"));
    assert!(report
        .usda
        .contains("uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:orient\", \"xformOp:scale\"]"));
}

/// Decoder accepts a hand-authored Matrix opinion (no writer in the
/// loop). Validates that the `read_matrix4` path handles real-world
/// USDA-formatted matrix4d literals.
#[test]
fn decoder_parses_hand_authored_matrix() {
    let usda = r#"#usda 1.0
def Xform "Root" {
    matrix4d xformOp:transform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (4, 5, 6, 1))
    uniform token[] xformOpOrder = ["xformOp:transform"]
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    let Transform::Matrix(m) = scene.nodes[0].transform else {
        panic!("expected matrix");
    };
    // USD's row-vector literal carries the translation in its last
    // row; the typed column-vector convention transposes it into the
    // last column.
    assert_eq!(
        [m[0][3], m[1][3], m[2][3]],
        [4.0, 5.0, 6.0],
        "translation in the last column"
    );
    assert_eq!(m[3], [0.0, 0.0, 0.0, 1.0], "affine last row");
}
