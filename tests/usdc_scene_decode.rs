//! End-to-end `.usdc` → `Scene3D`: the committed Elephant Crate
//! fixture bridges through `usdc_layer::layer_from_usdc` into the
//! same `usda::Layer` model the text parser produces, and a USDZ
//! archive whose default layer is that `.usdc` decodes to a full
//! typed scene through the ordinary `UsdzDecoder` pipeline.

use oxideav_usdz::usda::{Prim, Value};
use oxideav_usdz::usdc_layer::layer_from_usdc;
use oxideav_usdz::zip_writer::Writer;
use oxideav_usdz::UsdzDecoder;

fn elephant_bytes() -> Option<Vec<u8>> {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/SoC-ElephantWithMonochord.usdc");
    if !fixture.exists() {
        eprintln!("skip: fixture {fixture:?} not present");
        return None;
    }
    Some(std::fs::read(&fixture).expect("read Elephant fixture"))
}

fn find_prim<'a>(prims: &'a [Prim], name: &str) -> Option<&'a Prim> {
    for p in prims {
        if p.name == name {
            return Some(p);
        }
        if let Some(hit) = find_prim(&p.children, name) {
            return Some(hit);
        }
    }
    None
}

fn collect_assets(prims: &[Prim], out: &mut Vec<String>) {
    for p in prims {
        for attr in p.attrs.values() {
            if let Value::Asset(a) = &attr.value {
                if !out.contains(a) {
                    out.push(a.clone());
                }
            }
        }
        collect_assets(&p.children, out);
    }
}

#[test]
fn elephant_usdc_bridges_to_a_full_layer() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    let layer = layer_from_usdc(&bytes).expect("bridge .usdc to Layer");

    // Layer metadata straight from the pseudo-root spec.
    assert_eq!(
        layer.metadata.get("defaultPrim"),
        Some(&Value::Token("SoC_ElephantWithMonochord".to_owned()))
    );
    assert_eq!(
        layer.metadata.get("upAxis"),
        Some(&Value::Token("Y".to_owned()))
    );
    assert_eq!(
        layer.metadata.get("metersPerUnit"),
        Some(&Value::Float(0.01))
    );

    // One root prim, correctly typed.
    assert_eq!(layer.prims.len(), 1);
    let root = &layer.prims[0];
    assert_eq!(root.name, "SoC_ElephantWithMonochord");
    assert_eq!(root.type_name, "Xform");
    assert_eq!(root.spec, "def");

    // The mesh prim decodes with its full attribute set.
    let mesh = find_prim(&layer.prims, "Elefant1").expect("Elefant1 mesh prim");
    assert_eq!(mesh.type_name, "Mesh");
    let points = &mesh.attrs["points"];
    assert_eq!(points.type_token, "point3f[]");
    let Value::Array(pts) = &points.value else {
        panic!("points is an array");
    };
    assert_eq!(pts.len(), 1312, "vertex count");
    let Value::Array(fvi) = &mesh.attrs["faceVertexIndices"].value else {
        panic!("faceVertexIndices is an array");
    };
    assert_eq!(fvi.len(), 6192);
    let Value::Array(fvc) = &mesh.attrs["faceVertexCounts"].value else {
        panic!("faceVertexCounts is an array");
    };
    assert_eq!(fvc.len(), 2064);
    // Compressed int[] content: all triangles.
    assert!(fvc.iter().all(|v| *v == Value::Float(3.0)), "triangulated");
    // Rel targets bridge as absolute paths.
    let Value::Path(binding) = &mesh.attrs["material:binding"].value else {
        panic!("material:binding is a path");
    };
    assert_eq!(
        binding,
        "/SoC_ElephantWithMonochord/Materials/Elefant_Mat_68050"
    );

    // Preview-surface shader network with texture connection.
    let surface = find_prim(&layer.prims, "PreviewSurface").expect("PreviewSurface shader");
    assert_eq!(
        surface.attrs["info:id"].value,
        Value::Token("UsdPreviewSurface".to_owned())
    );
    assert!(matches!(
        surface.attrs["inputs:diffuseColor.connect"].value,
        Value::Path(_)
    ));

    // Skeleton + animation bridge with time samples.
    let skel = find_prim(&layer.prims, "_skel").expect("skeleton prim");
    assert_eq!(skel.type_name, "Skeleton");
    let Value::Array(joints) = &skel.attrs["joints"].value else {
        panic!("joints is an array");
    };
    assert_eq!(joints.len(), 27);
    let anim = find_prim(&layer.prims, "_anim").expect("animation prim");
    assert_eq!(anim.type_name, "SkelAnimation");
    let Value::TimeSamples(rot) = &anim.attrs["rotations.timeSamples"].value else {
        panic!("rotations.timeSamples present");
    };
    assert_eq!(rot.len(), 3023);

    // Spatial audio prim.
    let audio = find_prim(&layer.prims, "CharacterAudioSource").expect("audio prim");
    assert_eq!(audio.type_name, "SpatialAudio");
    assert_eq!(
        audio.attrs["filePath"].value,
        Value::Asset("0/Elefant.mp3".to_owned())
    );
}

#[test]
fn usdz_with_usdc_default_layer_decodes_to_scene3d() {
    let Some(bytes) = elephant_bytes() else {
        return;
    };
    // Referenced companion assets must exist in the archive for the
    // self-contained-container rule; stub them with tiny payloads.
    let layer = layer_from_usdc(&bytes).expect("bridge for asset roster");
    let mut assets = Vec::new();
    collect_assets(&layer.prims, &mut assets);
    assert!(
        assets.contains(&"0/Elefant_Diff.packed.png".to_owned()),
        "texture roster: {assets:?}"
    );

    let mut writer = Writer::new();
    writer.add_stored("model.usdc", &bytes);
    for asset in &assets {
        writer.add_stored(asset, b"stub-payload");
    }
    let usdz = writer.finish();

    let scene = UsdzDecoder::new()
        .decode_bytes(&usdz)
        .expect("decode USDZ with .usdc default layer");

    // Two materials, at least two meshes, a 27-joint skeleton, and
    // the sampled animation all arrive in the typed model.
    assert!(
        scene.meshes.len() >= 2,
        "meshes materialised: {}",
        scene.meshes.len()
    );
    assert!(
        scene
            .meshes
            .iter()
            .flat_map(|m| m.primitives.iter())
            .any(|p| p.positions.len() == 1312),
        "the 1312-vertex Elefant1 primitive is present"
    );
    assert!(scene.materials.len() >= 2, "both materials arrive");
    assert!(
        scene.skeletons.iter().any(|s| s.joints.len() == 27),
        "27-joint skeleton"
    );
    assert!(!scene.animations.is_empty(), "sampled animation arrives");
    scene.validate().expect("bridged scene passes validate()");
}
