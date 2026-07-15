//! Every scene this decoder produces must pass the typed model's own
//! cross-arena `Scene3D::validate()` — in particular the texture ids
//! placed on the `MaterialExt` extension slots by the r414 migration
//! are checked through `Material::texture_refs()` (core + all
//! extensions), so a dangling ext-slot reference is a decoder bug.

mod common;

use oxideav_mesh3d::Mesh3DDecoder;
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn assert_valid(scene: &oxideav_mesh3d::Scene3D, label: &str) {
    if let Err(errors) = scene.validate() {
        panic!("{label}: decoded scene fails validate(): {errors:?}");
    }
}

#[test]
fn expanded_material_scene_validates() {
    let usda = r#"#usda 1.0
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
            int inputs:useSpecularWorkflow = 1
            color3f inputs:specularColor.connect = </Root/Mat/SpecTex.outputs:rgb>
            float inputs:clearcoat.connect = </Root/Mat/CcTex.outputs:r>
            float inputs:clearcoatRoughness.connect = </Root/Mat/CcTex.outputs:g>
            float inputs:ior = 1.45
            color3f inputs:diffuseColor.connect = </Root/Mat/DiffTex.outputs:rgb>
            token outputs:surface
        }
        def Shader "SpecTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @spec.png@
            float3 outputs:rgb
        }
        def Shader "CcTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @cc.png@
            float outputs:r
            float outputs:g
        }
        def Shader "DiffTex" {
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @diff.png@
            float3 outputs:rgb
        }
    }
}
"#;
    let usdz = common::build_usdz(&[
        common::UsdzEntry {
            name: "scene.usda",
            payload: usda.as_bytes(),
        },
        common::UsdzEntry {
            name: "spec.png",
            payload: b"SPEC",
        },
        common::UsdzEntry {
            name: "cc.png",
            payload: b"CC",
        },
        common::UsdzEntry {
            name: "diff.png",
            payload: b"DIFF",
        },
    ]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    // All three ext-slot texture refs live and distinct.
    let mat = &scene.materials[0];
    assert!(mat.ext.specular.as_ref().unwrap().color_texture.is_some());
    assert!(mat.ext.clearcoat.as_ref().unwrap().factor_texture.is_some());
    assert_eq!(scene.textures.len(), 3);
    assert_valid(&scene, "expanded-material decode");

    // The re-decoded output of our own writer validates too.
    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    assert_valid(&s2, "expanded-material repack");
}

#[test]
fn skel_scene_validates() {
    let usda = r#"#usda 1.0
(
    defaultPrim = "Model"
)
def SkelRoot "Model" {
    def Skeleton "Skel" {
        uniform token[] joints = ["Root", "Root/Hip"]
        uniform matrix4d[] bindTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,2,0,1))
        ]
        uniform matrix4d[] restTransforms = [
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1)),
            ((1,0,0,0), (0,1,0,0), (0,0,1,0), (0,2,0,1))
        ]
    }
    def Mesh "Body" (
        prepend apiSchemas = ["SkelBindingAPI"]
    ) {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel skel:skeleton = </Model/Skel>
        int[] primvars:skel:jointIndices = [1] (
            elementSize = 1
            interpolation = "constant"
        )
        float[] primvars:skel:jointWeights = [1] (
            elementSize = 1
            interpolation = "constant"
        )
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).unwrap();
    assert_eq!(scene.skeletons.len(), 1);
    assert_eq!(scene.skins.len(), 1);
    assert_valid(&scene, "skel decode");

    let bytes = UsdzEncoder::new().encode_bytes(&scene).expect("encode ok");
    let s2 = UsdzDecoder::new().decode(&bytes).expect("re-decode ok");
    assert_valid(&s2, "skel repack");
}
