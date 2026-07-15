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

#[test]
fn specular_workflow_lands_on_typed_slot() {
    let scene = decode(EXPANDED_USDA);
    let mat = &scene.materials[0];
    let spec = mat
        .ext
        .specular
        .as_ref()
        .expect("active workflow on the typed slot");
    assert!(approx(spec.color_factor[0], 0.9));
    assert!(approx(spec.color_factor[1], 0.8));
    assert!(approx(spec.color_factor[2], 0.7));
    assert!(
        approx(spec.factor, 1.0),
        "no USD counterpart — neutral default"
    );
    assert!(
        !mat.extras.contains_key("usd:useSpecularWorkflow")
            && !mat.extras.contains_key("usd:inputs:specularColor"),
        "typed slot replaces the extras shims"
    );
}

#[test]
fn inert_specular_color_stays_on_extras() {
    // `specularColor` authored while the workflow is off is inert
    // (schema §2.1: ignored when `useSpecularWorkflow = 0`) — it must
    // not activate the typed slot, but the authored opinion still
    // round-trips via extras.
    let usda = r#"#usda 1.0
def Material "Mat" {
    def Shader "Surface" {
        uniform token info:id = "UsdPreviewSurface"
        color3f inputs:specularColor = (0.3, 0.2, 0.1)
        token outputs:surface
    }
}
"#;
    let scene = decode(usda);
    let mat = &scene.materials[0];
    assert_eq!(
        mat.ext.specular, None,
        "inert color must not activate the workflow"
    );
    let sc = mat
        .extras
        .get("usd:inputs:specularColor")
        .and_then(|v| v.as_array())
        .expect("inert color preserved on extras");
    assert!(approx(sc[0].as_f64().unwrap() as f32, 0.3));

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    assert_eq!(s2.materials[0].ext.specular, None);
    assert!(s2.materials[0]
        .extras
        .contains_key("usd:inputs:specularColor"));
    let out = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    assert!(
        !out.contains("useSpecularWorkflow"),
        "workflow selector must not be synthesised"
    );
}

#[test]
fn specular_color_texture_lands_on_typed_slot() {
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
            int inputs:useSpecularWorkflow = 1
            color3f inputs:specularColor.connect = </Root/Mat/SpecTex.outputs:rgb>
            token outputs:surface
        }
        def Shader "SpecTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @spec.png@
            float3 outputs:rgb
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
            name: "spec.png",
            payload: b"SPEC-PIXEL-DATA",
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let mat = &scene.materials[0];
    let spec = mat.ext.specular.as_ref().expect("typed workflow");
    assert!(spec.color_texture.is_some(), "F0 map on the typed slot");
    assert!(
        !mat.extras.contains_key("usd:tex:specularColor"),
        "typed slot replaces the extras shim"
    );

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    assert!(s2.materials[0]
        .ext
        .specular
        .as_ref()
        .expect("workflow survives")
        .color_texture
        .is_some());
    let first = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    assert_eq!(first, second, "specular-texture round trip fixed point");
}

#[test]
fn clearcoat_ior_occlusion_mapped() {
    let scene = decode(EXPANDED_USDA);
    let mat = &scene.materials[0];
    let cc = mat
        .ext
        .clearcoat
        .as_ref()
        .expect("authored clearcoat on the typed slot");
    assert!(approx(cc.factor, 0.6));
    assert!(approx(cc.roughness, 0.2));
    assert!(
        !mat.extras.contains_key("usd:inputs:clearcoat")
            && !mat.extras.contains_key("usd:inputs:clearcoatRoughness"),
        "typed slot replaces the extras shim"
    );
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
    // Only `clearcoat` authored — the typed lobe carries the schema
    // default roughness (0.01) for the consumer, but the writer must
    // not materialise a synthetic `clearcoatRoughness` opinion in the
    // output file.
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
    let cc = mat.ext.clearcoat.as_ref().expect("typed clearcoat lobe");
    assert!(approx(cc.factor, 1.0));
    assert!(
        approx(cc.roughness, 0.01),
        "unauthored roughness evaluates to the schema default"
    );
    let out = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    assert!(out.contains("inputs:clearcoat = 1"));
    assert!(
        !out.contains("inputs:clearcoatRoughness"),
        "unauthored input must not be synthesised in the output"
    );
}

#[test]
fn unauthored_clearcoat_stays_none() {
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
    assert_eq!(
        scene.materials[0].ext.clearcoat, None,
        "no clearcoat opinion, no lobe"
    );
}

#[test]
fn clearcoat_texture_connections_land_on_typed_slot() {
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
            float inputs:clearcoat.connect = </Root/Mat/CcTex.outputs:r>
            float inputs:clearcoatRoughness.connect = </Root/Mat/CcTex.outputs:g>
            token outputs:surface
        }
        def Shader "CcTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @cc.png@
            float outputs:r
            float outputs:g
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
            name: "cc.png",
            payload: b"CC-PIXEL-DATA",
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let mat = &scene.materials[0];
    let cc = mat.ext.clearcoat.as_ref().expect("typed clearcoat lobe");
    assert!(cc.factor_texture.is_some(), "factor map on the typed slot");
    assert!(
        cc.roughness_texture.is_some(),
        "roughness map on the typed slot"
    );
    assert!(
        !mat.extras.contains_key("usd:tex:clearcoat")
            && !mat.extras.contains_key("usd:tex:clearcoatRoughness"),
        "typed slots replace the extras shims"
    );
    assert_eq!(scene.textures.len(), 1);

    // Write → re-read: connections and the shared UsdUVTexture prim
    // survive, and the second encode is a fixed point.
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    let cc2 = s2.materials[0]
        .ext
        .clearcoat
        .as_ref()
        .expect("typed lobe survives");
    assert!(cc2.factor_texture.is_some() && cc2.roughness_texture.is_some());
    assert_eq!(s2.textures.len(), 1);
    let first = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    assert_eq!(first, second, "clearcoat-texture round trip fixed point");
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
    for key in ["usd:inputs:displacement", "usd:inputs:normal"] {
        assert_eq!(a.extras.get(key), b.extras.get(key), "extras `{key}`");
    }
    assert_eq!(a.ext.ior, b.ext.ior, "typed ior survives the round trip");
    assert_eq!(
        a.ext.clearcoat, b.ext.clearcoat,
        "typed clearcoat survives the round trip"
    );
    assert_eq!(
        a.ext.specular, b.ext.specular,
        "typed specular survives the round trip"
    );
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
