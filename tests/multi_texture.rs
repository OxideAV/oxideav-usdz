//! Material with diffuse + normal + emissive textures all loaded
//! as `ZipStoredAsset` — verify each texture lands in
//! `Scene3D::textures` and the Material's slots reference the
//! right `TextureId`.

mod common;

use oxideav_usdz::UsdzDecoder;

const MULTI_USDA: &str = r#"#usda 1.0
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
            color3f inputs:diffuseColor.connect = </Root/Mat/DiffuseTex.outputs:rgb>
            normal3f inputs:normal.connect = </Root/Mat/NormalTex.outputs:rgb>
            color3f inputs:emissiveColor.connect = </Root/Mat/EmissiveTex.outputs:rgb>
            token outputs:surface
        }
        def Shader "DiffuseTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @diffuse.png@
            float3 outputs:rgb
        }
        def Shader "NormalTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @normal.png@
            float3 outputs:rgb
        }
        def Shader "EmissiveTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @emissive.jpg@
            float3 outputs:rgb
        }
    }
}
"#;

const D_BYTES: &[u8] = b"DIFFUSE-PIXEL-DATA";
const N_BYTES: &[u8] = b"NORMAL-PIXEL-DATA";
const E_BYTES: &[u8] = b"EMISSIVE-PIXEL-DATA";

#[test]
fn three_textures_loaded() {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: MULTI_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: D_BYTES,
        },
        common::UsdzEntry {
            name: "normal.png",
            payload: N_BYTES,
        },
        common::UsdzEntry {
            name: "emissive.jpg",
            payload: E_BYTES,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    assert_eq!(scene.textures.len(), 3, "three textures expected");
    let mat = &scene.materials[0];
    assert!(mat.base_color_texture.is_some());
    assert!(mat.normal_texture.is_some());
    assert!(mat.emissive_texture.is_some());
}

#[test]
fn texture_bytes_match_archive_entries() {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: MULTI_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: D_BYTES,
        },
        common::UsdzEntry {
            name: "normal.png",
            payload: N_BYTES,
        },
        common::UsdzEntry {
            name: "emissive.jpg",
            payload: E_BYTES,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let mat = &scene.materials[0];
    let take = |slot: &oxideav_mesh3d::TextureRef| -> Vec<u8> {
        let tex = &scene.textures[slot.texture.0 as usize];
        let oxideav_mesh3d::ImageData::Source(asset) = &tex.image else {
            panic!("expected ImageData::Source");
        };
        asset.raw_storage().unwrap().bytes.to_vec()
    };
    let d = take(&mat.base_color_texture.unwrap());
    let n = take(&mat.normal_texture.unwrap());
    let e = take(&mat.emissive_texture.unwrap());
    // Order in scene.textures depends on first-seen during the
    // material walk — verify by content rather than index.
    let mut all = vec![d.as_slice(), n.as_slice(), e.as_slice()];
    all.sort();
    let mut want = vec![D_BYTES, N_BYTES, E_BYTES];
    want.sort();
    assert_eq!(all, want);
}

#[test]
fn jpeg_mime_inferred() {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: MULTI_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: D_BYTES,
        },
        common::UsdzEntry {
            name: "normal.png",
            payload: N_BYTES,
        },
        common::UsdzEntry {
            name: "emissive.jpg",
            payload: E_BYTES,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let mut mimes: Vec<&str> = scene
        .textures
        .iter()
        .filter_map(|t| match &t.image {
            oxideav_mesh3d::ImageData::Source(s) => s.mime(),
            _ => None,
        })
        .collect();
    mimes.sort();
    assert_eq!(mimes, vec!["image/jpeg", "image/png", "image/png"]);
}
