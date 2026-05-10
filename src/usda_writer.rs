//! Serialise a [`Scene3D`] into a USDA (`#usda 1.0`) text layer.
//!
//! The output is the inverse of [`usda::parse`](crate::usda::parse)
//! followed by [`usd_to_scene::translate`](crate::usd_to_scene::translate),
//! in the limited sense that re-decoding what we emit produces a
//! [`Scene3D`] equivalent to the input. Whitespace, attribute order,
//! and metadata key order are deterministic but not necessarily
//! identical to whatever produced the original file.
//!
//! Schema coverage mirrors r1's reader: `Xform`/`Scope` nodes with
//! optional mesh attachment, `UsdGeomMesh` (Triangles topology,
//! positions + first-UV-set + optional normals + material binding),
//! `UsdPreviewSurface` material with `UsdUVTexture` shader children
//! for any bound texture maps. Audio / skinning / animation are
//! still reader-side TODOs and are skipped on the encoder side too.

use std::fmt::Write;

use oxideav_mesh3d::{
    AudioData, AudioEmitter, AudioSource, AuralMode, Axis, ImageData, Indices, Material, Mesh,
    MeshId, NodeId, Primitive, Scene3D, Texture, TextureRef, Topology, Transform, Unit,
};

/// Serialise `scene` to a UTF-8 USDA text layer.
///
/// Returns the text with a trailing newline. A companion call to
/// [`collect_texture_assets`] returns the inner-file list the USDZ
/// writer needs to embed alongside the USDA.
pub fn write_layer(scene: &Scene3D) -> String {
    let mut w = Out::default();
    writeln!(w.s, "#usda 1.0").unwrap();
    write_layer_metadata(&mut w, scene);
    writeln!(w.s).unwrap();

    for &root in &scene.roots {
        write_node(&mut w, scene, root, /*parent_path=*/ "");
    }
    // Materials live outside the node tree in our model — emit any
    // material that wasn't already pulled in as a node child by
    // hanging them off a synthetic `/Materials` Scope. Real-world
    // USDZ assets typically nest materials under their mesh's prim
    // path, but the synthetic scope keeps our output self-consistent
    // and decodable: every `material:binding = </Materials/<name>>`
    // resolves through our reader.
    if !scene.materials.is_empty() {
        writeln!(w.s, "def Scope \"Materials\" {{").unwrap();
        w.indent += 1;
        for (i, mat) in scene.materials.iter().enumerate() {
            write_material(&mut w, scene, mat, i);
        }
        w.indent -= 1;
        writeln!(w.s, "}}").unwrap();
    }
    w.s
}

/// Inner-file payload that the encoder needs to attach to the
/// archive alongside the USDA layer.
///
/// `bytes` is the on-disk form (uncompressed for STORED entries —
/// USDZ's only allowed compression method) and `name` is the
/// archive-relative path the USDA references via
/// `asset inputs:file = @name@`. `from_pass_through` records
/// whether the bytes came straight from a `RawStorage` slice (the
/// USDZ → USDZ optimisation surface) or were materialised through
/// the streaming `open()` path.
#[derive(Debug)]
pub struct EmittedAsset {
    pub name: String,
    pub bytes: Vec<u8>,
    /// `true` when the asset's source exposed
    /// `raw_storage(scheme = "zip-stored")` so the encoder copied
    /// the inner-file bytes verbatim. `false` when the encoder fell
    /// back to `open()` + `read_to_end()`. Used by tests to assert
    /// the optimisation actually fires for USDZ → USDZ pipelines.
    pub from_pass_through: bool,
}

