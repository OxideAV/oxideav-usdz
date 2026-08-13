//! UsdSkel §1.4.1 inbetween shapes (staged schema
//! `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`, added
//! 2026-08-10): `inbetweens:<name>` attributes with the target
//! weight in the attribute's `weight` metadata field. Each channel
//! with k valid inbetweens expands into k + 1 morph targets, the
//! scalar channel weight bakes into per-target weights through the
//! documented piecewise-linear resolution (implicit 0/1 endpoints,
//! unbounded extrapolation, keyframes inserted at knot crossings so
//! the typed model's linear interpolation is exact), authoring
//! errors (weight 0/1, duplicate weights) are ignored-but-preserved,
//! per-inbetween normal offsets are discovered by enumeration
//! (§1.4.2 — the spelling is unpublished), and the writer inverts
//! the bake exactly (`w = Σ vⱼ·knotⱼ`) for a one-cycle round-trip
//! fixed point.

mod common;

use oxideav_mesh3d::{AnimationProperty, AnimationValues, Scene3D};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn decode(usda: &str) -> Scene3D {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    UsdzDecoder::new().decode_bytes(&usdz).unwrap()
}

fn default_layer_text(usdz: &[u8]) -> String {
    let name_len = u16::from_le_bytes([usdz[26], usdz[27]]) as usize;
    let extra_len = u16::from_le_bytes([usdz[28], usdz[29]]) as usize;
    let size = u32::from_le_bytes([usdz[18], usdz[19], usdz[20], usdz[21]]) as usize;
    let start = 30 + name_len + extra_len;
    String::from_utf8(usdz[start..start + size].to_vec()).expect("utf8 layer")
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

const INBETWEEN_USDA: &str = r#"#usda 1.0
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
        uniform token[] blendShapes = ["smile"]
        float[] blendShapeWeights.timeSamples = {
            0: [0],
            24: [1],
        }
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        rel skel:animationSource = </Model/Anim>
        uniform token[] skel:blendShapes = ["smile"]
        rel skel:blendShapeTargets = [</Model/Face/Smile>]
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 1, 0), (0, 1, 0), (0, 0, 0)]
            uniform vector3f[] inbetweens:halfSmile = [(0, 0.7, 0), (0, 0.7, 0), (0, 0, 0)] (
                weight = 0.5
            )
        }
    }
}
"#;

#[test]
fn inbetween_expands_channel_into_two_targets() {
    let scene = decode(INBETWEEN_USDA);
    let prim = &scene.meshes[0].primitives[0];
    // One channel with one inbetween = two morph targets:
    // [halfSmile, primary].
    assert_eq!(prim.targets.len(), 2);
    let inb = prim.targets[0].position.as_ref().expect("inbetween deltas");
    assert!(approx(inb[0][1], 0.7));
    let primary = prim.targets[1].position.as_ref().expect("primary deltas");
    assert!(approx(primary[0][1], 1.0));
    // Roster stash: channel list stays ONE channel; the inbetween
    // roster records the group.
    let names = prim
        .extras
        .get("usd:skel:blendShapes")
        .and_then(|v| v.as_array())
        .expect("roster");
    assert_eq!(names.len(), 1);
    let roster = prim
        .extras
        .get("usd:skel:inbetweens")
        .and_then(|v| v.as_object())
        .expect("inbetween roster");
    let shapes = roster["smile"]["shapes"].as_array().expect("shapes");
    assert_eq!(shapes.len(), 1);
    assert_eq!(shapes[0]["name"].as_str(), Some("halfSmile"));
    assert!(approx(shapes[0]["weight"].as_f64().unwrap() as f32, 0.5));
    scene.validate().expect("validates");
}

#[test]
fn weight_animation_bakes_piecewise_linear_with_knot_insertion() {
    let scene = decode(INBETWEEN_USDA);
    let anim = &scene.animations[0];
    let morph = anim
        .channels
        .iter()
        .find(|ch| ch.target.property == AnimationProperty::MorphWeights)
        .expect("morph channel");
    // Authored keyframes at w=0 (t=0s) and w=1 (t=1s); the ramp
    // crosses the 0.5 knot at t=0.5s, which must gain a keyframe
    // for the linear bake to be exact.
    assert_eq!(morph.sampler.keyframes.len(), 3);
    assert!(approx(morph.sampler.keyframes[1], 0.5));
    let AnimationValues::Scalar(vals) = &morph.sampler.values else {
        panic!("scalar morph weights");
    };
    // Stride 2 ([halfSmile, primary]):
    // w=0   → [0, 0]
    // w=0.5 → [1, 0]   (exactly the inbetween shape)
    // w=1   → [0, 1]   (exactly the primary shape)
    assert_eq!(vals.len(), 6);
    assert!(approx(vals[0], 0.0) && approx(vals[1], 0.0));
    assert!(approx(vals[2], 1.0) && approx(vals[3], 0.0));
    assert!(approx(vals[4], 0.0) && approx(vals[5], 1.0));
}

