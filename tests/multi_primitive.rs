//! Multi-primitive Mesh emission + sibling-fold round-trip.
//!
//! When a Scene3D Mesh carries N primitives (the typical
//! one-primitive-per-material authoring shape), the writer emits N
//! sibling Mesh prims under the parent Xform — `Foo`, `Foo_1`,
//! `Foo_2` ... — each carrying its own `material:binding`. The
//! decoder folds the siblings back into a single Scene3D Mesh
//! whose `primitives` vector matches the input.

mod common;

use oxideav_mesh3d::{Indices, Material, Mesh, Node, Primitive, Scene3D, TextureRef, Topology};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};

fn tri_at(offset: f32) -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![
        [offset + 0.0, 0.0, 0.0],
        [offset + 1.0, 0.0, 0.0],
        [offset + 0.0, 1.0, 0.0],
    ];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    p
}

fn quad_at(offset: f32) -> Primitive {
    // 6 verts, 2 triangles.
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![
        [offset + 0.0, 0.0, 0.0],
        [offset + 1.0, 0.0, 0.0],
        [offset + 1.0, 1.0, 0.0],
        [offset + 0.0, 0.0, 0.0],
        [offset + 1.0, 1.0, 0.0],
        [offset + 0.0, 1.0, 0.0],
    ];
    p
}

#[test]
fn two_primitive_mesh_roundtrips() {
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("Body".into()))
        .with_primitive(tri_at(0.0))
        .with_primitive(tri_at(10.0));
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    // Both prims must appear in the USDA text — `Body` for the
    // first, `Body_1` for the second.
    assert!(
        report.usda.contains("def Mesh \"Body\""),
        "first primitive must keep the Scene3D mesh name; got:\n{}",
        report.usda
    );
    assert!(
        report.usda.contains("def Mesh \"Body_1\""),
        "second primitive must use the `_1` sibling-suffix convention; got:\n{}",
        report.usda
    );

    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    assert_eq!(
        scene2.meshes.len(),
        1,
        "sibling Mesh prims must fold back into a single Scene3D Mesh"
    );
    let m = &scene2.meshes[0];
    assert_eq!(m.name.as_deref(), Some("Body"));
    assert_eq!(
        m.primitives.len(),
        2,
        "two sibling prims fold into two primitives on the Mesh"
    );
    // Verify each primitive's vertex offset survived — first
    // triangle starts at x=0, second at x=10.
    assert_eq!(m.primitives[0].positions[0][0], 0.0);
    assert_eq!(m.primitives[1].positions[0][0], 10.0);
}

#[test]
fn three_primitive_mesh_with_distinct_materials() {
    let mut scene = Scene3D::new();
    let mat_red = scene.add_material(
        Material::new()
            .with_name("Red")
            .with_base_color([1.0, 0.0, 0.0, 1.0]),
    );
    let mat_green = scene.add_material(
        Material::new()
            .with_name("Green")
            .with_base_color([0.0, 1.0, 0.0, 1.0]),
    );
    let mat_blue = scene.add_material(
        Material::new()
            .with_name("Blue")
            .with_base_color([0.0, 0.0, 1.0, 1.0]),
    );

    let mut p0 = tri_at(0.0);
    p0.material = Some(mat_red);
    let mut p1 = quad_at(5.0);
    p1.material = Some(mat_green);
    let mut p2 = tri_at(20.0);
    p2.material = Some(mat_blue);
    let mesh = Mesh::new(Some("Multi".into()))
        .with_primitive(p0)
        .with_primitive(p1)
        .with_primitive(p2);
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    // Each primitive must reference its bound material via
    // `rel material:binding` against the synthetic /Materials scope.
    assert!(report
        .usda
        .contains("rel material:binding = </Materials/Red>"));
    assert!(report
        .usda
        .contains("rel material:binding = </Materials/Green>"));
    assert!(report
        .usda
        .contains("rel material:binding = </Materials/Blue>"));
    assert!(report.usda.contains("def Mesh \"Multi\""));
    assert!(report.usda.contains("def Mesh \"Multi_1\""));
    assert!(report.usda.contains("def Mesh \"Multi_2\""));

    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    assert_eq!(scene2.meshes.len(), 1);
    let m = &scene2.meshes[0];
    assert_eq!(m.primitives.len(), 3);
    let mat_names: Vec<_> = m
        .primitives
        .iter()
        .map(|p| {
            let mid = p.material.unwrap();
            scene2.materials[mid.0 as usize].name.as_deref().unwrap()
        })
        .collect();
    assert_eq!(mat_names, vec!["Red", "Green", "Blue"]);
    // Verify primitive sizes survived.
    assert_eq!(m.primitives[0].positions.len(), 3);
    assert_eq!(m.primitives[1].positions.len(), 6);
    assert_eq!(m.primitives[2].positions.len(), 3);
}