/// Walk every texture in `scene` and pull its bytes out via the
/// `AssetSource` trait. Textures whose source exposes
/// `raw_storage(scheme = "zip-stored")` are returned with their
/// stored bytes verbatim — no copy through `open()`. This is the
/// USDZ → USDZ pass-through optimisation: the input texture's
/// already-aligned ZIP-stored bytes flow straight into the output
/// archive without ever being decoded.
pub fn collect_texture_assets(scene: &Scene3D) -> Vec<EmittedAsset> {
    let mut out = Vec::with_capacity(scene.textures.len());
    for (i, tex) in scene.textures.iter().enumerate() {
        let name = texture_filename(tex, i);
        let (bytes, from_pass_through) = match &tex.image {
            ImageData::Source(asset) => {
                if let Some(raw) = asset.raw_storage() {
                    if raw.scheme == "zip-stored" {
                        (raw.bytes.to_vec(), true)
                    } else {
                        (read_via_open(asset.as_ref()), false)
                    }
                } else {
                    (read_via_open(asset.as_ref()), false)
                }
            }
            ImageData::External { .. } => {
                // External URI — we don't fetch. Skip; the writer
                // will leave the asset reference dangling, which is
                // legal USD (the consumer resolves at load).
                continue;
            }
            #[cfg(feature = "registry")]
            ImageData::Embedded(_) => {
                // Pre-decoded pixels — re-encoding into a real image
                // format is out of scope for r2; skip so we don't
                // emit a broken asset reference.
                continue;
            }
        };
        out.push(EmittedAsset {
            name,
            bytes,
            from_pass_through,
        });
    }
    out
}

/// Walk every audio source in `scene` and pull its bytes out via
/// the [`AssetSource`](oxideav_mesh3d::AssetSource) trait. The
/// `raw_storage(scheme = "zip-stored")` pass-through path fires for
/// audio sources that came in from a sibling USDZ archive (i.e.
/// the reader's [`ZipStoredAsset`](crate::ZipStoredAsset)) — bytes
/// flow straight through with no intermediate buffer. Sources
/// pointing at external URIs ([`AudioData::External`]) are skipped;
/// the writer leaves the asset reference dangling, which is
/// legal USD (the consumer resolves at load).
pub fn collect_audio_assets(scene: &Scene3D) -> Vec<EmittedAsset> {
    let mut out = Vec::with_capacity(scene.audio_sources.len());
    for (i, src) in scene.audio_sources.iter().enumerate() {
        let name = audio_filename(src, i);
        let (bytes, from_pass_through) = match &src.data {
            AudioData::Source(asset) => {
                if let Some(raw) = asset.raw_storage() {
                    if raw.scheme == "zip-stored" {
                        (raw.bytes.to_vec(), true)
                    } else {
                        (read_via_open(asset.as_ref()), false)
                    }
                } else {
                    (read_via_open(asset.as_ref()), false)
                }
            }
            AudioData::External { .. } => continue,
            #[cfg(feature = "registry")]
            AudioData::Embedded(_) => continue,
        };
        out.push(EmittedAsset {
            name,
            bytes,
            from_pass_through,
        });
    }
    out
}

/// Per-source filename used both for the USDA `filePath = @name@`
/// reference and the inner-archive entry name. Mirrors
/// [`texture_filename`] so the two collection paths stay
/// symmetric.
fn audio_filename(src: &AudioSource, idx: usize) -> String {
    let stem = src
        .name
        .as_deref()
        .map(sanitize_filename)
        .unwrap_or_else(|| format!("audio_{idx}"));
    let ext = match &src.data {
        AudioData::Source(asset) => audio_mime_to_ext(asset.mime()),
        AudioData::External { mime, .. } => audio_mime_to_ext(mime.as_deref()),
        #[cfg(feature = "registry")]
        AudioData::Embedded(_) => "wav".to_owned(),
    };
    format!("{stem}.{ext}")
}

fn audio_mime_to_ext(mime: Option<&str>) -> String {
    match mime {
        Some("audio/wav") | Some("audio/x-wav") => "wav",
        Some("audio/mpeg") => "mp3",
        Some("audio/mp4") => "m4a",
        Some("audio/ogg") => "ogg",
        Some("audio/flac") => "flac",
        Some("audio/aac") => "aac",
        _ => "wav",
    }
    .to_owned()
}

fn read_via_open(asset: &dyn oxideav_mesh3d::AssetSource) -> Vec<u8> {
    use std::io::Read;
    let Ok(mut r) = asset.open() else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    buf
}

/// Mirror of [`collect_texture_assets`] for the per-texture filename
/// the USDA `inputs:file` reference uses.
fn texture_filename(tex: &Texture, idx: usize) -> String {
    if let Some(name) = tex.name.as_deref() {
        let stem = sanitize_filename(name);
        let ext = match &tex.image {
            ImageData::Source(asset) => mime_to_ext(asset.mime()),
            _ => "png".to_owned(),
        };
        return format!("{stem}.{ext}");
    }
    let ext = match &tex.image {
        ImageData::Source(asset) => mime_to_ext(asset.mime()),
        _ => "png".to_owned(),
    };
    format!("texture_{idx}.{ext}")
}

