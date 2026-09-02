//! Typed **composition record** — the opinion model that lets the
//! writer choose between *flattening* a composed scene into one layer
//! (the historical default) and *preserving* the authored composition
//! structure: sublayer / reference / payload / inherits / specializes
//! arcs, `class` prims, and the layer entries those arcs point at.
//!
//! The read path ([`crate::usd_to_scene::translate`]) evaluates every
//! in-archive arc and merges the targeted opinions into the local
//! prim tree before the typed `Scene3D` is built. That is the right
//! thing for a *consumer* — the typed model sees the composed result
//! — but it erases the authoring: the encoder had nothing left but
//! the flattened tree. The decoder now also records, on
//! `Scene3D::extras[`[`EXTRAS_KEY`]`]`, a [`CompositionRecord`]:
//!
//! * the **local prim tree skeleton** exactly as the root layer
//!   authored it, *before* any arc composed — specifier, schema,
//!   name, the `( ... )` metadata block (which is where the arcs
//!   live) and the child tree. `def` prims drop their attributes
//!   (the typed model carries them); non-`def` prims (`class`,
//!   `over`) are kept verbatim because the typed model has no slot
//!   for them at all;
//! * every **in-archive layer** the composition consumed (sublayers,
//!   referenced / payloaded layers, transitively), with its bytes,
//!   plus the non-layer assets those layers reference, so the
//!   preserving writer can re-emit the package entries verbatim;
//! * the **root layer's entry name**, so anchored (`./x.usda`)
//!   paths inside the re-emitted package resolve the way they did
//!   in the source package.
//!
//! A prim path present in the composed tree but absent from the
//! local skeleton is a **contributed** prim — it arrived through an
//! arc or a variant selection. The preserving writer skips those
//! (they come back through the re-authored arc on the next decode)
//! and re-authors the arc opinions from the skeleton, so
//! decode → encode → decode is a one-cycle fixed point on the
//! composed `Scene3D` *and* the emitted package keeps the source's
//! layer structure.
//!
//! [`CompositionMode::Flatten`] ignores the record entirely (apart
//! from pruning the consumed `subLayers` entries, which would
//! otherwise dangle), which is byte-for-byte the historical output.

use std::collections::BTreeMap;

use serde_json::{Map as JMap, Value as JValue};

use crate::usda::{Attr, Layer, Prim, Value};
use crate::variant_codec::{decode_btree_value, decode_prim, encode_btree_value, encode_prim};

/// `Scene3D::extras` key carrying the JSON-encoded [`CompositionRecord`].
pub const EXTRAS_KEY: &str = "usd:composition";

/// The four prim-level composition-arc metadata fields whose
/// authored value the preserving writer restores from the local
/// skeleton (the read path strips the consumed ones from the prim
/// metadata stash so the flattening writer does not re-author them).
pub const PRIM_ARC_KEYS: [&str; 4] = ["references", "payload", "inherits", "specializes"];

/// How the encoder treats composed structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompositionMode {
    /// Emit one self-contained layer with every composed opinion
    /// flattened in. Consumed arcs are not re-authored; in-archive
    /// sublayers the decoder folded in are pruned from `subLayers`.
    /// This is the default and the historical behaviour.
    #[default]
    Flatten,
    /// Re-author the source's composition structure from the
    /// [`CompositionRecord`]: arcs on their prims, `class` / `over`
    /// prims verbatim, the consumed layer entries (and their assets)
    /// copied into the package, the root layer keeping its entry
    /// name. Prims contributed by an arc or a variant selection are
    /// left to the arc. Falls back to [`Self::Flatten`] output when
    /// the scene carries no record.
    Preserve,
}

/// One package entry the composition consumed, with its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLayer {
    /// Archive entry name (e.g. `geom.usda`, `props/Marble.usd`).
    pub path: String,
    /// The entry's payload, verbatim.
    pub bytes: Vec<u8>,
    /// Non-layer entries (textures, audio) this layer references by
    /// an in-archive asset path, so a re-emitted package stays
    /// self-contained.
    pub assets: Vec<SourceAsset>,
}

