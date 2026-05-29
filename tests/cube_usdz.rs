//! Synthetic 12-triangle cube `.usdz` round-trip — verify the
//! decoder produces the expected `Scene3D` shape.

mod common;

use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::UsdzDecoder;

#[test]
fn loads_cube_usdz() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "cube.usda",
        payload: common::CUBE_USDA.as_bytes(),
    }]);
    let mut decoder = UsdzDecoder::new();
    let scene = decoder.decode(&usdz).expect("decode ok");

    // One root Xform with one Mesh child.
    assert_eq!(scene.roots.len(), 1, "expected single root");
    assert_eq!(scene.meshes.len(), 1, "expected single mesh");

    let mesh = &scene.meshes[0];
    assert_eq!(mesh.name.as_deref(), Some("Cube"));
    assert_eq!(mesh.primitives.len(), 1);
    let prim = &mesh.primitives[0];
    assert_eq!(prim.positions.len(), 24);
    let idx_len = prim.indices.as_ref().unwrap().len();
    assert_eq!(idx_len, 36, "12 triangles × 3 indices each");
    // Triangle count derived from the index buffer.
    assert_eq!(scene.triangle_count(), 12);
    // 24 vertex positions across the lone mesh.
    assert_eq!(scene.vertex_count(), 24);
}

#[test]
fn corrupted_payload_is_rejected_by_crc() {
    // A single flipped byte inside the Default Layer payload — the
    // archive structure stays valid, but the CRC-32 the central
    // directory recorded no longer matches. The walker must reject
    // it rather than feeding garbage USDA into the parser.
    let mut usdz = common::build_usdz(&[common::UsdzEntry {
        name: "cube.usda",
        payload: common::CUBE_USDA.as_bytes(),
    }]);
    // Locate the `#usda` header inside the payload and flip a byte
    // of it; this is well inside the stored payload, away from any
    // header field.
    let needle = b"#usda";
    let pos = usdz
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("payload header present");
    usdz[pos] ^= 0xFF;

    let err = UsdzDecoder::new().decode_bytes(&usdz).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("CRC-32"), "expected CRC failure, got: {msg}");
}

#[test]
fn cube_indices_are_u16() {
    // 24 vertices fits in U16 — the decoder should pick the
    // compact representation rather than promoting to U32.
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "cube.usda",
        payload: common::CUBE_USDA.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let prim = &scene.meshes[0].primitives[0];
    assert!(matches!(
        prim.indices,
        Some(oxideav_mesh3d::Indices::U16(_))
    ));
}