fn mime_to_ext(mime: Option<&str>) -> String {
    match mime {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/x-exr") => "exr",
        Some("image/vnd.radiance") => "hdr",
        Some("image/tiff") => "tif",
        Some("image/ktx2") => "ktx2",
        _ => "png",
    }
    .to_owned()
}

/// Strip characters that aren't safe inside a ZIP entry name. USDZ
/// readers (us included) treat the path component-wise; we keep
/// alphanumerics + `-_./`.
fn sanitize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "texture".into()
    } else {
        out
    }
}

// ----------------------------------------------------------------
// Writer state + low-level helpers
// ----------------------------------------------------------------

#[derive(Default)]
struct Out {
    s: String,
    indent: usize,
}

impl Out {
    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.s.push_str("    ");
        }
    }
}

fn write_layer_metadata(w: &mut Out, scene: &Scene3D) {
    let up_axis = match scene.up_axis {
        Axis::PosY => "Y",
        Axis::PosZ => "Z",
        Axis::PosX => "X",
        // USD only canonicalises Y / Z / X; other variants fall
        // back to Y to keep the output schema-valid.
        _ => "Y",
    };
    let mpu = match scene.unit {
        Unit::Metres => 1.0f32,
        Unit::Centimetres => 0.01,
        Unit::Millimetres => 0.001,
        Unit::Inches => 0.0254,
        Unit::Feet => 0.3048,
        Unit::Yards => 0.9144,
    };
    writeln!(w.s, "(").unwrap();
    writeln!(w.s, "    upAxis = \"{up_axis}\"").unwrap();
    writeln!(w.s, "    metersPerUnit = {}", format_float(mpu as f64)).unwrap();
    writeln!(w.s, ")").unwrap();
}

