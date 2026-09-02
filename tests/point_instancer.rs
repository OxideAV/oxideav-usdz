//! `UsdGeomPointInstancer` (staged schema Part 2) on the typed model,
//! both directions, plus the expansion helper.
//!
//! * decode maps `prototypes` / `protoIndices` / `positions` /
//!   `orientations` / `scales` / `ids` / `invisibleIds` / velocities
//!   (defaults and `.timeSamples`) plus the `inactiveIds` metadata
//!   onto `Node::extras["usd:pointInstancer"]`; prototypes are
//!   ordinary child nodes;
//! * the writer re-emits the prim symmetrically — the round trip is
//!   a one-cycle fixed point on the USDA text;
//! * `point_instancer::expand` lifts the instancer onto plain nodes
//!   in the §2.3 order with the §2.4 mask applied;
//! * where `usdcat` / `usdchecker` are installed, the emitted
//!   package is black-box validated.

mod common;

use std::process::Command;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Scene3D, Transform};
use oxideav_usdz::point_instancer::{self, ExpandOptions, PointInstancer};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const INSTANCER: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    timeCodesPerSecond = 24
)

def Xform "World"
{
    def PointInstancer "Scatter" (
        inactiveIds = [102]
    )
    {
        double3 xformOp:translate = (0, 5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel prototypes = [</World/Scatter/Prototypes/Tri>, </World/Scatter/Prototypes/Empty>]
        int[] protoIndices = [0, 1, 0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0, 0)]
        point3f[] positions.timeSamples = {
            0: [(0, 0, 0), (1, 0, 0), (2, 0, 0), (3, 0, 0)],
            24: [(0, 10, 0), (1, 10, 0), (2, 10, 0), (3, 10, 0)]
        }
        quath[] orientations = [(1, 0, 0, 0), (0, 0, 1, 0), (1, 0, 0, 0), (1, 0, 0, 0)]
        float3[] scales = [(1, 1, 1), (2, 2, 2), (0.5, 0.5, 0.5), (1, 1, 1)]
        int64[] ids = [100, 101, 102, 103]
        int64[] invisibleIds = [103]
        vector3f[] velocities = [(0, 24, 0), (0, 24, 0), (0, 24, 0), (0, 24, 0)]

        def Scope "Prototypes"
        {
            def Xform "Tri"
            {
                double3 xformOp:translate = (0, 0, 7)
                uniform token[] xformOpOrder = ["xformOp:translate"]
                def Mesh "Geom"
                {
                    int[] faceVertexCounts = [3]
                    int[] faceVertexIndices = [0, 1, 2]
                    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
                }
            }
            def Xform "Empty"
            {
            }
        }
    }
}
"#;

fn package(usda: &str) -> Vec<u8> {
    build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }])
}

fn decode(bytes: &[u8]) -> Scene3D {
    UsdzDecoder::new().decode(bytes).expect("decode")
}

fn usda_of(scene: &Scene3D) -> String {
    UsdzEncoder::new()
        .encode_with_report(scene)
        .expect("encode")
        .usda
}

fn instancer_node(scene: &Scene3D) -> (oxideav_mesh3d::NodeId, PointInstancer) {
    let (idx, node) = scene
        .nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.name.as_deref() == Some("Scatter"))
        .expect("Scatter node");
    let record = PointInstancer::from_node(node).expect("usd:pointInstancer record");
    (oxideav_mesh3d::NodeId(idx as u32), record)
}

