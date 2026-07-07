//! Decode → encode → decode round-trip fidelity harness.
//!
//! Builds a matrix of `Scene3D` inputs through the public
//! [`oxideav_mesh3d`] model, runs each one through
//! `UsdzEncoder → UsdzDecoder`, and asserts the semantically-relevant
//! fields survive. The `fidelity_matrix` test reports a measured
//! pass-count so a regression that silently drops one channel shows up
//! as a smaller number rather than an all-or-nothing failure.
//!
//! Regression anchor: the `single_root_survives_material_scope` case
//! guards the round-398 fix where the top-level `def Scope "Materials"`
//! the writer emits round-tripped back into a *spurious empty second
//! root node*. A USD `Scope` is a non-transformable namespace
//! container; when it holds only `Material` children (indexed
//! separately into `scene.materials`) it must not surface as a
//! scene-graph node.

use oxideav_mesh3d::{
    Axis, ImageData, InMemoryAsset, Indices, Material, Mesh, Mesh3DDecoder, Node, Primitive,
    Scene3D, Texture, TextureRef, Topology, Transform, Unit,
};
use oxideav_usdz::{UsdzDecoder, UsdzEncoder};
use std::sync::Arc;

fn round_trip(scene: &Scene3D) -> Scene3D {
    let bytes = UsdzEncoder::new().encode_bytes(scene).expect("encode ok");
    UsdzDecoder::new().decode(&bytes).expect("decode ok")
}

/// Second-cycle stability: encode → decode → encode must produce the
/// same USDA text as the first encode (the writer is deterministic and
/// the reader loses nothing structural on the second pass).
fn usda_of(scene: &Scene3D) -> String {
    UsdzEncoder::new()
        .encode_with_report(scene)
        .expect("encode ok")
        .usda
}

fn tri_prim() -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    p.indices = Some(Indices::U16(vec![0, 1, 2]));
    p
}

#[test]
fn single_root_survives_material_scope() {
    // A one-root scene whose single mesh binds a material. The writer
    // emits a top-level `def Scope "Materials"`; the reader must NOT
    // turn that scope into a second root node.
    let mut scene = Scene3D::new();
    let mut mat = Material::new().with_name("Red");
    mat.base_color = [1.0, 0.0, 0.0, 1.0];
    mat.metallic = 0.0;
    mat.roughness = 0.5;
    let mid = scene.add_material(mat);
    let mut prim = tri_prim();
    prim.material = Some(mid);
    let mesh_id = scene.add_mesh(Mesh::new(Some("M".into())).with_primitive(prim));
    let root = scene.add_node(Node::new().with_name("Root").with_mesh(mesh_id));
    scene.add_root(root);

    let s2 = round_trip(&scene);
    assert_eq!(
        s2.roots.len(),
        1,
        "material `Scope` must not inject a spurious second root"
    );
    assert_eq!(s2.materials.len(), 1, "material must survive");
    assert_eq!(s2.materials[0].name.as_deref(), Some("Red"));
    // No node should be a childless, mesh-less namespace container.
    for n in &s2.nodes {
        let empty = n.mesh.is_none()
            && n.children.is_empty()
            && n.camera.is_none()
            && n.light.is_none()
            && n.audio_emitter.is_none();
        assert!(
            !empty || n.name.as_deref() != Some("Materials"),
            "empty `Materials` scope leaked into the scene graph"
        );
    }
}

#[test]
fn root_count_stable_across_material_counts() {
    // Regardless of how many materials the scene carries, a single
    // geometry root stays a single root after round-trip.
    for n_mats in [0usize, 1, 3, 8] {
        let mut scene = Scene3D::new();
        let mut mesh = Mesh::new(Some("M".into()));
        for i in 0..n_mats.max(1) {
            let mut mat = Material::new().with_name(format!("Mat{i}"));
            mat.metallic = 0.0;
            mat.roughness = 0.5;
            let mid = scene.add_material(mat);
            let mut prim = tri_prim();
            if n_mats > 0 {
                prim.material = Some(mid);
            }
            mesh.primitives.push(prim);
        }
        let mesh_id = scene.add_mesh(mesh);
        let root = scene.add_node(Node::new().with_name("Root").with_mesh(mesh_id));
        scene.add_root(root);

        let s2 = round_trip(&scene);
        assert_eq!(
            s2.roots.len(),
            1,
            "n_mats={n_mats}: expected exactly one root after round-trip"
        );
    }
}

