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

use std::collections::BTreeMap;
use std::fmt::Write;

use oxideav_mesh3d::{
    AudioData, AudioEmitter, AudioSource, AuralMode, Axis, ImageData, Indices, Material, Mesh,
    MeshId, NodeId, Primitive, Scene3D, Texture, TextureRef, Topology, Transform, Unit,
};

use crate::usda::Value;

/// `Scene3D::extras` key holding the lossless layer-metadata blob
/// produced by [`crate::usd_to_scene::translate`].  The blob is a
/// JSON object whose values use the tagged shape from
/// [`crate::variant_codec`]; on the writer side we decode it back into
/// a [`BTreeMap<String, Value>`](Value) and emit each entry inside the
/// USDA `( ... )` layer-metadata block alongside `upAxis` /
/// `metersPerUnit`.
///
/// Added round 9 — round 1..8 wrote a per-key untagged entry
/// (`usd:<key>` → string-shaped JSON) which loses the
/// `Token` / `Asset` / `Path` distinction. The untagged entries stay
/// for direct-JSON consumers; this blob is the round-trip channel.
pub const LAYER_METADATA_EXTRAS_KEY: &str = "usd:layerMetadata";

/// `Node::extras` key holding the lossless prim-metadata blob
/// (counterpart to [`LAYER_METADATA_EXTRAS_KEY`] for per-prim
/// `( ... )` metadata).  Same tagged shape from
/// [`crate::variant_codec`].
///
/// Added round 9 — round 1..8 stashed the prim metadata under
/// `usd:metadata` as an untagged JSON object.  That entry stays for
/// callers reading it directly; this one is what the writer round-
/// trips through so composition-arc opinions (`references`,
/// `payload`, `inherits`, `specializes`, `kind`, `apiSchemas`, ...)
/// survive USDZ → `Scene3D` → USDZ.
pub const PRIM_METADATA_EXTRAS_KEY: &str = "usd:primMetadata";

/// Metadata keys we always emit with the `prepend` list-edit
/// operator.  These are USD composition arcs — every authoring tool
/// (see the various authoring tools that emit USD) writes
/// them with `prepend` because that's the standard LIVRPS-strength
/// authoring intent: the new opinion sits at the front of the list,
/// strongest. We follow the same convention so a USDZ round-trip
/// produces the same shape one would author by hand.
const PREPEND_LIST_EDIT_KEYS: &[&str] = &[
    "references",
    "payload",
    "inherits",
    "specializes",
    "apiSchemas",
    "variantSets",
];

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
    // Round 9: re-emit `defaultPrim`, `subLayers`, `customLayerData`,
    // and any other layer-metadata key picked up by the decoder.  The
    // canonical [`USD glossary`][1] LIVRPS section + the `subLayers` /
    // `references` examples mandate these keys survive a round-trip
    // for cross-layer composition to work; r1..r8 dropped them.
    //
    // [1]: docs/3d/usd/glossary.html § Composition Arcs / Sub-layers
    let layer_meta = scene
        .extras
        .get(LAYER_METADATA_EXTRAS_KEY)
        .map(crate::variant_codec::decode_btree_value)
        .unwrap_or_default();
    // Round 12: keep the emitted `defaultPrim` token consistent with
    // the actual prim names we write. Three situations to handle:
    //
    //   * The preserved token names a root that exists verbatim under
    //     its sanitised name — emit unchanged.
    //   * The preserved token names a root whose name was sanitised
    //     (`"My Cube"` → `def Xform "My_Cube"`) — rewrite to the
    //     sanitised spelling so cross-archive `references = @./scene.usda@`
    //     (selector-less, i.e. resolve-to-defaultPrim) still finds the
    //     target.
    //   * The preserved token names nothing in the scene (e.g. the
    //     `defaultPrim` root was removed downstream) — drop the
    //     opinion entirely. Emitting a dangling `defaultPrim` makes
    //     a strict USD validator reject the layer.
    //
    // When no `defaultPrim` is authored at all but the scene has at
    // least one root, synthesise one from the first root's sanitised
    // name. Without this, every selector-less `@./scene.usda@`
    // reference downstream of us is silently dropped.
    let root_names = root_prim_names(scene);
    let resolved_default_prim = layer_meta
        .get("defaultPrim")
        .and_then(|v| v.as_text())
        .and_then(|token| resolve_default_prim_token(token, &root_names));
    for (k, v) in &layer_meta {
        // Don't double-emit the canonical keys we already wrote.
        if matches!(k.as_str(), "upAxis" | "metersPerUnit") {
            continue;
        }
        if k == "defaultPrim" {
            // Either rewrite the token to track sanitisation or skip
            // the dangling opinion outright; we'll re-emit a fresh
            // line below when a resolution survived.
            continue;
        }
        for formatted in format_metadata_lines(k, v) {
            writeln!(w.s, "    {formatted}").unwrap();
        }
    }
    let default_prim = resolved_default_prim.or_else(|| root_names.first().cloned());
    if let Some(name) = default_prim {
        writeln!(w.s, "    defaultPrim = \"{name}\"").unwrap();
    }
    writeln!(w.s, ")").unwrap();
}

