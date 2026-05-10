//! `UsdPreviewSurface` with all PBR fields populated → verify the
//! Scene3D `Material` carries the expected values.

mod common;

use oxideav_usdz::UsdzDecoder;

const PBR_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "Root"
)
def Xform "Root" {
    def Mesh "M" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    ) {
        rel material:binding = </Root/Materials/Mat>
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Scope "Materials" {
        def Material "Mat" {
            token outputs:surface.connect = </Root/Materials/Mat/Surface.outputs:surface>
            def Shader "Surface" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.8, 0.2, 0.1)
                float inputs:metallic = 0.4
                float inputs:roughness = 0.6
                color3f inputs:emissiveColor = (0.0, 0.5, 0.0)
                float inputs:opacity = 0.75
                token outputs:surface
            }
        }
    }
}
"#;

#[test]
fn preview_surface_pbr_fields() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: PBR_USDA.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    assert_eq!(scene.materials.len(), 1, "expected one Material");
    let mat = &scene.materials[0];
    assert_eq!(mat.name.as_deref(), Some("Mat"));
    let approx = |a: f32, b: f32| (a - b).abs() < 1e-5;
    assert!(approx(mat.base_color[0], 0.8));
    assert!(approx(mat.base_color[1], 0.2));
    assert!(approx(mat.base_color[2], 0.1));
    assert!(approx(mat.base_color[3], 0.75));
    assert!(approx(mat.metallic, 0.4));
    assert!(approx(mat.roughness, 0.6));
    assert!(approx(mat.emissive_factor[1], 0.5));
}

#[test]
fn material_binding_resolves() {
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: PBR_USDA.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    let mat_id = scene
        .materials
        .iter()
        .position(|m| m.name.as_deref() == Some("Mat"))
        .map(|i| oxideav_mesh3d::MaterialId(i as u32))
        .unwrap();
    let prim = &scene.meshes[0].primitives[0];
    assert_eq!(prim.material, Some(mat_id));
}