#[test]
fn decodes_every_array_and_prototypes_stay_children() {
    let scene = decode(&package(INSTANCER));
    let (id, pi) = instancer_node(&scene);
    assert_eq!(
        pi.prototypes,
        vec![
            "/World/Scatter/Prototypes/Tri".to_string(),
            "/World/Scatter/Prototypes/Empty".to_string()
        ]
    );
    assert_eq!(pi.instance_count(None), 4);
    assert_eq!(
        pi.proto_indices.default.as_deref(),
        Some(&[0u32, 1, 0, 0][..])
    );
    assert_eq!(pi.positions.samples.len(), 2);
    assert_eq!(pi.positions.samples[1].0, 24.0);
    assert_eq!(
        pi.orientations.default.as_ref().unwrap()[1],
        [0.0, 1.0, 0.0, 0.0]
    );
    assert_eq!(pi.scales.default.as_ref().unwrap()[2], [0.5, 0.5, 0.5]);
    assert_eq!(
        pi.ids.default.as_deref(),
        Some(&[100i64, 101, 102, 103][..])
    );
    assert_eq!(pi.invisible_ids.default.as_deref(), Some(&[103i64][..]));
    assert_eq!(pi.inactive_ids, vec![102]);
    assert_eq!(pi.velocities.default.as_ref().unwrap()[0], [0.0, 24.0, 0.0]);
    // Prototype subtree is an ordinary child (Scope → Xform → mesh).
    let node = scene.node(id).unwrap();
    assert_eq!(
        node.extras.get("usd:type").and_then(|v| v.as_str()),
        Some("PointInstancer")
    );
    assert_eq!(node.children.len(), 1);
    assert_eq!(scene.meshes.len(), 1, "the prototype mesh is decoded once");
    assert!(
        matches!(node.transform, Transform::Trs { translation, .. } if translation == [0.0, 5.0, 0.0])
    );
}

#[test]
fn writer_round_trip_is_a_fixed_point() {
    let scene = decode(&package(INSTANCER));
    let usda1 = usda_of(&scene);
    assert!(usda1.contains("def PointInstancer \"Scatter\""), "{usda1}");
    assert!(
        usda1.contains(
            "rel prototypes = [</World/Scatter/Prototypes/Tri>, </World/Scatter/Prototypes/Empty>]"
        ),
        "{usda1}"
    );
    assert!(
        usda1.contains("int[] protoIndices = [0, 1, 0, 0]"),
        "{usda1}"
    );
    assert!(
        usda1.contains(
            "quath[] orientations = [(1, 0, 0, 0), (0, 0, 1, 0), (1, 0, 0, 0), (1, 0, 0, 0)]"
        ),
        "{usda1}"
    );
    assert!(
        usda1.contains("point3f[] positions.timeSamples = { 0: "),
        "{usda1}"
    );
    assert!(usda1.contains("inactiveIds = [102]"), "{usda1}");
    let bytes = UsdzEncoder::new().encode_bytes(&scene).unwrap();
    let s2 = decode(&bytes);
    let (_, p1) = instancer_node(&scene);
    let (_, p2) = instancer_node(&s2);
    assert_eq!(p1, p2, "typed record survives the round trip");
    assert_eq!(usda_of(&s2), usda1, "second encode is byte-identical");
    assert_eq!(s2.meshes.len(), 1);
}

#[test]
fn expand_applies_order_and_mask() {
    let mut scene = decode(&package(INSTANCER));
    let (id, _) = instancer_node(&scene);
    let meshes_before = scene.meshes.len();
    let new_ids =
        point_instancer::expand(&mut scene, id, ExpandOptions::default()).expect("expand");
    // 4 instances: id 102 inactive, id 103 invisible → 2 remain.
    assert_eq!(new_ids.len(), 2);
    let node = scene.node(id).unwrap();
    // Prototype root detached; the two instances attached; record gone.
    assert_eq!(node.children, new_ids);
    assert!(PointInstancer::from_node(node).is_none());
    assert!(!node.extras.contains_key("usd:type"));
    let inst0 = scene.node(new_ids[0]).unwrap();
    assert_eq!(inst0.name.as_deref(), Some("Scatter_inst0"));
    assert!(
        matches!(inst0.transform, Transform::Trs { translation, scale, .. }
        if translation == [0.0, 0.0, 0.0] && scale == [1.0, 1.0, 1.0])
    );
    let inst1 = scene.node(new_ids[1]).unwrap();
    assert_eq!(inst1.name.as_deref(), Some("Scatter_inst1"));
    assert!(
        matches!(inst1.transform, Transform::Trs { translation, rotation, scale }
        if translation == [1.0, 0.0, 0.0] && rotation == [0.0, 1.0, 0.0, 0.0] && scale == [2.0, 2.0, 2.0])
    );
    // Instance 0 wraps a copy of the Tri prototype (its own root
    // transform kept — §2.3 step 1), instance 1 the Empty one.
    let proto0 = scene.node(inst0.children[0]).unwrap();
    assert_eq!(proto0.name.as_deref(), Some("Tri"));
    assert!(
        matches!(proto0.transform, Transform::Trs { translation, .. } if translation == [0.0, 0.0, 7.0])
    );
    let geom = scene.node(proto0.children[0]).unwrap();
    assert!(geom.mesh.is_some());
    assert_eq!(
        scene.meshes.len(),
        meshes_before,
        "geometry is shared, not duplicated"
    );
    assert_eq!(inst0.extras["usd:instance"]["id"].as_i64(), Some(100));
    // Expanded scene encodes as plain Xforms and re-decodes with the
    // instance nodes intact.
    let usda = usda_of(&scene);
    assert!(!usda.contains("PointInstancer"), "{usda}");
    assert!(usda.contains("def Xform \"Scatter_inst1\""), "{usda}");
    let s2 = decode(&UsdzEncoder::new().encode_bytes(&scene).unwrap());
    assert!(s2
        .nodes
        .iter()
        .any(|n| n.name.as_deref() == Some("Scatter_inst0")));
}

