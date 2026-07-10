//! UsdSkel core (staged schema §1.1–§1.6,
//! `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`): `SkelRoot` /
//! `Skeleton` prims into the typed Skeleton + joint-node tree,
//! BindingAPI joint influences into per-vertex joint/weight quads
//! (elementSize / interpolation layouts, `skel:joints` remap),
//! `geomBindTransform` + `skinningMethod` preservation, and the
//! write → re-read round trip.

mod common;

use oxideav_mesh3d::{Scene3D, Transform};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const SKEL_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Model"
)
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root", "Root/Hip", "Root/Hip/Knee"]
        uniform token[] jointNames = ["root", "hip", "knee"]
        uniform matrix4d[] bindTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,2,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,3,0,1))
        ]
        uniform matrix4d[] restTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,2,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,1,0,1))
        ]
    }
    def Mesh "Body" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        int[] primvars:skel:jointIndices = [0, 1, 1, 2, 2, 0] (
            elementSize = 2
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [0.75, 0.25, 0.5, 0.5, 0.9, 0.1] (
            elementSize = 2
            interpolation = "vertex"
        )
        matrix4d primvars:skel:geomBindTransform = ((1,0,0,0), (0,1,0,0), (0,0,1,0), (5,0,0,1))
        uniform token primvars:skel:skinningMethod = "dualQuaternion"
    }
}
"#;

fn decode(usda: &str) -> Scene3D {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    UsdzDecoder::new().decode_bytes(&usdz).unwrap()
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

fn node_name(scene: &Scene3D, id: oxideav_mesh3d::NodeId) -> &str {
    scene.nodes[id.0 as usize].name.as_deref().unwrap_or("")
}

#[test]
fn skeleton_builds_joint_node_tree() {
    let scene = decode(SKEL_USDA);
    assert_eq!(scene.skeletons.len(), 1);
    let skel = &scene.skeletons[0];
    assert_eq!(skel.name.as_deref(), Some("Skel"));
    assert_eq!(skel.joints.len(), 3);
    assert_eq!(node_name(&scene, skel.joints[0]), "Root");
    assert_eq!(node_name(&scene, skel.joints[1]), "Hip");
    assert_eq!(node_name(&scene, skel.joints[2]), "Knee");
    // Tree topology from the path-prefix rule.
    let root = &scene.nodes[skel.joints[0].0 as usize];
    assert_eq!(root.children, vec![skel.joints[1]]);
    let hip = &scene.nodes[skel.joints[1].0 as usize];
    assert_eq!(hip.children, vec![skel.joints[2]]);
}

#[test]
fn rest_transforms_land_on_joint_nodes() {
    let scene = decode(SKEL_USDA);
    let skel = &scene.skeletons[0];
    // Knee's local rest transform translates by (0, 1, 0) — the
    // parent-relative offset (bind is (0,3), hip bind (0,2)).
    let knee = &scene.nodes[skel.joints[2].0 as usize];
    match knee.transform {
        Transform::Matrix(m) => {
            assert!(approx(m[3][1], 1.0), "knee local Y offset");
        }
        ref other => panic!("expected Matrix transform, got {other:?}"),
    }
}

#[test]
fn inverse_bind_matrices_invert_bind_transforms() {
    let scene = decode(SKEL_USDA);
    let skel = &scene.skeletons[0];
    // Hip bind translates (0, 2, 0) → its inverse translates (0, -2, 0).
    let ibm = skel.inverse_bind_matrices[1];
    assert!(approx(ibm[3][1], -2.0), "ibm hip: {ibm:?}");
    // Rotation part stays identity.
    assert!(approx(ibm[0][0], 1.0));
    assert!(approx(ibm[1][1], 1.0));
    assert!(approx(ibm[2][2], 1.0));
}

#[test]
fn joint_influences_pad_to_quads() {
    let scene = decode(SKEL_USDA);
    let prim = &scene.meshes[0].primitives[0];
    let joints = prim.joints.as_ref().expect("joints decoded");
    let weights = prim.weights.as_ref().expect("weights decoded");
    assert_eq!(joints.len(), 3);
    assert_eq!(weights.len(), 3);
    // elementSize 2 pads with zero-weight slots.
    assert_eq!(joints[0], [0, 1, 0, 0]);
    assert!(approx(weights[0][0], 0.75));
    assert!(approx(weights[0][1], 0.25));
    assert!(approx(weights[0][2], 0.0));
    assert_eq!(joints[2], [2, 0, 0, 0]);
    assert!(approx(weights[2][0], 0.9));
}

#[test]
fn skin_binds_node_to_skeleton() {
    let scene = decode(SKEL_USDA);
    assert_eq!(scene.skins.len(), 1);
    let mesh_node = scene
        .nodes
        .iter()
        .find(|n| n.mesh.is_some())
        .expect("mesh node");
    let skin_id = mesh_node.skin.expect("node.skin set");
    let skin = &scene.skins[skin_id.0 as usize];
    assert_eq!(skin.skeleton.0, 0);
    // Explicit root = the skeleton's root joint.
    let root = skin.root_node.expect("explicit root");
    assert_eq!(node_name(&scene, root), "Root");
}

#[test]
fn geom_bind_transform_and_skinning_method_preserved() {
    let scene = decode(SKEL_USDA);
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(
        prim.extras
            .get("usd:skel:skinningMethod")
            .and_then(|v| v.as_str()),
        Some("dualQuaternion")
    );
    let rows = prim
        .extras
        .get("usd:skel:geomBindTransform")
        .and_then(|v| v.as_array())
        .expect("geomBindTransform preserved");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3].as_array().unwrap()[0].as_f64(), Some(5.0));
}

