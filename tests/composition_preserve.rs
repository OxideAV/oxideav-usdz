//! Composition-arc preservation on the writer — the typed opinion
//! model (`oxideav_usdz::composition`) in both of its modes.
//!
//! The decoder evaluates sublayers / references / payloads / class
//! arcs / variant selections into the typed `Scene3D`; historically
//! the encoder could only flatten that result into one layer. With
//! the `CompositionRecord` the decoder now stashes on
//! `Scene3D::extras["usd:composition"]`, `UsdzEncoder::with_composition
//! (CompositionMode::Preserve)` re-authors the source structure:
//!
//! * the arc opinions come back on their prims exactly as authored;
//! * prims an arc / variant selection contributed are left to the
//!   arc (no duplicate flattened copy);
//! * `class` / `over` prims the typed model has no slot for replay
//!   verbatim;
//! * the consumed layer entries (and the assets they reference) are
//!   copied into the package; the root layer keeps its entry name.
//!
//! Every preserved case is pinned as a **one-cycle fixed point**:
//! decode → encode → decode yields the same typed scene, and the
//! second encode is byte-identical to the first. The flattening
//! default is pinned alongside (and no longer re-authors a
//! `subLayers` entry it folded in). Where `usdchecker` is installed
//! the preserved package is also black-box validated.

mod common;

use std::process::Command;

use common::{build_usdz, UsdzEntry};
use oxideav_mesh3d::{Mesh3DDecoder, Scene3D, Transform};
use oxideav_usdz::{CompositionMode, CompositionRecord, UsdzDecoder, UsdzEncoder};

const TRI_MESH: &str = r#"        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
"#;

fn decode(bytes: &[u8]) -> Scene3D {
    UsdzDecoder::new().decode(bytes).expect("decode")
}

fn encode(scene: &Scene3D, mode: CompositionMode) -> oxideav_usdz::EncodeReport {
    UsdzEncoder::new()
        .with_composition(mode)
        .encode_with_report(scene)
        .expect("encode")
}

fn entry_names(bytes: &[u8]) -> Vec<String> {
    oxideav_usdz::zip::walk(bytes)
        .expect("walk")
        .into_iter()
        .map(|e| e.name)
        .collect()
}

fn entry_payload(bytes: &[u8], name: &str) -> Vec<u8> {
    let entries = oxideav_usdz::zip::walk(bytes).expect("walk");
    let e = entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("entry {name} present"));
    bytes[e.payload_offset as usize..][..e.payload_len as usize].to_vec()
}

/// decode → encode(Preserve) → decode → encode(Preserve): the second
/// package must be byte-identical to the first (one-cycle fixed
/// point), and the re-decoded scene must agree on the structural
/// counts. Returns (first package, re-decoded scene).
fn preserve_fixed_point(source: &[u8]) -> (Vec<u8>, Scene3D) {
    let s1 = decode(source);
    let out1 = encode(&s1, CompositionMode::Preserve).bytes;
    let s2 = decode(&out1);
    let out2 = encode(&s2, CompositionMode::Preserve).bytes;
    assert_eq!(
        out1,
        out2,
        "second preserving encode diverged from the first:\n--- first ---\n{}\n--- second ---\n{}",
        String::from_utf8_lossy(&entry_payload(&out1, &entry_names(&out1)[0])),
        String::from_utf8_lossy(&entry_payload(&out2, &entry_names(&out2)[0]))
    );
    assert_eq!(s1.meshes.len(), s2.meshes.len(), "mesh count survives");
    assert_eq!(s1.roots.len(), s2.roots.len(), "root count survives");
    assert_eq!(s1.nodes.len(), s2.nodes.len(), "node count survives");
    assert_eq!(
        s1.materials.len(),
        s2.materials.len(),
        "material count survives"
    );
    (out1, s2)
}

fn usdchecker_available() -> bool {
    Command::new("usdchecker")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Black-box validate a package with `usdchecker` when it is
/// installed (skip-early otherwise — never `#[ignore]`).
fn usdchecker_ok(bytes: &[u8], label: &str) {
    if !usdchecker_available() {
        eprintln!("usdchecker not installed; skipping black-box validation of {label}");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "oxideav-usdz-preserve-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.usdz");
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("usdchecker").arg(&path).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "usdchecker rejected the preserved {label} package:\n{text}"
    );
}

// ---------------------------------------------------------------- sublayers