/// Collect the sanitised prim names the writer will emit for every
/// root in `scene`. Mirrors the naming choices [`write_node`] makes
/// (anonymous nodes fall back to `node_<id>`) so the result is
/// authoritative for `defaultPrim` resolution.
fn root_prim_names(scene: &Scene3D) -> Vec<String> {
    scene
        .roots
        .iter()
        .filter_map(|id| {
            let node = scene.node(*id)?;
            let raw = node
                .name
                .clone()
                .unwrap_or_else(|| format!("node_{}", id.0));
            Some(sanitize_prim_name(&raw))
        })
        .collect()
}

/// Match a preserved `defaultPrim` token against the writer's actual
/// emitted root names. Returns the spelling we should write, or
/// `None` if the token refers to nothing in the scene.
///
/// Tries the raw token first (covers tokens that were already
/// sanitisation-safe), then the sanitised spelling of the token
/// (covers tokens that named the pre-sanitisation form).
fn resolve_default_prim_token(token: &str, root_names: &[String]) -> Option<String> {
    if root_names.iter().any(|n| n == token) {
        return Some(token.to_string());
    }
    let sanitised = sanitize_prim_name(token);
    if root_names.iter().any(|n| n == &sanitised) {
        return Some(sanitised);
    }
    None
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
    // Round 8: surface the prim's variant declarations on the prim
    // metadata block — `prepend variantSets = [...]` lists the set
    // names we'll emit inside the body, and `variants = {...}` carries
    // the selection that resolved a variant during decode (so re-decoding
    // the round-tripped USDA reproduces the same Scene3D).
    let variant_sets = node
        .extras
        .get(crate::variant_codec::EXTRAS_KEY)
        .map(crate::variant_codec::decode_variant_sets)
        .unwrap_or_default();
    let selection = extract_variant_selection(&node.extras);
    let composition_lines = extract_composition_arc_lines(&node.extras);
    let prim_metadata_lines =
        build_prim_metadata_lines(&variant_sets, &selection, &composition_lines);

    // We always emit `Xform` for now — meshes hang off as inner
    // `def Mesh` children rather than collapsing into the node's
    // own prim type. Round-3 work: also synthesise `Camera` /
    // `Light` prim types when the node carries those references.
    w.write_indent();
    if prim_metadata_lines.is_empty() {
        writeln!(w.s, "def Xform \"{safe_name}\" {{").unwrap();
    } else {
        writeln!(w.s, "def Xform \"{safe_name}\" (").unwrap();
        w.indent += 1;
        for line in &prim_metadata_lines {
            w.write_indent();
            writeln!(w.s, "{line}").unwrap();
        }
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, ") {{").unwrap();
    }
    w.indent += 1;

    // Emit each declared `variantSet "name" = { "variant" { ... } }`
    // block.  Keys are walked in BTreeMap order so output is
    // deterministic regardless of input ordering.
    if !variant_sets.is_empty() {
        write_variant_sets(w, &variant_sets);
    }

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

/// Produce the `( ... )` metadata-block lines for a node prim — one
/// per declared variantSet plus the optional `variants = {...}`
/// selection.  Returns an empty vec when the node carries no variant
/// information, which lets the caller skip the metadata block entirely
/// for the common case (preserving the round-1 / r2 output shape).
fn build_prim_metadata_lines(
    variant_sets: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, crate::usda::Variant>,
    >,
    selection: &std::collections::BTreeMap<String, String>,
    composition_lines: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    // Round 9: composition-arc opinions go first so the LIVRPS
    // ordering visible in the source file's metadata block is
    // preserved.  Selection `variants = {...}` follows because it's
    // strength-ordered below References in the LIVRPS recipe.
    out.extend_from_slice(composition_lines);
    if !selection.is_empty() {
        // `variants = { string SET = "VAR" ... }` — sorted on SET
        // so output is deterministic regardless of source ordering.
        let mut buf = String::from("variants = {");
        let mut first = true;
        for (set, var) in selection {
            if !first {
                buf.push(';');
            }
            buf.push_str(&format!(" string {set} = \"{var}\""));
            first = false;
        }
        buf.push_str(" }");
        out.push(buf);
    }
    if !variant_sets.is_empty() {
        // `prepend variantSets = ["a", "b", ...]` — the list-edit
        // operator follows the OpenUSD glossary's convention for
        // contributing variantSet declarations from a layer.
        let names: Vec<String> = variant_sets.keys().map(|k| format!("\"{k}\"")).collect();
        out.push(format!("prepend variantSets = [{}]", names.join(", ")));
    }
    out
}

