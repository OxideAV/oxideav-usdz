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
    AlphaMode, AudioData, AudioEmitter, AudioSource, AuralMode, Axis, ImageData, Indices, Material,
    MaterialId, Mesh, MeshId, NodeId, Primitive, Scene3D, Texture, TextureRef, Topology, Transform,
    Unit,
};

use crate::composition::{CompositionMode, CompositionRecord};
use crate::usda::Value;

/// Archive entry name of the root layer the encoder emits when the
/// scene carries no composition record naming another one.
pub const DEFAULT_ROOT_LAYER_NAME: &str = "scene.usda";

/// Writer options — see [`write_layer_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteOptions {
    /// Flatten (default) or preserve the composed structure recorded
    /// on `Scene3D::extras["usd:composition"]`.
    pub composition: CompositionMode,
}

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
    write_layer_with(scene, &WriteOptions::default())
}

/// [`write_layer`] with explicit [`WriteOptions`].
///
/// Under [`CompositionMode::Preserve`] and a scene carrying a
/// [`CompositionRecord`] (`Scene3D::extras["usd:composition"]`, the
/// decoder's typed opinion model), the emitted root layer re-authors
/// the source's composition instead of the flattened result: prims
/// an arc or a variant selection contributed are left to that arc,
/// the arc opinions come back from the local skeleton, and `class` /
/// `over` prims the typed model has no slot for are replayed
/// verbatim. The consumed layer entries themselves are the
/// encoder's job ([`crate::UsdzEncoder`] copies them into the
/// package) — see [`crate::composition`].
pub fn write_layer_with(scene: &Scene3D, opts: &WriteOptions) -> String {
    // Typed per-reference UV transforms (`TextureRef::transform`)
    // have no staged USD encoding — bake them into UV channels
    // first (no-op on the common transform-free scene).
    let baked;
    let scene = match bake_texture_transforms(scene) {
        Some(b) => {
            baked = b;
            &baked
        }
        None => scene,
    };
    let mut w = Out::default();
    if opts.composition == CompositionMode::Preserve {
        w.record = CompositionRecord::from_extras(&scene.extras).filter(|r| !r.is_trivial());
    }
    writeln!(w.s, "#usda 1.0").unwrap();
    write_layer_metadata(&mut w, scene);
    writeln!(w.s).unwrap();

    for &root in &scene.roots {
        write_node(&mut w, scene, root, /*parent_path=*/ "");
    }
    // Composition-preserving: the local layer's non-`def` root prims
    // (`class` hierarchies, dangling `over`s) have no typed-model
    // slot — replay them verbatim from the record.
    if let Some(record) = w.record.clone() {
        for prim in record.local_non_def_children("") {
            write_prim(&mut w, prim);
        }
    }
    // Typed-model static morph states (`Node::weights`) with no
    // SkelAnimation carrier get a synthesized root-level
    // `def SkelAnimation "BlendState_<id>"` — USD's only encoding
    // for a blend-shape weight state (§1.3/§1.5).
    write_synth_blend_states(&mut w, scene);
    // Materials live outside the node tree in our model — emit any
    // material that wasn't already pulled in as a node child by
    // hanging them off a synthetic `/Materials` Scope. Real-world
    // USDZ assets typically nest materials under their mesh's prim
    // path, but the synthetic scope keeps our output self-consistent
    // and decodable: every `material:binding = </Materials/<name>>`
    // resolves through our reader.
    let emitted_materials: Vec<(usize, &Material)> = scene
        .materials
        .iter()
        .enumerate()
        .filter(|(_, mat)| !w.material_is_contributed(mat))
        .collect();
    if !emitted_materials.is_empty() {
        writeln!(w.s, "def Scope \"Materials\" {{").unwrap();
        w.indent += 1;
        for (i, mat) in emitted_materials {
            write_material(&mut w, scene, mat, i);
        }
        w.indent -= 1;
        writeln!(w.s, "}}").unwrap();
    }
    w.s
}

/// Texture ids the emitted layer references — every texture unless
/// a composition-preserving write left some materials to the arcs
/// that contributed them (their textures then ride in the consumed
/// layer's own assets).
pub fn emitted_texture_ids(scene: &Scene3D, opts: &WriteOptions) -> Vec<bool> {
    let record = match opts.composition {
        CompositionMode::Preserve => {
            CompositionRecord::from_extras(&scene.extras).filter(|r| !r.is_trivial())
        }
        CompositionMode::Flatten => None,
    };
    let Some(record) = record else {
        return vec![true; scene.textures.len()];
    };
    let mut used = vec![false; scene.textures.len()];
    for mat in &scene.materials {
        if material_contributed(&record, mat) {
            continue;
        }
        for (_, tref) in mat.texture_refs() {
            if let Some(slot) = used.get_mut(tref.texture.0 as usize) {
                *slot = true;
            }
        }
        for key in EXTRAS_TEX_INPUTS
            .iter()
            .map(|(k, _)| *k)
            .chain(["usd:tex:opacity"])
        {
            if let Some(idx) = mat
                .extras
                .get(key)
                .and_then(|v| v.get("texture"))
                .and_then(|v| v.as_u64())
            {
                if let Some(slot) = used.get_mut(idx as usize) {
                    *slot = true;
                }
            }
        }
    }
    used
}

fn material_contributed(record: &CompositionRecord, mat: &Material) -> bool {
    mat.extras
        .get("usd:primPath")
        .and_then(|v| v.as_str())
        .is_some_and(|path| !record.is_local(path))
}

/// Bake every typed per-reference UV transform
/// (`TextureRef::transform`, the ratified `KHR_texture_transform`
/// semantics carried by `oxideav-mesh3d` 0.0.5) into concrete UV
/// channels, returning the rewritten scene — or `None` when the
/// scene declares no transforms (the common case; zero cost).
///
/// The staged USD material schema gives this writer no way to
/// express a UV-coordinate transform as a prim: `UsdUVTexture`'s
/// §2.2 `scale` / `bias` inputs are per-channel *color* affines,
/// and no 2D-transform shader-node schema is staged under
/// `docs/3d/usd/`. `TextureTransform::apply_channel` is the typed
/// model's documented baking lift for exporters targeting a
/// transform-free encoding, so:
///
/// * each distinct `(source channel, transform)` pair on a material
///   appends one pre-transformed UV channel to **every** primitive
///   that can draw with that material (base binding or a
///   `KHR_materials_variants` mapping) — allocated at a shared
///   index so the single per-material `UsdPrimvarReader` varname
///   (`st<N>`) is consistent across all of them; primitives with
///   fewer channels pad with copies of the source channel so no
///   index gap violates the typed model's parallel-array contract;
/// * the reference retargets to the baked channel with its
///   transform cleared (a `texCoord`-only override — affine
///   identity — folds straight into `uv_set`, no new channel);
/// * a transform whose source channel is missing on some bound
///   primitive cannot be baked (the input scene fails
///   `Scene3D::validate`'s `UvSetOutOfRange` there anyway) and is
///   flattened to its `texCoord` half.
///
/// Like composition flattening, this is a *lossy-flattening* encode
/// step: the transform itself is consumed, but the sampled
/// coordinates — the ground truth — are preserved exactly, and the
/// output round-trips through this crate's reader as a fixed point.
fn bake_texture_transforms(scene: &Scene3D) -> Option<Scene3D> {
    use oxideav_mesh3d::TextureTransform;
    let affine_active =
        |t: &TextureTransform| t.offset != [0.0, 0.0] || t.rotation != 0.0 || t.scale != [1.0, 1.0];
    if !scene
        .materials
        .iter()
        .any(|m| m.texture_refs().iter().any(|(_, r)| r.transform.is_some()))
    {
        return None;
    }
    let mut out = scene.clone();
    for mid in 0..out.materials.len() {
        // Distinct (source channel, transform) pairs needing a bake.
        let plans: Vec<(u32, TextureTransform)> = {
            let mut v: Vec<(u32, TextureTransform)> = Vec::new();
            for (_, r) in out.materials[mid].texture_refs() {
                if let Some(t) = r.transform {
                    let key = (r.effective_uv_set(), t);
                    // A non-finite transform (validate's
                    // `TextureTransformNotFinite`) would poison every
                    // baked coordinate — never bake it; the fold
                    // below keeps its `texCoord` half only.
                    if affine_active(&t) && t.is_finite() && !v.contains(&key) {
                        v.push(key);
                    }
                }
            }
            v
        };
        // Primitives that can draw with this material.
        let uses: Vec<(usize, usize)> = out
            .meshes
            .iter()
            .enumerate()
            .flat_map(|(mi, mesh)| {
                mesh.primitives
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        p.material.is_some_and(|id| id.0 as usize == mid)
                            || p.variant_mappings
                                .iter()
                                .any(|vm| vm.material.0 as usize == mid)
                    })
                    .map(move |(pi, _)| (mi, pi))
            })
            .collect();
        let mut alloc: Vec<((u32, TextureTransform), u32)> = Vec::new();
        for (src, t) in plans {
            let bakeable = !uses.is_empty()
                && uses.iter().all(|&(mi, pi)| {
                    out.meshes[mi].primitives[pi]
                        .uvs
                        .get(src as usize)
                        .is_some_and(|c| !c.is_empty())
                });
            if !bakeable {
                continue;
            }
            let n = uses
                .iter()
                .map(|&(mi, pi)| out.meshes[mi].primitives[pi].uvs.len())
                .max()
                .unwrap_or(0) as u32;
            for &(mi, pi) in &uses {
                let prim = &mut out.meshes[mi].primitives[pi];
                let srcv = prim.uvs[src as usize].clone();
                while (prim.uvs.len() as u32) < n {
                    prim.uvs.push(srcv.clone());
                }
                prim.uvs.push(t.apply_channel(&srcv));
            }
            alloc.push(((src, t), n));
        }
        out.materials[mid].map_texture_refs(|mut r| {
            if let Some(t) = r.transform {
                let src = r.effective_uv_set();
                r.uv_set = alloc
                    .iter()
                    .find(|((s, at), _)| *s == src && *at == t)
                    .map(|&(_, n)| n)
                    .unwrap_or(src);
                r.transform = None;
            }
            r
        });
    }
    Some(out)
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
    // An external URI reference (`<UDIM>` tile sets, cross-file
    // references) re-emits the authored path verbatim — no archive
    // entry backs it (`collect_texture_assets` skips it), so no
    // entry-name sanitisation applies.
    if let ImageData::External { uri, .. } = &tex.image {
        return uri.clone();
    }
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
    /// Composition-preserving write: the decoder's typed opinion
    /// model. `None` flattens.
    record: Option<CompositionRecord>,
}