#[test]
fn constant_interpolation_replicates_per_point() {
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root"]
        uniform matrix4d[] bindTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
        uniform matrix4d[] restTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
    }
    def Mesh "Body" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        int[] primvars:skel:jointIndices = [0] (
            elementSize = 1
            interpolation = "constant"
        )
        float[] primvars:skel:jointWeights = [1] (
            elementSize = 1
            interpolation = "constant"
        )
    }
}
"#;
    let scene = decode(usda);
    let prim = &scene.meshes[0].primitives[0];
    let joints = prim.joints.as_ref().unwrap();
    let weights = prim.weights.as_ref().unwrap();
    assert_eq!(joints.len(), 3, "rigid binding covers every point");
    for (jq, wq) in joints.iter().zip(weights) {
        assert_eq!(*jq, [0, 0, 0, 0]);
        assert!(approx(wq[0], 1.0));
    }
}

#[test]
fn skel_joints_override_remaps_into_skeleton_order() {
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["A", "A/B", "A/B/C"]
        uniform matrix4d[] bindTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))
        ]
        uniform matrix4d[] restTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))
        ]
    }
    def Mesh "Body" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        uniform token[] skel:joints = ["A/B/C", "A"]
        int[] primvars:skel:jointIndices = [0, 1, 0, 1, 1, 0] (
            elementSize = 2
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [0.6, 0.4, 0.7, 0.3, 0.8, 0.2] (
            elementSize = 2
            interpolation = "vertex"
        )
    }
}
"#;
    let scene = decode(usda);
    let prim = &scene.meshes[0].primitives[0];
    let joints = prim.joints.as_ref().unwrap();
    // Override index 0 = "A/B/C" = skeleton index 2; override 1 =
    // "A" = skeleton index 0.
    assert_eq!(joints[0], [2, 0, 0, 0]);
    assert_eq!(joints[1], [2, 0, 0, 0]);
    assert_eq!(joints[2], [0, 2, 0, 0]);
}

#[test]
fn more_than_four_influences_keep_strongest() {
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["J0", "J1", "J2", "J3", "J4", "J5"]
        uniform matrix4d[] bindTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))
        ]
        uniform matrix4d[] restTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))
        ]
    }
    def Mesh "Body" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        int[] primvars:skel:jointIndices = [
            0, 1, 2, 3, 4, 5,
            0, 1, 2, 3, 4, 5,
            0, 1, 2, 3, 4, 5
        ] (
            elementSize = 6
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [
            0.05, 0.3, 0.25, 0.2, 0.15, 0.05,
            0.1, 0.1, 0.2, 0.2, 0.2, 0.2,
            0.5, 0.1, 0.1, 0.1, 0.1, 0.1
        ] (
            elementSize = 6
            interpolation = "vertex"
        )
    }
}
"#;
    let scene = decode(usda);
    let prim = &scene.meshes[0].primitives[0];
    let joints = prim.joints.as_ref().unwrap();
    let weights = prim.weights.as_ref().unwrap();
    // Point 0: the four strongest of [.05,.3,.25,.2,.15,.05] are
    // joints 1, 2, 3, 4.
    assert_eq!(joints[0], [1, 2, 3, 4]);
    assert!(approx(weights[0][0], 0.3));
    assert!(approx(weights[0][3], 0.15));
    // Point 2: joint 0 dominates.
    assert_eq!(joints[2][0], 0);
    assert!(approx(weights[2][0], 0.5));
}

