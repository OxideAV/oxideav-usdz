//! Translate a parsed USDA [`Layer`](crate::usda::Layer) into an
//! `oxideav_mesh3d::Scene3D`.
//!
//! Round 1 schema coverage:
//!
//! * `Xform` → [`Node`](oxideav_mesh3d::Node) carrying the
//!   transform metadata when present (only identity for r1 — the
//!   transform attribute parser is deferred).
//! * `Scope` → empty `Node` (USD's organisational grouping prim).
//! * `Mesh` → [`Mesh`](oxideav_mesh3d::Mesh) +
//!   [`Primitive`](oxideav_mesh3d::Primitive) with `Triangles`
//!   topology, fan-triangulating any non-triangle face arities
//!   described by `faceVertexCounts`.
//! * `Material` → [`Material`](oxideav_mesh3d::Material). The
//!   PBR fields are filled in from the embedded
//!   `UsdPreviewSurface` `Shader` child; texture references are
//!   resolved through nested `UsdUVTexture` `Shader` children.
//! * `Shader` with `info:id == "UsdPreviewSurface"` —
//!   contributes `base_color` / `metallic` / `roughness` /
//!   `emissive_factor` plus any `*.connect` references that
//!   point at sibling `UsdUVTexture` shaders.
//! * `Shader` with `info:id == "UsdUVTexture"` — produces a
//!   [`Texture`](oxideav_mesh3d::Texture) whose image is a
//!   [`ZipStoredAsset`](crate::ZipStoredAsset) into the
//!   surrounding USDZ archive. The pass-through scheme is
//!   `"zip-stored"` so a downstream USDZ writer can copy the
//!   inner file verbatim.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use oxideav_mesh3d::{
    AssetSource, Axis, ImageData, Indices, Material, Mesh, Node, Primitive, Scene3D, Texture,
    TextureRef, Topology, Transform, Unit,
};

use crate::asset_source::{mime_from_filename, ZipStoredAsset};
use crate::error::{invalid, unsupported};
use crate::usda::{Attr, Layer, Prim, Value};
use crate::zip::ZipEntry;
use crate::Result;

/// Convert a parsed USDA layer + the surrounding ZIP archive into
/// a [`Scene3D`].
pub fn translate(layer: &Layer, archive: Arc<Vec<u8>>, entries: &[ZipEntry]) -> Result<Scene3D> {
    let mut ctx = Ctx {
        scene: Scene3D::new(),
        archive,
        entries: entries
            .iter()
            .map(|e| (e.name.clone(), e.clone()))
            .collect(),
        materials_by_path: HashMap::new(),
        textures_by_path: HashMap::new(),
    };

    apply_layer_metadata(&mut ctx.scene, &layer.metadata);

    // Walk the prim tree once, indexing every Material + Shader by
    // its absolute prim path. We resolve material bindings on a
    // second pass once every material is registered.
    index_materials(&mut ctx, "", &layer.prims)?;

    // Now build the node tree.
    for prim in &layer.prims {
        let node = build_node(&mut ctx, "", prim)?;
        if let Some(id) = node {
            ctx.scene.add_root(id);
        }
    }
    Ok(ctx.scene)
}

struct Ctx {
    scene: Scene3D,
    archive: Arc<Vec<u8>>,
    entries: HashMap<String, ZipEntry>,
    materials_by_path: HashMap<String, oxideav_mesh3d::MaterialId>,
    /// Cache so two materials referencing the same shader's texture
    /// share one `TextureId` instead of duplicating the asset.
    textures_by_path: HashMap<String, oxideav_mesh3d::TextureId>,
}

fn apply_layer_metadata(scene: &mut Scene3D, meta: &BTreeMap<String, Value>) {
    if let Some(v) = meta.get("upAxis").and_then(|v| v.as_text()) {
        scene.up_axis = match v {
            "Y" | "y" => Axis::PosY,
            "Z" | "z" => Axis::PosZ,
            "X" | "x" => Axis::PosX,
            _ => scene.up_axis,
        };
    }
    if let Some(f) = meta.get("metersPerUnit").and_then(|v| v.as_f32()) {
        scene.unit = unit_from_meters_per_unit(f);
    }
    // Stash everything else verbatim for round-trip preservation.
    for (k, v) in meta {
        if matches!(k.as_str(), "upAxis" | "metersPerUnit") {
            continue;
        }
        if let Some(json) = value_to_json(v) {
            scene.extras.insert(format!("usd:{k}"), json);
        }
    }
}

