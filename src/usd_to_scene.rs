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
///
/// **Variant composition (round 7).** Before the material index +
/// node-build passes run, the prim tree is transformed by
/// recursively applying [`Prim::resolved_variants`](crate::usda::Prim::resolved_variants):
/// every prim with `metadata["variants"] = { string SET = "VAR" }`
/// has the corresponding `variant_sets[SET][VAR]`'s attrs +
/// children composed in (under local opinions per LIVRPS strength
/// order). The downstream `index_materials` + `build_node` passes
/// then see the composed view, so a `def Material` declared inside
/// a selected variant participates in `material:binding`
/// resolution and a `def Mesh` declared inside a selected variant
/// becomes a Scene3D Mesh + Node just like a directly-declared
/// child would.
///
/// Variant `references = @...@` and `payload = @...@` opinions
/// are NOT followed (round 7 is single-layer evaluation only) —
/// they're stashed on the resolved prim's metadata under
/// `usd:variantReferences:<set>:<var>:references` so the extras
/// stash surfaces the unresolved arc to the caller.
pub fn translate(layer: &Layer, archive: Arc<Vec<u8>>, entries: &[ZipEntry]) -> Result<Scene3D> {
    let entry_map: HashMap<String, ZipEntry> = entries
        .iter()
        .map(|e| (e.name.clone(), e.clone()))
        .collect();

    let mut ctx = Ctx {
        scene: Scene3D::new(),
        archive: archive.clone(),
        entries: entry_map.clone(),
        materials_by_path: HashMap::new(),
        textures_by_path: HashMap::new(),
        skeletons_by_path: HashMap::new(),
        skins_by_skeleton: HashMap::new(),
        time_codes_per_second: 24.0,
        blendshapes_by_path: HashMap::new(),
        pending_blend_weights: Vec::new(),
        binding_levels: Vec::new(),
        collections: HashMap::new(),
    };

    apply_layer_metadata(&mut ctx.scene, &layer.metadata);
    // SkelAnimation timeCodes → seconds conversion factor. USD's
    // layer default when unauthored is 24 timeCodes per second.
    if let Some(tcps) = layer
        .metadata
        .get("timeCodesPerSecond")
        .or_else(|| layer.metadata.get("framesPerSecond"))
        .and_then(|v| v.as_f32())
    {
        if tcps > 0.0 {
            ctx.time_codes_per_second = tcps as f64;
        }
    }

    // LayerStack composition (round 10): gather every in-archive
    // sublayer recursively, then merge their prim trees underneath
    // the local layer's prims per the OpenUSD glossary's LayerStack
    // semantics:
    //
    //   "LayerStack: The ordered set of layers resulting from the
    //    recursive gathering of all SubLayers of a Layer, plus the
    //    layer itself as first and strongest."
    //
    // Local opinions beat sublayer opinions; each subLayers entry is
    // weaker than the one before it (USD-doc order = strength order).
    // Cross-package + external sublayers (anything we can't find in
    // the surrounding ZIP entries) are skipped silently — they stay
    // as opinions on `scene.extras["usd:layerMetadata"]` so a writer
    // round-trip keeps them, and the caller can resolve them through
    // a higher-level pipeline (the asset-resolver pre-resolve step a
    // full USD runtime would perform before composition).
    let mut composed_paths: Vec<String> = Vec::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged_layer = layer.clone();
    compose_sublayers(
        &mut merged_layer,
        &archive,
        &entry_map,
        /* anchor_layer */ None,
        &mut visited,
        &mut composed_paths,
    )?;
    if !composed_paths.is_empty() {
        let names: Vec<serde_json::Value> = composed_paths
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        ctx.scene.extras.insert(
            "usd:composedSubLayers".into(),
            serde_json::Value::Array(names),
        );
    }

    // Inherits composition (round 13, LIVRPS "I" — stronger than
    // references / payload). `inherits` targets an in-layer prim path
    // (`</Class/Foo>`) and contributes its opinions to the inheriting
    // prim. Composed BEFORE references so a referenced opinion can't
    // overwrite an inherited one (LIVRPS: L > I > V > R > P > S).
    let inherits_snapshot = merged_layer.prims.clone();
    let mut composed_inherits: Vec<String> = Vec::new();
    for prim in &mut merged_layer.prims {
        compose_class_arcs(
            prim,
            "inherits",
            &inherits_snapshot,
            &mut composed_inherits,
            &mut std::collections::HashSet::new(),
        );
    }
    if !composed_inherits.is_empty() {
        let names: Vec<serde_json::Value> = composed_inherits
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        ctx.scene.extras.insert(
            "usd:composedInherits".into(),
            serde_json::Value::Array(names),
        );
    }

    // Reference / payload composition (round 11): walk every prim in
    // the composed LayerStack and, for any prim carrying a
    // `references` / `payload` opinion that targets an in-archive
    // `.usd` / `.usda` layer, compose the targeted prim underneath
    // the referencing prim per the OpenUSD glossary's References
    // semantics (the referencing prim keeps its name; the target's
    // type / kind / children / attrs ride up; local opinions win).
    // The consumed arc keys are stripped so the writer flattens the
    // composed tree into a single layer (mirroring the round-10
    // sublayer-flatten-on-write policy). Unresolved / external arcs
    // are left in place so the writer round-trips them verbatim.
    let mut composed_refs: Vec<String> = Vec::new();
    let mut ref_layer_cache: HashMap<String, Layer> = HashMap::new();
    for prim in &mut merged_layer.prims {
        compose_references(
            prim,
            &archive,
            &entry_map,
            /* anchor_layer */ None,
            &mut ref_layer_cache,
            &mut composed_refs,
            &mut std::collections::HashSet::new(),
        )?;
    }
    if !composed_refs.is_empty() {
        let names: Vec<serde_json::Value> = composed_refs
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        ctx.scene.extras.insert(
            "usd:composedReferences".into(),
            serde_json::Value::Array(names),
        );
    }

    // Specializes composition (round 13, LIVRPS "S" — weakest arc).
    // `specializes` targets an in-layer prim path (`</Class/Foo>`)
    // and contributes its opinions to the specialising prim as
    // refinable defaults. Composed AFTER references / payload so an
    // earlier-strength arc has already filled in opinions; only what
    // remains missing gets filled from the specialised target.
    let specializes_snapshot = merged_layer.prims.clone();
    let mut composed_specializes: Vec<String> = Vec::new();
    for prim in &mut merged_layer.prims {
        compose_class_arcs(
            prim,
            "specializes",
            &specializes_snapshot,
            &mut composed_specializes,
            &mut std::collections::HashSet::new(),
        );
    }
    if !composed_specializes.is_empty() {
        let names: Vec<serde_json::Value> = composed_specializes
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        ctx.scene.extras.insert(
            "usd:composedSpecializes".into(),
            serde_json::Value::Array(names),
        );
    }

    // Recursively compose variant selections so the rest of the
    // translator never has to think about variant_sets again.
    let mut resolved: Vec<Prim> = merged_layer
        .prims
        .iter()
        .map(resolve_variants_recursive)
        .collect();

    // UsdSkel BindingAPI inheritance (§1.5): `skel:skeleton`,
    // `skel:animationSource`, and `skel:joints` authored on an
    // ancestor apply to all descendants unless overridden. Push the
    // opinions down onto the geometry prims so the per-prim builders
    // read them locally.
    for prim in &mut resolved {
        propagate_skel_bindings(prim);
    }

    // CollectionAPI pre-pass (Core Specification §15): index every
    // `collection:<inst>:*` opinion by its property path so
    // `material:binding:collection:*` targets resolve during the
    // node walk regardless of declaration order.
    {
        let mut collections = HashMap::new();
        index_collections(&mut collections, "", &resolved);
        ctx.collections = collections;
    }

    // UsdSkel pre-pass: register every `def Skeleton` (joint nodes +
    // inverse bind matrices) so `rel skel:skeleton` bindings resolve
    // during the node walk regardless of declaration order.
    index_skeletons(&mut ctx, "", &resolved)?;

    // Walk the prim tree once, indexing every Material + Shader by
    // its absolute prim path. We resolve material bindings on a
    // second pass once every material is registered.
    index_materials(&mut ctx, "", &resolved)?;

    // Now build the node tree.
    for prim in &resolved {
        let node = build_node(&mut ctx, "", prim)?;
        if let Some(id) = node {
            ctx.scene.add_root(id);
        }
    }

    // §1.8: attach SkelAnimation blend-shape weight channels now
    // that every mesh's morph-target roster exists.
    attach_blend_weight_channels(&mut ctx);
    Ok(ctx.scene)
}

/// LayerStack composition (round 10).
///
/// Walks the local layer's `subLayers` metadata list — each entry is
/// a `Value::Asset` path — and for every path that resolves to an
/// in-archive `.usd` / `.usda` entry parses the sublayer, recurses
/// into its own sublayers (depth-first, strength-ordered), and
/// merges the result *underneath* the local layer's prims.
///
/// Sublayer paths are resolved against `anchor_layer`, the path of
/// the parent layer that contains the `subLayers` opinion — per the
/// USDZ spec §USD Constraints, anchored paths (`./foo.usd`) resolve
/// "to the layer-within-the-package in which the path is authored".
/// At the root call `anchor_layer = None` and we fall back to
/// resolving against the archive root.
///
/// Per the glossary LayerStack definition the local layer is "first
/// and strongest", so we keep local prims verbatim and merge each
/// sublayer's prims as `over`-style opinions: a same-named prim from
/// the sublayer contributes any attrs / metadata / children that the
/// local layer didn't author.
///
/// `visited` tracks layer paths already composed in the current
/// recursion to break sublayer cycles (`a.usd` sublayers `b.usd`
/// which sublayers `a.usd` again — invalid per USD but we should
/// not loop on it).
///
/// `.usdc` sublayer entries surface as `Error::Unsupported` because
/// the binary Crate parser is still deferred (docs gap — see
/// `docs/3d/usd/README.md`). External sublayers (anything not in
/// `entries`) are silently dropped from composition; the original
/// path stays on `scene.extras["usd:layerMetadata"]` so a writer
/// round-trip preserves the unresolved opinion.
fn compose_sublayers(
    target: &mut Layer,
    archive: &Arc<Vec<u8>>,
    entries: &HashMap<String, ZipEntry>,
    anchor_layer: Option<&str>,
    visited: &mut std::collections::HashSet<String>,
    composed_out: &mut Vec<String>,
) -> Result<()> {
    let raw = match listedited_items(target.metadata.get("subLayers")) {
        Some(items) => items,
        None => return Ok(()),
    };
    for item in &raw {
        let path = match item {
            Value::Asset(s) => s.clone(),
            Value::String(s) | Value::Token(s) => s.clone(),
            _ => continue,
        };
        let resolved = match resolve_archive_path(&path, anchor_layer, entries) {
            Some(p) => p,
            None => continue, // External / cross-package — silently skip.
        };
        if !visited.insert(resolved.clone()) {
            continue; // Cycle break.
        }
        let entry = match entries.get(&resolved) {
            Some(e) => e,
            None => continue,
        };
        let lower = resolved.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        if ext == "usdc" {
            return Err(unsupported(format!(
                "USDZ subLayer `{resolved}` is in `.usdc` (binary crate) format; \
                 binary USDC parsing is still gated on a docs-collaborator trace \
                 doc. Re-package the sublayer with `usdcat -o foo.usda`."
            )));
        }
        if ext != "usd" && ext != "usda" {
            continue;
        }
        let start = entry.payload_offset as usize;
        let end = start + entry.payload_len as usize;
        let bytes = &archive[start..end];
        let mut sub = crate::usda::parse(bytes)?;
        // Depth-first: a sublayer's own sublayers compose first, so
        // by the time we merge `sub` it is itself the LayerStack
        // rooted at that sublayer.
        compose_sublayers(
            &mut sub,
            archive,
            entries,
            Some(&resolved),
            visited,
            composed_out,
        )?;
        composed_out.push(resolved.clone());
        merge_layer_under(target, &sub);
    }
    Ok(())
}

/// Normalise a `subLayers` / `references` entry path against the
/// authoring layer's location inside the package.
///
/// Path-anchoring rules (USD relative-asset-path convention):
///
/// * `./foo.usd` — anchored to the authoring layer's directory.
/// * `foo.usd` (no leading `./` or `/`) — also anchored to the
///   authoring layer's directory; treated identically.
/// * Absolute archive path (`textures/diffuse.png`, no leading `/`)
///   — falls back to a lookup against the archive root if anchored
///   resolution misses.
/// * Anything containing `://`, `[`, or starting with `/` is
///   considered external / cross-package and returns `None`.
fn resolve_archive_path(
    path: &str,
    anchor_layer: Option<&str>,
    entries: &HashMap<String, ZipEntry>,
) -> Option<String> {
    if path.is_empty() || path.contains("://") || path.starts_with('/') || path.contains('[') {
        return None;
    }
    let stripped = path.strip_prefix("./").unwrap_or(path);
    if let Some(anchor) = anchor_layer {
        if let Some(slash) = anchor.rfind('/') {
            let dir = &anchor[..slash + 1];
            let candidate = format!("{dir}{stripped}");
            if entries.contains_key(&candidate) {
                return Some(candidate);
            }
        }
    }
    if entries.contains_key(stripped) {
        return Some(stripped.to_string());
    }
    None
}

/// Merge `weak` underneath `strong` per LayerStack strength order:
/// every key in `weak.metadata` that `strong` doesn't already author
/// rides up to `strong`; every root prim in `weak` either merges
/// into a same-named local prim (recursively) or is appended as a
/// new prim if no local prim shares the name.
fn merge_layer_under(strong: &mut Layer, weak: &Layer) {
    for (k, v) in &weak.metadata {
        // Skip `subLayers` — the sublayer's own subLayers have
        // already been folded in during the depth-first recursion;
        // re-inheriting them would loop.
        if k == "subLayers" {
            continue;
        }
        strong
            .metadata
            .entry(k.clone())
            .or_insert_with(|| v.clone());
    }
    for sub_prim in &weak.prims {
        if let Some(local) = strong.prims.iter_mut().find(|p| p.name == sub_prim.name) {
            merge_prim_under(local, sub_prim);
        } else {
            strong.prims.push(sub_prim.clone());
        }
    }
}

/// Merge `weak` prim into `strong` prim. Local opinions win for
/// every metadata key, every attr key, and every same-named child.
/// Children present only in the weak prim are appended after the
/// local children (preserving local declaration order).
///
/// **Specifier upgrade.** `over` is a non-defining specifier; `def`
/// is the defining one. If the local layer authored an `over` opinion
/// on a prim that the sublayer defines via `def`, the merged prim
/// inherits the `def` specifier — the prim IS defined (by the
/// sublayer) and the local layer is merely overriding opinions. The
/// same upgrade applies to `class` only when the strong side was
/// empty; otherwise we keep the strong specifier (since `class`
/// declares a class hierarchy that `def` shouldn't silently shadow).
fn merge_prim_under(strong: &mut Prim, weak: &Prim) {
    if weak.spec == "def" && strong.spec != "def" {
        strong.spec = "def".to_string();
    }
    // `type_name` from weak fills in only when strong's is empty (a
    // bare `over "Foo" {}` with no type spec inherits the sublayer's
    // typed declaration).
    if strong.type_name.is_empty() && !weak.type_name.is_empty() {
        strong.type_name = weak.type_name.clone();
    }
    for (k, v) in &weak.metadata {
        strong
            .metadata
            .entry(k.clone())
            .or_insert_with(|| v.clone());
    }
    for (k, v) in &weak.attrs {
        strong.attrs.entry(k.clone()).or_insert_with(|| v.clone());
    }
    for (k, v) in &weak.variant_sets {
        strong
            .variant_sets
            .entry(k.clone())
            .or_insert_with(|| v.clone());
    }
    for child in &weak.children {
        if let Some(local_child) = strong.children.iter_mut().find(|c| c.name == child.name) {
            merge_prim_under(local_child, child);
        } else {
            strong.children.push(child.clone());
        }
    }
}