/// A non-layer package entry referenced from a [`SourceLayer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAsset {
    /// Archive entry name.
    pub path: String,
    /// The entry's payload, verbatim.
    pub bytes: Vec<u8>,
}

/// The typed opinion model — see the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompositionRecord {
    /// Entry name of the root (default) layer in the source package.
    pub root_layer: String,
    /// Layer-level metadata of the root layer as authored (before
    /// sublayer metadata merged in).
    pub local_metadata: BTreeMap<String, Value>,
    /// Root prims of the local layer, pre-composition. `def` prims
    /// are skeletons (no attributes); other specifiers are verbatim.
    pub local: Vec<Prim>,
    /// Every in-archive layer the composition consumed.
    pub layers: Vec<SourceLayer>,
}

impl CompositionRecord {
    /// Build the record from the root layer as parsed (pre-composition).
    pub fn from_local_layer(root_layer: &str, layer: &Layer) -> Self {
        Self {
            root_layer: root_layer.to_owned(),
            local_metadata: layer.metadata.clone(),
            local: layer.prims.iter().map(skeleton).collect(),
            layers: Vec::new(),
        }
    }

    /// `true` when nothing in the record would change the writer's
    /// output — no consumed layers and a local tree made only of
    /// `def` prims without arcs. Such a record is not worth stashing.
    pub fn is_trivial(&self) -> bool {
        self.layers.is_empty() && !self.local.iter().any(prim_needs_record)
    }

    /// Look up the local (pre-composition) prim at an absolute prim
    /// path (`/Root/Child`).
    pub fn local_prim(&self, path: &str) -> Option<&Prim> {
        find_prim(&self.local, path)
    }

    /// `true` when the local layer authored a prim at `path` (with
    /// any specifier). A composed prim for which this is `false` was
    /// contributed by an arc or a variant selection.
    pub fn is_local(&self, path: &str) -> bool {
        self.local_prim(path).is_some()
    }

    /// The local prim's authored composition-arc opinions
    /// ([`PRIM_ARC_KEYS`]) at `path`, if any.
    pub fn local_arcs(&self, path: &str) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        if let Some(prim) = self.local_prim(path) {
            for key in PRIM_ARC_KEYS {
                if let Some(v) = prim.metadata.get(key) {
                    out.insert(key.to_owned(), v.clone());
                }
            }
        }
        out
    }

    /// Non-`def` children (`class` / `over`) the local layer authored
    /// directly under `path` (the root prims for `""` / `/`), verbatim.
    pub fn local_non_def_children(&self, path: &str) -> Vec<&Prim> {
        let siblings: &[Prim] = if path.is_empty() || path == "/" {
            &self.local
        } else {
            match self.local_prim(path) {
                Some(p) => &p.children,
                None => return Vec::new(),
            }
        };
        siblings.iter().filter(|p| p.spec != "def").collect()
    }

    /// Serialise to the JSON shape stored on `Scene3D::extras`.
    pub fn to_json(&self) -> JValue {
        let mut o = JMap::new();
        o.insert("rootLayer".into(), JValue::String(self.root_layer.clone()));
        if !self.local_metadata.is_empty() {
            o.insert(
                "localMetadata".into(),
                encode_btree_value(&self.local_metadata),
            );
        }
        o.insert(
            "local".into(),
            JValue::Array(self.local.iter().map(encode_prim).collect()),
        );
        o.insert(
            "layers".into(),
            JValue::Array(self.layers.iter().map(layer_to_json).collect()),
        );
        JValue::Object(o)
    }

    /// Inverse of [`Self::to_json`]. Malformed input yields an empty
    /// (trivial) record.
    pub fn from_json(value: &JValue) -> Self {
        let JValue::Object(obj) = value else {
            return Self::default();
        };
        Self {
            root_layer: obj
                .get("rootLayer")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            local_metadata: obj
                .get("localMetadata")
                .map(decode_btree_value)
                .unwrap_or_default(),
            local: obj
                .get("local")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().map(decode_prim).collect())
                .unwrap_or_default(),
            layers: obj
                .get("layers")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(layer_from_json).collect())
                .unwrap_or_default(),
        }
    }

    /// Read the record off a scene's extras, if present.
    pub fn from_extras(extras: &std::collections::HashMap<String, JValue>) -> Option<Self> {
        extras.get(EXTRAS_KEY).map(Self::from_json)
    }
}

