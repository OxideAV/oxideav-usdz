//! `<UDIM>` tile-set texture references (staged schema §2.2: the
//! `file` input "supports the `<UDIM>` token for UDIM tile sets").
//! A `<UDIM>` path names a *set* of tile files, so no single archive
//! entry can back it — the texture decodes to `ImageData::External`
//! with the authored URI preserved verbatim (previously the whole
//! decode failed with "not present in the USDZ archive"), and the
//! writer re-emits the URI unchanged.

mod common;

use oxideav_mesh3d::{ImageData, Mesh3DDecoder};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const UDIM_USDA: &str = r#"#usda 1.0
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
    }
    def Material "Mat" {
        def Shader "Surface" {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </Root/Mat/Tiles.outputs:rgb>
            token outputs:surface
        }
        def Shader "Tiles" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @textures/diffuse.<UDIM>.png@
            token inputs:wrapS = "clamp"
            float3 outputs:rgb
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
fn udim_reference_decodes_to_external_image() {
    let scene = decode(UDIM_USDA);
    assert_eq!(scene.textures.len(), 1);
    let tex = &scene.textures[0];
    match &tex.image {
        ImageData::External { uri, mime } => {
            assert_eq!(uri, "textures/diffuse.<UDIM>.png");
            assert_eq!(mime.as_deref(), Some("image/png"));
        }
        other => panic!("expected External image, got {other:?}"),
    }
    // The sampler still decodes off the shader inputs.
    assert_eq!(
        tex.sampler.wrap_s,
        oxideav_mesh3d::WrapMode::ClampToEdge,
        "wrapS applies to a UDIM texture too"
    );
    assert!(
        scene.materials[0].base_color_texture.is_some(),
        "the typed slot binds the external texture"
    );
}

#[test]
fn udim_reference_round_trips_verbatim() {
    let scene = decode(UDIM_USDA);
    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert!(
        report
            .usda
            .contains("asset inputs:file = @textures/diffuse.<UDIM>.png@"),
        "authored URI re-emitted verbatim:\n{}",
        report.usda
    );

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    match &s2.textures[0].image {
        ImageData::External { uri, .. } => assert_eq!(uri, "textures/diffuse.<UDIM>.png"),
        other => panic!("expected External image after repack, got {other:?}"),
    }
    let second = UsdzEncoder::new()
        .encode_with_report(&s2)
        .expect("encode ok")
        .usda;
    assert_eq!(report.usda, second, "UDIM round trip is a fixed point");
}

#[test]
fn plain_missing_file_still_errors() {
    // Only the `<UDIM>` token opts into the external fallback — a
    // plain path that isn't in the archive is still a self-contained-
    // container violation with a precise diagnostic.
    let usda = UDIM_USDA.replace("textures/diffuse.<UDIM>.png", "textures/missing.png");
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let err = UsdzDecoder::new().decode_bytes(&usdz).unwrap_err();
    assert!(
        format!("{err}").contains("not present in the USDZ archive"),
        "unexpected error: {err}"
    );
}
