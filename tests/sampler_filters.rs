//! Sampler filter state through the `oxideav-mesh3d` 0.0.5
//! Option-shaped surface: USD's `UsdUVTexture` (staged schema §2.2)
//! authors **no** minification/magnification filter inputs, so a
//! decoded sampler must report *undefined* filters — distinguishable
//! from any explicit choice — while the wrap axes stay the typed
//! mapping of `wrapS`/`wrapT`.

mod common;

use oxideav_mesh3d::{MagFilter, MinFilter, Scene3D, WrapMode};
use oxideav_usdz::UsdzDecoder;

const TEX_BYTES: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4];

fn decode_with_wraps(wrap_block: &str) -> Scene3D {
    let usda = format!(
        r#"#usda 1.0
def Xform "Root" {{
    def Mesh "M" {{
        rel material:binding = </Root/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        texCoord2f[] primvars:st = [(0,0), (1,0), (0,1)]
    }}
    def Material "Mat" {{
        def Shader "Surface" {{
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Root/Mat/Diffuse.outputs:rgb>
            token outputs:surface
        }}
        def Shader "Diffuse" {{
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @diffuse.png@
{wrap_block}
            float3 outputs:rgb
        }}
    }}
}}
"#
    );
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: usda.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: TEX_BYTES,
        },
    ]);
    UsdzDecoder::new().decode_bytes(&usdz).unwrap()
}

#[test]
fn decoded_sampler_filters_are_undefined() {
    let scene = decode_with_wraps("");
    let sampler = &scene.textures[0].sampler;
    assert_eq!(
        sampler.mag_filter, None,
        "UsdUVTexture authors no magnification filter — must decode as undefined"
    );
    assert_eq!(
        sampler.min_filter, None,
        "UsdUVTexture authors no minification filter — must decode as undefined"
    );
    // The crate-documented fallbacks apply only on demand.
    assert_eq!(sampler.effective_mag_filter(), MagFilter::Linear);
    assert_eq!(sampler.effective_min_filter(), MinFilter::LinearMipLinear);
    assert!(
        sampler.uses_mipmaps(),
        "trilinear fallback needs the mip chain"
    );
    // No sampler object = the glTF default sampler state exactly.
    assert_eq!(*sampler, oxideav_mesh3d::Sampler::default_sampler());
}

#[test]
fn wrap_tokens_map_onto_typed_wrap_modes() {
    let scene = decode_with_wraps(
        r#"            token inputs:wrapS = "clamp"
            token inputs:wrapT = "mirror""#,
    );
    let sampler = &scene.textures[0].sampler;
    assert_eq!(sampler.wrap_s, WrapMode::ClampToEdge);
    assert_eq!(sampler.wrap_t, WrapMode::MirroredRepeat);
    // Filters stay undefined regardless of wrap authoring.
    assert_eq!(sampler.mag_filter, None);
    assert_eq!(sampler.min_filter, None);
    // The typed CPU-side reference semantics resolve per axis.
    let wrapped = sampler.wrap_uv([1.25, 1.25]);
    assert!((wrapped[0] - 1.0).abs() < 1e-6, "clamp pins to 1.0");
    assert!(
        (wrapped[1] - 0.75).abs() < 1e-6,
        "mirror reflects 1.25 to 0.75"
    );
}
