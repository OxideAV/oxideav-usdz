//! UsdSkel blend shapes (staged schema §1.4 / §1.5 / §1.8,
//! `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`): BlendShape
//! prims into typed MorphTargets (dense + sparse `pointIndices`
//! scatter), positional `skel:blendShapes` / `skel:blendShapeTargets`
//! matching, animation `blendShapeWeights` into a MorphWeights
//! channel remapped by channel name — and the write → re-read round
//! trip.

mod common;

use oxideav_mesh3d::{AnimationProperty, AnimationValues, Scene3D};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const BLEND_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Model"
    timeCodesPerSecond = 24
)
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root"]
        uniform matrix4d[] bindTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
        uniform matrix4d[] restTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
    }
    def SkelAnimation "Anim" {
        uniform token[] blendShapes = ["smile", "frown"]
        float[] blendShapeWeights.timeSamples = {
            0: [0, 0],
            24: [1, 0.5],
        }
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        rel skel:animationSource = </Model/Anim>
        uniform token[] skel:blendShapes = ["smile", "frown"]
        rel skel:blendShapeTargets = [</Model/Face/Smile>, </Model/Face/Frown>]
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 0.5, 0), (0, 0.25, 0), (0, 0, 0)]
            uniform vector3f[] normalOffsets = [(0, 0, 0.1), (0, 0, 0.1), (0, 0, 0)]
        }
        def BlendShape "Frown" {
            uniform vector3f[] offsets = [(0, -0.5, 0)]
            uniform int[] pointIndices = [1]
        }
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

#[test]
fn blend_shapes_become_morph_targets() {
    let scene = decode(BLEND_USDA);
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.targets.len(), 2, "two channels = two morph targets");
    // Dense shape carries the authored deltas verbatim.
    let smile = &prim.targets[0];
    let pos = smile.position.as_ref().expect("position deltas");
    assert_eq!(pos.len(), 3);
    assert!(approx(pos[0][1], 0.5));
    assert!(approx(pos[1][1], 0.25));
    let nrm = smile.normal.as_ref().expect("normal deltas");
    assert!(approx(nrm[0][2], 0.1));
    // Channel-name roster preserved for the writer + weight remap.
    let names = prim
        .extras
        .get("usd:skel:blendShapes")
        .and_then(|v| v.as_array())
        .expect("channel roster");
    assert_eq!(names.len(), 2);
    assert_eq!(names[0].as_str(), Some("smile"));
}

#[test]
fn sparse_point_indices_scatter_to_dense() {
    let scene = decode(BLEND_USDA);
    let prim = &scene.meshes[0].primitives[0];
    let frown = &prim.targets[1];
    let pos = frown.position.as_ref().expect("position deltas");
    assert_eq!(pos.len(), 3, "sparse shape expands to per-point deltas");
    assert!(approx(pos[0][1], 0.0), "untouched point stays zero");
    assert!(approx(pos[1][1], -0.5), "pointIndices[0]=1 gets the delta");
    assert!(approx(pos[2][1], 0.0));
}

#[test]
fn blend_weights_become_morph_weights_channel() {
    let scene = decode(BLEND_USDA);
    let anim = &scene.animations[0];
    let morph = anim
        .channels
        .iter()
        .find(|c| c.target.property == AnimationProperty::MorphWeights)
        .expect("MorphWeights channel");
    // Targets the mesh node.
    let target = &scene.nodes[morph.target.node.0 as usize];
    assert!(target.mesh.is_some(), "channel targets the mesh node");
    // 2 keyframes (0s, 1s) x 2 channels.
    assert_eq!(morph.sampler.keyframes.len(), 2);
    assert!(approx(morph.sampler.keyframes[1], 1.0));
    match &morph.sampler.values {
        AnimationValues::Scalar(v) => {
            assert_eq!(v.len(), 4);
            assert!(approx(v[0], 0.0));
            assert!(approx(v[1], 0.0));
            assert!(approx(v[2], 1.0), "smile weight at t=1");
            assert!(approx(v[3], 0.5), "frown weight at t=1");
        }
        other => panic!("expected Scalar values, got {other:?}"),
    }
}

#[test]
fn animation_channel_order_remaps_by_name() {
    // Animation authors channels in the REVERSE of the geometry's
    // order — weights must land per-name, not per-position.
    let usda = BLEND_USDA
        .replace(
            "uniform token[] blendShapes = [\"smile\", \"frown\"]",
            "uniform token[] blendShapes = [\"frown\", \"smile\"]",
        )
        .replace("24: [1, 0.5],", "24: [0.5, 1],");
    let scene = decode(&usda);
    let anim = &scene.animations[0];
    let morph = anim
        .channels
        .iter()
        .find(|c| c.target.property == AnimationProperty::MorphWeights)
        .expect("MorphWeights channel");
    match &morph.sampler.values {
        AnimationValues::Scalar(v) => {
            // Mesh order is [smile, frown] — smile still gets 1.
            assert!(approx(v[2], 1.0));
            assert!(approx(v[3], 0.5));
        }
        other => panic!("expected Scalar values, got {other:?}"),
    }
}

#[test]
fn blend_shapes_round_trip() {
    let scene = decode(BLEND_USDA);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new()
        .decode_bytes(&bytes)
        .expect("re-decode ok");

    let (pa, pb) = (&scene.meshes[0].primitives[0], &s2.meshes[0].primitives[0]);
    assert_eq!(pa.targets.len(), pb.targets.len());
    for (ta, tb) in pa.targets.iter().zip(&pb.targets) {
        let (da, db) = (ta.position.as_ref().unwrap(), tb.position.as_ref().unwrap());
        assert_eq!(da.len(), db.len());
        for (xa, xb) in da.iter().zip(db) {
            for e in 0..3 {
                assert!(approx(xa[e], xb[e]), "{xa:?} vs {xb:?}");
            }
        }
        assert_eq!(ta.normal.is_some(), tb.normal.is_some());
    }
    // MorphWeights channel survives.
    let morph = s2.animations[0]
        .channels
        .iter()
        .find(|c| c.target.property == AnimationProperty::MorphWeights)
        .expect("MorphWeights channel after round trip");
    match &morph.sampler.values {
        AnimationValues::Scalar(v) => {
            assert!(approx(v[2], 1.0));
            assert!(approx(v[3], 0.5));
        }
        other => panic!("expected Scalar values, got {other:?}"),
    }
    // Channel names survive.
    assert_eq!(
        pa.extras.get("usd:skel:blendShapes"),
        pb.extras.get("usd:skel:blendShapes")
    );
}

#[test]
fn blend_encode_reaches_fixed_point_after_one_cycle() {
    let scene = decode(BLEND_USDA);
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
        "blend-shape round-trip must be a fixed point after one cycle"
    );
}

#[test]
fn writer_emits_blend_shape_prims() {
    let scene = decode(BLEND_USDA);
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("def BlendShape \"smile\""),
        "BlendShape prim (channel name) must re-emit:\n{}",
        report.usda
    );
    assert!(report
        .usda
        .contains("uniform token[] skel:blendShapes = [\"smile\", \"frown\"]"));
    assert!(report.usda.contains("rel skel:blendShapeTargets = ["));
    assert!(report.usda.contains("blendShapeWeights.timeSamples"));
    assert!(report
        .usda
        .contains("uniform token[] blendShapes = [\"smile\", \"frown\"]"));
}