/// A prim needs the record when it is not a plain `def`, carries an
/// arc opinion or a variant selection (whose contributed children
/// the preserving writer leaves to the variant body), or has such a
/// descendant.
fn prim_needs_record(p: &Prim) -> bool {
    p.spec != "def"
        || PRIM_ARC_KEYS.iter().any(|k| p.metadata.contains_key(*k))
        || p.metadata.contains_key("variants")
        || p.children.iter().any(prim_needs_record)
}

/// §10.5 namespace mapping for a composed arc: every prim / property
/// path authored inside the arc's target subtree that points at or
/// under `from` (the target prim's path in its own layer) is
/// rewritten to point at or under `to` (the path of the prim the
/// arc is authored on). Relationship targets and `.connect`
/// sources inside a referenced asset otherwise keep naming the
/// asset's own namespace, which no longer exists once the target
/// rides up under the referencing prim.
pub fn remap_paths(prim: &mut Prim, from: &str, to: &str) {
    if from == to || from.is_empty() {
        return;
    }
    fn map_one(p: &str, from: &str, to: &str) -> Option<String> {
        if p == from {
            return Some(to.to_owned());
        }
        let rest = p.strip_prefix(from)?;
        if rest.starts_with('/') || rest.starts_with('.') {
            Some(format!("{to}{rest}"))
        } else {
            None
        }
    }
    fn walk_value(v: &mut Value, from: &str, to: &str) {
        match v {
            Value::Path(p) => {
                if let Some(mapped) = map_one(p, from, to) {
                    *p = mapped;
                }
            }
            Value::Tuple(seq) | Value::Array(seq) => {
                seq.iter_mut().for_each(|x| walk_value(x, from, to))
            }
            Value::Dict(map) => map.values_mut().for_each(|x| walk_value(x, from, to)),
            Value::TimeSamples(samples) => samples
                .iter_mut()
                .for_each(|(_, x)| walk_value(x, from, to)),
            Value::ListOp(list) => {
                for x in [
                    &mut list.prepended,
                    &mut list.appended,
                    &mut list.deleted,
                    &mut list.explicit,
                    &mut list.reordered,
                ]
                .into_iter()
                .flatten()
                {
                    walk_value(x, from, to);
                }
            }
            _ => {}
        }
    }
    fn walk_attrs(attrs: &mut BTreeMap<String, Attr>, from: &str, to: &str) {
        for a in attrs.values_mut() {
            walk_value(&mut a.value, from, to);
            a.metadata
                .values_mut()
                .for_each(|v| walk_value(v, from, to));
        }
    }
    fn walk_prim(p: &mut Prim, from: &str, to: &str) {
        p.metadata
            .values_mut()
            .for_each(|v| walk_value(v, from, to));
        walk_attrs(&mut p.attrs, from, to);
        for set in p.variant_sets.values_mut() {
            for variant in set.values_mut() {
                variant
                    .metadata
                    .values_mut()
                    .for_each(|v| walk_value(v, from, to));
                walk_attrs(&mut variant.attrs, from, to);
                variant
                    .children
                    .iter_mut()
                    .for_each(|c| walk_prim(c, from, to));
            }
        }
        p.children.iter_mut().for_each(|c| walk_prim(c, from, to));
    }
    walk_prim(prim, from, to);
}

/// Skeleton of a local prim: `def` prims lose their attributes and
/// variant sets (the typed model / `usd:variantSets` carry them);
/// any other specifier is kept verbatim.
fn skeleton(p: &Prim) -> Prim {
    if p.spec != "def" {
        return p.clone();
    }
    Prim {
        spec: p.spec.clone(),
        type_name: p.type_name.clone(),
        name: p.name.clone(),
        metadata: p.metadata.clone(),
        attrs: BTreeMap::new(),
        children: p.children.iter().map(skeleton).collect(),
        variant_sets: BTreeMap::new(),
    }
}