fn write_node(w: &mut Out, scene: &Scene3D, id: NodeId, parent_path: &str) {
    let Some(node) = scene.node(id) else { return };
    let name = node
        .name
        .clone()
        .unwrap_or_else(|| format!("node_{}", id.0));
    let safe_name = sanitize_prim_name(&name);
    let path = if parent_path.is_empty() {
        format!("/{safe_name}")
    } else {
        format!("{parent_path}/{safe_name}")
    };
    // We always emit `Xform` for now — meshes hang off as inner
    // `def Mesh` children rather than collapsing into the node's
    // own prim type. Round-3 work: also synthesise `Camera` /
    // `Light` prim types when the node carries those references.
    w.write_indent();
    writeln!(w.s, "def Xform \"{safe_name}\" {{").unwrap();
    w.indent += 1;

    // Per-node transform → `xformOp:*` opinions. Identity TRS
    // collapses to nothing (cleaner output + matches what the r1/r2
    // reader produces for unxformed prims). Anything non-identity
    // emits the opinion list + an `xformOpOrder` token array per the
    // UsdGeomXformable contract.
    write_node_transform(w, &node.transform);

    // Mesh attachment — emit an inner `def Mesh` so its prim path is
    // `<parent>/<node_name>/<mesh_name>`.
    if let Some(mesh_id) = node.mesh {
        if let Some(mesh) = scene.mesh(mesh_id) {
            write_mesh(w, scene, mesh, mesh_id, &path);
        }
    }

    // Audio emitter attachment — emit a child `def SpatialAudio`
    // per USD's `UsdMediaSpatialAudio` schema. The decoder side
    // produces a sibling node carrying the emitter; on the writer
    // side we nest it inside the parent Xform so the prim path
    // matches the typical USDZ authoring tool's output.
    if let Some(emitter_id) = node.audio_emitter {
        if let Some(emitter) = scene.audio_emitter(emitter_id) {
            if let Some(source) = scene.audio_source(emitter.source) {
                write_spatial_audio(w, emitter, source);
            }
        }
    }

    // Children.
    for &child in &node.children {
        write_node(w, scene, child, &path);
    }

    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Serialise the per-node transform as a list of `xformOp:*`
/// opinions plus a matching `xformOpOrder` token array, per the
/// UsdGeomXformable schema contract:
///
/// * [`Transform::Trs`] → three opinions
///   (`xformOp:translate`, `xformOp:orient`, `xformOp:scale`) with
///   `xformOpOrder = ["xformOp:translate", "xformOp:orient", "xformOp:scale"]`.
///   The orient quaternion is written as `(w, x, y, z)` to match
///   USD's `quatf` literal layout (USD stores the real component
///   first; our internal `[x,y,z,w]` follows glTF's xyzw layout).
/// * [`Transform::Matrix`] → a single `xformOp:transform` opinion
///   carrying the 4x4 row-major matrix (USD's `matrix4d` literal).
/// * Identity TRS — no opinions emitted (the reader treats an
///   absent xformOpOrder as identity).
fn write_node_transform(w: &mut Out, t: &Transform) {
    if is_identity(t) {
        return;
    }
    match *t {
        Transform::Trs {
            translation,
            rotation,
            scale,
        } => {
            w.write_indent();
            writeln!(
                w.s,
                "double3 xformOp:translate = ({}, {}, {})",
                format_float(translation[0] as f64),
                format_float(translation[1] as f64),
                format_float(translation[2] as f64)
            )
            .unwrap();
            // Quaternion order: USD's `quatf` literal is
            // `(w, x, y, z)`; our `Transform::Trs::rotation` is
            // glTF/xyzw.
            w.write_indent();
            writeln!(
                w.s,
                "quatf xformOp:orient = ({}, {}, {}, {})",
                format_float(rotation[3] as f64),
                format_float(rotation[0] as f64),
                format_float(rotation[1] as f64),
                format_float(rotation[2] as f64)
            )
            .unwrap();
            w.write_indent();
            writeln!(
                w.s,
                "float3 xformOp:scale = ({}, {}, {})",
                format_float(scale[0] as f64),
                format_float(scale[1] as f64),
                format_float(scale[2] as f64)
            )
            .unwrap();
            w.write_indent();
            writeln!(
                w.s,
                "uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:orient\", \"xformOp:scale\"]"
            )
            .unwrap();
        }
        Transform::Matrix(m) => {
            w.write_indent();
            write!(w.s, "matrix4d xformOp:transform = (").unwrap();
            for (i, row) in m.iter().enumerate() {
                if i > 0 {
                    w.s.push_str(", ");
                }
                write!(
                    w.s,
                    "({}, {}, {}, {})",
                    format_float(row[0] as f64),
                    format_float(row[1] as f64),
                    format_float(row[2] as f64),
                    format_float(row[3] as f64)
                )
                .unwrap();
            }
            writeln!(w.s, ")").unwrap();
            w.write_indent();
            writeln!(
                w.s,
                "uniform token[] xformOpOrder = [\"xformOp:transform\"]"
            )
            .unwrap();
        }
    }
}

/// `true` iff the transform is the identity TRS produced by
/// [`Transform::identity`]. We compare exact bit patterns rather
/// than fuzzy: the writer's job is to mirror the in-memory
/// `Scene3D` and the reader-side identity comes through as exact
/// `Transform::identity()` already.
fn is_identity(t: &Transform) -> bool {
    match *t {
        Transform::Trs {
            translation,
            rotation,
            scale,
        } => {
            translation == [0.0, 0.0, 0.0]
                && rotation == [0.0, 0.0, 0.0, 1.0]
                && scale == [1.0, 1.0, 1.0]
        }
        Transform::Matrix(m) => {
            m == [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        }
    }
}

fn write_mesh(w: &mut Out, scene: &Scene3D, mesh: &Mesh, _id: MeshId, parent_path: &str) {
    let raw_name = mesh.name.clone().unwrap_or_else(|| "Mesh".to_string());
    let mesh_name = sanitize_prim_name(&raw_name);
    let _ = parent_path; // future use for relative material paths
    if mesh.primitives.is_empty() {
        return;
    }
    // Multi-primitive meshes (one Primitive per material in the
    // typical authoring tool output) become N sibling Mesh prims
    // under the parent Xform — UsdGeomMesh holds a single vertex
    // buffer, so we cannot fold the primitives onto a single
    // Mesh prim. The reader rule documented in `usd_to_scene.rs`
    // folds these siblings back into a single Scene3D Mesh
    // when the prim names match the `<stem>` / `<stem>_<N>`
    // convention emitted here, preserving the typed model.
    for (i, prim) in mesh.primitives.iter().enumerate() {
        if !matches!(prim.topology, Topology::Triangles) {
            // Strips / fans / points / lines need conversion into
            // triangles first; skip rather than emit a broken
            // mesh prim. Round-4 followup tracked in r3 handoff.
            continue;
        }
        let prim_name = if i == 0 {
            mesh_name.clone()
        } else {
            format!("{mesh_name}_{i}")
        };
        write_one_mesh_prim(w, scene, prim, &prim_name);
    }
}

/// Emit a single `def Mesh "<name>" { ... }` block carrying one
/// USD `UsdGeomMesh`. Wraps [`write_triangle_mesh`] with the
/// prim-frame braces + the optional `material:binding`
/// relationship.
fn write_one_mesh_prim(w: &mut Out, scene: &Scene3D, prim: &Primitive, prim_name: &str) {
    w.write_indent();
    writeln!(w.s, "def Mesh \"{prim_name}\" {{").unwrap();
    w.indent += 1;
    write_triangle_mesh(w, prim);
    if let Some(mat_id) = prim.material {
        if let Some(mat) = scene.materials.get(mat_id.0 as usize) {
            let mat_name = material_prim_name(mat, mat_id.0 as usize);
            w.write_indent();
            writeln!(w.s, "rel material:binding = </Materials/{mat_name}>").unwrap();
        }
    }
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Emit a `def SpatialAudio "<name>" { ... }` block carrying one
/// USD `UsdMediaSpatialAudio` prim for `emitter` referencing
/// `source`.
///
/// Output covers the schema fields the round-4 reader picks up
/// (`filePath`, `auralMode`, `gain`, `startTime`, `endTime`,
/// `mediaOffset`, `fillBufferTime`). The `auralMode` token comes
/// from `emitter.extras["usd:auralMode"]` when present (so the
/// exact spelling round-trips), else falls back to the USD default
/// `"spatial"`. Asset-source paths come from `audio_filename`
/// when the source is in-archive
/// ([`AudioData::Source`](oxideav_mesh3d::AudioData::Source)) or the
/// raw URI when it's external
/// ([`AudioData::External`](oxideav_mesh3d::AudioData::External)).
fn write_spatial_audio(w: &mut Out, emitter: &AudioEmitter, source: &AudioSource) {
    let raw_name = emitter
        .name
        .clone()
        .or_else(|| source.name.clone())
        .unwrap_or_else(|| format!("audio_{}", emitter.source.0));
    let safe_name = sanitize_prim_name(&raw_name);

    w.write_indent();
    writeln!(w.s, "def SpatialAudio \"{safe_name}\" {{").unwrap();
    w.indent += 1;

    // filePath — in-archive sources use the per-source filename
    // (matches the entry the encoder writes via
    // `collect_audio_assets`); external URIs pass through verbatim.
    let file_ref = match &source.data {
        AudioData::Source(_) => audio_filename(source, emitter.source.0 as usize),
        AudioData::External { uri, .. } => uri.clone(),
        #[cfg(feature = "registry")]
        AudioData::Embedded(_) => audio_filename(source, emitter.source.0 as usize),
    };
    w.write_indent();
    writeln!(w.s, "uniform asset filePath = @{file_ref}@").unwrap();

    // auralMode — prefer the round-trip token from extras when
    // present (preserves the input file's exact spelling); fall
    // back to mapping the typed `aural_mode` enum.
    let aural_token = emitter
        .extras
        .get("usd:auralMode")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            let mode = emitter
                .spatial
                .as_ref()
                .map(|s| s.aural_mode)
                .unwrap_or(AuralMode::SpatialNonAcoustic);
            aural_mode_to_token(mode).to_string()
        });
    w.write_indent();
    writeln!(w.s, "uniform token auralMode = \"{aural_token}\"").unwrap();

    // gain — USD's schema default is 1.0 (and that's also our typed
    // default), but we emit the value unconditionally so the
    // round-trip is value-faithful even when a downstream tool
    // tweaks it.
    w.write_indent();
    writeln!(
        w.s,
        "uniform double gain = {}",
        format_float(emitter.gain as f64)
    )
    .unwrap();

    // Per-source extras land back as their original USD attributes.
    for (extra_key, attr_key) in [
        ("usd:startTime", "startTime"),
        ("usd:endTime", "endTime"),
        ("usd:mediaOffset", "mediaOffset"),
    ] {
        if let Some(v) = source.extras.get(extra_key) {
            if let Some(num) = v.as_f64() {
                w.write_indent();
                writeln!(w.s, "uniform double {attr_key} = {}", format_float(num)).unwrap();
            }
        }
    }

    if let Some(v) = emitter.extras.get("usd:fillBufferTime") {
        if let Some(num) = v.as_f64() {
            w.write_indent();
            writeln!(w.s, "uniform double fillBufferTime = {}", format_float(num)).unwrap();
        }
    }

    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Inverse of `aural_mode_from_token` in `usd_to_scene` — used as a
/// fallback when the round-trip token isn't preserved in
/// `emitter.extras`.
fn aural_mode_to_token(m: AuralMode) -> &'static str {
    match m {
        AuralMode::SpatialNonAcoustic => "spatial",
        AuralMode::SpatialAcoustic => "nonSpatial",
    }
}

fn write_triangle_mesh(w: &mut Out, prim: &Primitive) {
    let n_tris = match &prim.indices {
        Some(Indices::U16(v)) => v.len() / 3,
        Some(Indices::U32(v)) => v.len() / 3,
        None => prim.positions.len() / 3,
    };

    // faceVertexCounts: one `3` per triangle.
    w.write_indent();
    write!(w.s, "int[] faceVertexCounts = [").unwrap();
    for i in 0..n_tris {
        if i > 0 {
            w.s.push_str(", ");
        }
        w.s.push('3');
    }
    writeln!(w.s, "]").unwrap();

    // faceVertexIndices.
    w.write_indent();
    write!(w.s, "int[] faceVertexIndices = [").unwrap();
    let push_idx = |w: &mut Out, i: usize, val: u32| {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(w.s, "{val}").unwrap();
    };
    match &prim.indices {
        Some(Indices::U16(v)) => {
            for (i, &x) in v.iter().enumerate() {
                push_idx(w, i, x as u32);
            }
        }
        Some(Indices::U32(v)) => {
            for (i, &x) in v.iter().enumerate() {
                push_idx(w, i, x);
            }
        }
        None => {
            for i in 0..(n_tris * 3) {
                push_idx(w, i, i as u32);
            }
        }
    }
    writeln!(w.s, "]").unwrap();

    // points.
    w.write_indent();
    write!(w.s, "point3f[] points = [").unwrap();
    for (i, p) in prim.positions.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(
            w.s,
            "({}, {}, {})",
            format_float(p[0] as f64),
            format_float(p[1] as f64),
            format_float(p[2] as f64)
        )
        .unwrap();
    }
    writeln!(w.s, "]").unwrap();

    // Normals (optional).
    if let Some(normals) = &prim.normals {
        w.write_indent();
        write!(w.s, "normal3f[] primvars:normals = [").unwrap();
        for (i, n) in normals.iter().enumerate() {
            if i > 0 {
                w.s.push_str(", ");
            }
            write!(
                w.s,
                "({}, {}, {})",
                format_float(n[0] as f64),
                format_float(n[1] as f64),
                format_float(n[2] as f64)
            )
            .unwrap();
        }
        writeln!(w.s, "]").unwrap();
    }

    // First UV set (optional).
    if let Some(uv0) = prim.uvs.first() {
        w.write_indent();
        write!(w.s, "texCoord2f[] primvars:st = [").unwrap();
        for (i, uv) in uv0.iter().enumerate() {
            if i > 0 {
                w.s.push_str(", ");
            }
            write!(
                w.s,
                "({}, {})",
                format_float(uv[0] as f64),
                format_float(uv[1] as f64)
            )
            .unwrap();
        }
        writeln!(w.s, "]").unwrap();
    }

    // Subdivision scheme — every USDZ exporter sets this to "none"
    // because USDZ readers default to Catmull-Clark otherwise (and
    // a renderer that subdivides our triangle soup would corrupt
    // the geometry).
    w.write_indent();
    writeln!(w.s, "uniform token subdivisionScheme = \"none\"").unwrap();
}

