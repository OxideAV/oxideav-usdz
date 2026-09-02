//! decode → encode → decode fixed-point harness over the staged
//! fixtures under `docs/3d/usd/fixtures/` (skipped when the private
//! `docs/` checkout is absent — never `#[ignore]`).
//!
//! Every typed-model channel the decoder fills from a fixture must
//! survive one encode → decode cycle unchanged in *count* and — for
//! the channels pinned below — in *content*, and the second encode
//! must be byte-identical to the first (one-cycle fixed point). A
//! channel that degrades shows up here as a failing pin rather than
//! as silent loss.

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Scene3D};
use oxideav_usdz::{CompositionMode, UsdzDecoder, UsdzEncoder};

fn fixture(name: &str) -> Option<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures")
        .join(name);
    if !path.exists() {
        eprintln!("skip: fixture {} not present", path.display());
        return None;
    }
    Some(std::fs::read(&path).expect("read fixture"))
}

/// Structural + content digest of the typed model.
#[derive(Debug, PartialEq)]
struct Digest {
    roots: usize,
    nodes: usize,
    node_names: Vec<Option<String>>,
    meshes: usize,
    primitives: usize,
    vertices: usize,
    indices: usize,
    uv_sets: usize,
    normals: usize,
    joints: usize,
    morph_targets: usize,
    materials: usize,
    material_names: Vec<Option<String>>,
    textures: usize,
    skeletons: usize,
    skins: usize,
    animations: usize,
    channels: usize,
    keyframes: usize,
    audio_sources: usize,
    audio_emitters: usize,
    cameras: usize,
    lights: usize,
}

fn digest(scene: &Scene3D) -> Digest {
    let mut node_names: Vec<Option<String>> = scene.nodes.iter().map(|n| n.name.clone()).collect();
    node_names.sort();
    let mut material_names: Vec<Option<String>> =
        scene.materials.iter().map(|m| m.name.clone()).collect();
    material_names.sort();
    Digest {
        roots: scene.roots.len(),
        nodes: scene.nodes.len(),
        node_names,
        meshes: scene.meshes.len(),
        primitives: scene.meshes.iter().map(|m| m.primitives.len()).sum(),
        vertices: scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .map(|p| p.positions.len())
            .sum(),
        indices: scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .map(|p| p.indices.as_ref().map_or(0, |i| i.len()))
            .sum(),
        uv_sets: scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .map(|p| p.uvs.len())
            .sum(),
        normals: scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .filter(|p| p.normals.is_some())
            .count(),
        joints: scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .filter(|p| p.joints.is_some())
            .count(),
        morph_targets: scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .map(|p| p.targets.len())
            .sum(),
        materials: scene.materials.len(),
        material_names,
        textures: scene.textures.len(),
        skeletons: scene.skeletons.len(),
        skins: scene.skins.len(),
        animations: scene.animations.len(),
        channels: scene.animations.iter().map(|a| a.channels.len()).sum(),
        keyframes: scene
            .animations
            .iter()
            .flat_map(|a| &a.channels)
            .map(|c| c.sampler.keyframes.len())
            .sum(),
        audio_sources: scene.audio_sources.len(),
        audio_emitters: scene.audio_emitters.len(),
        cameras: scene.cameras.len(),
        lights: scene.lights.len(),
    }
}

/// Field-by-field view of two digests, so a degraded channel is
/// named in the failure.
fn digest_fields(a: &Digest, b: &Digest) -> Vec<(&'static str, String, String)> {
    vec![
        ("roots", format!("{:?}", a.roots), format!("{:?}", b.roots)),
        ("nodes", format!("{:?}", a.nodes), format!("{:?}", b.nodes)),
        (
            "node_names",
            format!("{:?}", a.node_names),
            format!("{:?}", b.node_names),
        ),
        (
            "meshes",
            format!("{:?}", a.meshes),
            format!("{:?}", b.meshes),
        ),
        (
            "primitives",
            format!("{:?}", a.primitives),
            format!("{:?}", b.primitives),
        ),
        (
            "vertices",
            format!("{:?}", a.vertices),
            format!("{:?}", b.vertices),
        ),
        (
            "indices",
            format!("{:?}", a.indices),
            format!("{:?}", b.indices),
        ),
        (
            "uv_sets",
            format!("{:?}", a.uv_sets),
            format!("{:?}", b.uv_sets),
        ),
        (
            "normals",
            format!("{:?}", a.normals),
            format!("{:?}", b.normals),
        ),
        (
            "joints",
            format!("{:?}", a.joints),
            format!("{:?}", b.joints),
        ),
        (
            "morph_targets",
            format!("{:?}", a.morph_targets),
            format!("{:?}", b.morph_targets),
        ),
        (
            "materials",
            format!("{:?}", a.materials),
            format!("{:?}", b.materials),
        ),
        (
            "material_names",
            format!("{:?}", a.material_names),
            format!("{:?}", b.material_names),
        ),
        (
            "textures",
            format!("{:?}", a.textures),
            format!("{:?}", b.textures),
        ),
        (
            "skeletons",
            format!("{:?}", a.skeletons),
            format!("{:?}", b.skeletons),
        ),
        ("skins", format!("{:?}", a.skins), format!("{:?}", b.skins)),
        (
            "animations",
            format!("{:?}", a.animations),
            format!("{:?}", b.animations),
        ),
        (
            "channels",
            format!("{:?}", a.channels),
            format!("{:?}", b.channels),
        ),
        (
            "keyframes",
            format!("{:?}", a.keyframes),
            format!("{:?}", b.keyframes),
        ),
        (
            "audio_sources",
            format!("{:?}", a.audio_sources),
            format!("{:?}", b.audio_sources),
        ),
        (
            "audio_emitters",
            format!("{:?}", a.audio_emitters),
            format!("{:?}", b.audio_emitters),
        ),
        (
            "cameras",
            format!("{:?}", a.cameras),
            format!("{:?}", b.cameras),
        ),
        (
            "lights",
            format!("{:?}", a.lights),
            format!("{:?}", b.lights),
        ),
    ]
}

