//! Typed Crate (`.usdc`) **writer** — serialise a [`Layer`] into the
//! §16.3 binary form.
//!
//! The structural writer ([`crate::usdc_writer::CrateImage`]) lays
//! out the six sections, the §3a / §3b compression stack, the
//! bootstrap and the TOC; this module builds the *content* of those
//! sections from the text model the parser and the USDA writer share:
//!
//! * the §16.3.8.4.5 **path tree** — every prim, property, variant
//!   set (`{set=}`) and variant (`{set=sel}`) element plus every path
//!   a value points at, walked depth-first with the jump codes the
//!   §16.3.8.4.5.4 construction algorithm inverts;
//! * one §16.3.8.4.6 **spec** per authored path — Layer (7) with the
//!   layer metadata, Prim (6) with `specifier` / `typeName` /
//!   `primChildren` / `properties` / variant fields / metadata,
//!   Attribute (1) with `typeName` / `default` / `timeSamples` /
//!   `connectionPaths` / `spline` / `custom` / `variability` /
//!   metadata, Relationship (8) with `targetPaths`, VariantSet (11)
//!   with `variantChildren`, Variant (10) with its body;
//! * **value encoding** per §16.3.9 / §16.3.10: the inlining rules
//!   (4-byte scalars, pool indices, int8 vectors and matrix
//!   diagonals, empty dictionaries), offset scalars, uncompressed
//!   arrays, dictionaries, the six-sublist list operations, reference
//!   / payload records, variant-selection maps, time samples,
//!   string / token / path vectors, relocates (0.11) and splines
//!   (0.12).
//!
//! The attribute's authored type token selects the wire type
//! (`point3f[]` → `Vec3f[]`, `half3[]` → `Vec3h[]`, …); untyped
//! metadata values are encoded from their shape. The file's version
//! is the lowest §16.3.8.2 row that covers the features used —
//! 0.8.0, or 0.11.0 with relocates, 0.12.0 with splines.
//!
//! Sources: the AOUSD Core Specification §16.3 as staged, the
//! errata, and this crate's own reader (which is the oracle every
//! encoding here is pinned against).

use std::collections::{BTreeMap, HashMap};

use crate::error::invalid;
use crate::spline::Spline;
use crate::usda::{Attr, Layer, ListEditOp, ListOp, Prim, Value, Variant};
use crate::usdc::{ValueRep, ValueType, Version, BOOTSTRAP_SIZE};
use crate::usdc_writer::CrateImage;
use crate::Result;

/// §16.3.8.4.6 spec forms.
const FORM_ATTRIBUTE: i32 = 1;
const FORM_PRIM: i32 = 6;
const FORM_LAYER: i32 = 7;
const FORM_RELATIONSHIP: i32 = 8;
const FORM_VARIANT: i32 = 10;
const FORM_VARIANT_SET: i32 = 11;

/// Serialise a layer to Crate bytes.
pub fn encode_layer(layer: &Layer) -> Result<Vec<u8>> {
    let mut enc = Encoder::new();
    enc.build(layer)?;
    enc.finish()
}

/// A node of the path tree under construction.
struct PathNode {
    /// Element token (`name`, `{set=sel}`, or a property name).
    element: String,
    /// Property elements join with `.` (negative token word).
    is_property: bool,
    /// Absolute path string (as the reader reconstructs it).
    path: String,
    children: Vec<usize>,
    /// The spec at this path, if authored.
    spec: Option<(i32, Vec<(String, u64)>)>,
}

struct Encoder {
    tokens: Vec<String>,
    token_index: HashMap<String, i32>,
    strings: Vec<u32>,
    string_index: HashMap<String, u32>,
    /// Value blob — placed right after the 88-byte bootstrap.
    values: Vec<u8>,
    nodes: Vec<PathNode>,
    node_by_path: HashMap<String, usize>,
    /// Blob positions (relative to the blob start) of every `u32`
    /// path reference written so far — they hold *node* indices until
    /// `finish` knows the walk order and patches them into path slots.
    path_refs: Vec<usize>,
    version: Version,
}

impl Encoder {
    fn new() -> Self {
        let mut enc = Self {
            tokens: Vec::new(),
            token_index: HashMap::new(),
            strings: Vec::new(),
            string_index: HashMap::new(),
            values: Vec::new(),
            nodes: Vec::new(),
            node_by_path: HashMap::new(),
            path_refs: Vec::new(),
            version: Version::V0_8_0,
        };
        // §16.3.8.4.5.2 forbids token index 0 as an element word, so
        // slot 0 holds a placeholder no element uses.
        enc.token("");
        enc.nodes.push(PathNode {
            element: "/".to_owned(),
            is_property: false,
            path: "/".to_owned(),
            children: Vec::new(),
            spec: None,
        });
        enc.node_by_path.insert("/".to_owned(), 0);
        enc
    }

    // ---- pools -----------------------------------------------------

    fn token(&mut self, s: &str) -> i32 {
        if let Some(&i) = self.token_index.get(s) {
            return i;
        }
        let i = self.tokens.len() as i32;
        self.tokens.push(s.to_owned());
        self.token_index.insert(s.to_owned(), i);
        i
    }

    fn string(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.string_index.get(s) {
            return i;
        }
        let tok = self.token(s) as u32;
        let i = self.strings.len() as u32;
        self.strings.push(tok);
        self.string_index.insert(s.to_owned(), i);
        i
    }

    // ---- path tree ---------------------------------------------------