#[test]
fn second_encode_is_stable() {
    // encode → decode → encode must reproduce the first USDA byte-for
    // -byte: the fixed point of the round-trip is reached in one cycle.
    let mut scene = Scene3D::new();
    let mut mat = Material::new().with_name("Mat");
    mat.base_color = [0.2, 0.4, 0.6, 0.8];
    mat.metallic = 0.3;
    mat.roughness = 0.7;
    mat.emissive_factor = [0.1, 0.2, 0.3];
    let mid = scene.add_material(mat);
    let mut prim = tri_prim();
    prim.normals = Some(vec![[0.0, 0.0, 1.0]; 3]);
    prim.uvs.push(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    prim.material = Some(mid);
    let mesh_id = scene.add_mesh(Mesh::new(Some("M".into())).with_primitive(prim));
    let root = scene.add_node(
        Node::new()
            .with_name("Root")
            .with_mesh(mesh_id)
            .with_transform(Transform::Trs {
                translation: [1.0, 2.0, 3.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [2.0, 2.0, 2.0],
            }),
    );
    scene.add_root(root);

    let usda1 = usda_of(&scene);
    let s2 = round_trip(&scene);
    let usda2 = usda_of(&s2);
    assert_eq!(usda1, usda2, "second encode diverged from the first");
}

/// Broad fidelity sweep: each closure builds a scene and returns
/// `true` when the round-tripped scene matches on the field under
/// test. The harness reports how many of the matrix survived.
#[test]
fn fidelity_matrix() {
    type Case = (&'static str, fn() -> bool);

    let base_color: fn() -> bool = || {
        let mut s = Scene3D::new();
        let mut m = Material::new().with_name("M");
        m.base_color = [0.2, 0.4, 0.6, 0.8];
        m.metallic = 0.0;
        m.roughness = 0.5;
        let mid = s.add_material(m);
        let mut p = tri_prim();
        p.material = Some(mid);
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh));
        s.add_root(r);
        let s2 = round_trip(&s);
        let bc = s2.materials[0].base_color;
        (0..4).all(|i| (bc[i] - [0.2, 0.4, 0.6, 0.8][i]).abs() < 1e-4)
    };
    let metallic_roughness: fn() -> bool = || {
        let mut s = Scene3D::new();
        let mut m = Material::new().with_name("M");
        m.metallic = 0.3;
        m.roughness = 0.7;
        let mid = s.add_material(m);
        let mut p = tri_prim();
        p.material = Some(mid);
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh));
        s.add_root(r);
        let s2 = round_trip(&s);
        (s2.materials[0].metallic - 0.3).abs() < 1e-4
            && (s2.materials[0].roughness - 0.7).abs() < 1e-4
    };
    let emissive: fn() -> bool = || {
        let mut s = Scene3D::new();
        let mut m = Material::new().with_name("M");
        m.metallic = 0.0;
        m.roughness = 0.5;
        m.emissive_factor = [0.1, 0.2, 0.3];
        let mid = s.add_material(m);
        let mut p = tri_prim();
        p.material = Some(mid);
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh));
        s.add_root(r);
        let s2 = round_trip(&s);
        let e = s2.materials[0].emissive_factor;
        (0..3).all(|i| (e[i] - [0.1, 0.2, 0.3][i]).abs() < 1e-4)
    };
    let normals_uvs: fn() -> bool = || {
        let mut s = Scene3D::new();
        let mut p = tri_prim();
        p.normals = Some(vec![[0.0, 0.0, 1.0]; 3]);
        p.uvs.push(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh));
        s.add_root(r);
        let s2 = round_trip(&s);
        let p2 = &s2.meshes[0].primitives[0];
        p2.normals.as_ref().map(|v| v.len()) == Some(3)
            && p2.uvs.first().map(|v| v.len()) == Some(3)
    };
    let texture_bind: fn() -> bool = || {
        let mut s = Scene3D::new();
        let asset: Arc<dyn oxideav_mesh3d::AssetSource> = Arc::new(InMemoryAsset::new(
            Some("image/png".into()),
            vec![1, 2, 3, 4],
        ));
        let tid = s.add_texture(Texture {
            name: Some("Diff".into()),
            image: ImageData::Source(asset),
            sampler: oxideav_mesh3d::Sampler::default_sampler(),
        });
        let mut m = Material::new().with_name("M");
        m.metallic = 0.0;
        m.roughness = 0.5;
        m.base_color_texture = Some(TextureRef::new(tid));
        let mid = s.add_material(m);
        let mut p = tri_prim();
        p.material = Some(mid);
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(p));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh));
        s.add_root(r);
        let s2 = round_trip(&s);
        s2.textures.len() == 1 && s2.materials[0].base_color_texture.is_some()
    };
    let transform: fn() -> bool = || {
        let mut s = Scene3D::new();
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(tri_prim()));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh).with_transform(
            Transform::Trs {
                translation: [1.0, 2.0, 3.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [2.0, 2.0, 2.0],
            },
        ));
        s.add_root(r);
        let s2 = round_trip(&s);
        let m = s2.nodes[s2.roots[0].0 as usize].transform.to_matrix();
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-4;
        approx(m[0][3], 1.0) && approx(m[1][3], 2.0) && approx(m[2][3], 3.0)
    };
    let axis_unit: fn() -> bool = || {
        let mut s = Scene3D::new();
        s.up_axis = Axis::PosZ;
        s.unit = Unit::Centimetres;
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(tri_prim()));
        let r = s.add_node(Node::new().with_name("R").with_mesh(mesh));
        s.add_root(r);
        let s2 = round_trip(&s);
        s2.up_axis == Axis::PosZ && s2.unit == Unit::Centimetres
    };
    let deep_hierarchy: fn() -> bool = || {
        let mut s = Scene3D::new();
        let mesh = s.add_mesh(Mesh::new(Some("M".into())).with_primitive(tri_prim()));
        let leaf = s.add_node(Node::new().with_name("Leaf").with_mesh(mesh));
        let mut child = Node::new().with_name("Child");
        child.children.push(leaf);
        let child = s.add_node(child);
        let mut root = Node::new().with_name("Root");
        root.children.push(child);
        let root = s.add_node(root);
        s.add_root(root);
        let s2 = round_trip(&s);
        // Root → Child → Leaf(mesh) three-level chain survives.
        fn depth(s: &Scene3D, n: usize) -> usize {
            s.nodes[n]
                .children
                .iter()
                .map(|c| 1 + depth(s, c.0 as usize))
                .max()
                .unwrap_or(0)
        }
        s2.roots.len() == 1 && depth(&s2, s2.roots[0].0 as usize) >= 2
    };

    let cases: &[Case] = &[
        ("base_color", base_color),
        ("metallic_roughness", metallic_roughness),
        ("emissive", emissive),
        ("normals_uvs", normals_uvs),
        ("texture_bind", texture_bind),
        ("transform", transform),
        ("axis_unit", axis_unit),
        ("deep_hierarchy", deep_hierarchy),
    ];

    let mut passed = 0usize;
    for (name, f) in cases {
        let ok = f();
        eprintln!("fidelity[{name}] = {}", if ok { "PASS" } else { "FAIL" });
        if ok {
            passed += 1;
        }
    }
    eprintln!("fidelity: {passed}/{} channels round-tripped", cases.len());
    assert_eq!(
        passed,
        cases.len(),
        "every fidelity channel must round-trip cleanly"
    );
}