fn cycle(archive: &[u8], label: &str) -> (Scene3D, Scene3D) {
    let s1 = UsdzDecoder::new().decode(archive).expect("first decode");
    for mode in [CompositionMode::Flatten, CompositionMode::Preserve] {
        let enc = UsdzEncoder::new().with_composition(mode);
        let out1 = enc.encode_bytes(&s1).expect("first encode");
        let s2 = UsdzDecoder::new().decode(&out1).expect("second decode");
        let out2 = enc.encode_bytes(&s2).expect("second encode");
        assert_eq!(
            out1.len(),
            out2.len(),
            "{label} ({mode:?}): second package size differs"
        );
        assert!(out1 == out2, "{label} ({mode:?}): second encode diverged");
        let d1 = digest(&s1);
        let d2 = digest(&s2);
        for (name, a, b) in digest_fields(&d1, &d2) {
            assert_eq!(
                a, b,
                "{label} ({mode:?}): `{name}` degraded across the cycle"
            );
        }
        // Positions survive exactly.
        for (m1, m2) in s1.meshes.iter().zip(&s2.meshes) {
            for (p1, p2) in m1.primitives.iter().zip(&m2.primitives) {
                assert_eq!(p1.positions, p2.positions, "{label}: positions");
                assert_eq!(p1.indices, p2.indices, "{label}: indices");
                assert_eq!(p1.normals, p2.normals, "{label}: normals");
                assert_eq!(p1.uvs, p2.uvs, "{label}: uvs");
            }
        }
        for (a1, a2) in s1.animations.iter().zip(&s2.animations) {
            for (c1, c2) in a1.channels.iter().zip(&a2.channels) {
                assert_eq!(
                    c1.sampler.keyframes, c2.sampler.keyframes,
                    "{label}: channel keyframes"
                );
            }
        }
    }
    let out = UsdzEncoder::new().encode_bytes(&s1).expect("encode");
    let s2 = UsdzDecoder::new().decode(&out).expect("decode");
    (s1, s2)
}

#[test]
fn elephant_crate_fixture_is_a_fixed_point() {
    let Some(bytes) = fixture("SoC-ElephantWithMonochord.usdc") else {
        return;
    };
    // The fixture's textures / audio are not staged: give every
    // referenced asset a distinct stub payload so pass-through can be
    // checked byte-for-byte.
    let layer = oxideav_usdz::usdc_layer::layer_from_usdc(&bytes).expect("layer");
    let mut assets: Vec<String> = Vec::new();
    fn collect(prims: &[oxideav_usdz::usda::Prim], out: &mut Vec<String>) {
        for p in prims {
            for a in p.attrs.values() {
                if let oxideav_usdz::usda::Value::Asset(path) = &a.value {
                    if !out.contains(path) {
                        out.push(path.clone());
                    }
                }
            }
            collect(&p.children, out);
        }
    }
    collect(&layer.prims, &mut assets);
    let stubs: Vec<Vec<u8>> = assets
        .iter()
        .enumerate()
        .map(|(i, a)| format!("stub-{i}-{a}").into_bytes())
        .collect();
    let mut entries = vec![UsdzEntry {
        name: "SoC-ElephantWithMonochord.usdc",
        payload: &bytes,
    }];
    for (a, stub) in assets.iter().zip(&stubs) {
        entries.push(UsdzEntry {
            name: a,
            payload: stub,
        });
    }
    let archive = build_usdz(&entries);
    let (s1, _) = cycle(&archive, "Elephant");
    let d = digest(&s1);
    // Pins on the decoded content (what the round trip must carry).
    assert_eq!(d.skeletons, 2, "two SkelRoots: {d:?}");
    assert_eq!(d.audio_sources, 1, "{d:?}");
    assert_eq!(d.textures, 4, "{d:?}");
    assert!(d.animations >= 1, "{d:?}");
    assert!(d.meshes >= 1, "{d:?}");
    assert!(d.materials >= 1, "{d:?}");
}

#[test]
fn crate_variant_specs_fixture_is_a_fixed_point() {
    let Some(bytes) = fixture("crate-variant-specs.usdc") else {
        return;
    };
    let archive = build_usdz(&[UsdzEntry {
        name: "crate-variant-specs.usdc",
        payload: &bytes,
    }]);
    let (s1, s2) = cycle(&archive, "crate-variant-specs");
    // The selected variants' opinions and the unselected bodies both
    // survive: the scale from `sizeVariant = small` is on Child.
    for s in [&s1, &s2] {
        let child = s
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Child"))
            .expect("Child");
        assert!(
            matches!(child.transform, oxideav_mesh3d::Transform::Trs { scale, .. } if (scale[0] - 0.5).abs() < 1e-6)
        );
        assert!(child.extras.contains_key("usd:variantSets"));
    }
}

#[test]
fn usdz_alignment_fixture_is_a_fixed_point() {
    let Some(bytes) = fixture("usdz-alignment.usdz") else {
        return;
    };
    let (s1, _) = cycle(&bytes, "usdz-alignment");
    assert!(digest(&s1).nodes >= 1);
}