    /// Ensure a prim-path node exists (creating spec-less ancestors).
    fn prim_node(&mut self, parent: usize, element: &str) -> usize {
        let parent_path = self.nodes[parent].path.clone();
        let path = if parent_path == "/" {
            format!("/{element}")
        } else {
            format!("{parent_path}/{element}")
        };
        if let Some(&i) = self.node_by_path.get(&path) {
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(PathNode {
            element: element.to_owned(),
            is_property: false,
            path: path.clone(),
            children: Vec::new(),
            spec: None,
        });
        self.nodes[parent].children.push(i);
        self.node_by_path.insert(path, i);
        i
    }

    fn property_node(&mut self, parent: usize, name: &str) -> usize {
        let parent_path = self.nodes[parent].path.clone();
        let path = if parent_path == "/" {
            format!("/.{name}")
        } else {
            format!("{parent_path}.{name}")
        };
        if let Some(&i) = self.node_by_path.get(&path) {
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(PathNode {
            element: name.to_owned(),
            is_property: true,
            path: path.clone(),
            children: Vec::new(),
            spec: None,
        });
        self.nodes[parent].children.push(i);
        self.node_by_path.insert(path, i);
        i
    }

    /// Resolve an absolute path string (`/A/B.attr`, `/A{set=sel}/C`)
    /// to a node, creating spec-less nodes along the way.
    fn ensure_path(&mut self, path: &str) -> usize {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "/" {
            return 0;
        }
        if let Some(&i) = self.node_by_path.get(trimmed) {
            return i;
        }
        let (prim_part, prop) = match trimmed.rfind('.') {
            Some(dot) if !trimmed[dot..].contains('/') && !trimmed[dot..].contains('}') => {
                (&trimmed[..dot], Some(&trimmed[dot + 1..]))
            }
            _ => (trimmed, None),
        };
        let mut cur = 0usize;
        // Split into elements: `/`-separated, with `{...}` variant
        // selections as their own elements (a source may spell
        // `/A{set=sel}/B` without the slash the reader emits).
        let mut rest = prim_part.trim_start_matches('/');
        while !rest.is_empty() {
            let elem_end = rest
                .find(['/', '{'])
                .map(|i| {
                    if i == 0 && rest.starts_with('{') {
                        rest.find('}').map_or(rest.len(), |j| j + 1)
                    } else {
                        i
                    }
                })
                .unwrap_or(rest.len());
            let (elem, tail) = rest.split_at(elem_end);
            if !elem.is_empty() {
                cur = self.prim_node(cur, elem);
            }
            rest = tail.trim_start_matches('/');
        }
        match prop {
            Some(p) => self.property_node(cur, p),
            None => cur,
        }
    }

    fn path_index_of(&mut self, path: &str) -> u32 {
        self.ensure_path(path) as u32
    }

    // ---- values ---------------------------------------------------------

    fn rep(vt: ValueType, flags: u16, payload: u64) -> u64 {
        ValueRep {
            type_and_flags: (vt as u16) | flags,
            payload,
        }
        .raw()
    }

    fn inline(vt: ValueType, payload: u32) -> u64 {
        Self::rep(vt, ValueRep::FLAG_INLINE, u64::from(payload))
    }

    /// Current absolute offset of the next byte in the value blob.
    fn offset(&self) -> u64 {
        BOOTSTRAP_SIZE as u64 + self.values.len() as u64
    }

    fn put(&mut self, bytes: &[u8]) -> u64 {
        let off = self.offset();
        self.values.extend_from_slice(bytes);
        off
    }

    fn put_u64(&mut self, v: u64) -> u64 {
        self.put(&v.to_le_bytes())
    }

    /// Store an offset-valued scalar / structure and return its rep.
    fn stored(&mut self, vt: ValueType, bytes: &[u8]) -> u64 {
        let off = self.put(bytes);
        Self::rep(vt, 0, off)
    }

    fn array_rep(&mut self, vt: ValueType, count: usize, elements: &[u8]) -> u64 {
        if count == 0 {
            return Self::rep(vt, ValueRep::FLAG_ARRAY, 0);
        }
        let off = self.put_u64(count as u64);
        self.put(elements);
        Self::rep(vt, ValueRep::FLAG_ARRAY, off)
    }

    /// Encode a value under an attribute type token (`point3f[]`,
    /// `uniform token`, `rel`, …); `None` infers from the shape.
    fn encode_typed(&mut self, v: &Value, type_token: Option<&str>) -> Result<u64> {
        let (vt, is_array) = match type_token.and_then(wire_type) {
            Some(t) => t,
            None => return self.encode_by_shape(v),
        };
        if is_array {
            let items: Vec<Value> = match v {
                Value::Array(items) => items.clone(),
                Value::None => Vec::new(),
                other => vec![other.clone()],
            };
            return self.encode_array(vt, &items);
        }
        self.encode_scalar(vt, v)
    }

    fn encode_scalar(&mut self, vt: ValueType, v: &Value) -> Result<u64> {
        use ValueType as VT;
        let num = |v: &Value| -> Result<f64> {
            match v {
                Value::Float(f) => Ok(*f),
                Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
                other => Err(invalid(format!(
                    "USDC encode: expected a number for {}, got {other:?}",
                    vt.name()
                ))),
            }
        };
        Ok(match vt {
            VT::Bool => Self::inline(
                vt,
                u32::from(matches!(v, Value::Bool(true)) || num(v).unwrap_or(0.0) != 0.0),
            ),
            VT::Uchar => Self::inline(vt, num(v)? as u8 as u32),
            VT::Int => Self::inline(vt, num(v)? as i32 as u32),
            VT::Uint => Self::inline(vt, num(v)? as u32),
            VT::Int64 => {
                let n = num(v)? as i64;
                if let Ok(small) = i32::try_from(n) {
                    Self::inline(vt, small as u32)
                } else {
                    self.stored(vt, &n.to_le_bytes())
                }
            }
            VT::Uint64 => {
                let n = num(v)? as u64;
                if let Ok(small) = u32::try_from(n) {
                    Self::inline(vt, small)
                } else {
                    self.stored(vt, &n.to_le_bytes())
                }
            }
            VT::Half => Self::inline(vt, u32::from(f32_to_half(num(v)? as f32))),
            VT::Float => Self::inline(vt, (num(v)? as f32).to_bits()),
            VT::Double | VT::TimeCode => {
                let f = num(v)?;
                // §16.3.10.10: a double exactly representable as a
                // float inlines as that float.
                if f64::from(f as f32) == f {
                    Self::inline(vt, (f as f32).to_bits())
                } else {
                    self.stored(vt, &f.to_le_bytes())
                }
            }
            VT::Token => {
                let s = text_of(v)?;
                let i = self.token(&s) as u32;
                Self::inline(vt, i)
            }
            VT::String => {
                let s = text_of(v)?;
                let i = self.string(&s);
                Self::inline(vt, i)
            }
            VT::Asset | VT::PathExpression => {
                let s = match v {
                    Value::AssetWithPath { asset, .. } => asset.clone(),
                    other => text_of(other)?,
                };
                let i = self.token(&s) as u32;
                Self::inline(vt, i)
            }
            VT::Half2
            | VT::Half3
            | VT::Half4
            | VT::Float2
            | VT::Float3
            | VT::Float4
            | VT::Double2
            | VT::Double3
            | VT::Double4
            | VT::Int2
            | VT::Int3
            | VT::Int4
            | VT::Quath
            | VT::Quatf
            | VT::Quatd
            | VT::Matrix2d
            | VT::Matrix3d
            | VT::Matrix4d => {
                let comps = components(v, vt)?;
                if let Some(inl) = inline_vector(vt, &comps) {
                    Self::inline(vt, inl)
                } else {
                    let bytes = element_bytes(vt, &comps)?;
                    self.stored(vt, &bytes)
                }
            }
            VT::Dictionary => match v {
                Value::Dict(map) => self.encode_dictionary(map)?,
                _ => Self::inline(vt, 0),
            },
            VT::Specifier | VT::Permission | VT::Variability => {
                let code = match (vt, text_of(v)?.as_str()) {
                    (VT::Specifier, "def")
                    | (VT::Permission, "public")
                    | (VT::Variability, "varying") => 0,
                    (VT::Specifier, "over")
                    | (VT::Permission, "private")
                    | (VT::Variability, "uniform") => 1,
                    (VT::Specifier, "class") => 2,
                    (_, other) => {
                        return Err(invalid(format!(
                            "USDC encode: `{other}` is not a {} value",
                            vt.name()
                        )))
                    }
                };
                Self::inline(vt, code)
            }
            VT::ValueBlock => Self::inline(vt, 0),
            VT::TimeSamples => match v {
                Value::TimeSamples(samples) => self.encode_time_samples(samples, None)?,
                _ => return Err(invalid("USDC encode: TimeSamples value expected")),
            },
            VT::Relocates => match v {
                Value::Relocates(pairs) => self.encode_relocates(pairs)?,
                _ => return Err(invalid("USDC encode: relocates map expected")),
            },
            VT::Splines => match v {
                Value::Spline(s) => self.encode_spline(s)?,
                _ => return Err(invalid("USDC encode: spline value expected")),
            },
            other => {
                return Err(invalid(format!(
                    "USDC encode: scalar {} is not an encodable attribute type",
                    other.name()
                )))
            }
        })
    }

    fn encode_array(&mut self, vt: ValueType, items: &[Value]) -> Result<u64> {
        use ValueType as VT;
        let mut bytes = Vec::new();
        for item in items {
            match vt {
                VT::Token => {
                    let s = text_of(item)?;
                    bytes.extend_from_slice(&(self.token(&s) as u32).to_le_bytes());
                }
                VT::String => {
                    let s = text_of(item)?;
                    bytes.extend_from_slice(&self.string(&s).to_le_bytes());
                }
                VT::Asset | VT::PathExpression => {
                    let s = text_of(item)?;
                    bytes.extend_from_slice(&self.string(&s).to_le_bytes());
                }
                VT::Bool => bytes.push(u8::from(
                    matches!(item, Value::Bool(true)) || item.as_f32().unwrap_or(0.0) != 0.0,
                )),
                VT::Uchar => bytes.push(item.as_f32().unwrap_or(0.0) as u8),
                VT::Int => bytes.extend_from_slice(&(scalar_f64(item)? as i32).to_le_bytes()),
                VT::Uint => bytes.extend_from_slice(&(scalar_f64(item)? as u32).to_le_bytes()),
                VT::Int64 => bytes.extend_from_slice(&(scalar_f64(item)? as i64).to_le_bytes()),
                VT::Uint64 => bytes.extend_from_slice(&(scalar_f64(item)? as u64).to_le_bytes()),
                VT::Half => {
                    bytes.extend_from_slice(&f32_to_half(scalar_f64(item)? as f32).to_le_bytes())
                }
                VT::Float => bytes.extend_from_slice(&(scalar_f64(item)? as f32).to_le_bytes()),
                VT::Double | VT::TimeCode => {
                    bytes.extend_from_slice(&scalar_f64(item)?.to_le_bytes())
                }
                _ => {
                    let comps = components(item, vt)?;
                    bytes.extend_from_slice(&element_bytes(vt, &comps)?);
                }
            }
        }
        Ok(self.array_rep(vt, items.len(), &bytes))
    }

    /// Infer a wire type from a value's shape (metadata, dictionary
    /// members, time-sample members of untyped attributes).
    fn encode_by_shape(&mut self, v: &Value) -> Result<u64> {
        use ValueType as VT;
        Ok(match v {
            Value::Bool(_) => self.encode_scalar(VT::Bool, v)?,
            Value::Float(f) => {
                if *f == f.trunc() && f.abs() < 2_147_483_648.0 {
                    self.encode_scalar(VT::Int, v)?
                } else {
                    self.encode_scalar(VT::Double, v)?
                }
            }
            Value::Token(_) => self.encode_scalar(VT::Token, v)?,
            Value::String(_) | Value::Raw(_) => self.encode_scalar(VT::String, v)?,
            Value::Asset(_) => self.encode_scalar(VT::Asset, v)?,
            Value::AssetWithPath { .. } => self.encode_list_op(
                &ListOp::single(ListEditOp::Explicit, v.clone()),
                VT::ReferenceListOp,
            )?,
            Value::Path(_) => self.encode_list_op(
                &ListOp::single(ListEditOp::Explicit, v.clone()),
                VT::PathListOp,
            )?,
            Value::Tuple(seq) => {
                let all_int = seq
                    .iter()
                    .all(|x| matches!(x, Value::Float(f) if *f == f.trunc()));
                let vt = match (seq.len(), all_int) {
                    (2, true) => VT::Int2,
                    (3, true) => VT::Int3,
                    (4, true) => VT::Int4,
                    (2, false) => VT::Double2,
                    (3, false) => VT::Double3,
                    (4, false) => VT::Double4,
                    _ => return Err(invalid("USDC encode: tuple arity has no wire type")),
                };
                if seq
                    .iter()
                    .all(|x| matches!(x, Value::Tuple(row) if row.len() == seq.len()))
                {
                    let vt = match seq.len() {
                        2 => VT::Matrix2d,
                        3 => VT::Matrix3d,
                        _ => VT::Matrix4d,
                    };
                    return self.encode_scalar(vt, v);
                }
                self.encode_scalar(vt, v)?
            }
            Value::Array(items) => {
                let vt = match items.first() {
                    None => VT::Int,
                    Some(Value::Bool(_)) => VT::Bool,
                    Some(Value::Float(_)) => {
                        if items.iter().all(|x| matches!(x, Value::Float(f) if *f == f.trunc() && f.abs() < 2_147_483_648.0)) {
                            VT::Int
                        } else {
                            VT::Double
                        }
                    }
                    Some(Value::Token(_)) => VT::Token,
                    Some(Value::String(_)) | Some(Value::Raw(_)) => VT::String,
                    Some(Value::Asset(_)) => VT::Asset,
                    Some(Value::Path(_)) => {
                        return self.encode_list_op(&ListOp::single(ListEditOp::Explicit, v.clone()), VT::PathListOp)
                    }
                    Some(Value::AssetWithPath { .. }) => {
                        return self.encode_list_op(&ListOp::single(ListEditOp::Explicit, v.clone()), VT::ReferenceListOp)
                    }
                    Some(Value::Tuple(t)) => match t.len() {
                        2 => VT::Double2,
                        3 => VT::Double3,
                        4 => VT::Double4,
                        _ => return Err(invalid("USDC encode: tuple arity has no wire type")),
                    },
                    Some(other) => {
                        return Err(invalid(format!(
                            "USDC encode: array of {other:?} has no wire type"
                        )))
                    }
                };
                self.encode_array(vt, items)?
            }
            Value::Dict(map) => self.encode_dictionary(map)?,
            Value::TimeSamples(samples) => self.encode_time_samples(samples, None)?,
            Value::ListOp(op) => {
                let vt = list_op_type_for(op, VT::TokenListOp);
                self.encode_list_op(op, vt)?
            }
            Value::Relocates(pairs) => self.encode_relocates(pairs)?,
            Value::Spline(s) => self.encode_spline(s)?,
            Value::None => Self::inline(VT::ValueBlock, 0),
        })
    }

    /// §16.3.10.19 dictionary, laid out the way production files
    /// are: `[u64 count]`, then per member `[u32 keyToken][i64 rel]`
    /// immediately followed by the member's own payload bytes (if
    /// any) and its rep word — `rel` is the forward distance from the
    /// `i64`'s own position to that rep word (8 for an inlined
    /// member). Nested dictionaries recurse in place.
    fn encode_dictionary(&mut self, map: &BTreeMap<String, Value>) -> Result<u64> {
        if map.is_empty() {
            return Ok(Self::inline(ValueType::Dictionary, 0));
        }
        let off = self.put_u64(map.len() as u64);
        for (k, v) in map {
            let key = self.token(k) as u32;
            self.put(&key.to_le_bytes());
            let link_pos = self.put_u64(0);
            let rep = self.encode_by_shape(v)?;
            let rep_pos = self.put_u64(rep);
            let rel = rep_pos as i64 - link_pos as i64;
            let at = (link_pos - BOOTSTRAP_SIZE as u64) as usize;
            self.values[at..at + 8].copy_from_slice(&rel.to_le_bytes());
        }
        Ok(Self::rep(ValueType::Dictionary, 0, off))
    }

    /// §16.3.10.25 list operation.
    fn encode_list_op(&mut self, op: &ListOp, vt: ValueType) -> Result<u64> {
        const MAKE_EXPLICIT: u8 = 0x1;
        const ADD_EXPLICIT: u8 = 0x2;
        const ADD: u8 = 0x4;
        const DELETE: u8 = 0x8;
        const REORDER: u8 = 0x10;
        const PREPEND: u8 = 0x20;
        const APPEND: u8 = 0x40;
        let mut header = 0u8;
        let mut body: Vec<(u8, Vec<Value>)> = Vec::new();
        let items = |v: &Value| -> Vec<Value> {
            match v {
                Value::Array(seq) => seq.clone(),
                Value::None => Vec::new(),
                other => vec![other.clone()],
            }
        };
        if let Some(v) = &op.explicit {
            header |= MAKE_EXPLICIT;
            let it = items(v);
            if !it.is_empty() {
                header |= ADD_EXPLICIT;
                body.push((ADD_EXPLICIT, it));
            }
        }
        // Sublists follow in the fixed wire order.
        let ordered = [
            (ADD, &op.appended, true),
            (PREPEND, &op.prepended, false),
            (APPEND, &op.appended, false),
            (DELETE, &op.deleted, false),
            (REORDER, &op.reordered, false),
        ];
        for (bit, slot, is_add_legacy) in ordered {
            // `add` (legacy) and `append` share the typed slot; the
            // parser folds `add` into `appended`, so it re-emits as
            // `append`.
            if is_add_legacy {
                continue;
            }
            if let Some(v) = slot {
                header |= bit;
                body.push((bit, items(v)));
            }
        }
        let mut bytes = vec![header];
        let mut refs: Vec<usize> = Vec::new();
        for (_, list) in &body {
            bytes.extend_from_slice(&(list.len() as u64).to_le_bytes());
            for item in list {
                self.encode_list_item(vt, item, &mut bytes, &mut refs)?;
            }
        }
        let off = self.put(&bytes);
        let base = (off - BOOTSTRAP_SIZE as u64) as usize;
        self.path_refs.extend(refs.into_iter().map(|r| base + r));
        Ok(Self::rep(vt, 0, off))
    }

    fn encode_list_item(
        &mut self,
        vt: ValueType,
        item: &Value,
        out: &mut Vec<u8>,
        refs: &mut Vec<usize>,
    ) -> Result<()> {
        use ValueType as VT;
        match vt {
            VT::TokenListOp => {
                let s = text_of(item)?;
                out.extend_from_slice(&(self.token(&s) as u32).to_le_bytes());
            }
            VT::StringListOp => {
                let s = text_of(item)?;
                out.extend_from_slice(&self.string(&s).to_le_bytes());
            }
            VT::PathListOp => {
                let p = match item {
                    Value::Path(p) => p.clone(),
                    other => text_of(other)?,
                };
                refs.push(out.len());
                out.extend_from_slice(&self.path_index_of(&p).to_le_bytes());
            }
            VT::IntListOp | VT::UIntListOp => {
                out.extend_from_slice(&(scalar_f64(item)? as i32 as u32).to_le_bytes());
            }
            VT::Int64ListOp | VT::UInt64ListOp => {
                out.extend_from_slice(&(scalar_f64(item)? as i64).to_le_bytes());
            }
            VT::ReferenceListOp | VT::PayloadListOp => {
                let (asset, prim_path) = match item {
                    Value::AssetWithPath { asset, prim_path } => (asset.clone(), prim_path.clone()),
                    Value::Asset(a) => (a.clone(), String::new()),
                    Value::Path(p) => (String::new(), p.clone()),
                    other => (text_of(other)?, String::new()),
                };
                out.extend_from_slice(&self.string(&asset).to_le_bytes());
                let path_idx = if prim_path.is_empty() {
                    0
                } else {
                    self.path_index_of(&prim_path)
                };
                refs.push(out.len());
                out.extend_from_slice(&path_idx.to_le_bytes());
                // §16.3.10.20 layer offset: identity (offset 0, scale 1).
                out.extend_from_slice(&0.0f64.to_le_bytes());
                out.extend_from_slice(&1.0f64.to_le_bytes());
                if vt == VT::ReferenceListOp {
                    // Empty custom-data dictionary.
                    out.extend_from_slice(&0u64.to_le_bytes());
                }
            }
            other => {
                return Err(invalid(format!(
                    "USDC encode: {} is not a list-op type",
                    other.name()
                )))
            }
        }
        Ok(())
    }

    /// §16.3.10.31 time samples (layout as the reader decodes it).
    fn encode_time_samples(
        &mut self,
        samples: &[(f64, Value)],
        elem_type: Option<&str>,
    ) -> Result<u64> {
        // Times as a DoubleVector.
        let mut times = Vec::with_capacity(8 + samples.len() * 8);
        times.extend_from_slice(&(samples.len() as u64).to_le_bytes());
        for (t, _) in samples {
            times.extend_from_slice(&t.to_le_bytes());
        }
        let times_off = self.put(&times);
        let times_rep = Self::rep(ValueType::DoubleVector, 0, times_off);
        // Sample values, encoded under the attribute's element type.
        let mut reps = Vec::with_capacity(samples.len());
        for (_, v) in samples {
            reps.push(self.encode_typed(v, elem_type)?);
        }
        let times_rep_pos = self.put_u64(times_rep);
        // Placeholder for the i64 → values array.
        let link_pos = self.put_u64(0);
        let values_pos = self.put_u64(samples.len() as u64);
        for r in reps {
            self.put_u64(r);
        }
        let rel = values_pos as i64 - link_pos as i64;
        let link_at = (link_pos - BOOTSTRAP_SIZE as u64) as usize;
        self.values[link_at..link_at + 8].copy_from_slice(&rel.to_le_bytes());
        // The rep's payload: an i64 (relative to itself) to the times rep.
        let off = self.offset();
        let rel_times = times_rep_pos as i64 - off as i64;
        self.put(&rel_times.to_le_bytes());
        Ok(Self::rep(ValueType::TimeSamples, 0, off))
    }

    fn encode_relocates(&mut self, pairs: &[(String, String)]) -> Result<u64> {
        if self.version.dispatch_key() < (0, 11) {
            self.version = Version::V0_11_0;
        }
        let mut bytes = Vec::with_capacity(8 + pairs.len() * 8);
        bytes.extend_from_slice(&(pairs.len() as u64).to_le_bytes());
        let mut refs = Vec::with_capacity(pairs.len() * 2);
        for (src, dst) in pairs {
            let a = self.path_index_of(src);
            let b = self.path_index_of(dst);
            refs.push(bytes.len());
            bytes.extend_from_slice(&a.to_le_bytes());
            refs.push(bytes.len());
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        let off = self.put(&bytes);
        let base = (off - BOOTSTRAP_SIZE as u64) as usize;
        self.path_refs.extend(refs.into_iter().map(|r| base + r));
        Ok(Self::rep(ValueType::Relocates, 0, off))
    }

    /// §16.3.10.33: byte-counted data run + `(timeCode, dictionary)`
    /// custom-data list (the dictionaries are inline tables).
    fn encode_spline(&mut self, s: &Spline) -> Result<u64> {
        if self.version.dispatch_key() < (0, 12) {
            self.version = Version::V0_12_0;
        }
        let run = s.to_crate_data();
        // Custom data: knots with a dictionary, plus the extras.
        let mut custom: Vec<(f64, &BTreeMap<String, Value>)> = s
            .knots
            .iter()
            .filter(|k| !k.custom_data.is_empty())
            .map(|k| (k.time, &k.custom_data))
            .collect();
        custom.extend(s.extra_custom_data.iter().map(|(t, d)| (*t, d)));
        let off = self.put_u64(run.len() as u64);
        self.put(&run);
        self.put_u64(custom.len() as u64);
        for (t, dict) in custom {
            self.put(&t.to_le_bytes());
            // Inline dictionary table (the same member layout as
            // `encode_dictionary`, without a rep of its own).
            self.put_u64(dict.len() as u64);
            for (k, v) in dict.iter() {
                let key = self.token(k) as u32;
                self.put(&key.to_le_bytes());
                let link_pos = self.put_u64(0);
                let rep = self.encode_by_shape(v)?;
                let rep_pos = self.put_u64(rep);
                let rel = rep_pos as i64 - link_pos as i64;
                let at = (link_pos - BOOTSTRAP_SIZE as u64) as usize;
                self.values[at..at + 8].copy_from_slice(&rel.to_le_bytes());
            }
        }
        Ok(Self::rep(ValueType::Splines, 0, off))
    }

    fn token_vector(&mut self, names: &[String]) -> u64 {
        let mut bytes = Vec::with_capacity(8 + names.len() * 4);
        bytes.extend_from_slice(&(names.len() as u64).to_le_bytes());
        for n in names {
            let t = self.token(n) as u32;
            bytes.extend_from_slice(&t.to_le_bytes());
        }
        self.stored(ValueType::TokenVector, &bytes)
    }

    fn string_vector(&mut self, items: &[String]) -> u64 {
        let mut bytes = Vec::with_capacity(8 + items.len() * 4);
        bytes.extend_from_slice(&(items.len() as u64).to_le_bytes());
        for s in items {
            let i = self.string(s);
            bytes.extend_from_slice(&i.to_le_bytes());
        }
        self.stored(ValueType::StringVector, &bytes)
    }

    // ---- specs ----------------------------------------------------------

    fn build(&mut self, layer: &Layer) -> Result<()> {
        // Layer spec.
        let mut fields: Vec<(String, u64)> = Vec::new();
        for (k, v) in &layer.metadata {
            let (name, rep) = match k.as_str() {
                "subLayers" => {
                    let items: Vec<String> = match v {
                        Value::Array(seq) => seq
                            .iter()
                            .filter_map(|x| x.as_text().map(str::to_owned))
                            .collect(),
                        Value::ListOp(op) => op
                            .additive_in_strength_order()
                            .flat_map(|(_, s)| match s {
                                Value::Array(seq) => seq.clone(),
                                other => vec![other.clone()],
                            })
                            .filter_map(|x| x.as_text().map(str::to_owned))
                            .collect(),
                        other => other.as_text().map(str::to_owned).into_iter().collect(),
                    };
                    // §16.3.10.27: the parallel offsets vector every
                    // sublayer entry needs (identity: offset 0, scale 1).
                    let mut offsets = Vec::with_capacity(8 + items.len() * 16);
                    offsets.extend_from_slice(&(items.len() as u64).to_le_bytes());
                    for _ in &items {
                        offsets.extend_from_slice(&0.0f64.to_le_bytes());
                        offsets.extend_from_slice(&1.0f64.to_le_bytes());
                    }
                    let rep = self.stored(ValueType::LayerOffsetVector, &offsets);
                    fields.push(("subLayerOffsets".to_owned(), rep));
                    ("subLayers".to_owned(), self.string_vector(&items))
                }
                "relocates" => ("layerRelocates".to_owned(), self.encode_by_shape(v)?),
                "doc" => (
                    "documentation".to_owned(),
                    self.encode_scalar(ValueType::String, v)?,
                ),
                "defaultPrim" | "upAxis" => (k.clone(), self.encode_scalar(ValueType::Token, v)?),
                "metersPerUnit" | "timeCodesPerSecond" | "framesPerSecond" | "startTimeCode"
                | "endTimeCode" => (k.clone(), self.encode_scalar(ValueType::Double, v)?),
                _ => (k.clone(), self.encode_by_shape(v)?),
            };
            fields.push((name, rep));
        }
        let child_names: Vec<String> = layer.prims.iter().map(|p| p.name.clone()).collect();
        if !child_names.is_empty() {
            let rep = self.token_vector(&child_names);
            fields.push(("primChildren".to_owned(), rep));
        }
        self.nodes[0].spec = Some((FORM_LAYER, fields));

        for prim in &layer.prims {
            self.build_prim(0, prim)?;
        }
        Ok(())
    }

    fn build_prim(&mut self, parent: usize, prim: &Prim) -> Result<()> {
        let node = self.prim_node(parent, &prim.name);
        let mut fields: Vec<(String, u64)> = Vec::new();
        fields.push((
            "specifier".to_owned(),
            self.encode_scalar(ValueType::Specifier, &Value::Token(prim.spec.clone()))?,
        ));
        if !prim.type_name.is_empty() {
            fields.push((
                "typeName".to_owned(),
                self.encode_scalar(ValueType::Token, &Value::Token(prim.type_name.clone()))?,
            ));
        }
        self.body_fields(
            node,
            &prim.metadata,
            &prim.attrs,
            &prim.children,
            &prim.variant_sets,
            &mut fields,
        )?;
        self.nodes[node].spec = Some((FORM_PRIM, fields));
        Ok(())
    }

    /// Shared prim / variant body: metadata, properties, children,
    /// variant sets.
    fn body_fields(
        &mut self,
        node: usize,
        metadata: &BTreeMap<String, Value>,
        attrs: &BTreeMap<String, Attr>,
        children: &[Prim],
        variant_sets: &BTreeMap<String, BTreeMap<String, Variant>>,
        fields: &mut Vec<(String, u64)>,
    ) -> Result<()> {
        for (k, v) in metadata {
            let (name, rep) = match k.as_str() {
                "variants" => {
                    let Value::Dict(map) = v else { continue };
                    let mut bytes = Vec::with_capacity(8 + map.len() * 8);
                    bytes.extend_from_slice(&(map.len() as u64).to_le_bytes());
                    for (set, sel) in map {
                        let a = self.string(set);
                        let b = self.string(sel.as_text().unwrap_or_default());
                        bytes.extend_from_slice(&a.to_le_bytes());
                        bytes.extend_from_slice(&b.to_le_bytes());
                    }
                    (
                        "variantSelection".to_owned(),
                        self.stored(ValueType::VariantSelectionMap, &bytes),
                    )
                }
                "variantSets" => {
                    let op = as_list_op(v);
                    (
                        "variantSetNames".to_owned(),
                        self.encode_list_op(&op, ValueType::StringListOp)?,
                    )
                }
                "references" => (
                    "references".to_owned(),
                    self.encode_list_op(&as_list_op(v), ValueType::ReferenceListOp)?,
                ),
                "payload" => (
                    "payload".to_owned(),
                    self.encode_list_op(&as_list_op(v), ValueType::PayloadListOp)?,
                ),
                "inherits" | "specializes" => (
                    k.clone(),
                    self.encode_list_op(&as_list_op(v), ValueType::PathListOp)?,
                ),
                "apiSchemas" => (
                    "apiSchemas".to_owned(),
                    self.encode_list_op(&as_list_op(v), ValueType::TokenListOp)?,
                ),
                "inactiveIds" => (
                    "inactiveIds".to_owned(),
                    self.encode_list_op(&as_list_op(v), ValueType::Int64ListOp)?,
                ),
                "kind" => ("kind".to_owned(), self.encode_scalar(ValueType::Token, v)?),
                "doc" => (
                    "documentation".to_owned(),
                    self.encode_scalar(ValueType::String, v)?,
                ),
                "active" | "hidden" | "instanceable" => {
                    (k.clone(), self.encode_scalar(ValueType::Bool, v)?)
                }
                _ => (k.clone(), self.encode_by_shape(v)?),
            };
            fields.push((name, rep));
        }
        // Properties: companion statements (`.timeSamples`,
        // `.connect`, `.spline`) fold into their attribute's spec.
        let mut property_names: Vec<String> = Vec::new();
        for (name, attr) in attrs {
            if name.contains('.') {
                continue;
            }
            property_names.push(name.clone());
            self.build_property(node, name, attr, attrs)?;
        }
        // Companions without a base statement (a bare
        // `float x.timeSamples = {…}`): give them a spec of their own.
        for (name, attr) in attrs {
            let Some((base, companion)) = name.rsplit_once('.') else {
                continue;
            };
            if attrs.contains_key(base)
                || !matches!(companion, "timeSamples" | "connect" | "spline")
            {
                continue;
            }
            if property_names.iter().any(|p| p == base) {
                continue;
            }
            property_names.push(base.to_owned());
            let bare = Attr {
                type_token: attr.type_token.clone(),
                value: Value::None,
                metadata: BTreeMap::new(),
            };
            self.build_property(node, base, &bare, attrs)?;
        }
        if !property_names.is_empty() {
            let rep = self.token_vector(&property_names);
            fields.push(("properties".to_owned(), rep));
        }
        let child_names: Vec<String> = children.iter().map(|p| p.name.clone()).collect();
        if !child_names.is_empty() {
            let rep = self.token_vector(&child_names);
            fields.push(("primChildren".to_owned(), rep));
        }
        if !variant_sets.is_empty() {
            let set_names: Vec<String> = variant_sets.keys().cloned().collect();
            let rep = self.token_vector(&set_names);
            fields.push(("variantSetChildren".to_owned(), rep));
            for (set, variants) in variant_sets {
                // VariantSet spec at `{set=}`.
                let set_node = self.prim_node(node, &format!("{{{set}=}}"));
                let sel_names: Vec<String> = variants.keys().cloned().collect();
                let rep = self.token_vector(&sel_names);
                self.nodes[set_node].spec =
                    Some((FORM_VARIANT_SET, vec![("variantChildren".to_owned(), rep)]));
                for (sel, variant) in variants {
                    let var_node = self.prim_node(node, &format!("{{{set}={sel}}}"));
                    let mut vfields = Vec::new();
                    self.body_fields(
                        var_node,
                        &variant.metadata,
                        &variant.attrs,
                        &variant.children,
                        &BTreeMap::new(),
                        &mut vfields,
                    )?;
                    self.nodes[var_node].spec = Some((FORM_VARIANT, vfields));
                    for child in &variant.children {
                        self.build_prim(var_node, child)?;
                    }
                }
            }
        }
        for child in children {
            self.build_prim(node, child)?;
        }
        Ok(())
    }

    fn build_property(
        &mut self,
        prim_node: usize,
        name: &str,
        attr: &Attr,
        attrs: &BTreeMap<String, Attr>,
    ) -> Result<()> {
        let node = self.property_node(prim_node, name);
        let mut fields: Vec<(String, u64)> = Vec::new();
        let (modifiers, base_type) = split_type_token(&attr.type_token);
        let custom = modifiers.contains(&"custom");
        let uniform = modifiers.contains(&"uniform");
        if base_type == "rel" {
            let targets = match &attr.value {
                Value::None => ListOp::single(ListEditOp::Explicit, Value::Array(Vec::new())),
                other => as_list_op(other),
            };
            fields.push((
                "targetPaths".to_owned(),
                self.encode_list_op(&targets, ValueType::PathListOp)?,
            ));
            fields.push((
                "variability".to_owned(),
                Self::inline(ValueType::Variability, 1),
            ));
            if custom {
                fields.push(("custom".to_owned(), Self::inline(ValueType::Bool, 1)));
            }
            for (k, v) in &attr.metadata {
                fields.push((k.clone(), self.encode_by_shape(v)?));
            }
            self.nodes[node].spec = Some((FORM_RELATIONSHIP, fields));
            return Ok(());
        }
        let type_name = if base_type.is_empty() {
            "token"
        } else {
            base_type
        };
        fields.push((
            "typeName".to_owned(),
            self.encode_scalar(ValueType::Token, &Value::Token(type_name.to_owned()))?,
        ));
        if custom {
            fields.push(("custom".to_owned(), Self::inline(ValueType::Bool, 1)));
        }
        if uniform {
            fields.push((
                "variability".to_owned(),
                Self::inline(ValueType::Variability, 1),
            ));
        }
        if !matches!(attr.value, Value::None) {
            let rep = self.encode_typed(&attr.value, Some(type_name))?;
            fields.push(("default".to_owned(), rep));
        }
        if let Some(Attr {
            value: Value::TimeSamples(samples),
            ..
        }) = attrs.get(&format!("{name}.timeSamples"))
        {
            let rep = self.encode_time_samples(samples, Some(type_name))?;
            fields.push(("timeSamples".to_owned(), rep));
        }
        if let Some(c) = attrs.get(&format!("{name}.connect")) {
            let op = match &c.value {
                Value::None => ListOp::single(ListEditOp::Explicit, Value::Array(Vec::new())),
                other => as_list_op(other),
            };
            fields.push((
                "connectionPaths".to_owned(),
                self.encode_list_op(&op, ValueType::PathListOp)?,
            ));
        }
        if let Some(Attr {
            value: Value::Spline(s),
            ..
        }) = attrs.get(&format!("{name}.spline"))
        {
            let rep = self.encode_spline(s)?;
            fields.push(("spline".to_owned(), rep));
        }
        for (k, v) in &attr.metadata {
            let rep = match k.as_str() {
                "interpolation" => self.encode_scalar(ValueType::Token, v)?,
                "elementSize" => self.encode_scalar(ValueType::Int, v)?,
                _ => self.encode_by_shape(v)?,
            };
            fields.push((k.clone(), rep));
        }
        self.nodes[node].spec = Some((FORM_ATTRIBUTE, fields));
        Ok(())
    }

    // ---- sections -------------------------------------------------------

    fn finish(mut self) -> Result<Vec<u8>> {
        // Walk the tree: entry order = path slot order.
        let mut order: Vec<usize> = Vec::with_capacity(self.nodes.len());
        let mut jumps: Vec<i32> = Vec::new();
        fn walk(enc: &Encoder, node: usize, order: &mut Vec<usize>, jumps: &mut Vec<i32>) {
            let entry = order.len();
            order.push(node);
            jumps.push(0);
            let children = &enc.nodes[node].children;
            let mut child_entries = Vec::with_capacity(children.len());
            for &c in children {
                child_entries.push(order.len());
                walk(enc, c, order, jumps);
            }
            // The node's own jump: child only / leaf; siblings are
            // patched by the parent below.
            jumps[entry] = if children.is_empty() { -2 } else { -1 };
            for (i, &c) in children.iter().enumerate() {
                let start = child_entries[i];
                let has_children = !enc.nodes[c].children.is_empty();
                let next = child_entries.get(i + 1).copied();
                jumps[start] = match (has_children, next) {
                    (false, None) => -2,
                    (true, None) => -1,
                    (false, Some(_)) => 0,
                    (true, Some(n)) => (n - start) as i32,
                };
            }
        }
        walk(&self, 0, &mut order, &mut jumps);
        let root_word = self.token("/");
        let mut path_token_indices = Vec::with_capacity(order.len());
        let mut element_token_indices = Vec::with_capacity(order.len());
        for (slot, &node) in order.iter().enumerate() {
            path_token_indices.push(slot as i32);
            let word = if node == 0 {
                root_word
            } else {
                let t = self.token(&self.nodes[node].element.clone());
                if self.nodes[node].is_property {
                    -t
                } else {
                    t
                }
            };
            element_token_indices.push(word);
        }
        // Node index → path slot (walk order), then patch every path
        // reference the values blob holds as a node index.
        let mut slot_of_node = vec![0usize; self.nodes.len()];
        for (slot, &node) in order.iter().enumerate() {
            slot_of_node[node] = slot;
        }
        for &pos in &self.path_refs {
            let node = u32::from_le_bytes(self.values[pos..pos + 4].try_into().unwrap()) as usize;
            let slot = slot_of_node[node] as u32;
            self.values[pos..pos + 4].copy_from_slice(&slot.to_le_bytes());
        }

        // Fields + field sets + specs.
        let mut fields: Vec<(i32, u64)> = Vec::new();
        let mut field_index: HashMap<(i32, u64), i32> = HashMap::new();
        let mut field_sets: Vec<i32> = Vec::new();
        let mut spec_path_indices = Vec::new();
        let mut spec_fieldset_indices = Vec::new();
        let mut spec_types = Vec::new();
        // Specs in path-walk order (the reader materialises prims in
        // spec order; the walk order is the authored order).
        type SpecRow = (usize, i32, Vec<(String, u64)>);
        let specs: Vec<SpecRow> = order
            .iter()
            .filter_map(|&i| {
                self.nodes[i]
                    .spec
                    .as_ref()
                    .map(|(form, f)| (i, *form, f.clone()))
            })
            .collect();
        for (node, form, spec_fields) in specs {
            let set_start = field_sets.len() as i32;
            for (name, rep) in spec_fields {
                let name_idx = self.token(&name);
                let key = (name_idx, rep);
                let idx = match field_index.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = fields.len() as i32;
                        fields.push(key);
                        field_index.insert(key, i);
                        i
                    }
                };
                field_sets.push(idx);
            }
            field_sets.push(-1);
            spec_path_indices.push(slot_of_node[node] as i32);
            spec_fieldset_indices.push(set_start);
            spec_types.push(form);
        }
        let image = CrateImage {
            version: self.version,
            tokens: self.tokens,
            strings: self.strings,
            fields,
            field_sets,
            path_token_indices,
            element_token_indices,
            path_jumps: jumps,
            spec_path_indices,
            spec_fieldset_indices,
            spec_types,
            values: self.values,
        };
        image.to_bytes()
    }
}

// ---- helpers --------------------------------------------------------------

fn text_of(v: &Value) -> Result<String> {
    match v {
        Value::Token(s) | Value::String(s) | Value::Asset(s) | Value::Path(s) | Value::Raw(s) => {
            Ok(s.clone())
        }
        Value::Float(f) => Ok(crate::usda_writer::format_metadata_value(&Value::Float(*f))),
        Value::Bool(b) => Ok(b.to_string()),
        Value::None => Ok(String::new()),
        other => Err(invalid(format!(
            "USDC encode: expected a text value, got {other:?}"
        ))),
    }
}

fn scalar_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(invalid(format!(
            "USDC encode: expected a number, got {other:?}"
        ))),
    }
}

