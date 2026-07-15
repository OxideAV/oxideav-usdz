//! Expanded `UsdPreviewSurface` input coverage (staged schema §2.1,
//! `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`): specular
//! workflow, clearcoat lobe, IOR, opacity threshold (cutout),
//! occlusion multiplier, displacement + constant normal
//! preservation, and metallic/roughness texture connections into the
//! packed slot — plus the write → re-read round trip for all of it.

mod common;

use oxideav_mesh3d::{AlphaMode, Mesh3DDecoder};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const EXPANDED_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)
def Xform "Root" {
    def Mesh "M" {
        rel material:binding = </Root/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Material "Mat" {
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.5, 0.4, 0.3)
            int inputs:useSpecularWorkflow = 1
            color3f inputs:specularColor = (0.9, 0.8, 0.7)
            float inputs:clearcoat = 0.6
            float inputs:clearcoatRoughness = 0.2
            float inputs:ior = 1.45
            float inputs:opacity = 0.8
            float inputs:opacityThreshold = 0.25
            float inputs:occlusion = 0.75
            float inputs:displacement = 0.125
            normal3f inputs:normal = (0, 1, 0)
            token outputs:surface
        }
    }
}
"#;

fn decode(usda: &str) -> oxideav_mesh3d::Scene3D {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    UsdzDecoder::new().decode_bytes(&usdz).unwrap()
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

fn extras_f32(mat: &oxideav_mesh3d::Material, key: &str) -> Option<f32> {
    mat.extras
        .get(key)
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
}

#[test]
fn specular_workflow_preserved_on_extras() {
    let scene = decode(EXPANDED_USDA);
    let mat = &scene.materials[0];
    assert_eq!(
        mat.extras
            .get("usd:useSpecularWorkflow")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    let sc: Vec<f64> = mat
        .extras
        .get("usd:inputs:specularColor")
        .and_then(|v| v.as_array())
        .expect("specularColor preserved")
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();
    assert!(approx(sc[0] as f32, 0.9));
    assert!(approx(sc[1] as f32, 0.8));
    assert!(approx(sc[2] as f32, 0.7));
}

#[test]
fn clearcoat_ior_occlusion_mapped() {
    let scene = decode(EXPANDED_USDA);
    let mat = &scene.materials[0];
    assert!(approx(
        extras_f32(mat, "usd:inputs:clearcoat").unwrap(),
        0.6
    ));
    assert!(approx(
        extras_f32(mat, "usd:inputs:clearcoatRoughness").unwrap(),
        0.2
    ));
    assert!(
        approx(mat.ext.ior.expect("authored ior on the typed slot"), 1.45),
        "ior lands on MaterialExt::ior"
    );
    assert!(
        !mat.extras.contains_key("usd:inputs:ior"),
        "typed slot replaces the extras shim"
    );
    assert!(approx(mat.occlusion_strength, 0.75));
}

#[test]
fn unauthored_ior_stays_none() {
    let usda = r#"#usda 1.0
def Material "Mat" {
    def Shader "Surface" {
        uniform token info:id = "UsdPreviewSurface"
        color3f inputs:diffuseColor = (1, 0, 0)
        token outputs:surface
    }
}
"#;
    let scene = decode(usda);
    let mat = &scene.materials[0];
    assert_eq!(
        mat.ext.ior, None,
        "unauthored ior must not synthesise the schema default"
    );
    assert!(approx(mat.effective_ior(), 1.5), "consumer-side default");
}

#[test]
fn explicit_default_ior_round_trips() {
    // An explicit `ior = 1.5` (the schema default) is an authored
    // opinion — the Option-shaped typed slot keeps it distinguishable
    // from absence, so the writer re-emits it.
    let usda = r#"#usda 1.0
def Material "Mat" {
    def Shader "Surface" {
        uniform token info:id = "UsdPreviewSurface"
        float inputs:ior = 1.5
        token outputs:surface
    }
}
"#;
    let scene = decode(usda);
    assert!(approx(scene.materials[0].ext.ior.unwrap(), 1.5));
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    assert!(approx(s2.materials[0].ext.ior.unwrap(), 1.5));
}

#[test]
fn opacity_threshold_selects_mask_alpha() {
    let scene = decode(EXPANDED_USDA);
    let mat = &scene.materials[0];
    match mat.alpha_mode {
        AlphaMode::Mask { cutoff } => assert!(approx(cutoff, 0.25)),
        other => panic!("expected Mask alpha mode, got {other:?}"),
    }
    assert!(approx(mat.base_color[3], 0.8));
}

#[test]
fn sub_one_opacity_without_threshold_selects_blend() {
    let usda = EXPANDED_USDA.replace("float inputs:opacityThreshold = 0.25\n", "");
    let scene = decode(&usda);
    assert_eq!(scene.materials[0].alpha_mode, AlphaMode::Blend);
}

#[test]
fn displacement_and_constant_normal_preserved_on_extras() {
    let scene = decode(EXPANDED_USDA);
    let mat = &scene.materials[0];
    assert!(approx(
        mat.extras
            .get("usd:inputs:displacement")
            .and_then(|v| v.as_f64())
            .unwrap() as f32,
        0.125
    ));
    let nrm = mat
        .extras
        .get("usd:inputs:normal")
        .and_then(|v| v.as_array())
        .expect("normal preserved");
    assert_eq!(nrm.len(), 3);
    assert!(approx(nrm[1].as_f64().unwrap() as f32, 1.0));
}

#[test]
fn only_authored_clearcoat_inputs_preserved() {
    // Only `clearcoat` authored — the unauthored roughness must not
    // materialise a synthetic opinion (the schema default 0.01 is the
    // consumer's job, not the file's).
    let usda = r#"#usda 1.0
def Material "Mat" {
    def Shader "Surface" {
        uniform token info:id = "UsdPreviewSurface"
        float inputs:clearcoat = 1
        token outputs:surface
    }
}
"#;
    let scene = decode(usda);
    let mat = &scene.materials[0];
    assert!(approx(
        extras_f32(mat, "usd:inputs:clearcoat").unwrap(),
        1.0
    ));
    assert!(
        !mat.extras.contains_key("usd:inputs:clearcoatRoughness"),
        "unauthored input must not be synthesised"
    );
}

#[test]
fn metallic_roughness_connections_share_packed_slot() {
    let usda = r#"#usda 1.0
def Xform "Root" {
    def Mesh "M" {
        rel material:binding = </Root/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Material "Mat" {
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
            float inputs:metallic.connect = </Root/Mat/OrmTex.outputs:b>
            float inputs:roughness.connect = </Root/Mat/OrmTex.outputs:g>
            token outputs:surface
        }
        def Shader "OrmTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @orm.png@
            float outputs:g
            float outputs:b
        }
    }
}
"#;
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: usda.as_bytes(),
        },
        common::UsdzEntry {
            name: "orm.png",
            payload: b"ORM-PIXEL-DATA",
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let mat = &scene.materials[0];
    assert!(mat.metallic_roughness_texture.is_some());
    assert_eq!(
        mat.extras.get("usd:mr_connect").and_then(|v| v.as_str()),
        Some("both"),
        "same texture on both inputs collapses to one packed slot"
    );
    assert_eq!(scene.textures.len(), 1);
}

#[test]
fn expanded_material_round_trips() {
    let scene = decode(EXPANDED_USDA);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    let a = &scene.materials[0];
    let b = &s2.materials[0];

    assert_eq!(a.name, b.name);
    for i in 0..4 {
        assert!(approx(a.base_color[i], b.base_color[i]), "base_color[{i}]");
    }
    assert_eq!(a.alpha_mode, b.alpha_mode);
    assert!(approx(a.occlusion_strength, b.occlusion_strength));
    for key in [
        "usd:useSpecularWorkflow",
        "usd:inputs:specularColor",
        "usd:inputs:clearcoat",
        "usd:inputs:clearcoatRoughness",
        "usd:inputs:displacement",
        "usd:inputs:normal",
    ] {
        assert_eq!(a.extras.get(key), b.extras.get(key), "extras `{key}`");
    }
    assert_eq!(a.ext.ior, b.ext.ior, "typed ior survives the round trip");
}

#[test]
fn second_encode_of_expanded_material_is_stable() {
    let scene = decode(EXPANDED_USDA);
    let first = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("decode ok");
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    assert_eq!(
        first, second,
        "expanded-material round-trip must be a fixed point"
    );
}