impl Out {
    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.s.push_str("    ");
        }
    }

    /// Preserve mode only: `true` when the prim at `path` was not
    /// authored by the local layer — an arc or a variant selection
    /// contributed it, so the re-authored arc brings it back.
    fn contributed(&self, path: &str) -> bool {
        self.record.as_ref().is_some_and(|r| !r.is_local(path))
    }

    fn material_is_contributed(&self, mat: &Material) -> bool {
        self.record
            .as_ref()
            .is_some_and(|r| material_contributed(r, mat))
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
    // Flattening: a sublayer the decoder folded into this layer must
    // not be re-authored — the entry is not in the emitted package
    // and its opinions are already here. Preserving keeps the list
    // verbatim; the encoder copies the entries alongside.
    let composed_sublayers: Vec<String> = if w.record.is_some() {
        Vec::new()
    } else {
        scene
            .extras
            .get("usd:composedSubLayers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
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
        if k == "subLayers" && !composed_sublayers.is_empty() {
            let Some(pruned) = prune_sublayers(v, &composed_sublayers) else {
                continue;
            };
            for formatted in format_metadata_lines(k, &pruned) {
                writeln!(w.s, "    {formatted}").unwrap();
            }
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

/// Drop the entries of a `subLayers` value that name one of the
/// decoder-composed (now flattened) sublayers. Returns `None` when
/// nothing survives. Entry paths are matched on their anchored
/// spelling (`./geom.usda` ↔ `geom.usda`, or a deeper
/// `dir/geom.usda` entry name).
fn prune_sublayers(v: &Value, composed: &[String]) -> Option<Value> {
    fn is_composed(item: &Value, composed: &[String]) -> bool {
        let Some(text) = item.as_text() else {
            return false;
        };
        let stripped = text.strip_prefix("./").unwrap_or(text);
        composed
            .iter()
            .any(|c| c == stripped || c.ends_with(&format!("/{stripped}")))
    }
    fn prune_seq(v: &Value, composed: &[String]) -> Option<Value> {
        match v {
            Value::Array(items) => {
                let kept: Vec<Value> = items
                    .iter()
                    .filter(|i| !is_composed(i, composed))
                    .cloned()
                    .collect();
                if kept.is_empty() {
                    None
                } else {
                    Some(Value::Array(kept))
                }
            }
            other if is_composed(other, composed) => None,
            other => Some(other.clone()),
        }
    }
    match v {
        Value::ListOp(list) => {
            let mut out = crate::usda::ListOp::default();
            let mut any = false;
            for (op, sub) in list.entries() {
                if let Some(kept) = prune_seq(sub, composed) {
                    out.set(op, kept);
                    any = true;
                }
            }
            any.then(|| Value::ListOp(Box::new(out)))
        }
        other => prune_seq(other, composed),
    }
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
    // Composition-preserving: a prim an arc / variant selection
    // contributed comes back through the re-authored arc.
    if w.contributed(&path) {
        return;
    }

    // UsdSkel: a skeleton-carrier node (decoder marker
    // `usd:skeleton` = SkeletonId) re-emits as a `def Skeleton` prim
    // reconstructed from the typed model; its joint subtree must NOT
    // be walked as plain Xforms.
    if let Some(skel_idx) = node.extras.get("usd:skeleton").and_then(|v| v.as_u64()) {
        write_skeleton_prim(w, scene, node, &safe_name, skel_idx as usize);
        return;
    }

    // UsdSkel §1.3: an animation-carrier node (decoder marker
    // `usd:skelAnimation` = animation index) re-emits as a
    // `def SkelAnimation` reconstructed from the typed channels.
    if let Some(anim_idx) = node
        .extras
        .get("usd:skelAnimation")
        .and_then(|v| v.as_u64())
    {
        write_skel_animation_prim(w, scene, &safe_name, anim_idx as usize, &node.extras);
        return;
    }
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
    let composition_lines = extract_composition_arc_lines(&node.extras, w.record.as_ref(), &path);
    let prim_metadata_lines =
        build_prim_metadata_lines(&variant_sets, &selection, &composition_lines);

    // Bare-mesh-carrier collapse. The decoder turns an inner
    // `def Mesh "M"` into a *standalone* mesh-carrier node named `M`
    // (mesh set, identity transform, no other content). Re-emitting
    // such a node as `def Xform "M" { def Mesh "M" }` would wrap the
    // geometry in a redundant Xform level — and because the decoder
    // then re-externalises that inner mesh again on the next read, the
    // structure grows by one Xform every encode→decode cycle (an
    // unbounded round-trip drift). When a node is a pure mesh carrier
    // whose own name already equals its mesh's prim name — exactly the
    // shape the decoder produces — emit the mesh prim(s) directly at
    // this level, skipping the wrapper. Nodes whose name differs from
    // the mesh (a genuine Xform locator wrapping a differently-named
    // mesh) keep the Xform and are unaffected, so hand-authored layers
    // round-trip byte-for-byte as before. This makes the round-trip a
    // fixed point instead of a monotonically-growing tree.
    if prim_metadata_lines.is_empty() {
        if let Some(mesh_id) = node.mesh {
            if node.children.is_empty()
                && node.audio_emitter.is_none()
                && node.camera.is_none()
                && node.light.is_none()
                && is_identity(&node.transform)
            {
                if let Some(mesh) = scene.mesh(mesh_id) {
                    let mesh_name = sanitize_prim_name(mesh.name.as_deref().unwrap_or("Mesh"));
                    if mesh_name == safe_name {
                        // A skinned carrier still collapses — the
                        // geometry prim itself carries every §1.5
                        // BindingAPI opinion, so no Xform wrapper is
                        // needed (and keeping one would re-grow the
                        // tree each encode→decode cycle).
                        let skel = skel_binding_paths(scene, id, node);
                        write_mesh(w, scene, mesh, mesh_id, parent_path, skel.as_ref());
                        return;
                    }
                }
            }
        }
    }

    // Prim schema token: the decoder preserves non-Xform container
    // schemas (`SkelRoot`, ...) on `extras["usd:type"]`; anything
    // that doesn't look like a bare schema identifier falls back to
    // `Xform`. Meshes hang off as inner `def Mesh` children rather
    // than collapsing into the node's own prim type.
    let prim_type = node
        .extras
        .get("usd:type")
        .and_then(|v| v.as_str())
        .filter(|t| {
            !t.is_empty()
                && t.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .unwrap_or("Xform");
    w.write_indent();
    if prim_metadata_lines.is_empty() {
        writeln!(w.s, "def {prim_type} \"{safe_name}\" {{").unwrap();
    } else {
        writeln!(w.s, "def {prim_type} \"{safe_name}\" (").unwrap();
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

    // UsdGeomPointInstancer: the typed record re-emits its
    // relationship + §2.2 arrays (defaults and time samples).
    if let Some(record) = crate::point_instancer::PointInstancer::from_node(node) {
        for line in record.to_usda_lines() {
            w.write_indent();
            writeln!(w.s, "{line}").unwrap();
        }
    }

    // §3.4 rule 1: a container prim's authored `material:binding*`
    // relationships (inherited by descendant gprims during decode)
    // and its §15.1 CollectionAPI properties replay verbatim from
    // the decoder's stashes.
    replay_attr_stash(w, &node.extras, "usd:materialBindings");
    replay_attr_stash(w, &node.extras, "usd:collections");

    // Mesh attachment — emit an inner `def Mesh` so its prim path is
    // `<parent>/<node_name>/<mesh_name>`. A skinned node passes its
    // bound skeleton's emitted prim path down so the geometry prim
    // carries the §1.5 BindingAPI opinions.
    if let Some(mesh_id) = node.mesh {
        if let Some(mesh) = scene.mesh(mesh_id) {
            let skel = skel_binding_paths(scene, id, node);
            write_mesh(w, scene, mesh, mesh_id, &path, skel.as_ref());
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
    // Composition-preserving: nested `class` / `over` prims the
    // local layer authored under this prim replay verbatim.
    if let Some(record) = w.record.clone() {
        for prim in record.local_non_def_children(&path) {
            write_prim(w, prim);
        }
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
    record: Option<&CompositionRecord>,
    path: &str,
) -> Vec<String> {
    let mut meta = extras
        .get(PRIM_METADATA_EXTRAS_KEY)
        .map(crate::variant_codec::decode_btree_value)
        .unwrap_or_default();
    // Composition-preserving: the arcs the decoder consumed (and
    // stripped from the stash) come back exactly as the local layer
    // authored them, list-edit operators included.
    if let Some(record) = record {
        for (k, v) in record.local_arcs(path) {
            meta.insert(k, v);
        }
    }
    if meta.is_empty() {
        return Vec::new();
    }
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

/// Emit `BTreeMap<String, Attr>` as `<type> <name> = <value>` lines,
/// with each attribute's authored `( ... )` metadata block replayed
/// inline (e.g. `bindMaterialAs`, `interpolation`, `elementSize`).
fn write_attr_map(w: &mut Out, attrs: &std::collections::BTreeMap<String, crate::usda::Attr>) {
    for (name, attr) in attrs {
        w.write_indent();
        let type_token = if attr.type_token.is_empty() {
            String::new()
        } else {
            format!("{} ", attr.type_token)
        };
        let meta = if attr.metadata.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                metadata_lines_from_value_map(&attr.metadata).join(", ")
            )
        };
        match render_value(&attr.value) {
            Some(rendered) => writeln!(w.s, "{type_token}{name} = {rendered}{meta}").unwrap(),
            None => writeln!(w.s, "{type_token}{name}{meta}").unwrap(),
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
        V::TimeSamples(samples) => format_time_samples(samples, render_value),
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

/// Serialise a timeSamples map back into its `{ T: V, T: V }` USDA
/// literal. `render` is the value renderer of the calling context
/// ([`render_value`] or [`format_metadata_value`]) so both emission
/// paths stay literal-compatible with their surroundings.
fn format_time_samples(
    samples: &[(f64, Value)],
    render: impl Fn(&Value) -> Option<String>,
) -> String {
    let mut s = String::from("{");
    for (i, (time, value)) in samples.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push(' ');
        s.push_str(&format_float(*time));
        s.push_str(": ");
        s.push_str(&render(value).unwrap_or_else(|| "None".into()));
    }
    s.push_str(" }");
    s
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
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            '\x0B' => out.push_str("\\v"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
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
            // Typed column-vector matrix → USD row-vector literal.
            let m = crate::usd_to_scene::transpose4(m);
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

fn write_mesh(
    w: &mut Out,
    scene: &Scene3D,
    mesh: &Mesh,
    _id: MeshId,
    parent_path: &str,
    skel: Option<&SkelBindingPaths>,
) {
    let raw_name = mesh.name.clone().unwrap_or_else(|| "Mesh".to_string());
    let mesh_name = sanitize_prim_name(&raw_name);
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
    // UsdGeomSubset re-emission (staged schema Part 1): primitives
    // the decoder split out of one authored Mesh prim (marked with
    // `extras["usd:subset"]`) fold back into a *single* `def Mesh`
    // whose triangles are the concatenation [base | subset₁ |
    // subset₂ | ...], with one `def GeomSubset` child per subset
    // primitive claiming its contiguous face run. The base primitive
    // (the one without the marker) carries the mesh-level state —
    // parent binding, doubleSided, skinning, blend shapes, plus the
    // `usd:subsetFamilies` / `usd:geomSubsets` replay stashes.
    if mesh
        .primitives
        .iter()
        .any(|p| p.topology == Topology::Triangles && p.extras.contains_key("usd:subset"))
    {
        if !w.contributed(&format!("{parent_path}/{mesh_name}")) {
            write_subset_mesh(w, scene, mesh, &mesh_name, parent_path, skel);
        }
        return;
    }
    for (i, prim) in mesh.primitives.iter().enumerate() {
        let prim_name = if i == 0 {
            mesh_name.clone()
        } else {
            format!("{mesh_name}_{i}")
        };
        if w.contributed(&format!("{parent_path}/{prim_name}")) {
            continue;
        }
        match prim.topology {
            Topology::Triangles => {
                let prim_path = format!("{parent_path}/{prim_name}");
                write_one_mesh_prim(
                    w,
                    scene,
                    prim,
                    &prim_name,
                    &prim_path,
                    None,
                    skel,
                    &[],
                    &mesh.target_names,
                )
            }
            Topology::TriangleStrip => {
                let expanded = expand_strip_to_triangle_list(prim);
                let prim_path = format!("{parent_path}/{prim_name}");
                write_one_mesh_prim(
                    w,
                    scene,
                    &expanded,
                    &prim_name,
                    &prim_path,
                    Some("triangleStrip"),
                    skel,
                    &[],
                    &mesh.target_names,
                );
            }
            Topology::TriangleFan => {
                let expanded = expand_fan_to_triangle_list(prim);
                let prim_path = format!("{parent_path}/{prim_name}");
                write_one_mesh_prim(
                    w,
                    scene,
                    &expanded,
                    &prim_name,
                    &prim_path,
                    Some("triangleFan"),
                    skel,
                    &[],
                    &mesh.target_names,
                );
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
///
/// `subsets` carries the `def GeomSubset` children to author inside
/// the prim body — non-empty only on the [`write_subset_mesh`]
/// path. Independently of it, the prim's own extras stashes
/// (`usd:subsetFamilies` familyType properties and
/// `usd:geomSubsets` verbatim non-material subsets) replay here, so
/// a mesh whose only subsets were preserved verbatim re-emits them
/// through the ordinary single-primitive path too.
#[allow(clippy::too_many_arguments)]
fn write_one_mesh_prim(
    w: &mut Out,
    scene: &Scene3D,
    prim: &Primitive,
    prim_name: &str,
    prim_path: &str,
    original_topology_hint: Option<&str>,
    skel: Option<&SkelBindingPaths>,
    subsets: &[SubsetChild],
    target_names: &[String],
) {
    let mut metadata_lines: Vec<String> = Vec::new();
    if let Some(token) = original_topology_hint {
        metadata_lines.push(format!("usd:original_topology = \"{token}\""));
    }
    if extras_no_fold(&prim.extras) {
        metadata_lines.push("usd:no_fold = 1".to_string());
    }
    let skinned = skel.is_some_and(|s| s.skeleton.is_some()) && prim.joints.is_some();
    if skinned {
        // §1.5: BindingAPI is an applied API schema — declare it on
        // the skinned geometry prim.
        metadata_lines.push("prepend apiSchemas = [\"SkelBindingAPI\"]".to_string());
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
    // §2.5 `doubleSided` — the decoder's extras flag is authoritative
    // (round-trip); a bound material's `double_sided` covers scenes
    // authored directly through the typed model.
    let double_sided = prim
        .extras
        .get("usd:doubleSided")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || prim
            .material
            .and_then(|id| scene.materials.get(id.0 as usize))
            .map(|m| m.double_sided)
            .unwrap_or(false);
    if double_sided {
        w.write_indent();
        writeln!(w.s, "uniform bool doubleSided = 1").unwrap();
    }
    write_material_binding(w, scene, prim.material, &prim.extras);
    // UsdSkel BindingAPI (§1.5 / §1.6): the skeleton relationship
    // plus the per-vertex joint influences in the canonical layout
    // (`vertex` interpolation, `elementSize = 4` matching the typed
    // quad width, indices in the Skeleton's own joint order — no
    // `skel:joints` override needed on output).
    let mut anim_rel_emitted = false;
    if skinned {
        if let (Some(joints), Some(weights), Some(skel_path)) = (
            &prim.joints,
            &prim.weights,
            skel.and_then(|s| s.skeleton.as_ref()),
        ) {
            w.write_indent();
            writeln!(w.s, "rel skel:skeleton = <{skel_path}>").unwrap();
            if let Some(anim_path) = skel.and_then(|s| s.animation.as_ref()) {
                w.write_indent();
                writeln!(w.s, "rel skel:animationSource = <{anim_path}>").unwrap();
                anim_rel_emitted = true;
            }
            w.write_indent();
            write!(w.s, "int[] primvars:skel:jointIndices = [").unwrap();
            for (i, q) in joints.iter().enumerate() {
                for (e, j) in q.iter().enumerate() {
                    if i > 0 || e > 0 {
                        w.s.push_str(", ");
                    }
                    write!(w.s, "{j}").unwrap();
                }
            }
            writeln!(w.s, "] (elementSize = 4, interpolation = \"vertex\")").unwrap();
            w.write_indent();
            write!(w.s, "float[] primvars:skel:jointWeights = [").unwrap();
            for (i, q) in weights.iter().enumerate() {
                for (e, wgt) in q.iter().enumerate() {
                    if i > 0 || e > 0 {
                        w.s.push_str(", ");
                    }
                    write!(w.s, "{}", format_float(*wgt as f64)).unwrap();
                }
            }
            writeln!(w.s, "] (elementSize = 4, interpolation = \"vertex\")").unwrap();
        }
        if let Some(rows) = prim
            .extras
            .get("usd:skel:geomBindTransform")
            .and_then(|v| v.as_array())
        {
            if let Some(m) = json_matrix4(rows) {
                w.write_indent();
                writeln!(
                    w.s,
                    "matrix4d primvars:skel:geomBindTransform = {}",
                    format_matrix4(m)
                )
                .unwrap();
            }
        }
        if let Some(method) = prim
            .extras
            .get("usd:skel:skinningMethod")
            .and_then(|v| v.as_str())
        {
            w.write_indent();
            writeln!(
                w.s,
                "uniform token primvars:skel:skinningMethod = \"{method}\""
            )
            .unwrap();
        }
    }
    write_blend_shapes(w, prim, prim_path, target_names);
    // §1.5: bind the blend-weight animation source. Skinned geometry
    // already authored the relationship above (one property per prim
    // — the TRS source also carries the blend table in that case).
    if !prim.targets.is_empty() && !anim_rel_emitted {
        if let Some(bp) = skel.and_then(|s| s.blend_animation.as_ref()) {
            w.write_indent();
            writeln!(w.s, "rel skel:animationSource = <{bp}>").unwrap();
        }
    }
    // UsdGeomSubset (staged schema Part 1): §1.4 familyType
    // properties replay on the geometric prim with their exact
    // authored spelling (the property name is discovered by
    // enumeration on decode — never constructed).
    if let Some(families) = prim
        .extras
        .get("usd:subsetFamilies")
        .and_then(|v| v.as_array())
    {
        for fam in families {
            let (Some(name), Some(tok), Some(val)) = (
                fam.get("name").and_then(|v| v.as_str()),
                fam.get("typeToken").and_then(|v| v.as_str()),
                fam.get("value").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            w.write_indent();
            writeln!(w.s, "{tok} {name} = \"{val}\"").unwrap();
        }
    }
    // Material face subsets split into typed primitives on decode.
    for sub in subsets {
        write_geom_subset_child(w, scene, sub);
    }
    // Non-material subsets preserved verbatim on decode.
    if let Some(stash) = prim
        .extras
        .get("usd:geomSubsets")
        .and_then(|v| v.as_array())
    {
        for entry in stash {
            let subset_prim = crate::variant_codec::decode_prim(entry);
            write_prim(w, &subset_prim);
        }
    }
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// One `def GeomSubset` child re-authored by [`write_subset_mesh`]:
/// the face run `[face_start, face_start + face_count)` of the
/// enclosing mesh's (all-triangle) topology, bound to `material`.
struct SubsetChild {
    name: String,
    family_name: Option<String>,
    face_start: u32,
    face_count: u32,
    material: Option<MaterialId>,
    /// Authored `material:binding*` relationship set to replay
    /// verbatim instead of synthesising from `material` (decoded
    /// from the `usd:subset` marker's `bindings` slot).
    bindings: Option<crate::usda::Prim>,
    /// Extra authored opinions preserved from the source subset
    /// prim (decoded from the `usd:subset` marker's `rest` slot).
    rest: Option<crate::usda::Prim>,
}

/// Emit one `def GeomSubset` child (staged schema §1.1/§1.2):
/// `elementType = "face"` (this crate only splits face subsets),
/// the authored `familyName`, the contiguous `indices` run the
/// subset's triangles occupy in the emitted parent topology, its
/// `material:binding`, plus any preserved extra opinions.
fn write_geom_subset_child(w: &mut Out, scene: &Scene3D, sub: &SubsetChild) {
    let name = sanitize_prim_name(&sub.name);
    w.write_indent();
    let metadata_lines = sub
        .rest
        .as_ref()
        .filter(|r| !r.metadata.is_empty())
        .map(|r| metadata_lines_from_value_map(&r.metadata))
        .unwrap_or_default();
    if metadata_lines.is_empty() {
        writeln!(w.s, "def GeomSubset \"{name}\" {{").unwrap();
    } else {
        writeln!(w.s, "def GeomSubset \"{name}\" (").unwrap();
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
    w.write_indent();
    writeln!(w.s, "uniform token elementType = \"face\"").unwrap();
    if let Some(fam) = &sub.family_name {
        w.write_indent();
        writeln!(w.s, "uniform token familyName = \"{fam}\"").unwrap();
    }
    w.write_indent();
    write!(w.s, "int[] indices = [").unwrap();
    for i in 0..sub.face_count {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(w.s, "{}", sub.face_start + i).unwrap();
    }
    writeln!(w.s, "]").unwrap();
    match &sub.bindings {
        // Authored relationship set preserved verbatim (purpose
        // forms, bindMaterialAs metadata, collection forms).
        Some(bindings) => write_attr_map(w, &bindings.attrs),
        None => {
            if let Some(mat_id) = sub.material {
                if let Some(mat) = scene.materials.get(mat_id.0 as usize) {
                    let mat_name = material_prim_name(mat, mat_id.0 as usize);
                    w.write_indent();
                    writeln!(w.s, "rel material:binding = </Materials/{mat_name}>").unwrap();
                }
            }
        }
    }
    if let Some(rest) = &sub.rest {
        write_attr_map(w, &rest.attrs);
        if !rest.variant_sets.is_empty() {
            write_variant_sets(w, &rest.variant_sets);
        }
        for child in &rest.children {
            write_prim(w, child);
        }
    }
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Replay a decoder attribute stash (a shell prim in the
/// [`crate::variant_codec`] tagged encoding) back into authored
/// `<type> <name> = <value> (meta)` lines. No-op when absent.
fn replay_attr_stash(
    w: &mut Out,
    extras: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
) {
    if let Some(stash) = extras.get(key) {
        let shell = crate::variant_codec::decode_prim(stash);
        write_attr_map(w, &shell.attrs);
    }
}

/// Emit a gprim's `material:binding*` relationships and §15.1
/// CollectionAPI properties: replay the decoder's verbatim
/// `usd:materialBindings` stash when present (staged schema
/// §3.1–§3.3 spellings survive byte-for-byte, and an **empty**
/// stash means the binding was inherited from an ancestor — emit
/// nothing), otherwise synthesise the single all-purpose
/// `rel material:binding` from the typed material slot. Any
/// `usd:collections` stash replays after.
fn write_material_binding(
    w: &mut Out,
    scene: &Scene3D,
    material: Option<MaterialId>,
    extras: &std::collections::HashMap<String, serde_json::Value>,
) {
    if extras.contains_key("usd:materialBindings") {
        replay_attr_stash(w, extras, "usd:materialBindings");
    } else if let Some(mat_id) = material {
        if let Some(mat) = scene.materials.get(mat_id.0 as usize) {
            let mat_name = material_prim_name(mat, mat_id.0 as usize);
            w.write_indent();
            writeln!(w.s, "rel material:binding = </Materials/{mat_name}>").unwrap();
        }
    }
    replay_attr_stash(w, extras, "usd:collections");
}

/// Re-author a decoder-split subset mesh as a **single**
/// `def Mesh` + `def GeomSubset` children (staged schema Part 1).
///
/// The emitted topology is the concatenation
/// `[base triangles | subset₁ triangles | subset₂ triangles | ...]`
/// (base = the primitive without the `usd:subset` marker), so each
/// subset claims a contiguous face run and the base's triangles are
/// exactly the §1.3 unassigned set falling back to the parent
/// binding. Vertex arrays come from the base primitive — the
/// decoder's split shares them across every subset primitive.
/// Non-triangle primitives and any additional unmarked primitives
/// beyond the first re-emit as sibling prims under the ordinary
/// per-primitive rules.
///
/// The first decode of an authored layer normalises subset
/// membership into this concatenated form (arbitrary interleaved /
/// overlapping face claims become contiguous runs, duplicating any
/// face claimed twice); from then on encode → decode → encode is a
/// fixed point.
fn write_subset_mesh(
    w: &mut Out,
    scene: &Scene3D,
    mesh: &Mesh,
    mesh_name: &str,
    parent_path: &str,
    skel: Option<&SkelBindingPaths>,
) {
    let is_subset =
        |p: &Primitive| p.topology == Topology::Triangles && p.extras.contains_key("usd:subset");
    let flat_indices = |p: &Primitive| -> Vec<u32> {
        match &p.indices {
            Some(Indices::U16(v)) => v.iter().map(|&i| i as u32).collect(),
            Some(Indices::U32(v)) => v.clone(),
            None => (0..p.positions.len() as u32).collect(),
        }
    };

    // The base primitive carries mesh-level state; a typed-model
    // scene authored without one borrows the first subset
    // primitive's vertex arrays and starts with zero faces.
    let base = mesh
        .primitives
        .iter()
        .find(|p| p.topology == Topology::Triangles && !p.extras.contains_key("usd:subset"));
    let mut combined = match base {
        Some(b) => b.clone(),
        None => {
            let first = mesh
                .primitives
                .iter()
                .find(|p| is_subset(p))
                .expect("write_subset_mesh called with no subset primitive");
            let mut shell = first.clone();
            shell.extras.remove("usd:subset");
            shell.material = None;
            shell.indices = Some(Indices::U32(Vec::new()));
            shell
        }
    };

    let mut flat = flat_indices(&combined);
    let mut children: Vec<SubsetChild> = Vec::new();
    for prim in mesh.primitives.iter().filter(|p| is_subset(p)) {
        let marker = prim
            .extras
            .get("usd:subset")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let sub_flat = flat_indices(prim);
        let face_start = (flat.len() / 3) as u32;
        let face_count = (sub_flat.len() / 3) as u32;
        flat.extend_from_slice(&sub_flat);
        children.push(SubsetChild {
            name: marker
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("subset")
                .to_string(),
            family_name: marker
                .get("familyName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            face_start,
            face_count,
            material: prim.material,
            bindings: marker
                .get("bindings")
                .map(crate::variant_codec::decode_prim),
            rest: marker.get("rest").map(crate::variant_codec::decode_prim),
        });
    }
    combined.indices = Some(if combined.positions.len() <= u16::MAX as usize {
        Indices::U16(flat.iter().map(|&i| i as u16).collect())
    } else {
        Indices::U32(flat)
    });

    let prim_path = format!("{parent_path}/{mesh_name}");
    write_one_mesh_prim(
        w,
        scene,
        &combined,
        mesh_name,
        &prim_path,
        None,
        skel,
        &children,
        &mesh.target_names,
    );

    // Any additional unmarked primitives beyond the base re-emit as
    // sibling prims under the ordinary per-primitive rules (they
    // never arise from this crate's own decoder, which produces
    // exactly one base).
    let base_ptr = base.map(|b| b as *const Primitive);
    for (i, prim) in mesh.primitives.iter().enumerate() {
        if is_subset(prim) || base_ptr == Some(prim as *const Primitive) {
            continue;
        }
        let prim_name = format!("{mesh_name}_{i}");
        match prim.topology {
            Topology::Triangles => {
                let p_path = format!("{parent_path}/{prim_name}");
                write_one_mesh_prim(
                    w,
                    scene,
                    prim,
                    &prim_name,
                    &p_path,
                    None,
                    skel,
                    &[],
                    &mesh.target_names,
                );
            }
            Topology::TriangleStrip => {
                let expanded = expand_strip_to_triangle_list(prim);
                let p_path = format!("{parent_path}/{prim_name}");
                write_one_mesh_prim(
                    w,
                    scene,
                    &expanded,
                    &prim_name,
                    &p_path,
                    Some("triangleStrip"),
                    skel,
                    &[],
                    &mesh.target_names,
                );
            }
            Topology::TriangleFan => {
                let expanded = expand_fan_to_triangle_list(prim);
                let p_path = format!("{parent_path}/{prim_name}");
                write_one_mesh_prim(
                    w,
                    scene,
                    &expanded,
                    &prim_name,
                    &p_path,
                    Some("triangleFan"),
                    skel,
                    &[],
                    &mesh.target_names,
                );
            }
            Topology::Lines | Topology::LineStrip | Topology::LineLoop => {
                write_basis_curves_prim(w, scene, prim, &prim_name);
            }
            Topology::Points => write_points_prim(w, scene, prim, &prim_name),
        }
    }
}

/// UsdSkel blend shapes (§1.4 / §1.5): emit the geometry's
/// morph-target roster back as `def BlendShape` children of the
/// mesh prim plus the positional `skel:blendShapes` /
/// `skel:blendShapeTargets` pair. Channel names come from the typed
/// `Mesh::target_names` (falling back to `shape_<i>` for unnamed
/// slots — see [`blend_channel_names`]); targets emit dense (a
/// decoded sparse shape was already scattered into per-point
/// deltas); each typed `MorphTarget::inbetweens` station re-authors
/// as a §1.4.1 `inbetweens:<name>` attribute with its `weight`
/// metadata.
fn write_blend_shapes(w: &mut Out, prim: &Primitive, prim_path: &str, target_names: &[String]) {
    if prim.targets.is_empty() {
        return;
    }
    let names = blend_channel_names(prim, target_names);
    let normals_attrs = prim
        .extras
        .get("usd:skel:inbetweenNormalsAttr")
        .and_then(|v| v.as_object());
    let malformed = prim
        .extras
        .get("usd:skel:malformedInbetweens")
        .and_then(|v| v.as_object());

    w.write_indent();
    write!(w.s, "uniform token[] skel:blendShapes = [").unwrap();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(w.s, "\"{name}\"").unwrap();
    }
    writeln!(w.s, "]").unwrap();
    w.write_indent();
    write!(w.s, "rel skel:blendShapeTargets = [").unwrap();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(w.s, "<{prim_path}/{name}>").unwrap();
    }
    writeln!(w.s, "]").unwrap();

    let write_deltas = |w: &mut Out, lead: &str, arr: &[[f32; 3]], trail: &str| {
        w.write_indent();
        w.s.push_str(lead);
        for (i, o) in arr.iter().enumerate() {
            if i > 0 {
                w.s.push_str(", ");
            }
            write!(
                w.s,
                "({}, {}, {})",
                format_float(o[0] as f64),
                format_float(o[1] as f64),
                format_float(o[2] as f64)
            )
            .unwrap();
        }
        writeln!(w.s, "]{trail}").unwrap();
    };

    for (name, target) in names.iter().zip(&prim.targets) {
        w.write_indent();
        writeln!(w.s, "def BlendShape \"{name}\" {{").unwrap();
        w.indent += 1;
        if let Some(offsets) = &target.position {
            write_deltas(w, "uniform vector3f[] offsets = [", offsets, "");
        }
        if let Some(normals) = &target.normal {
            write_deltas(w, "uniform vector3f[] normalOffsets = [", normals, "");
        }
        // §1.4.1: each inbetween is a single `inbetweens:<name>`
        // attribute; its station weight rides in the attribute's
        // `weight` metadata field. Anonymous typed-model stations
        // get a deterministic `inbetween_<j>` name.
        for (j, inb) in target.inbetweens.iter().enumerate() {
            let inb_name = inb
                .name
                .as_deref()
                .map(sanitize_prim_name)
                .unwrap_or_else(|| format!("inbetween_{j}"));
            if let Some(offsets) = &inb.position {
                write_deltas(
                    w,
                    &format!("uniform vector3f[] inbetweens:{inb_name} = ["),
                    offsets,
                    &format!(" (weight = {})", format_float(inb.weight as f64)),
                );
            }
            // Normal offsets replay only under the exact authored
            // spelling discovered on decode — the property name is
            // not published (§1.4.2), so it is never constructed;
            // typed-model-authored inbetween normals without a
            // stashed spelling cannot be authored and are skipped.
            let spelling = normals_attrs
                .and_then(|r| r.get(name))
                .and_then(|m| inb.name.as_deref().and_then(|n| m.get(n)))
                .and_then(|v| v.as_str());
            if let (Some(attr_name), Some(normals)) = (spelling, &inb.normal) {
                write_deltas(
                    w,
                    &format!("uniform vector3f[] {attr_name} = ["),
                    normals,
                    "",
                );
            }
        }
        // §1.4.1 authoring-error inbetweens (weight 0/1, duplicate
        // weights, missing weight metadata) were excluded from
        // evaluation on decode but replay verbatim.
        if let Some(shell) = malformed.and_then(|m| m.get(name)) {
            let shell = crate::variant_codec::decode_prim(shell);
            write_attr_map(w, &shell.attrs);
        }
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, "}}").unwrap();
    }
}

/// The writer-side channel roster of a primitive: one sanitized
/// name per morph target — the typed `Mesh::target_names` entry
/// when present, else `shape_<i>` — exactly the roster
/// [`write_blend_shapes`] authors as `skel:blendShapes`.
fn blend_channel_names(prim: &Primitive, target_names: &[String]) -> Vec<String> {
    (0..prim.targets.len())
        .map(|i| {
            target_names
                .get(i)
                .map(|n| sanitize_prim_name(n))
                .unwrap_or_else(|| format!("shape_{i}"))
        })
        .collect()
}

/// Reconstruct a 4x4 from the row-array JSON stash shape
/// (`[[a,b,c,d], ...]` — four rows of four numbers).
fn json_matrix4(rows: &[serde_json::Value]) -> Option<[[f32; 4]; 4]> {
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
    Some(m)
}

/// Format a 4x4 as USD's `matrix4d` literal — a tuple of 4 row
/// tuples.
fn format_matrix4(m: [[f32; 4]; 4]) -> String {
    let mut s = String::from("(");
    for (i, row) in m.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!(
            "({}, {}, {}, {})",
            format_float(row[0] as f64),
            format_float(row[1] as f64),
            format_float(row[2] as f64),
            format_float(row[3] as f64)
        ));
    }
    s.push(')');
    s
}

/// UsdSkel binding paths the node walk passes down to a skinned
/// geometry prim: the bound skeleton's emitted prim path plus (when
/// an animation drives that skeleton) the animation's emitted prim
/// path for the `skel:animationSource` relationship.
struct SkelBindingPaths {
    /// Bound `Skeleton` carrier path — `Some` only for skinned nodes.
    skeleton: Option<String>,
    /// TRS animation source for the bound skeleton.
    animation: Option<String>,
    /// Blend-weight animation source for this node's geometry: the
    /// SkelAnimation whose `MorphWeights` channel targets the node,
    /// or (static `Node::weights` state) the carrier the decoder
    /// recorded / the `BlendState_<id>` prim the writer synthesizes.
    blend_animation: Option<String>,
}

/// Resolve a node's [`SkelBindingPaths`]. `None` when the node
/// carries neither a skin binding nor any blend-weight animation
/// state.
fn skel_binding_paths(
    scene: &Scene3D,
    id: NodeId,
    node: &oxideav_mesh3d::Node,
) -> Option<SkelBindingPaths> {
    let skeleton = node
        .skin
        .and_then(|sid| scene.skins.get(sid.0 as usize))
        .map(|skin| skin.skeleton)
        .and_then(|sk| marker_prim_path(scene, "usd:skeleton", sk.0 as u64).map(|p| (sk, p)));
    let animation = skeleton.as_ref().and_then(|(sk, _)| {
        animation_index_for_skeleton(scene, *sk)
            .and_then(|idx| marker_prim_path(scene, "usd:skelAnimation", idx as u64))
    });
    let blend_animation = blend_animation_rel_path(scene, id, node);
    let skeleton = skeleton.map(|(_, p)| p);
    if skeleton.is_none() && blend_animation.is_none() {
        return None;
    }
    Some(SkelBindingPaths {
        skeleton,
        animation,
        blend_animation,
    })
}

/// Path of the SkelAnimation carrier already driving this node's
/// morph state: (a) the animation with a `MorphWeights` channel
/// targeting the node, else (b) the carrier index the decoder
/// recorded on the node when it attached a static blend state
/// (`usd:skel:weightsAnim`).
fn existing_blend_anim_path(
    scene: &Scene3D,
    id: NodeId,
    node: &oxideav_mesh3d::Node,
) -> Option<String> {
    use oxideav_mesh3d::AnimationProperty;
    if let Some(idx) = scene.animations.iter().position(|a| {
        a.channels
            .iter()
            .any(|ch| ch.target.property == AnimationProperty::MorphWeights && ch.target.node == id)
    }) {
        // A typed-model animation with no decoder carrier node gets
        // the root-level prim `write_synth_blend_states` synthesizes.
        return marker_prim_path(scene, "usd:skelAnimation", idx as u64)
            .or_else(|| Some(format!("/{}", synth_blend_anim_name(idx))));
    }
    node.extras
        .get("usd:skel:weightsAnim")
        .and_then(|v| v.as_u64())
        .and_then(|idx| marker_prim_path(scene, "usd:skelAnimation", idx))
}

/// `true` when the node holds a static morph state the output layer
/// must express: a non-empty `Node::weights` override over a mesh
/// that actually carries morph targets.
fn node_static_morph_state(scene: &Scene3D, node: &oxideav_mesh3d::Node) -> bool {
    !node.weights.is_empty()
        && node
            .mesh
            .and_then(|m| scene.mesh(m))
            .and_then(|m| m.primitives.first())
            .is_some_and(|p| !p.targets.is_empty())
}

/// The `skel:animationSource` target for a node's blend-shape
/// state: an existing carrier when one drives the node, else — for
/// a typed-model `Node::weights` override with no carrier at all —
/// the deterministic root-level `BlendState_<id>` prim
/// [`write_synth_blend_states`] emits.
fn blend_animation_rel_path(
    scene: &Scene3D,
    id: NodeId,
    node: &oxideav_mesh3d::Node,
) -> Option<String> {
    existing_blend_anim_path(scene, id, node)
        .or_else(|| node_static_morph_state(scene, node).then(|| format!("/BlendState_{}", id.0)))
}

/// Index of the first animation whose channels target the given
/// skeleton's joints — the `skel:animationSource` the writer
/// re-authors for geometry bound to that skeleton.
fn animation_index_for_skeleton(
    scene: &Scene3D,
    skeleton_id: oxideav_mesh3d::SkeletonId,
) -> Option<usize> {
    let skeleton = scene.skeletons.get(skeleton_id.0 as usize)?;
    let joint_set: std::collections::BTreeSet<u32> = skeleton.joints.iter().map(|j| j.0).collect();
    scene.animations.iter().position(|anim| {
        anim.channels
            .iter()
            .any(|ch| joint_set.contains(&ch.target.node.0))
    })
}

/// Emitted prim path of the carrier node whose extras `marker` holds
/// `target` — found by replaying the writer's deterministic naming
/// over the node forest. `None` when the scene has no such carrier
/// (e.g. a programmatically built scene without the decoder's
/// markers).
fn marker_prim_path(scene: &Scene3D, marker: &str, target: u64) -> Option<String> {
    fn walk(
        scene: &Scene3D,
        id: NodeId,
        parent_path: &str,
        marker: &str,
        target: u64,
    ) -> Option<String> {
        let node = scene.node(id)?;
        let name = node
            .name
            .clone()
            .unwrap_or_else(|| format!("node_{}", id.0));
        let path = format!("{parent_path}/{}", sanitize_prim_name(&name));
        if node.extras.get(marker).and_then(|v| v.as_u64()) == Some(target) {
            return Some(path);
        }
        for &child in &node.children {
            if let Some(found) = walk(scene, child, &path, marker, target) {
                return Some(found);
            }
        }
        None
    }
    scene
        .roots
        .iter()
        .find_map(|&root| walk(scene, root, "", marker, target))
}

/// Layer timeCodes-per-second used to map the typed model's
/// keyframe seconds back onto SkelAnimation timeCodes — read from
/// the preserved layer metadata (`usd:timeCodesPerSecond` /
/// `usd:framesPerSecond` extras), defaulting to USD's 24.
fn time_codes_per_second(scene: &Scene3D) -> f64 {
    scene
        .extras
        .get("usd:timeCodesPerSecond")
        .or_else(|| scene.extras.get("usd:framesPerSecond"))
        .and_then(|v| v.as_f64())
        .filter(|f| *f > 0.0)
        .unwrap_or(24.0)
}

/// Emit a `def SkelAnimation "<name>" { ... }` prim (§1.3)
/// reconstructed from one typed
/// [`Animation`](oxideav_mesh3d::Animation):
///
/// * `joints` — token per animated joint node (first-appearance
///   channel order), rebuilt from the skeleton carriers' joint
///   trees.
/// * `translations` / `rotations` / `scales` `.timeSamples` — one
///   per-joint array per keyframe, timeCodes = seconds x the
///   layer's `timeCodesPerSecond`. Rotations convert xyzw →
///   USD's `(w, x, y, z)` quatf literal. A joint lacking a channel
///   for an emitted property gets the identity default for that
///   slot.
/// * A *static* blend state (decoder stash
///   `usd:skelAnim:staticWeights` on the carrier — the source file
///   authored `blendShapeWeights` as a plain default, which landed
///   on `Node::weights`) re-emits in the same default-value form,
///   with each scalar refreshed from the live `Node::weights` of
///   the node the state was attached to (so a typed-model edit of
///   the override survives the round trip).
fn write_skel_animation_prim(
    w: &mut Out,
    scene: &Scene3D,
    safe_name: &str,
    anim_idx: usize,
    carrier_extras: &std::collections::HashMap<String, serde_json::Value>,
) {
    use oxideav_mesh3d::{AnimationProperty, AnimationValues};
    let Some(anim) = scene.animations.get(anim_idx) else {
        return;
    };

    // Token per joint node across every skeleton carrier.
    let mut tokens_by_node: std::collections::HashMap<u32, String> =
        std::collections::HashMap::new();
    for node in &scene.nodes {
        if node.extras.contains_key("usd:skeleton") {
            for &root in &node.children {
                assign_joint_tokens(scene, root, "", &mut tokens_by_node);
            }
        }
    }

    // Animated joints in first-appearance channel order.
    let mut joints: Vec<NodeId> = Vec::new();
    for ch in &anim.channels {
        if matches!(
            ch.target.property,
            AnimationProperty::Translation | AnimationProperty::Rotation | AnimationProperty::Scale
        ) && !joints.contains(&ch.target.node)
        {
            joints.push(ch.target.node);
        }
    }

    w.write_indent();
    writeln!(w.s, "def SkelAnimation \"{safe_name}\" {{").unwrap();
    w.indent += 1;

    w.write_indent();
    write!(w.s, "uniform token[] joints = [").unwrap();
    for (i, joint) in joints.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        let fallback = format!("joint_{}", joint.0);
        let token = tokens_by_node
            .get(&joint.0)
            .map(String::as_str)
            .unwrap_or(&fallback);
        write!(w.s, "\"{token}\"").unwrap();
    }
    writeln!(w.s, "]").unwrap();

    let tcps = time_codes_per_second(scene);
    let channel_for = |node: NodeId, property: AnimationProperty| {
        anim.channels
            .iter()
            .find(|ch| ch.target.node == node && ch.target.property == property)
    };
    // Keyframe timeline: the decoder produces parallel samplers, so
    // the first TRS channel's keyframes are the shared timeline.
    let timeline: Vec<f32> = anim
        .channels
        .iter()
        .find(|ch| {
            matches!(
                ch.target.property,
                AnimationProperty::Translation
                    | AnimationProperty::Rotation
                    | AnimationProperty::Scale
            )
        })
        .map(|ch| ch.sampler.keyframes.clone())
        .unwrap_or_default();

    for (attr_name, type_token, property, default) in [
        (
            "translations",
            "float3[]",
            AnimationProperty::Translation,
            [0.0f32, 0.0, 0.0],
        ),
        (
            "scales",
            "half3[]",
            AnimationProperty::Scale,
            [1.0, 1.0, 1.0],
        ),
    ] {
        if !joints.iter().any(|&j| channel_for(j, property).is_some()) {
            continue;
        }
        w.write_indent();
        write!(w.s, "{type_token} {attr_name}.timeSamples = {{").unwrap();
        for (k, t) in timeline.iter().enumerate() {
            if k > 0 {
                w.s.push(',');
            }
            write!(w.s, " {}: [", format_float((*t as f64) * tcps)).unwrap();
            for (i, &joint) in joints.iter().enumerate() {
                if i > 0 {
                    w.s.push_str(", ");
                }
                let v = channel_for(joint, property)
                    .and_then(|ch| match &ch.sampler.values {
                        AnimationValues::Vec3(vals) => vals.get(k).copied(),
                        _ => None,
                    })
                    .unwrap_or(default);
                write!(
                    w.s,
                    "({}, {}, {})",
                    format_float(v[0] as f64),
                    format_float(v[1] as f64),
                    format_float(v[2] as f64)
                )
                .unwrap();
            }
            w.s.push(']');
        }
        writeln!(w.s, " }}").unwrap();
    }

    if joints
        .iter()
        .any(|&j| channel_for(j, AnimationProperty::Rotation).is_some())
    {
        w.write_indent();
        write!(w.s, "quatf[] rotations.timeSamples = {{").unwrap();
        for (k, t) in timeline.iter().enumerate() {
            if k > 0 {
                w.s.push(',');
            }
            write!(w.s, " {}: [", format_float((*t as f64) * tcps)).unwrap();
            for (i, &joint) in joints.iter().enumerate() {
                if i > 0 {
                    w.s.push_str(", ");
                }
                let q = channel_for(joint, AnimationProperty::Rotation)
                    .and_then(|ch| match &ch.sampler.values {
                        AnimationValues::Quat(vals) => vals.get(k).copied(),
                        _ => None,
                    })
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                // Internal xyzw → USD's (w, x, y, z) literal.
                write!(
                    w.s,
                    "({}, {}, {}, {})",
                    format_float(q[3] as f64),
                    format_float(q[0] as f64),
                    format_float(q[1] as f64),
                    format_float(q[2] as f64)
                )
                .unwrap();
            }
            w.s.push(']');
        }
        writeln!(w.s, " }}").unwrap();
    }

    // §1.3 blend-shape weights: a MorphWeights channel re-emits as
    // the `blendShapes` channel-name roster (the target mesh's typed
    // `Mesh::target_names`, `shape_<i>` for unnamed slots) + the
    // per-keyframe `blendShapeWeights` map, read back losslessly
    // through `morph_weight_frames` (the stored per-keyframe
    // vectors; a CubicSpline sampler contributes its centre values —
    // USD timeSamples carry no tangents).
    if let Some(morph_ch) = anim
        .channels
        .iter()
        .find(|ch| ch.target.property == AnimationProperty::MorphWeights)
    {
        let names: Vec<String> = scene
            .node(morph_ch.target.node)
            .and_then(|n| n.mesh)
            .and_then(|mid| scene.meshes.get(mid.0 as usize))
            .and_then(|m| m.primitives.first().map(|p| (m, p)))
            .map(|(m, p)| blend_channel_names(p, &m.target_names))
            .unwrap_or_default();
        if let (Some(stride), Some(frames)) = (
            morph_ch.sampler.morph_weight_stride(),
            morph_ch.sampler.morph_weight_frames(),
        ) {
            // A sampler wider than the mesh roster (typed-model
            // authored without matching targets) still emits every
            // slot under a `shape_<i>` fallback name.
            let names: Vec<String> = (0..stride)
                .map(|i| {
                    names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("shape_{i}"))
                })
                .collect();
            w.write_indent();
            write!(w.s, "uniform token[] blendShapes = [").unwrap();
            for (i, name) in names.iter().enumerate() {
                if i > 0 {
                    w.s.push_str(", ");
                }
                write!(w.s, "\"{name}\"").unwrap();
            }
            writeln!(w.s, "]").unwrap();
            w.write_indent();
            write!(w.s, "float[] blendShapeWeights.timeSamples = {{").unwrap();
            for (k, (t, frame)) in morph_ch.sampler.keyframes.iter().zip(frames).enumerate() {
                if k > 0 {
                    w.s.push(',');
                }
                write!(w.s, " {}: [", format_float((*t as f64) * tcps)).unwrap();
                for (i, v) in frame.iter().enumerate() {
                    if i > 0 {
                        w.s.push_str(", ");
                    }
                    write!(w.s, "{}", format_float(*v as f64)).unwrap();
                }
                w.s.push(']');
            }
            writeln!(w.s, " }}").unwrap();
        }
    }

    // Static blend state (default-value `blendShapeWeights`): replay
    // the authored roster from the carrier stash, refreshing each
    // scalar from the live `Node::weights` of the node the decoder
    // attached the state to (one scalar per channel — the typed
    // model holds §1.4.1 inbetweens on the target, not the weight).
    if let Some(stash) = carrier_extras
        .get("usd:skelAnim:staticWeights")
        .and_then(|v| v.as_object())
    {
        let names: Vec<String> = stash
            .get("names")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let stashed: Vec<f32> = stash
            .get("weights")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect()
            })
            .unwrap_or_default();
        // Live values from the marked node, keyed by channel name.
        let mut live: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
        for node in &scene.nodes {
            if node
                .extras
                .get("usd:skel:weightsAnim")
                .and_then(|v| v.as_u64())
                != Some(anim_idx as u64)
            {
                continue;
            }
            if let Some((mesh, prim)) = node
                .mesh
                .and_then(|m| scene.meshes.get(m.0 as usize))
                .and_then(|m| m.primitives.first().map(|p| (m, p)))
            {
                for (name, scalar) in blend_channel_names(prim, &mesh.target_names)
                    .into_iter()
                    .zip(padded_weights(&node.weights, prim.targets.len()))
                {
                    live.entry(name).or_insert(scalar);
                }
            }
        }
        if !names.is_empty() {
            let weights: Vec<f32> = names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    live.get(n.as_str())
                        .copied()
                        .or_else(|| stashed.get(i).copied())
                        .unwrap_or(0.0)
                })
                .collect();
            write_static_blend_weights(w, &names, &weights);
        }
    }

    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();
}

/// Deterministic prim name of the root-level `def SkelAnimation`
/// synthesized for a typed-model animation (index `idx`) that drives
/// `MorphWeights` but has no decoder carrier node.
fn synth_blend_anim_name(idx: usize) -> String {
    format!("BlendAnim_{idx}")
}

/// Synthesize the root-level `def SkelAnimation` prims a typed-model
/// scene needs for its morph state:
///
/// * one `BlendAnim_<idx>` per animation carrying a sampled
///   `MorphWeights` channel (e.g. built through
///   `AnimationSampler::morph_weights`) with no decoder carrier
///   node — the `blendShapes` / `blendShapeWeights.timeSamples`
///   table, bound from the geometry via `skel:animationSource`;
/// * one `BlendState_<id>` per reachable node that holds a static
///   morph state ([`node_static_morph_state`]) with no existing
///   carrier — the counterpart of the geometry prim's
///   `rel skel:animationSource = </BlendState_<id>>` authored by
///   [`write_one_mesh_prim`].
///
/// Channel names come from the same roster the geometry's
/// `skel:blendShapes` uses, scalars straight from `Node::weights`
/// (zero-padded to the target count), so the decoder reattaches
/// exactly this state (scoped by the relationship even when several
/// nodes share one mesh with divergent overrides).
fn write_synth_blend_states(w: &mut Out, scene: &Scene3D) {
    fn collect(scene: &Scene3D, id: NodeId, out: &mut Vec<NodeId>) {
        let Some(node) = scene.node(id) else { return };
        out.push(id);
        for &child in &node.children {
            collect(scene, child, out);
        }
    }
    for (idx, anim) in scene.animations.iter().enumerate() {
        let drives_morph = anim
            .channels
            .iter()
            .any(|ch| ch.target.property == oxideav_mesh3d::AnimationProperty::MorphWeights);
        if !drives_morph || marker_prim_path(scene, "usd:skelAnimation", idx as u64).is_some() {
            continue;
        }
        write_skel_animation_prim(
            w,
            scene,
            &synth_blend_anim_name(idx),
            idx,
            &std::collections::HashMap::new(),
        );
    }
    let mut ids: Vec<NodeId> = Vec::new();
    for &root in &scene.roots {
        collect(scene, root, &mut ids);
    }
    for id in ids {
        let Some(node) = scene.node(id) else { continue };
        if !node_static_morph_state(scene, node)
            || existing_blend_anim_path(scene, id, node).is_some()
        {
            continue;
        }
        let Some((mesh, prim)) = node
            .mesh
            .and_then(|m| scene.mesh(m))
            .and_then(|m| m.primitives.first().map(|p| (m, p)))
        else {
            continue;
        };
        let names = blend_channel_names(prim, &mesh.target_names);
        let scalars = padded_weights(&node.weights, prim.targets.len());
        w.write_indent();
        writeln!(w.s, "def SkelAnimation \"BlendState_{}\" {{", id.0).unwrap();
        w.indent += 1;
        write_static_blend_weights(w, &names, &scalars);
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, "}}").unwrap();
    }
}

/// Emit the §1.3 static (default-value) blend-weight pair:
/// `uniform token[] blendShapes` + a non-sampled
/// `float[] blendShapeWeights`.
fn write_static_blend_weights(w: &mut Out, names: &[String], weights: &[f32]) {
    w.write_indent();
    write!(w.s, "uniform token[] blendShapes = [").unwrap();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(w.s, "\"{name}\"").unwrap();
    }
    writeln!(w.s, "]").unwrap();
    w.write_indent();
    write!(w.s, "float[] blendShapeWeights = [").unwrap();
    for (i, wt) in weights.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        write!(w.s, "{}", format_float(*wt as f64)).unwrap();
    }
    writeln!(w.s, "]").unwrap();
}

/// `Node::weights` sized to the mesh's morph-target roster: one
/// scalar per channel, missing tail entries read as `0` (glTF's
/// "absent override = zero weight"), surplus entries dropped.
fn padded_weights(weights: &[f32], n_targets: usize) -> Vec<f32> {
    (0..n_targets)
        .map(|i| weights.get(i).copied().unwrap_or(0.0))
        .collect()
}

/// DFS from a joint node assigning slash-joined name-path tokens —
/// shared by the Skeleton and SkelAnimation writers.
fn assign_joint_tokens(
    scene: &Scene3D,
    id: NodeId,
    prefix: &str,
    out: &mut std::collections::HashMap<u32, String>,
) {
    let Some(n) = scene.node(id) else { return };
    let name = sanitize_prim_name(n.name.as_deref().unwrap_or("joint"));
    let token = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    };
    for &child in &n.children {
        assign_joint_tokens(scene, child, &token, out);
    }
    out.insert(id.0, token);
}

/// Emit a `def Skeleton "<name>" { ... }` prim (§1.2) reconstructed
/// from the typed model:
///
/// * `joints` — the token array rebuilt from the joint nodes' names
///   walked from the carrier (`Root`, `Root/Hip`, ...), in
///   `Skeleton::joints` order (the canonical joint index space).
/// * `bindTransforms` — the inverse of each
///   `inverse_bind_matrices` entry (world-space bind pose).
/// * `restTransforms` — each joint node's local transform.
/// * `jointNames` — replayed from the decoder's extras stash when
///   present.
fn write_skeleton_prim(
    w: &mut Out,
    scene: &Scene3D,
    node: &oxideav_mesh3d::Node,
    safe_name: &str,
    skel_idx: usize,
) {
    let Some(skeleton) = scene.skeletons.get(skel_idx) else {
        return;
    };
    // Token per joint: DFS from the carrier's children building
    // slash-joined name paths.
    let mut tokens_by_node: std::collections::HashMap<u32, String> =
        std::collections::HashMap::new();
    for &root in &node.children {
        assign_joint_tokens(scene, root, "", &mut tokens_by_node);
    }

    w.write_indent();
    writeln!(w.s, "def Skeleton \"{safe_name}\" {{").unwrap();
    w.indent += 1;

    w.write_indent();
    write!(w.s, "uniform token[] joints = [").unwrap();
    for (i, joint) in skeleton.joints.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        let fallback = format!("joint_{}", joint.0);
        let token = tokens_by_node
            .get(&joint.0)
            .map(String::as_str)
            .unwrap_or(&fallback);
        write!(w.s, "\"{token}\"").unwrap();
    }
    writeln!(w.s, "]").unwrap();

    if let Some(names) = node
        .extras
        .get("usd:skel:jointNames")
        .and_then(|v| v.as_array())
    {
        w.write_indent();
        write!(w.s, "uniform token[] jointNames = [").unwrap();
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                w.s.push_str(", ");
            }
            write!(w.s, "\"{}\"", n.as_str().unwrap_or("")).unwrap();
        }
        writeln!(w.s, "]").unwrap();
    }

    w.write_indent();
    write!(w.s, "uniform matrix4d[] bindTransforms = [").unwrap();
    for (i, ibm) in skeleton.inverse_bind_matrices.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        // Typed column-vector inverse-bind → bind, then into the USD
        // row-vector literal layout.
        let bind = crate::usd_to_scene::transpose4(crate::usd_to_scene::invert_matrix4(*ibm));
        write!(w.s, "{}", format_matrix4(bind)).unwrap();
    }
    writeln!(w.s, "]").unwrap();

    w.write_indent();
    write!(w.s, "uniform matrix4d[] restTransforms = [").unwrap();
    for (i, joint) in skeleton.joints.iter().enumerate() {
        if i > 0 {
            w.s.push_str(", ");
        }
        let m = scene
            .node(*joint)
            .map(|n| crate::usd_to_scene::transform_to_matrix(&n.transform))
            .unwrap_or(crate::usd_to_scene::IDENTITY4);
        write!(w.s, "{}", format_matrix4(m)).unwrap();
    }
    writeln!(w.s, "]").unwrap();

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
        // The extras stash keeps the USD literal layout (row-vector);
        // the typed Transform is column-vector, so transpose here and
        // let `write_node_transform` transpose back — the replayed
        // literal is byte-identical to the authored one.
        return Some(Transform::Matrix(crate::usd_to_scene::transpose4(m)));
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

    write_material_binding(w, scene, prim.material, &prim.extras);
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

    write_material_binding(w, scene, prim.material, &prim.extras);
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

    // UV sets: the first is `primvars:st`, additional sets follow the
    // §2.5 multi-UV convention `primvars:st1`, `primvars:st2`, ...
    // (a `UsdPrimvarReader_float2` selects one by `varname`). Empty
    // sets (padding for authoring gaps) emit nothing.
    for (set_idx, uv_set) in prim.uvs.iter().enumerate() {
        if uv_set.is_empty() {
            continue;
        }
        let name = if set_idx == 0 {
            "primvars:st".to_string()
        } else {
            format!("primvars:st{set_idx}")
        };
        w.write_indent();
        write!(w.s, "texCoord2f[] {name} = [").unwrap();
        for (i, uv) in uv_set.iter().enumerate() {
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

    // §2.5 display primvars. A per-vertex colour set emits
    // `primvars:displayColor` (+ `displayOpacity` when any alpha
    // departs from 1); a constant/uniform set preserved on extras
    // re-emits the authored shape.
    if let Some(colors) = prim.colors.first() {
        w.write_indent();
        write!(w.s, "color3f[] primvars:displayColor = [").unwrap();
        for (i, c) in colors.iter().enumerate() {
            if i > 0 {
                w.s.push_str(", ");
            }
            write!(
                w.s,
                "({}, {}, {})",
                format_float(c[0] as f64),
                format_float(c[1] as f64),
                format_float(c[2] as f64)
            )
            .unwrap();
        }
        writeln!(w.s, "]").unwrap();
        if colors.iter().any(|c| c[3] != 1.0) {
            w.write_indent();
            write!(w.s, "float[] primvars:displayOpacity = [").unwrap();
            for (i, c) in colors.iter().enumerate() {
                if i > 0 {
                    w.s.push_str(", ");
                }
                write!(w.s, "{}", format_float(c[3] as f64)).unwrap();
            }
            writeln!(w.s, "]").unwrap();
        }
    } else if let Some(dc) = prim
        .extras
        .get("usd:displayColor")
        .and_then(|v| v.as_array())
    {
        w.write_indent();
        write!(w.s, "color3f[] primvars:displayColor = [").unwrap();
        for (i, c) in dc.iter().enumerate() {
            if i > 0 {
                w.s.push_str(", ");
            }
            let comp = |j: usize| c.get(j).and_then(|x| x.as_f64()).unwrap_or(0.0);
            write!(
                w.s,
                "({}, {}, {})",
                format_float(comp(0)),
                format_float(comp(1)),
                format_float(comp(2))
            )
            .unwrap();
        }
        writeln!(w.s, "]").unwrap();
        if let Some(op) = prim
            .extras
            .get("usd:displayOpacity")
            .and_then(|v| v.as_array())
        {
            w.write_indent();
            write!(w.s, "float[] primvars:displayOpacity = [").unwrap();
            for (i, x) in op.iter().enumerate() {
                if i > 0 {
                    w.s.push_str(", ");
                }
                write!(w.s, "{}", format_float(x.as_f64().unwrap_or(1.0))).unwrap();
            }
            writeln!(w.s, "]").unwrap();
        }
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

    // §2.1 expanded inputs — alpha coverage, specular workflow,
    // clearcoat lobe, IOR, occlusion multiplier, and the no-typed-slot
    // constants preserved on extras.
    if let AlphaMode::Mask { cutoff } = mat.alpha_mode {
        w.write_indent();
        writeln!(
            w.s,
            "float inputs:opacityThreshold = {}",
            format_float(cutoff as f64)
        )
        .unwrap();
    }
    // Specular workflow — the typed slot's presence IS the workflow
    // selector. The F0 color re-emits unless it equals the schema
    // default black (an authored default collapses to absence, which
    // evaluates identically); the color map is connected further
    // below with the other texture connections. An inert
    // `specularColor` (workflow off) re-emits from extras.
    if let Some(spec) = &mat.ext.specular {
        w.write_indent();
        writeln!(w.s, "int inputs:useSpecularWorkflow = 1").unwrap();
        if spec.color_factor != [0.0, 0.0, 0.0] {
            let c = spec.color_factor;
            w.write_indent();
            writeln!(
                w.s,
                "color3f inputs:specularColor = ({}, {}, {})",
                format_float(c[0] as f64),
                format_float(c[1] as f64),
                format_float(c[2] as f64)
            )
            .unwrap();
        }
    } else if let Some(sc) = mat
        .extras
        .get("usd:inputs:specularColor")
        .and_then(|v| v.as_array())
    {
        if sc.len() == 3 {
            let c = |i: usize| sc[i].as_f64().unwrap_or(0.0);
            w.write_indent();
            writeln!(
                w.s,
                "color3f inputs:specularColor = ({}, {}, {})",
                format_float(c(0)),
                format_float(c(1)),
                format_float(c(2))
            )
            .unwrap();
        }
    }
    // Index of refraction — the typed `MaterialExt::ior` slot is
    // `Option`-shaped, so `Some` is exactly "the source authored an
    // opinion" and re-emits verbatim (including an explicit 1.5).
    if let Some(ior) = mat.ext.ior {
        w.write_indent();
        writeln!(w.s, "float inputs:ior = {}", format_float(ior as f64)).unwrap();
    }
    // Clearcoat lobe — re-emit from the typed slot. Values equal to
    // the schema §2.1 defaults (`clearcoat` 0, `clearcoatRoughness`
    // 0.01) are skipped so an unauthored input never materialises a
    // synthetic opinion in the output; an authored default collapses
    // to absence, which evaluates identically.
    if let Some(cc) = &mat.ext.clearcoat {
        if cc.factor != 0.0 {
            w.write_indent();
            writeln!(
                w.s,
                "float inputs:clearcoat = {}",
                format_float(cc.factor as f64)
            )
            .unwrap();
        }
        if cc.roughness != 0.01 {
            w.write_indent();
            writeln!(
                w.s,
                "float inputs:clearcoatRoughness = {}",
                format_float(cc.roughness as f64)
            )
            .unwrap();
        }
    }
    for input in ["displacement"] {
        if let Some(f) = mat
            .extras
            .get(&format!("usd:inputs:{input}"))
            .and_then(|v| v.as_f64())
        {
            w.write_indent();
            writeln!(w.s, "float inputs:{input} = {}", format_float(f)).unwrap();
        }
    }
    if mat.occlusion_strength != 1.0 {
        w.write_indent();
        writeln!(
            w.s,
            "float inputs:occlusion = {}",
            format_float(mat.occlusion_strength as f64)
        )
        .unwrap();
    }
    if let Some(nrm) = mat
        .extras
        .get("usd:inputs:normal")
        .and_then(|v| v.as_array())
    {
        if nrm.len() == 3 {
            let c = |i: usize| nrm[i].as_f64().unwrap_or(0.0);
            w.write_indent();
            writeln!(
                w.s,
                "normal3f inputs:normal = ({}, {}, {})",
                format_float(c(0)),
                format_float(c(1)),
                format_float(c(2))
            )
            .unwrap();
        }
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
    // Packed metallic/roughness slot → re-emit exactly the inputs the
    // decoder recorded on `usd:mr_connect` ("both" when the source
    // wired one texture into both inputs; default "both" for scenes
    // authored directly through the typed model). Channel wiring
    // follows the packed-map convention the typed slot documents:
    // roughness = G, metallic = B.
    if let Some(tref) = mat.metallic_roughness_texture {
        let which = mat
            .extras
            .get("usd:mr_connect")
            .and_then(|v| v.as_str())
            .unwrap_or("both");
        if which == "metallic" || which == "both" {
            write_tex_connect(w, &mat_path, "metallic", tref, "b");
        }
        if which == "roughness" || which == "both" {
            write_tex_connect(w, &mat_path, "roughness", tref, "g");
        }
    }
    // Typed specular F0-color map (RGB channels).
    if let Some(tref) = mat.ext.specular.as_ref().and_then(|s| s.color_texture) {
        write_tex_connect(w, &mat_path, "specularColor", tref, "rgb");
    }
    // Typed clearcoat texture connections (factor = R, roughness = G,
    // the channels the packed-map documentation on the typed slots
    // records).
    if let Some(cc) = &mat.ext.clearcoat {
        if let Some(tref) = cc.factor_texture {
            write_tex_connect(w, &mat_path, "clearcoat", tref, "r");
        }
        if let Some(tref) = cc.roughness_texture {
            write_tex_connect(w, &mat_path, "clearcoatRoughness", tref, "g");
        }
    }
    // No-typed-slot texture inputs preserved on extras
    // (`usd:tex:<input>` → {"texture": N, "uv_set": M}).
    for (input, channel) in EXTRAS_TEX_INPUTS {
        if let Some(tref) = texref_from_extras(&mat.extras, input) {
            write_tex_connect(w, &mat_path, input, tref, channel);
        }
    }
    // §2.3 UsdPrimvarReader-driven inputs preserved on extras
    // (`usd:primvar:<input>`): connect each input to the reader prim
    // emitted after the surface shader. Sorted for deterministic
    // output (extras is a HashMap).
    let mut primvar_inputs: Vec<(&str, &serde_json::Map<String, serde_json::Value>)> = mat
        .extras
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("usd:primvar:")?, v.as_object()?)))
        .collect();
    primvar_inputs.sort_by_key(|(input, _)| *input);
    for (input, _) in &primvar_inputs {
        w.write_indent();
        writeln!(
            w.s,
            "{} inputs:{input}.connect = <{mat_path}/PrimvarReader_{input}.outputs:result>",
            type_for_slot(input)
        )
        .unwrap();
    }
    w.write_indent();
    writeln!(w.s, "token outputs:surface").unwrap();
    w.indent -= 1;
    w.write_indent();
    writeln!(w.s, "}}").unwrap();

    // One UsdPrimvarReader_<T> shader child per reader-driven input
    // (§2.3): variant type, `varname` (authored type spelling), and
    // the `fallback` literal replay verbatim from the decoder's
    // stash.
    for (input, stash) in &primvar_inputs {
        let variant = stash
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("float2");
        w.write_indent();
        writeln!(w.s, "def Shader \"PrimvarReader_{input}\" {{").unwrap();
        w.indent += 1;
        w.write_indent();
        writeln!(
            w.s,
            "uniform token info:id = \"UsdPrimvarReader_{variant}\""
        )
        .unwrap();
        if let Some(vn) = stash.get("varname").and_then(|v| v.as_str()) {
            let vn_type = stash
                .get("varname_type")
                .and_then(|v| v.as_str())
                .unwrap_or("string");
            w.write_indent();
            writeln!(w.s, "{vn_type} inputs:varname = \"{vn}\"").unwrap();
        }
        if let Some(fb) = stash.get("fallback").and_then(|v| v.as_str()) {
            let fb_type = stash
                .get("fallback_type")
                .and_then(|v| v.as_str())
                .unwrap_or(variant);
            w.write_indent();
            writeln!(w.s, "{fb_type} inputs:fallback = {fb}").unwrap();
        }
        w.write_indent();
        writeln!(
            w.s,
            "{} outputs:result",
            primvar_reader_result_type(variant)
        )
        .unwrap();
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, "}}").unwrap();
    }

    // One UsdUVTexture child per bound texture (deduped on TextureId).
    // Only slots this writer actually connects participate — typed
    // extension slots USD cannot express (sheen, volume, …) must not
    // materialise an orphan UsdUVTexture prim with no connection.
    let ext_texrefs = [
        mat.ext.specular.as_ref().and_then(|s| s.color_texture),
        mat.ext.clearcoat.as_ref().and_then(|c| c.factor_texture),
        mat.ext.clearcoat.as_ref().and_then(|c| c.roughness_texture),
    ];
    let extras_texrefs: Vec<Option<TextureRef>> = EXTRAS_TEX_INPUTS
        .iter()
        .map(|(input, _)| texref_from_extras(&mat.extras, input))
        .collect();
    let mut emitted = std::collections::BTreeSet::new();
    for tref in [
        mat.base_color_texture,
        mat.normal_texture,
        mat.emissive_texture,
        mat.occlusion_texture,
        mat.metallic_roughness_texture,
    ]
    .into_iter()
    .chain(ext_texrefs)
    .chain(extras_texrefs)
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
        // §2.2 no-typed-slot inputs preserved by the decoder on the
        // scene-level stash (wrapS/wrapT exact tokens, scale, bias,
        // fallback, sourceColorSpace, non-standard varname).
        let stash = scene
            .extras
            .get(&format!("usd:uvtexture:{}", tref.texture.0))
            .and_then(|v| v.as_object());
        let stash_str = |key: &str| stash.and_then(|s| s.get(key)).and_then(|v| v.as_str());
        w.write_indent();
        writeln!(w.s, "def Shader \"{shader_name}\" {{").unwrap();
        w.indent += 1;
        w.write_indent();
        writeln!(w.s, "uniform token info:id = \"UsdUVTexture\"").unwrap();
        w.write_indent();
        writeln!(w.s, "asset inputs:file = @{asset_name}@").unwrap();
        // Wrap modes: the stash carries the authored spelling
        // (including `black` / `useMetadata`, which have no typed
        // Sampler equivalent); a typed-model-only scene falls back to
        // the Sampler mapping, skipping the schema-default-adjacent
        // `repeat`.
        for (key, sampler_wrap) in [("wrapS", tex.sampler.wrap_s), ("wrapT", tex.sampler.wrap_t)] {
            if let Some(tok) = stash_str(key) {
                w.write_indent();
                writeln!(w.s, "token inputs:{key} = \"{tok}\"").unwrap();
            } else {
                let tok = match sampler_wrap {
                    oxideav_mesh3d::WrapMode::ClampToEdge => Some("clamp"),
                    oxideav_mesh3d::WrapMode::MirroredRepeat => Some("mirror"),
                    oxideav_mesh3d::WrapMode::Repeat => None,
                };
                if let Some(tok) = tok {
                    w.write_indent();
                    writeln!(w.s, "token inputs:{key} = \"{tok}\"").unwrap();
                }
            }
        }
        for key in ["scale", "bias", "fallback"] {
            if let Some(v4) = stash.and_then(|s| s.get(key)).and_then(|v| v.as_array()) {
                if v4.len() == 4 {
                    let c = |i: usize| v4[i].as_f64().unwrap_or(0.0);
                    w.write_indent();
                    writeln!(
                        w.s,
                        "float4 inputs:{key} = ({}, {}, {}, {})",
                        format_float(c(0)),
                        format_float(c(1)),
                        format_float(c(2)),
                        format_float(c(3))
                    )
                    .unwrap();
                }
            }
        }
        if let Some(cs) = stash_str("sourceColorSpace") {
            w.write_indent();
            writeln!(w.s, "token inputs:sourceColorSpace = \"{cs}\"").unwrap();
        }
        // §2.3 UsdPrimvarReader wiring: a texture sampling a UV set
        // other than `st` (or a non-standard primvar name) gets an
        // explicit `st` connection to a reader sibling emitted below.
        // `effective_uv_set` resolves a lingering `texCoord`
        // override (post-bake references carry none, but a caller
        // may hand `write_layer` an unbaked material directly).
        let uv_set = tref.effective_uv_set();
        let varname = stash_str("varname")
            .map(str::to_owned)
            .or_else(|| (uv_set > 0).then(|| format!("st{uv_set}")));
        if varname.is_some() {
            w.write_indent();
            writeln!(
                w.s,
                "float2 inputs:st.connect = <{mat_path}/{shader_name}_stReader.outputs:result>"
            )
            .unwrap();
        }
        w.write_indent();
        writeln!(w.s, "float3 outputs:rgb").unwrap();
        w.write_indent();
        writeln!(w.s, "float outputs:r").unwrap();
        w.indent -= 1;
        w.write_indent();
        writeln!(w.s, "}}").unwrap();

        if let Some(varname) = varname {
            w.write_indent();
            writeln!(w.s, "def Shader \"{shader_name}_stReader\" {{").unwrap();
            w.indent += 1;
            w.write_indent();
            writeln!(w.s, "uniform token info:id = \"UsdPrimvarReader_float2\"").unwrap();
            w.write_indent();
            writeln!(w.s, "string inputs:varname = \"{varname}\"").unwrap();
            w.write_indent();
            writeln!(w.s, "float2 outputs:result").unwrap();
            w.indent -= 1;
            w.write_indent();
            writeln!(w.s, "}}").unwrap();
        }
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
        "diffuseColor" | "emissiveColor" | "specularColor" => "color3f",
        "normal" => "normal3f",
        "occlusion" | "metallic" | "roughness" | "clearcoat" | "clearcoatRoughness" | "opacity"
        | "displacement" => "float",
        _ => "color3f",
    }
}

/// The `outputs:result` element type token for each §2.3
/// `UsdPrimvarReader_<T>` typed variant (schema table: variant
/// suffix → `result` type).
fn primvar_reader_result_type(variant: &str) -> &'static str {
    match variant {
        "float" => "float",
        "float2" => "float2",
        "float3" => "float3",
        "float4" => "float4",
        "int" => "int",
        "string" => "string",
        "normal" => "normal3f",
        "point" => "point3f",
        "vector" => "vector3f",
        "matrix" => "matrix4d",
        _ => "float2",
    }
}

/// Shader inputs whose texture connection has no typed model slot;
/// each round-trips through `Material::extras["usd:tex:<input>"]`
/// with the listed output channel.
const EXTRAS_TEX_INPUTS: [(&str, &str); 4] = [
    ("opacity", "a"),
    ("displacement", "r"),
    ("roughness", "g"),
    ("specularColor", "rgb"),
];

/// Decode a `usd:tex:<input>` extras entry back into a [`TextureRef`]
/// (`{"texture": N, "uv_set": M}` — the shape the decoder stashes for
/// shader inputs without a typed texture slot).
fn texref_from_extras(
    extras: &std::collections::HashMap<String, serde_json::Value>,
    input: &str,
) -> Option<TextureRef> {
    let v = extras.get(&format!("usd:tex:{input}"))?;
    let texture = v.get("texture")?.as_u64()? as u32;
    let uv_set = v.get("uv_set").and_then(|u| u.as_u64()).unwrap_or(0) as u32;
    Some(TextureRef {
        texture: oxideav_mesh3d::TextureId(texture),
        uv_set,
        // The stash shape is decoder-authored and the decoder never
        // attaches a UV transform (no 2D-transform node schema is
        // staged), so the replayed reference carries none either.
        transform: None,
    })
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
pub(crate) fn format_metadata_value(value: &Value) -> String {
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
        Value::TimeSamples(samples) => {
            format_time_samples(samples, |v| Some(format_metadata_value(v)))
        }
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
        Value::TimeSamples(_) => "double",
        Value::Dict(_) => "dictionary",
        Value::ListOp(list) => list
            .entries()
            .next()
            .map(|(_, v)| guess_usda_type(v))
            .unwrap_or("token"),
        Value::Raw(_) | Value::None => "token",
    }
}

/// Escape a quoted string for emission inside `"..."` USDA syntax,
/// per the §16.2.5 `Escaped` production: the named single-character
/// escapes for the C0 controls that have one, `\xHH` for the rest,
/// and backslash + double-quote; other bytes stay verbatim so
/// non-ASCII names round-trip.
fn escape_usda_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            '\x0B' => out.push_str("\\v"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
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
    // §16.2.5 Number: the non-finite spellings are `inf`, `-inf`,
    // and `nan` (Rust's default `NaN` is not valid USDA).
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
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
    fn escape_functions_emit_spec_escape_set() {
        // §16.2.5 Escaped: named single-character escapes for the C0
        // controls that have one, \xHH for the rest.
        let raw = "bell\x07 back\x08 page\x0C vert\x0B tab\t nl\n esc\x1b q\"b\\";
        let escaped = escape_quoted(raw);
        assert_eq!(
            escaped,
            "bell\\a back\\b page\\f vert\\v tab\\t nl\\n esc\\x1b q\\\"b\\\\"
        );
        assert_eq!(escape_usda_string(raw), escaped);
        // And the parser decodes the emission back to the raw bytes.
        let src = format!("#usda 1.0\ndef Scope \"S\" {{\n    string a = \"{escaped}\"\n}}\n");
        let layer = crate::usda::parse(src.as_bytes()).expect("parse escaped emission");
        let crate::usda::Value::String(back) = &layer.prims[0].attrs["a"].value else {
            panic!("string value");
        };
        assert_eq!(back, raw, "writer escapes and parser escapes are inverses");
    }

    #[test]
    fn format_float_spells_non_finite_values_per_spec() {
        assert_eq!(format_float(f64::INFINITY), "inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-inf");
        assert_eq!(format_float(f64::NAN), "nan");
        assert_eq!(format_float(1.0), "1");
    }

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
        // Typed column-vector convention: translation in the last
        // *column*. The emitted USD `matrix4d` literal is the
        // row-vector transpose — translation in the last row.
        let m = [
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 20.0],
            [0.0, 0.0, 1.0, 30.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        write_node_transform(&mut w, &Transform::Matrix(m));
        assert!(w.s.contains("matrix4d xformOp:transform = ("));
        assert!(
            w.s.contains("(1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (10, 20, 30, 1)"),
            "translation lands in the literal's last row: {}",
            w.s
        );
        assert!(w.s.contains("xformOpOrder = [\"xformOp:transform\"]"));
    }
}