/// Reference / payload composition (round 11).
///
/// Walks a single prim (recursing into its children first, so a
/// referenced layer that itself references is fully composed before
/// it merges upward), and for every `references` / `payload` opinion
/// the prim carries that targets an **in-archive** `.usd` / `.usda`
/// layer, composes the targeted prim underneath `prim` per the
/// OpenUSD glossary's References definition:
///
///   "the primary use for References is to compose smaller units of
///    scene description into larger aggregates … the prim name
///    [of the target] is gone, since the references allowed us to
///    perform a prim name-change on the prim targeted by the
///    reference."
///
/// So the referencing prim KEEPS its own name; the target prim's
/// `type_name`, metadata (`kind`, …), attrs, and children all ride
/// up under it via [`merge_prim_under`] (local opinions win, exactly
/// as the glossary's flattened `MarbleCollection` example shows the
/// local `xformOp:translate` and the `over "marble_geom"`
/// `displayColor` override beating the referenced asset's opinions).
///
/// **Target selection.** A `references = @file@</Prim>` /
/// `payload = @file@</Prim>` ([`Value::AssetWithPath`]) targets the
/// named prim inside the referenced layer. A bare `references =
/// @file@` ([`Value::Asset`]) targets the referenced layer's
/// `defaultPrim` (per the glossary `defaultPrim` definition: an
/// aggregating file `references = @asset.usd@` resolves against the
/// asset's `defaultPrim`). A layer with neither an explicit selector
/// nor a `defaultPrim` is skipped (nothing to compose).
///
/// **Strength.** References are stronger than payloads ("Payloads
/// are weaker than references"), so when a prim carries both we
/// compose `references` first; an earlier list entry is stronger
/// than a later one (USD list order = strength order), so we compose
/// them weakest-last using [`merge_prim_under`]'s local-wins merge.
///
/// **Flatten-on-write.** Consumed in-archive arcs are stripped from
/// `prim.metadata` so the writer emits the flattened result rather
/// than re-authoring a now-already-composed `references` opinion
/// (mirrors the round-10 sublayer flatten-on-write policy). External
/// / cross-package / `.usdc` targets stay in `metadata` untouched so
/// the writer round-trips them verbatim, and the unresolved arc
/// surfaces to the caller through the layer-metadata stash.
///
/// `.usd` / `.usda` targets compose; a `.usdc` target raises
/// [`Error::Unsupported`](crate::Error) consistent with the
/// round-1 Default-Layer + round-10 sublayer policy (the binary
/// Crate parser is still a documented docs gap).
///
/// `visited` breaks reference cycles (`a.usd</A>` references
/// `b.usd</B>` which references `a.usd</A>` again).
fn compose_references(
    prim: &mut Prim,
    archive: &Arc<Vec<u8>>,
    entries: &HashMap<String, ZipEntry>,
    anchor_layer: Option<&str>,
    cache: &mut HashMap<String, Layer>,
    composed_out: &mut Vec<String>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<()> {
    // Compose children's own arcs first (depth-first).
    for child in &mut prim.children {
        compose_references(
            child,
            archive,
            entries,
            anchor_layer,
            cache,
            composed_out,
            visited,
        )?;
    }

    // Gather the prim's arcs in strength order: every `references`
    // entry (strongest-first) followed by every `payload` entry.
    // We collect the resolved targets first, then merge them
    // weakest-last so the local prim's own opinions stay strongest.
    let ref_arcs = collect_arc_targets(prim.metadata.get("references"));
    let payload_arcs = collect_arc_targets(prim.metadata.get("payload"));
    let had_ref_arcs = !ref_arcs.is_empty();
    let had_payload_arcs = !payload_arcs.is_empty();

    // Track which keys we actually consumed so unresolved/external
    // arcs stay authored for the writer. A key is only stripped when
    // it had composable arcs AND *every* one of them composed (a
    // partly-external list keeps the whole opinion so the writer
    // round-trips the unresolved members).
    let mut any_consumed = false;
    let mut consumed_all_references = true;
    let mut consumed_all_payload = true;

    // Strength order: references[0] strongest … references[n] …
    // payload[0] … payload[m] weakest. We merge strongest-first into
    // `prim`: `prim` already holds the LOCAL opinions (strongest of
    // all), and [`merge_prim_under`] only fills in keys / children the
    // accumulator lacks, so each arc loses to the local prim and to
    // every stronger arc composed before it.
    let strength_ordered: Vec<(&str, ArcTarget)> = ref_arcs
        .into_iter()
        .map(|t| ("references", t))
        .chain(payload_arcs.into_iter().map(|t| ("payload", t)))
        .collect();

    for (kind, target) in strength_ordered {
        let resolved_layer_path = match resolve_archive_path(&target.asset, anchor_layer, entries) {
            Some(p) => p,
            None => {
                // External / cross-package — leave the arc
                // authored so the writer round-trips it.
                if kind == "references" {
                    consumed_all_references = false;
                } else {
                    consumed_all_payload = false;
                }
                continue;
            }
        };
        let lower = resolved_layer_path.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        if ext == "usdc" {
            return Err(unsupported(format!(
                "USDZ {kind} `{resolved_layer_path}` is in `.usdc` (binary crate) \
                 format; binary USDC parsing is still gated on a docs-collaborator \
                 trace doc. Re-package the referenced layer with `usdcat -o foo.usda`."
            )));
        }
        if ext != "usd" && ext != "usda" {
            if kind == "references" {
                consumed_all_references = false;
            } else {
                consumed_all_payload = false;
            }
            continue;
        }

        // Cycle guard keyed on (layer-path, in-asset prim path).
        let cycle_key = format!("{resolved_layer_path}|{}", target.prim_path);
        if !visited.insert(cycle_key.clone()) {
            // Already composing this exact target up the stack — drop
            // it to avoid infinite recursion. Mark consumed so the
            // writer doesn't re-author a cycle.
            any_consumed = true;
            continue;
        }

        let ref_layer = load_layer_cached(&resolved_layer_path, archive, entries, cache)?;
        // Pick the target prim: explicit `</Prim>` selector, else
        // the layer's `defaultPrim`.
        let Some(mut target_prim) = select_target_prim(&ref_layer, &target.prim_path).cloned()
        else {
            // Nothing to compose (no selector + no defaultPrim, or the
            // named prim is absent). Leave the arc authored; surface
            // nothing.
            visited.remove(&cycle_key);
            if kind == "references" {
                consumed_all_references = false;
            } else {
                consumed_all_payload = false;
            }
            continue;
        };

        // The target may itself carry references/payloads — compose
        // them against the referenced layer's own location (so its
        // anchored sub-references resolve correctly).
        compose_references(
            &mut target_prim,
            archive,
            entries,
            Some(&resolved_layer_path),
            cache,
            composed_out,
            visited,
        )?;

        merge_prim_under(prim, &target_prim);
        composed_out.push(format!("{resolved_layer_path}|{kind}"));
        any_consumed = true;
        visited.remove(&cycle_key);
    }

    if any_consumed {
        if had_ref_arcs && consumed_all_references {
            prim.metadata.remove("references");
        }
        if had_payload_arcs && consumed_all_payload {
            prim.metadata.remove("payload");
        }
    }
    Ok(())
}

/// Class-hierarchy composition (round 13).
///
/// Resolves `inherits = </Class/Foo>` / `specializes = </Class/Foo>`
/// arcs in a single in-layer pass. Both arcs target a prim path
/// within the *same* layer (per the OpenUSD glossary's Inherits /
/// Specializes definitions — they're "internal" composition arcs,
/// distinct from `references` / `payload` which target external
/// asset paths).
///
/// **Strength.** Per LIVRPS strength order — L > I > V > R > P > S
/// — `inherits` is stronger than every external-asset arc; only the
/// local prim's own opinions and a stronger variant selection beat
/// inherited opinions. `specializes` is the weakest arc; every
/// stronger opinion (Local, Inherits, Variants, References,
/// Payload) has already had its chance to fill in the prim by the
/// time specializes composes. The caller arranges this by invoking
/// `compose_class_arcs(prim, "inherits", ...)` BEFORE
/// [`compose_references`] and `compose_class_arcs(prim,
/// "specializes", ...)` AFTER it.
///
/// **List ordering.** Within a multi-target `inherits = [</A>, </B>]`
/// list, the earlier entry is stronger (USD list order = strength
/// order). We compose them strongest-first into `prim`, so each
/// later target fills only what's still missing.
///
/// **Targeting.** A `Value::Path` (`</A/B>`) or a `Value::Array` of
/// paths is followed; any other shape (asset paths, raw strings)
/// is left in place — that's not a valid class-arc value and the
/// writer should keep authoring whatever was originally there.
///
/// **Cycle break.** `visited` carries the prim paths already being
/// composed up the recursion stack so a class-arc cycle (`/A`
/// inherits `</B>` which inherits `</A>`) terminates instead of
/// looping. The visited path uses the root prim's name as the
/// anchor; recursive descent into children appends `/child`.
///
/// **Flatten-on-write.** A consumed in-layer arc is stripped from
/// `prim.metadata` so the writer flattens the composed tree
/// matching the round-10 sublayer / round-11 reference policy.
/// External arcs (paths that don't resolve to any prim in the
/// local layer's snapshot) stay authored so the writer round-trips
/// them verbatim.
///
/// **Audit trail.** Each composed `(target-prim-path, kind)` lands
/// in `composed_out` formatted as `"<prim-path>|<kind>"` so the
/// caller can stash it on `Scene3D::extras["usd:composedInherits"]`
/// / `"usd:composedSpecializes"` mirroring the references audit
/// shape.
fn compose_class_arcs(
    prim: &mut Prim,
    kind: &str,
    layer_snapshot: &[Prim],
    composed_out: &mut Vec<String>,
    visited: &mut std::collections::HashSet<String>,
) {
    // Compose this prim's class arcs before descending into children
    // for `inherits` (stronger fills the prim before child-level
    // weaker arcs get a chance to look at the inherited shape) and
    // after children for `specializes` semantics doesn't actually
    // change observable behaviour at this layer because each prim's
    // own arcs are independent — but matching `compose_references`
    // shape keeps the call sites symmetric.
    let targets = collect_class_arc_targets(prim.metadata.get(kind));
    let had_targets = !targets.is_empty();
    let mut any_consumed = false;
    let mut consumed_all = true;

    for target_path in &targets {
        // Cycle guard keyed on the target prim path. An arc that
        // names a prim we're already in the middle of composing is
        // skipped to terminate; the consumed flag still flips so the
        // writer doesn't re-author the cycle.
        let normalised = target_path.trim_start_matches('/').to_string();
        if !visited.insert(normalised.clone()) {
            any_consumed = true;
            continue;
        }
        match resolve_in_layer_path(layer_snapshot, target_path) {
            Some(target) => {
                // The target may itself carry class arcs that resolve
                // against the same layer; compose them so the merged
                // shape under `prim` is already the fully-composed
                // target tree.
                let mut composed_target = target.clone();
                compose_class_arcs(
                    &mut composed_target,
                    kind,
                    layer_snapshot,
                    composed_out,
                    visited,
                );
                merge_prim_under(prim, &composed_target);
                composed_out.push(format!("{target_path}|{kind}"));
                any_consumed = true;
            }
            None => {
                // External / cross-layer class-arc target — leave
                // the opinion authored so the writer round-trips it.
                consumed_all = false;
            }
        }
        visited.remove(&normalised);
    }

    if any_consumed && had_targets && consumed_all {
        prim.metadata.remove(kind);
    }

    // Recurse into children so a nested prim's own class arcs also
    // compose. The children walk happens after the prim's own arcs
    // are merged in so a freshly-composed child (one that rode up
    // from the inherited target) can be reached.
    for child in &mut prim.children {
        compose_class_arcs(child, kind, layer_snapshot, composed_out, visited);
    }
}

/// Flatten an `inherits` / `specializes` metadata value into an
/// ordered list of in-layer prim paths. A scalar [`Value::Path`]
/// yields one entry; an [`Value::Array`] of `Path`s yields one per
/// element (USD list order = strength order). Any other variant
/// (asset paths, raw strings, empty) yields nothing.
fn collect_class_arc_targets(v: Option<&Value>) -> Vec<String> {
    fn one(v: &Value) -> Option<String> {
        match v {
            Value::Path(p) if !p.is_empty() => Some(p.clone()),
            _ => None,
        }
    }
    fn flat(v: &Value) -> Vec<String> {
        match v {
            Value::Array(items) => items.iter().filter_map(one).collect(),
            other => one(other).into_iter().collect(),
        }
    }
    match v {
        // `inherits` / `specializes` are list-edited too: flatten the
        // additive sublists in strength order, then drop deleted paths.
        Some(Value::ListOp(list)) => {
            let mut out: Vec<String> = Vec::new();
            for (_, sublist) in list.additive_in_strength_order() {
                out.extend(flat(sublist));
            }
            if let Some(del) = list.deleted.as_ref() {
                let deleted = flat(del);
                out.retain(|p| !deleted.contains(p));
            }
            out
        }
        Some(other) => flat(other),
        None => Vec::new(),
    }
}

/// Walk a prim path (`</A/B/C>`) against a flat list of root prims
/// (the layer snapshot) and return the matching prim. Returns
/// `None` when any path segment is absent or the path is empty.
fn resolve_in_layer_path<'a>(roots: &'a [Prim], path: &str) -> Option<&'a Prim> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let mut segments = trimmed.split('/');
    let first = segments.next()?;
    let mut cur = roots.iter().find(|p| p.name == first)?;
    for seg in segments {
        cur = cur.children.iter().find(|c| c.name == seg)?;
    }
    Some(cur)
}

/// Flatten a list-edited array-valued metadata field (e.g.
/// `subLayers`) to its effective ordered items: the additive sublists
/// in strength order with any `delete`-authored items removed. A bare
/// `Array` value passes through; a non-array / absent value yields
/// `None`.
fn listedited_items(v: Option<&Value>) -> Option<Vec<Value>> {
    fn as_items(v: &Value) -> Vec<Value> {
        match v {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        }
    }
    match v {
        Some(Value::Array(items)) => Some(items.clone()),
        Some(Value::ListOp(list)) => {
            let mut out: Vec<Value> = Vec::new();
            for (_, sublist) in list.additive_in_strength_order() {
                out.extend(as_items(sublist));
            }
            if let Some(del) = list.deleted.as_ref() {
                let deleted = as_items(del);
                out.retain(|item| {
                    !deleted
                        .iter()
                        .any(|d| d.as_text().is_some() && d.as_text() == item.as_text())
                });
            }
            Some(out)
        }
        _ => None,
    }
}

/// A single resolved composition-arc target.
struct ArcTarget {
    /// `@...@` asset path portion.
    asset: String,
    /// `</...>` in-asset prim selector (empty when the arc targets
    /// the referenced layer's `defaultPrim`).
    prim_path: String,
}

/// Flatten a `references` / `payload` metadata value into an ordered
/// list of [`ArcTarget`]s, honouring USD list-edit semantics.
///
/// * A bare `Asset` / `AssetWithPath` (or an `Array` of them) yields
///   one target per entry, in authoring order (USD list order =
///   strength order).
/// * A [`Value::ListOp`] flattens its additive sublists in strength
///   order — *prepended* (strongest), then *explicit*, then *appended*
///   — and then removes any target named in the *deleted* sublist.
///   This makes `delete references = @x@` an actual removal rather
///   than the round-1 behaviour of treating every list-op the same.
fn collect_arc_targets(v: Option<&Value>) -> Vec<ArcTarget> {
    fn one(v: &Value) -> Option<ArcTarget> {
        match v {
            Value::Asset(a) => Some(ArcTarget {
                asset: a.clone(),
                prim_path: String::new(),
            }),
            Value::AssetWithPath { asset, prim_path } if !asset.is_empty() => Some(ArcTarget {
                asset: asset.clone(),
                prim_path: prim_path.clone(),
            }),
            _ => None,
        }
    }
    fn flat(v: &Value) -> Vec<ArcTarget> {
        match v {
            Value::Array(items) => items.iter().filter_map(one).collect(),
            other => one(other).into_iter().collect(),
        }
    }
    match v {
        Some(Value::ListOp(list)) => {
            let mut out: Vec<ArcTarget> = Vec::new();
            for (_, sublist) in list.additive_in_strength_order() {
                out.extend(flat(sublist));
            }
            // Remove any deleted targets (matched on asset + prim_path).
            if let Some(del) = list.deleted.as_ref() {
                let deleted = flat(del);
                out.retain(|t| {
                    !deleted
                        .iter()
                        .any(|d| d.asset == t.asset && d.prim_path == t.prim_path)
                });
            }
            out
        }
        Some(other) => flat(other),
        None => Vec::new(),
    }
}

/// Resolve `</A/B/C>` (or a bare `defaultPrim` lookup) to the target
/// prim inside a referenced layer.
///
/// * Non-empty `prim_path` — split on `/`, descend the prim tree
///   from the layer roots. Returns `None` if any segment is missing.
/// * Empty `prim_path` — look up the layer's `defaultPrim` token and
///   return the matching root prim. Returns `None` if the layer has
///   no `defaultPrim` or it names no root prim.
fn select_target_prim<'a>(layer: &'a Layer, prim_path: &str) -> Option<&'a Prim> {
    let trimmed = prim_path.trim_start_matches('/');
    if trimmed.is_empty() {
        let name = layer.metadata.get("defaultPrim")?.as_text()?;
        return layer.prims.iter().find(|p| p.name == name);
    }
    let mut segments = trimmed.split('/');
    let first = segments.next()?;
    let mut cur = layer.prims.iter().find(|p| p.name == first)?;
    for seg in segments {
        cur = cur.children.iter().find(|c| c.name == seg)?;
    }
    Some(cur)
}

/// Parse + cache an in-archive USDA layer by its archive entry name.
fn load_layer_cached(
    path: &str,
    archive: &Arc<Vec<u8>>,
    entries: &HashMap<String, ZipEntry>,
    cache: &mut HashMap<String, Layer>,
) -> Result<Layer> {
    if let Some(layer) = cache.get(path) {
        return Ok(layer.clone());
    }
    let entry = entries
        .get(path)
        .ok_or_else(|| invalid(format!("referenced layer `{path}` vanished from archive")))?;
    let start = entry.payload_offset as usize;
    let end = start + entry.payload_len as usize;
    let bytes = &archive[start..end];
    let layer = crate::usda::parse(bytes)?;
    cache.insert(path.to_string(), layer.clone());
    Ok(layer)
}

/// Apply [`Prim::resolved_variants`](crate::usda::Prim::resolved_variants)
/// at every depth of the prim tree, returning a new tree where
/// every prim's variant selection (if any) has been composed in.
fn resolve_variants_recursive(prim: &Prim) -> Prim {
    let mut composed = prim.resolved_variants();
    composed.children = composed
        .children
        .iter()
        .map(resolve_variants_recursive)
        .collect();
    composed
}

struct Ctx {
    scene: Scene3D,
    archive: Arc<Vec<u8>>,
    entries: HashMap<String, ZipEntry>,
    materials_by_path: HashMap<String, oxideav_mesh3d::MaterialId>,
    /// Cache so two materials referencing the same shader's texture
    /// share one `TextureId` instead of duplicating the asset.
    textures_by_path: HashMap<String, TextureRef>,
    /// Every `def Skeleton` indexed by absolute prim path — built by
    /// the pre-pass so `rel skel:skeleton = </path>` bindings resolve
    /// regardless of declaration order.
    skeletons_by_path: HashMap<String, SkelInfo>,
    /// One [`Skin`](oxideav_mesh3d::Skin) per bound skeleton, shared
    /// across every geometry node binding it.
    skins_by_skeleton: HashMap<u32, oxideav_mesh3d::SkinId>,
    /// Layer `timeCodesPerSecond` (fallback `framesPerSecond`,
    /// default 24) — maps SkelAnimation timeCodes onto the typed
    /// model's keyframe seconds.
    time_codes_per_second: f64,
    /// Every `def BlendShape` indexed by absolute prim path — built
    /// by the pre-pass so `rel skel:blendShapeTargets` resolves
    /// regardless of declaration order.
    blendshapes_by_path: HashMap<String, BlendShapeData>,
    /// SkelAnimation `blendShapes` / `blendShapeWeights` gathered
    /// during the node walk; a post-pass attaches the MorphWeights
    /// channels once every mesh (and its morph-target roster) is
    /// built.
    pending_blend_weights: Vec<PendingBlendWeights>,
    /// Direct `material:binding*` opinions of the container prims
    /// enclosing the prim currently being built, outermost first —
    /// pushed/popped around the `build_node` child recursion so a
    /// gprim can resolve §3.4 rule 1 (bindings inherit down
    /// namespace; `strongerThanDescendants` ancestors win).
    binding_levels: Vec<PrimBindingLevel>,
    /// Every CollectionAPI instance in the composed layer, keyed by
    /// its property path (`</prim/path.collection:instName>`) —
    /// built by a pre-pass so `material:binding:collection:*`
    /// targets resolve regardless of declaration order (AOUSD Core
    /// Specification §15).
    collections: HashMap<String, CollectionDef>,
}

/// §15.1.1.1 `expansionRule` allowed values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpansionRule {
    ExplicitOnly,
    ExpandPrims,
    ExpandPrimsAndProperties,
}

/// One CollectionAPI instance (§15.1): the includes / excludes
/// relationship targets plus the expansion knobs.
#[derive(Clone, Debug)]
struct CollectionDef {
    includes: Vec<String>,
    excludes: Vec<String>,
    expansion: ExpansionRule,
    include_root: bool,
}

impl Default for CollectionDef {
    fn default() -> Self {
        CollectionDef {
            includes: Vec::new(),
            excludes: Vec::new(),
            // §15.1.1.1 fallback.
            expansion: ExpansionRule::ExpandPrims,
            include_root: false,
        }
    }
}

