//! Cross-package `@pkg.usdz[path/within.usd]@` selectors (Core
//! Specification §16.4 package-relative paths), resolved when the
//! sibling package is an entry of the archive being read.
//!
//! * `references = @props.usdz[Marble.usd]@</Marble>` composes the
//!   prim from inside the sibling package;
//! * a bare `@props.usdz@` targets the package's default (first)
//!   layer;
//! * paths authored *inside* the package (`./sub.usda`, `tex.png`)
//!   anchor at the package, and their textures pass through
//!   byte-identical;
//! * `..` climbing above the package root and a selector into a
//!   package that is absent stay authored, un-composed;
//! * the preserving writer copies the whole sibling package through
//!   verbatim (fixed point).

mod common;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{ImageData, Mesh3DDecoder, Scene3D};
use oxideav_usdz::{CompositionMode, CompositionRecord, UsdzDecoder, UsdzEncoder};

const MARBLE: &str = r#"#usda 1.0
(
    defaultPrim = "Marble"
    subLayers = [@./sub.usda@]
)

def Xform "Marble"
{
    def Mesh "Geom" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
        rel material:binding = </Marble/Looks/Paint>
    }
    def Scope "Looks"
    {
        def Material "Paint"
        {
            token outputs:surface.connect = </Marble/Looks/Paint/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </Marble/Looks/Paint/Tex.outputs:rgb>
                float inputs:metallic = 0
                float inputs:roughness = 1
            }
            def Shader "Tex"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @textures/tex.png@
                float3 outputs:rgb
            }
        }
    }
}
"#;

const SUB: &str = r#"#usda 1.0

over "Marble"
{
    double3 xformOp:translate = (7, 0, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#;

const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 9, 8, 7, 6, 5, 4, 3, 2];

/// The sibling package: `Marble.usd` (default layer), its sublayer,
/// and a texture in a sub-directory.
fn props_package() -> Vec<u8> {
    build_usdz(&[
        UsdzEntry {
            name: "Marble.usd",
            payload: MARBLE.as_bytes(),
        },
        UsdzEntry {
            name: "sub.usda",
            payload: SUB.as_bytes(),
        },
        UsdzEntry {
            name: "textures/tex.png",
            payload: &PNG,
        },
    ])
}

fn outer(root: &str, props: &[u8]) -> Vec<u8> {
    build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "assets/props.usdz",
            payload: props,
        },
    ])
}

fn root_with(arc: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{{
    def "Ball" (
        references = {arc}
    )
    {{
    }}
}}
"#
    )
}

fn decode(bytes: &[u8]) -> Scene3D {
    UsdzDecoder::new().decode(bytes).expect("decode")
}

fn texture_bytes(scene: &Scene3D) -> Vec<u8> {
    let ImageData::Source(src) = &scene.textures[0].image else {
        panic!("zip-stored texture");
    };
    let raw = src.raw_storage().expect("raw storage");
    assert_eq!(raw.scheme, "zip-stored");
    raw.bytes.to_vec()
}

#[test]
fn selector_into_sibling_package_composes() {
    let props = props_package();
    let src = outer(
        &root_with("@./assets/props.usdz[Marble.usd]@</Marble>"),
        &props,
    );
    let scene = decode(&src);
    assert_eq!(
        scene.meshes.len(),
        1,
        "Marble's mesh composed from inside the package"
    );
    assert_eq!(scene.materials.len(), 1);
    assert_eq!(scene.textures.len(), 1);
    // The texture inside the package resolved through the same reader
    // (package-anchored `textures/tex.png`) with its exact bytes.
    assert_eq!(texture_bytes(&scene), PNG);
    let ImageData::Source(src) = &scene.textures[0].image else {
        panic!("zip-stored texture");
    };
    assert_eq!(
        src.mime(),
        Some("image/png"),
        "MIME from the inner file name"
    );
    // The package-internal sublayer (`./sub.usda`, anchored inside
    // the package) composed too: the translate rides up.
    let ball = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Ball"))
        .unwrap();
    assert!(matches!(
        ball.transform,
        oxideav_mesh3d::Transform::Trs { translation, .. } if translation == [7.0, 0.0, 0.0]
    ));
    let audit = scene.extras["usd:composedReferences"].as_array().unwrap();
    assert_eq!(
        audit[0].as_str(),
        Some("assets/props.usdz[Marble.usd]|references")
    );
    let sub = scene.extras["usd:composedSubLayers"].as_array().unwrap();
    assert_eq!(sub[0].as_str(), Some("assets/props.usdz[sub.usda]"));
}