/// Pull every non-variant prim-metadata entry back out of the node's
/// extras and re-emit each as a USDA assignment line for the prim's
/// `( ... )` metadata block.
///
/// The composition-arc keys (`references`, `payload`, `inherits`,
/// `specializes`, `apiSchemas`) are auto-prefixed with the `prepend`
/// list-edit operator per [`PREPEND_LIST_EDIT_KEYS`].
///
/// `variants` + `variantSets` are intentionally skipped here — the
/// round-8 variant writer paths handle those.
fn extract_composition_arc_lines(
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> Vec<String> {
    let Some(blob) = extras.get(PRIM_METADATA_EXTRAS_KEY) else {
        return Vec::new();
    };
    let meta = crate::variant_codec::decode_btree_value(blob);
    let mut out = Vec::new();
    for (k, v) in &meta {
        // `variants` selection + `variantSets` declaration are emitted
        // by the variant-aware paths; don't double-emit them here.
        if matches!(k.as_str(), "variants" | "variantSets") {
            continue;
        }
        out.extend(format_metadata_lines(k, v));
    }
    out
}

/// Pull the `variants = { string SET = "VAR" }` selection back out of
/// the node's `usd:metadata` extras stash.  The decoder mirrors the
/// prim's metadata into `extras["usd:metadata"]` as a JSON object
/// where each value is a JSON-shaped [`Value`](crate::usda::Value);
/// the `variants` entry surfaces as a JSON object whose values are
/// the variant names (since the parser drops the `string` type token
/// during the `Value::Dict` flattening).
fn extract_variant_selection(
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(meta) = extras.get("usd:metadata").and_then(|v| v.as_object()) else {
        return out;
    };
    let Some(variants) = meta.get("variants").and_then(|v| v.as_object()) else {
        return out;
    };
    for (set, val) in variants {
        if let Some(s) = val.as_str() {
            out.insert(set.clone(), s.to_string());
        }
    }
    out
}

/// Emit each `variantSet "name" = { "variant" ( meta ) { body } }`
/// block on the prim body.  Walks BTreeMap key-sorted order so the
/// output matches the structural form decode_variant_sets returns.
fn write_variant_sets(
    w: &mut Out,
    sets: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, crate::usda::Variant>,
    >,
) {
    for (set_name, variants) in sets {
        w.write_indent();
        writeln!(w.s, "variantSet \"{set_name}\" = {{").unwrap();
        w.indent += 1;
        for (variant_name, variant) in variants {
            write_one_variant(w, variant_name, variant);
        }
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, "}}").unwrap();
    }
}

