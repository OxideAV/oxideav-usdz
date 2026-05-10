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
    AssetSource, AudioData, AudioEmitter, AudioSource, AudioSourceId, AuralMode, Axis, ImageData,
    Indices, Material, Mesh, Node, Primitive, Scene3D, SpatialAudio, Texture, TextureRef, Topology,
    Transform, Unit,
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
///
/// Sibling-Mesh folding rule (added in r3): when an Xform/Scope
/// parent contains multiple `Mesh` children whose names share a
/// common stem (`Foo`, `Foo_1`, `Foo_2` — i.e. base name +
/// optional `_<digits>` suffix), they fold into a SINGLE
/// [`Mesh`](oxideav_mesh3d::Mesh) carrying N
/// [`Primitive`](oxideav_mesh3d::Primitive)s. This is the inverse
/// of the multi-primitive emission rule in `usda_writer`'s
/// `write_mesh`: a Scene3D Mesh with N primitives serialises as
/// N sibling Mesh prims and round-trips back into one Mesh on
/// decode. Hand-authored USD with sibling Mesh prims that don't
/// match the convention (`HeadGeo`, `BodyGeo`) is unaffected —
/// each becomes its own Scene3D Mesh as before.
fn build_node(ctx: &mut Ctx, parent: &str, prim: &Prim) -> Result<Option<oxideav_mesh3d::NodeId>> {
    if prim.spec != "def" {
        return Ok(None);
    }
    let path = join_path(parent, &prim.name);
    match prim.type_name.as_str() {
        "Material" | "Shader" => Ok(None),
        "SpatialAudio" => {
            let (source, emitter) = build_audio_emitter(ctx, &path, prim)?;
            let source_id = ctx.scene.add_audio_source(source);
            let emitter_with_src = AudioEmitter {
                source: source_id,
                ..emitter
            };
            let emitter_id = ctx.scene.add_audio_emitter(emitter_with_src);
            let mut node = Node::new()
                .with_name(prim.name.clone())
                .with_audio_emitter(emitter_id);
            node.transform = read_node_transform(prim);
            stash_extras(&mut node.extras, prim);
            let id = ctx.scene.add_node(node);
            Ok(Some(id))
        }
        "Xform" | "Scope" | "" => {
            let mut node = Node::new().with_name(prim.name.clone());
            node.transform = read_node_transform(prim);
            // Recurse children — collect the scene-graph children
            // first, push attribute extras after. Mesh children
            // sharing a common stem fold into a single Mesh per
            // the r3 multi-primitive convention; everything else
            // recurses into `build_node` one-at-a-time.
            //
            // Round-5 wrinkle: a `def Mesh` prim carrying the
            // `(usd:no_fold = 1)` metadata flag opts out of the
            // fold heuristic — it always becomes its own Scene3D
            // Mesh + Node, even when sibling stems would otherwise
            // group it. Mirrors the encoder side, which sets the
            // flag when re-emitting `Primitive::extras["usd:no_fold"]`.
            let mut child_ids = Vec::new();
            let mut i = 0usize;
            while i < prim.children.len() {
                let child = &prim.children[i];
                if child.spec == "def"
                    && (child.type_name == "Mesh"
                        || child.type_name == "BasisCurves"
                        || child.type_name == "Points")
                {
                    if child.type_name == "Mesh" && !prim_no_fold(child) {
                        // Look ahead for additional Mesh siblings
                        // sharing the same stem so they fold into one
                        // Scene3D Mesh — but only when none of them
                        // carries the `usd:no_fold` opt-out.
                        let stem = mesh_name_stem(&child.name);
                        let mut group_end = i + 1;
                        while group_end < prim.children.len()
                            && prim.children[group_end].spec == "def"
                            && prim.children[group_end].type_name == "Mesh"
                            && mesh_name_stem(&prim.children[group_end].name) == stem
                            && !prim_no_fold(&prim.children[group_end])
                        {
                            group_end += 1;
                        }
                        let group = &prim.children[i..group_end];
                        let id = build_mesh_group(ctx, &path, group)?;
                        child_ids.push(id);
                        i = group_end;
                    } else {
                        // Standalone primitive — build a single
                        // Scene3D Mesh + Node directly. Covers the
                        // `usd:no_fold` Mesh opt-out, plus
                        // BasisCurves / Points which the fold
                        // convention doesn't apply to anyway.
                        let id = build_standalone_mesh_node(ctx, &path, child)?;
                        child_ids.push(id);
                        i += 1;
                    }
                } else if let Some(id) = build_node(ctx, &path, child)? {
                    child_ids.push(id);
                    i += 1;
                } else {
                    i += 1;
                }
            }
            node.children = child_ids;
            stash_extras(&mut node.extras, prim);
            let id = ctx.scene.add_node(node);
            Ok(Some(id))
        }
        "Mesh" | "BasisCurves" | "Points" => {
            // Top-level prim with no enclosing Xform — the
            // sibling-fold doesn't apply (we have no parent's
            // child list to scan). Build it as a single-primitive
            // Mesh + Node, matching r1/r2 behaviour.
            let id = build_standalone_mesh_node(ctx, &path, prim)?;
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

/// Strip a trailing `_<digits>` suffix to recover the "stem" used
/// by the multi-primitive emission rule. `Foo` → `Foo`,
/// `Foo_1` → `Foo`, `Foo_12` → `Foo`, `Foo_bar` → `Foo_bar`,
/// `Foo_` → `Foo_` (trailing underscore with no digits stays).
fn mesh_name_stem(name: &str) -> &str {
    let Some(idx) = name.rfind('_') else {
        return name;
    };
    let suffix = &name[idx + 1..];
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return name;
    }
    &name[..idx]
}

/// Build a single Scene3D Mesh + Node bundle by folding `group`
/// (one or more sibling Mesh prims sharing a name stem) into one
/// Mesh whose `primitives` vector has one entry per group member.
fn build_mesh_group(
    ctx: &mut Ctx,
    parent_path: &str,
    group: &[Prim],
) -> Result<oxideav_mesh3d::NodeId> {
    debug_assert!(!group.is_empty());
    let head = &group[0];
    let head_path = join_path(parent_path, &head.name);
    // Build the first prim's mesh as the seed; this captures the
    // mesh name (which we always strip back to the stem so the
    // round-trip preserves the Scene3D mesh name exactly).
    let mut seed_mesh = build_mesh(ctx, &head_path, head)?;
    let stem = mesh_name_stem(&head.name).to_string();
    seed_mesh.name = Some(stem);
    // Append each subsequent sibling's primitive.
    for sibling in &group[1..] {
        let sibling_path = join_path(parent_path, &sibling.name);
        let extra = build_mesh(ctx, &sibling_path, sibling)?;
        for p in extra.primitives {
            seed_mesh.primitives.push(p);
        }
    }
    let mesh_id = ctx.scene.add_mesh(seed_mesh);
    let mut node = Node::new().with_name(head.name.clone()).with_mesh(mesh_id);
    node.transform = read_node_transform(head);
    stash_extras(&mut node.extras, head);
    Ok(ctx.scene.add_node(node))
}

/// Build a single Scene3D Mesh + Node from one Mesh / BasisCurves /
/// Points prim — the unfolded path used both for top-level prims
/// and for any inner def-Mesh that opted out of the fold via
/// `usd:no_fold`.
fn build_standalone_mesh_node(
    ctx: &mut Ctx,
    parent_path: &str,
    prim: &Prim,
) -> Result<oxideav_mesh3d::NodeId> {
    let path = join_path(parent_path, &prim.name);
    let mesh = build_mesh(ctx, &path, prim)?;
    let mesh_id = ctx.scene.add_mesh(mesh);
    let mut node = Node::new().with_name(prim.name.clone()).with_mesh(mesh_id);
    node.transform = read_node_transform(prim);
    stash_extras(&mut node.extras, prim);
    Ok(ctx.scene.add_node(node))
}

/// `true` when the prim's metadata block carries
/// `usd:no_fold = 1` (or `usd:no_fold = true` — we accept either
/// spelling). Set by the round-5 encoder on `Primitive::extras`
/// round-trip; honoured by the decoder to skip the sibling-fold
/// heuristic.
fn prim_no_fold(prim: &Prim) -> bool {
    let Some(v) = prim.metadata.get("usd:no_fold") else {
        return false;
    };
    match v {
        Value::Bool(b) => *b,
        Value::Float(f) => *f != 0.0,
        Value::Token(s) | Value::String(s) => {
            matches!(s.as_str(), "true" | "1" | "yes" | "on")
        }
        _ => false,
    }
}

/// Build an [`AudioSource`] + [`AudioEmitter`] pair from a
/// `def SpatialAudio` prim per USD's `UsdMediaSpatialAudio` schema.
///
/// Field mapping (clean-room — built from the round-4 dispatch
/// brief's prose, no USD library code consulted):
///
/// * `uniform asset filePath = @path@` — the audio asset reference.
///   When `@path@` resolves against an inner ZIP entry the
///   [`AudioSource::data`] becomes
///   [`AudioData::Source`](`oxideav_mesh3d::AudioData::Source`)
///   wrapping a [`ZipStoredAsset`] (so the writer's USDZ → USDZ
///   pass-through fires for audio just like it does for textures).
///   When the path is external (no ZIP match) the source becomes
///   [`AudioData::External`](`oxideav_mesh3d::AudioData::External`)
///   carrying the raw URI for downstream resolution.
/// * `uniform token auralMode` — `"spatial"` →
///   [`AuralMode::SpatialNonAcoustic`] (USD's positional default —
///   panning + distance attenuation), `"nonSpatial"` →
///   [`AuralMode::SpatialAcoustic`]. The original token is also
///   stashed into `emitter.extras["usd:auralMode"]` so the writer
///   can round-trip the exact spelling.
/// * `uniform double gain` — clamped into
///   [`AudioEmitter::gain`].
/// * `uniform double startTime` / `endTime` / `mediaOffset` — held
///   on the source's `extras` (`usd:startTime`, `usd:endTime`,
///   `usd:mediaOffset`).
/// * `uniform double fillBufferTime` — held on the emitter's
///   `extras` (`usd:fillBufferTime`).
///
/// The emitter returned has a sentinel
/// [`AudioSource`](oxideav_mesh3d::AudioSource) id of `0`; the
/// caller registers the source with the scene and replaces the id
/// before pushing the emitter — this keeps `build_audio_emitter`
/// pure (no `Scene3D` mutation) so test cases can reuse the helper.
fn build_audio_emitter(ctx: &Ctx, path: &str, prim: &Prim) -> Result<(AudioSource, AudioEmitter)> {
    // filePath — required per the USD schema; without it the prim
    // is malformed and we don't have anything to play.
    let file_attr = prim
        .attrs
        .get("filePath")
        .ok_or_else(|| invalid(format!("SpatialAudio `{path}` missing `filePath`")))?;
    let asset_path = match &file_attr.value {
        Value::Asset(s) => s.as_str(),
        // Tolerate quoted-string spellings (rare but legal in USD
        // because the type system also accepts `string`).
        Value::String(s) => s.as_str(),
        _ => {
            return Err(invalid(format!(
                "SpatialAudio `{path}` `filePath` must be an asset reference (`@...@`)"
            )))
        }
    };

    let mut source = if let Some(entry) = lookup_zip_entry(&ctx.entries, asset_path) {
        // In-archive — wrap in ZipStoredAsset so the writer's
        // pass-through optimisation fires.
        let mime = mime_from_filename(&entry.name);
        let asset = ZipStoredAsset::new(
            ctx.archive.clone(),
            entry.payload_offset,
            entry.payload_len,
            mime.clone(),
        );
        let arc: Arc<dyn AssetSource> = Arc::new(asset);
        AudioSource {
            name: Some(prim.name.clone()),
            data: AudioData::Source(arc),
            extras: HashMap::new(),
        }
    } else {
        // External (or unresolved) — keep the URI so the consumer
        // can fetch lazily.
        AudioSource {
            name: Some(prim.name.clone()),
            data: AudioData::External {
                uri: asset_path.to_string(),
                mime: mime_from_filename(asset_path),
            },
            extras: HashMap::new(),
        }
    };

    // auralMode — token; USD defaults to "spatial". Map both
    // documented tokens, otherwise fall back to the "spatial"
    // semantics.
    let aural_token = prim
        .attrs
        .get("auralMode")
        .and_then(|a| match &a.value {
            Value::Token(s) | Value::String(s) => Some(s.clone()),
            // The schema's listed type is `uniform token[]`; tolerate
            // an array of one token.
            Value::Array(items) => items.first().and_then(|v| match v {
                Value::Token(s) | Value::String(s) => Some(s.clone()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_else(|| "spatial".to_string());
    let aural_mode = aural_mode_from_token(&aural_token);

    // gain — defaults to 1.0 per the schema.
    let gain = prim
        .attrs
        .get("gain")
        .and_then(|a| a.value.as_f32())
        .unwrap_or(1.0);

    // Per-source playback knobs go on AudioSource.extras so the
    // writer can round-trip them.
    for (key, dst_key) in [
        ("startTime", "usd:startTime"),
        ("endTime", "usd:endTime"),
        ("mediaOffset", "usd:mediaOffset"),
    ] {
        if let Some(v) = prim.attrs.get(key).and_then(|a| value_to_json(&a.value)) {
            source.extras.insert(dst_key.into(), v);
        }
    }

    let mut emitter = AudioEmitter::new(AudioSourceId(0)).with_name(prim.name.clone());
    emitter.gain = gain;
    emitter.spatial = Some(SpatialAudio {
        aural_mode,
        ..SpatialAudio::default()
    });
    emitter.extras.insert(
        "usd:auralMode".into(),
        serde_json::Value::String(aural_token),
    );

    if let Some(v) = prim
        .attrs
        .get("fillBufferTime")
        .and_then(|a| value_to_json(&a.value))
    {
        emitter.extras.insert("usd:fillBufferTime".into(), v);
    }

    Ok((source, emitter))
}

/// USD `auralMode` token → [`AuralMode`]. `"spatial"` is the
/// schema default ("the audio source plays spatially in the
/// scene"); `"nonSpatial"` requests global / non-positional
/// playback. We map both into [`AuralMode`] variants so callers
/// don't lose information; the original spelling is preserved in
/// `emitter.extras["usd:auralMode"]` for byte-faithful round-trip.
fn aural_mode_from_token(token: &str) -> AuralMode {
    match token {
        "spatial" => AuralMode::SpatialNonAcoustic,
        "nonSpatial" => AuralMode::SpatialAcoustic,
        // Forward-compatible default — unknown tokens keep the
        // panning+distance behaviour the schema documents.
        _ => AuralMode::SpatialNonAcoustic,
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

/// Build a `Mesh + Primitive` from a USD `Mesh` / `BasisCurves` /
/// `Points` prim.
///
/// The Scene3D model collapses all three USD geometry prim types
/// into a [`Primitive`] keyed off [`Topology`]. The dispatch below
/// mirrors the writer's encoding rules:
///
/// * `Mesh` → `Topology::Triangles` (USD always sends triangle
///   lists via `faceVertexCounts` after fan-triangulation).
/// * `BasisCurves` → `Topology::Lines / LineStrip / LineLoop` per
///   the schema's `wrap` token (`periodic` → LineLoop) and
///   `curveVertexCounts` shape (`[2,2,2,…]` → Lines, single count
///   → LineStrip).
/// * `Points` → `Topology::Points`.
///
/// Round-5 hints picked up from the prim's `(...)` metadata block:
///
/// * `usd:original_topology` — when the writer rewrote a strip /
///   fan / lines / points source into the schema-typed prim, the
///   original token is preserved here. We surface it on
///   `Primitive::extras["usd:original_topology"]`.
/// * `usd:no_fold` — surfaces on
///   `Primitive::extras["usd:no_fold"] = true` so a re-encode
///   propagates the opt-out.
/// * `usd:mesh_transform` — per-prim transform serialised via
///   `xformOp:transform` directly on the def Mesh (rather than the
///   parent Xform). Read back into
///   `Primitive::extras["usd:mesh_transform"]` as a Matrix-shaped
///   JSON object.
fn build_mesh(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Mesh> {
    let prim_out = match prim.type_name.as_str() {
        "BasisCurves" => build_basis_curves_primitive(ctx, path, prim)?,
        "Points" => build_points_primitive(ctx, path, prim)?,
        // "Mesh" or anything else routes through the triangle path.
        _ => build_triangle_primitive(ctx, path, prim)?,
    };

    let mesh = Mesh::new(Some(prim.name.clone())).with_primitive(prim_out);
    Ok(mesh)
}

/// Build a `Topology::Triangles` primitive from a USD `Mesh` prim.
/// Inverse of [`crate::usda_writer::write_one_mesh_prim`].
fn build_triangle_primitive(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Primitive> {
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

    apply_mesh_metadata_to_primitive(prim, &mut prim_out);
    Ok(prim_out)
}

/// Build a Lines / LineStrip / LineLoop primitive from a USD
/// `BasisCurves` prim. Inverse of
/// [`crate::usda_writer::write_basis_curves_prim`].
///
/// Topology choice:
///
/// * `wrap = "periodic"` → `Topology::LineLoop`.
/// * `curveVertexCounts` is all `2`s → `Topology::Lines` (one
///   straight segment per pair).
/// * Otherwise → `Topology::LineStrip`.
///
/// Width (`widths` attribute) is currently dropped — the typed
/// model has no per-point thickness slot.
fn build_basis_curves_primitive(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Primitive> {
    let positions = prim.attrs.get("points").ok_or_else(|| {
        invalid(format!(
            "BasisCurves `{path}` is missing `points` attribute"
        ))
    })?;
    let positions = read_vec3_array(&positions.value)
        .ok_or_else(|| invalid(format!("BasisCurves `{path}` has malformed `points`")))?;

    let counts_attr = prim.attrs.get("curveVertexCounts").ok_or_else(|| {
        invalid(format!(
            "BasisCurves `{path}` is missing `curveVertexCounts`"
        ))
    })?;
    let counts = read_int_array(&counts_attr.value).ok_or_else(|| {
        invalid(format!(
            "BasisCurves `{path}` has malformed `curveVertexCounts`"
        ))
    })?;

    let wrap = prim
        .attrs
        .get("wrap")
        .and_then(|a| a.value.as_text())
        .unwrap_or("nonperiodic");
    let topology = if wrap == "periodic" {
        Topology::LineLoop
    } else if !counts.is_empty() && counts.iter().all(|&c| c == 2) {
        Topology::Lines
    } else {
        Topology::LineStrip
    };

    let mut prim_out = Primitive::new(topology);
    prim_out.positions = positions;
    // Synthesise a 0..N index buffer so the consumer can iterate
    // the geometry uniformly with the Mesh path's index-based
    // primitives.
    let n = prim_out.positions.len();
    prim_out.indices = Some(if n <= u16::MAX as usize {
        Indices::U16((0..n as u16).collect())
    } else {
        Indices::U32((0..n as u32).collect())
    });

    if let Some(rel) = prim
        .attrs
        .get("material:binding")
        .and_then(|a| a.value.as_text())
    {
        if let Some(&mid) = ctx.materials_by_path.get(rel) {
            prim_out.material = Some(mid);
        }
    }

    apply_mesh_metadata_to_primitive(prim, &mut prim_out);
    Ok(prim_out)
}

/// Build a `Topology::Points` primitive from a USD `Points` prim.
/// Inverse of [`crate::usda_writer::write_points_prim`].
fn build_points_primitive(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Primitive> {
    let positions = prim
        .attrs
        .get("points")
        .ok_or_else(|| invalid(format!("Points `{path}` is missing `points` attribute")))?;
    let positions = read_vec3_array(&positions.value)
        .ok_or_else(|| invalid(format!("Points `{path}` has malformed `points`")))?;

    let mut prim_out = Primitive::new(Topology::Points);
    let n = positions.len();
    prim_out.positions = positions;
    prim_out.indices = Some(if n <= u16::MAX as usize {
        Indices::U16((0..n as u16).collect())
    } else {
        Indices::U32((0..n as u32).collect())
    });

    if let Some(rel) = prim
        .attrs
        .get("material:binding")
        .and_then(|a| a.value.as_text())
    {
        if let Some(&mid) = ctx.materials_by_path.get(rel) {
            prim_out.material = Some(mid);
        }
    }

    apply_mesh_metadata_to_primitive(prim, &mut prim_out);
    Ok(prim_out)
}

/// Surface the round-5 prim-metadata hints
/// (`usd:no_fold`, `usd:original_topology`, `usd:mesh_transform`)
/// onto `Primitive::extras` so a re-encode round-trips them.
///
/// Also lifts a Mesh-level `xformOp:transform` opinion (the
/// per-Mesh transform path) into a JSON-shaped
/// `usd:mesh_transform = {"matrix": [[...], …]}` extras entry.
fn apply_mesh_metadata_to_primitive(prim: &Prim, out: &mut Primitive) {
    if prim_no_fold(prim) {
        out.extras
            .insert("usd:no_fold".into(), serde_json::Value::Bool(true));
    }
    if let Some(v) = prim.metadata.get("usd:original_topology") {
        if let Some(s) = v.as_text() {
            out.extras.insert(
                "usd:original_topology".into(),
                serde_json::Value::String(s.to_string()),
            );
        }
    }
    if let Some(t) = read_inner_mesh_transform(prim) {
        out.extras.insert("usd:mesh_transform".into(), t);
    }
}

/// Read an inner-def-Mesh `xformOp:transform` opinion (or the
/// matching TRS triple) into a JSON-shaped value suitable for the
/// `usd:mesh_transform` extras slot.
///
/// Output schema mirrors what
/// [`crate::usda_writer::transform_from_extras`] consumes:
///
/// * Matrix opinion → `{"matrix": [[a,b,c,d], …]}` (4 rows of 4).
/// * TRS triple → `{"trs": {"translation": [...], "rotation": [...],
///   "scale": [...]}}`.
///
/// Returns `None` when the prim doesn't carry an `xformOpOrder`
/// (the common case — most authoring tools put the transform on
/// the parent Xform, not the inner def Mesh).
fn read_inner_mesh_transform(prim: &Prim) -> Option<serde_json::Value> {
    let order_attr = prim.attrs.get("xformOpOrder")?;
    let order_seq = order_attr.value.as_seq()?;
    let order: Vec<&str> = order_seq.iter().filter_map(|v| v.as_text()).collect();
    if order.is_empty() {
        return None;
    }
    if order.len() == 1 && order[0] == "xformOp:transform" {
        let m_attr = prim.attrs.get("xformOp:transform")?;
        let m = read_matrix4(&m_attr.value)?;
        let rows: Vec<serde_json::Value> = m
            .iter()
            .map(|row| {
                serde_json::Value::Array(
                    row.iter()
                        .filter_map(|c| {
                            serde_json::Number::from_f64(*c as f64).map(serde_json::Value::Number)
                        })
                        .collect(),
                )
            })
            .collect();
        let mut obj = serde_json::Map::new();
        obj.insert("matrix".into(), serde_json::Value::Array(rows));
        return Some(serde_json::Value::Object(obj));
    }
    if order
        .iter()
        .all(|t| matches!(*t, "xformOp:translate" | "xformOp:orient" | "xformOp:scale"))
    {
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
        let to_arr = |xs: &[f32]| {
            serde_json::Value::Array(
                xs.iter()
                    .filter_map(|c| {
                        serde_json::Number::from_f64(*c as f64).map(serde_json::Value::Number)
                    })
                    .collect(),
            )
        };
        let mut trs = serde_json::Map::new();
        trs.insert("translation".into(), to_arr(&translation));
        trs.insert("rotation".into(), to_arr(&rotation));
        trs.insert("scale".into(), to_arr(&scale));
        let mut obj = serde_json::Map::new();
        obj.insert("trs".into(), serde_json::Value::Object(trs));
        return Some(serde_json::Value::Object(obj));
    }
    None
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
        // Round 1 just preserves the *shape* of dictionaries — the
        // contents flatten to a JSON object that mirrors the typed
        // entries (recursively) so a future custom-data extractor
        // can read them back.
        Value::Dict(map) => J::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v).unwrap_or(J::Null)))
                .collect(),
        ),
        // `references = @file@</Prim>` — flatten to "asset</prim>"
        // for the JSON view; consumers wanting the parts can walk
        // the prim tree directly.
        Value::AssetWithPath { asset, prim_path } => J::String(format!("{asset}<{prim_path}>")),
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

    #[test]
    fn mesh_name_stem_strips_digit_suffix() {
        assert_eq!(mesh_name_stem("Body"), "Body");
        assert_eq!(mesh_name_stem("Body_1"), "Body");
        assert_eq!(mesh_name_stem("Body_12"), "Body");
        assert_eq!(mesh_name_stem("Body_123456"), "Body");
    }

    #[test]
    fn aural_mode_token_round_trip() {
        assert_eq!(
            aural_mode_from_token("spatial"),
            AuralMode::SpatialNonAcoustic
        );
        assert_eq!(
            aural_mode_from_token("nonSpatial"),
            AuralMode::SpatialAcoustic
        );
        // Unknown tokens default to SpatialNonAcoustic — matches
        // USD's documented default semantics for the field.
        assert_eq!(
            aural_mode_from_token("unknownToken"),
            AuralMode::SpatialNonAcoustic
        );
    }

    #[test]
    fn mesh_name_stem_keeps_non_digit_suffix() {
        // `Body_` (no digits) and `Body_bar` (alpha suffix) must
        // NOT strip — the convention only fires for `_<digits>`.
        assert_eq!(mesh_name_stem("Body_"), "Body_");
        assert_eq!(mesh_name_stem("Body_bar"), "Body_bar");
        assert_eq!(mesh_name_stem("Body_1a"), "Body_1a");
    }
}