#[test]
fn bare_package_reference_targets_its_default_layer() {
    let props = props_package();
    let src = outer(&root_with("@assets/props.usdz@"), &props);
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(
        scene.extras["usd:composedReferences"][0].as_str(),
        Some("assets/props.usdz[Marble.usd]|references")
    );
}

#[test]
fn unresolvable_selectors_stay_authored() {
    let props = props_package();
    for arc in [
        "@./assets/props.usdz[../escape.usd]@</Marble>",
        "@./assets/missing.usdz[Marble.usd]@</Marble>",
        "@./assets/props.usdz[absent.usd]@</Marble>",
    ] {
        let src = outer(&root_with(arc), &props);
        let scene = decode(&src);
        assert_eq!(scene.meshes.len(), 0, "{arc}");
        assert!(
            !scene.extras.contains_key("usd:composedReferences"),
            "{arc}"
        );
        let usda = UsdzEncoder::new().encode_with_report(&scene).unwrap().usda;
        assert!(
            usda.contains("references = "),
            "{arc}: arc re-authored:\n{usda}"
        );
    }
}

#[test]
fn flatten_and_preserve_round_trips() {
    let props = props_package();
    let src = outer(
        &root_with("@./assets/props.usdz[Marble.usd]@</Marble>"),
        &props,
    );
    let scene = decode(&src);

    // Flatten: geometry + material + texture bytes re-emitted in the
    // single-layer package (texture passes through verbatim).
    let flat = UsdzEncoder::new().encode_with_report(&scene).unwrap();
    assert_eq!(flat.pass_through_textures, 1);
    let s2 = decode(&flat.bytes);
    assert_eq!(s2.meshes.len(), 1);
    assert_eq!(texture_bytes(&s2), PNG);
    let flat2 = UsdzEncoder::new().encode_with_report(&s2).unwrap();
    assert_eq!(flat.usda, flat2.usda);

    // Preserve: the sibling package rides through byte-identical, the
    // selector arc is re-authored, nothing from inside it is
    // flattened into the root.
    let record = CompositionRecord::from_extras(&scene.extras).unwrap();
    assert_eq!(record.layers.len(), 1);
    assert_eq!(record.layers[0].path, "assets/props.usdz");
    assert_eq!(record.layers[0].bytes, props);
    let kept = UsdzEncoder::new()
        .with_composition(CompositionMode::Preserve)
        .encode_with_report(&scene)
        .unwrap();
    assert!(
        kept.usda
            .contains("references = @./assets/props.usdz[Marble.usd]@</Marble>"),
        "{}",
        kept.usda
    );
    assert!(!kept.usda.contains("def Mesh"), "{}", kept.usda);
    assert!(!kept.usda.contains("def Material"), "{}", kept.usda);
    assert_eq!(kept.layer_names, vec!["assets/props.usdz".to_string()]);
    let names: Vec<String> = oxideav_usdz::zip::walk(&kept.bytes)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["scene.usda", "assets/props.usdz"]);
    let s3 = decode(&kept.bytes);
    assert_eq!(s3.meshes.len(), 1);
    assert_eq!(texture_bytes(&s3), PNG);
    let kept2 = UsdzEncoder::new()
        .with_composition(CompositionMode::Preserve)
        .encode_bytes(&s3)
        .unwrap();
    assert_eq!(kept.bytes, kept2);
}