fn sublayer_package() -> Vec<u8> {
    let root = r#"#usda 1.0
(
    defaultPrim = "Root"
    subLayers = [
        @./geom.usda@
    ]
)

def Xform "Root" {
    double3 xformOp:translate = (1, 2, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#;
    let geom = format!(
        r#"#usda 1.0
(
)

def Xform "Root" {{
    def Mesh "Cube" {{
{TRI_MESH}    }}
}}
"#
    );
    build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "geom.usda",
            payload: geom.as_bytes(),
        },
    ])
}

#[test]
fn sublayer_preserved_as_entry_and_opinion() {
    let src = sublayer_package();
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let record = CompositionRecord::from_extras(&scene.extras).expect("composition record");
    assert_eq!(record.root_layer, "scene.usda");
    assert_eq!(record.layers.len(), 1);
    assert_eq!(record.layers[0].path, "geom.usda");
    assert!(record.is_local("/Root"));
    assert!(!record.is_local("/Root/Cube"), "the mesh was contributed");

    let report = encode(&scene, CompositionMode::Preserve);
    assert_eq!(report.root_layer_name, "scene.usda");
    assert_eq!(report.layer_names, vec!["geom.usda".to_string()]);
    assert!(
        report.usda.contains("subLayers"),
        "subLayers opinion re-authored:\n{}",
        report.usda
    );
    assert!(
        !report.usda.contains("def Mesh"),
        "contributed mesh must not be flattened into the root:\n{}",
        report.usda
    );
    assert!(report.usda.contains("xformOp:translate = (1, 2, 3)"));
    let names = entry_names(&report.bytes);
    assert_eq!(names, vec!["scene.usda", "geom.usda"]);
    // The sublayer entry is byte-identical to the source's.
    assert_eq!(
        entry_payload(&report.bytes, "geom.usda"),
        entry_payload(&src, "geom.usda")
    );

    let (out, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);
    assert_eq!(
        s2.extras
            .get("usd:composedSubLayers")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(1),
        "the re-decoded package composes the sublayer again"
    );
    usdchecker_ok(&out, "sublayer");
}

#[test]
fn flatten_drops_consumed_sublayer_entry() {
    let src = sublayer_package();
    let scene = decode(&src);
    let report = encode(&scene, CompositionMode::Flatten);
    assert_eq!(report.layer_names.len(), 0);
    assert!(
        !report.usda.contains("subLayers"),
        "a folded-in sublayer must not dangle in the flattened layer:\n{}",
        report.usda
    );
    assert!(report.usda.contains("def Mesh \"Cube\""));
    assert_eq!(entry_names(&report.bytes), vec!["scene.usda"]);
    let s2 = decode(&report.bytes);
    assert_eq!(s2.meshes.len(), 1);
    assert!(!s2.extras.contains_key("usd:composedSubLayers"));
    // Flattening is itself a fixed point.
    let again = encode(&s2, CompositionMode::Flatten);
    assert_eq!(again.usda, report.usda);
}

#[test]
fn flatten_keeps_external_sublayer_entries() {
    // One composed, one external: only the composed one is pruned.
    let root = r#"#usda 1.0
(
    subLayers = [
        @./geom.usda@,
        @./elsewhere.usda@
    ]
)

def Xform "Root" {
}
"#;
    let geom = "#usda 1.0\n\ndef Xform \"Root\" {\n}\n";
    let src = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "geom.usda",
            payload: geom.as_bytes(),
        },
    ]);
    let report = encode(&decode(&src), CompositionMode::Flatten);
    assert!(
        report.usda.contains("@./elsewhere.usda@"),
        "{}",
        report.usda
    );
    assert!(!report.usda.contains("@./geom.usda@"), "{}", report.usda);
}

#[test]
fn preserved_root_keeps_its_entry_name_and_anchoring() {
    // Root and sublayer live in a sub-directory; the anchored
    // `./geom.usda` only resolves if the root keeps its name.
    let root = r#"#usda 1.0
(
    subLayers = [@./geom.usda@]
)

def Xform "Root" {
}
"#;
    let geom = format!(
        "#usda 1.0\n\ndef Xform \"Root\" {{\n    def Mesh \"M\" {{\n{TRI_MESH}    }}\n}}\n"
    );
    let src = build_usdz(&[
        UsdzEntry {
            name: "assets/root.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "assets/geom.usda",
            payload: geom.as_bytes(),
        },
    ]);
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let report = encode(&scene, CompositionMode::Preserve);
    assert_eq!(report.root_layer_name, "assets/root.usda");
    assert_eq!(
        entry_names(&report.bytes),
        vec!["assets/root.usda", "assets/geom.usda"]
    );
    let (_, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);
}