#[test]
fn skel_binding_inherits_from_skelroot() {
    // §1.5: skel:skeleton authored on the SkelRoot applies to the
    // whole subtree.
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    rel skel:skeleton = </Model/Skel>
    def Skeleton "Skel" {
        uniform token[] joints = ["Root"]
        uniform matrix4d[] bindTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
        uniform matrix4d[] restTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
    }
    def Mesh "Body" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
    }
}
"#;
    let scene = decode(usda);
    let prim = &scene.meshes[0].primitives[0];
    assert!(prim.joints.is_some(), "binding inherited from SkelRoot");
    let mesh_node = scene.nodes.iter().find(|n| n.mesh.is_some()).unwrap();
    assert!(mesh_node.skin.is_some());
}

#[test]
fn skel_scene_round_trips() {
    let scene = decode(SKEL_USDA);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new()
        .decode_bytes(&bytes)
        .expect("re-decode ok");

    assert_eq!(s2.skeletons.len(), 1);
    let (a, b) = (&scene.skeletons[0], &s2.skeletons[0]);
    assert_eq!(a.joints.len(), b.joints.len());
    for (ia, ib) in a.inverse_bind_matrices.iter().zip(&b.inverse_bind_matrices) {
        for r in 0..4 {
            for c in 0..4 {
                assert!(
                    (ia[r][c] - ib[r][c]).abs() < 1e-4,
                    "ibm[{r}][{c}]: {} vs {}",
                    ia[r][c],
                    ib[r][c]
                );
            }
        }
    }
    // Joint node names + topology survive.
    for (ja, jb) in a.joints.iter().zip(&b.joints) {
        assert_eq!(node_name(&scene, *ja), node_name(&s2, *jb));
    }
    // Influences survive verbatim (already 4-padded canonical form).
    let (pa, pb) = (&scene.meshes[0].primitives[0], &s2.meshes[0].primitives[0]);
    assert_eq!(pa.joints, pb.joints);
    let (wa, wb) = (pa.weights.as_ref().unwrap(), pb.weights.as_ref().unwrap());
    for (qa, qb) in wa.iter().zip(wb) {
        for e in 0..4 {
            assert!(approx(qa[e], qb[e]));
        }
    }
    // Binding extras survive.
    assert_eq!(
        pa.extras.get("usd:skel:skinningMethod"),
        pb.extras.get("usd:skel:skinningMethod")
    );
    assert_eq!(
        pa.extras.get("usd:skel:geomBindTransform"),
        pb.extras.get("usd:skel:geomBindTransform")
    );
    // Skin binding survives.
    let mesh_node = s2.nodes.iter().find(|n| n.mesh.is_some()).unwrap();
    assert!(mesh_node.skin.is_some());
    // jointNames stash survives on the carrier.
    let carrier = s2
        .nodes
        .iter()
        .find(|n| n.extras.contains_key("usd:skeleton"))
        .expect("skeleton carrier");
    assert!(carrier.extras.contains_key("usd:skel:jointNames"));
}

#[test]
fn skel_encode_reaches_fixed_point_after_one_cycle() {
    let scene = decode(SKEL_USDA);
    let bytes1 = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes1).expect("decode ok");
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    let bytes2 = UsdzEncoder::new().encode_bytes(&s2).expect("encode ok");
    let s3 = UsdzDecoder::new().decode_bytes(&bytes2).expect("decode ok");
    let third = UsdzEncoder::new()
        .encode_with_report(&s3)
        .expect("encode ok")
        .usda;
    assert_eq!(
        second, third,
        "skel round-trip must be a fixed point after one cycle"
    );
}

#[test]
fn skelroot_prim_type_survives() {
    let scene = decode(SKEL_USDA);
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("def SkelRoot \"Model\""),
        "SkelRoot schema token must re-emit:\n{}",
        report.usda
    );
    assert!(report.usda.contains("def Skeleton \"Skel\""));
    assert!(report.usda.contains("rel skel:skeleton = </Model/Skel>"));
}