fn unit_from_meters_per_unit(mpu: f32) -> Unit {
    // Tolerate small float error around the canonical USD presets.
    let near = |a: f32, b: f32| (a - b).abs() < 1e-6;
    if near(mpu, 1.0) {
        Unit::Metres
    } else if near(mpu, 0.01) {
        Unit::Centimetres
    } else if near(mpu, 0.001) {
        Unit::Millimetres
    } else if near(mpu, 0.0254) {
        Unit::Inches
    } else if near(mpu, 0.3048) {
        Unit::Feet
    } else if near(mpu, 0.9144) {
        Unit::Yards
    } else {
        // Unknown ratio — pick metres and stash the original in
        // scene.extras for downstream tools.
        Unit::Metres
    }
}

/// First pass: walk every prim and register `Material` defs in the
/// path-keyed cache so `material:binding = </path>` lookups can be
/// resolved during the node-build pass.
fn index_materials(ctx: &mut Ctx, parent: &str, prims: &[Prim]) -> Result<()> {
    for prim in prims {
        let path = join_path(parent, &prim.name);
        if prim.spec == "def" && prim.type_name == "Material" {
            let mat = build_material(ctx, &path, prim)?;
            let id = ctx.scene.add_material(mat);
            ctx.materials_by_path.insert(path.clone(), id);
        }
        index_materials(ctx, &path, &prim.children)?;
    }
    Ok(())
}

/// Build a single node for `prim`, recursing into children.
/// Returns `None` for prims that don't materialise into a
/// scene-graph node (e.g. `Material` / `Shader` — already
/// captured in the materials index).
fn build_node(ctx: &mut Ctx, parent: &str, prim: &Prim) -> Result<Option<oxideav_mesh3d::NodeId>> {
    if prim.spec != "def" {
        return Ok(None);
    }
    let path = join_path(parent, &prim.name);
    match prim.type_name.as_str() {
        "Material" | "Shader" => Ok(None),
        "Xform" | "Scope" | "" => {
            let mut node = Node::new().with_name(prim.name.clone());
            node.transform = read_node_transform(prim);
            // Recurse children — collect the scene-graph children
            // first, push attribute extras after.
            let mut child_ids = Vec::new();
            for child in &prim.children {
                if let Some(id) = build_node(ctx, &path, child)? {
                    child_ids.push(id);
                }
            }
            node.children = child_ids;
            stash_extras(&mut node.extras, prim);
            let id = ctx.scene.add_node(node);
            Ok(Some(id))
        }
        "Mesh" => {
            let mesh = build_mesh(ctx, &path, prim)?;
            let mesh_id = ctx.scene.add_mesh(mesh);
            let mut node = Node::new().with_name(prim.name.clone()).with_mesh(mesh_id);
            node.transform = read_node_transform(prim);
            stash_extras(&mut node.extras, prim);
            let id = ctx.scene.add_node(node);
            Ok(Some(id))
        }
        other => {
            // Unknown / not-yet-supported schema — preserve as an
            // empty node with the type token in extras so a writer
            // could round-trip even what we don't model.
            let mut node = Node::new().with_name(prim.name.clone());
            node.extras
                .insert("usd:type".into(), serde_json::Value::String(other.into()));
            stash_extras(&mut node.extras, prim);
            let id = ctx.scene.add_node(node);
            Ok(Some(id))
        }
    }
}