fn write_material(w: &mut Out, scene: &Scene3D, mat: &Material, idx: usize) {
    let name = material_prim_name(mat, idx);
    let mat_path = format!("/Materials/{name}");
    w.write_indent();
    writeln!(w.s, "def Material \"{name}\" {{").unwrap();
    w.indent += 1;

    // UsdPreviewSurface shader child.
    w.write_indent();
    writeln!(w.s, "def Shader \"Surface\" {{").unwrap();
    w.indent += 1;
    w.write_indent();
    writeln!(w.s, "uniform token info:id = \"UsdPreviewSurface\"").unwrap();
    let bc = mat.base_color;
    w.write_indent();
    writeln!(
        w.s,
        "color3f inputs:diffuseColor = ({}, {}, {})",
        format_float(bc[0] as f64),
        format_float(bc[1] as f64),
        format_float(bc[2] as f64)
    )
    .unwrap();
    w.write_indent();
    writeln!(w.s, "float inputs:opacity = {}", format_float(bc[3] as f64)).unwrap();
    w.write_indent();
    writeln!(
        w.s,
        "float inputs:metallic = {}",
        format_float(mat.metallic as f64)
    )
    .unwrap();
    w.write_indent();
    writeln!(
        w.s,
        "float inputs:roughness = {}",
        format_float(mat.roughness as f64)
    )
    .unwrap();
    let ec = mat.emissive_factor;
    if ec != [0.0, 0.0, 0.0] {
        w.write_indent();
        writeln!(
            w.s,
            "color3f inputs:emissiveColor = ({}, {}, {})",
            format_float(ec[0] as f64),
            format_float(ec[1] as f64),
            format_float(ec[2] as f64)
        )
        .unwrap();
    }
    if let Some(tref) = mat.base_color_texture {
        write_tex_connect(w, &mat_path, "diffuseColor", tref, "rgb");
    }
    if let Some(tref) = mat.normal_texture {
        write_tex_connect(w, &mat_path, "normal", tref, "rgb");
    }
    if let Some(tref) = mat.emissive_texture {
        write_tex_connect(w, &mat_path, "emissiveColor", tref, "rgb");
    }
    if let Some(tref) = mat.occlusion_texture {
        write_tex_connect(w, &mat_path, "occlusion", tref, "r");
    }
    w.write_indent();
    writeln!(w.s, "token outputs:surface").unwrap();
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();

    // One UsdUVTexture child per bound texture (deduped on TextureId).
    let mut emitted = std::collections::BTreeSet::new();
    for tref in [
        mat.base_color_texture,
        mat.normal_texture,
        mat.emissive_texture,
        mat.occlusion_texture,
    ]
    .into_iter()
    .flatten()
    {
        if !emitted.insert(tref.texture.0) {
            continue;
        }
        let Some(tex) = scene.textures.get(tref.texture.0 as usize) else {
            continue;
        };
        let shader_name = texture_shader_name(tref.texture.0);
        let asset_name = texture_filename(tex, tref.texture.0 as usize);
        w.write_indent();
        writeln!(w.s, "def Shader \"{shader_name}\" {{").unwrap();
        w.indent += 1;
        w.write_indent();
        writeln!(w.s, "uniform token info:id = \"UsdUVTexture\"").unwrap();
        w.write_indent();
        writeln!(w.s, "asset inputs:file = @{asset_name}@").unwrap();
        w.write_indent();
        writeln!(w.s, "float3 outputs:rgb").unwrap();
        w.write_indent();
        writeln!(w.s, "float outputs:r").unwrap();
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, "}}").unwrap();
    }

    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Emit a `inputs:<slot>.connect = </path>` line that points at a