/// A non-list-op metadata value as an explicit list op.
fn as_list_op(v: &Value) -> ListOp {
    match v {
        Value::ListOp(op) => (**op).clone(),
        other => ListOp::single(ListEditOp::Explicit, other.clone()),
    }
}

/// Wire list-op type from the items' shape.
fn list_op_type_for(op: &ListOp, fallback: ValueType) -> ValueType {
    let first = op
        .entries()
        .flat_map(|(_, v)| match v {
            Value::Array(seq) => seq.iter().collect::<Vec<_>>(),
            other => vec![other],
        })
        .next();
    match first {
        Some(Value::Path(_)) => ValueType::PathListOp,
        Some(Value::AssetWithPath { .. }) | Some(Value::Asset(_)) => ValueType::ReferenceListOp,
        Some(Value::String(_)) => ValueType::StringListOp,
        Some(Value::Float(f)) if *f == f.trunc() => ValueType::Int64ListOp,
        Some(Value::Token(_)) => ValueType::TokenListOp,
        _ => fallback,
    }
}

/// `[custom] [uniform|varying|config] type[/[]]` → (modifiers, base).
fn split_type_token(token: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = token.split_whitespace().collect();
    let base = parts.pop().unwrap_or("");
    (parts, base)
}

/// Text type token → (wire type, is_array).
fn wire_type(token: &str) -> Option<(ValueType, bool)> {
    use ValueType as VT;
    let (_, base) = split_type_token(token);
    let (name, is_array) = match base.strip_suffix("[]") {
        Some(n) => (n, true),
        None => (base, false),
    };
    let vt = match name {
        "bool" => VT::Bool,
        "uchar" => VT::Uchar,
        "int" => VT::Int,
        "uint" => VT::Uint,
        "int64" => VT::Int64,
        "uint64" => VT::Uint64,
        "half" => VT::Half,
        "float" => VT::Float,
        "double" => VT::Double,
        "timecode" => VT::TimeCode,
        "string" => VT::String,
        "token" => VT::Token,
        "asset" => VT::Asset,
        "half2" | "texCoord2h" => VT::Half2,
        "half3" | "point3h" | "normal3h" | "vector3h" | "color3h" | "texCoord3h" => VT::Half3,
        "half4" | "color4h" => VT::Half4,
        "float2" | "texCoord2f" => VT::Float2,
        "float3" | "point3f" | "normal3f" | "vector3f" | "color3f" | "texCoord3f" => VT::Float3,
        "float4" | "color4f" => VT::Float4,
        "double2" | "texCoord2d" => VT::Double2,
        "double3" | "point3d" | "normal3d" | "vector3d" | "color3d" | "texCoord3d" => VT::Double3,
        "double4" | "color4d" => VT::Double4,
        "int2" => VT::Int2,
        "int3" => VT::Int3,
        "int4" => VT::Int4,
        "quath" => VT::Quath,
        "quatf" => VT::Quatf,
        "quatd" => VT::Quatd,
        "matrix2d" => VT::Matrix2d,
        "matrix3d" => VT::Matrix3d,
        "matrix4d" | "frame4d" => VT::Matrix4d,
        "dictionary" => VT::Dictionary,
        _ => return None,
    };
    Some((vt, is_array))
}

