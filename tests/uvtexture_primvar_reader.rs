//! Expanded `UsdUVTexture` (§2.2) + `UsdPrimvarReader` (§2.3) +
//! gprim display attributes (§2.5) coverage per the staged schema
//! (`docs/3d/usd/usdskel-usdpreviewsurface-schema.md`): wrap-mode
//! mapping, scale/bias/fallback/sourceColorSpace preservation,
//! `varname`-driven UV-set selection, multi-UV meshes, `doubleSided`,
//! and `displayColor` / `displayOpacity` — with write → re-read
//! round trips.

mod common;

use oxideav_mesh3d::WrapMode;
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const UV_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)
def Xform "Root" {
    def Mesh "M" {
        rel material:binding = </Root/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        texCoord2f[] primvars:st = [(0,0), (1,0), (0,1)]
        texCoord2f[] primvars:st1 = [(0.5,0.5), (0.75,0.5), (0.5,0.75)]
        uniform bool doubleSided = 1
        color3f[] primvars:displayColor = [(1,0,0), (0,1,0), (0,0,1)]
        float[] primvars:displayOpacity = [1, 0.5, 0.25]
    }
    def Material "Mat" {
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Root/Mat/Tex.outputs:rgb>
            token outputs:surface
        }
        def Shader "Tex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @diffuse.png@
            token inputs:wrapS = "clamp"
            token inputs:wrapT = "mirror"
            float4 inputs:scale = (2, 2, 2, 1)
            float4 inputs:bias = (-1, -1, -1, 0)
            float4 inputs:fallback = (0.5, 0.5, 0.5, 1)
            token inputs:sourceColorSpace = "sRGB"
            float2 inputs:st.connect = </Root/Mat/StReader.outputs:result>
            float3 outputs:rgb
        }
        def Shader "StReader" {
            uniform token info:id = "UsdPrimvarReader_float2"
            string inputs:varname = "st1"
            float2 outputs:result
        }
    }
}
"#;

fn decode_uv_scene() -> oxideav_mesh3d::Scene3D {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: UV_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: b"PIXELS",
        },
    ]);
    UsdzDecoder::new().decode_bytes(&usdz).unwrap()
}

#[test]
fn wrap_tokens_map_to_sampler() {
    let scene = decode_uv_scene();
    let tex = &scene.textures[0];
    assert_eq!(tex.sampler.wrap_s, WrapMode::ClampToEdge);
    assert_eq!(tex.sampler.wrap_t, WrapMode::MirroredRepeat);
}

#[test]
fn scale_bias_fallback_colorspace_preserved() {
    let scene = decode_uv_scene();
    let stash = scene
        .extras
        .get("usd:uvtexture:0")
        .and_then(|v| v.as_object())
        .expect("uvtexture stash present");
    assert_eq!(
        stash.get("sourceColorSpace").and_then(|v| v.as_str()),
        Some("sRGB")
    );
    let scale: Vec<f64> = stash["scale"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();
    assert_eq!(scale, vec![2.0, 2.0, 2.0, 1.0]);
    let bias: Vec<f64> = stash["bias"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();
    assert_eq!(bias, vec![-1.0, -1.0, -1.0, 0.0]);
    assert!(stash.contains_key("fallback"));
}

#[test]
fn primvar_reader_varname_selects_uv_set() {
    let scene = decode_uv_scene();
    let mat = &scene.materials[0];
    let tref = mat.base_color_texture.expect("diffuse texture bound");
    assert_eq!(tref.uv_set, 1, "varname `st1` selects UV set 1");
}

#[test]
fn multi_uv_sets_land_in_order() {
    let scene = decode_uv_scene();
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.uvs.len(), 2, "st + st1 = two UV sets");
    assert_eq!(prim.uvs[1][0], [0.5, 0.5]);
}

#[test]
fn double_sided_flags_primitive_and_material() {
    let scene = decode_uv_scene();
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(
        prim.extras.get("usd:doubleSided").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(scene.materials[0].double_sided);
}

#[test]
fn display_color_lands_in_vertex_colors() {
    let scene = decode_uv_scene();
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.colors.len(), 1);
    let c = &prim.colors[0];
    assert_eq!(c[0], [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(c[1], [0.0, 1.0, 0.0, 0.5]);
    assert_eq!(c[2], [0.0, 0.0, 1.0, 0.25]);
}

#[test]
fn constant_display_color_preserved_on_extras() {
    let usda = r#"#usda 1.0
def Mesh "M" {
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    color3f[] primvars:displayColor = [(0.2, 0.4, 0.6)]
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let prim = &scene.meshes[0].primitives[0];
    assert!(prim.colors.is_empty(), "constant color is not per-vertex");
    let dc = prim
        .extras
        .get("usd:displayColor")
        .and_then(|v| v.as_array())
        .expect("constant displayColor preserved");
    assert_eq!(dc.len(), 1);
}

#[test]
fn uvtexture_expansion_round_trips() {
    let scene = decode_uv_scene();
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new()
        .decode_bytes(&bytes)
        .expect("re-decode ok");

    // Sampler wrap modes survive.
    let tex = &s2.textures[0];
    assert_eq!(tex.sampler.wrap_s, WrapMode::ClampToEdge);
    assert_eq!(tex.sampler.wrap_t, WrapMode::MirroredRepeat);
    // UV-set selection survives through the emitted reader.
    let tref = s2.materials[0].base_color_texture.expect("texture bound");
    assert_eq!(tref.uv_set, 1);
    // Multi-UV mesh data survives.
    let prim = &s2.meshes[0].primitives[0];
    assert_eq!(prim.uvs.len(), 2);
    assert_eq!(prim.uvs[1][2], [0.5, 0.75]);
    // Display colors + opacity survive.
    assert_eq!(prim.colors.len(), 1);
    assert_eq!(prim.colors[0][1], [0.0, 1.0, 0.0, 0.5]);
    // doubleSided survives.
    assert!(s2.materials[0].double_sided);
    // scale/bias/fallback/sourceColorSpace stash survives (texture 0
    // is the only texture in both cycles).
    let stash = s2
        .extras
        .get("usd:uvtexture:0")
        .and_then(|v| v.as_object())
        .expect("stash survives round-trip");
    assert_eq!(
        stash.get("sourceColorSpace").and_then(|v| v.as_str()),
        Some("sRGB")
    );
}

#[test]
fn encode_of_uv_scene_reaches_fixed_point_after_one_cycle() {
    // The first cycle renames the texture shader (and so the emitted
    // asset filename) to the writer's canonical `Texture_<id>` — a
    // pre-existing writer convention. From the second encode onward
    // the text must be byte-stable.
    let scene = decode_uv_scene();
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
        "UV-expansion round-trip must be a fixed point after one cycle"
    );
}