#[test]
fn resolution_matches_doc_worked_example_with_extrapolation() {
    // Inbetween at 0.25; channel samples at w=-0.25 and w=0.625.
    // Doc: at w=-0.25 the 0.25 shape applies with weight −1; at
    // 0.25 ≤ w ≤ 1 interpolate the 0.25 shape and the primary.
    let usda = r#"#usda 1.0
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
        uniform token[] blendShapes = ["s"]
        float[] blendShapeWeights.timeSamples = {
            0: [-0.25],
            24: [0.625],
        }
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        rel skel:animationSource = </Model/Anim>
        uniform token[] skel:blendShapes = ["s"]
        rel skel:blendShapeTargets = [</Model/Face/S>]
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        def BlendShape "S" {
            uniform vector3f[] offsets = [(0, 1, 0)]
            uniform int[] pointIndices = [0]
            uniform vector3f[] inbetweens:quarter = [(1, 0, 0)] (
                weight = 0.25
            )
        }
    }
}
"#;
    let scene = decode(usda);
    let anim = &scene.animations[0];
    let morph = anim
        .channels
        .iter()
        .find(|ch| ch.target.property == AnimationProperty::MorphWeights)
        .expect("morph channel");
    let AnimationValues::Scalar(vals) = &morph.sampler.values else {
        panic!("scalar morph weights");
    };
    // Keyframes: t=0 (w=-0.25), knot crossing (w=0.25), t=1 (w=0.625).
    assert_eq!(morph.sampler.keyframes.len(), 3);
    // w=-0.25 → segment [0, 0.25], t=-1 → [S=-1, primary=0]: the
    // doc's worked extrapolation example.
    assert!(approx(vals[0], -1.0) && approx(vals[1], 0.0));
    // w=0.25 → exactly the inbetween.
    assert!(approx(vals[2], 1.0) && approx(vals[3], 0.0));
    // w=0.625 = midpoint of [0.25, 1] → [0.5, 0.5].
    assert!(approx(vals[4], 0.5) && approx(vals[5], 0.5));
    // pointIndices governs the inbetween too: its delta scattered
    // to point 0.
    let prim = &scene.meshes[0].primitives[0];
    let inb = prim.targets[0].position.as_ref().unwrap();
    assert!(approx(inb[0][0], 1.0));
    assert!(approx(inb[1][0], 0.0));
}

#[test]
fn malformed_inbetweens_ignored_but_preserved() {
    // weight = 1 is an implicit endpoint (authoring error) and two
    // shapes sharing weight 0.5 collide — all three are excluded
    // from evaluation, but replay verbatim.
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root"]
        uniform matrix4d[] bindTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
        uniform matrix4d[] restTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        uniform token[] skel:blendShapes = ["smile"]
        rel skel:blendShapeTargets = [</Model/Face/Smile>]
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 1, 0), (0, 1, 0), (0, 0, 0)]
            uniform vector3f[] inbetweens:atOne = [(9, 9, 9), (9, 9, 9), (9, 9, 9)] (
                weight = 1
            )
            uniform vector3f[] inbetweens:dupA = [(1, 0, 0), (0, 0, 0), (0, 0, 0)] (
                weight = 0.5
            )
            uniform vector3f[] inbetweens:dupB = [(2, 0, 0), (0, 0, 0), (0, 0, 0)] (
                weight = 0.5
            )
        }
    }
}
"#;
    let scene = decode(usda);
    let prim = &scene.meshes[0].primitives[0];
    // Only the primary shape survives evaluation.
    assert_eq!(prim.targets.len(), 1);
    // The malformed attrs replay verbatim on re-encode.
    let out = UsdzEncoder::new().encode_bytes(&scene).unwrap();
    let text = default_layer_text(&out);
    assert!(text.contains("inbetweens:atOne"), "{text}");
    assert!(text.contains("(weight = 1)"), "{text}");
    assert!(text.contains("inbetweens:dupA"), "{text}");
    assert!(text.contains("inbetweens:dupB"), "{text}");
    // Fixed point.
    let scene2 = UsdzDecoder::new().decode_bytes(&out).unwrap();
    let out2 = UsdzEncoder::new().encode_bytes(&scene2).unwrap();
    assert_eq!(text, default_layer_text(&out2));
}

