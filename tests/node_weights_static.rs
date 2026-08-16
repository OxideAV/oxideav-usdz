//! Static blend-shape weight states onto `Node::weights` (staged
//! schema §1.3 / §1.5 / §1.4.1 +
//! `oxideav-mesh3d` 0.0.5's node-level morph-weight override):
//!
//! * a `blendShapeWeights` authored as a plain **default** value (no
//!   `.timeSamples`) is a static state — it lands on the mesh node's
//!   `Node::weights` instead of fabricating a one-keyframe animation
//!   channel;
//! * an authored/inherited `skel:animationSource` relationship
//!   scopes which SkelAnimation drives which geometry, so divergent
//!   static states over identical rosters stay apart;
//! * the writer re-emits the state in the same default-value form
//!   (and synthesizes a `BlendState_<id>` carrier for typed-model
//!   scenes), making the round trip a one-cycle fixed point.

mod common;

use oxideav_mesh3d::{
    AnimationProperty, Indices, Mesh, MorphTarget, Node, Primitive, Scene3D, Topology,
};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

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

const STATIC_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Model"
    timeCodesPerSecond = 24
)
def SkelRoot "Model" {
    def SkelAnimation "Anim" {
        uniform token[] blendShapes = ["smile", "frown"]
        float[] blendShapeWeights = [0.3, 0.7]
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:animationSource = </Model/Anim>
        uniform token[] skel:blendShapes = ["smile", "frown"]
        rel skel:blendShapeTargets = [</Model/Face/Smile>, </Model/Face/Frown>]
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 0.5, 0), (0, 0.25, 0), (0, 0, 0)]
        }
        def BlendShape "Frown" {
            uniform vector3f[] offsets = [(0, -0.5, 0)]
            uniform int[] pointIndices = [1]
        }
    }
}
"#;

#[test]
fn static_default_weights_land_on_node_weights() {
    let scene = decode(STATIC_USDA);
    let node = scene
        .nodes
        .iter()
        .find(|n| n.mesh.is_some())
        .expect("mesh node");
    assert_eq!(node.weights, vec![0.3, 0.7], "static state → Node::weights");
    // No fabricated one-keyframe animation channel.
    for anim in &scene.animations {
        assert!(
            !anim
                .channels
                .iter()
                .any(|c| c.target.property == AnimationProperty::MorphWeights),
            "a default-value blendShapeWeights must not become a MorphWeights channel"
        );
    }
    // The static (node > mesh) precedence chain resolves the override.
    let node_id = scene
        .roots
        .iter()
        .flat_map(|&r| collect_ids(&scene, r))
        .find(|&id| scene.node(id).is_some_and(|n| n.mesh.is_some()))
        .expect("mesh node id");
    let eff = scene
        .effective_morph_weights(node_id)
        .expect("effective weights resolve");
    assert_eq!(eff, &[0.3, 0.7]);
    scene.validate().expect("decoded scene validates");
}

fn collect_ids(scene: &Scene3D, id: oxideav_mesh3d::NodeId) -> Vec<oxideav_mesh3d::NodeId> {
    let mut out = vec![id];
    if let Some(n) = scene.node(id) {
        for &c in &n.children {
            out.extend(collect_ids(scene, c));
        }
    }
    out
}

#[test]
fn sampled_weights_still_become_a_channel() {
    let sampled = STATIC_USDA.replace(
        "float[] blendShapeWeights = [0.3, 0.7]",
        "float[] blendShapeWeights.timeSamples = { 0: [0.3, 0.7], 24: [1, 0] }",
    );
    let scene = decode(&sampled);
    let node = scene
        .nodes
        .iter()
        .find(|n| n.mesh.is_some())
        .expect("mesh node");
    assert!(
        node.weights.is_empty(),
        "sampled weights are animation, not a static override"
    );
    assert!(scene.animations.iter().any(|a| a
        .channels
        .iter()
        .any(|c| c.target.property == AnimationProperty::MorphWeights)));
}