fn find_prim<'a>(roots: &'a [Prim], path: &str) -> Option<&'a Prim> {
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

/// Collect every in-archive asset path a layer references through
/// `@...@` values (attributes, attribute metadata, prim metadata,
/// variant bodies), resolved against the layer's own entry name.
/// Layer targets (`.usd` / `.usda` / `.usdc`) are excluded — they are
/// recorded as [`SourceLayer`]s in their own right.
pub fn referenced_asset_paths(
    layer: &Layer,
    anchor: &str,
    resolve: &dyn Fn(&str, Option<&str>) -> Option<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |asset: &str| {
        if let Some(resolved) = resolve(asset, Some(anchor)) {
            let lower = resolved.to_ascii_lowercase();
            if lower.ends_with(".usd") || lower.ends_with(".usda") || lower.ends_with(".usdc") {
                return;
            }
            if !out.contains(&resolved) {
                out.push(resolved);
            }
        }
    };
    fn walk_value(v: &Value, push: &mut dyn FnMut(&str)) {
        match v {
            Value::Asset(a) => push(a),
            Value::AssetWithPath { asset, .. } => push(asset),
            Value::Tuple(seq) | Value::Array(seq) => seq.iter().for_each(|x| walk_value(x, push)),
            Value::Dict(map) => map.values().for_each(|x| walk_value(x, push)),
            Value::TimeSamples(samples) => samples.iter().for_each(|(_, x)| walk_value(x, push)),
            Value::ListOp(list) => list.entries().for_each(|(_, x)| walk_value(x, push)),
            _ => {}
        }
    }
    fn walk_attrs(attrs: &BTreeMap<String, Attr>, push: &mut dyn FnMut(&str)) {
        for a in attrs.values() {
            walk_value(&a.value, push);
            a.metadata.values().for_each(|v| walk_value(v, push));
        }
    }
    fn walk_prim(p: &Prim, push: &mut dyn FnMut(&str)) {
        p.metadata.values().for_each(|v| walk_value(v, push));
        walk_attrs(&p.attrs, push);
        for set in p.variant_sets.values() {
            for variant in set.values() {
                variant.metadata.values().for_each(|v| walk_value(v, push));
                walk_attrs(&variant.attrs, push);
                variant.children.iter().for_each(|c| walk_prim(c, push));
            }
        }
        p.children.iter().for_each(|c| walk_prim(c, push));
    }
    layer
        .metadata
        .values()
        .for_each(|v| walk_value(v, &mut push));
    layer.prims.iter().for_each(|p| walk_prim(p, &mut push));
    out
}