/// Emit one `"variantName" ( meta ) { ... }` entry inside a
/// variantSet body.  The body is a synthetic prim-body — we walk the
/// variant's `metadata` (the optional `( ... )` block), `attrs`, and
/// recursive `children` with the same rules used to round-trip prim
/// bodies elsewhere.
fn write_one_variant(w: &mut Out, name: &str, variant: &crate::usda::Variant) {
    w.write_indent();
    if variant.metadata.is_empty() {
        writeln!(w.s, "\"{name}\" {{").unwrap();
    } else {
        writeln!(w.s, "\"{name}\" (").unwrap();
        w.indent += 1;
        for line in metadata_lines_from_value_map(&variant.metadata) {
            w.write_indent();
            writeln!(w.s, "{line}").unwrap();
        }
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, ") {{").unwrap();
    }
    w.indent += 1;
    write_attr_map(w, &variant.attrs);
    for child in &variant.children {
        write_prim(w, child);
    }
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Render a `BTreeMap<String, Value>` (a parsed `(...)` metadata
/// block) back into one `name = value` line per entry.  Used inside
/// per-variant `(...)` blocks; mirrors the input shape of
/// [`crate::usda::parse_metadata_block`].
fn metadata_lines_from_value_map(
    map: &std::collections::BTreeMap<String, crate::usda::Value>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in map {
        match render_value(v) {
            Some(rendered) => out.push(format!("{k} = {rendered}")),
            None => out.push(k.clone()),
        }
    }
    out
}

/// Emit `BTreeMap<String, Attr>` as `<type> <name> = <value>` lines.
fn write_attr_map(w: &mut Out, attrs: &std::collections::BTreeMap<String, crate::usda::Attr>) {
    for (name, attr) in attrs {
        w.write_indent();
        let type_token = if attr.type_token.is_empty() {
            String::new()
        } else {
            format!("{} ", attr.type_token)
        };
        match render_value(&attr.value) {
            Some(rendered) => writeln!(w.s, "{type_token}{name} = {rendered}").unwrap(),
            None => writeln!(w.s, "{type_token}{name}").unwrap(),
        }
    }
}

/// Recursively serialise a [`crate::usda::Prim`] back into USDA text.
/// Used to re-emit a variant's child prim trees so a round-trip
/// preserves nested geometry / shaders authored inside variants.
fn write_prim(w: &mut Out, prim: &crate::usda::Prim) {
    w.write_indent();
    let type_token = if prim.type_name.is_empty() {
        String::new()
    } else {
        format!(" {}", prim.type_name)
    };
    let metadata_lines = if prim.metadata.is_empty() {
        Vec::new()
    } else {
        metadata_lines_from_value_map(&prim.metadata)
    };
    if metadata_lines.is_empty() {
        writeln!(w.s, "{}{} \"{}\" {{", prim.spec, type_token, prim.name).unwrap();
    } else {
        writeln!(w.s, "{}{} \"{}\" (", prim.spec, type_token, prim.name).unwrap();
        w.indent += 1;
        for line in metadata_lines {
            w.write_indent();
            writeln!(w.s, "{line}").unwrap();
        }
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, ") {{").unwrap();
    }
    w.indent += 1;
    write_attr_map(w, &prim.attrs);
    if !prim.variant_sets.is_empty() {
        write_variant_sets(w, &prim.variant_sets);
    }
    for child in &prim.children {
        write_prim(w, child);
    }
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Render a [`crate::usda::Value`] back into the USDA literal form
/// the parser would accept.  Returns `None` for [`Value::None`] (the
/// caller emits the bare attribute name without a `=`).
fn render_value(v: &crate::usda::Value) -> Option<String> {
    use crate::usda::Value as V;
    Some(match v {
        V::Token(s) => s.clone(),
        V::String(s) => format!("\"{}\"", escape_quoted(s)),
        V::Float(f) => format_float(*f),
        V::Bool(b) => if *b { "true" } else { "false" }.into(),
        V::Tuple(seq) => format!(
            "({})",
            seq.iter()
                .map(|x| render_value(x).unwrap_or_else(|| "none".into()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        V::Array(seq) => format!(
            "[{}]",
            seq.iter()
                .map(|x| render_value(x).unwrap_or_else(|| "none".into()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        V::Asset(s) => format!("@{s}@"),
        V::Path(s) => format!("<{s}>"),
        V::Dict(map) => {
            // Synthesise a `string foo = "bar"` entry per key — Apple's
            // round-trippable spelling for unknown-typed dicts.  This
            // is good enough for the `variants = {...}` selection (the
            // only Dict shape we actively reconstruct on the writer
            // path); other Dict round-trips fall back to the same
            // shape, which the parser is happy to re-ingest.
            let mut buf = String::from("{");
            let mut first = true;
            for (k, val) in map {
                if !first {
                    buf.push(';');
                }
                let rendered = render_value(val).unwrap_or_else(|| "none".into());
                let type_hint = match val {
                    V::String(_) | V::Token(_) => "string",
                    V::Float(_) => "double",
                    V::Bool(_) => "bool",
                    _ => "string",
                };
                buf.push_str(&format!(" {type_hint} {k} = {rendered}"));
                first = false;
            }
            buf.push_str(" }");
            buf
        }
        V::AssetWithPath { asset, prim_path } => format!("@{asset}@<{prim_path}>"),
        V::Raw(s) => s.clone(),
        // A list-edited field has no single literal body; render the
        // strongest authored sublist as a fallback for callers that
        // want one value. Multi-operator emission goes through
        // `format_metadata_lines` instead.
        V::ListOp(list) => {
            let sub = list
                .additive_in_strength_order()
                .map(|(_, v)| v)
                .next()
                .or(list.deleted.as_ref())
                .or(list.reordered.as_ref())?;
            return render_value(sub);
        }
        V::None => return None,
    })
}

fn escape_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
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
    //
    // Topology dispatch (r5):
    //
    // * `Triangles` → `def Mesh` (UsdGeomMesh).
    // * `TriangleStrip` / `TriangleFan` → expanded to a triangle
    //   list in-place + emitted as `def Mesh`. The original
    //   topology token is preserved in
    //   `extras["usd:original_topology"]` so a downstream consumer
    //   can recover the source spelling.
    // * `Lines` / `LineStrip` / `LineLoop` → `def BasisCurves`
    //   (UsdGeomBasisCurves) with `type = "linear"` plus the
    //   matching `wrap` token (`nonperiodic` for Lines / LineStrip,
    //   `periodic` for LineLoop). `Lines` collapses to one curve
    //   segment per pair of indices, mirroring USD's per-curve
    //   `curveVertexCounts` schema.
    // * `Points` → `def Points` (UsdGeomPoints) carrying just the
    //   `points` array (no per-point widths in r5; downstream tools
    //   default to a sensible point size when none is authored).
    for (i, prim) in mesh.primitives.iter().enumerate() {
        let prim_name = if i == 0 {
            mesh_name.clone()
        } else {
            format!("{mesh_name}_{i}")
        };
        match prim.topology {
            Topology::Triangles => write_one_mesh_prim(w, scene, prim, &prim_name, None),
            Topology::TriangleStrip => {
                let expanded = expand_strip_to_triangle_list(prim);
                write_one_mesh_prim(w, scene, &expanded, &prim_name, Some("triangleStrip"));
            }
            Topology::TriangleFan => {
                let expanded = expand_fan_to_triangle_list(prim);
                write_one_mesh_prim(w, scene, &expanded, &prim_name, Some("triangleFan"));
            }
            Topology::Lines | Topology::LineStrip | Topology::LineLoop => {
                write_basis_curves_prim(w, scene, prim, &prim_name);
            }
            Topology::Points => write_points_prim(w, scene, prim, &prim_name),
        }
    }
}

/// Emit a single `def Mesh "<name>" { ... }` block carrying one
/// USD `UsdGeomMesh`. Wraps [`write_triangle_mesh`] with the
/// prim-frame braces + the optional `material:binding`
/// relationship.
///
/// `original_topology_hint`, when `Some`, marks the emitted prim
/// with a `(usd:original_topology = "<token>")` metadata block so
/// the source topology survives the conversion. Set by the
/// strip/fan tessellation paths in [`write_mesh`].
fn write_one_mesh_prim(
    w: &mut Out,
    scene: &Scene3D,
    prim: &Primitive,
    prim_name: &str,
    original_topology_hint: Option<&str>,
) {
    let mut metadata_lines: Vec<String> = Vec::new();
    if let Some(token) = original_topology_hint {
        metadata_lines.push(format!("usd:original_topology = \"{token}\""));
    }
    if extras_no_fold(&prim.extras) {
        metadata_lines.push("usd:no_fold = 1".to_string());
    }
    w.write_indent();
    if metadata_lines.is_empty() {
        writeln!(w.s, "def Mesh \"{prim_name}\" {{").unwrap();
    } else {
        writeln!(
            w.s,
            "def Mesh \"{prim_name}\" ({}) {{",
            metadata_lines.join(" ")
        )
        .unwrap();
    }
    w.indent += 1;
    // Per-Primitive transform on the inner def Mesh — only emit
    // when the source carries a `usd:mesh_transform` extras entry.
    // Mirrors `write_node_transform` but writes onto the Mesh prim
    // directly per the UsdGeomXformable schema (which Mesh inherits).
    if let Some(t) = transform_from_extras(&prim.extras) {
        write_node_transform(w, &t);
    }
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

/// `true` iff the primitive's extras carry the `usd:no_fold = true`
/// hint emitted by the round-5 sibling-collision flow.
fn extras_no_fold(extras: &std::collections::HashMap<String, serde_json::Value>) -> bool {
    extras
        .get("usd:no_fold")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Decode a per-primitive `usd:mesh_transform` extras entry into a
/// [`Transform`]. Two shapes are recognised:
///
/// * `{"matrix": [[a,b,c,d], [e,f,g,h], [i,j,k,l], [m,n,o,p]]}`
///   → [`Transform::Matrix`]. Row-major (USD's `matrix4d` literal
///   layout).
/// * `{"trs": {"translation": [x,y,z], "rotation": [x,y,z,w],
///   "scale": [sx,sy,sz]}}` → [`Transform::Trs`]. Quaternion order
///   matches our internal xyzw.
///
/// Returns `None` for anything else (including malformed JSON or
/// the extras key being absent), causing the writer to skip the
/// inner xformOp emission entirely.
fn transform_from_extras(
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<Transform> {
    let v = extras.get("usd:mesh_transform")?;
    if let Some(rows) = v.get("matrix").and_then(|m| m.as_array()) {
        if rows.len() != 4 {
            return None;
        }
        let mut m = [[0f32; 4]; 4];
        for (i, row) in rows.iter().enumerate() {
            let r = row.as_array()?;
            if r.len() != 4 {
                return None;
            }
            for (j, c) in r.iter().enumerate() {
                m[i][j] = c.as_f64()? as f32;
            }
        }
        return Some(Transform::Matrix(m));
    }
    if let Some(trs) = v.get("trs") {
        let t = json_array_n::<3>(trs.get("translation"))?;
        let r = json_array_n::<4>(trs.get("rotation"))?;
        let s = json_array_n::<3>(trs.get("scale"))?;
        return Some(Transform::Trs {
            translation: t,
            rotation: r,
            scale: s,
        });
    }
    None
}

fn json_array_n<const N: usize>(v: Option<&serde_json::Value>) -> Option<[f32; N]> {
    let arr = v?.as_array()?;
    if arr.len() != N {
        return None;
    }
    let mut out = [0f32; N];
    for (i, c) in arr.iter().enumerate() {
        out[i] = c.as_f64()? as f32;
    }
    Some(out)
}

/// Expand a `TriangleStrip` primitive into a triangle-list
/// primitive, preserving alternating winding per OpenGL/glTF
/// semantics (`(i, i+1, i+2)` for even `i`, `(i+1, i, i+2)` for
/// odd). The returned [`Primitive`] is a fresh
/// [`Topology::Triangles`] instance whose `positions` (and any
/// per-vertex attribute) is unchanged; only `indices` is rebuilt.
/// When the source has no index buffer we fabricate a `0..N`
/// running sequence first so the strip→list rewrite has something
/// to rewire.
fn expand_strip_to_triangle_list(prim: &Primitive) -> Primitive {
    let n = prim
        .indices
        .as_ref()
        .map(|i| i.len())
        .unwrap_or(prim.positions.len());
    let src: Vec<u32> = match &prim.indices {
        Some(Indices::U16(v)) => v.iter().map(|&x| x as u32).collect(),
        Some(Indices::U32(v)) => v.clone(),
        None => (0..n as u32).collect(),
    };
    let mut out = Vec::with_capacity(n.saturating_sub(2) * 3);
    for i in 0..src.len().saturating_sub(2) {
        let (a, b, c) = if i % 2 == 0 {
            (src[i], src[i + 1], src[i + 2])
        } else {
            (src[i + 1], src[i], src[i + 2])
        };
        out.push(a);
        out.push(b);
        out.push(c);
    }
    let mut new_prim = clone_prim_metadata(prim);
    new_prim.topology = Topology::Triangles;
    new_prim.positions = prim.positions.clone();
    new_prim.normals = prim.normals.clone();
    new_prim.tangents = prim.tangents.clone();
    new_prim.uvs = prim.uvs.clone();
    new_prim.colors = prim.colors.clone();
    new_prim.joints = prim.joints.clone();
    new_prim.weights = prim.weights.clone();
    new_prim.material = prim.material;
    new_prim.indices = Some(if prim.positions.len() <= u16::MAX as usize {
        Indices::U16(out.iter().map(|&i| i as u16).collect())
    } else {
        Indices::U32(out)
    });
    new_prim
}

/// Expand a `TriangleFan` primitive into a triangle-list primitive.
/// Each interior triangle is `(0, i, i+1)` per the GL fan winding
/// convention.
fn expand_fan_to_triangle_list(prim: &Primitive) -> Primitive {
    let n = prim
        .indices
        .as_ref()
        .map(|i| i.len())
        .unwrap_or(prim.positions.len());
    let src: Vec<u32> = match &prim.indices {
        Some(Indices::U16(v)) => v.iter().map(|&x| x as u32).collect(),
        Some(Indices::U32(v)) => v.clone(),
        None => (0..n as u32).collect(),
    };
    let mut out = Vec::with_capacity(n.saturating_sub(2) * 3);
    if src.len() >= 3 {
        let v0 = src[0];
        for i in 1..(src.len() - 1) {
            out.push(v0);
            out.push(src[i]);
            out.push(src[i + 1]);
        }
    }
    let mut new_prim = clone_prim_metadata(prim);
    new_prim.topology = Topology::Triangles;
    new_prim.positions = prim.positions.clone();
    new_prim.normals = prim.normals.clone();
    new_prim.tangents = prim.tangents.clone();
    new_prim.uvs = prim.uvs.clone();
    new_prim.colors = prim.colors.clone();
    new_prim.joints = prim.joints.clone();
    new_prim.weights = prim.weights.clone();
    new_prim.material = prim.material;
    new_prim.indices = Some(if prim.positions.len() <= u16::MAX as usize {
        Indices::U16(out.iter().map(|&i| i as u16).collect())
    } else {
        Indices::U32(out)
    });
    new_prim
}

/// Helper: clone the per-primitive metadata (extras + material)
/// without copying the bulky vertex buffers — the strip/fan paths
/// do their own buffer copies.
fn clone_prim_metadata(prim: &Primitive) -> Primitive {
    let mut p = Primitive::new(Topology::Triangles);
    p.extras = prim.extras.clone();
    p.material = prim.material;
    p
}

/// Emit a `def BasisCurves "<name>" { ... }` block carrying one
/// USD `UsdGeomBasisCurves` for a Lines / LineStrip / LineLoop
/// primitive.
///
/// Schema basics (clean-room, drawn from the public UsdGeom prim
/// type contract):
///
/// * `type = "linear"` — every variant we emit is straight-segment.
/// * `wrap = "nonperiodic"` for Lines / LineStrip; `wrap = "periodic"`
///   for LineLoop (the closing segment is implicit per UsdGeom's
///   periodic-curve rule).
/// * `curveVertexCounts` — for Lines: one `2` per index pair (each
///   pair is its own straight curve); for LineStrip / LineLoop: a
///   single count equal to the index/vertex count.
/// * `points` — the position array (one entry per index when
///   `indices` is present, else just the source positions).
/// * `material:binding` — same per-primitive binding shape used by
///   `write_one_mesh_prim`.
fn write_basis_curves_prim(w: &mut Out, scene: &Scene3D, prim: &Primitive, prim_name: &str) {
    let mut metadata_lines: Vec<String> = Vec::new();
    let original = match prim.topology {
        Topology::Lines => "lines",
        Topology::LineStrip => "lineStrip",
        Topology::LineLoop => "lineLoop",
        _ => "lines",
    };
    metadata_lines.push(format!("usd:original_topology = \"{original}\""));
    if extras_no_fold(&prim.extras) {
        metadata_lines.push("usd:no_fold = 1".to_string());
    }
    w.write_indent();
    writeln!(
        w.s,
        "def BasisCurves \"{prim_name}\" ({}) {{",
        metadata_lines.join(" ")
    )
    .unwrap();
    w.indent += 1;

    // Optional per-primitive transform.
    if let Some(t) = transform_from_extras(&prim.extras) {
        write_node_transform(w, &t);
    }

    // Materialise the index list (synthesise 0..N when absent so the
    // emitted positions array matches `curveVertexCounts`).
    let n = prim
        .indices
        .as_ref()
        .map(|i| i.len())
        .unwrap_or(prim.positions.len());
    let idx: Vec<u32> = match &prim.indices {
        Some(Indices::U16(v)) => v.iter().map(|&x| x as u32).collect(),
        Some(Indices::U32(v)) => v.clone(),
        None => (0..n as u32).collect(),
    };

    // curveVertexCounts.
    w.write_indent();
    write!(w.s, "int[] curveVertexCounts = [").unwrap();
    match prim.topology {
        Topology::Lines => {
            // One straight curve per index pair.
            let segments = idx.len() / 2;
            for i in 0..segments {
                if i > 0 {
                    w.s.push_str(", ");
                }
                w.s.push('2');
            }
        }
        Topology::LineStrip | Topology::LineLoop => {
            write!(w.s, "{}", idx.len()).unwrap();
        }
        _ => unreachable!("write_basis_curves_prim only handles line topologies"),
    }
    writeln!(w.s, "]").unwrap();

    // points — emit the actual coordinates referenced by `idx`.
    w.write_indent();
    write!(w.s, "point3f[] points = [").unwrap();
    for (i, &v) in idx.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        let p = prim
            .positions
            .get(v as usize)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
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

    // type + wrap.
    w.write_indent();
    writeln!(w.s, "uniform token type = \"linear\"").unwrap();
    w.write_indent();
    let wrap = if matches!(prim.topology, Topology::LineLoop) {
        "periodic"
    } else {
        "nonperiodic"
    };
    writeln!(w.s, "uniform token wrap = \"{wrap}\"").unwrap();

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

/// Emit a `def Points "<name>" { ... }` block carrying one USD
/// `UsdGeomPoints` prim.
///
/// One point per source vertex (or per index entry when an index
/// buffer is present). No per-point `widths` are authored in r5;
/// downstream renderers fall back to a sensible default.
fn write_points_prim(w: &mut Out, scene: &Scene3D, prim: &Primitive, prim_name: &str) {
    let mut metadata_lines = vec!["usd:original_topology = \"points\"".to_string()];
    if extras_no_fold(&prim.extras) {
        metadata_lines.push("usd:no_fold = 1".to_string());
    }
    w.write_indent();
    writeln!(
        w.s,
        "def Points \"{prim_name}\" ({}) {{",
        metadata_lines.join(" ")
    )
    .unwrap();
    w.indent += 1;

    if let Some(t) = transform_from_extras(&prim.extras) {
        write_node_transform(w, &t);
    }

    let idx: Vec<u32> = match &prim.indices {
        Some(Indices::U16(v)) => v.iter().map(|&x| x as u32).collect(),
        Some(Indices::U32(v)) => v.clone(),
        None => (0..prim.positions.len() as u32).collect(),
    };
    w.write_indent();
    write!(w.s, "point3f[] points = [").unwrap();
    for (i, &v) in idx.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        let p = prim
            .positions
            .get(v as usize)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
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

/// Serialise `value` as a USDA right-hand-side literal.  Inverse of
/// [`crate::usda::parse_value`].
///
/// Covers the [`Value`] variants the decoder can produce from layer
/// metadata + prim metadata.  Used by the round-9 layer-metadata /
/// prim-metadata round-trip paths.
///
/// Float arrays inside [`Value::Array`] / [`Value::Tuple`] use the
/// same compact `format_float` rules as the rest of the writer for
/// determinism.
fn format_metadata_value(value: &Value) -> String {
    match value {
        Value::Token(s) => format!("\"{s}\""),
        Value::String(s) => format!("\"{}\"", escape_usda_string(s)),
        Value::Asset(s) => format!("@{s}@"),
        Value::Path(s) => format!("<{s}>"),
        Value::Float(f) => format_float(*f),
        Value::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Value::Tuple(seq) => {
            let mut s = String::from("(");
            for (i, v) in seq.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format_metadata_value(v));
            }
            s.push(')');
            s
        }
        Value::Array(seq) => {
            let mut s = String::from("[");
            for (i, v) in seq.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format_metadata_value(v));
            }
            s.push(']');
            s
        }
        Value::AssetWithPath { asset, prim_path } => format!("@{asset}@<{prim_path}>"),
        Value::Dict(map) => format_metadata_dict(map),
        // Single-body fallback (multi-operator emission lives in
        // `format_metadata_lines`): render the strongest authored
        // sublist body.
        Value::ListOp(list) => list
            .entries()
            .next()
            .map(|(_, v)| format_metadata_value(v))
            .unwrap_or_default(),
        Value::Raw(s) => s.clone(),
        Value::None => String::new(),
    }
}

/// Serialise a typed-dictionary value (`Value::Dict`) as a USDA
/// `{ TYPE NAME = VALUE; ... }` block.  Round 1 dropped the type
/// token during parsing, so round-trip output uses a heuristic to
/// pick a plausible USDA type per value variant — `string` for
/// quoted strings, `token` for tokens, `dictionary` for nested dicts,
/// `bool` for bools, `double` for floats.  This is good enough for
/// the customLayerData blob that Apple's `usdzconvert` emits;
/// reconstructing the exact original type tokens would require
/// preserving them in the parser, which round 1 declined to do.
fn format_metadata_dict(map: &BTreeMap<String, Value>) -> String {
    if map.is_empty() {
        return "{ }".into();
    }
    let mut s = String::from("{ ");
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            s.push_str("; ");
        }
        let ty = guess_usda_type(v);
        let body = format_metadata_value(v);
        if matches!(v, Value::None) {
            s.push_str(&format!("{ty} {k}"));
        } else {
            s.push_str(&format!("{ty} {k} = {body}"));
        }
    }
    s.push_str(" }");
    s
}

fn guess_usda_type(v: &Value) -> &'static str {
    match v {
        Value::Token(_) => "token",
        Value::String(_) => "string",
        Value::Asset(_) | Value::AssetWithPath { .. } => "asset",
        Value::Path(_) => "rel",
        Value::Float(_) => "double",
        Value::Bool(_) => "bool",
        Value::Tuple(_) => "double3",
        Value::Array(_) => "string[]",
        Value::Dict(_) => "dictionary",
        Value::ListOp(list) => list
            .entries()
            .next()
            .map(|(_, v)| guess_usda_type(v))
            .unwrap_or("token"),
        Value::Raw(_) | Value::None => "token",
    }
}

/// Escape a quoted string for emission inside `"..."` USDA syntax.
/// Backslash + double-quote are the only mandatory escapes; we keep
/// the rest of the bytes verbatim so non-ASCII names round-trip.
fn escape_usda_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Build the metadata assignment line(s) for one `( ... )` block entry.
///
/// * A [`Value::ListOp`] emits one `OP key = VALUE` line per populated
///   sublist — `prepend` / `append` / `delete` / `reorder` — and an
///   unqualified `key = VALUE` line for the explicit (reset) sublist,
///   so a `delete references = @x@` round-trips as a *delete* rather
///   than being silently turned into a `prepend` add.
/// * A non-list-op value on a composition-arc key still honours the
///   [`PREPEND_LIST_EDIT_KEYS`] convention (emit with `prepend`) so
///   pre-existing authored opinions and writer-synthesised arcs keep
///   the LIVRPS-strength spelling.
/// * Every other field emits a single bare `key = VALUE` line.
fn format_metadata_lines(key: &str, value: &Value) -> Vec<String> {
    if let Value::ListOp(list) = value {
        let mut lines = Vec::new();
        for (op, sublist) in list.entries() {
            let body = format_metadata_value(sublist);
            match op.keyword() {
                Some(kw) => lines.push(format!("{kw} {key} = {body}")),
                None => lines.push(format!("{key} = {body}")),
            }
        }
        if lines.is_empty() {
            lines.push(format!("{key} = {}", format_metadata_value(value)));
        }
        return lines;
    }

    let body = format_metadata_value(value);
    if PREPEND_LIST_EDIT_KEYS.contains(&key) {
        vec![format!("prepend {key} = {body}")]
    } else {
        vec![format!("{key} = {body}")]
    }
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