/// Flatten a relationship value's target list (single path, array
/// of paths, or a list-edited opinion) into absolute path strings.
fn rel_target_paths(v: &Value) -> Vec<String> {
    match listedited_items(Some(v)) {
        Some(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Path(p) => Some(p.clone()),
                _ => None,
            })
            .collect(),
        None => match v {
            Value::Path(p) => vec![p.clone()],
            Value::Array(items) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Path(p) => Some(p.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
    }
}

/// Walk the composed prim tree once and index every CollectionAPI
/// instance (§15.1): `collection:<inst>:includes` / `:excludes`
/// relationships plus the `:expansionRule` / `:includeRoot`
/// attributes, keyed by `<prim path>.collection:<inst>`.
fn index_collections(map: &mut HashMap<String, CollectionDef>, parent: &str, prims: &[Prim]) {
    for prim in prims {
        let path = join_path(parent, &prim.name);
        let mut instances: BTreeMap<String, CollectionDef> = BTreeMap::new();
        for (name, attr) in &prim.attrs {
            let Some(rest) = name.strip_prefix("collection:") else {
                continue;
            };
            let Some((inst, field)) = rest.rsplit_once(':') else {
                continue;
            };
            if inst.is_empty() {
                continue;
            }
            let def = instances.entry(inst.to_string()).or_default();
            match field {
                "includes" => def.includes = rel_target_paths(&attr.value),
                "excludes" => def.excludes = rel_target_paths(&attr.value),
                "expansionRule" => {
                    def.expansion = match attr.value.as_text() {
                        Some("explicitOnly") => ExpansionRule::ExplicitOnly,
                        Some("expandPrimsAndProperties") => ExpansionRule::ExpandPrimsAndProperties,
                        // Fallback (and any unknown token): expandPrims.
                        _ => ExpansionRule::ExpandPrims,
                    }
                }
                "includeRoot" => {
                    def.include_root = matches!(&attr.value, Value::Bool(true))
                        || attr.value.as_f32().is_some_and(|f| f != 0.0)
                        || attr.value.as_text().is_some_and(|s| s == "true");
                }
                _ => {}
            }
        }
        for (inst, def) in instances {
            map.insert(format!("{path}.collection:{inst}"), def);
        }
        index_collections(map, &path, &prim.children);
    }
}

/// `true` when prim path `p` is `root` or a namespace descendant of
/// it. The absolute root `/` contains every prim.
fn at_or_under(p: &str, root: &str) -> bool {
    if root == "/" {
        return true;
    }
    p == root || (p.len() > root.len() && p.starts_with(root) && p.as_bytes()[root.len()] == b'/')
}

/// §15.2 membership test: does the collection at property path
/// `coll` contain the prim at `p`?
///
/// * `expandPrims` / `expandPrimsAndProperties`: an included prim
///   path contributes itself and all its descendants (for prim
///   membership the two rules agree — properties are not prims).
/// * `explicitOnly`: only the exact listed paths.
/// * `includeRoot` (§15.1.1.2): the absolute root — every prim.
/// * An include targeting another collection's property path
///   contributes that collection's members recursively (cycle
///   guarded).
/// * Excludes subtract at-or-under the excluded prim path; an
///   *orphaned* exclude — one that is not itself in the pre-exclude
///   included set — is inert per §15.2. Property-path excludes
///   cannot affect prim membership.
fn collection_contains(map: &HashMap<String, CollectionDef>, coll: &str, p: &str) -> bool {
    fn pre_member(
        map: &HashMap<String, CollectionDef>,
        def: &CollectionDef,
        p: &str,
        stack: &mut std::collections::HashSet<String>,
    ) -> bool {
        if def.include_root {
            return true;
        }
        for inc in &def.includes {
            if let Some(dot) = inc.find('.') {
                // Property-path include: only a `collection:` target
                // contributes prim members (recursively).
                if inc[dot + 1..].starts_with("collection:") && contains_inner(map, inc, p, stack) {
                    return true;
                }
                continue;
            }
            let hit = match def.expansion {
                ExpansionRule::ExplicitOnly => p == inc,
                _ => at_or_under(p, inc),
            };
            if hit {
                return true;
            }
        }
        false
    }

    fn contains_inner(
        map: &HashMap<String, CollectionDef>,
        coll: &str,
        p: &str,
        stack: &mut std::collections::HashSet<String>,
    ) -> bool {
        if !stack.insert(coll.to_string()) {
            // Include cycle — already being evaluated above us.
            return false;
        }
        let result = match map.get(coll) {
            Some(def) if pre_member(map, def, p, stack) => {
                !def.excludes.iter().any(|e| {
                    if e.contains('.') {
                        // §15.1.1.4: property excludes (which may
                        // not target collections) don't remove
                        // prim members.
                        return false;
                    }
                    let applies = match def.expansion {
                        ExpansionRule::ExplicitOnly => p == e,
                        _ => at_or_under(p, e),
                    };
                    // Orphaned excludes are inert.
                    applies && pre_member(map, def, e, stack)
                })
            }
            _ => false,
        };
        stack.remove(coll);
        result
    }

    contains_inner(map, coll, p, &mut std::collections::HashSet::new())
}

/// One direct material binding (staged schema §3.2 two/three-token
/// forms): the target `Material` path plus the §3.3
/// `bindMaterialAs` strength flag.
#[derive(Clone, Debug)]
struct DirectBinding {
    path: String,
    /// `bindMaterialAs = "strongerThanDescendants"` (§3.3; the
    /// unauthored default is `weakerThanDescendants`).
    stronger: bool,
}

/// One collection binding (§3.2 four/five-token forms): the
/// collection property path + material path pair, with purpose and
/// §3.3 strength.
#[derive(Clone, Debug)]
struct CollectionBinding {
    /// Full relationship name (`material:binding:collection:...`) —
    /// the §3.4 rule-5 ordering key.
    rel_name: String,
    /// `None` = all-purpose; `Some` for `preview` / `full` (other
    /// purpose tokens never bind the typed slot and aren't stored).
    purpose: Option<&'static str>,
    collection: String,
    material: String,
    stronger: bool,
}

/// The `material:binding*` opinions authored on one prim, split by
/// binding purpose (§3.1): the all-purpose fallback, the `preview`
/// restriction, and the `full` restriction, plus the collection
/// bindings in §3.4 rule-5 property order. Other (arbitrary)
/// purpose tokens never bind the typed model's single material
/// slot; they are preserved verbatim for round-trip.
#[derive(Clone, Debug, Default)]
struct PrimBindingLevel {
    all: Option<DirectBinding>,
    preview: Option<DirectBinding>,
    full: Option<DirectBinding>,
    /// Collection bindings in the normative property order — name-
    /// sorted (Core Specification §11.3.2 path-element ordering)
    /// with the prim's strongest authored `propertyOrder`
    /// (§7.6.2.4.6) applied: listed names first in listed order,
    /// absent ones after.
    collections: Vec<CollectionBinding>,
}

/// Read one prim's direct `material:binding*` relationships into a
/// [`PrimBindingLevel`]. §3.2: a direct binding has exactly one
/// target — a multi-target value is not a direct binding and is
/// left for the verbatim stash (a reader "should reject, not guess
/// at" malformed spellings; here the typed slot simply doesn't
/// bind, while the authored bytes survive).
fn read_binding_level(prim: &Prim) -> PrimBindingLevel {
    let mut level = PrimBindingLevel::default();
    for (name, attr) in &prim.attrs {
        let Some(rest) = name.strip_prefix("material:binding") else {
            continue;
        };
        let stronger = attr
            .metadata
            .get("bindMaterialAs")
            .and_then(|v| v.as_text())
            .map(|s| s == "strongerThanDescendants")
            .unwrap_or(false);
        // §3.2 four/five-token collection forms.
        if let Some(tail) = rest.strip_prefix(":collection:") {
            let purpose = match tail.split(':').collect::<Vec<_>>()[..] {
                [n] if !n.is_empty() => None,
                [p, n] if !p.is_empty() && !n.is_empty() => match p {
                    "preview" => Some("preview"),
                    "full" => Some("full"),
                    // Arbitrary purpose tokens never bind the
                    // typed slot; preserved verbatim only.
                    _ => continue,
                },
                // ≥ 6 tokens — not a §3.2 spelling.
                _ => continue,
            };
            // §3.2: exactly two targets — one collection property
            // path and one Material prim path. The pairing is
            // classified structurally (the collection target is the
            // one carrying `.collection:`), and anything else is
            // rejected for the typed slot (never guessed at) while
            // surviving verbatim in the stash.
            let targets = rel_target_paths(&attr.value);
            let [a, b] = &targets[..] else {
                continue;
            };
            let (collection, material) = if a.contains(".collection:") && !b.contains('.') {
                (a.clone(), b.clone())
            } else if b.contains(".collection:") && !a.contains('.') {
                (b.clone(), a.clone())
            } else {
                continue;
            };
            level.collections.push(CollectionBinding {
                rel_name: name.clone(),
                purpose,
                collection,
                material,
                stronger,
            });
            continue;
        }
        // §3.1/§3.2 direct forms: "" = all-purpose,
        // ":<purpose>" = purpose-restricted.
        let purpose = match rest {
            "" => None,
            _ => match rest.strip_prefix(':') {
                Some(p) if !p.is_empty() && !p.contains(':') => Some(p),
                _ => continue,
            },
        };
        let Some(path) = (match &attr.value {
            Value::Path(p) => Some(p.clone()),
            _ => None,
        }) else {
            continue;
        };
        let binding = DirectBinding { path, stronger };
        match purpose {
            None => level.all = Some(binding),
            Some("preview") => level.preview = Some(binding),
            Some("full") => level.full = Some(binding),
            // Arbitrary purpose tokens (§3.1: the purpose position
            // is not a closed enumeration) — preserved verbatim
            // only.
            Some(_) => {}
        }
    }
    // §3.4 rule 5: among collection bindings on one prim, native
    // property order decides — §11.3.2 pins that as name-sorted
    // (the BTreeMap walk above) reordered by the strongest authored
    // `propertyOrder` (§7.6.2.4.6: listed names first, in listed
    // order; absent names after).
    if let Some(order) = prim
        .metadata
        .get("propertyOrder")
        .and_then(|v| v.as_seq())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_text())
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
    {
        level.collections.sort_by_key(|cb| {
            order
                .iter()
                .position(|n| *n == cb.rel_name)
                .unwrap_or(usize::MAX)
        });
    }
    level
}

/// Resolve the binding chain `levels` (outermost ancestor first,
/// the gprim's own level last) for the gprim at absolute path `g`
/// into the winning `Material` path per the staged schema §3.4
/// ordered rules, for the typed model's single material slot:
///
/// * **Requested purpose.** The typed model carries one material
///   per primitive and this crate's material model is
///   `UsdPreviewSurface` — the *preview* shading schema — so
///   resolution requests `preview`: a `preview`-restricted opinion
///   is preferred over the all-purpose one within the same class
///   at the same prim (rule 3). When *no* preview or all-purpose
///   binding matches anywhere on the chain, the `full` restriction
///   resolves as a last resort so an asset authored only with
///   full-purpose bindings doesn't decode unbound; the authored
///   spelling always survives verbatim for round-trip.
/// * **Collections (rules 2, 4, 5, 6).** A collection binding
///   matches when its collection contains `g` (§15 membership;
///   rule 2's at-or-beneath-the-owner restriction is satisfied by
///   construction — every level *is* an ancestor of `g`). At one
///   prim, collection bindings beat direct bindings (rule 4);
///   among collection bindings, the §11.3.2 property order decides
///   (rule 5); membership is boolean, so namespace specificity
///   within a collection never enters (rule 6).
/// * **Namespace strength (rule 1).** The binding closest to the
///   leaf wins — unless an ancestor binding is marked
///   `strongerThanDescendants` (§3.3), in which case the
///   *outermost* such ancestor wins.
fn resolve_binding(
    levels: &[PrimBindingLevel],
    collections: &HashMap<String, CollectionDef>,
    g: &str,
) -> Option<String> {
    // One prim's ordered candidates for a requested restricted
    // purpose: collection-restricted, collection-all-purpose,
    // direct-restricted, direct-all-purpose (rule 4 ranks the
    // classes; rule 3 ranks purposes within each class).
    let match_at = |level: &PrimBindingLevel,
                    restricted: &str,
                    with_all: bool,
                    stronger_only: bool|
     -> Option<String> {
        for want in [Some(restricted), None] {
            if want.is_none() && !with_all {
                continue;
            }
            for cb in &level.collections {
                if cb.purpose == want
                    && (!stronger_only || cb.stronger)
                    && collection_contains(collections, &cb.collection, g)
                {
                    return Some(cb.material.clone());
                }
            }
        }
        let direct = if restricted == "preview" {
            &level.preview
        } else {
            &level.full
        };
        if let Some(b) = direct.as_ref().filter(|b| !stronger_only || b.stronger) {
            return Some(b.path.clone());
        }
        if with_all {
            if let Some(b) = level.all.as_ref().filter(|b| !stronger_only || b.stronger) {
                return Some(b.path.clone());
            }
        }
        None
    };
    for (restricted, with_all) in [("preview", true), ("full", false)] {
        // Outermost level carrying a strongerThanDescendants
        // matching binding wins over everything deeper.
        for level in levels {
            if let Some(m) = match_at(level, restricted, with_all, true) {
                return Some(m);
            }
        }
        // Otherwise the matching binding nearest the leaf.
        for level in levels.iter().rev() {
            if let Some(m) = match_at(level, restricted, with_all, false) {
                return Some(m);
            }
        }
    }
    None
}

