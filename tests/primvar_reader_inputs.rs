//! `UsdPreviewSurface` inputs connected to a `UsdPrimvarReader_<T>`
//! (staged schema §2.3, `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`):
//! the reader's authored inputs (typed variant, `varname`, `fallback`)
//! are preserved on `Material::extras["usd:primvar:<input>"]` and the
//! writer re-emits the reader prim + connection — previously any
//! non-`UsdUVTexture` connection was a hard decode failure.

mod common;

use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const READER_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)
def Xform "Root" {
    def Mesh "M" {
        rel material:binding = </Root/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        color3f[] primvars:displayColor = [(1, 0, 0), (0, 1, 0), (0, 0, 1)]
    }
    def Material "Mat" {
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Root/Mat/ColorReader.outputs:result>
            float inputs:opacity.connect = </Root/Mat/OpacityReader.outputs:result>
            token outputs:surface
        }
        def Shader "ColorReader" {
            uniform token info:id = "UsdPrimvarReader_float3"
            string inputs:varname = "displayColor"
            float3 inputs:fallback = (0.18, 0.18, 0.18)
            float3 outputs:result
        }
        def Shader "OpacityReader" {
            uniform token info:id = "UsdPrimvarReader_float"
            token inputs:varname = "displayOpacity"
            float outputs:result
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

#[test]
fn primvar_reader_connection_decodes_instead_of_failing() {
    // The premise being fixed: this file previously died with
    // "only `UsdUVTexture` is supported".
    let scene = decode(READER_USDA);
    let mat = &scene.materials[0];
    assert!(
        mat.base_color_texture.is_none(),
        "a primvar reader is not a texture"
    );
    assert!(scene.textures.is_empty());

    let stash = mat
        .extras
        .get("usd:primvar:diffuseColor")
        .and_then(|v| v.as_object())
        .expect("reader stash for diffuseColor");
    assert_eq!(stash.get("type").and_then(|v| v.as_str()), Some("float3"));
    assert_eq!(
        stash.get("varname").and_then(|v| v.as_str()),
        Some("displayColor")
    );
    assert_eq!(
        stash.get("varname_type").and_then(|v| v.as_str()),
        Some("string")
    );
    assert_eq!(
        stash.get("fallback").and_then(|v| v.as_str()),
        Some("(0.18, 0.18, 0.18)"),
        "fallback preserved as an exact USDA literal"
    );
    assert_eq!(
        stash.get("fallback_type").and_then(|v| v.as_str()),
        Some("float3")
    );

    // Second reader: float variant, token-typed varname, no fallback.
    let stash = mat
        .extras
        .get("usd:primvar:opacity")
        .and_then(|v| v.as_object())
        .expect("reader stash for opacity");
    assert_eq!(stash.get("type").and_then(|v| v.as_str()), Some("float"));
    assert_eq!(
        stash.get("varname_type").and_then(|v| v.as_str()),
        Some("token"),
        "newer-schema token spelling preserved"
    );
    assert!(stash.get("fallback").is_none());
}

#[test]
fn primvar_reader_round_trips_and_is_fixed_point() {
    let scene = decode(READER_USDA);
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    let a = &scene.materials[0];
    let b = &s2.materials[0];
    for key in ["usd:primvar:diffuseColor", "usd:primvar:opacity"] {
        assert_eq!(a.extras.get(key), b.extras.get(key), "extras `{key}`");
    }

    let first = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok")
        .usda;
    assert!(
        first.contains("uniform token info:id = \"UsdPrimvarReader_float3\""),
        "reader prim re-emitted"
    );
    assert!(
        first.contains(
            "color3f inputs:diffuseColor.connect = \
             </Materials/Mat/PrimvarReader_diffuseColor.outputs:result>"
        ),
        "connection re-emitted:\n{first}"
    );
    assert!(first.contains("float3 inputs:fallback = (0.18, 0.18, 0.18)"));
    assert!(first.contains("token inputs:varname = \"displayOpacity\""));
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    assert_eq!(first, second, "reader round trip must be a fixed point");
}

#[test]
fn unknown_shader_connection_still_refused() {
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
            color3f inputs:diffuseColor.connect = </Root/Mat/Mystery.outputs:rgb>
            token outputs:surface
        }
        def Shader "Mystery" {
            uniform token info:id = "SomeCustomShader"
            float3 outputs:rgb
        }
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let err = UsdzDecoder::new().decode_bytes(&usdz).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("SomeCustomShader"),
        "unknown shader type still surfaces a precise refusal: {msg}"
    );
}

#[test]
fn bogus_reader_variant_refused() {
    // `UsdPrimvarReader_bogus` is not one of the ten §2.3 typed
    // variants — refuse rather than fabricating a reader.
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
            color3f inputs:diffuseColor.connect = </Root/Mat/R.outputs:result>
            token outputs:surface
        }
        def Shader "R" {
            uniform token info:id = "UsdPrimvarReader_bogus"
            float3 outputs:result
        }
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    assert!(UsdzDecoder::new().decode_bytes(&usdz).is_err());
}