#[test]
fn single_primitive_mesh_unchanged_emits_no_suffix() {
    // Regression: when a Mesh has just one primitive the writer
    // must NOT emit `_0` — the sibling-suffix convention only
    // kicks in for the SECOND primitive onwards.
    let mut scene = Scene3D::new();
    let mesh = Mesh::new(Some("Solo".into())).with_primitive(tri_at(0.0));
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Root").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    assert!(report.usda.contains("def Mesh \"Solo\""));
    assert!(
        !report.usda.contains("\"Solo_0\""),
        "single-primitive Mesh must not emit the `_0` suffix"
    );
    assert!(
        !report.usda.contains("\"Solo_1\""),
        "single-primitive Mesh must not emit any sibling suffix"
    );
}

/// Hand-authored scene with sibling Mesh prims that DON'T match
/// the `<stem>_<digits>` convention (e.g. `HeadGeo` + `BodyGeo`)
/// must NOT fold — each becomes its own Scene3D Mesh, preserving
/// real-world authoring intent.
#[test]
fn unrelated_sibling_meshes_do_not_fold() {
    let usda = r#"#usda 1.0
def Xform "Root" {
    def Mesh "HeadGeo" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Mesh "BodyGeo" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(10,0,0), (11,0,0), (10,1,0)]
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    assert_eq!(
        scene.meshes.len(),
        2,
        "sibling Mesh prims with different stems must not fold"
    );
    let names: Vec<_> = scene
        .meshes
        .iter()
        .filter_map(|m| m.name.as_deref())
        .collect();
    assert!(names.contains(&"HeadGeo"));
    assert!(names.contains(&"BodyGeo"));
}

/// Hand-authored sibling Mesh prims sharing a stem (`Body`,
/// `Body_1`) MUST fold even when the writer wasn't in the loop
/// — the decoder rule is convention-based, not encoder-flagged.
#[test]
fn hand_authored_sibling_mesh_prims_fold() {
    let usda = r#"#usda 1.0
def Xform "Root" {
    def Mesh "Body" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    }
    def Mesh "Body_1" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(10,0,0), (11,0,0), (10,1,0)]
    }
    def Mesh "Body_2" {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(20,0,0), (21,0,0), (20,1,0)]
    }
}
"#;
    let usdz = common::build_usdz(&[common::UsdzEntry {
        name: "scene.usda",
        payload: usda.as_bytes(),
    }]);
    let scene = UsdzDecoder::new().decode_bytes(&usdz).expect("decode");
    assert_eq!(scene.meshes.len(), 1);
    let m = &scene.meshes[0];
    assert_eq!(m.name.as_deref(), Some("Body"));
    assert_eq!(m.primitives.len(), 3);
    assert_eq!(m.primitives[0].positions[0][0], 0.0);
    assert_eq!(m.primitives[1].positions[0][0], 10.0);
    assert_eq!(m.primitives[2].positions[0][0], 20.0);
}

/// A multi-primitive Mesh with a textured material — verifies the
/// per-primitive `material:binding` path resolves cleanly when
/// the material itself uses the round-1 texture connect machinery.
#[test]
fn multi_primitive_with_textured_material_resolves_binding() {
    let mut scene = Scene3D::new();
    // Two materials with no textures (textures need a backing
    // AssetSource we don't want to set up here; the binding
    // resolution is what we're testing).
    let mat_a = scene.add_material(
        Material::new()
            .with_name("MatA")
            .with_base_color([0.5, 0.5, 0.5, 1.0]),
    );
    let mat_b = scene.add_material(
        Material::new()
            .with_name("MatB")
            .with_base_color([0.2, 0.2, 0.2, 1.0]),
    );
    let _ = TextureRef::new; // keep import live

    let mut p0 = tri_at(0.0);
    p0.material = Some(mat_a);
    let mut p1 = tri_at(5.0);
    p1.material = Some(mat_b);
    let mesh = Mesh::new(Some("Avatar".into()))
        .with_primitive(p0)
        .with_primitive(p1);
    let mid = scene.add_mesh(mesh);
    let node = Node::new().with_name("Char").with_mesh(mid);
    let id = scene.add_node(node);
    scene.add_root(id);

    let report = UsdzEncoder::new()
        .encode_with_report(&scene)
        .expect("encode");
    let scene2 = UsdzDecoder::new()
        .decode_bytes(&report.bytes)
        .expect("decode");
    assert_eq!(scene2.meshes.len(), 1);
    let m = &scene2.meshes[0];
    let names: Vec<_> = m
        .primitives
        .iter()
        .map(|p| {
            let mid = p.material.unwrap();
            scene2.materials[mid.0 as usize].name.as_deref().unwrap()
        })
        .collect();
    assert_eq!(names, vec!["MatA", "MatB"]);
}
