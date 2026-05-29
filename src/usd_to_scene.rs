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
    };

    apply_layer_metadata(&mut ctx.scene, &layer.metadata);

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
    // a higher-level pipeline (mirroring the ArResolver pre-resolve
    // step described in `spec_usdz.html` §USD Constraints).
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
    let resolved: Vec<Prim> = merged_layer
        .prims
        .iter()
        .map(resolve_variants_recursive)
        .collect();

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
    let raw = match target.metadata.get("subLayers") {
        Some(Value::Array(items)) => items.clone(),
        _ => return Ok(()),
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
/// Rules (per `spec_usdz.html` §USD Constraints):
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
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(one).collect(),
        Some(other) => one(other).into_iter().collect(),
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

/// A single resolved composition-arc target.
struct ArcTarget {
    /// `@...@` asset path portion.
    asset: String,
    /// `</...>` in-asset prim selector (empty when the arc targets
    /// the referenced layer's `defaultPrim`).
    prim_path: String,
}

/// Flatten a `references` / `payload` metadata value into an ordered
/// list of [`ArcTarget`]s. A scalar `Asset` / `AssetWithPath` yields
/// one target; an `Array` of them yields one per element (USD list
/// order = strength order). Any other shape (or a bare path-only
/// arc) yields nothing.
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
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(one).collect(),
        Some(other) => one(other).into_iter().collect(),
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
        // Round 9: also stash the metadata block in the lossless
        // tagged shape from [`crate::variant_codec`] so the writer can
        // reconstruct each value's USDA type-token (Token vs String vs
        // Asset vs AssetWithPath vs Path).  The untagged `usd:metadata`
        // entry above stays for callers that consume the JSON
        // directly without going back through the writer.
        let tagged = crate::variant_codec::encode_btree_value(&prim.metadata);
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