/// Flatten a tuple / matrix / quaternion value to its wire-ordered
/// component list.
fn components(v: &Value, vt: ValueType) -> Result<Vec<f64>> {
    use ValueType as VT;
    let flat = |v: &Value| -> Result<Vec<f64>> {
        match v {
            Value::Tuple(seq) => seq
                .iter()
                .map(|x| match x {
                    Value::Float(f) => Ok(*f),
                    Value::Tuple(row) => Err(invalid(format!("USDC encode: nested tuple {row:?}"))),
                    other => Err(invalid(format!(
                        "USDC encode: non-numeric component {other:?}"
                    ))),
                })
                .collect(),
            Value::Float(f) => Ok(vec![*f]),
            other => Err(invalid(format!(
                "USDC encode: expected a tuple for {}, got {other:?}",
                vt.name()
            ))),
        }
    };
    let n = match vt {
        VT::Half2 | VT::Float2 | VT::Double2 | VT::Int2 => 2,
        VT::Half3 | VT::Float3 | VT::Double3 | VT::Int3 => 3,
        VT::Half4 | VT::Float4 | VT::Double4 | VT::Int4 | VT::Quath | VT::Quatf | VT::Quatd => 4,
        VT::Matrix2d => 4,
        VT::Matrix3d => 9,
        VT::Matrix4d => 16,
        _ => 0,
    };
    let mut comps = match vt {
        VT::Matrix2d | VT::Matrix3d | VT::Matrix4d => match v {
            Value::Tuple(rows) => {
                let mut out = Vec::with_capacity(n);
                for row in rows {
                    out.extend(flat(row)?);
                }
                out
            }
            other => flat(other)?,
        },
        _ => flat(v)?,
    };
    if comps.len() != n {
        return Err(invalid(format!(
            "USDC encode: {} needs {n} components, got {}",
            vt.name(),
            comps.len()
        )));
    }
    if matches!(vt, VT::Quath | VT::Quatf | VT::Quatd) {
        // Text `(real, i, j, k)` → wire imaginary-first.
        let re = comps.remove(0);
        comps.push(re);
    }
    Ok(comps)
}