// ---------------------------------------------------------------- references

const MARBLE: &str = r#"#usda 1.0
(
    defaultPrim = "Marble"
)

def Xform "Marble" (
    kind = "component"
)
{
    def Mesh "marble_geom" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
}
"#;

fn reference_package(arc_line: &str) -> Vec<u8> {
    let collection = format!(
        r#"#usda 1.0
(
    defaultPrim = "Holder"
)

def Xform "Holder"
{{
    def "Inst" (
        {arc_line}
    )
    {{
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}
"#
    );
    build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: collection.as_bytes(),
        },
        UsdzEntry {
            name: "Marble.usd",
            payload: MARBLE.as_bytes(),
        },
    ])
}

#[test]
fn reference_arc_preserved_with_target_layer() {
    let src = reference_package("prepend references = @Marble.usd@");
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let report = encode(&scene, CompositionMode::Preserve);
    assert!(
        report.usda.contains("prepend references = @Marble.usd@"),
        "arc re-authored as written:\n{}",
        report.usda
    );
    assert!(
        !report.usda.contains("def Mesh"),
        "referenced geometry stays in Marble.usd:\n{}",
        report.usda
    );
    // Local opinions on the referencing prim survive.
    assert!(report.usda.contains("xformOp:translate = (5, 0, 0)"));
    assert_eq!(entry_names(&report.bytes), vec!["scene.usda", "Marble.usd"]);
    assert_eq!(
        entry_payload(&report.bytes, "Marble.usd"),
        MARBLE.as_bytes()
    );
    let (out, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);
    let inst = s2
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Inst"))
        .expect("Inst node");
    assert!(
        matches!(inst.transform, Transform::Trs { translation, .. } if translation == [5.0, 0.0, 0.0])
    );
    usdchecker_ok(&out, "reference");
}

#[test]
fn payload_arc_preserved_with_target_layer() {
    let src = reference_package("payload = @Marble.usd@</Marble>");
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let report = encode(&scene, CompositionMode::Preserve);
    assert!(
        report.usda.contains("payload = @Marble.usd@</Marble>"),
        "{}",
        report.usda
    );
    assert!(!report.usda.contains("def Mesh"), "{}", report.usda);
    let (_, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);
}

#[test]
fn flatten_of_reference_is_unchanged() {
    // The historical behaviour: geometry flattened in, no arc.
    let src = reference_package("references = @Marble.usd@");
    let report = encode(&decode(&src), CompositionMode::Flatten);
    assert!(!report.usda.contains("references"), "{}", report.usda);
    assert!(
        report.usda.contains("def Mesh \"marble_geom\""),
        "{}",
        report.usda
    );
    assert_eq!(entry_names(&report.bytes), vec!["scene.usda"]);
}

#[test]
fn transitive_reference_chain_and_asset_pass_through() {
    // scene → props/Set.usda → ../Marble.usd, with a texture the
    // referenced material binds; preserving copies every consumed
    // layer plus the texture the inner layer references.
    let set = r#"#usda 1.0
(
    defaultPrim = "Set"
)

def Xform "Set"
{
    def "Ball" (
        references = @../Marble.usd@
    )
    {
    }
    def Scope "Looks"
    {
        def Material "Paint"
        {
            token outputs:surface.connect = </Set/Looks/Paint/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </Set/Looks/Paint/Tex.outputs:rgb>
                float inputs:metallic = 0
                float inputs:roughness = 1
            }
            def Shader "Tex"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @../textures/paint.png@
                float3 outputs:rgb
            }
        }
    }
}
"#;
    let root = r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def "Stage" (
        references = @./props/Set.usda@
    )
    {
    }
}
"#;
    let png = [0x89u8, b'P', b'N', b'G', 1, 2, 3, 4, 5, 6, 7, 8];
    let src = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "props/Set.usda",
            payload: set.as_bytes(),
        },
        UsdzEntry {
            name: "Marble.usd",
            payload: MARBLE.as_bytes(),
        },
        UsdzEntry {
            name: "textures/paint.png",
            payload: &png,
        },
    ]);
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    assert_eq!(scene.materials.len(), 1);
    assert_eq!(scene.textures.len(), 1);
    let record = CompositionRecord::from_extras(&scene.extras).expect("record");
    let mut paths: Vec<&str> = record.layers.iter().map(|l| l.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, vec!["Marble.usd", "props/Set.usda"]);
    let set_layer = record
        .layers
        .iter()
        .find(|l| l.path == "props/Set.usda")
        .unwrap();
    assert_eq!(set_layer.assets.len(), 1);
    assert_eq!(set_layer.assets[0].path, "textures/paint.png");
    assert_eq!(set_layer.assets[0].bytes, png);

    let report = encode(&scene, CompositionMode::Preserve);
    // The contributed material is left to the arc: no /Materials
    // scope, no re-emitted texture file.
    assert!(!report.usda.contains("def Material"), "{}", report.usda);
    assert_eq!(report.texture_names.len(), 0);
    let mut names = entry_names(&report.bytes);
    names.sort();
    assert_eq!(
        names,
        vec![
            "Marble.usd",
            "props/Set.usda",
            "scene.usda",
            "textures/paint.png"
        ]
    );
    let (_, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.materials.len(), 1);
    assert_eq!(s2.textures.len(), 1);
}

