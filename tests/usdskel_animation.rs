//! UsdSkel SkelAnimation (staged schema §1.3,
//! `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`): time-sampled
//! joint TRS into typed Animation channels targeting the joint
//! nodes, timeCodes → seconds through `timeCodesPerSecond`, subset /
//! reordered animation joints remapped by token, quatf (w,x,y,z) →
//! xyzw — and the write → re-read round trip including the
//! `skel:animationSource` relationship.

mod common;

use oxideav_mesh3d::{AnimationProperty, AnimationValues, Scene3D};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const ANIM_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Model"
    timeCodesPerSecond = 24
)
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root", "Root/Arm"]
        uniform matrix4d[] bindTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,1,0,1))
        ]
        uniform matrix4d[] restTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,1,0,1))
        ]
    }
    def SkelAnimation "Anim" {
        uniform token[] joints = ["Root", "Root/Arm"]
        float3[] translations.timeSamples = {
            0: [(0, 0, 0), (0, 1, 0)],
            24: [(0, 0, 2), (0, 1, 0)],
        }
        quatf[] rotations.timeSamples = {
            0: [(1, 0, 0, 0), (1, 0, 0, 0)],
            24: [(1, 0, 0, 0), (0.707, 0.707, 0, 0)],
        }
        half3[] scales.timeSamples = {
            0: [(1, 1, 1), (1, 1, 1)],
            24: [(1, 1, 1), (2, 2, 2)],
        }
    }
    def Mesh "Body" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        rel skel:animationSource = </Model/Anim>
        int[] primvars:skel:jointIndices = [0, 1, 1] (
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

fn decode(usda: &str) -> Scene3D {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    UsdzDecoder::new().decode_bytes(&usdz).unwrap()
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn node_name(scene: &Scene3D, id: oxideav_mesh3d::NodeId) -> &str {
    scene.nodes[id.0 as usize].name.as_deref().unwrap_or("")
}

#[test]
fn animation_decodes_trs_channels_per_joint() {
    let scene = decode(ANIM_USDA);
    assert_eq!(scene.animations.len(), 1);
    let anim = &scene.animations[0];
    assert_eq!(anim.name.as_deref(), Some("Anim"));
    // 2 joints x 3 properties = 6 channels.
    assert_eq!(anim.channels.len(), 6);
    let count = |p: AnimationProperty| {
        anim.channels
            .iter()
            .filter(|c| c.target.property == p)
            .count()
    };
    assert_eq!(count(AnimationProperty::Translation), 2);
    assert_eq!(count(AnimationProperty::Rotation), 2);
    assert_eq!(count(AnimationProperty::Scale), 2);
}

#[test]
fn time_codes_convert_to_seconds() {
    let scene = decode(ANIM_USDA);
    let anim = &scene.animations[0];
    let ch = &anim.channels[0];
    // timeCodesPerSecond = 24, timeCodes 0 + 24 → 0s + 1s.
    assert_eq!(ch.sampler.keyframes.len(), 2);
    assert!(approx(ch.sampler.keyframes[0], 0.0));
    assert!(approx(ch.sampler.keyframes[1], 1.0));
}

#[test]
fn channels_target_the_joint_nodes() {
    let scene = decode(ANIM_USDA);
    let skel = &scene.skeletons[0];
    let anim = &scene.animations[0];
    // Root's translation channel carries (0,0,0) → (0,0,2).
    let root_t = anim
        .channels
        .iter()
        .find(|c| {
            c.target.node == skel.joints[0] && c.target.property == AnimationProperty::Translation
        })
        .expect("Root translation channel");
    match &root_t.sampler.values {
        AnimationValues::Vec3(v) => {
            assert_eq!(v.len(), 2);
            assert!(approx(v[1][2], 2.0));
        }
        other => panic!("expected Vec3 values, got {other:?}"),
    }
    // Arm's rotation channel converts (w,x,y,z) → xyzw.
    let arm_r = anim
        .channels
        .iter()
        .find(|c| {
            c.target.node == skel.joints[1] && c.target.property == AnimationProperty::Rotation
        })
        .expect("Arm rotation channel");
    match &arm_r.sampler.values {
        AnimationValues::Quat(v) => {
            // Authored (0.707, 0.707, 0, 0) = w, x → internal
            // [x, y, z, w] = [0.707, 0, 0, 0.707].
            assert!(approx(v[1][0], 0.707));
            assert!(approx(v[1][3], 0.707));
            assert!(approx(v[1][1], 0.0));
        }
        other => panic!("expected Quat values, got {other:?}"),
    }
}

#[test]
fn subset_and_reordered_joints_remap_by_token() {
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
    def SkelAnimation "Anim" {
        uniform token[] joints = ["A/B/C", "A"]
        float3[] translations.timeSamples = {
            0: [(9, 0, 0), (7, 0, 0)],
        }
    }
}
"#;
    let scene = decode(usda);
    let skel = &scene.skeletons[0];
    let anim = &scene.animations[0];
    // Only the two named joints get channels, matched by token.
    assert_eq!(anim.channels.len(), 2);
    let for_joint = |node: oxideav_mesh3d::NodeId| {
        anim.channels
            .iter()
            .find(|c| c.target.node == node)
            .expect("channel")
    };
    // "A/B/C" = skeleton joint 2 gets x = 9; "A" = joint 0 gets 7.
    match &for_joint(skel.joints[2]).sampler.values {
        AnimationValues::Vec3(v) => assert!(approx(v[0][0], 9.0)),
        other => panic!("unexpected {other:?}"),
    }
    match &for_joint(skel.joints[0]).sampler.values {
        AnimationValues::Vec3(v) => assert!(approx(v[0][0], 7.0)),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn animation_round_trips() {
    let scene = decode(ANIM_USDA);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new()
        .decode_bytes(&bytes)
        .expect("re-decode ok");

    assert_eq!(s2.animations.len(), 1);
    let (a, b) = (&scene.animations[0], &s2.animations[0]);
    assert_eq!(a.name, b.name);
    assert_eq!(a.channels.len(), b.channels.len());
    // Compare channel-by-channel via (target joint name, property).
    for ch_a in &a.channels {
        let name_a = node_name(&scene, ch_a.target.node).to_string();
        let ch_b = b
            .channels
            .iter()
            .find(|c| {
                node_name(&s2, c.target.node) == name_a && c.target.property == ch_a.target.property
            })
            .unwrap_or_else(|| panic!("channel for {name_a} {:?}", ch_a.target.property));
        assert_eq!(ch_a.sampler.keyframes.len(), ch_b.sampler.keyframes.len());
        for (ta, tb) in ch_a.sampler.keyframes.iter().zip(&ch_b.sampler.keyframes) {
            assert!(approx(*ta, *tb), "keyframe {ta} vs {tb}");
        }
        match (&ch_a.sampler.values, &ch_b.sampler.values) {
            (AnimationValues::Vec3(va), AnimationValues::Vec3(vb)) => {
                assert_eq!(va.len(), vb.len());
                for (xa, xb) in va.iter().zip(vb) {
                    for e in 0..3 {
                        assert!(approx(xa[e], xb[e]), "{xa:?} vs {xb:?}");
                    }
                }
            }
            (AnimationValues::Quat(va), AnimationValues::Quat(vb)) => {
                assert_eq!(va.len(), vb.len());
                for (xa, xb) in va.iter().zip(vb) {
                    for e in 0..4 {
                        assert!(approx(xa[e], xb[e]), "{xa:?} vs {xb:?}");
                    }
                }
            }
            other => panic!("value-shape mismatch: {other:?}"),
        }
    }
}

#[test]
fn writer_emits_animation_source_relationship() {
    let scene = decode(ANIM_USDA);
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("def SkelAnimation \"Anim\""),
        "SkelAnimation prim must re-emit:\n{}",
        report.usda
    );
    assert!(
        report
            .usda
            .contains("rel skel:animationSource = </Model/Anim>"),
        "animationSource rel must re-emit:\n{}",
        report.usda
    );
    assert!(report.usda.contains("translations.timeSamples"));
    assert!(report.usda.contains("rotations.timeSamples"));
    assert!(report.usda.contains("scales.timeSamples"));
}

#[test]
fn anim_encode_reaches_fixed_point_after_one_cycle() {
    let scene = decode(ANIM_USDA);
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
        "animation round-trip must be a fixed point after one cycle"
    );
}

#[test]
fn default_value_arrays_become_single_keyframe() {
    // Non-sampled (default) TRS arrays = one keyframe at t = 0.
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root"]
        uniform matrix4d[] bindTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
        uniform matrix4d[] restTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
    }
    def SkelAnimation "Pose" {
        uniform token[] joints = ["Root"]
        float3[] translations = [(3, 4, 5)]
    }
}
"#;
    let scene = decode(usda);
    let anim = &scene.animations[0];
    assert_eq!(anim.channels.len(), 1);
    let ch = &anim.channels[0];
    assert_eq!(ch.sampler.keyframes, vec![0.0]);
    match &ch.sampler.values {
        AnimationValues::Vec3(v) => assert_eq!(v[0], [3.0, 4.0, 5.0]),
        other => panic!("unexpected {other:?}"),
    }
}