/// `UsdUVTexture` shader sibling under `mat_path`. The reader walks
/// the prim path, strips the `mat_path/` prefix, and looks up the
/// shader by remaining-relative-name.
fn write_tex_connect(w: &mut Out, mat_path: &str, slot: &str, tref: TextureRef, channel: &str) {
    let shader_name = texture_shader_name(tref.texture.0);
    let prefix = type_for_slot(slot);
    w.write_indent();
    writeln!(
        w.s,
        "{prefix} inputs:{slot}.connect = <{mat_path}/{shader_name}.outputs:{channel}>"
    )
    .unwrap();
}

/// Map an `inputs:foo` slot name to the USDA element type token the
/// connection statement needs.
fn type_for_slot(slot: &str) -> &'static str {
    match slot {
        "diffuseColor" | "emissiveColor" => "color3f",
        "normal" => "normal3f",
        "occlusion" => "float",
        _ => "color3f",
    }
}

fn material_prim_name(mat: &Material, idx: usize) -> String {
    if let Some(name) = mat.name.as_deref() {
        sanitize_prim_name(name)
    } else {
        format!("Material_{idx}")
    }
}

fn texture_shader_name(idx: u32) -> String {
    format!("Texture_{idx}")
}

fn sanitize_prim_name(s: &str) -> String {
    // USD prim names must match `[A-Za-z_][A-Za-z0-9_]*`; sanitise
    // anything else to `_` and prepend `_` if the leading char is a
    // digit.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return "Unnamed".into();
    }
    if out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