// ---------------------------------------------------------------- class arcs

fn class_package(arc: &str) -> Vec<u8> {
    let src = format!(
        r#"#usda 1.0
(
    defaultPrim = "Prop"
)

class Xform "_Base" (
    kind = "component"
)
{{
    float custom:tag = 3
    def Mesh "Geom" {{
{TRI_MESH}    }}
}}

def Xform "Prop" (
    {arc} = </_Base>
)
{{
    double3 xformOp:translate = (0, 1, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}}
"#
    );
    build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: src.as_bytes(),
    }])
}

#[test]
fn inherits_arc_and_class_prim_preserved() {
    let src = class_package("inherits");
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let report = encode(&scene, CompositionMode::Preserve);
    assert!(
        report.usda.contains("prepend inherits = </_Base>"),
        "{}",
        report.usda
    );
    assert!(
        report.usda.contains("class Xform \"_Base\""),
        "class prim replayed verbatim:\n{}",
        report.usda
    );
    assert!(
        report.usda.contains("float custom:tag = 3"),
        "{}",
        report.usda
    );
    // Exactly one Mesh prim: the one inside the class body.
    assert_eq!(
        report.usda.matches("def Mesh").count(),
        1,
        "{}",
        report.usda
    );
    let (out, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);
    assert!(s2.extras.contains_key("usd:composedInherits"));
    usdchecker_ok(&out, "inherits");
}

#[test]
fn specializes_arc_preserved() {
    let src = class_package("specializes");
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let report = encode(&scene, CompositionMode::Preserve);
    assert!(
        report.usda.contains("prepend specializes = </_Base>"),
        "{}",
        report.usda
    );
    assert_eq!(
        report.usda.matches("def Mesh").count(),
        1,
        "{}",
        report.usda
    );
    let (_, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);
    assert!(s2.extras.contains_key("usd:composedSpecializes"));
}

#[test]
fn flatten_of_class_arc_is_unchanged() {
    let src = class_package("inherits");
    let report = encode(&decode(&src), CompositionMode::Flatten);
    assert!(!report.usda.contains("inherits"), "{}", report.usda);
    assert!(!report.usda.contains("class "), "{}", report.usda);
    assert!(report.usda.contains("def Mesh \"Geom\""), "{}", report.usda);
}

// ---------------------------------------------------------------- variants

#[test]
fn selected_variant_children_stay_in_the_variant_body() {
    let src = format!(
        r#"#usda 1.0
(
    defaultPrim = "Root"
)

def Xform "Root" (
    variants = {{
        string shape = "tri"
    }}
    prepend variantSets = "shape"
)
{{
    variantSet "shape" = {{
        "tri" {{
            def Mesh "Geom" {{
{TRI_MESH}            }}
        }}
        "empty" {{
        }}
    }}
}}
"#
    );
    let src = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: src.as_bytes(),
    }]);
    let scene = decode(&src);
    assert_eq!(scene.meshes.len(), 1);
    let preserved = encode(&scene, CompositionMode::Preserve);
    // One Mesh prim, inside the variant body — not a flattened copy.
    assert_eq!(
        preserved.usda.matches("def Mesh").count(),
        1,
        "{}",
        preserved.usda
    );
    assert!(
        preserved.usda.contains("variantSet \"shape\""),
        "{}",
        preserved.usda
    );
    let (_, s2) = preserve_fixed_point(&src);
    assert_eq!(s2.meshes.len(), 1);

    // Flattening keeps the historical shape: the selected content
    // is flattened in *and* the variant block is replayed.
    let flat = encode(&scene, CompositionMode::Flatten);
    assert_eq!(flat.usda.matches("def Mesh").count(), 2, "{}", flat.usda);
    assert_eq!(decode(&flat.bytes).meshes.len(), 1);
}