#[test]
fn static_state_reemits_in_default_value_form() {
    let scene = decode(STATIC_USDA);
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report
            .usda
            .contains("float[] blendShapeWeights = [0.3, 0.7]"),
        "static state must re-emit as a plain default value:\n{}",
        report.usda
    );
    assert!(
        !report.usda.contains("blendShapeWeights.timeSamples"),
        "no timeSamples fabrication:\n{}",
        report.usda
    );
    assert!(report
        .usda
        .contains("uniform token[] blendShapes = [\"smile\", \"frown\"]"));
    // The geometry re-binds its animation source.
    assert!(
        report.usda.contains("rel skel:animationSource = <"),
        "geometry must re-author skel:animationSource:\n{}",
        report.usda
    );
}

#[test]
fn static_state_roundtrip_is_one_cycle_fixed_point() {
    let scene = decode(STATIC_USDA);
    let bytes1 = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes1).expect("decode ok");
    let n2 = s2
        .nodes
        .iter()
        .find(|n| n.mesh.is_some())
        .expect("mesh node");
    assert_eq!(n2.weights, vec![0.3, 0.7], "weights survive the cycle");
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
    assert_eq!(second, third, "fixed point after one cycle");
}

#[test]
fn live_node_weight_edit_survives_reencode() {
    // Mutating the typed override after decode must win over the
    // decoder's stash on re-encode.
    let mut scene = decode(STATIC_USDA);
    let idx = scene
        .nodes
        .iter()
        .position(|n| n.mesh.is_some())
        .expect("mesh node");
    scene.nodes[idx].weights = vec![0.9, 0.1];
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report
            .usda
            .contains("float[] blendShapeWeights = [0.9, 0.1]"),
        "edited Node::weights must re-emit:\n{}",
        report.usda
    );
}

/// §1.4.1: a channel with one inbetween (weight 0.5) at static
/// scalar 0.25 expands to [0.5, 0.0] — halfway up the ramp to the
/// inbetween, primary untouched.
#[test]
fn static_weights_expand_through_inbetweens() {
    let usda = r#"#usda 1.0
(
    defaultPrim = "Model"
)
def SkelRoot "Model" {
    def SkelAnimation "Anim" {
        uniform token[] blendShapes = ["smile"]
        float[] blendShapeWeights = [0.25]
    }
    def Mesh "Face" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:animationSource = </Model/Anim>
        uniform token[] skel:blendShapes = ["smile"]
        rel skel:blendShapeTargets = [</Model/Face/Smile>]
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 1, 0), (0, 1, 0), (0, 1, 0)]
            uniform vector3f[] inbetweens:Half = [(0, 0.4, 0), (0, 0.4, 0), (0, 0.4, 0)] (weight = 0.5)
        }
    }
}
"#;
    let scene = decode(usda);
    let node = scene
        .nodes
        .iter()
        .find(|n| n.mesh.is_some())
        .expect("mesh node");
    assert_eq!(node.weights.len(), 2, "inbetween + primary targets");
    assert!(approx(node.weights[0], 0.5), "inbetween at half its knot");
    assert!(approx(node.weights[1], 0.0), "primary untouched");
    // Writer inverts the bake back to the authored scalar.
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("float[] blendShapeWeights = [0.25]"),
        "closed-form inversion recovers the authored scalar:\n{}",
        report.usda
    );
}