fn format_float(f: f64) -> String {
    // USD canonical: keep at most 6 fractional digits, strip
    // trailing zeros so `1.0` round-trips as `1` (USD parses both
    // identically via our own `read_number`).
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{}", f as i64);
    }
    let s = format!("{f:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_minimal_layer() {
        let scene = Scene3D::new();
        let text = write_layer(&scene);
        assert!(text.starts_with("#usda 1.0\n"));
        assert!(text.contains("upAxis = \"Y\""));
        assert!(text.contains("metersPerUnit = 1"));
    }

    #[test]
    fn float_format_drops_trailing_zeros() {
        assert_eq!(format_float(1.0), "1");
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(1.25), "1.25");
    }

    #[test]
    fn sanitize_name_prepends_underscore_for_digits() {
        assert_eq!(sanitize_prim_name("3d"), "_3d");
        assert_eq!(sanitize_prim_name("foo bar"), "foo_bar");
        assert_eq!(sanitize_prim_name(""), "Unnamed");
    }

    #[test]
    fn write_node_transform_skips_identity() {
        let mut w = Out::default();
        write_node_transform(&mut w, &Transform::identity());
        assert!(
            w.s.is_empty(),
            "identity should write nothing, got `{}`",
            w.s
        );
    }

    #[test]
    fn write_node_transform_trs_emits_three_ops_plus_order() {
        let mut w = Out::default();
        write_node_transform(
            &mut w,
            &Transform::Trs {
                translation: [1.0, 2.0, 3.0],
                // identity quaternion (xyzw)
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
            },
        );
        assert!(w.s.contains("xformOp:translate = (1, 2, 3)"));
        // USD orient is wxyz — identity quat serialises as
        // (1, 0, 0, 0).
        assert!(w.s.contains("xformOp:orient = (1, 0, 0, 0)"));
        assert!(w.s.contains("xformOp:scale = (1, 1, 1)"));
        assert!(w.s.contains("xformOpOrder = ["));
    }

    #[test]
    fn aural_mode_token_mapping() {
        assert_eq!(
            aural_mode_to_token(AuralMode::SpatialNonAcoustic),
            "spatial"
        );
        assert_eq!(
            aural_mode_to_token(AuralMode::SpatialAcoustic),
            "nonSpatial"
        );
    }

    #[test]
    fn audio_filename_uses_source_name_and_mime() {
        use oxideav_mesh3d::AudioSource;
        let mut s = AudioSource::from_uri("ignored").with_name("Bg Music");
        // External-URI source with no MIME → defaults to .wav.
        if let AudioData::External { mime, .. } = &mut s.data {
            *mime = Some("audio/mpeg".into());
        }
        let name = audio_filename(&s, 7);
        assert_eq!(name, "Bg_Music.mp3");
    }

    #[test]
    fn write_node_transform_matrix_emits_4x4_then_order() {
        let mut w = Out::default();
        let m = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [10.0, 20.0, 30.0, 1.0],
        ];
        write_node_transform(&mut w, &Transform::Matrix(m));
        assert!(w.s.contains("matrix4d xformOp:transform = ("));
        assert!(w.s.contains("(10, 20, 30, 1)"));
        assert!(w.s.contains("xformOpOrder = [\"xformOp:transform\"]"));
    }
}