// ---------------------------------------------------------------- crate layers

/// The staged Crate fixture lives in the private `docs/` sibling
/// checkout — skip (never fail, never `#[ignore]`) when it is absent,
/// mirroring the other Crate fixture tests.
fn crate_fixture() -> Option<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/3d/usd/fixtures/crate-variant-specs.usdc");
    if !path.exists() {
        eprintln!("staged fixture {} absent; skipping", path.display());
        return None;
    }
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())))
}

#[test]
fn usdc_reference_composes_and_is_preserved() {
    // The staged Crate fixture is referenced by its prim path; the
    // selected variants inside it (`sizeVariant = small`) compose.
    let Some(fixture) = crate_fixture() else {
        return;
    };
    let root = r#"#usda 1.0
(
    defaultPrim = "Root"
)

def Xform "Root"
{
    def "Fixture" (
        references = @crate-variant-specs.usdc@</VariantFixture>
    )
    {
    }
}
"#;
    let src = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "crate-variant-specs.usdc",
            payload: &fixture,
        },
    ]);
    let scene = decode(&src);
    let child = scene
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Child"))
        .expect("Child composed from the Crate layer");
    assert!(
        matches!(child.transform, Transform::Trs { scale, .. } if (scale[0] - 0.5).abs() < 1e-6),
        "sizeVariant=small selected inside the crate: {:?}",
        child.transform
    );
    let report = encode(&scene, CompositionMode::Preserve);
    assert!(
        report
            .usda
            .contains("references = @crate-variant-specs.usdc@</VariantFixture>"),
        "{}",
        report.usda
    );
    assert!(!report.usda.contains("\"Child\""), "{}", report.usda);
    assert_eq!(
        entry_payload(&report.bytes, "crate-variant-specs.usdc"),
        fixture
    );
    let (_, s2) = preserve_fixed_point(&src);
    assert!(s2.nodes.iter().any(|n| n.name.as_deref() == Some("Child")));
}

#[test]
fn usdc_sublayer_composes() {
    let Some(fixture) = crate_fixture() else {
        return;
    };
    let root = r#"#usda 1.0
(
    subLayers = [@./crate-variant-specs.usdc@]
)

over "VariantFixture"
{
}
"#;
    let src = build_usdz(&[
        UsdzEntry {
            name: "scene.usda",
            payload: root.as_bytes(),
        },
        UsdzEntry {
            name: "crate-variant-specs.usdc",
            payload: &fixture,
        },
    ]);
    let scene = decode(&src);
    assert!(
        scene
            .nodes
            .iter()
            .any(|n| n.name.as_deref() == Some("VariantFixture")),
        "the local `over` fused with the crate's `def`"
    );
    let report = encode(&scene, CompositionMode::Preserve);
    assert!(report.usda.contains("subLayers"), "{}", report.usda);
    assert_eq!(
        report.layer_names,
        vec!["crate-variant-specs.usdc".to_string()]
    );
    let (_, s2) = preserve_fixed_point(&src);
    assert!(s2
        .nodes
        .iter()
        .any(|n| n.name.as_deref() == Some("VariantFixture")));
}

// ---------------------------------------------------------------- scenes without a record

#[test]
fn preserve_without_record_equals_flatten() {
    let src = build_usdz(&[UsdzEntry {
        name: "scene.usda",
        payload: common::CUBE_USDA.as_bytes(),
    }]);
    let scene = decode(&src);
    assert!(
        CompositionRecord::from_extras(&scene.extras).is_none(),
        "a single self-contained layer stashes no record"
    );
    let a = encode(&scene, CompositionMode::Preserve);
    let b = encode(&scene, CompositionMode::Flatten);
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.root_layer_name, "scene.usda");
}