#[test]
fn expand_at_time_uses_velocity_path() {
    let mut scene = decode(&package(INSTANCER));
    let (id, _) = instancer_node(&scene);
    // t = 12 timeCodes = 0.5 s at 24 tcps: velocities (0, 24, 0)
    // from the left sample (t=0) → y = 12, not the interpolated 5.
    let ids = point_instancer::expand(
        &mut scene,
        id,
        ExpandOptions {
            time: Some(12.0),
            time_codes_per_second: 24.0,
        },
    )
    .unwrap();
    let inst0 = scene.node(ids[0]).unwrap();
    assert!(
        matches!(inst0.transform, Transform::Trs { translation, .. }
        if (translation[1] - 12.0).abs() < 1e-5),
        "{:?}",
        inst0.transform
    );
}

#[test]
fn expand_rejects_unknown_prototype_and_out_of_range_index() {
    let bad_proto = INSTANCER.replace("</World/Scatter/Prototypes/Empty>", "</Nowhere>");
    let mut scene = decode(&package(&bad_proto));
    let (id, _) = instancer_node(&scene);
    let err = point_instancer::expand(&mut scene, id, ExpandOptions::default()).unwrap_err();
    assert!(err.to_string().contains("/Nowhere"), "{err}");

    let bad_index = INSTANCER.replace(
        "int[] protoIndices = [0, 1, 0, 0]",
        "int[] protoIndices = [0, 7, 0, 0]",
    );
    let mut scene = decode(&package(&bad_index));
    let (id, _) = instancer_node(&scene);
    let err = point_instancer::expand(&mut scene, id, ExpandOptions::default()).unwrap_err();
    assert!(err.to_string().contains("protoIndices"), "{err}");
}

#[test]
fn malformed_arrays_are_decode_errors() {
    let short = INSTANCER.replace(
        "float3[] scales = [(1, 1, 1), (2, 2, 2), (0.5, 0.5, 0.5), (1, 1, 1)]",
        "float3[] scales = [(1, 1, 1)]",
    );
    let err = UsdzDecoder::new().decode(&package(&short)).unwrap_err();
    assert!(err.to_string().contains("scales"), "{err}");
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn black_box_validators_accept_the_written_instancer() {
    if !tool_available("usdcat") || !tool_available("usdchecker") {
        eprintln!("usdcat / usdchecker not installed; skipping black-box validation");
        return;
    }
    let scene = decode(&package(INSTANCER));
    let bytes = UsdzEncoder::new().encode_bytes(&scene).unwrap();
    let dir = std::env::temp_dir().join(format!("oxideav-usdz-pi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scatter.usdz");
    std::fs::write(&path, &bytes).unwrap();
    let check = Command::new("usdchecker").arg(&path).output().unwrap();
    let check_text = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    // `usdcat --flatten` re-serialises the composed stage: the
    // instancer arrays must survive the external reader verbatim.
    let cat = Command::new("usdcat")
        .arg("--flatten")
        .arg(&path)
        .output()
        .unwrap();
    let cat_text = String::from_utf8_lossy(&cat.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(check.status.success(), "usdchecker rejected:\n{check_text}");
    assert!(cat.status.success(), "usdcat failed");
    assert!(
        cat_text.contains("def PointInstancer \"Scatter\""),
        "{cat_text}"
    );
    assert!(
        cat_text.contains("int[] protoIndices = [0, 1, 0, 0]"),
        "{cat_text}"
    );
    assert!(
        cat_text.contains("int64[] invisibleIds = [103]"),
        "{cat_text}"
    );
    assert!(cat_text.contains("inactiveIds = [102]"), "{cat_text}");
}
