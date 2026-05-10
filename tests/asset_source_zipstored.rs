//! Verify the `ZipStoredAsset` plumbed into a Texture exposes
//! `raw_storage(scheme = "zip-stored", ...)` over the original
//! USDZ inner-file bytes — this is the round-2 USDZ → USDZ
//! pass-through optimisation surface.

mod common;

use oxideav_mesh3d::ImageData;
use oxideav_usdz::UsdzDecoder;

const TEX_USDA: &str = r#"#usda 1.0
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
            color3f inputs:diffuseColor.connect = </Root/Mat/Diffuse.outputs:rgb>
            token outputs:surface
        }
        def Shader "Diffuse" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @diffuse.png@
            float3 outputs:rgb
        }
    }
}
"#;

// Synthetic "PNG" payload — header bytes are realistic enough to
// be sniffed; the decoder doesn't actually decode the image, it
// just slices a window into the archive bytes.
const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, // payload
];

#[test]
fn texture_raw_storage_is_zip_stored() {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: TEX_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: PNG_BYTES,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    assert_eq!(scene.textures.len(), 1, "exactly one texture expected");
    let tex = &scene.textures[0];
    let ImageData::Source(asset) = &tex.image else {
        panic!("expected ImageData::Source, got {:?}", tex.image);
    };
    let raw = asset
        .raw_storage()
        .expect("ZipStoredAsset must implement raw_storage");
    assert_eq!(raw.scheme, "zip-stored");
    assert_eq!(raw.uncompressed_size, Some(PNG_BYTES.len() as u64));
    assert_eq!(
        raw.bytes, PNG_BYTES,
        "pass-through bytes must match the inner PNG verbatim"
    );
    // MIME inferred from the .png extension.
    assert_eq!(asset.mime(), Some("image/png"));
}

#[test]
fn texture_open_returns_same_bytes() {
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: TEX_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: PNG_BYTES,
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let ImageData::Source(asset) = &scene.textures[0].image else {
        panic!("expected ImageData::Source");
    };
    use std::io::Read;
    let mut reader = asset.open().expect("open ok");
    let mut got = Vec::new();
    reader.read_to_end(&mut got).expect("read ok");
    assert_eq!(
        got, PNG_BYTES,
        "open() must return the same bytes as raw_storage()"
    );
}