#[test]
fn round_trip_reaches_a_fixed_point() {
    // The USDA a scene serialises to must stop changing after at most
    // one decode→encode cycle — the round-trip is a fixed point, never
    // a monotonically-growing tree. Regression for the round-398
    // bare-mesh-carrier collapse (a `def Mesh` was re-wrapped in a new
    // `def Xform` on every cycle, so the archive grew without bound).
    let mut scene = Scene3D::new();
    let mesh_id = scene.add_mesh(Mesh::new(Some("M".into())).with_primitive(tri_prim()));
    let root = scene.add_node(Node::new().with_name("Root").with_mesh(mesh_id));
    scene.add_root(root);

    let u1 = usda_of(&scene);
    let s2 = round_trip(&scene);
    let u2 = usda_of(&s2);
    let s3 = round_trip(&s2);
    let u3 = usda_of(&s3);
    let s4 = round_trip(&s3);
    let u4 = usda_of(&s4);

    // A single mesh under a single named root reaches its fixed point
    // immediately (node name "Root" carries a mesh named "M" — no
    // wrapper is added), and stays there.
    assert_eq!(u1, u2, "first cycle drifted");
    assert_eq!(u2, u3, "second cycle drifted");
    assert_eq!(u3, u4, "third cycle drifted");
    assert!(
        u1.len() < 512,
        "USDA unexpectedly large: {} bytes",
        u1.len()
    );
}

#[test]
fn mesh_carrier_child_does_not_grow() {
    // A named locator whose child is a differently-named mesh: the
    // node/mesh name distinction cannot survive USD's single-name
    // prims, so the first round-trip may reshape it — but from cycle 2
    // on it must be a stable fixed point, not an ever-deeper tree.
    let mut scene = Scene3D::new();
    let mesh_id = scene.add_mesh(Mesh::new(Some("Geo".into())).with_primitive(tri_prim()));
    let leaf = scene.add_node(Node::new().with_name("Locator").with_mesh(mesh_id));
    let mut root = Node::new().with_name("Root");
    root.children.push(leaf);
    let root = scene.add_node(root);
    scene.add_root(root);

    let s2 = round_trip(&scene);
    let u2 = usda_of(&s2);
    let s3 = round_trip(&s2);
    let u3 = usda_of(&s3);
    let s4 = round_trip(&s3);
    let u4 = usda_of(&s4);
    assert_eq!(u2, u3, "cycle 2→3 drifted (unbounded growth)");
    assert_eq!(u3, u4, "cycle 3→4 drifted (unbounded growth)");
}
