//! USDZ → USDZ encoder roundtrip — load an archive built by the
//! r1 fixture builder, re-encode it through `UsdzEncoder`, then
//! verify:
//!
//! * the re-encoded archive parses back through the reader cleanly,
//! * the texture's inner-file bytes survived bit-identical, AND
//! * the encoder report flags the texture as
//!   `from_pass_through = true`, proving the
//!   `raw_storage(scheme = "zip-stored")` optimisation actually
//!   fires for USDZ-sourced assets.

mod common;

use oxideav_mesh3d::{ImageData, Mesh3DDecoder};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

const SCENE_USDA: &str = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1.0
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

// 32-byte synthetic PNG-ish payload — content is opaque to the
// encoder/decoder; we just need to verify it survives byte-for-byte.
const TEX_BYTES: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk header
    0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, // payload
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // payload
];

#[test]
fn texture_bytes_pass_through_unchanged() {
    let usdz_in = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: SCENE_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: TEX_BYTES,
        },
    ]);

    let scene = UsdzDecoder::new()
        .decode_bytes(&usdz_in)
        .expect("decode ok");
    assert_eq!(scene.textures.len(), 1, "expected one texture");

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode ok");
    assert_eq!(
        report.pass_through_textures, 1,
        "USDZ-sourced texture must use raw_storage(zip-stored) pass-through"
    );
    assert_eq!(report.reencoded_textures, 0);
    assert_eq!(report.texture_names.len(), 1);

    // Walk the re-encoded archive directly and pull the texture
    // payload out — it MUST be byte-identical to the input texture
    // bytes (the whole point of the optimisation).
    let entries = oxideav_usdz::zip::walk(&report.bytes).expect("re-walk ok");
    let tex_entry = entries
        .iter()
        .find(|e| e.name.ends_with(".png"))
        .expect("texture entry present in re-encoded archive");
    let tex_payload = &report.bytes[tex_entry.payload_offset as usize
        ..(tex_entry.payload_offset + tex_entry.payload_len) as usize];
    assert_eq!(
        tex_payload, TEX_BYTES,
        "texture bytes must round-trip bit-identical via the zip-stored pass-through path"
    );
}

#[test]
fn reencoded_archive_re_decodes() {
    let usdz_in = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: SCENE_USDA.as_bytes(),
        },
        common::UsdzEntry {
            name: "diffuse.png",
            payload: TEX_BYTES,
        },
    ]);

    let scene = UsdzDecoder::new()
        .decode_bytes(&usdz_in)
        .expect("decode ok");
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");

    // Re-decode the encoder's output — the reader/writer pair must
    // round-trip through itself.
    let mut decoder = UsdzDecoder::new();
    let scene2 = decoder.decode(&bytes).expect("re-decode ok");

    assert_eq!(scene2.meshes.len(), 1, "expected one mesh after roundtrip");
    let mesh = &scene2.meshes[0];
    assert_eq!(mesh.primitives.len(), 1);
    let prim = &mesh.primitives[0];
    assert_eq!(prim.positions.len(), 3, "mesh has 3 vertices end-to-end");
    assert!(
        prim.indices.is_some(),
        "indices must survive the round-trip"
    );

    // And the texture is still wrapped in a ZipStoredAsset whose
    // raw_storage scheme is `zip-stored` — the chain
    // USDZ → Scene3D → USDZ → Scene3D preserves the optimisation
    // surface end-to-end.
    assert_eq!(scene2.textures.len(), 1);
    let ImageData::Source(asset) = &scene2.textures[0].image else {
        panic!("expected ImageData::Source after roundtrip");
    };
    let raw = asset.raw_storage().expect("raw_storage available");
    assert_eq!(raw.scheme, "zip-stored");
    assert_eq!(
        raw.bytes, TEX_BYTES,
        "texture bytes must be identical after a full USDZ → Scene3D → USDZ → Scene3D round trip"
    );
}

/// USDA-only scene (no textures) — verifies the writer still puts
/// the Default Layer first and aligns its payload to 64 bytes even
/// when there are no companion files.
const PLAIN_USDA: &str = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1.0
)

def Xform "Root" {
    def Mesh "M" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;

#[test]
fn first_entry_is_default_layer() {
    let usdz_in = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: PLAIN_USDA.as_bytes(),
    }]);
    let scene = UsdzDecoder::new()
        .decode_bytes(&usdz_in)
        .expect("decode ok");
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let entries = oxideav_usdz::zip::walk(&bytes).expect("walk ok");
    let first = &entries[0];
    let lower = first.name.to_ascii_lowercase();
    assert!(
        lower.ends_with(".usd") || lower.ends_with(".usda") || lower.ends_with(".usdc"),
        "first entry must be the USD Default Layer, got `{}`",
        first.name
    );
    // And it must be 64-byte aligned per the USDZ spec.
    assert_eq!(first.payload_offset % 64, 0);
}