/// Rewrite every in-archive asset reference inside `prim` (attribute
/// values, attribute / prim metadata, variant bodies, children) from
/// its anchored spelling to the archive entry name `resolve` maps it
/// to. Applied to a subtree an arc composes into another layer: the
/// authoring layer's directory — which anchored `./x.png` /
/// `../tex/x.png` paths were relative to — is not where the merged
/// prim ends up. Paths `resolve` cannot map (external, cross-package)
/// are left untouched.
pub fn rebase_assets(prim: &mut Prim, resolve: &dyn Fn(&str) -> Option<String>) {
    fn fix(asset: &mut String, resolve: &dyn Fn(&str) -> Option<String>) {
        if let Some(entry) = resolve(asset) {
            *asset = entry;
        }
    }
    fn walk_value(v: &mut Value, resolve: &dyn Fn(&str) -> Option<String>) {
        match v {
            Value::Asset(a) => fix(a, resolve),
            Value::AssetWithPath { asset, .. } => fix(asset, resolve),
            Value::Tuple(seq) | Value::Array(seq) => {
                seq.iter_mut().for_each(|x| walk_value(x, resolve))
            }
            Value::Dict(map) => map.values_mut().for_each(|x| walk_value(x, resolve)),
            Value::TimeSamples(samples) => {
                samples.iter_mut().for_each(|(_, x)| walk_value(x, resolve))
            }
            Value::ListOp(list) => {
                for x in [
                    &mut list.prepended,
                    &mut list.appended,
                    &mut list.deleted,
                    &mut list.explicit,
                    &mut list.reordered,
                ]
                .into_iter()
                .flatten()
                {
                    walk_value(x, resolve);
                }
            }
            _ => {}
        }
    }
    fn walk_attrs(attrs: &mut BTreeMap<String, Attr>, resolve: &dyn Fn(&str) -> Option<String>) {
        for a in attrs.values_mut() {
            walk_value(&mut a.value, resolve);
            a.metadata.values_mut().for_each(|v| walk_value(v, resolve));
        }
    }
    fn walk_prim(p: &mut Prim, resolve: &dyn Fn(&str) -> Option<String>) {
        p.metadata.values_mut().for_each(|v| walk_value(v, resolve));
        walk_attrs(&mut p.attrs, resolve);
        for set in p.variant_sets.values_mut() {
            for variant in set.values_mut() {
                variant
                    .metadata
                    .values_mut()
                    .for_each(|v| walk_value(v, resolve));
                walk_attrs(&mut variant.attrs, resolve);
                variant
                    .children
                    .iter_mut()
                    .for_each(|c| walk_prim(c, resolve));
            }
        }
        p.children.iter_mut().for_each(|c| walk_prim(c, resolve));
    }
    walk_prim(prim, resolve);
}

fn layer_to_json(l: &SourceLayer) -> JValue {
    let mut o = JMap::new();
    o.insert("path".into(), JValue::String(l.path.clone()));
    put_bytes(&mut o, &l.bytes);
    if !l.assets.is_empty() {
        o.insert(
            "assets".into(),
            JValue::Array(
                l.assets
                    .iter()
                    .map(|a| {
                        let mut ao = JMap::new();
                        ao.insert("path".into(), JValue::String(a.path.clone()));
                        put_bytes(&mut ao, &a.bytes);
                        JValue::Object(ao)
                    })
                    .collect(),
            ),
        );
    }
    JValue::Object(o)
}