#[test]
fn inbetween_normal_offsets_discovered_and_replayed() {
    // §1.4.2: the per-inbetween normal-offsets attribute spelling is
    // unpublished — it is discovered by enumerating deeper
    // `inbetweens:<name>:*` attributes, and the exact authored
    // spelling replays.
    let usda = r#"#usda 1.0
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root"]
        uniform matrix4d[] bindTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
        uniform matrix4d[] restTransforms = [((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1))]
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        uniform token[] skel:blendShapes = ["smile"]
        rel skel:blendShapeTargets = [</Model/Face/Smile>]
        int[] primvars:skel:jointIndices = [0, 0, 0] (
            elementSize = 1
            interpolation = "vertex"
        )
        float[] primvars:skel:jointWeights = [1, 1, 1] (
            elementSize = 1
            interpolation = "vertex"
        )
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 1, 0), (0, 0, 0), (0, 0, 0)]
            uniform vector3f[] inbetweens:half = [(0, 0.6, 0), (0, 0, 0), (0, 0, 0)] (
                weight = 0.5
            )
            uniform vector3f[] inbetweens:half:normalOffsets = [(0, 0, 0.2), (0, 0, 0), (0, 0, 0)] (
                weight = 0.5
            )
        }
    }
}
"#;
    let scene = decode(usda);
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.targets.len(), 2);
    let inb_normals = prim.targets[0].normal.as_ref().expect("inbetween normals");
    assert!(approx(inb_normals[0][2], 0.2));
    let out = UsdzEncoder::new().encode_bytes(&scene).unwrap();
    let text = default_layer_text(&out);
    assert!(
        text.contains("uniform vector3f[] inbetweens:half:normalOffsets = ["),
        "authored spelling replays: {text}"
    );
    let scene2 = UsdzDecoder::new().decode_bytes(&out).unwrap();
    let out2 = UsdzEncoder::new().encode_bytes(&scene2).unwrap();
    assert_eq!(text, default_layer_text(&out2));
}

#[test]
fn inbetween_roundtrip_is_a_fixed_point() {
    let scene1 = decode(INBETWEEN_USDA);
    let out1 = UsdzEncoder::new().encode_bytes(&scene1).unwrap();
    let text1 = default_layer_text(&out1);
    // The BlendShape re-authors with the inbetween attribute + its
    // weight metadata, and the animation re-emits the *scalar*
    // channel samples (inverse of the bake), not per-target vectors.
    assert!(
        text1.contains("uniform vector3f[] inbetweens:halfSmile = ["),
        "{text1}"
    );
    assert!(text1.contains("(weight = 0.5)"), "{text1}");
    assert!(
        text1.contains("uniform token[] blendShapes = [\"smile\"]"),
        "single scalar channel: {text1}"
    );
    // Inserted knot keyframe reconstructs scalar 0.5 exactly.
    assert!(text1.contains("12: [0.5]"), "{text1}");
    let scene2 = UsdzDecoder::new().decode_bytes(&out1).unwrap();
    let out2 = UsdzEncoder::new().encode_bytes(&scene2).unwrap();
    assert_eq!(text1, default_layer_text(&out2), "fixed point");
    // Typed-model equivalence across the cycle.
    let p1 = &scene1.meshes[0].primitives[0];
    let p2 = &scene2.meshes[0].primitives[0];
    assert_eq!(p1.targets.len(), p2.targets.len());
    let m1 = scene1.animations[0]
        .channels
        .iter()
        .find(|c| c.target.property == AnimationProperty::MorphWeights)
        .unwrap();
    let m2 = scene2.animations[0]
        .channels
        .iter()
        .find(|c| c.target.property == AnimationProperty::MorphWeights)
        .unwrap();
    assert_eq!(m1.sampler.keyframes, m2.sampler.keyframes);
    let (AnimationValues::Scalar(v1), AnimationValues::Scalar(v2)) =
        (&m1.sampler.values, &m2.sampler.values)
    else {
        panic!("scalar values");
    };
    assert_eq!(v1.len(), v2.len());
    for (a, b) in v1.iter().zip(v2) {
        assert!(approx(*a, *b));
    }
}