/// §1.5 scoping: two SkelAnimations over *identical* channel
/// rosters, each bound to its own geometry via
/// `skel:animationSource` — roster intersection alone could not
/// tell them apart.
#[test]
fn animation_source_scopes_divergent_static_states() {
    let usda = r#"#usda 1.0
(
    defaultPrim = "Model"
)
def SkelRoot "Model" {
    def SkelAnimation "AnimA" {
        uniform token[] blendShapes = ["smile"]
        float[] blendShapeWeights = [0.2]
    }
    def SkelAnimation "AnimB" {
        uniform token[] blendShapes = ["smile"]
        float[] blendShapeWeights = [0.9]
    }
    def Mesh "FaceA" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:animationSource = </Model/AnimA>
        uniform token[] skel:blendShapes = ["smile"]
        rel skel:blendShapeTargets = [</Model/FaceA/Smile>]
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 1, 0), (0, 1, 0), (0, 1, 0)]
        }
    }
    def Mesh "FaceB" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:animationSource = </Model/AnimB>
        uniform token[] skel:blendShapes = ["smile"]
        rel skel:blendShapeTargets = [</Model/FaceB/Smile>]
        def BlendShape "Smile" {
            uniform vector3f[] offsets = [(0, 1, 0), (0, 1, 0), (0, 1, 0)]
        }
    }
}
"#;
    let scene = decode(usda);
    let weights: Vec<Vec<f32>> = scene
        .nodes
        .iter()
        .filter(|n| n.mesh.is_some())
        .map(|n| n.weights.clone())
        .collect();
    assert_eq!(weights.len(), 2, "two mesh nodes");
    assert!(
        weights.contains(&vec![0.2]) && weights.contains(&vec![0.9]),
        "each geometry keeps its own animation source's state, got {weights:?}"
    );

    // And the divergence survives a full round trip.
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    let w2: Vec<Vec<f32>> = s2
        .nodes
        .iter()
        .filter(|n| n.mesh.is_some())
        .map(|n| n.weights.clone())
        .collect();
    assert!(
        w2.contains(&vec![0.2]) && w2.contains(&vec![0.9]),
        "divergent states survive the round trip, got {w2:?}"
    );
}

fn morph_prim() -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    let mut t = MorphTarget::new();
    t.position = Some(vec![[0.0, 1.0, 0.0]; 3]);
    p.targets.push(t);
    p
}

/// A typed-model scene (no decoder stashes at all) with a
/// `Node::weights` override synthesizes a `BlendState_<id>` carrier
/// and survives the round trip.
#[test]
fn typed_model_node_weights_synthesize_blend_state() {
    let mut scene = Scene3D::new();
    let mesh_id = scene.add_mesh(Mesh::new(Some("M".into())).with_primitive(morph_prim()));
    let root = scene.add_node(
        Node::new()
            .with_name("M")
            .with_mesh(mesh_id)
            .with_weights(vec![0.4]),
    );
    scene.add_root(root);
    scene.validate().expect("input scene validates");

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report.usda.contains("def SkelAnimation \"BlendState_"),
        "synthesized carrier:\n{}",
        report.usda
    );
    assert!(
        report.usda.contains("float[] blendShapeWeights = [0.4]"),
        "static scalar emitted:\n{}",
        report.usda
    );
    assert!(
        report
            .usda
            .contains("rel skel:animationSource = </BlendState_"),
        "geometry binds the synthesized carrier:\n{}",
        report.usda
    );

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    let n2 = s2
        .nodes
        .iter()
        .find(|n| n.mesh.is_some() && !n.weights.is_empty())
        .expect("weighted mesh node after round trip");
    assert_eq!(n2.weights, vec![0.4]);
    s2.validate().expect("round-tripped scene validates");

    // One-cycle fixed point.
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
    assert_eq!(second, third, "fixed point after one cycle");
}

/// The flagship 0.0.5 use case: two nodes share ONE mesh with
/// divergent static blend states. The writer synthesizes one
/// `BlendState_<id>` per node, the reader scopes each back through
/// its `skel:animationSource`.
#[test]
fn divergent_states_over_shared_mesh_roundtrip() {
    let mut scene = Scene3D::new();
    let mesh_id = scene.add_mesh(Mesh::new(Some("M".into())).with_primitive(morph_prim()));
    let a = scene.add_node(
        Node::new()
            .with_name("A")
            .with_mesh(mesh_id)
            .with_weights(vec![0.2]),
    );
    let b = scene.add_node(
        Node::new()
            .with_name("B")
            .with_mesh(mesh_id)
            .with_weights(vec![0.9]),
    );
    scene.add_root(a);
    scene.add_root(b);
    scene.validate().expect("input scene validates");

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    let weights: Vec<Vec<f32>> = s2
        .nodes
        .iter()
        .filter(|n| n.mesh.is_some())
        .map(|n| n.weights.clone())
        .collect();
    assert_eq!(weights.len(), 2, "two mesh instances");
    assert!(
        weights.contains(&vec![0.2]) && weights.contains(&vec![0.9]),
        "divergent per-instance states survive, got {weights:?}"
    );
    s2.validate().expect("round-tripped scene validates");
}