fn layer_from_json(v: &JValue) -> Option<SourceLayer> {
    let obj = v.as_object()?;
    let path = obj.get("path")?.as_str()?.to_owned();
    let bytes = take_bytes(obj)?;
    let assets = obj
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let ao = e.as_object()?;
                    Some(SourceAsset {
                        path: ao.get("path")?.as_str()?.to_owned(),
                        bytes: take_bytes(ao)?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(SourceLayer {
        path,
        bytes,
        assets,
    })
}

/// Text layers are stored as a JSON string (readable, compact);
/// anything else as base64.
fn put_bytes(o: &mut JMap<String, JValue>, bytes: &[u8]) {
    match std::str::from_utf8(bytes) {
        Ok(text) if text.starts_with("#usda") => {
            o.insert("text".into(), JValue::String(text.to_owned()));
        }
        _ => {
            o.insert("base64".into(), JValue::String(base64_encode(bytes)));
        }
    }
}

fn take_bytes(o: &JMap<String, JValue>) -> Option<Vec<u8>> {
    if let Some(text) = o.get("text").and_then(|v| v.as_str()) {
        return Some(text.as_bytes().to_vec());
    }
    base64_decode(o.get("base64")?.as_str()?)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648 §4) with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Inverse of [`base64_encode`]; `None` on any non-alphabet byte or
/// a malformed group.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for group in bytes.chunks(4) {
        let pad = group.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || group[..4 - pad].contains(&b'=') {
            return None;
        }
        let mut n = 0u32;
        for &c in &group[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_every_padding_shape() {
        for len in 0..16usize {
            let bytes: Vec<u8> = (0..len as u8).map(|i| i.wrapping_mul(37) ^ 0xA5).collect();
            let enc = base64_encode(&bytes);
            assert_eq!(enc.len() % 4, 0);
            assert_eq!(base64_decode(&enc).unwrap(), bytes, "len {len}");
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert!(base64_decode("TQ=").is_none());
        assert!(base64_decode("T=Q=").is_none());
        assert!(base64_decode("****").is_none());
    }

    #[test]
    fn record_json_round_trips_skeleton_and_layers() {
        let layer = crate::usda::parse(
            br#"#usda 1.0
(
    subLayers = [@./geom.usda@]
)
class Xform "_Base" {
    float x = 1
}
def Xform "Root" (
    prepend references = @./a.usd@</A>
) {
    def Mesh "M" {
        int[] faceVertexCounts = [3]
    }
}
"#,
        )
        .unwrap();
        let mut rec = CompositionRecord::from_local_layer("scene.usda", &layer);
        rec.layers.push(SourceLayer {
            path: "geom.usda".into(),
            bytes: b"#usda 1.0\n".to_vec(),
            assets: vec![SourceAsset {
                path: "tex.png".into(),
                bytes: vec![0x89, b'P', b'N', b'G', 0, 1, 2],
            }],
        });
        // `def` prims are skeletons: the Mesh lost its attrs.
        assert!(rec.local_prim("/Root/M").unwrap().attrs.is_empty());
        // `class` prims are verbatim.
        assert_eq!(rec.local_prim("/_Base").unwrap().attrs.len(), 1);
        assert!(!rec.is_trivial());
        assert!(rec.is_local("/Root"));
        assert!(!rec.is_local("/Root/Contributed"));
        assert_eq!(rec.local_arcs("/Root").len(), 1);
        assert_eq!(rec.local_non_def_children("").len(), 1);

        let back = CompositionRecord::from_json(&rec.to_json());
        assert_eq!(back, rec);
    }

    #[test]
    fn remap_paths_rewrites_only_the_target_namespace() {
        let layer = crate::usda::parse(
            br#"#usda 1.0
def Xform "Set" {
    def Mesh "M" {
        rel material:binding = </Set/Looks/Paint>
        float inputs:x.connect = </Set/Looks/Paint.outputs:x>
        rel other = [</Settings/A>, </Set>]
    }
}
"#,
        )
        .unwrap();
        let mut prim = layer.prims[0].clone();
        remap_paths(&mut prim, "/Set", "/World/Stage");
        let m = &prim.children[0];
        assert_eq!(
            m.attrs["material:binding"].value,
            Value::Path("/World/Stage/Looks/Paint".into())
        );
        assert_eq!(
            m.attrs["inputs:x.connect"].value,
            Value::Path("/World/Stage/Looks/Paint.outputs:x".into())
        );
        // `/Settings/A` shares the prefix bytes but not the namespace.
        assert_eq!(
            m.attrs["other"].value,
            Value::Array(vec![
                Value::Path("/Settings/A".into()),
                Value::Path("/World/Stage".into())
            ])
        );
    }

    #[test]
    fn trivial_record_for_plain_def_tree() {
        let layer =
            crate::usda::parse(b"#usda 1.0\ndef Xform \"R\" { def Mesh \"M\" {} }\n").unwrap();
        assert!(CompositionRecord::from_local_layer("scene.usda", &layer).is_trivial());
    }

    #[test]
    fn referenced_assets_skip_layers_and_dedupe() {
        let layer = crate::usda::parse(
            br#"#usda 1.0
(
    subLayers = [@./geom.usda@]
)
def Xform "R" (
    references = @./a.usd@
) {
    def Shader "S" {
        asset inputs:file = @tex.png@
        asset inputs:file2 = @tex.png@
        asset inputs:file3 = @missing.png@
    }
}
"#,
        )
        .unwrap();
        let resolve = |p: &str, _anchor: Option<&str>| -> Option<String> {
            let s = p.strip_prefix("./").unwrap_or(p);
            if ["geom.usda", "a.usd", "tex.png"].contains(&s) {
                Some(s.to_owned())
            } else {
                None
            }
        };
        let assets = referenced_asset_paths(&layer, "scene.usda", &resolve);
        assert_eq!(assets, vec!["tex.png".to_string()]);
    }
}