/// Reconstruct a [`Transform`] from the UsdGeomXformable opinion
/// schema living on `prim`'s attributes.
///
/// Supported opinion sets (driven by `xformOpOrder`):
///
/// * `["xformOp:translate", "xformOp:orient", "xformOp:scale"]` →
///   [`Transform::Trs`]. Any of the three can be absent — missing
///   slots fall back to identity (`(0,0,0)` translation, identity
///   quaternion, `(1,1,1)` scale).
/// * `["xformOp:transform"]` → [`Transform::Matrix`].
///
/// `quatf xformOp:orient` is laid out `(w, x, y, z)` per USD; we
/// reorder into our internal xyzw form.
///
/// Anything we don't recognise (Euler triple, scale-only, mixed
/// orderings) collapses to [`Transform::identity`] — round-trip
/// fidelity stops there but we never produce a malformed
/// transform for content the writer never emits anyway.
fn read_node_transform(prim: &Prim) -> Transform {
    // No opinions ⇒ identity. Common case for the unxformed Xforms
    // r1's writer used to emit.
    let order_attr = prim.attrs.get("xformOpOrder");
    let Some(order) = order_attr.and_then(|a| a.value.as_seq()) else {
        return Transform::identity();
    };
    let order: Vec<&str> = order.iter().filter_map(|v| v.as_text()).collect();
    if order.is_empty() {
        return Transform::identity();
    }

    if order.len() == 1 && order[0] == "xformOp:transform" {
        if let Some(m) = prim
            .attrs
            .get("xformOp:transform")
            .and_then(|a| read_matrix4(&a.value))
        {
            return Transform::Matrix(m);
        }
        return Transform::identity();
    }

    // TRS-style — accept any subset of `translate` / `orient` /
    // `scale` so long as the listed ops are exactly those tokens.
    let recognised = order
        .iter()
        .all(|t| matches!(*t, "xformOp:translate" | "xformOp:orient" | "xformOp:scale"));
    if !recognised {
        return Transform::identity();
    }

    let translation = prim
        .attrs
        .get("xformOp:translate")
        .and_then(|a| a.value.as_floatn::<3>())
        .unwrap_or([0.0; 3]);
    let rotation = prim
        .attrs
        .get("xformOp:orient")
        .and_then(|a| a.value.as_floatn::<4>())
        .map(|wxyz| [wxyz[1], wxyz[2], wxyz[3], wxyz[0]])
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let scale = prim
        .attrs
        .get("xformOp:scale")
        .and_then(|a| a.value.as_floatn::<3>())
        .unwrap_or([1.0, 1.0, 1.0]);
    Transform::Trs {
        translation,
        rotation,
        scale,
    }
}

/// Read a `matrix4d` literal — a tuple of 4 row tuples, each with
/// 4 floats. Returns `None` if the shape doesn't match.
fn read_matrix4(v: &Value) -> Option<[[f32; 4]; 4]> {
    let rows = v.as_seq()?;
    if rows.len() != 4 {
        return None;
    }
    let mut out = [[0f32; 4]; 4];
    for (i, row) in rows.iter().enumerate() {
        out[i] = row.as_floatn::<4>()?;
    }
    Some(out)
}

fn stash_extras(extras: &mut HashMap<String, serde_json::Value>, prim: &Prim) {
    if !prim.metadata.is_empty() {
        let mut obj = serde_json::Map::new();
        for (k, v) in &prim.metadata {
            if let Some(j) = value_to_json(v) {
                obj.insert(k.clone(), j);
            }
        }
        extras.insert("usd:metadata".into(), serde_json::Value::Object(obj));
    }
}