/// Collect every authored `material:binding*` attribute on a prim —
/// the direct forms *and* the collection forms — for the verbatim
/// round-trip stash.
fn collect_binding_attrs(prim: &Prim) -> BTreeMap<String, Attr> {
    prim.attrs
        .iter()
        .filter(|(name, _)| {
            name.as_str() == "material:binding" || name.starts_with("material:binding:")
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Collect every authored CollectionAPI property (§15.1 —
/// `collection:<inst>:*`) on a prim for the verbatim round-trip
/// stash.
fn collect_collection_attrs(prim: &Prim) -> BTreeMap<String, Attr> {
    prim.attrs
        .iter()
        .filter(|(name, _)| name.starts_with("collection:"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `true` when the authored binding set is exactly the shape the
/// writer synthesises from `Primitive::material` — a single
/// all-purpose `material:binding` targeting `resolved`, with no
/// relationship metadata — so no verbatim stash is needed.
fn bindings_are_synthetic(authored: &BTreeMap<String, Attr>, resolved: Option<&str>) -> bool {
    if authored.is_empty() {
        return resolved.is_none();
    }
    if authored.len() != 1 {
        return false;
    }
    let Some(attr) = authored.get("material:binding") else {
        return false;
    };
    if !attr.metadata.is_empty() {
        return false;
    }
    match (&attr.value, resolved) {
        (Value::Path(p), Some(r)) => p == r,
        _ => false,
    }
}

/// Stash the authored `material:binding*` attributes as a shell
/// prim (the lossless tagged encoding from
/// [`crate::variant_codec`]) under `usd:materialBindings`, for the
/// writer to replay verbatim *instead of* synthesising a binding
/// from the typed material slot. An **empty** stash is meaningful:
/// it marks "this prim authored no binding — the typed slot was
/// resolved from an ancestor — emit nothing".
fn binding_stash_json(authored: &BTreeMap<String, Attr>) -> serde_json::Value {
    let shell = Prim {
        spec: String::new(),
        type_name: String::new(),
        name: String::new(),
        metadata: BTreeMap::new(),
        attrs: authored.clone(),
        children: Vec::new(),
        variant_sets: BTreeMap::new(),
    };
    crate::variant_codec::encode_prim(&shell)
}

/// One parsed `def BlendShape` (§1.4).
#[derive(Clone)]
struct BlendShapeData {
    /// Per-point position deltas (dense, or sparse via
    /// `point_indices`).
    offsets: Vec<[f32; 3]>,
    /// Optional per-point normal deltas, parallel to `offsets`.
    normal_offsets: Option<Vec<[f32; 3]>>,
    /// Sparse index map: `offsets[i]` applies to base point
    /// `point_indices[i]`. Dense when absent.
    point_indices: Option<Vec<u32>>,
    /// §1.4.1 inbetween shapes — valid ones only (weight strictly
    /// inside (0, 1) exclusive, no duplicate weights), sorted by
    /// weight ascending.
    inbetweens: Vec<InbetweenShape>,
    /// Malformed `inbetweens:*` attributes (§1.4.1 authoring
    /// errors: weight 0/1, duplicate weights, missing `weight`
    /// metadata, unparsable arrays) — ignored for evaluation per
    /// the documented error-but-continue behaviour, but preserved
    /// verbatim (a tagged shell prim) so the writer replays them.
    malformed_inbetweens: Option<serde_json::Value>,
}

/// One valid §1.4.1 inbetween shape.
#[derive(Clone)]
struct InbetweenShape {
    /// The `<name>` of the `inbetweens:<name>` attribute.
    name: String,
    /// The attribute's `weight` metadata field.
    weight: f32,
    /// Position deltas (governed by the prim-level `pointIndices`).
    offsets: Vec<[f32; 3]>,
    /// Optional normal deltas + the exact authored attribute name
    /// they were found under. Per §1.4.2 the spelling is not
    /// published, so it is *discovered by enumeration* (the first
    /// `inbetweens:<name>:*` attribute parsing as a vector3f array)
    /// and preserved for verbatim re-emission.
    normal_offsets: Option<(String, Vec<[f32; 3]>)>,
}

/// Blend-shape weight animation gathered from one SkelAnimation
/// (§1.3 `blendShapes` + `blendShapeWeights`), pending channel
/// attachment (§1.8 step 1 — match the channel names into each
/// bound geometry's `skel:blendShapes`).
struct PendingBlendWeights {
    anim_idx: usize,
    /// The SkelAnimation prim's own path — an authored (or §1.5
    /// inherited) `skel:animationSource` on bound geometry scopes
    /// attachment to exactly this animation.
    anim_path: String,
    channel_names: Vec<String>,
    /// Keyframe times in seconds.
    times: Vec<f32>,
    /// One weight per channel name, per keyframe.
    frames: Vec<Vec<f32>>,
    /// `true` when `blendShapeWeights` was authored as a plain
    /// default value (no `.timeSamples`) — a *static* blend state,
    /// which lands on `Node::weights` (the glTF `node.weights`
    /// override) instead of fabricating a one-keyframe animation
    /// channel.
    static_default: bool,
}

/// Pre-pass record for one `def Skeleton` (UsdSkel §1.2).
#[derive(Clone)]
struct SkelInfo {
    skeleton: oxideav_mesh3d::SkeletonId,
    /// The authored `joints` token array — its index order is the
    /// canonical joint index space (`jointIndices` + animations
    /// reference it).
    joint_tokens: Vec<String>,
    /// One scene-graph node per joint, same order as `joint_tokens`.
    /// The SkelAnimation channel decoder resolves each animated
    /// joint token to its node target through this table.
    joint_nodes: Vec<oxideav_mesh3d::NodeId>,
    /// The joints with no parent among `joint_tokens` — the carrier
    /// node's children.
    root_joints: Vec<oxideav_mesh3d::NodeId>,
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
    // Stash everything else verbatim for round-trip preservation.  Two
    // shapes ride here:
    //
    //   * The lossy, per-key untagged form (`usd:<key>` → JSON value)
    //     kept from r1 for back-compat with tools that read the keys
    //     directly without round-tripping the layer.
    //   * The lossless tagged blob (`usd:layerMetadata` → JSON object
    //     of every non-canonical key, preserving the [`Value`]
    //     discriminant via [`crate::variant_codec`]) added in r9 so
    //     [`crate::usda_writer::write_layer_metadata`] can re-emit the
    //     original `defaultPrim` / `subLayers` / `customLayerData` etc.
    //     declarations with their correct USDA type-token shape.
    let mut tagged = BTreeMap::new();
    for (k, v) in meta {
        if matches!(k.as_str(), "upAxis" | "metersPerUnit") {
            continue;
        }
        if let Some(json) = value_to_json(v) {
            scene.extras.insert(format!("usd:{k}"), json);
        }
        tagged.insert(k.clone(), v.clone());
    }
    if !tagged.is_empty() {
        let blob = crate::variant_codec::encode_btree_value(&tagged);
        scene
            .extras
            .insert(crate::usda_writer::LAYER_METADATA_EXTRAS_KEY.into(), blob);
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

/// UsdSkel BindingAPI inheritance (§1.5): `skel:skeleton` and
/// `skel:animationSource` are relationships that, when authored on an
/// ancestor (typically the `SkelRoot`), apply to all descendants
/// unless overridden; `skel:joints` is likewise "inherited
/// hierarchically". Copy each such opinion onto children lacking it
/// so the per-prim builders read them locally. `Skeleton` /
/// `SkelAnimation` / shading prims are skipped — the binding targets
/// skinnable geometry and scopes, not the rig itself.
fn propagate_skel_bindings(prim: &mut Prim) {
    const INHERITED: [&str; 3] = ["skel:skeleton", "skel:animationSource", "skel:joints"];
    let inherited: Vec<(String, Attr)> = INHERITED
        .iter()
        .filter_map(|k| prim.attrs.get(*k).map(|a| (k.to_string(), a.clone())))
        .collect();
    for child in &mut prim.children {
        if matches!(
            child.type_name.as_str(),
            "Skeleton" | "SkelAnimation" | "PackedJointAnimation" | "Material" | "Shader"
        ) {
            continue;
        }
        for (k, a) in &inherited {
            child.attrs.entry(k.clone()).or_insert_with(|| a.clone());
        }
        propagate_skel_bindings(child);
    }
}

/// UsdSkel pre-pass: walk every prim and materialise each
/// `def Skeleton` (§1.2) into scene resources — one joint
/// [`Node`](oxideav_mesh3d::Node) per `joints` token (tree topology
/// from the path-prefix rule, local rest transform from
/// `restTransforms`) plus a [`Skeleton`](oxideav_mesh3d::Skeleton)
/// whose `inverse_bind_matrices` are the inverted world-space
/// `bindTransforms`.
fn index_skeletons(ctx: &mut Ctx, parent: &str, prims: &[Prim]) -> Result<()> {
    for prim in prims {
        let path = join_path(parent, &prim.name);
        if prim.spec == "def" && prim.type_name == "Skeleton" {
            let info = build_skeleton(ctx, &path, prim)?;
            ctx.skeletons_by_path.insert(path.clone(), info);
        }
        // §1.4: BlendShape prims register alongside — their
        // `rel skel:blendShapeTargets` referrers resolve by path.
        if prim.spec == "def" && prim.type_name == "BlendShape" {
            if let Some(data) = read_blend_shape(prim) {
                ctx.blendshapes_by_path.insert(path.clone(), data);
            }
        }
        index_skeletons(ctx, &path, &prim.children)?;
    }
    Ok(())
}

/// Parse one `def BlendShape` prim's §1.4 attribute set.
fn read_blend_shape(prim: &Prim) -> Option<BlendShapeData> {
    let offsets = read_vec3_array(&prim.attrs.get("offsets")?.value)?;
    let normal_offsets = prim
        .attrs
        .get("normalOffsets")
        .and_then(|a| read_vec3_array(&a.value));
    let point_indices = prim
        .attrs
        .get("pointIndices")
        .and_then(|a| read_int_array(&a.value));

    // §1.4.1 inbetween shapes: each is a single
    // `uniform vector3f[] inbetweens:<name>` attribute whose target
    // weight is the attribute's `weight` **metadata** field. Deeper
    // `inbetweens:<name>:*` attributes are enumerated (never
    // constructed — §1.4.2) as that inbetween's normal offsets.
    let mut candidates: Vec<InbetweenShape> = Vec::new();
    let mut malformed: BTreeMap<String, Attr> = BTreeMap::new();
    for (name, attr) in &prim.attrs {
        let Some(shape_name) = name.strip_prefix("inbetweens:") else {
            continue;
        };
        if shape_name.contains(':') {
            // A deeper attribute — handled with its owning shape
            // below (or preserved with it when the shape itself is
            // malformed).
            continue;
        }
        let weight = attr.metadata.get("weight").and_then(|v| v.as_f32());
        let shape_offsets = read_vec3_array(&attr.value);
        // §1.4.1 authoring errors: weight 0 / 1 (the implicit
        // endpoints must not be authored), missing weight metadata,
        // or an unparsable array — error-but-continue, ignoring the
        // malformed inbetween (preserved verbatim below).
        let valid = matches!(weight, Some(w) if w != 0.0 && w != 1.0) && shape_offsets.is_some();
        if !valid {
            malformed.insert(name.clone(), attr.clone());
            for (deep_name, deep_attr) in &prim.attrs {
                if deep_name.starts_with(&format!("{name}:")) {
                    malformed.insert(deep_name.clone(), deep_attr.clone());
                }
            }
            continue;
        }
        let normal = prim
            .attrs
            .iter()
            .filter(|(n, _)| n.starts_with(&format!("{name}:")))
            .find_map(|(n, a)| read_vec3_array(&a.value).map(|arr| (n.clone(), arr)));
        candidates.push(InbetweenShape {
            name: shape_name.to_string(),
            weight: weight.expect("validated above"),
            offsets: shape_offsets.expect("validated above"),
            normal_offsets: normal,
        });
    }
    // §1.4.1: two inbetweens sharing a weight are an authoring
    // error — the documented behaviour ignores the malformed
    // inbetweens, so every shape participating in a collision is
    // dropped from evaluation (and preserved verbatim).
    let mut inbetweens: Vec<InbetweenShape> = Vec::new();
    for shape in &candidates {
        if candidates
            .iter()
            .filter(|o| o.weight == shape.weight)
            .count()
            > 1
        {
            let attr_name = format!("inbetweens:{}", shape.name);
            for (n, a) in &prim.attrs {
                if n == &attr_name || n.starts_with(&format!("{attr_name}:")) {
                    malformed.insert(n.clone(), a.clone());
                }
            }
        } else {
            inbetweens.push(shape.clone());
        }
    }
    inbetweens.sort_by(|a, b| a.weight.total_cmp(&b.weight));
    let malformed_inbetweens = if malformed.is_empty() {
        None
    } else {
        Some(binding_stash_json(&malformed))
    };
    Some(BlendShapeData {
        offsets,
        normal_offsets,
        point_indices,
        inbetweens,
        malformed_inbetweens,
    })
}

/// Materialise one `def Skeleton` prim (see [`index_skeletons`]).
fn build_skeleton(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<SkelInfo> {
    let joint_tokens: Vec<String> = prim
        .attrs
        .get("joints")
        .and_then(|a| a.value.as_seq())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_text().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if joint_tokens.is_empty() {
        return Err(invalid(format!(
            "Skeleton `{path}` is missing its `joints` token array"
        )));
    }
    let read_matrices = |name: &str| -> Option<Vec<[[f32; 4]; 4]>> {
        let seq = prim.attrs.get(name)?.value.as_seq()?;
        let mut out = Vec::with_capacity(seq.len());
        for m in seq {
            out.push(read_matrix4(m)?);
        }
        Some(out)
    };
    let bind = read_matrices("bindTransforms");
    let rest = read_matrices("restTransforms");
    if let Some(b) = &bind {
        if b.len() != joint_tokens.len() {
            return Err(invalid(format!(
                "Skeleton `{path}`: bindTransforms count {} != joints count {}",
                b.len(),
                joint_tokens.len()
            )));
        }
    }
    if let Some(r) = &rest {
        if r.len() != joint_tokens.len() {
            return Err(invalid(format!(
                "Skeleton `{path}`: restTransforms count {} != joints count {}",
                r.len(),
                joint_tokens.len()
            )));
        }
    }

    // One node per joint. The token is a relative SdfPath
    // ("Root/Hip/Knee"); its parent is the longest other entry that
    // is a path-prefix — with canonical authoring that's exactly the
    // token up to the last `/`.
    let mut nodes_by_token: HashMap<&str, oxideav_mesh3d::NodeId> = HashMap::new();
    let mut joint_nodes = Vec::with_capacity(joint_tokens.len());
    for (i, token) in joint_tokens.iter().enumerate() {
        let name = token.rsplit('/').next().unwrap_or(token);
        let mut node = Node::new().with_name(name.to_string());
        if let Some(r) = &rest {
            // USD row-vector literal → typed column-vector matrix.
            node.transform = Transform::Matrix(transpose4(r[i]));
        }
        let id = ctx.scene.add_node(node);
        nodes_by_token.insert(token.as_str(), id);
        joint_nodes.push(id);
    }
    // Parent wiring + root detection.
    let mut root_joints = Vec::new();
    for (i, token) in joint_tokens.iter().enumerate() {
        let parent_id = token
            .rfind('/')
            .and_then(|cut| nodes_by_token.get(&token[..cut]).copied());
        match parent_id {
            Some(pid) => {
                let child = joint_nodes[i];
                ctx.scene.nodes[pid.0 as usize].children.push(child);
            }
            None => root_joints.push(joint_nodes[i]),
        }
    }

    // USD row-vector literals → typed column-vector matrices, then
    // invert (the typed model stores the *inverse* bind matrices with
    // an affine `[0,0,0,1]` fourth row — `Scene3D::validate` checks
    // exactly that).
    let inverse_bind_matrices: Vec<[[f32; 4]; 4]> = match &bind {
        Some(b) => b.iter().map(|m| invert_matrix4(transpose4(*m))).collect(),
        None => joint_tokens.iter().map(|_| IDENTITY4).collect(),
    };
    let skeleton = oxideav_mesh3d::Skeleton {
        name: Some(prim.name.clone()),
        joints: joint_nodes.clone(),
        inverse_bind_matrices,
    };
    let skeleton_id = ctx.scene.add_skeleton(skeleton);
    Ok(SkelInfo {
        skeleton: skeleton_id,
        joint_tokens,
        joint_nodes,
        root_joints,
    })
}

/// Transpose a 4x4 matrix — the conversion between USD's `matrix4d`
/// convention (row-vector maths, `p' = p · M`, translation in the
/// last *row*) and the typed model's [`Transform::Matrix`]
/// convention (column-vector maths, `p' = M · p`, translation in the
/// last *column*). The two are exact transposes of each other, so
/// this one helper is its own inverse and is applied at every
/// USD-literal ↔ typed-model boundary.
pub(crate) fn transpose4(m: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0f32; 4]; 4];
    for (i, row) in m.iter().enumerate() {
        for (j, cell) in row.iter().enumerate() {
            out[j][i] = *cell;
        }
    }
    out
}

/// Row-major 4x4 identity.
pub(crate) const IDENTITY4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Invert a 4x4 matrix by Gauss-Jordan elimination with partial
/// pivoting (f64 internally). Returns the identity for a singular
/// input — a defensive fallback for malformed bind transforms; a
/// well-formed bind pose is always invertible.
pub(crate) fn invert_matrix4(m: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut a = [[0f64; 8]; 4];
    for i in 0..4 {
        for j in 0..4 {
            a[i][j] = m[i][j] as f64;
        }
        a[i][4 + i] = 1.0;
    }
    for col in 0..4 {
        // Partial pivot.
        let mut pivot = col;
        for row in (col + 1)..4 {
            if a[row][col].abs() > a[pivot][col].abs() {
                pivot = row;
            }
        }
        if a[pivot][col].abs() < 1e-12 {
            return IDENTITY4;
        }
        a.swap(col, pivot);
        let p = a[col][col];
        for cell in a[col].iter_mut() {
            *cell /= p;
        }
        for row in 0..4 {
            if row == col {
                continue;
            }
            let f = a[row][col];
            if f != 0.0 {
                let pivot_row = a[col];
                for (cell, p) in a[row].iter_mut().zip(pivot_row.iter()) {
                    *cell -= f * p;
                }
            }
        }
    }
    let mut out = [[0f32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            out[i][j] = a[i][4 + j] as f32;
        }
    }
    out
}

/// Compose a [`Transform`] into the row-major, row-vector-convention
/// 4x4 matrix USD's `matrix4d` literal uses (translation in the last
/// row): `p' = p · S · R · T` for TRS semantics. A typed
/// [`Transform::Matrix`] (column-vector convention) transposes into
/// the literal layout.
pub(crate) fn transform_to_matrix(t: &Transform) -> [[f32; 4]; 4] {
    match *t {
        Transform::Matrix(m) => transpose4(m),
        Transform::Trs {
            translation,
            rotation,
            scale,
        } => {
            let [x, y, z, w] = rotation;
            // Column-vector rotation matrix, transposed into the
            // row-vector convention as it's written out.
            let r = [
                [
                    1.0 - 2.0 * (y * y + z * z),
                    2.0 * (x * y + z * w),
                    2.0 * (x * z - y * w),
                ],
                [
                    2.0 * (x * y - z * w),
                    1.0 - 2.0 * (x * x + z * z),
                    2.0 * (y * z + x * w),
                ],
                [
                    2.0 * (x * z + y * w),
                    2.0 * (y * z - x * w),
                    1.0 - 2.0 * (x * x + y * y),
                ],
            ];
            let mut m = IDENTITY4;
            for i in 0..3 {
                for j in 0..3 {
                    m[i][j] = scale[i] * r[i][j];
                }
                m[i][3] = 0.0;
            }
            m[3] = [translation[0], translation[1], translation[2], 1.0];
            m
        }
    }
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
        // UsdSkel §1.2: the Skeleton prim materialises as a carrier
        // node whose children are the root joints built by the
        // `index_skeletons` pre-pass. The `usd:skeleton` extras
        // marker (the SkeletonId) tells the writer to re-emit a
        // `def Skeleton` — with joints / bindTransforms /
        // restTransforms reconstructed from the typed model — instead
        // of walking the joints as plain Xforms.
        "Skeleton" => {
            let Some(info) = ctx.skeletons_by_path.get(&path).cloned() else {
                return Ok(None);
            };
            let mut node = Node::new().with_name(prim.name.clone());
            node.children = info.root_joints.clone();
            node.transform = read_node_transform(prim);
            node.extras.insert(
                "usd:skeleton".into(),
                serde_json::Value::Number(info.skeleton.0.into()),
            );
            // §1.2 jointNames (presentational) ride on extras.
            if let Some(names) = prim.attrs.get("jointNames").and_then(|a| a.value.as_seq()) {
                let arr: Vec<serde_json::Value> = names
                    .iter()
                    .filter_map(|v| v.as_text())
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect();
                node.extras
                    .insert("usd:skel:jointNames".into(), serde_json::Value::Array(arr));
            }
            stash_extras(&mut node.extras, prim);
            Ok(Some(ctx.scene.add_node(node)))
        }
        // UsdSkel §1.3: a SkelAnimation decodes into one typed
        // [`Animation`](oxideav_mesh3d::Animation) — per-joint
        // Translation / Rotation / Scale channels targeting the
        // joint nodes of the skeleton its `joints` tokens name. The
        // carrier node marks `usd:skelAnimation` (the animation
        // index) so the writer re-emits a `def SkelAnimation` at the
        // same spot. `PackedJointAnimation` is the deprecated
        // predecessor with the same attribute surface.
        "SkelAnimation" | "PackedJointAnimation" => {
            let Some(anim_idx) = build_skel_animation(ctx, &path, prim)? else {
                // No skeleton matches the animation's joints — keep
                // the prim as an unknown-schema node so nothing is
                // silently dropped.
                let mut node = Node::new().with_name(prim.name.clone());
                node.extras.insert(
                    "usd:type".into(),
                    serde_json::Value::String(prim.type_name.clone()),
                );
                stash_extras(&mut node.extras, prim);
                return Ok(Some(ctx.scene.add_node(node)));
            };
            let mut node = Node::new().with_name(prim.name.clone());
            node.extras.insert(
                "usd:skelAnimation".into(),
                serde_json::Value::Number(anim_idx.into()),
            );
            // A static (default-value) `blendShapeWeights` table has
            // no animation channel to invert on the writer side —
            // stash the authored roster + scalars on the carrier so
            // the `def SkelAnimation` re-emits them verbatim in the
            // default-value form.
            if let Some(entry) = ctx
                .pending_blend_weights
                .iter()
                .find(|e| e.anim_idx == anim_idx && e.static_default)
            {
                let mut stash = serde_json::Map::new();
                stash.insert(
                    "names".into(),
                    serde_json::Value::Array(
                        entry
                            .channel_names
                            .iter()
                            .map(|n| serde_json::Value::String(n.clone()))
                            .collect(),
                    ),
                );
                stash.insert(
                    "weights".into(),
                    serde_json::Value::Array(
                        entry
                            .frames
                            .first()
                            .map(|f| f.as_slice())
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|w| {
                                serde_json::Number::from_f64(*w as f64)
                                    .map(serde_json::Value::Number)
                            })
                            .collect(),
                    ),
                );
                node.extras.insert(
                    "usd:skelAnim:staticWeights".into(),
                    serde_json::Value::Object(stash),
                );
            }
            stash_extras(&mut node.extras, prim);
            Ok(Some(ctx.scene.add_node(node)))
        }
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
        "Xform" | "Scope" | "SkelRoot" | "" => {
            let mut node = Node::new().with_name(prim.name.clone());
            node.transform = read_node_transform(prim);
            // §1.1: SkelRoot marks the skinnable subtree; keep the
            // schema token so the writer re-emits `def SkelRoot`.
            if prim.type_name == "SkelRoot" {
                node.extras.insert(
                    "usd:type".into(),
                    serde_json::Value::String("SkelRoot".into()),
                );
            }
            // §3.4 rule 1: direct material bindings inherit down
            // namespace — push this container's binding opinions
            // for descendant gprims to resolve against, and stash
            // the authored relationships verbatim so the writer
            // replays them on the re-emitted container prim.
            let binding_level = read_binding_level(prim);
            let authored_bindings = collect_binding_attrs(prim);
            if !authored_bindings.is_empty() {
                node.extras.insert(
                    "usd:materialBindings".into(),
                    binding_stash_json(&authored_bindings),
                );
            }
            // §15.1 CollectionAPI properties round-trip verbatim.
            let authored_collections = collect_collection_attrs(prim);
            if !authored_collections.is_empty() {
                node.extras.insert(
                    "usd:collections".into(),
                    binding_stash_json(&authored_collections),
                );
            }
            ctx.binding_levels.push(binding_level);
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
            // (An early `?` return above discards `ctx` wholesale,
            // so no unwind bookkeeping is needed on the error path.)
            ctx.binding_levels.pop();
            node.children = child_ids;
            stash_extras(&mut node.extras, prim);
            // A USD `Scope` is a pure namespace container — it is
            // *non-transformable* and holds no geometry of its own. When
            // such a scope contributed no scene-graph children it adds
            // nothing to the render graph; the canonical case is the
            // top-level `def Scope "Materials"` the writer emits, whose
            // `Material` children are indexed into `scene.materials`
            // separately (via `index_materials`) and never become nodes.
            // Emitting an empty node for it would inject a spurious extra
            // root on every encode→decode cycle (breaking round-trip
            // symmetry: a one-root scene came back with two roots). Drop
            // it. Empty `Xform`s are *kept* — an Xform carries a
            // transform and may be a meaningful locator/anchor.
            if prim.type_name == "Scope" && node.children.is_empty() {
                return Ok(None);
            }
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
        if seed_mesh.target_names.is_empty() {
            seed_mesh.target_names = extra.target_names;
        }
        for p in extra.primitives {
            seed_mesh.primitives.push(p);
        }
    }
    let mesh_id = ctx.scene.add_mesh(seed_mesh);
    let mut node = Node::new().with_name(head.name.clone()).with_mesh(mesh_id);
    node.transform = read_node_transform(head);
    node.skin = skel_skin_for_prim(ctx, head);
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
    node.skin = skel_skin_for_prim(ctx, prim);
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
            // USD row-vector literal → typed column-vector matrix.
            return Transform::Matrix(transpose4(m));
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

/// Prim-metadata keys that are the *writer's own* geometry-recovery
/// hints. The decoder consumes each of these into `Primitive::extras`
/// (see [`apply_mesh_metadata_to_primitive`]) and the mesh writer
/// re-emits them directly on the `def Mesh` / `def BasisCurves` /
/// `def Points` prim from that Primitive. Stashing them a *second* time
/// onto the carrier node's extras would (a) double-emit them (once on
/// the mesh prim, once on the enclosing Xform) and (b) leave the carrier
/// node with non-empty prim-metadata, which defeats the bare-mesh
/// collapse in the writer and makes the round-trip grow an extra Xform
/// level per cycle. They are never authored on non-geometry (`Xform` /
/// `Scope`) prims, so filtering them unconditionally is safe.
const GEOMETRY_HINT_METADATA_KEYS: &[&str] = &["usd:original_topology", "usd:no_fold"];

fn stash_extras(extras: &mut HashMap<String, serde_json::Value>, prim: &Prim) {
    let filtered: BTreeMap<String, Value> = prim
        .metadata
        .iter()
        .filter(|(k, _)| !GEOMETRY_HINT_METADATA_KEYS.contains(&k.as_str()))
        .filter_map(|(k, v)| {
            // `SkelBindingAPI` is regenerated by the writer on every
            // skinned geometry prim (from `Primitive::joints` +
            // `Node::skin`) — stashing it here would double-emit it
            // AND leave the mesh-carrier node with non-empty prim
            // metadata, defeating the bare-mesh collapse (one extra
            // Xform level per encode→decode cycle).
            if k == "apiSchemas" {
                return strip_skel_binding_api(v).map(|nv| (k.clone(), nv));
            }
            Some((k.clone(), v.clone()))
        })
        .collect();
    if !filtered.is_empty() {
        let mut obj = serde_json::Map::new();
        for (k, v) in &filtered {
            if let Some(j) = value_to_json(v) {
                obj.insert(k.clone(), j);
            }
        }
        extras.insert("usd:metadata".into(), serde_json::Value::Object(obj));
        // Round 9: also stash the metadata block in the lossless
        // tagged shape from [`crate::variant_codec`] so the writer can
        // reconstruct each value's USDA type-token (Token vs String vs
        // Asset vs AssetWithPath vs Path).  The untagged `usd:metadata`
        // entry above stays for callers that consume the JSON
        // directly without going back through the writer.
        let tagged = crate::variant_codec::encode_btree_value(&filtered);
        extras.insert(crate::usda_writer::PRIM_METADATA_EXTRAS_KEY.into(), tagged);
    }
    // Round 8: stash the prim's structured `variant_sets` block on
    // `usd:variantSets` so the writer can re-emit every unselected
    // variant (round 7 only evaluated the *selected* one, leaving the
    // others invisible to a USDZ → USDZ round trip).  See
    // [`crate::variant_codec`] for the JSON shape.
    if let Some(json) = crate::variant_codec::encode_variant_sets(&prim.variant_sets) {
        extras.insert(crate::variant_codec::EXTRAS_KEY.into(), json);
    }
}

/// Remove the `"SkelBindingAPI"` token from an `apiSchemas` opinion
/// (arrays, scalars, and list-op sublists). Returns `None` when
/// nothing else remains — the whole key is then dropped from the
/// stash. See [`stash_extras`].
fn strip_skel_binding_api(v: &Value) -> Option<Value> {
    const TOKEN: &str = "SkelBindingAPI";
    match v {
        Value::String(s) | Value::Token(s) if s == TOKEN => None,
        Value::Array(items) => {
            let kept: Vec<Value> = items
                .iter()
                .filter(|item| item.as_text() != Some(TOKEN))
                .cloned()
                .collect();
            if kept.is_empty() {
                None
            } else {
                Some(Value::Array(kept))
            }
        }
        Value::ListOp(list) => {
            let strip = |slot: &Option<Value>| slot.as_ref().and_then(strip_skel_binding_api);
            let out = crate::usda::ListOp {
                prepended: strip(&list.prepended),
                appended: strip(&list.appended),
                deleted: list.deleted.clone(),
                explicit: strip(&list.explicit),
                reordered: list.reordered.clone(),
            };
            if out.prepended.is_none()
                && out.appended.is_none()
                && out.deleted.is_none()
                && out.explicit.is_none()
                && out.reordered.is_none()
            {
                None
            } else {
                Some(Value::ListOp(Box::new(out)))
            }
        }
        other => Some(other.clone()),
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
    let primitives = match prim.type_name.as_str() {
        "BasisCurves" => vec![build_basis_curves_primitive(ctx, path, prim)?],
        "Points" => vec![build_points_primitive(ctx, path, prim)?],
        // "Mesh" or anything else routes through the triangle path.
        // A Mesh prim carrying `def GeomSubset` children may split
        // into one Primitive per material-bound face subset.
        _ => {
            let base = build_triangle_primitive(ctx, path, prim)?;
            apply_geom_subsets(ctx, path, prim, base)?
        }
    };

    let mut mesh = Mesh::new(Some(prim.name.clone()));
    mesh.primitives = primitives;
    // §1.5 `skel:blendShapes` → typed `Mesh::target_names` (one name
    // per morph-target slot, shared by every primitive of the mesh —
    // a GeomSubset split clones the base roster).
    for p in &mut mesh.primitives {
        if let Some(v) = p.extras.remove(PENDING_TARGET_NAMES) {
            if mesh.target_names.is_empty() {
                mesh.target_names = v
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|n| n.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
    }
    Ok(mesh)
}

/// Transient `Primitive::extras` key carrying a gprim's
/// `skel:blendShapes` roster from [`apply_blend_shapes`] up to
/// [`build_mesh`], which lifts it onto `Mesh::target_names` and
/// removes it — never visible on a decoded scene.
const PENDING_TARGET_NAMES: &str = "usd:skel:blendShapes.pending";

/// One material-bound face subset gathered by [`apply_geom_subsets`].
struct MaterialFaceSubset {
    name: String,
    family_name: String,
    faces: Vec<u32>,
    material: oxideav_mesh3d::MaterialId,
    /// The authored `material:binding*` relationship set, stashed
    /// verbatim (see [`binding_stash_json`]) when it differs from
    /// the single synthetic `rel material:binding` the writer would
    /// emit — purpose-restricted spellings, `bindMaterialAs`
    /// metadata, collection forms.
    bindings: Option<serde_json::Value>,
    /// Unconsumed attrs / metadata / children of the subset prim,
    /// preserved losslessly for the writer replay. `None` when the
    /// subset carried nothing beyond the schema quartet.
    rest: Option<serde_json::Value>,
}

/// `UsdGeomSubset` (staged schema `usdgeom-usdshade-schema.md`
/// Part 1): split a Mesh prim's per-face **material** subsets into
/// one typed [`Primitive`] per bound material.
///
/// Placement rule §1.1: a `GeomSubset` is a direct namespace child
/// of the geometric prim it subsets, so discovery is a single scan
/// of `prim.children`. A subset participates in the split when
///
/// * `elementType` is `face` (the schema fallback — §1.2), and
/// * it carries a `material:binding` that resolves to an indexed
///   `Material` (§1.1: materials bind to a subset through the
///   ordinary MaterialBindingAPI).
///
/// Every other subset (point/edge/segment/tetrahedron element
/// types, families with no material binding, bindings whose target
/// is not present in the layer) is preserved losslessly on the base
/// primitive's `extras["usd:geomSubsets"]` and re-emitted verbatim
/// by the writer.
///
/// **Face → triangle mapping.** `indices` index the parent's
/// authored faces (§1.2); the typed model is triangle-list, so face
/// `f` (arity `n`) maps to the `n - 2` fan triangles
/// [`fan_triangulate`] emitted for it. Each subset primitive shares
/// the parent's vertex arrays (points / normals / UVs / colours /
/// joint influences / morph targets are all point-indexed) and
/// carries only its own faces' triangles. Faces claimed by **no**
/// material subset stay on the base primitive with the parent
/// prim's own binding — the §1.3 "unassigned elements" set, which
/// falls back to the parent's binding during material resolution.
/// The base primitive is always kept (even when a partition family
/// claims every face) so mesh-level state — parent binding,
/// doubleSided, skinning, blend shapes — has a stable carrier.
///
/// **Families (§1.3).** `partition` / `nonOverlapping` /
/// `unrestricted` constraints are declarative assertions by the
/// author, not enforced invariants — per the schema doc a reader
/// must "validate ... or tolerate its violation — not assume". We
/// tolerate: overlapping face claims keep every claiming subset's
/// authored membership (each subset primitive carries all its
/// faces). The family *type* token lives on the **parent** prim
/// under a property whose spelling the published documentation does
/// not state (§1.4); per the documented consequence we discover it
/// by enumeration — any token-valued parent property whose value is
/// one of the three familyType tokens and whose name carries an
/// authored `familyName` as a `:`-separated segment — and preserve
/// the exact authored spelling on
/// `extras["usd:subsetFamilies"]` for verbatim re-emission.
///
/// An `indices` value that is malformed or out of the parent's
/// face range is a precise `InvalidData` refusal (a corrupt
/// subset, not a tolerable family-constraint violation).
fn apply_geom_subsets(
    ctx: &mut Ctx,
    path: &str,
    prim: &Prim,
    mut base: Primitive,
) -> Result<Vec<Primitive>> {
    let subset_prims: Vec<&Prim> = prim
        .children
        .iter()
        .filter(|c| c.spec == "def" && c.type_name == "GeomSubset")
        .collect();
    if subset_prims.is_empty() {
        return Ok(vec![base]);
    }

    // Rebuild the authored face → triangle-run map. Face `f` of
    // arity `n` yields `n.saturating_sub(2)` triangles, in authored
    // order — exactly `fan_triangulate`'s emission rule (degenerate
    // faces with arity < 3 yield none).
    let counts = prim
        .attrs
        .get("faceVertexCounts")
        .and_then(|a| read_int_array(&a.value))
        .ok_or_else(|| invalid(format!("Mesh `{path}` has malformed `faceVertexCounts`")))?;
    let n_faces = counts.len();
    let mut tri_start: Vec<u32> = Vec::with_capacity(n_faces);
    let mut acc = 0u32;
    for &n in &counts {
        tri_start.push(acc);
        acc += n.saturating_sub(2);
    }

    // Flatten the base primitive's triangulated index buffer.
    let base_indices: Vec<u32> = match &base.indices {
        Some(Indices::U16(v)) => v.iter().map(|&i| i as u32).collect(),
        Some(Indices::U32(v)) => v.clone(),
        None => (0..base.positions.len() as u32).collect(),
    };

    let mut material_subsets: Vec<MaterialFaceSubset> = Vec::new();
    let mut verbatim: Vec<serde_json::Value> = Vec::new();
    for sp in &subset_prims {
        let spath = join_path(path, &sp.name);
        let element_type = sp
            .attrs
            .get("elementType")
            .and_then(|a| a.value.as_text())
            // §1.2 fallback.
            .unwrap_or("face");
        let family_name = sp
            .attrs
            .get("familyName")
            .and_then(|a| a.value.as_text())
            .unwrap_or("");
        // §1.1: a subset is a binding target like any other prim —
        // its *own authored* binding (any §3.1 purpose form, direct
        // or collection, resolved under the same preview-first
        // preference) selects the split material. A subset
        // authoring no binding keeps its faces on the base
        // primitive, which is exactly the §3.4 fallback through the
        // parent's own resolution.
        let own_level = read_binding_level(sp);
        let resolved = resolve_binding(std::slice::from_ref(&own_level), &ctx.collections, &spath);
        let material = resolved
            .as_ref()
            .and_then(|rel| ctx.materials_by_path.get(rel).copied());
        if element_type != "face" || material.is_none() {
            // Not a material face subset — preserve the whole prim
            // losslessly for the writer replay.
            verbatim.push(crate::variant_codec::encode_prim(sp));
            continue;
        }
        // §1.2: `indices` fallback is the empty list.
        let faces = match sp.attrs.get("indices") {
            Some(a) => read_int_array(&a.value)
                .ok_or_else(|| invalid(format!("GeomSubset `{spath}` has malformed `indices`")))?,
            None => Vec::new(),
        };
        if let Some(&bad) = faces.iter().find(|&&f| f as usize >= n_faces) {
            return Err(invalid(format!(
                "GeomSubset `{spath}` face index {bad} is out of range \
                 (parent mesh has {n_faces} faces)"
            )));
        }
        // Unconsumed opinions on the subset prim ride along.
        let mut stripped = (*sp).clone();
        for k in ["elementType", "familyName", "indices"] {
            stripped.attrs.remove(k);
        }
        stripped
            .attrs
            .retain(|k, _| k != "material:binding" && !k.starts_with("material:binding:"));
        let authored_bindings = collect_binding_attrs(sp);
        let bindings_stash = if bindings_are_synthetic(&authored_bindings, resolved.as_deref()) {
            None
        } else {
            Some(binding_stash_json(&authored_bindings))
        };
        let rest = if stripped.attrs.is_empty()
            && stripped.metadata.is_empty()
            && stripped.children.is_empty()
            && stripped.variant_sets.is_empty()
        {
            None
        } else {
            Some(crate::variant_codec::encode_prim(&stripped))
        };
        material_subsets.push(MaterialFaceSubset {
            name: sp.name.clone(),
            family_name: family_name.to_string(),
            faces,
            material: material.expect("checked above"),
            bindings: bindings_stash,
            rest,
        });
    }

    // §1.4 familyType discovery-by-enumeration on the parent prim.
    let family_names: std::collections::BTreeSet<&str> = material_subsets
        .iter()
        .map(|m| m.family_name.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let mut family_props: Vec<serde_json::Value> = Vec::new();
    for (name, attr) in &prim.attrs {
        if !attr.type_token.contains("token") {
            continue;
        }
        let Some(val) = attr.value.as_text() else {
            continue;
        };
        if !matches!(val, "partition" | "nonOverlapping" | "unrestricted") {
            continue;
        }
        if name.split(':').any(|seg| family_names.contains(seg)) {
            family_props.push(serde_json::json!({
                "name": name,
                "typeToken": attr.type_token,
                "value": val,
            }));
        }
    }

    if !verbatim.is_empty() {
        base.extras
            .insert("usd:geomSubsets".into(), serde_json::Value::Array(verbatim));
    }
    if material_subsets.is_empty() {
        return Ok(vec![base]);
    }
    if !family_props.is_empty() {
        base.extras.insert(
            "usd:subsetFamilies".into(),
            serde_json::Value::Array(family_props),
        );
    }

    // §2.5 doubleSided is a gprim-wide flag: a subset-bound material
    // inherits it just like the parent-bound one does.
    let double_sided = base
        .extras
        .get("usd:doubleSided")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Split: each material subset takes its authored faces'
    // triangles (authored face order preserved); the base keeps the
    // unclaimed remainder.
    let n_tris = (base_indices.len() / 3) as u32;
    let mut claimed = vec![false; n_tris as usize];
    let tris_of_face = |f: u32| -> std::ops::Range<u32> {
        let start = tri_start[f as usize];
        start..start + counts[f as usize].saturating_sub(2)
    };
    let pack_indices = |flat: Vec<u32>, n_points: usize| -> Indices {
        if n_points <= u16::MAX as usize {
            Indices::U16(flat.iter().map(|&i| i as u16).collect())
        } else {
            Indices::U32(flat)
        }
    };
    let mut out: Vec<Primitive> = Vec::new();
    let mut subset_primitives: Vec<Primitive> = Vec::new();
    for ms in &material_subsets {
        let mut flat: Vec<u32> = Vec::new();
        for &f in &ms.faces {
            for t in tris_of_face(f) {
                claimed[t as usize] = true;
                let b = (t as usize) * 3;
                flat.extend_from_slice(&base_indices[b..b + 3]);
            }
        }
        let mut sp = Primitive::new(Topology::Triangles);
        sp.positions = base.positions.clone();
        sp.normals = base.normals.clone();
        sp.tangents = base.tangents.clone();
        sp.uvs = base.uvs.clone();
        sp.colors = base.colors.clone();
        sp.joints = base.joints.clone();
        sp.weights = base.weights.clone();
        // Morph targets are per-point deltas — every primitive of
        // the mesh must carry the same target roster for the typed
        // model's per-mesh weight vector to stay meaningful.
        sp.targets = base.targets.clone();
        sp.indices = Some(pack_indices(flat, base.positions.len()));
        sp.material = Some(ms.material);
        let mut marker = serde_json::Map::new();
        marker.insert("name".into(), serde_json::Value::String(ms.name.clone()));
        if !ms.family_name.is_empty() {
            marker.insert(
                "familyName".into(),
                serde_json::Value::String(ms.family_name.clone()),
            );
        }
        if let Some(bindings) = &ms.bindings {
            marker.insert("bindings".into(), bindings.clone());
        }
        if let Some(rest) = &ms.rest {
            marker.insert("rest".into(), rest.clone());
        }
        sp.extras
            .insert("usd:subset".into(), serde_json::Value::Object(marker));
        if double_sided {
            if let Some(mat) = ctx.scene.materials.get_mut(ms.material.0 as usize) {
                mat.double_sided = true;
            }
        }
        subset_primitives.push(sp);
    }
    // Remainder faces stay on the base primitive.
    let mut remainder: Vec<u32> = Vec::new();
    for t in 0..n_tris {
        if !claimed[t as usize] {
            let b = (t as usize) * 3;
            remainder.extend_from_slice(&base_indices[b..b + 3]);
        }
    }
    base.indices = Some(pack_indices(remainder, base.positions.len()));
    out.push(base);
    out.extend(subset_primitives);
    Ok(out)
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
    // §2.5 multi-UV: additional `primvars:st1`, `primvars:st2`, ...
    // sets land on `uvs[1]`, `uvs[2]`, ... (a `UsdPrimvarReader_float2`
    // selects one by `varname` — see `resolve_uv_set`). The walk stops
    // at the first missing index so authoring gaps don't shift sets.
    let mut uv_n = 1u32;
    while let Some(extra) = prim.attrs.get(&format!("primvars:st{uv_n}")) {
        // Pad any skipped set (uvs[0] missing but st1 authored) so
        // indices line up with the primvar names.
        if let Some(arr) = read_vec2_array(&extra.value) {
            while prim_out.uvs.len() < uv_n as usize {
                prim_out.uvs.push(Vec::new());
            }
            prim_out.uvs.push(arr);
        }
        uv_n += 1;
    }

    // §2.5 `doubleSided` (UsdGeomGprim): disable back-face culling.
    // The typed model carries the flag on the *material* (glTF
    // convention), so a bound material picks it up below; the
    // primitive-level extras flag is the round-trip authority.
    let double_sided = prim
        .attrs
        .get("doubleSided")
        .map(|a| match &a.value {
            Value::Bool(b) => *b,
            Value::Float(f) => *f != 0.0,
            Value::Token(s) | Value::String(s) => matches!(s.as_str(), "true" | "1"),
            _ => false,
        })
        .unwrap_or(false);
    if double_sided {
        prim_out
            .extras
            .insert("usd:doubleSided".into(), serde_json::Value::Bool(true));
    }

    // §2.5 display primvars: a per-vertex `primvars:displayColor`
    // (one color3f per point) lands on the first vertex-colour set,
    // with `primvars:displayOpacity` supplying the alpha channel.
    // Constant / non-vertex interpolations have no typed slot and
    // ride on extras verbatim.
    if let Some(dc) = prim.attrs.get("primvars:displayColor") {
        if let Some(colors) = read_vec3_array(&dc.value) {
            let opacity: Option<Vec<f32>> = prim
                .attrs
                .get("primvars:displayOpacity")
                .and_then(|a| a.value.as_seq())
                .map(|seq| seq.iter().filter_map(|v| v.as_f32()).collect());
            if colors.len() == prim_out.positions.len() {
                let rgba: Vec<[f32; 4]> = colors
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let a = opacity
                            .as_ref()
                            .and_then(|o| o.get(i).copied())
                            .unwrap_or(1.0);
                        [c[0], c[1], c[2], a]
                    })
                    .collect();
                prim_out.colors.push(rgba);
            } else {
                // Constant / uniform interpolation — preserve on
                // extras so the writer re-emits the authored shape.
                prim_out.extras.insert(
                    "usd:displayColor".into(),
                    serde_json::Value::Array(
                        colors
                            .iter()
                            .map(|c| {
                                serde_json::Value::Array(
                                    c.iter()
                                        .filter_map(|x| {
                                            serde_json::Number::from_f64(*x as f64)
                                                .map(serde_json::Value::Number)
                                        })
                                        .collect(),
                                )
                            })
                            .collect(),
                    ),
                );
                if let Some(o) = opacity {
                    prim_out.extras.insert(
                        "usd:displayOpacity".into(),
                        serde_json::Value::Array(
                            o.iter()
                                .filter_map(|x| {
                                    serde_json::Number::from_f64(*x as f64)
                                        .map(serde_json::Value::Number)
                                })
                                .collect(),
                        ),
                    );
                }
            }
        }
    }

    apply_material_binding(ctx, path, prim, &mut prim_out, double_sided);

    apply_skel_binding(ctx, path, prim, &mut prim_out)?;
    let channel_names = apply_blend_shapes(ctx, path, prim, &mut prim_out)?;
    if !channel_names.is_empty() {
        // Hand the roster up to `build_mesh`, which lifts it onto the
        // typed `Mesh::target_names` and strips this transient key.
        prim_out.extras.insert(
            PENDING_TARGET_NAMES.into(),
            serde_json::Value::Array(
                channel_names
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    apply_mesh_metadata_to_primitive(prim, &mut prim_out);
    Ok(prim_out)
}

/// UsdSkel blend shapes (§1.4 / §1.5 / §1.8): resolve the geometry's
/// `skel:blendShapes` channel names + positionally matched
/// `rel skel:blendShapeTargets` into typed
/// [`MorphTarget`](oxideav_mesh3d::MorphTarget)s — **one target per
/// channel**, its §1.4.1 inbetween shapes on the target's typed
/// `inbetweens` roster.
///
/// Each target's `offsets` (and optional `normalOffsets`) expand to
/// dense per-point deltas — a sparse shape (`pointIndices`) scatters
/// into a zero-filled buffer; the same map governs every inbetween.
/// The channel-name roster is returned for the mesh's typed
/// `Mesh::target_names` (the caller lifts it — every primitive of
/// one mesh shares the roster). Two wire-only details stay on
/// `Primitive::extras` because the typed model has no slot for
/// them: the exact authored inbetween normal-offsets attribute
/// spelling (§1.4.2 — discovered, never constructed) and the
/// malformed `inbetweens:*` attributes preserved for verbatim
/// replay.
fn apply_blend_shapes(
    ctx: &mut Ctx,
    path: &str,
    prim: &Prim,
    out: &mut Primitive,
) -> Result<Vec<String>> {
    let Some(names_attr) = prim.attrs.get("skel:blendShapes") else {
        return Ok(Vec::new());
    };
    let names: Vec<String> = names_attr
        .value
        .as_seq()
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_text().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let targets: Vec<String> = match prim.attrs.get("skel:blendShapeTargets").map(|a| &a.value) {
        Some(Value::Path(p)) => vec![p.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Path(p) => Some(p.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if names.is_empty() || targets.is_empty() {
        return Ok(Vec::new());
    }
    if names.len() != targets.len() {
        return Err(invalid(format!(
            "Mesh `{path}`: skel:blendShapes has {} names for {} blendShapeTargets \
             (§1.5 requires positional match)",
            names.len(),
            targets.len()
        )));
    }
    let n_points = out.positions.len();
    let mut normals_attr_roster = serde_json::Map::new();
    let mut malformed_roster = serde_json::Map::new();
    for (name, target_path) in names.iter().zip(&targets) {
        let Some(shape) = ctx.blendshapes_by_path.get(target_path).cloned() else {
            return Err(invalid(format!(
                "Mesh `{path}`: skel:blendShapeTargets entry `{target_path}` names no \
                 BlendShape prim (channel `{name}`)"
            )));
        };
        let mut morph = oxideav_mesh3d::MorphTarget::new();
        morph.position = Some(expand_deltas(
            &shape.offsets,
            shape.point_indices.as_deref(),
            n_points,
            path,
            name,
        )?);
        if let Some(normals) = &shape.normal_offsets {
            morph.normal = Some(expand_deltas(
                normals,
                shape.point_indices.as_deref(),
                n_points,
                path,
                name,
            )?);
        }
        // §1.4.1: each valid inbetween becomes a typed station on
        // the channel's target; the typed model resolves the
        // piecewise-linear interpolation (implicit 0/1 endpoints,
        // unbounded) itself, so the scalar channel weight is stored
        // verbatim downstream.
        let mut spellings = serde_json::Map::new();
        for inb in &shape.inbetweens {
            let mut typed = oxideav_mesh3d::Inbetween::new(inb.weight).with_name(inb.name.clone());
            typed.position = Some(expand_deltas(
                &inb.offsets,
                // §1.4.1: the prim-level pointIndices governs every
                // inbetween's arrays too.
                shape.point_indices.as_deref(),
                n_points,
                path,
                name,
            )?);
            if let Some((attr_name, normals)) = &inb.normal_offsets {
                typed.normal = Some(expand_deltas(
                    normals,
                    shape.point_indices.as_deref(),
                    n_points,
                    path,
                    name,
                )?);
                spellings.insert(
                    inb.name.clone(),
                    serde_json::Value::String(attr_name.clone()),
                );
            }
            morph.inbetweens.push(typed);
        }
        out.targets.push(morph);
        if !spellings.is_empty() {
            normals_attr_roster.insert(name.clone(), serde_json::Value::Object(spellings));
        }
        if let Some(m) = &shape.malformed_inbetweens {
            malformed_roster.insert(name.clone(), m.clone());
        }
    }
    // §1.5: an authored (or inherited) `skel:animationSource` names
    // THE animation that drives this geometry — stash the target
    // path so the blend-weight post-pass scopes attachment to it
    // (without it, roster-name intersection alone decides, which
    // cannot distinguish two animations over identical rosters).
    if let Some(Value::Path(p)) = prim.attrs.get("skel:animationSource").map(|a| &a.value) {
        out.extras.insert(
            "usd:skel:animationSource".into(),
            serde_json::Value::String(p.clone()),
        );
    }
    if !normals_attr_roster.is_empty() {
        out.extras.insert(
            "usd:skel:inbetweenNormalsAttr".into(),
            serde_json::Value::Object(normals_attr_roster),
        );
    }
    if !malformed_roster.is_empty() {
        out.extras.insert(
            "usd:skel:malformedInbetweens".into(),
            serde_json::Value::Object(malformed_roster),
        );
    }
    Ok(names)
}

/// Expand a BlendShape delta array to one dense entry per base
/// point: a sparse shape scatters `deltas[i]` to
/// `point_indices[i]`; a dense shape must already be one delta per
/// point (§1.4).
fn expand_deltas(
    deltas: &[[f32; 3]],
    point_indices: Option<&[u32]>,
    n_points: usize,
    path: &str,
    channel: &str,
) -> Result<Vec<[f32; 3]>> {
    match point_indices {
        Some(indices) => {
            if indices.len() != deltas.len() {
                return Err(invalid(format!(
                    "Mesh `{path}` blend shape `{channel}`: pointIndices length {} !=                      offsets length {}",
                    indices.len(),
                    deltas.len()
                )));
            }
            let mut dense = vec![[0f32; 3]; n_points];
            for (i, &pt) in indices.iter().enumerate() {
                let slot = dense.get_mut(pt as usize).ok_or_else(|| {
                    invalid(format!(
                        "Mesh `{path}` blend shape `{channel}`: pointIndices entry {pt}                          exceeds point count {n_points}"
                    ))
                })?;
                *slot = deltas[i];
            }
            Ok(dense)
        }
        None => {
            if deltas.len() != n_points {
                return Err(invalid(format!(
                    "Mesh `{path}` blend shape `{channel}`: dense offsets length {} !=                      point count {n_points}",
                    deltas.len()
                )));
            }
            Ok(deltas.to_vec())
        }
    }
}

/// UsdSkel BindingAPI joint influences (§1.5 / §1.6) → the typed
/// per-vertex `joints` / `weights` quads.
///
/// * `primvars:skel:jointIndices` / `primvars:skel:jointWeights` —
///   parallel arrays whose layout is governed by the primvar
///   `interpolation` (`constant` = one influence set for every
///   point, `vertex` = one set per point) and `elementSize` (the
///   fixed influence count per set). Both knobs must agree between
///   the two primvars; the stored lengths are validated against
///   them. When `elementSize` is unauthored it is inferred from the
///   array length (÷ point count for `vertex`, the whole array for
///   `constant`).
/// * `skel:joints` — optional per-geometry joint-order override:
///   when authored, the indices reference *its* order and are
///   remapped into the bound Skeleton's canonical `joints` order by
///   token match (§1.5).
/// * More than 4 influences per point keep the 4 largest weights
///   (the typed model's quad width); weights are stored as-authored
///   — §1.6 leaves normalization to the consumer at skinning time.
/// * `primvars:skel:geomBindTransform` (matrix4d) and
///   `primvars:skel:skinningMethod` (token) ride on
///   `Primitive::extras["usd:skel:*"]` for the writer.
///
/// Silently a no-op when the prim has no `skel:skeleton` binding or
/// no influence primvars.
fn apply_skel_binding(ctx: &mut Ctx, path: &str, prim: &Prim, out: &mut Primitive) -> Result<()> {
    let Some(Value::Path(skel_path)) = prim.attrs.get("skel:skeleton").map(|a| &a.value) else {
        return Ok(());
    };
    let Some(info) = ctx.skeletons_by_path.get(skel_path).cloned() else {
        // Unresolved skeleton target — leave the geometry unskinned;
        // the binding attrs still round-trip via the prim stash.
        return Ok(());
    };

    // Optional per-geometry joint-order override (`skel:joints`):
    // remap table from override index → skeleton canonical index.
    let remap: Option<Vec<u16>> = prim
        .attrs
        .get("skel:joints")
        .and_then(|a| a.value.as_seq())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_text())
                .map(|token| {
                    info.joint_tokens
                        .iter()
                        .position(|t| t == token)
                        .map(|i| i as u16)
                        .ok_or_else(|| {
                            invalid(format!(
                                "Mesh `{path}`: skel:joints token `{token}` names no joint \
                                 of Skeleton `{skel_path}`"
                            ))
                        })
                })
                .collect::<Result<Vec<u16>>>()
        })
        .transpose()?;

    let (Some(idx_attr), Some(w_attr)) = (
        prim.attrs.get("primvars:skel:jointIndices"),
        prim.attrs.get("primvars:skel:jointWeights"),
    ) else {
        return Ok(());
    };
    let indices = read_int_array(&idx_attr.value).ok_or_else(|| {
        invalid(format!(
            "Mesh `{path}` has malformed `primvars:skel:jointIndices`"
        ))
    })?;
    let weights: Vec<f32> = w_attr
        .value
        .as_seq()
        .map(|seq| seq.iter().filter_map(|v| v.as_f32()).collect())
        .unwrap_or_default();
    if indices.len() != weights.len() {
        return Err(invalid(format!(
            "Mesh `{path}`: jointIndices length {} != jointWeights length {} (§1.6 requires \
             identical layout)",
            indices.len(),
            weights.len()
        )));
    }
    let meta_i32 = |attr: &Attr, key: &str| attr.metadata.get(key).and_then(|v| v.as_f32());
    let meta_text = |attr: &Attr, key: &str| {
        attr.metadata
            .get(key)
            .and_then(|v| v.as_text().map(str::to_string))
    };
    // §1.6 consistency constraint: `interpolation` and `elementSize`
    // "must be identical between `jointIndices` and `jointWeights`" —
    // a mismatch means the two arrays describe different layouts and
    // any pairing of them fabricates influences, so it is rejected
    // rather than resolved in favour of one primvar.
    if let (Some(a), Some(b)) = (
        meta_text(idx_attr, "interpolation"),
        meta_text(w_attr, "interpolation"),
    ) {
        if a != b {
            return Err(invalid(format!(
                "Mesh `{path}`: jointIndices interpolation `{a}` != jointWeights \
                 interpolation `{b}` (§1.6 requires identical layout)"
            )));
        }
    }
    if let (Some(a), Some(b)) = (
        meta_i32(idx_attr, "elementSize"),
        meta_i32(w_attr, "elementSize"),
    ) {
        if a != b {
            return Err(invalid(format!(
                "Mesh `{path}`: jointIndices elementSize {a} != jointWeights elementSize \
                 {b} (§1.6 requires identical layout)"
            )));
        }
    }
    // Read the (now known-consistent) knobs from either primvar,
    // preferring jointIndices.
    let interp = meta_text(idx_attr, "interpolation")
        .or_else(|| meta_text(w_attr, "interpolation"))
        .unwrap_or_else(|| "vertex".to_string());
    let n_points = out.positions.len();
    let element_size = meta_i32(idx_attr, "elementSize")
        .or_else(|| meta_i32(w_attr, "elementSize"))
        .map(|f| f as usize)
        .unwrap_or_else(|| match interp.as_str() {
            "constant" => indices.len(),
            _ => indices.len().checked_div(n_points).unwrap_or(0),
        });
    if element_size == 0 {
        return Err(invalid(format!(
            "Mesh `{path}`: jointIndices elementSize resolves to 0"
        )));
    }
    let expected = match interp.as_str() {
        "constant" => element_size,
        "vertex" => n_points * element_size,
        other => {
            return Err(invalid(format!(
                "Mesh `{path}`: joint-influence interpolation `{other}` is not in the schema \
                 (`constant` | `vertex`)"
            )))
        }
    };
    if indices.len() != expected {
        return Err(invalid(format!(
            "Mesh `{path}`: jointIndices length {} != {expected} \
             ({interp} interpolation x elementSize {element_size})",
            indices.len()
        )));
    }

    let n_joints = info.joint_tokens.len();
    let mut joints_out = Vec::with_capacity(n_points);
    let mut weights_out = Vec::with_capacity(n_points);
    for p in 0..n_points {
        let base = if interp == "constant" {
            0
        } else {
            p * element_size
        };
        // Collect this point's influences, remapping through the
        // skel:joints override when present.
        let mut influences: Vec<(u16, f32)> = Vec::with_capacity(element_size);
        for e in 0..element_size {
            let raw = indices[base + e];
            let mapped = match &remap {
                Some(table) => *table.get(raw as usize).ok_or_else(|| {
                    invalid(format!(
                        "Mesh `{path}`: jointIndices entry {raw} is outside the \
                         skel:joints override table"
                    ))
                })?,
                None => {
                    if raw as usize >= n_joints {
                        return Err(invalid(format!(
                            "Mesh `{path}`: jointIndices entry {raw} exceeds Skeleton \
                             `{skel_path}` joint count {n_joints}"
                        )));
                    }
                    raw as u16
                }
            };
            influences.push((mapped, weights[base + e]));
        }
        // Keep the 4 strongest influences (stable order for ties).
        if influences.len() > 4 {
            influences.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            influences.truncate(4);
        }
        let mut jq = [0u16; 4];
        let mut wq = [0f32; 4];
        for (slot, (j, wgt)) in influences.iter().enumerate() {
            jq[slot] = *j;
            wq[slot] = *wgt;
        }
        joints_out.push(jq);
        weights_out.push(wq);
    }
    out.joints = Some(joints_out);
    out.weights = Some(weights_out);

    // §1.5 primvars with no typed slot → extras for the writer.
    if let Some(gbt) = prim
        .attrs
        .get("primvars:skel:geomBindTransform")
        .and_then(|a| read_matrix4(&a.value))
    {
        let rows: Vec<serde_json::Value> = gbt.iter().map(|row| f32s_to_json(row)).collect();
        out.extras.insert(
            "usd:skel:geomBindTransform".into(),
            serde_json::Value::Array(rows),
        );
    }
    if let Some(method) = prim
        .attrs
        .get("primvars:skel:skinningMethod")
        .and_then(|a| a.value.as_text())
    {
        out.extras.insert(
            "usd:skel:skinningMethod".into(),
            serde_json::Value::String(method.to_string()),
        );
    }
    Ok(())
}

/// Resolve a prim's `rel skel:skeleton` binding to a shared
/// [`Skin`](oxideav_mesh3d::Skin) (one per skeleton), creating it on
/// first sight with the skeleton's first root joint as the explicit
/// scene-graph root.
fn skel_skin_for_prim(ctx: &mut Ctx, prim: &Prim) -> Option<oxideav_mesh3d::SkinId> {
    let Value::Path(p) = &prim.attrs.get("skel:skeleton")?.value else {
        return None;
    };
    let info = ctx.skeletons_by_path.get(p)?.clone();
    if let Some(&sid) = ctx.skins_by_skeleton.get(&info.skeleton.0) {
        return Some(sid);
    }
    let mut skin = oxideav_mesh3d::Skin::new(info.skeleton);
    if let Some(&root) = info.root_joints.first() {
        skin = skin.with_root(root);
    }
    let sid = ctx.scene.add_skin(skin);
    ctx.skins_by_skeleton.insert(info.skeleton.0, sid);
    Some(sid)
}

/// UsdSkel §1.3: decode a `def SkelAnimation` into one typed
/// [`Animation`](oxideav_mesh3d::Animation) and return its index in
/// `scene.animations` — or `None` when no registered skeleton
/// matches any of the animation's `joints` tokens (nothing to
/// target).
///
/// * `joints` (uniform token[]) — the joint ordering this animation
///   authors; **may be a subset of, and differently ordered than,**
///   the Skeleton's `joints`. Tokens are matched into the skeleton
///   whose joint set overlaps most.
/// * `translations` (float3[]) / `rotations` (quatf[]) / `scales`
///   (half3[]) — parallel per-joint arrays per time sample. Each
///   becomes one channel per joint (Translation / Rotation / Scale)
///   targeting that joint's node. USD's `quatf` literal is
///   `(w, x, y, z)`; the typed model stores xyzw.
/// * timeCodes map to seconds through the layer's
///   `timeCodesPerSecond`; a non-sampled (default-value) array is a
///   single keyframe at `t = 0`.
/// * `blendShapes` / `blendShapeWeights` decode into a
///   `MorphWeights` channel per bound geometry (matched through the
///   geometry's `skel:blendShapes` channel names) — handled by the
///   blend-shape pass.
fn build_skel_animation(ctx: &mut Ctx, path: &str, prim: &Prim) -> Result<Option<usize>> {
    let joint_tokens: Vec<String> = prim
        .attrs
        .get("joints")
        .and_then(|a| a.value.as_seq())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_text().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Pick the skeleton whose joint set overlaps the animation's
    // tokens the most (typical content has exactly one skeleton).
    // A blend-shape-only animation carries no joints — TRS decoding
    // is skipped and the weights ride the pending list below.
    let mut best: Option<(&SkelInfo, usize)> = None;
    for info in ctx.skeletons_by_path.values() {
        let matches = joint_tokens
            .iter()
            .filter(|t| info.joint_tokens.contains(t))
            .count();
        if matches > 0 && best.map(|(_, m)| matches > m).unwrap_or(true) {
            best = Some((info, matches));
        }
    }
    // Target node per animation joint (skipping unmatched tokens —
    // §1.3 says consumers remap by matching path tokens).
    let targets: Vec<Option<oxideav_mesh3d::NodeId>> = match best {
        Some((info, _)) => joint_tokens
            .iter()
            .map(|t| {
                info.joint_tokens
                    .iter()
                    .position(|jt| jt == t)
                    .map(|i| info.joint_nodes[i])
            })
            .collect(),
        None => Vec::new(),
    };

    let tcps = ctx.time_codes_per_second;
    // Materialise `<name>.timeSamples` (or the non-sampled default
    // as one keyframe at t = 0) into (times_seconds, per-sample
    // sequence-of-tuples) pairs.
    let read_samples = |name: &str| -> Option<(Vec<f32>, Vec<Vec<Value>>)> {
        if let Some(attr) = prim.attrs.get(&format!("{name}.timeSamples")) {
            if let Value::TimeSamples(samples) = &attr.value {
                let mut times = Vec::with_capacity(samples.len());
                let mut frames = Vec::with_capacity(samples.len());
                for (t, v) in samples {
                    times.push((t / tcps) as f32);
                    frames.push(v.as_seq().map(<[Value]>::to_vec).unwrap_or_default());
                }
                return Some((times, frames));
            }
        }
        let attr = prim.attrs.get(name)?;
        let frame = attr.value.as_seq()?.to_vec();
        Some((vec![0.0], vec![frame]))
    };

    let n_joints = joint_tokens.len();
    let mut channels: Vec<oxideav_mesh3d::AnimationChannel> = Vec::new();

    // Translation + Scale share the float3 shape.
    for (attr_name, property) in [
        (
            "translations",
            oxideav_mesh3d::AnimationProperty::Translation,
        ),
        ("scales", oxideav_mesh3d::AnimationProperty::Scale),
    ] {
        if targets.is_empty() {
            break;
        }
        let Some((times, frames)) = read_samples(attr_name) else {
            continue;
        };
        for frame in &frames {
            if frame.len() != n_joints {
                return Err(invalid(format!(
                    "SkelAnimation `{path}`: a `{attr_name}` sample has {} entries for {} joints \
                     (§1.3 requires parallel arrays)",
                    frame.len(),
                    n_joints
                )));
            }
        }
        for (j, target) in targets.iter().enumerate() {
            let Some(node) = target else { continue };
            let mut values = Vec::with_capacity(frames.len());
            for frame in &frames {
                values.push(frame[j].as_floatn::<3>().ok_or_else(|| {
                    invalid(format!(
                        "SkelAnimation `{path}`: malformed `{attr_name}` tuple for joint {j}"
                    ))
                })?);
            }
            channels.push(oxideav_mesh3d::AnimationChannel {
                target: oxideav_mesh3d::AnimationTarget {
                    node: *node,
                    property,
                },
                sampler: oxideav_mesh3d::AnimationSampler {
                    keyframes: times.clone(),
                    values: oxideav_mesh3d::AnimationValues::Vec3(values),
                    interpolation: oxideav_mesh3d::Interpolation::Linear,
                },
            });
        }
    }

    // Rotations: quatf authored (w, x, y, z) → internal xyzw.
    if let Some((times, frames)) = read_samples("rotations").filter(|_| !targets.is_empty()) {
        for frame in &frames {
            if frame.len() != n_joints {
                return Err(invalid(format!(
                    "SkelAnimation `{path}`: a `rotations` sample has {} entries for {} joints",
                    frame.len(),
                    n_joints
                )));
            }
        }
        for (j, target) in targets.iter().enumerate() {
            let Some(node) = target else { continue };
            let mut values = Vec::with_capacity(frames.len());
            for frame in &frames {
                let wxyz = frame[j].as_floatn::<4>().ok_or_else(|| {
                    invalid(format!(
                        "SkelAnimation `{path}`: malformed `rotations` quaternion for joint {j}"
                    ))
                })?;
                values.push([wxyz[1], wxyz[2], wxyz[3], wxyz[0]]);
            }
            channels.push(oxideav_mesh3d::AnimationChannel {
                target: oxideav_mesh3d::AnimationTarget {
                    node: *node,
                    property: oxideav_mesh3d::AnimationProperty::Rotation,
                },
                sampler: oxideav_mesh3d::AnimationSampler {
                    keyframes: times.clone(),
                    values: oxideav_mesh3d::AnimationValues::Quat(values),
                    interpolation: oxideav_mesh3d::Interpolation::Linear,
                },
            });
        }
    }

    // §1.3 blend-shape weight channels: gather now, attach after the
    // node walk (the bound geometry's morph-target roster may not be
    // built yet). Weight per channel name, per keyframe.
    let blend_names: Vec<String> = prim
        .attrs
        .get("blendShapes")
        .and_then(|a| a.value.as_seq())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_text().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut pending_blend: Option<PendingBlendWeights> = None;
    if !blend_names.is_empty() {
        if let Some((times, frames)) = read_samples("blendShapeWeights") {
            // A `.timeSamples` map is animation; a plain default
            // value is a *static* weight state (§1.3's arrays are
            // time-varying only when sampled) and maps onto the
            // typed model's per-instance `Node::weights` override.
            let static_default = !matches!(
                prim.attrs
                    .get("blendShapeWeights.timeSamples")
                    .map(|a| &a.value),
                Some(Value::TimeSamples(_))
            );
            let mut weight_frames = Vec::with_capacity(frames.len());
            for frame in &frames {
                if frame.len() != blend_names.len() {
                    return Err(invalid(format!(
                        "SkelAnimation `{path}`: a `blendShapeWeights` sample has {} \
                         entries for {} blendShapes channels",
                        frame.len(),
                        blend_names.len()
                    )));
                }
                weight_frames.push(
                    frame
                        .iter()
                        .filter_map(|v| v.as_f32())
                        .collect::<Vec<f32>>(),
                );
            }
            pending_blend = Some(PendingBlendWeights {
                anim_idx: 0, // patched below once the index is known
                anim_path: path.to_string(),
                channel_names: blend_names,
                times,
                frames: weight_frames,
                static_default,
            });
        }
    }

    if channels.is_empty() && pending_blend.is_none() {
        return Ok(None);
    }
    let idx = ctx.scene.add_animation(oxideav_mesh3d::Animation {
        name: Some(prim.name.clone()),
        channels,
    });
    if let Some(mut pending) = pending_blend {
        pending.anim_idx = idx;
        ctx.pending_blend_weights.push(pending);
    }
    Ok(Some(idx))
}

/// Post-pass (§1.8 step 1): for every gathered SkelAnimation
/// blend-weight table, find each mesh node whose morph-channel
/// roster (`Mesh::target_names`) intersects the animation's channel
/// names, remap the per-frame weights into the mesh's target order,
/// and attach one `MorphWeights` channel targeting that node
/// (synthesized through `AnimationSampler::morph_weights`, so the
/// authored per-keyframe vectors read back losslessly via
/// `morph_weight_frames`). Channels the animation doesn't drive hold
/// weight 0.
///
/// Scoping: geometry that authored (or §1.5-inherited) a
/// `skel:animationSource` relationship accepts weights only from
/// *that* SkelAnimation prim; roster intersection alone decides for
/// geometry without one.
///
/// A *static* table (`blendShapeWeights` authored as a plain default
/// value, not `.timeSamples`) does not fabricate an animation
/// channel: the single frame lands on `Node::weights` — the typed
/// model's per-instance morph-weight override — so two nodes bound
/// to different animation sources can hold divergent static blend
/// states over one shared mesh. The carrier node records the source
/// animation index (`usd:skel:weightsAnim`) for the writer's
/// `skel:animationSource` re-authoring.
///
/// Scalar channel weights are stored **verbatim** — §1.4.1 inbetween
/// resolution lives on the typed `MorphTarget::inbetweens` roster
/// and is evaluated by the typed model at sample time.
fn attach_blend_weight_channels(ctx: &mut Ctx) {
    let pending = std::mem::take(&mut ctx.pending_blend_weights);
    for entry in &pending {
        for node_idx in 0..ctx.scene.nodes.len() {
            let Some(mesh_id) = ctx.scene.nodes[node_idx].mesh else {
                continue;
            };
            let Some(mesh) = ctx.scene.meshes.get(mesh_id.0 as usize) else {
                continue;
            };
            let Some(prim) = mesh.primitives.first() else {
                continue;
            };
            let mesh_names = &mesh.target_names;
            if mesh_names.is_empty() || !mesh_names.iter().any(|n| entry.channel_names.contains(n))
            {
                continue;
            }
            // §1.5 scoping: an authored/inherited animation source
            // on the geometry names THE animation that drives it.
            if let Some(source) = prim
                .extras
                .get("usd:skel:animationSource")
                .and_then(|v| v.as_str())
            {
                if source != entry.anim_path {
                    continue;
                }
            }
            // Per-frame weight vector in mesh channel order.
            let remap = |frame: &[f32]| -> Vec<f32> {
                mesh_names
                    .iter()
                    .map(|name| {
                        entry
                            .channel_names
                            .iter()
                            .position(|c| c == name)
                            .and_then(|i| frame.get(i).copied())
                            .unwrap_or(0.0)
                    })
                    .collect()
            };
            // Static state → `Node::weights` (no animation channel).
            if entry.static_default {
                let Some(frame) = entry.frames.first() else {
                    continue;
                };
                let weights = remap(frame);
                let node = &mut ctx.scene.nodes[node_idx];
                node.weights = weights;
                node.extras.insert(
                    "usd:skel:weightsAnim".into(),
                    serde_json::Value::Number(entry.anim_idx.into()),
                );
                continue;
            }
            let frames: Vec<Vec<f32>> = entry.frames.iter().map(|f| remap(f)).collect();
            let Some(sampler) = oxideav_mesh3d::AnimationSampler::morph_weights(
                entry.times.clone(),
                frames,
                oxideav_mesh3d::Interpolation::Linear,
            ) else {
                // Structurally unusable table (no keyframes / not
                // strictly increasing) — nothing to attach.
                continue;
            };
            if let Some(anim) = ctx.scene.animations.get_mut(entry.anim_idx) {
                anim.channels.push(oxideav_mesh3d::AnimationChannel::new(
                    oxideav_mesh3d::NodeId(node_idx as u32),
                    oxideav_mesh3d::AnimationProperty::MorphWeights,
                    sampler,
                ));
            }
        }
    }
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

    apply_material_binding(ctx, path, prim, &mut prim_out, false);

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

    apply_material_binding(ctx, path, prim, &mut prim_out, false);

    apply_mesh_metadata_to_primitive(prim, &mut prim_out);
    Ok(prim_out)
}

/// Resolve a gprim's effective material binding (staged schema
/// §3.1–§3.4: purpose forms, `bindMaterialAs` strength, rule-1
/// namespace inheritance from the enclosing containers, and §15
/// collection bindings) into the typed material slot, and preserve
/// the authored relationship set for the writer.
///
/// The verbatim `usd:materialBindings` stash is written whenever
/// the authored opinions differ from the single synthetic
/// `rel material:binding = </resolved>` the writer would emit from
/// `Primitive::material` — including the **empty** stash, which
/// marks "this gprim authored nothing; the binding came from an
/// ancestor; emit no gprim-level relationship".
fn apply_material_binding(
    ctx: &mut Ctx,
    path: &str,
    prim: &Prim,
    out: &mut Primitive,
    double_sided: bool,
) {
    let own_level = read_binding_level(prim);
    let mut levels: Vec<PrimBindingLevel> = ctx.binding_levels.clone();
    levels.push(own_level);
    let resolved = resolve_binding(&levels, &ctx.collections, path);
    if let Some(path) = &resolved {
        if let Some(&mid) = ctx.materials_by_path.get(path) {
            out.material = Some(mid);
            if double_sided {
                if let Some(mat) = ctx.scene.materials.get_mut(mid.0 as usize) {
                    mat.double_sided = true;
                }
            }
        }
    }
    let authored = collect_binding_attrs(prim);
    // `bindings_are_synthetic` also covers the ancestor-resolved
    // case (authored empty + resolved Some ⇒ stash an empty set so
    // no synthetic gprim-level relationship appears on re-encode).
    if !bindings_are_synthetic(&authored, resolved.as_deref()) {
        out.extras
            .insert("usd:materialBindings".into(), binding_stash_json(&authored));
    }
    // §15.1 CollectionAPI properties authored on the gprim itself
    // round-trip verbatim.
    let authored_collections = collect_collection_attrs(prim);
    if !authored_collections.is_empty() {
        out.extras.insert(
            "usd:collections".into(),
            binding_stash_json(&authored_collections),
        );
    }
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
///
/// Full §2.1 input coverage (staged schema tables,
/// `docs/3d/usd/usdskel-usdpreviewsurface-schema.md`):
///
/// * `diffuseColor` / `emissiveColor` / `metallic` / `roughness` /
///   `opacity` — the round-1 core set, mapped onto the matching
///   [`Material`] factor slots.
/// * `opacityThreshold` — `> 0` selects cutout semantics: fragments
///   with `opacity < threshold` are discarded, the rest render
///   opaque. Maps onto [`AlphaMode::Mask`] with the threshold as
///   `cutoff`. `0` (the default) disables cutout; an authored
///   `opacity < 1` then selects [`AlphaMode::Blend`].
/// * `useSpecularWorkflow = 1` + `specularColor` (+ its texture
///   connection) — the typed
///   [`MaterialExt::specular`](oxideav_mesh3d::MaterialExt::specular)
///   slot; `Some` is exactly "the specular workflow is active". An
///   inert `specularColor` (workflow off) stays on
///   `extras["usd:inputs:specularColor"]` / `extras["usd:tex:specularColor"]`.
/// * `clearcoat` / `clearcoatRoughness` (+ their texture
///   connections) — the typed
///   [`MaterialExt::clearcoat`](oxideav_mesh3d::MaterialExt::clearcoat)
///   lobe, materialised only when at least one clearcoat opinion is
///   authored (unauthored inputs inside it take the schema
///   defaults).
/// * `ior` — the typed `Option`-shaped
///   [`MaterialExt::ior`](oxideav_mesh3d::MaterialExt::ior) slot.
/// * `displacement` — scalar input preserved on
///   `extras["usd:inputs:displacement"]`, so only an authored
///   opinion re-emits.
/// * `occlusion` — constant ambient-occlusion multiplier, mapped
///   onto [`Material::occlusion_strength`].
/// * constant (non-default) `normal` — preserved on
///   `extras["usd:inputs:normal"]`.
///
/// Texture connections resolve for every input; slots without a
/// typed texture home (opacity / displacement / a roughness map
/// that differs from the metallic map) round-trip through
/// `extras["usd:tex:<input>"] = {"texture": N, "uv_set": M}`.
/// `metallic.connect` / `roughness.connect` share the glTF-packed
/// [`Material::metallic_roughness_texture`] slot; which of the two
/// inputs was authored is recorded on `extras["usd:mr_connect"]`
/// (`"metallic"` / `"roughness"` / `"both"`) so the writer re-emits
/// exactly the authored connections.
fn apply_preview_surface(
    ctx: &mut Ctx,
    mat: &mut Material,
    parent_path: &str,
    parent: &Prim,
    surface: &Prim,
) -> Result<()> {
    let scalar = |name: &str| surface.attrs.get(name).and_then(|a| a.value.as_f32());
    let color = |name: &str| {
        surface
            .attrs
            .get(name)
            .and_then(|a| a.value.as_floatn::<3>())
    };

    if let Some(c) = color("inputs:diffuseColor") {
        mat.base_color = [c[0], c[1], c[2], mat.base_color[3]];
    }
    if let Some(f) = scalar("inputs:metallic") {
        mat.metallic = f;
    } else {
        // USDPreviewSurface defaults to non-metallic — override the
        // glTF "fully metallic" sentinel only when we know the shader
        // is in play.
        mat.metallic = 0.0;
    }
    if let Some(f) = scalar("inputs:roughness") {
        mat.roughness = f;
    } else {
        mat.roughness = 0.5;
    }
    if let Some(c) = color("inputs:emissiveColor") {
        mat.emissive_factor = c;
    }
    if let Some(f) = scalar("inputs:opacity") {
        mat.base_color[3] = f;
    }

    // Alpha-coverage mode. Schema §2.1: `opacityThreshold > 0` is
    // cutout — fragments below the threshold are discarded, the
    // rest stay opaque (mask). With cutout disabled, a sub-1
    // opacity is a straight blend.
    let threshold = scalar("inputs:opacityThreshold").unwrap_or(0.0);
    if threshold > 0.0 {
        mat.alpha_mode = oxideav_mesh3d::AlphaMode::Mask { cutoff: threshold };
    } else if mat.base_color[3] < 1.0 {
        mat.alpha_mode = oxideav_mesh3d::AlphaMode::Blend;
    }

    // Specular workflow (`useSpecularWorkflow = 1`): `specularColor`
    // is the reflectivity at normal incidence. An active workflow
    // lands on the typed
    // [`MaterialExt::specular`](oxideav_mesh3d::MaterialExt::specular)
    // slot — `Some` is exactly "the source selected the specular
    // workflow", with the authored color (or the schema default
    // black) as the F0 `color_factor` and the `specularColor.connect`
    // map as `color_texture`. `Specular::factor` has no USD
    // counterpart and stays at its neutral default 1. A
    // `specularColor` authored while the workflow is off is inert
    // per schema §2.1 ("ignored when `useSpecularWorkflow = 0`") and
    // keeps riding on extras so the writer re-authors it without
    // activating the workflow.
    let use_specular = scalar("inputs:useSpecularWorkflow")
        .map(|f| f as i32)
        .unwrap_or(0);
    let specular_color = color("inputs:specularColor");
    let specular_tex = connect_input(ctx, mat, parent_path, parent, surface, "specularColor")?;
    if use_specular == 1 {
        mat.ext.specular = Some(oxideav_mesh3d::Specular {
            color_factor: specular_color.unwrap_or([0.0, 0.0, 0.0]),
            color_texture: specular_tex,
            ..Default::default()
        });
    } else {
        if let Some(sc) = specular_color {
            mat.extras
                .insert("usd:inputs:specularColor".into(), f32s_to_json(&sc));
        }
        if let Some(tref) = specular_tex {
            mat.extras
                .insert("usd:tex:specularColor".into(), texref_to_json(tref));
        }
    }

    // Index of refraction — typed extension slot. The typed model's
    // `MaterialExt::ior` is `Option`-shaped, so an unauthored input
    // stays `None` (the schema default 1.5 is the consumer's job via
    // `effective_ior()`), and only an authored opinion re-emits.
    if let Some(f) = scalar("inputs:ior") {
        mat.ext.ior = Some(f);
    }

    // Clearcoat lobe — typed extension slot. The lobe materialises
    // only when the source authored at least one clearcoat opinion
    // (either scalar or texture connection); unauthored inputs inside
    // it take the schema §2.1 defaults (`clearcoat` 0, `clearcoatRoughness`
    // 0.01) so a consumer reading the struct gets the value the shader
    // would evaluate. The writer re-emits only non-default values, so
    // no synthetic opinion lands in the output file.
    let cc_factor = scalar("inputs:clearcoat");
    let cc_roughness = scalar("inputs:clearcoatRoughness");
    let cc_factor_tex = connect_input(ctx, mat, parent_path, parent, surface, "clearcoat")?;
    let cc_roughness_tex =
        connect_input(ctx, mat, parent_path, parent, surface, "clearcoatRoughness")?;
    if cc_factor.is_some()
        || cc_roughness.is_some()
        || cc_factor_tex.is_some()
        || cc_roughness_tex.is_some()
    {
        mat.ext.clearcoat = Some(oxideav_mesh3d::Clearcoat {
            factor: cc_factor.unwrap_or(0.0),
            factor_texture: cc_factor_tex,
            roughness: cc_roughness.unwrap_or(0.01),
            roughness_texture: cc_roughness_tex,
            ..Default::default()
        });
    }

    // Displacement — scalar input preserved per-input so only the
    // authored opinion re-emits (no typed slot in the model).
    for input in ["displacement"] {
        if let Some(f) = scalar(&format!("inputs:{input}")) {
            if let Some(n) = serde_json::Number::from_f64(f as f64) {
                mat.extras
                    .insert(format!("usd:inputs:{input}"), serde_json::Value::Number(n));
            }
        }
    }

    if let Some(o) = scalar("inputs:occlusion") {
        mat.occlusion_strength = o;
    }
    // A constant (unconnected) normal opinion other than the
    // unperturbed default (0, 0, 1) has no typed slot — preserve it.
    if !surface.attrs.contains_key("inputs:normal.connect") {
        if let Some(nrm) = color("inputs:normal") {
            if nrm != [0.0, 0.0, 1.0] {
                mat.extras.insert(
                    "usd:inputs:normal".into(),
                    serde_json::Value::Array(
                        nrm.iter()
                            .filter_map(|c| {
                                serde_json::Number::from_f64(*c as f64)
                                    .map(serde_json::Value::Number)
                            })
                            .collect(),
                    ),
                );
            }
        }
    }

    // Texture connections — `inputs:diffuseColor.connect = </path/to/Tex.outputs:rgb>`.
    if let Some(tex_ref) = connect_input(ctx, mat, parent_path, parent, surface, "diffuseColor")? {
        mat.base_color_texture = Some(tex_ref);
    }
    if let Some(tex_ref) = connect_input(ctx, mat, parent_path, parent, surface, "normal")? {
        mat.normal_texture = Some(tex_ref);
    }
    if let Some(tex_ref) = connect_input(ctx, mat, parent_path, parent, surface, "emissiveColor")? {
        mat.emissive_texture = Some(tex_ref);
    }
    if let Some(tex_ref) = connect_input(ctx, mat, parent_path, parent, surface, "occlusion")? {
        mat.occlusion_texture = Some(tex_ref);
    }

    // `metallic` / `roughness` maps share the glTF-packed slot; the
    // authored-input record lets the writer re-emit exactly what the
    // source wired.
    let metallic_tex = connect_input(ctx, mat, parent_path, parent, surface, "metallic")?;
    let roughness_tex = connect_input(ctx, mat, parent_path, parent, surface, "roughness")?;
    match (metallic_tex, roughness_tex) {
        (Some(m), Some(r)) if m == r => {
            mat.metallic_roughness_texture = Some(m);
            mat.extras.insert(
                "usd:mr_connect".into(),
                serde_json::Value::String("both".into()),
            );
        }
        (Some(m), Some(r)) => {
            // Two *different* textures — keep the metallic map in the
            // typed slot, preserve the roughness map on extras.
            mat.metallic_roughness_texture = Some(m);
            mat.extras.insert(
                "usd:mr_connect".into(),
                serde_json::Value::String("metallic".into()),
            );
            mat.extras
                .insert("usd:tex:roughness".into(), texref_to_json(r));
        }
        (Some(m), None) => {
            mat.metallic_roughness_texture = Some(m);
            mat.extras.insert(
                "usd:mr_connect".into(),
                serde_json::Value::String("metallic".into()),
            );
        }
        (None, Some(r)) => {
            mat.metallic_roughness_texture = Some(r);
            mat.extras.insert(
                "usd:mr_connect".into(),
                serde_json::Value::String("roughness".into()),
            );
        }
        (None, None) => {}
    }

    // Inputs with no typed texture slot round-trip via extras.
    for input in ["opacity", "displacement"] {
        if let Some(tex_ref) = connect_input(ctx, mat, parent_path, parent, surface, input)? {
            mat.extras
                .insert(format!("usd:tex:{input}"), texref_to_json(tex_ref));
        }
    }
    Ok(())
}

/// JSON array of numbers from an `f32` slice (extras stash shape for
/// color / vector shader inputs).
fn f32s_to_json(xs: &[f32]) -> serde_json::Value {
    serde_json::Value::Array(
        xs.iter()
            .filter_map(|c| serde_json::Number::from_f64(*c as f64).map(serde_json::Value::Number))
            .collect(),
    )
}

/// JSON shape for a [`TextureRef`] preserved on `Material::extras`
/// (`usd:tex:<input>` slots): `{"texture": N, "uv_set": M}`.
fn texref_to_json(tref: TextureRef) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "texture".into(),
        serde_json::Value::Number(tref.texture.0.into()),
    );
    obj.insert(
        "uv_set".into(),
        serde_json::Value::Number(tref.uv_set.into()),
    );
    serde_json::Value::Object(obj)
}

/// A resolved `inputs:<x>.connect` target under a material.
enum ShaderConnect {
    /// A `UsdUVTexture` (§2.2) — the input samples a texture.
    Texture(TextureRef),
    /// A `UsdPrimvarReader_<T>` (§2.3) — the input reads a primvar
    /// off the bound geometry. Carried as the writer-ready stash
    /// object (`type` / `varname` / `varname_type` /
    /// `fallback` + `fallback_type`).
    PrimvarReader(serde_json::Value),
}

/// The ten §2.3 `UsdPrimvarReader_<T>` typed variants.
const PRIMVAR_READER_VARIANTS: [&str; 10] = [
    "float", "float2", "float3", "float4", "int", "string", "normal", "point", "vector", "matrix",
];

/// Resolve a preview-surface `inputs:<input>.connect` for `mat`:
/// a `UsdUVTexture` target comes back as a [`TextureRef`]; a
/// `UsdPrimvarReader_<T>` target (§2.3 — e.g. `diffuseColor`
/// reading `displayColor` off the geometry) is preserved on
/// `mat.extras["usd:primvar:<input>"]` for the writer's replay and
/// returns `None` (the typed model has no primvar-driven material
/// slot).
fn connect_input(
    ctx: &mut Ctx,
    mat: &mut Material,
    parent_path: &str,
    parent: &Prim,
    surface: &Prim,
    input: &str,
) -> Result<Option<TextureRef>> {
    let attr = surface
        .attrs
        .get(format!("inputs:{input}.connect").as_str());
    match resolve_shader_connect(ctx, parent_path, parent, attr)? {
        Some(ShaderConnect::Texture(tref)) => Ok(Some(tref)),
        Some(ShaderConnect::PrimvarReader(stash)) => {
            mat.extras.insert(format!("usd:primvar:{input}"), stash);
            Ok(None)
        }
        None => Ok(None),
    }
}

/// Resolve an `inputs:foo.connect = </path/Tex.outputs:rgb>`
/// statement into a [`ShaderConnect`], materialising the underlying
/// `Texture` (and its `ZipStoredAsset`) on first sight of a
/// `UsdUVTexture`, or capturing a `UsdPrimvarReader_<T>`'s authored
/// inputs (§2.3). Any other `info:id` is refused.
///
/// Full §2.2 `UsdUVTexture` input coverage (staged schema tables):
///
/// * `file` — the in-archive texture asset (round 1).
/// * `wrapS` / `wrapT` — mapped onto the typed
///   [`Sampler`](oxideav_mesh3d::Sampler) wrap modes (`repeat` /
///   `clamp` / `mirror`). `black` and `useMetadata` have no typed
///   equivalent and keep the default `Repeat`; the authored token
///   always rides on the scene-level stash (below) so the writer
///   re-emits the exact spelling.
/// * `st` — when connected to a `UsdPrimvarReader_float2` (§2.3),
///   the reader's `varname` selects the UV set: `st` → set 0,
///   `st1` → set 1, `st<N>` → set N. Any other primvar name keeps
///   set 0 and is preserved on the stash under `"varname"`.
/// * `scale` / `bias` / `fallback` (`float4`) and
///   `sourceColorSpace` (token) — no typed slot; preserved on the
///   stash.
///
/// The stash lives on
/// `Scene3D::extras["usd:uvtexture:<TextureId>"]` as a JSON object
/// of the authored inputs, and the writer replays each entry onto
/// the emitted `UsdUVTexture` shader.
fn resolve_shader_connect(
    ctx: &mut Ctx,
    parent_path: &str,
    parent: &Prim,
    attr: Option<&Attr>,
) -> Result<Option<ShaderConnect>> {
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
    if let Some(&tref) = ctx.textures_by_path.get(prim_path) {
        return Ok(Some(ShaderConnect::Texture(tref)));
    }
    // Locate the shader prim under the enclosing material.
    let Some(rel) = prim_path.strip_prefix(&format!("{parent_path}/")) else {
        return Err(unsupported(format!(
            "texture shader path `{prim_path}` is outside material `{parent_path}` (cross-material refs deferred to round 2)"
        )));
    };
    let shader = find_child_by_name(parent, rel).ok_or_else(|| {
        invalid(format!(
            "connected shader `{rel}` not found under material `{parent_path}`"
        ))
    })?;
    let info_id = shader
        .attrs
        .get("info:id")
        .and_then(|a| a.value.as_text())
        .unwrap_or_default();
    // §2.3 UsdPrimvarReader_<T> — the input reads a primvar off the
    // bound geometry. Preserve the reader's authored inputs (variant
    // type, `varname` with its type spelling, `fallback` as an exact
    // USDA literal) so the writer re-emits the reader prim verbatim.
    if let Some(variant) = info_id.strip_prefix("UsdPrimvarReader_") {
        if PRIMVAR_READER_VARIANTS.contains(&variant) {
            let mut stash = serde_json::Map::new();
            stash.insert(
                "type".into(),
                serde_json::Value::String(variant.to_string()),
            );
            if let Some(vn_attr) = shader.attrs.get("inputs:varname") {
                if let Some(vn) = vn_attr.value.as_text() {
                    stash.insert("varname".into(), serde_json::Value::String(vn.to_string()));
                    stash.insert(
                        "varname_type".into(),
                        serde_json::Value::String(vn_attr.type_token.clone()),
                    );
                }
            }
            if let Some(fb) = shader.attrs.get("inputs:fallback") {
                stash.insert(
                    "fallback".into(),
                    serde_json::Value::String(crate::usda_writer::format_metadata_value(&fb.value)),
                );
                stash.insert(
                    "fallback_type".into(),
                    serde_json::Value::String(fb.type_token.clone()),
                );
            }
            return Ok(Some(ShaderConnect::PrimvarReader(
                serde_json::Value::Object(stash),
            )));
        }
    }
    if info_id != "UsdUVTexture" {
        return Err(unsupported(format!(
            "shader `{rel}` has info:id `{info_id}` — only `UsdUVTexture` and `UsdPrimvarReader_<T>` connections are supported"
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
    // §2.2: a `file` path carrying the `<UDIM>` token names a UDIM
    // *tile set* — no single archive entry can match it, so the
    // texture is preserved as an external URI reference (the consumer
    // expands the tile numbering at load). Anything else must
    // resolve inside the archive (USDZ is self-contained).
    let is_udim = asset_path.contains("<UDIM>");
    let (image, entry_name): (ImageData, &str) = if is_udim {
        (
            ImageData::External {
                uri: asset_path.to_string(),
                mime: mime_from_filename(asset_path),
            },
            asset_path,
        )
    } else {
        let entry = lookup_zip_entry(&ctx.entries, asset_path).ok_or_else(|| {
            invalid(format!(
                "UsdUVTexture `{prim_path}` references `{asset_path}` which is not present in the USDZ archive"
            ))
        })?;
        let mime = mime_from_filename(&entry.name);
        let asset: Arc<dyn AssetSource> = Arc::new(ZipStoredAsset::new(
            ctx.archive.clone(),
            entry.payload_offset,
            entry.payload_len,
            mime,
        ));
        (ImageData::Source(asset), entry.name.as_str())
    };

    // §2.2 wrap modes → typed sampler. `black` / `useMetadata` have
    // no Sampler equivalent; the stash below preserves the token.
    let wrap_token = |name: &str| shader.attrs.get(name).and_then(|a| a.value.as_text());
    let map_wrap = |tok: Option<&str>| match tok {
        Some("clamp") => oxideav_mesh3d::WrapMode::ClampToEdge,
        Some("mirror") => oxideav_mesh3d::WrapMode::MirroredRepeat,
        // `repeat` explicit, `black`, `useMetadata`, or unauthored.
        _ => oxideav_mesh3d::WrapMode::Repeat,
    };
    let mut sampler = oxideav_mesh3d::Sampler::default_sampler();
    sampler.wrap_s = map_wrap(wrap_token("inputs:wrapS"));
    sampler.wrap_t = map_wrap(wrap_token("inputs:wrapT"));

    // §2.3 UsdPrimvarReader wiring: the `st` connection's `varname`
    // picks which UV set this texture samples.
    let (uv_set, nonstandard_varname) = resolve_uv_set(parent_path, parent, shader);

    // Stash the no-typed-slot inputs for the writer's replay.
    let mut stash = serde_json::Map::new();
    for key in ["wrapS", "wrapT", "sourceColorSpace"] {
        if let Some(tok) = wrap_token(&format!("inputs:{key}")) {
            stash.insert(key.into(), serde_json::Value::String(tok.to_string()));
        }
    }
    for key in ["scale", "bias", "fallback"] {
        if let Some(v4) = shader
            .attrs
            .get(&format!("inputs:{key}"))
            .and_then(|a| a.value.as_floatn::<4>())
        {
            stash.insert(
                key.into(),
                serde_json::Value::Array(
                    v4.iter()
                        .filter_map(|c| {
                            serde_json::Number::from_f64(*c as f64).map(serde_json::Value::Number)
                        })
                        .collect(),
                ),
            );
        }
    }
    if let Some(varname) = nonstandard_varname {
        stash.insert("varname".into(), serde_json::Value::String(varname));
    }

    // Name the texture after its archive entry's basename stem, not
    // the shader prim name: the writer derives the output entry name
    // (and the `inputs:file` reference) from `Texture::name`, so the
    // stem keeps the original asset filename stable across a
    // decode → encode repack. It also makes the text round trip a
    // one-cycle fixed point — a re-decoded scene reproduces the same
    // stem, where the prim-name scheme drifted onto the writer's
    // `Texture_<id>` prim name on the second cycle. (An External
    // image re-emits its full URI verbatim; the stem name is
    // cosmetic there.)
    let base = entry_name.rsplit('/').next().unwrap_or(entry_name);
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    let texture = Texture {
        name: Some(if stem.is_empty() { rel } else { stem }.to_string()),
        image,
        sampler,
    };
    let tex_id = ctx.scene.add_texture(texture);
    if !stash.is_empty() {
        ctx.scene.extras.insert(
            format!("usd:uvtexture:{}", tex_id.0),
            serde_json::Value::Object(stash),
        );
    }
    // No `TextureTransform` is ever attached on decode: the staged
    // schema (§2.2) gives `UsdUVTexture` no UV-transform inputs (its
    // `scale`/`bias` are per-channel *color* affines, not coordinate
    // transforms), and no 2D-transform node schema is staged in
    // `docs/3d/usd/`.
    let tref = TextureRef {
        texture: tex_id,
        uv_set,
        transform: None,
    };
    ctx.textures_by_path.insert(prim_path.to_string(), tref);
    Ok(Some(ShaderConnect::Texture(tref)))
}

/// Follow a `UsdUVTexture`'s `inputs:st.connect` to its
/// `UsdPrimvarReader_float2` sibling and map the reader's `varname`
/// onto a UV-set index: `st` → 0, `st1` → 1, `st<N>` → N (§2.5's
/// multi-UV convention). Returns `(uv_set, nonstandard_varname)` —
/// the second slot carries a primvar name outside the `st<N>` family
/// so the caller can preserve the exact spelling.
fn resolve_uv_set(parent_path: &str, parent: &Prim, shader: &Prim) -> (u32, Option<String>) {
    let Some(Value::Path(p)) = shader.attrs.get("inputs:st.connect").map(|a| &a.value) else {
        return (0, None);
    };
    let reader_path = match p.find('.') {
        Some(i) => &p[..i],
        None => p.as_str(),
    };
    let Some(rel) = reader_path.strip_prefix(&format!("{parent_path}/")) else {
        return (0, None);
    };
    let Some(reader) = find_child_by_name(parent, rel) else {
        return (0, None);
    };
    let info_id = reader
        .attrs
        .get("info:id")
        .and_then(|a| a.value.as_text())
        .unwrap_or_default();
    if !info_id.starts_with("UsdPrimvarReader") {
        return (0, None);
    }
    let Some(varname) = reader
        .attrs
        .get("inputs:varname")
        .and_then(|a| a.value.as_text())
    else {
        return (0, None);
    };
    match uv_set_from_varname(varname) {
        Some(n) => (n, None),
        None => (0, Some(varname.to_string())),
    }
}

/// `st` → 0, `st1` → 1, `st2` → 2, ... `None` for any name outside
/// the `st<N>` family.
fn uv_set_from_varname(varname: &str) -> Option<u32> {
    let rest = varname.strip_prefix("st")?;
    if rest.is_empty() {
        return Some(0);
    }
    rest.parse::<u32>().ok()
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
        // Time-sample maps flatten to a JSON object keyed by the
        // timeCode's decimal string so an extras consumer can read
        // each sample; the typed round-trip path goes through
        // `variant_codec` which preserves order + f64 keys exactly.
        Value::TimeSamples(samples) => J::Object(
            samples
                .iter()
                .map(|(t, v)| (format!("{t}"), value_to_json(v).unwrap_or(J::Null)))
                .collect(),
        ),
        // `references = @file@</Prim>` — flatten to "asset</prim>"
        // for the JSON view; consumers wanting the parts can walk
        // the prim tree directly.
        Value::AssetWithPath { asset, prim_path } => J::String(format!("{asset}<{prim_path}>")),
        // A list-edited field flattens to a JSON object keyed by
        // operator so an extras consumer can still see each authored
        // sublist (`{"prepend": …, "delete": …}`).
        Value::ListOp(list) => {
            let mut obj = serde_json::Map::new();
            for (op, sublist) in list.entries() {
                if let Some(keyword) = op.keyword().or(Some("explicit")) {
                    if let Some(j) = value_to_json(sublist) {
                        obj.insert(keyword.to_string(), j);
                    }
                }
            }
            J::Object(obj)
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
