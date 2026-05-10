//! Verify n-gon faces (faceVertexCounts entries > 3) get
//! fan-triangulated into the Primitive's index buffer.

mod common;

use oxideav_usdz::UsdzDecoder;

// One quad + one pentagon = 5 triangles total = 15 indices.
const POLY_USDA: &str = r#"#usda 1.0
def Mesh "Poly" {
    int[] faceVertexCounts = [4, 5]
    int[] faceVertexIndices = [0, 1, 2, 3,  4, 5, 6, 7, 8]
    point3f[] points = [
        (0,0,0), (1,0,0), (1,1,0), (0,1,0),
        (2,0,0), (3,0,0), (3.5,1,0), (3,2,0), (2,1,0)
    ]
}
"#;

#[test]
fn quad_and_pentagon_triangulated() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "poly.usda",
        payload: POLY_USDA.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let prim = &scene.meshes[0].primitives[0];
    let indices_len = prim.indices.as_ref().unwrap().len();
    // quad → 2 triangles → 6 indices; pentagon → 3 triangles → 9 indices; total 15.
    assert_eq!(indices_len, 15);
    // triangle_count uses index_len / 3 for Triangles topology.
    assert_eq!(prim.triangle_count(), 5);
}
