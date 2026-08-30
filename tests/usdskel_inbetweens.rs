//! UsdSkel §1.4.1 inbetween shapes (staged schema
//! `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`, added
//! 2026-08-10): `inbetweens:<name>` attributes with the target
//! weight in the attribute's `weight` metadata field. Each channel
//! is ONE typed `MorphTarget` whose `inbetweens` roster carries the
//! valid stations (`oxideav-mesh3d` 0.0.6 `Inbetween`); the scalar
//! channel weight is stored verbatim (sampled `MorphWeights` built
//! through `AnimationSampler::morph_weights`, static states on
//! `Node::weights`) and the typed model resolves the documented
//! piecewise-linear interpolation (implicit 0/1 endpoints, unbounded
//! extrapolation) through `MorphTarget::at_weight`. Authoring errors
//! (weight 0/1, duplicate weights) are ignored-but-preserved,
//! per-inbetween normal offsets are discovered by enumeration
//! (§1.4.2 — the spelling is unpublished), and the round trip is a
//! one-cycle fixed point.

mod common;

use oxideav_mesh3d::{AnimationProperty, Interpolation, Scene3D};
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
fn inbetween_lands_on_typed_morph_target_roster() {
    let scene = decode(INBETWEEN_USDA);
    let mesh = &scene.meshes[0];
    let prim = &mesh.primitives[0];
    // One channel = ONE morph target; the inbetween is a typed
    // station on it, not a sibling target.
    assert_eq!(prim.targets.len(), 1);
    assert_eq!(mesh.target_names, vec!["smile".to_string()]);
    let primary = prim.targets[0].position.as_ref().expect("primary deltas");
    assert!(approx(primary[0][1], 1.0));
    let inbetweens = &prim.targets[0].inbetweens;
    assert_eq!(inbetweens.len(), 1);
    assert_eq!(inbetweens[0].name.as_deref(), Some("halfSmile"));
    assert!(approx(inbetweens[0].weight, 0.5));
    let inb = inbetweens[0].position.as_ref().expect("inbetween deltas");
    assert!(approx(inb[0][1], 0.7));
    assert!(inbetweens[0].normal.is_none(), "no normal offsets authored");
    // No extras side-channel roster survives.
    assert!(!prim.extras.contains_key("usd:skel:blendShapes"));
    assert!(!prim.extras.contains_key("usd:skel:inbetweens"));
    scene.validate().expect("validates");
}

#[test]
fn weight_animation_stores_scalar_channel_weights_verbatim() {
    let scene = decode(INBETWEEN_USDA);
    let anim = &scene.animations[0];
    let node = scene
        .nodes
        .iter()
        .position(|n| n.mesh.is_some())
        .expect("mesh node");
    let morph = anim
        .channel_for(
            oxideav_mesh3d::NodeId(node as u32),
            AnimationProperty::MorphWeights,
        )
        .expect("morph channel");
    // Authored keyframes only (t=0s → 0, t=1s → 1): no knot
    // insertion — the typed model resolves stations at sample time.
    assert_eq!(morph.sampler.keyframes, vec![0.0, 1.0]);
    assert_eq!(morph.sampler.interpolation, Interpolation::Linear);
    assert_eq!(morph.sampler.morph_weight_stride(), Some(1));
    let frames = morph
        .sampler
        .morph_weight_frames()
        .expect("lossless read-back");
    assert_eq!(frames, vec![&[0.0f32][..], &[1.0f32][..]]);
    // Typed resolution at the inbetween station is exactly the
    // inbetween shape; halfway between station and primary is the
    // linear blend of the two.
    let target = &scene.meshes[0].primitives[0].targets[0];
    let at_half = target.at_weight(0.5);
    assert!(approx(at_half.position.as_ref().unwrap()[0][1], 0.7));
    let at_three_quarters = target.at_weight(0.75);
    assert!(approx(
        at_three_quarters.position.as_ref().unwrap()[0][1],
        0.85
    ));
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
    // Authored scalars verbatim: t=0 (w=-0.25), t=1 (w=0.625).
    assert_eq!(morph.sampler.keyframes.len(), 2);
    let frames = morph.sampler.morph_weight_frames().unwrap();
    assert!(approx(frames[0][0], -0.25) && approx(frames[1][0], 0.625));
    // pointIndices governs the inbetween too: its delta scattered
    // to point 0.
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.targets.len(), 1);
    let target = &prim.targets[0];
    let inb = target.inbetweens[0].position.as_ref().unwrap();
    assert!(approx(inb[0][0], 1.0));
    assert!(approx(inb[1][0], 0.0));
    // Typed §1.4.1 resolution reproduces the doc's worked example:
    // w=-0.25 → segment [0, 0.25], the 0.25 shape at weight −1.
    let r = target.at_weight(-0.25);
    let p = r.position.as_ref().unwrap();
    assert!(approx(p[0][0], -1.0) && approx(p[0][1], 0.0));
    // w=0.25 → exactly the inbetween.
    let r = target.at_weight(0.25);
    let p = r.position.as_ref().unwrap();
    assert!(approx(p[0][0], 1.0) && approx(p[0][1], 0.0));
    // w=0.625 = midpoint of [0.25, 1] → half inbetween + half primary.
    let r = target.at_weight(0.625);
    let p = r.position.as_ref().unwrap();
    assert!(approx(p[0][0], 0.5) && approx(p[0][1], 0.5));
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
    // Only the primary shape survives evaluation — no typed station.
    assert_eq!(prim.targets.len(), 1);
    assert!(prim.targets[0].inbetweens.is_empty());
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
    assert_eq!(prim.targets.len(), 1);
    let inb = &prim.targets[0].inbetweens[0];
    let inb_normals = inb.normal.as_ref().expect("inbetween normals");
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
    // weight metadata, and the animation re-emits the authored
    // scalar channel samples (lossless `morph_weight_frames`).
    assert!(
        text1.contains("uniform vector3f[] inbetweens:halfSmile = ["),
        "{text1}"
    );
    assert!(text1.contains("(weight = 0.5)"), "{text1}");
    assert!(
        text1.contains("uniform token[] blendShapes = [\"smile\"]"),
        "single scalar channel: {text1}"
    );
    // Only the authored keyframes come back — no synthetic knot.
    assert!(text1.contains("0: [0], 24: [1]"), "{text1}");
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
    assert_eq!(
        m1.sampler.morph_weight_frames(),
        m2.sampler.morph_weight_frames()
    );
    assert_eq!(p1.targets[0].inbetweens, p2.targets[0].inbetweens);
    assert_eq!(scene1.meshes[0].target_names, scene2.meshes[0].target_names);
}
