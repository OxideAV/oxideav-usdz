//! The `oxideav-mesh3d` 0.0.6 typed morph surfaces on the encode
//! side: `Mesh::target_names` → `skel:blendShapes`,
//! `MorphTarget::inbetweens` → §1.4.1 `inbetweens:<name>` attributes
//! with `weight` metadata, a sampled `MorphWeights` channel built
//! through `AnimationSampler::morph_weights` → `blendShapeWeights`
//! time samples — all authored purely through the typed model (no
//! decoder stashes), round-tripped through the reader with lossless
//! read-back and pinned as a one-cycle fixed point.

mod common;

use oxideav_mesh3d::{
    Animation, AnimationProperty, AnimationSampler, Inbetween, Indices, Interpolation, Mesh,
    MorphTarget, Node, NodeId, Primitive, Scene3D, Topology,
};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

fn tri() -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    p
}

/// Two channels: `smile` with a named in-between at 0.5 (position +
/// normal deltas), `frown` plain.
fn morph_prim() -> Primitive {
    let mut p = tri();
    let mut smile = MorphTarget::with_deltas(Some(vec![[0.0, 1.0, 0.0]; 3]), None, None);
    smile.inbetweens.push(
        Inbetween::new(0.5)
            .with_name("half")
            .with_position(vec![[0.0, 0.7, 0.0]; 3])
            .with_normal(vec![[0.0, 0.0, 0.2]; 3]),
    );
    p.targets.push(smile);
    p.targets.push(MorphTarget::with_deltas(
        Some(vec![[0.0, -1.0, 0.0]; 3]),
        None,
        None,
    ));
    p
}

fn typed_scene(prim: Primitive, names: &[&str]) -> (Scene3D, NodeId) {
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("Face".into()))
        .with_primitive(prim)
        .with_target_names(names.iter().copied());
    let mesh_id = scene.add_mesh(mesh);
    let node = scene.add_node(Node::new().with_name("Face").with_mesh(mesh_id));
    scene.add_root(node);
    (scene, node)
}

fn morph_channel(scene: &Scene3D, node: NodeId) -> &AnimationSampler {
    scene
        .animations
        .iter()
        .find_map(|a| a.channel_for(node, AnimationProperty::MorphWeights))
        .map(|ch| &ch.sampler)
        .expect("morph channel")
}

fn mesh_node(scene: &Scene3D) -> NodeId {
    NodeId(
        scene
            .nodes
            .iter()
            .position(|n| n.mesh.is_some())
            .expect("mesh node") as u32,
    )
}

#[test]
fn typed_roster_inbetweens_and_sampled_weights_encode_and_read_back() {
    let (mut scene, node) = typed_scene(morph_prim(), &["smile", "frown"]);
    let sampler = AnimationSampler::morph_weights(
        vec![0.0, 0.5, 1.0],
        vec![vec![0.0, 0.0], vec![0.5, 0.25], vec![1.0, 0.0]],
        Interpolation::Linear,
    )
    .expect("well-formed sampler");
    scene.add_animation(Animation::new("blink".to_owned()).with_channel(
        node,
        AnimationProperty::MorphWeights,
        sampler,
    ));
    scene.validate().expect("typed input validates");

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    let usda = &report.usda;
    assert!(
        usda.contains("uniform token[] skel:blendShapes = [\"smile\", \"frown\"]"),
        "typed target names author the roster:\n{usda}"
    );
    assert!(
        usda.contains("uniform vector3f[] inbetweens:half = ["),
        "typed in-between authors the §1.4.1 attribute:\n{usda}"
    );
    assert!(usda.contains("(weight = 0.5)"), "{usda}");
    assert!(
        usda.contains("uniform token[] blendShapes = [\"smile\", \"frown\"]"),
        "animation roster:\n{usda}"
    );
    assert!(
        usda.contains("0: [0, 0], 12: [0.5, 0.25], 24: [1, 0]"),
        "per-keyframe vectors emit verbatim:\n{usda}"
    );
    // §1.4.2: no spelling for the in-between normal-offsets
    // attribute is published, so a typed-model normal delta set has
    // no wire form and is never constructed.
    assert!(!usda.contains("inbetweens:half:"), "{usda}");

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    s2.validate().expect("decoded scene validates");
    let n2 = mesh_node(&s2);
    let m2 = &s2.meshes[0];
    assert_eq!(m2.target_names, vec!["smile".to_string(), "frown".into()]);
    assert_eq!(m2.find_target("frown"), Some(1));
    let t2 = &m2.primitives[0].targets[0];
    assert_eq!(t2.inbetweens.len(), 1);
    assert_eq!(t2.inbetweens[0].name.as_deref(), Some("half"));
    assert!(approx(t2.inbetweens[0].weight, 0.5));
    assert_eq!(
        t2.inbetweens[0].position,
        scene.meshes[0].primitives[0].targets[0].inbetweens[0].position
    );
    assert!(t2.inbetweens[0].normal.is_none(), "no wire form (§1.4.2)");
    let s2_sampler = morph_channel(&s2, n2);
    assert_eq!(s2_sampler.keyframes, vec![0.0, 0.5, 1.0]);
    assert_eq!(
        s2_sampler.morph_weight_frames().unwrap(),
        vec![&[0.0f32, 0.0][..], &[0.5, 0.25][..], &[1.0, 0.0][..]]
    );
    // The typed §1.4.1 resolution survives: at the station the
    // decoded target reproduces the in-between exactly.
    let at_half = t2.at_weight(0.5);
    assert!(approx(at_half.position.as_ref().unwrap()[0][1], 0.7));

    // One-cycle fixed point.
    let bytes2 = UsdzEncoder::new().encode_bytes(&s2).expect("encode ok");
    let s3 = UsdzDecoder::new().decode_bytes(&bytes2).expect("decode ok");
    let second = UsdzEncoder::new().encode_with_report(&s2).unwrap().usda;
    let third = UsdzEncoder::new().encode_with_report(&s3).unwrap().usda;
    assert_eq!(second, third, "fixed point after one cycle");
}