/// §16.3.9.1 inline packing: int8 components for vectors (Vec2h
/// stores its halves directly), int8 diagonal for matrices.
fn inline_vector(vt: ValueType, comps: &[f64]) -> Option<u32> {
    use ValueType as VT;
    let fits_i8 = |f: f64| f == f.trunc() && (-128.0..=127.0).contains(&f);
    let pack = |bytes: &[u8]| -> u32 {
        let mut b = [0u8; 4];
        b[..bytes.len()].copy_from_slice(bytes);
        u32::from_le_bytes(b)
    };
    match vt {
        VT::Half2 => {
            let a = f32_to_half(comps[0] as f32).to_le_bytes();
            let b = f32_to_half(comps[1] as f32).to_le_bytes();
            Some(pack(&[a[0], a[1], b[0], b[1]]))
        }
        VT::Float2
        | VT::Double2
        | VT::Int2
        | VT::Half3
        | VT::Float3
        | VT::Double3
        | VT::Int3
        | VT::Half4
        | VT::Float4
        | VT::Double4
        | VT::Int4 => {
            if comps.iter().all(|&c| fits_i8(c)) {
                let bytes: Vec<u8> = comps.iter().map(|&c| c as i8 as u8).collect();
                Some(pack(&bytes))
            } else {
                None
            }
        }
        VT::Matrix2d | VT::Matrix3d | VT::Matrix4d => {
            let n = match vt {
                VT::Matrix2d => 2,
                VT::Matrix3d => 3,
                _ => 4,
            };
            let mut diag = Vec::with_capacity(n);
            for r in 0..n {
                for c in 0..n {
                    let x = comps[r * n + c];
                    if r == c {
                        if !fits_i8(x) {
                            return None;
                        }
                        diag.push(x as i8 as u8);
                    } else if x != 0.0 {
                        return None;
                    }
                }
            }
            Some(pack(&diag))
        }
        _ => None,
    }
}