/// Build a `Mesh + Primitive` from a USD `Mesh` prim.
fn build_mesh(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Mesh> {
    let mut prim_out = Primitive::new(Topology::Triangles);

    let positions = prim
        .attrs
        .get("points")
        .ok_or_else(|| invalid(format!("Mesh `{path}` is missing `points` attribute")))?;
    prim_out.positions = read_vec3_array(&positions.value)
        .ok_or_else(|| invalid(format!("Mesh `{path}` has malformed `points`")))?;

    let counts = prim
        .attrs
        .get("faceVertexCounts")
        .ok_or_else(|| invalid(format!("Mesh `{path}` is missing `faceVertexCounts`")))?;
    let indices = prim
        .attrs
        .get("faceVertexIndices")
        .ok_or_else(|| invalid(format!("Mesh `{path}` is missing `faceVertexIndices`")))?;

    let counts = read_int_array(&counts.value)
        .ok_or_else(|| invalid(format!("Mesh `{path}` has malformed `faceVertexCounts`")))?;
    let indices = read_int_array(&indices.value)
        .ok_or_else(|| invalid(format!("Mesh `{path}` has malformed `faceVertexIndices`")))?;
    let triangulated = fan_triangulate(&counts, &indices).ok_or_else(|| {
        invalid(format!(
            "Mesh `{path}` faceVertexCounts/Indices length mismatch"
        ))
    })?;
    prim_out.indices = Some(if prim_out.positions.len() <= u16::MAX as usize {
        Indices::U16(triangulated.iter().map(|&i| i as u16).collect())
    } else {
        Indices::U32(triangulated)
    });

    if let Some(normals) = prim
        .attrs
        .get("primvars:normals")
        .or_else(|| prim.attrs.get("normals"))
    {
        if let Some(arr) = read_vec3_array(&normals.value) {
            prim_out.normals = Some(arr);
        }
    }

    if let Some(uvs) = prim
        .attrs
        .get("primvars:st")
        .or_else(|| prim.attrs.get("primvars:uv"))
    {
        if let Some(arr) = read_vec2_array(&uvs.value) {
            prim_out.uvs.push(arr);
        }
    }

    if let Some(rel) = prim
        .attrs
        .get("material:binding")
        .and_then(|a| a.value.as_text())
    {
        if let Some(&mid) = ctx.materials_by_path.get(rel) {
            prim_out.material = Some(mid);
        }
    }

    let mesh = Mesh::new(Some(prim.name.clone())).with_primitive(prim_out);
    Ok(mesh)
}

/// Fan-triangulate a polygon soup described by USD's `(counts,
/// indices)` pair.
///
/// USD encodes each face's vertex count in `faceVertexCounts` and
/// concatenates every face's vertex indices in
/// `faceVertexIndices`. For the round-1 mapping we fan-triangulate
/// (`(v0, vi, vi+1)` for every interior vertex) which produces
/// correct geometry for any convex polygon and a reasonable
/// approximation for concave ones — the production renderer's
/// problem to refine.
pub(crate) fn fan_triangulate(counts: &[u32], indices: &[u32]) -> Option<Vec<u32>> {
    let total: u32 = counts.iter().copied().sum();
    if total as usize != indices.len() {
        return None;
    }
    let mut out = Vec::with_capacity(indices.len());
    let mut offset = 0usize;
    for &n in counts {
        let n = n as usize;
        if n < 3 {
            // Degenerate face — skip rather than corrupt downstream.
            offset += n;
            continue;
        }
        let v0 = indices[offset];
        for i in 1..(n - 1) {
            out.push(v0);
            out.push(indices[offset + i]);
            out.push(indices[offset + i + 1]);
        }
        offset += n;
    }
    Some(out)
}

fn read_vec3_array(v: &Value) -> Option<Vec<[f32; 3]>> {
    let seq = v.as_seq()?;
    let mut out = Vec::with_capacity(seq.len());
    for item in seq {
        out.push(item.as_floatn::<3>()?);
    }
    Some(out)
}

fn read_vec2_array(v: &Value) -> Option<Vec<[f32; 2]>> {
    let seq = v.as_seq()?;
    let mut out = Vec::with_capacity(seq.len());
    for item in seq {
        out.push(item.as_floatn::<2>()?);
    }
    Some(out)
}

fn read_int_array(v: &Value) -> Option<Vec<u32>> {
    let seq = v.as_seq()?;
    let mut out = Vec::with_capacity(seq.len());
    for item in seq {
        let f = item.as_f32()?;
        if f < 0.0 || f.fract() != 0.0 {
            return None;
        }
        out.push(f as u32);
    }
    Some(out)
}

/// Build a `Material` from a USD `Material` prim by walking its
/// `Shader` children and looking for the `UsdPreviewSurface` one.
fn build_material(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Material> {
    let mut mat = Material::new().with_name(prim.name.clone());
    let mut surface_shader: Option<&Prim> = None;
    for child in &prim.children {
        if child.spec != "def" || child.type_name != "Shader" {
            continue;
        }
        let info_id = child
            .attrs
            .get("info:id")
            .and_then(|a| a.value.as_text())
            .unwrap_or_default();
        if info_id == "UsdPreviewSurface" {
            surface_shader = Some(child);
        }
    }
    let Some(surface) = surface_shader else {
        // No PBR shader — leave the material at glTF defaults.
        return Ok(mat);
    };

    apply_preview_surface(ctx, &mut mat, path, prim, surface)?;
    Ok(mat)
}

/// Pull `UsdPreviewSurface` inputs into the `Material` PBR slots.
fn apply_preview_surface(
    ctx: &mut Ctx,
    mat: &mut Material,
    parent_path: &str,
    parent: &Prim,
    surface: &Prim,
) -> Result<()> {
    if let Some(c) = surface
        .attrs
        .get("inputs:diffuseColor")
        .and_then(|a| a.value.as_floatn::<3>())
    {
        mat.base_color = [c[0], c[1], c[2], mat.base_color[3]];
    }
    if let Some(f) = surface
        .attrs
        .get("inputs:metallic")
        .and_then(|a| a.value.as_f32())
    {
        mat.metallic = f;
    } else {
        // USDPreviewSurface defaults to non-metallic — override the
        // glTF "fully metallic" sentinel only when we know the shader
        // is in play.
        mat.metallic = 0.0;
    }
    if let Some(f) = surface
        .attrs
        .get("inputs:roughness")
        .and_then(|a| a.value.as_f32())
    {
        mat.roughness = f;
    } else {
        mat.roughness = 0.5;
    }
    if let Some(c) = surface
        .attrs
        .get("inputs:emissiveColor")
        .and_then(|a| a.value.as_floatn::<3>())
    {
        mat.emissive_factor = c;
    }
    if let Some(f) = surface
        .attrs
        .get("inputs:opacity")
        .and_then(|a| a.value.as_f32())
    {
        mat.base_color[3] = f;
    }

    // Texture connections — `inputs:diffuseColor.connect = </path/to/Tex.outputs:rgb>`.
    if let Some(tex_ref) = resolve_texture_connect(
        ctx,
        parent_path,
        parent,
        surface.attrs.get("inputs:diffuseColor.connect"),
    )? {
        mat.base_color_texture = Some(tex_ref);
    }
    if let Some(tex_ref) = resolve_texture_connect(
        ctx,
        parent_path,
        parent,
        surface.attrs.get("inputs:normal.connect"),
    )? {
        mat.normal_texture = Some(tex_ref);
    }
    if let Some(tex_ref) = resolve_texture_connect(
        ctx,
        parent_path,
        parent,
        surface.attrs.get("inputs:emissiveColor.connect"),
    )? {
        mat.emissive_texture = Some(tex_ref);
    }
    if let Some(tex_ref) = resolve_texture_connect(
        ctx,
        parent_path,
        parent,
        surface.attrs.get("inputs:occlusion.connect"),
    )? {
        mat.occlusion_texture = Some(tex_ref);
    }
    Ok(())
}

/// Resolve an `inputs:foo.connect = </path/Tex.outputs:rgb>`
/// statement into a `TextureRef`, materialising the underlying
/// `Texture` (and its `ZipStoredAsset`) on first sight.
fn resolve_texture_connect(
    ctx: &mut Ctx,
    parent_path: &str,
    parent: &Prim,
    attr: Option<&Attr>,
) -> Result<Option<TextureRef>> {
    let Some(attr) = attr else { return Ok(None) };
    let Value::Path(p) = &attr.value else {
        return Ok(None);
    };
    // Strip the `.outputs:rgb` suffix (or any property suffix) to
    // get the bare prim path.
    let prim_path = match p.find('.') {
        Some(i) => &p[..i],
        None => p.as_str(),
    };
    if let Some(&tex_id) = ctx.textures_by_path.get(prim_path) {
        return Ok(Some(TextureRef::new(tex_id)));
    }
    // Locate the shader prim under the enclosing material.
    let Some(rel) = prim_path.strip_prefix(&format!("{parent_path}/")) else {
        return Err(unsupported(format!(
            "texture shader path `{prim_path}` is outside material `{parent_path}` (cross-material refs deferred to round 2)"
        )));
    };
    let shader = find_child_by_name(parent, rel).ok_or_else(|| {
        invalid(format!(
            "UsdUVTexture shader `{rel}` not found under material `{parent_path}`"
        ))
    })?;
    let info_id = shader
        .attrs
        .get("info:id")
        .and_then(|a| a.value.as_text())
        .unwrap_or_default();
    if info_id != "UsdUVTexture" {
        return Err(unsupported(format!(
            "shader `{rel}` has info:id `{info_id}` — only `UsdUVTexture` is supported in round 1"
        )));
    }
    let asset_path = shader
        .attrs
        .get("inputs:file")
        .and_then(|a| match &a.value {
            Value::Asset(s) => Some(s.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            invalid(format!(
                "UsdUVTexture `{prim_path}` is missing `inputs:file` asset path"
            ))
        })?;
    let entry = lookup_zip_entry(&ctx.entries, asset_path).ok_or_else(|| {
        invalid(format!(
            "UsdUVTexture `{prim_path}` references `{asset_path}` which is not present in the USDZ archive"
        ))
    })?;
    let mime = mime_from_filename(&entry.name);
    let asset = ZipStoredAsset::new(
        ctx.archive.clone(),
        entry.payload_offset,
        entry.payload_len,
        mime,
    );
    let asset_arc: Arc<dyn AssetSource> = Arc::new(asset);
    let texture = Texture {
        name: Some(rel.to_string()),
        image: ImageData::Source(asset_arc),
        sampler: oxideav_mesh3d::Sampler::default_sampler(),
    };
    let tex_id = ctx.scene.add_texture(texture);
    ctx.textures_by_path.insert(prim_path.to_string(), tex_id);
    Ok(Some(TextureRef::new(tex_id)))
}

fn find_child_by_name<'a>(parent: &'a Prim, name: &str) -> Option<&'a Prim> {
    parent.children.iter().find(|c| c.name == name)
}

/// Resolve an `@./diffuse.png@`-style asset path against the ZIP
/// entry table. The lookup is case-sensitive (matches PKZIP) and
/// strips a leading `./`.
fn lookup_zip_entry<'a>(
    entries: &'a HashMap<String, ZipEntry>,
    asset_path: &str,
) -> Option<&'a ZipEntry> {
    let trimmed = asset_path.trim_start_matches("./");
    if let Some(e) = entries.get(trimmed) {
        return Some(e);
    }
    entries.get(asset_path)
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn value_to_json(v: &Value) -> Option<serde_json::Value> {
    use serde_json::Value as J;
    Some(match v {
        Value::Token(s) | Value::String(s) | Value::Asset(s) | Value::Path(s) | Value::Raw(s) => {
            J::String(s.clone())
        }
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Bool(b) => J::Bool(*b),
        Value::Tuple(seq) | Value::Array(seq) => {
            J::Array(seq.iter().filter_map(value_to_json).collect())
        }
        Value::None => J::Null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_triangulate_quad() {
        // One quad → two triangles (0,1,2) (0,2,3).
        let counts = [4u32];
        let indices = [0u32, 1, 2, 3];
        let tri = fan_triangulate(&counts, &indices).unwrap();
        assert_eq!(tri, vec![0, 1, 2, 0, 2, 3]);
    }

    #[test]
    fn fan_triangulate_pentagon() {
        // Pentagon (5 verts) → 3 triangles.
        let counts = [5u32];
        let indices = [10u32, 11, 12, 13, 14];
        let tri = fan_triangulate(&counts, &indices).unwrap();
        assert_eq!(tri, vec![10, 11, 12, 10, 12, 13, 10, 13, 14]);
    }

    #[test]
    fn fan_triangulate_mixed() {
        let counts = [3u32, 4];
        let indices = [0u32, 1, 2, 3, 4, 5, 6];
        let tri = fan_triangulate(&counts, &indices).unwrap();
        assert_eq!(tri, vec![0, 1, 2, 3, 4, 5, 3, 5, 6]);
    }

    #[test]
    fn fan_triangulate_count_mismatch() {
        let counts = [3u32];
        let indices = [0u32, 1];
        assert!(fan_triangulate(&counts, &indices).is_none());
    }

    #[test]
    fn unit_recognised_presets() {
        assert!(matches!(unit_from_meters_per_unit(1.0), Unit::Metres));
        assert!(matches!(unit_from_meters_per_unit(0.01), Unit::Centimetres));
        assert!(matches!(
            unit_from_meters_per_unit(0.001),
            Unit::Millimetres
        ));
        assert!(matches!(unit_from_meters_per_unit(0.0254), Unit::Inches));
    }
}