#[test]
fn unnamed_targets_and_anonymous_inbetweens_get_deterministic_names() {
    let mut prim = tri();
    let mut t = MorphTarget::with_deltas(Some(vec![[1.0, 0.0, 0.0]; 3]), None, None);
    t.inbetweens
        .push(Inbetween::new(0.25).with_position(vec![[0.4, 0.0, 0.0]; 3]));
    prim.targets.push(t);
    let (mut scene, node) = typed_scene(prim, &[]);
    scene.nodes[node.0 as usize].weights = vec![0.75];
    scene.validate().expect("typed input validates");

    let usda = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    assert!(
        usda.contains("uniform token[] skel:blendShapes = [\"shape_0\"]"),
        "{usda}"
    );
    assert!(
        usda.contains("uniform vector3f[] inbetweens:inbetween_0 = ["),
        "{usda}"
    );
    assert!(usda.contains("(weight = 0.25)"), "{usda}");
    assert!(
        usda.contains("float[] blendShapeWeights = [0.75]"),
        "{usda}"
    );

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    s2.validate().expect("decoded scene validates");
    assert_eq!(s2.meshes[0].target_names, vec!["shape_0".to_string()]);
    let t2 = &s2.meshes[0].primitives[0].targets[0];
    assert_eq!(t2.inbetweens[0].name.as_deref(), Some("inbetween_0"));
    assert!(approx(t2.inbetweens[0].weight, 0.25));
    let n2 = mesh_node(&s2);
    assert_eq!(s2.nodes[n2.0 as usize].weights, vec![0.75]);
}

#[test]
fn static_node_weights_use_typed_target_names() {
    let (mut scene, node) = typed_scene(morph_prim(), &["smile", "frown"]);
    scene.nodes[node.0 as usize].weights = vec![0.3, 0.6];
    let usda = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    assert!(
        usda.contains("uniform token[] blendShapes = [\"smile\", \"frown\"]"),
        "{usda}"
    );
    assert!(
        usda.contains("float[] blendShapeWeights = [0.3, 0.6]"),
        "{usda}"
    );
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    let n2 = mesh_node(&s2);
    assert_eq!(s2.nodes[n2.0 as usize].weights, vec![0.3, 0.6]);
    assert_eq!(
        s2.effective_morph_weights(n2).map(<[f32]>::to_vec),
        Some(vec![0.3, 0.6])
    );
}

/// A typed-model in-between at an implicit endpoint is a §1.4.1
/// authoring error the writer still expresses (the model's content
/// is the model's content); the reader then applies the documented
/// error-but-continue rule — the station drops from the typed
/// roster, replays verbatim, and the decoded scene validates.
#[test]
fn endpoint_inbetween_round_trips_as_preserved_authoring_error() {
    let mut prim = tri();
    let mut t = MorphTarget::with_deltas(Some(vec![[1.0, 0.0, 0.0]; 3]), None, None);
    t.inbetweens.push(
        Inbetween::new(1.0)
            .with_name("atOne")
            .with_position(vec![[9.0, 0.0, 0.0]; 3]),
    );
    prim.targets.push(t);
    let (scene, _) = typed_scene(prim, &["s"]);
    assert!(
        scene.validate().is_err(),
        "the typed model reports the endpoint station"
    );
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    s2.validate().expect("reader drops the malformed station");
    assert!(s2.meshes[0].primitives[0].targets[0].inbetweens.is_empty());
    let usda = UsdzEncoder::new().encode_with_report(&s2).unwrap().usda;
    assert!(
        usda.contains("inbetweens:atOne"),
        "replays verbatim: {usda}"
    );
    assert!(usda.contains("(weight = 1)"), "{usda}");
}

/// `Step` samplers have no USD `timeSamples` form (USD interpolates
/// samples linearly); the stored frames still emit verbatim and the
/// reader yields a `Linear` sampler over the same frames.
#[test]
fn step_sampler_frames_emit_verbatim() {
    let (mut scene, node) = typed_scene(morph_prim(), &["smile", "frown"]);
    let sampler = AnimationSampler::morph_weights(
        vec![0.0, 1.0],
        vec![vec![0.0, 1.0], vec![1.0, 0.0]],
        Interpolation::Step,
    )
    .unwrap();
    scene.add_animation(Animation::new("step".to_owned()).with_channel(
        node,
        AnimationProperty::MorphWeights,
        sampler,
    ));
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode_bytes(&bytes).expect("decode ok");
    let s = morph_channel(&s2, mesh_node(&s2));
    assert_eq!(s.interpolation, Interpolation::Linear);
    assert_eq!(
        s.morph_weight_frames().unwrap(),
        vec![&[0.0f32, 1.0][..], &[1.0, 0.0][..]]
    );
}