/// Fixed-width element bytes (§16.3.10.22–24).
fn element_bytes(vt: ValueType, comps: &[f64]) -> Result<Vec<u8>> {
    use ValueType as VT;
    let mut out = Vec::new();
    match vt {
        VT::Half2 | VT::Half3 | VT::Half4 | VT::Quath => {
            for &c in comps {
                out.extend_from_slice(&f32_to_half(c as f32).to_le_bytes());
            }
        }
        VT::Float2 | VT::Float3 | VT::Float4 | VT::Quatf => {
            for &c in comps {
                out.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        VT::Int2 | VT::Int3 | VT::Int4 => {
            for &c in comps {
                out.extend_from_slice(&(c as i32).to_le_bytes());
            }
        }
        VT::Double2
        | VT::Double3
        | VT::Double4
        | VT::Quatd
        | VT::Matrix2d
        | VT::Matrix3d
        | VT::Matrix4d => {
            for &c in comps {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        other => {
            return Err(invalid(format!(
                "USDC encode: {} has no fixed-width element layout",
                other.name()
            )))
        }
    }
    Ok(out)
}

/// IEEE-754 binary16 from binary32, round-to-nearest-even
/// (inverse of [`crate::usdc_values::half_to_f32`]).
pub fn f32_to_half(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let frac = bits & 0x7F_FFFF;
    if exp == 0xFF {
        // Inf / NaN.
        return sign | 0x7C00 | if frac != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1F {
        return sign | 0x7C00; // overflow → inf
    }
    if e <= 0 {
        if e < -10 {
            return sign; // underflow → ±0
        }
        // Denormal half.
        let m = (frac | 0x80_0000) >> (1 - e);
        // Round to nearest even on the 13 dropped bits.
        let rounded = (m + 0x0FFF + ((m >> 13) & 1)) >> 13;
        return sign | rounded as u16;
    }
    let mut h = ((e as u32) << 10) | (frac >> 13);
    let rem = frac & 0x1FFF;
    if rem > 0x1000 || (rem == 0x1000 && (h & 1) == 1) {
        h += 1; // may carry into the exponent (correct: rounds up)
    }
    sign | h as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usdc_values::half_to_f32;

    #[test]
    fn half_round_trips_representable_values() {
        for f in [
            0.0f32,
            1.0,
            -1.0,
            0.5,
            65504.0,
            1.0 / 1024.0,
            3.140625,
            -0.1,
        ] {
            let h = f32_to_half(f);
            let back = half_to_f32(h);
            let f16_exact = half_to_f32(f32_to_half(back));
            assert_eq!(back, f16_exact, "{f}");
            assert!((back - f).abs() <= f.abs() * 1e-3 + 1e-7, "{f} → {back}");
        }
        assert_eq!(f32_to_half(f32::INFINITY), 0x7C00);
        assert_eq!(f32_to_half(f32::NEG_INFINITY), 0xFC00);
        assert!(f32_to_half(f32::NAN) & 0x7C00 == 0x7C00);
        assert_eq!(f32_to_half(1e10), 0x7C00, "overflow saturates to inf");
        assert_eq!(f32_to_half(1e-9), 0, "underflow to zero");
    }

    #[test]
    fn wire_type_covers_the_schema_spellings() {
        assert_eq!(wire_type("point3f[]"), Some((ValueType::Float3, true)));
        assert_eq!(wire_type("uniform token"), Some((ValueType::Token, false)));
        assert_eq!(wire_type("custom half3[]"), Some((ValueType::Half3, true)));
        assert_eq!(wire_type("matrix4d"), Some((ValueType::Matrix4d, false)));
        assert_eq!(wire_type("quath[]"), Some((ValueType::Quath, true)));
        assert_eq!(wire_type("rel"), None);
    }

    #[test]
    fn inline_vector_packs_small_ints_and_diagonals() {
        assert_eq!(
            inline_vector(ValueType::Float3, &[1.0, -2.0, 3.0]),
            Some(u32::from_le_bytes([1, 0xFE, 3, 0]))
        );
        assert_eq!(inline_vector(ValueType::Float3, &[1.5, 0.0, 0.0]), None);
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        assert_eq!(
            inline_vector(ValueType::Matrix4d, &identity),
            Some(0x0101_0101)
        );
        let mut shifted = identity;
        shifted[12] = 5.0;
        assert_eq!(inline_vector(ValueType::Matrix4d, &shifted), None);
    }
}
