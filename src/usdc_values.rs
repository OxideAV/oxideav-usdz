//! §16.3.9 / §16.3.10 — typed decoding of Crate value representations.
//!
//! [`ValueDecoder`] resolves a §4.3 [`ValueRep`] to a concrete
//! [`usda::Value`], so a `.usdc` field decodes to the same value model
//! the USDA text parser produces and the whole downstream pipeline
//! (composition, `usd_to_scene`) can consume it unchanged.
//!
//! Grounding: the AOUSD *USD Core Specification* 1.0.1 —
//! §16.3.9 (value representations: payload / type byte / flag byte,
//! inlining rules, offset scalars, array forms), §16.3.9.3.x
//! (compressed integral + compressed floating-point arrays, LUT
//! form), and §16.3.10 (per-type wire encodings). Every layout with
//! any textual ambiguity is additionally pinned against the committed
//! Elephant fixture (`docs/3d/usd/fixtures/`), noted inline.
//!
//! Version posture: everything needed for Crate 0.8.0 files plus the
//! 0.9.0 `TimeCode` and 0.10.0 `PathExpression` value types is
//! implemented, including `Relocates` (0.11.0) and `Splines` (0.12.0);
//! `UnregisteredValueListOp` payloads refuse
//! with a precise `Unsupported` — the reader's version gate keeps
//! such files out before a rep of those types can be met.

use std::collections::BTreeMap;

use crate::error::{invalid, unsupported};
use crate::usda::{ListEditOp, ListOp, Value};
use crate::usdc::{
    construct_paths, decode_int_array, decode_int_array64, int_coded_max_len, CompressedBuffer,
    SectionName, TokensSection, UsdcFile, ValueRep, ValueType, BOOTSTRAP_SIZE,
};
use crate::Result;

/// Maximum recursion depth for indirect values (`Value`,
/// `UnregisteredValue`, dictionary members, time-sample members).
/// §16.3.10.17 requires guarding against pointer loops; real files
/// nest a handful of levels at most.
const MAX_DEPTH: u32 = 16;

/// Decoder for §4.3 value representations, holding the resolved
/// TOKENS / STRINGS / PATHS pools the per-type encodings index into.
#[derive(Debug)]
pub struct ValueDecoder<'a> {
    bytes: &'a [u8],
    /// End of the value region: the tail TOC offset (§16.3.8.4 puts
    /// the TOC after all value data), clamped to the file length.
    region_end: usize,
    tokens: Vec<String>,
    strings: Vec<String>,
    paths: Vec<String>,
}

impl<'a> ValueDecoder<'a> {
    /// Build a decoder for `file`, resolving the TOKENS pool
    /// (required), the STRINGS pool, and the §16.3.8.4.5.4 path table
    /// (each empty when its section is absent — a conforming file
    /// without string- or path-valued reps needs neither).
    ///
    /// `bytes` must be the same buffer [`UsdcFile::parse`] was called
    /// on.
    pub fn new(file: &UsdcFile, bytes: &'a [u8]) -> Result<Self> {
        let tokens_bytes = file
            .section_bytes(SectionName::Tokens, bytes)
            .ok_or_else(|| invalid("USDC: TOKENS section absent — cannot decode values"))?;
        let tokens = TokensSection::parse(tokens_bytes)?.decode()?;
        let strings = match file.section_bytes(SectionName::Strings, bytes) {
            Some(_) => file.decode_strings(bytes)?,
            None => Vec::new(),
        };
        let paths = match file.section_bytes(SectionName::Paths, bytes) {
            Some(_) => {
                let elements = file.decode_path_elements(bytes)?;
                construct_paths(&elements, &tokens)?
            }
            None => Vec::new(),
        };
        let region_end = usize::try_from(file.bootstrap.toc_offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        Ok(Self {
            bytes,
            region_end,
            tokens,
            strings,
            paths,
        })
    }

    /// The resolved §4.1 TOKENS pool.
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// The resolved §4.2 STRINGS pool.
    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    /// The reconstructed path table (§16.3.8.4.5.4), indexed by path
    /// index.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Decode one value representation to a [`usda::Value`].
    pub fn decode(&self, rep: ValueRep) -> Result<Value> {
        self.decode_at_depth(rep, 0)
    }

    // ---- pools -----------------------------------------------------

    fn token(&self, idx: u64) -> Result<&str> {
        self.tokens
            .get(usize::try_from(idx).unwrap_or(usize::MAX))
            .map(String::as_str)
            .ok_or_else(|| {
                invalid(format!(
                    "USDC value: token index {idx} outside the {}-atom TOKENS pool",
                    self.tokens.len()
                ))
            })
    }

    fn string(&self, idx: u64) -> Result<&str> {
        self.strings
            .get(usize::try_from(idx).unwrap_or(usize::MAX))
            .map(String::as_str)
            .ok_or_else(|| {
                invalid(format!(
                    "USDC value: string index {idx} outside the {}-entry STRINGS pool",
                    self.strings.len()
                ))
            })
    }

    fn path(&self, idx: u64) -> Result<&str> {
        self.paths
            .get(usize::try_from(idx).unwrap_or(usize::MAX))
            .map(String::as_str)
            .ok_or_else(|| {
                invalid(format!(
                    "USDC value: path index {idx} outside the {}-entry path table",
                    self.paths.len()
                ))
            })
    }

    // ---- bounded raw reads ----------------------------------------

    fn get(&self, off: usize, len: usize) -> Result<&'a [u8]> {
        let end = off
            .checked_add(len)
            .ok_or_else(|| invalid(format!("USDC value: offset {off:#x} + {len} overflows")))?;
        if off < BOOTSTRAP_SIZE || end > self.region_end {
            return Err(invalid(format!(
                "USDC value: read [{off:#x}, {end:#x}) outside the value region [{BOOTSTRAP_SIZE:#x}, {:#x})",
                self.region_end
            )));
        }
        Ok(&self.bytes[off..end])
    }

    fn u64_at(&self, off: usize) -> Result<u64> {
        Ok(u64::from_le_bytes(self.get(off, 8)?.try_into().unwrap()))
    }

    fn i64_at(&self, off: usize) -> Result<i64> {
        Ok(i64::from_le_bytes(self.get(off, 8)?.try_into().unwrap()))
    }

    fn count_at(&self, off: usize, what: &str) -> Result<usize> {
        let count = self.u64_at(off)?;
        // A count can never exceed the number of remaining bytes (every
        // element is at least one byte), which also bounds allocation.
        let cap = self.region_end - off;
        if count as u128 > cap as u128 {
            return Err(invalid(format!(
                "USDC value: {what} count {count} at {off:#x} exceeds the {cap} remaining value-region bytes"
            )));
        }
        Ok(count as usize)
    }

    // ---- dispatch --------------------------------------------------

    fn decode_at_depth(&self, rep: ValueRep, depth: u32) -> Result<Value> {
        if depth > MAX_DEPTH {
            return Err(invalid(
                "USDC value: indirection depth exceeded (recursive value pointers — §16.3.10.17 requires guarding against loops)",
            ));
        }
        let vt = rep.value_type().ok_or_else(|| {
            unsupported(format!(
                "USDC value: type code {:#04x} is not in the §16.3.10.1 table",
                rep.type_code()
            ))
        })?;
        if rep.is_array() {
            return self.decode_array(vt, rep);
        }
        if rep.is_inline() {
            return self.decode_inline(vt, rep);
        }
        self.decode_offset(vt, rep, depth)
    }

    // ---- inline scalars (§16.3.9.1) --------------------------------

    fn decode_inline(&self, vt: ValueType, rep: ValueRep) -> Result<Value> {
        use ValueType as VT;
        // §16.3.9.1: only the first (lowest) 4 bytes of the payload may
        // encode data.
        let p = (rep.payload & 0xFFFF_FFFF) as u32;
        let b = p.to_le_bytes();
        Ok(match vt {
            VT::Bool => Value::Bool(p != 0),
            VT::Uchar => Value::Float(f64::from(b[0])),
            VT::Int => Value::Float(f64::from(p as i32)),
            VT::Uint => Value::Float(f64::from(p)),
            // §16.3.10.6/.7: (u)int64 inline as (u)int32.
            VT::Int64 => Value::Float(f64::from(p as i32)),
            VT::Uint64 => Value::Float(f64::from(p)),
            VT::Half => Value::Float(f64::from(half_to_f32(p as u16))),
            VT::Float => Value::Float(f64::from(f32::from_bits(p))),
            // §16.3.10.10/.32: double / VT::TimeCode inline as float.
            VT::Double | VT::TimeCode => Value::Float(f64::from(f32::from_bits(p))),
            VT::String => Value::String(self.string(u64::from(p))?.to_owned()),
            VT::Token => Value::Token(self.token(u64::from(p))?.to_owned()),
            // §16.3.10.13/.14: inlined asset paths (and path
            // expressions, which share the representation) index the
            // TOKENS pool.
            VT::Asset => Value::Asset(self.token(u64::from(p))?.to_owned()),
            VT::PathExpression => Value::String(self.token(u64::from(p))?.to_owned()),
            VT::Specifier => Value::Token(specifier_name(p as i32)?.to_owned()),
            VT::Permission => Value::Token(permission_name(p as i32)?.to_owned()),
            VT::Variability => Value::Token(variability_name(p as i32)?.to_owned()),
            // §16.3.9.1: an empty dictionary is represented inlined.
            VT::Dictionary => Value::Dict(BTreeMap::new()),
            VT::ValueBlock => Value::None,
            // Two-dimension types that fit 4 bytes store components
            // directly (§16.3.9.1 — the Vec2h example); everything
            // wider stores int8 components.
            VT::Half2 => Value::Tuple(vec![
                Value::Float(f64::from(half_to_f32(u16::from_le_bytes([b[0], b[1]])))),
                Value::Float(f64::from(half_to_f32(u16::from_le_bytes([b[2], b[3]])))),
            ]),
            VT::Double2 | VT::Float2 | VT::Int2 => tuple_from_i8(&b[..2]),
            VT::Double3 | VT::Float3 | VT::Half3 | VT::Int3 => tuple_from_i8(&b[..3]),
            VT::Double4 | VT::Float4 | VT::Half4 | VT::Int4 => tuple_from_i8(&b[..4]),
            // §16.3.9.1: matrices inline their int8 diagonal (pinned by
            // the fixture's 0x01010101 identity matrix4d payloads).
            VT::Matrix2d => diagonal_matrix(&b[..2]),
            VT::Matrix3d => diagonal_matrix(&b[..3]),
            VT::Matrix4d => diagonal_matrix(&b[..4]),
            other => {
                return Err(invalid(format!(
                    "USDC value: type {} cannot be inlined",
                    other.name()
                )))
            }
        })
    }

    // ---- offset scalars + structured values ------------------------

    fn decode_offset(&self, vt: ValueType, rep: ValueRep, depth: u32) -> Result<Value> {
        use ValueType as VT;
        let off = usize::try_from(rep.payload)
            .map_err(|_| invalid("USDC value: offset payload does not fit in usize"))?;
        Ok(match vt {
            VT::Double | VT::TimeCode => {
                Value::Float(f64::from_le_bytes(self.get(off, 8)?.try_into().unwrap()))
            }
            VT::Int64 => Value::Float(self.i64_at(off)? as f64),
            VT::Uint64 => Value::Float(self.u64_at(off)? as f64),
            // §16.3.10.13: a non-inlined asset is an Index to the
            // STRINGS section (the payload is the index, not an
            // offset). VT::PathExpression shares the representation.
            VT::Asset => Value::Asset(self.string(rep.payload)?.to_owned()),
            VT::PathExpression => Value::String(self.string(rep.payload)?.to_owned()),
            VT::Specifier => Value::Token(specifier_name(self.i32_scalar_at(off)?)?.to_owned()),
            VT::Permission => Value::Token(permission_name(self.i32_scalar_at(off)?)?.to_owned()),
            VT::Variability => Value::Token(variability_name(self.i32_scalar_at(off)?)?.to_owned()),
            VT::Matrix2d
            | VT::Matrix3d
            | VT::Matrix4d
            | VT::Quatd
            | VT::Quatf
            | VT::Quath
            | VT::Double2
            | VT::Float2
            | VT::Half2
            | VT::Int2
            | VT::Double3
            | VT::Float3
            | VT::Half3
            | VT::Int3
            | VT::Double4
            | VT::Float4
            | VT::Half4
            | VT::Int4 => {
                let width = scalar_width(vt).expect("dimensioned types have widths");
                decode_element(vt, self.get(off, width)?)?
            }
            VT::Dictionary => Value::Dict(self.decode_dictionary(off, depth)?),
            VT::TokenVector => self.decode_index_vector(off, "VT::TokenVector", |i, s| {
                s.token(i).map(|t| Value::Token(t.to_owned()))
            })?,
            VT::StringVector => self.decode_index_vector(off, "VT::StringVector", |i, s| {
                s.string(i).map(|t| Value::String(t.to_owned()))
            })?,
            VT::PathVector => self.decode_index_vector(off, "VT::PathVector", |i, s| {
                s.path(i).map(|t| Value::Path(t.to_owned()))
            })?,
            VT::DoubleVector => {
                let count = self.count_at(off, "VT::DoubleVector")?;
                let data = self.get(off + 8, count * 8)?;
                Value::Array(
                    data.chunks_exact(8)
                        .map(|c| Value::Float(f64::from_le_bytes(c.try_into().unwrap())))
                        .collect(),
                )
            }
            VT::LayerOffsetVector => {
                let count = self.count_at(off, "VT::LayerOffsetVector")?;
                let data = self.get(off + 8, count * 16)?;
                Value::Array(
                    data.chunks_exact(16)
                        .map(|c| {
                            Value::Tuple(vec![
                                Value::Float(f64::from_le_bytes(c[0..8].try_into().unwrap())),
                                Value::Float(f64::from_le_bytes(c[8..16].try_into().unwrap())),
                            ])
                        })
                        .collect(),
                )
            }
            VT::TokenListOp
            | VT::StringListOp
            | VT::PathListOp
            | VT::IntListOp
            | VT::Int64ListOp
            | VT::UIntListOp
            | VT::UInt64ListOp
            | VT::ReferenceListOp
            | VT::PayloadListOp => self.decode_list_op(vt, off, depth)?,
            VT::VariantSelectionMap => {
                let count = self.count_at(off, "VT::VariantSelectionMap")?;
                let data = self.get(off + 8, count * 8)?;
                let mut map = BTreeMap::new();
                for pair in data.chunks_exact(8) {
                    let key = u32::from_le_bytes(pair[0..4].try_into().unwrap());
                    let val = u32::from_le_bytes(pair[4..8].try_into().unwrap());
                    map.insert(
                        self.string(u64::from(key))?.to_owned(),
                        Value::String(self.string(u64::from(val))?.to_owned()),
                    );
                }
                Value::Dict(map)
            }
            VT::TimeSamples => self.decode_time_samples(off, depth)?,
            VT::Payload => {
                let (value, _) = self.decode_reference(off, false, depth)?;
                value
            }
            VT::Value => {
                // §16.3.10.17: an indirect pointer to a rep word.
                let inner = ValueRep::from_raw(self.u64_at(off)?);
                self.decode_at_depth(inner, depth + 1)?
            }
            VT::UnregisteredValue => {
                // §16.3.10.18: an i64 offset (from its own position) to
                // a rep word restricted to string / dictionary /
                // unregistered-value list op.
                let rel = self.i64_at(off)?;
                let target = checked_rel(off, rel)?;
                let inner = ValueRep::from_raw(self.u64_at(target)?);
                match inner.value_type() {
                    Some(VT::String) | Some(VT::Dictionary) | Some(VT::UnregisteredValueOp) => {}
                    other => {
                        return Err(invalid(format!(
                            "USDC VT::UnregisteredValue: inner value must be a string, dictionary, or unregistered-value list op, found {:?}",
                            other.map(ValueType::name)
                        )))
                    }
                }
                self.decode_at_depth(inner, depth + 1)?
            }
            VT::UnregisteredValueOp => {
                return Err(unsupported(
                    "USDC value: UnregisteredValueListOp payloads are not decoded yet",
                ))
            }
            VT::Relocates => {
                // §16.3.10.15: `[u64 count][count × (u32 source path
                // index, u32 target path index)]`.
                let count = self.count_at(off, "VT::Relocates")?;
                let data = self.get(off + 8, count * 8)?;
                let mut pairs = Vec::with_capacity(count);
                for pair in data.chunks_exact(8) {
                    let src = u32::from_le_bytes(pair[0..4].try_into().unwrap());
                    let dst = u32::from_le_bytes(pair[4..8].try_into().unwrap());
                    pairs.push((
                        self.path(u64::from(src))?.to_owned(),
                        self.path(u64::from(dst))?.to_owned(),
                    ));
                }
                Value::Relocates(pairs)
            }
            VT::Splines => {
                // §16.3.10.33: `[u64 byteCount][byteCount bytes of
                // spline data][u64 customCount][customCount × (f64
                // timeCode, dictionary)]`.
                let byte_count = self.count_at(off, "VT::Splines data")?;
                let data = self.get(off + 8, byte_count)?;
                let mut cursor = off + 8 + byte_count;
                let custom_count = self.count_at(cursor, "VT::Splines custom data")?;
                cursor += 8;
                let mut custom = Vec::with_capacity(custom_count);
                for _ in 0..custom_count {
                    let time = f64::from_le_bytes(self.get(cursor, 8)?.try_into().unwrap());
                    cursor += 8;
                    let (dict, end) = self.decode_dictionary_span(cursor, depth + 1)?;
                    cursor = end;
                    custom.push((time, dict));
                }
                Value::Spline(Box::new(crate::spline::Spline::from_crate_bytes(
                    data, custom,
                )?))
            }
            VT::ValueBlock => Value::None,
            // §16.3.9.1 lists these as always inlined; a non-inline
            // non-array rep of these types is malformed.
            VT::Bool
            | VT::Uchar
            | VT::Int
            | VT::Uint
            | VT::Half
            | VT::Float
            | VT::String
            | VT::Token => {
                return Err(invalid(format!(
                    "USDC value: type {} must be inlined when not an array (§16.3.9.1)",
                    vt.name()
                )))
            }
        })
    }

    fn i32_scalar_at(&self, off: usize) -> Result<i32> {
        Ok(i32::from_le_bytes(self.get(off, 4)?.try_into().unwrap()))
    }

    /// §16.3.10.19 dictionary: `[u64 count]` then per entry a 4-byte
    /// token-index key and an 8-byte signed offset (relative to the
    /// offset field's own position) to the member's rep word.
    /// §16.3.10.19 dictionary: `[u64 count]`, then per member a
    /// `u32` key token and an `i64` forward offset (from the `i64`'s
    /// own position) to the member's rep word. Members are
    /// **chained**: the member's payload bytes sit between its `i64`
    /// and its rep word, and the next member's key follows that rep
    /// word — pinned on a production file (a black-box-produced
    /// dictionary crate; §16.3.10.19 states the offset but not the
    /// interleaving). Returns the map and the position just past the
    /// last member's rep word.
    fn decode_dictionary(&self, off: usize, depth: u32) -> Result<BTreeMap<String, Value>> {
        Ok(self.decode_dictionary_span(off, depth)?.0)
    }

    fn decode_dictionary_span(
        &self,
        off: usize,
        depth: u32,
    ) -> Result<(BTreeMap<String, Value>, usize)> {
        let count = self.count_at(off, "dictionary")?;
        let mut map = BTreeMap::new();
        let mut cursor = off + 8;
        for _ in 0..count {
            let key_idx = u32::from_le_bytes(self.get(cursor, 4)?.try_into().unwrap());
            let key = self.token(u64::from(key_idx))?.to_owned();
            let rel = self.i64_at(cursor + 4)?;
            let target = checked_rel(cursor + 4, rel)?;
            let inner = ValueRep::from_raw(self.u64_at(target)?);
            map.insert(key, self.decode_at_depth(inner, depth + 1)?);
            cursor = target + 8;
        }
        Ok((map, cursor))
    }

    /// §16.3.10.26 index-element vectors: `[u64 count][count × u32]`.
    fn decode_index_vector(
        &self,
        off: usize,
        what: &str,
        resolve: impl Fn(u64, &Self) -> Result<Value>,
    ) -> Result<Value> {
        let count = self.count_at(off, what)?;
        let data = self.get(off + 8, count * 4)?;
        let mut out = Vec::with_capacity(count);
        for c in data.chunks_exact(4) {
            let idx = u32::from_le_bytes(c.try_into().unwrap());
            out.push(resolve(u64::from(idx), self)?);
        }
        Ok(Value::Array(out))
    }

    /// §16.3.10.25 list operations: a bitmask header byte then, per
    /// set bit, `[u64 count][count × element]` in the documented
    /// order (Explicit, Add, Prepend, Append, Delete, Reorder).
    fn decode_list_op(&self, vt: ValueType, off: usize, depth: u32) -> Result<Value> {
        const MAKE_EXPLICIT: u8 = 0x1;
        const ADD_EXPLICIT: u8 = 0x2;
        const ADD: u8 = 0x4;
        const DELETE: u8 = 0x8;
        const REORDER: u8 = 0x10;
        const PREPEND: u8 = 0x20;
        const APPEND: u8 = 0x40;

        let header = self.get(off, 1)?[0];
        let mut cursor = off + 1;
        let mut op = ListOp::default();
        // The arrays follow in this fixed order (§16.3.10.25).
        let sublists = [
            (ADD_EXPLICIT, ListEditOp::Explicit),
            (ADD, ListEditOp::Add),
            (PREPEND, ListEditOp::Prepend),
            (APPEND, ListEditOp::Append),
            (DELETE, ListEditOp::Delete),
            (REORDER, ListEditOp::Reorder),
        ];
        let mut any = false;
        for (bit, edit) in sublists {
            if header & bit == 0 {
                continue;
            }
            any = true;
            let (items, next) = self.decode_list_op_items(vt, cursor, depth)?;
            cursor = next;
            op.set(edit, Value::Array(items));
        }
        if header & MAKE_EXPLICIT != 0 && !any {
            // §16.3.10.25: only Make Explicit set = an explicit empty
            // list.
            op.set(ListEditOp::Explicit, Value::Array(Vec::new()));
        }
        Ok(Value::ListOp(Box::new(op)))
    }

    fn decode_list_op_items(
        &self,
        vt: ValueType,
        off: usize,
        depth: u32,
    ) -> Result<(Vec<Value>, usize)> {
        use ValueType as VT;
        let count = self.count_at(off, "list-op sublist")?;
        let mut cursor = off + 8;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let (value, next) = match vt {
                VT::TokenListOp => {
                    let idx = u32::from_le_bytes(self.get(cursor, 4)?.try_into().unwrap());
                    (
                        Value::Token(self.token(u64::from(idx))?.to_owned()),
                        cursor + 4,
                    )
                }
                VT::StringListOp => {
                    let idx = u32::from_le_bytes(self.get(cursor, 4)?.try_into().unwrap());
                    (
                        Value::String(self.string(u64::from(idx))?.to_owned()),
                        cursor + 4,
                    )
                }
                VT::PathListOp => {
                    let idx = u32::from_le_bytes(self.get(cursor, 4)?.try_into().unwrap());
                    (
                        Value::Path(self.path(u64::from(idx))?.to_owned()),
                        cursor + 4,
                    )
                }
                VT::IntListOp => (
                    Value::Float(f64::from(i32::from_le_bytes(
                        self.get(cursor, 4)?.try_into().unwrap(),
                    ))),
                    cursor + 4,
                ),
                VT::UIntListOp => (
                    Value::Float(f64::from(u32::from_le_bytes(
                        self.get(cursor, 4)?.try_into().unwrap(),
                    ))),
                    cursor + 4,
                ),
                VT::Int64ListOp => (Value::Float(self.i64_at(cursor)? as f64), cursor + 8),
                VT::UInt64ListOp => (Value::Float(self.u64_at(cursor)? as f64), cursor + 8),
                VT::ReferenceListOp => self.decode_reference(cursor, true, depth)?,
                VT::PayloadListOp => self.decode_reference(cursor, false, depth)?,
                _ => unreachable!("decode_list_op only dispatches list-op types"),
            };
            out.push(value);
            cursor = next;
        }
        Ok((out, cursor))
    }

    /// §16.3.10.21 references / payloads: a 4-byte STRINGS index for
    /// the asset layer path (empty = internal reference), a 4-byte
    /// path index for the target prim, a 16-byte layer offset, and —
    /// for references only — a trailing custom-data dictionary.
    /// Returns the value plus the byte position after the record.
    fn decode_reference(
        &self,
        off: usize,
        with_custom_data: bool,
        depth: u32,
    ) -> Result<(Value, usize)> {
        let head = self.get(off, 24)?;
        let asset_idx = u32::from_le_bytes(head[0..4].try_into().unwrap());
        let path_idx = u32::from_le_bytes(head[4..8].try_into().unwrap());
        let asset = self.string(u64::from(asset_idx))?.to_owned();
        let prim_path = self.path(u64::from(path_idx))?.to_owned();
        // The 16-byte layer offset (time offset + scale doubles) and
        // any custom data have no slot on `usda::Value`'s composition
        // arc form; they are decoded for framing correctness.
        let mut cursor = off + 24;
        if with_custom_data {
            // Walk the dictionary to find its end (and validate it).
            let (_, end) = self.decode_dictionary_span(cursor, depth)?;
            cursor = end;
        }
        let prim_path = if prim_path == "/" {
            std::string::String::new()
        } else {
            prim_path
        };
        Ok((Value::AssetWithPath { asset, prim_path }, cursor))
    }

    /// §16.3.10.31 time samples. Layouts pinned on the fixture:
    /// at the payload offset `O` sits an `i64` whose target (relative
    /// to `O` itself) is the rep word of the timecodes DoubleVector;
    /// immediately after that rep word sits another `i64` whose target
    /// (relative to its own position) is the values array —
    /// `[u64 count][count × u64 rep]`, index-paired with the
    /// timecodes.
    fn decode_time_samples(&self, off: usize, depth: u32) -> Result<Value> {
        let times_rel = self.i64_at(off)?;
        let times_rep_pos = checked_rel(off, times_rel)?;
        let times_rep = ValueRep::from_raw(self.u64_at(times_rep_pos)?);
        if times_rep.value_type() != Some(ValueType::DoubleVector) {
            return Err(invalid(format!(
                "USDC TimeSamples: timecodes rep must be a DoubleVector, found type code {:#04x}",
                times_rep.type_code()
            )));
        }
        let times = match self.decode_at_depth(times_rep, depth + 1)? {
            Value::Array(items) => items
                .into_iter()
                .map(|v| match v {
                    Value::Float(f) => Ok(f),
                    _ => Err(invalid("USDC TimeSamples: non-numeric timecode")),
                })
                .collect::<Result<Vec<f64>>>()?,
            _ => {
                return Err(invalid(
                    "USDC TimeSamples: timecodes did not decode to an array",
                ))
            }
        };
        let values_off_pos = times_rep_pos + 8;
        let values_rel = self.i64_at(values_off_pos)?;
        let values_pos = checked_rel(values_off_pos, values_rel)?;
        let count = self.count_at(values_pos, "TimeSamples values")?;
        if count != times.len() {
            return Err(invalid(format!(
                "USDC TimeSamples: {count} values for {} timecodes",
                times.len()
            )));
        }
        let reps = self.get(values_pos + 8, count * 8)?;
        let mut samples = Vec::with_capacity(count);
        for (i, chunk) in reps.chunks_exact(8).enumerate() {
            let rep = ValueRep::from_raw(u64::from_le_bytes(chunk.try_into().unwrap()));
            samples.push((times[i], self.decode_at_depth(rep, depth + 1)?));
        }
        Ok(Value::TimeSamples(samples))
    }

    // ---- arrays (§16.3.9.3) ---------------------------------------

    fn decode_array(&self, vt: ValueType, rep: ValueRep) -> Result<Value> {
        use ValueType as VT;
        if !vt.supports_array() {
            return Err(invalid(format!(
                "USDC value: type {} does not support arrays (§16.3.10.1)",
                vt.name()
            )));
        }
        // §16.3.9.3: a zero payload is the empty array.
        if rep.payload == 0 {
            return Ok(Value::Array(Vec::new()));
        }
        let off = usize::try_from(rep.payload)
            .map_err(|_| invalid("USDC value: array offset does not fit in usize"))?;
        let count = self.count_at(off, "array")?;
        let data_off = off + 8;

        if rep.is_compressed() {
            return self.decode_compressed_array(vt, data_off, count);
        }

        let width = scalar_width(vt).ok_or_else(|| {
            invalid(format!(
                "USDC value: no element width for array type {}",
                vt.name()
            ))
        })?;
        let data = self.get(data_off, count * width)?;
        let mut out = Vec::with_capacity(count);
        for chunk in data.chunks_exact(width) {
            out.push(match vt {
                VT::String => Value::String(
                    self.string(u64::from(u32::from_le_bytes(chunk.try_into().unwrap())))?
                        .to_owned(),
                ),
                VT::Token => Value::Token(
                    self.token(u64::from(u32::from_le_bytes(chunk.try_into().unwrap())))?
                        .to_owned(),
                ),
                VT::Asset => Value::Asset(
                    self.string(u64::from(u32::from_le_bytes(chunk.try_into().unwrap())))?
                        .to_owned(),
                ),
                VT::PathExpression => Value::String(
                    self.string(u64::from(u32::from_le_bytes(chunk.try_into().unwrap())))?
                        .to_owned(),
                ),
                _ => decode_element(vt, chunk)?,
            });
        }
        Ok(Value::Array(out))
    }

    /// §16.3.9.3.1 / §16.3.9.3.2 compressed arrays. `data_off` points
    /// just past the array's `u64 count`.
    fn decode_compressed_array(
        &self,
        vt: ValueType,
        data_off: usize,
        count: usize,
    ) -> Result<Value> {
        use ValueType as VT;
        match vt {
            VT::Int => Ok(Value::Array(
                self.compressed_ints(data_off, count)?
                    .into_iter()
                    .map(|v| Value::Float(f64::from(v)))
                    .collect(),
            )),
            VT::Uint => Ok(Value::Array(
                self.compressed_ints(data_off, count)?
                    .into_iter()
                    .map(|v| Value::Float(f64::from(v as u32)))
                    .collect(),
            )),
            VT::Int64 => Ok(Value::Array(
                self.compressed_ints64(data_off, count)?
                    .into_iter()
                    .map(|v| Value::Float(v as f64))
                    .collect(),
            )),
            VT::Uint64 => Ok(Value::Array(
                self.compressed_ints64(data_off, count)?
                    .into_iter()
                    .map(|v| Value::Float(v as u64 as f64))
                    .collect(),
            )),
            VT::Half | VT::Float | VT::Double => self.decode_compressed_float_array(vt, data_off, count),
            other => Err(invalid(format!(
                "USDC value: type {} arrays cannot be compressed (§16.3.9.3 restricts compression to simple element types)",
                other.name()
            ))),
        }
    }

    /// One §16.3.7.2.3 compressed-integer stream at `off`:
    /// `[u64 compressedSize][§3a chunked LZ4]` whose decompressed body
    /// is the §3b int-coded array. Returns the raw decompressed body.
    fn compressed_int_body(&self, off: usize, budget: usize) -> Result<Vec<u8>> {
        let csize = self.u64_at(off)?;
        let csize = usize::try_from(csize)
            .map_err(|_| invalid("USDC value: compressedSize does not fit in usize"))?;
        let block = self.get(off + 8, csize)?;
        CompressedBuffer::parse(block)?.decompress(budget)
    }

    fn compressed_ints(&self, off: usize, count: usize) -> Result<Vec<i32>> {
        let body = self.compressed_int_body(off, int_coded_max_len(count))?;
        decode_int_array(&body, count)
    }

    fn compressed_ints64(&self, off: usize, count: usize) -> Result<Vec<i64>> {
        // The 64-bit form doubles every width; 8 + count * 8 is a
        // conservative budget mirroring `int_coded_max_len`.
        let budget = 8 + count.div_ceil(4) + count * 8;
        let body = self.compressed_int_body(off, budget)?;
        decode_int_array64(&body, count)
    }

    /// §16.3.9.3.2 compressed floating-point arrays: a leading
    /// encoding character — `i` (all elements integral, one
    /// compressed-integer stream) or `t` (lookup table:
    /// `[u32 lutCount][lutCount × element]` then a compressed-integer
    /// stream of indices). Both layouts pinned on the fixture's
    /// `float[]` reps.
    fn decode_compressed_float_array(
        &self,
        vt: ValueType,
        data_off: usize,
        count: usize,
    ) -> Result<Value> {
        let code = self.get(data_off, 1)?[0];
        match code {
            b'i' => {
                let ints = self.compressed_ints(data_off + 1, count)?;
                Ok(Value::Array(
                    ints.into_iter().map(|v| Value::Float(f64::from(v))).collect(),
                ))
            }
            b't' => {
                let lut_count =
                    u32::from_le_bytes(self.get(data_off + 1, 4)?.try_into().unwrap()) as usize;
                let width = scalar_width(vt).expect("float types have widths");
                let lut_bytes = self.get(data_off + 5, lut_count * width)?;
                let lut: Vec<f64> = lut_bytes
                    .chunks_exact(width)
                    .map(|c| float_from_bytes(vt, c))
                    .collect();
                let indices =
                    self.compressed_ints(data_off + 5 + lut_count * width, count)?;
                let mut out = Vec::with_capacity(count);
                for idx in indices {
                    let value = usize::try_from(idx)
                        .ok()
                        .and_then(|i| lut.get(i))
                        .ok_or_else(|| {
                            invalid(format!(
                                "USDC value: LUT index {idx} outside the {lut_count}-entry table"
                            ))
                        })?;
                    out.push(Value::Float(*value));
                }
                Ok(Value::Array(out))
            }
            other => Err(invalid(format!(
                "USDC value: unknown compressed-float encoding character {other:#04x} (§16.3.9.3.2 defines 'i' and 't')"
            ))),
        }
    }
}

/// Apply a signed relative offset to a base position, bounds-checked.
fn checked_rel(base: usize, rel: i64) -> Result<usize> {
    let target = (base as i64).checked_add(rel).ok_or_else(|| {
        invalid(format!(
            "USDC value: relative offset {rel} from {base:#x} overflows"
        ))
    })?;
    usize::try_from(target).map_err(|_| {
        invalid(format!(
            "USDC value: relative offset {rel} from {base:#x} is negative"
        ))
    })
}

/// Natural on-disk width of one element of `vt`, for the types that
/// appear as fixed-width scalars / array elements.
fn scalar_width(vt: ValueType) -> Option<usize> {
    use ValueType as VT;
    Some(match vt {
        VT::Bool | VT::Uchar => 1,
        VT::Half => 2,
        VT::Int
        | VT::Uint
        | VT::Float
        | VT::String
        | VT::Token
        | VT::Asset
        | VT::PathExpression => 4,
        VT::Int64
        | VT::Uint64
        | VT::Double
        | VT::TimeCode
        | VT::Quath
        | VT::Float2
        | VT::Int2
        | VT::Half4 => 8,
        VT::Half2 => 4,
        VT::Half3 => 6,
        VT::Float3 | VT::Int3 => 12,
        VT::Quatf | VT::Double2 | VT::Float4 | VT::Int4 => 16,
        VT::Double3 => 24,
        VT::Quatd | VT::Double4 | VT::Matrix2d => 32,
        VT::Matrix3d => 72,
        VT::Matrix4d => 128,
        _ => return None,
    })
}

/// Decode one fixed-width element of a dimensioned / numeric type.
fn decode_element(vt: ValueType, bytes: &[u8]) -> Result<Value> {
    use ValueType as VT;
    fn f32s(bytes: &[u8]) -> Vec<f64> {
        bytes
            .chunks_exact(4)
            .map(|c| f64::from(f32::from_le_bytes(c.try_into().unwrap())))
            .collect()
    }
    fn f64s(bytes: &[u8]) -> Vec<f64> {
        bytes
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }
    fn f16s(bytes: &[u8]) -> Vec<f64> {
        bytes
            .chunks_exact(2)
            .map(|c| f64::from(half_to_f32(u16::from_le_bytes(c.try_into().unwrap()))))
            .collect()
    }
    fn i32s(bytes: &[u8]) -> Vec<f64> {
        bytes
            .chunks_exact(4)
            .map(|c| f64::from(i32::from_le_bytes(c.try_into().unwrap())))
            .collect()
    }
    fn tuple(vals: Vec<f64>) -> Value {
        Value::Tuple(vals.into_iter().map(Value::Float).collect())
    }
    /// §16.3.10.22: on disk the imaginary coefficients come first and
    /// the real coefficient last; the text form displays the real
    /// coefficient first, which is the order `usda::Value` carries.
    fn quat(vals: Vec<f64>) -> Value {
        let (im, re) = vals.split_at(3);
        let mut out = vec![re[0]];
        out.extend_from_slice(im);
        tuple(out)
    }
    /// §16.3.10.24: row-major square matrix -> tuple of row tuples.
    fn matrix(vals: Vec<f64>, n: usize) -> Value {
        Value::Tuple(vals.chunks(n).map(|row| tuple(row.to_vec())).collect())
    }
    Ok(match vt {
        VT::Bool => Value::Bool(bytes[0] != 0),
        VT::Uchar => Value::Float(f64::from(bytes[0])),
        VT::Int => Value::Float(f64::from(i32::from_le_bytes(bytes.try_into().unwrap()))),
        VT::Uint => Value::Float(f64::from(u32::from_le_bytes(bytes.try_into().unwrap()))),
        VT::Int64 => Value::Float(i64::from_le_bytes(bytes.try_into().unwrap()) as f64),
        VT::Uint64 => Value::Float(u64::from_le_bytes(bytes.try_into().unwrap()) as f64),
        VT::Half => Value::Float(f64::from(half_to_f32(u16::from_le_bytes(
            bytes.try_into().unwrap(),
        )))),
        VT::Float => Value::Float(f64::from(f32::from_le_bytes(bytes.try_into().unwrap()))),
        VT::Double | VT::TimeCode => Value::Float(f64::from_le_bytes(bytes.try_into().unwrap())),
        VT::Half2 | VT::Half3 | VT::Half4 => tuple(f16s(bytes)),
        VT::Float2 | VT::Float3 | VT::Float4 => tuple(f32s(bytes)),
        VT::Double2 | VT::Double3 | VT::Double4 => tuple(f64s(bytes)),
        VT::Int2 | VT::Int3 | VT::Int4 => tuple(i32s(bytes)),
        VT::Quath => quat(f16s(bytes)),
        VT::Quatf => quat(f32s(bytes)),
        VT::Quatd => quat(f64s(bytes)),
        VT::Matrix2d => matrix(f64s(bytes), 2),
        VT::Matrix3d => matrix(f64s(bytes), 3),
        VT::Matrix4d => matrix(f64s(bytes), 4),
        other => {
            return Err(invalid(format!(
                "USDC value: {} is not a fixed-width element type",
                other.name()
            )))
        }
    })
}

fn float_from_bytes(vt: ValueType, bytes: &[u8]) -> f64 {
    match vt {
        ValueType::Half => f64::from(half_to_f32(u16::from_le_bytes(bytes.try_into().unwrap()))),
        ValueType::Float => f64::from(f32::from_le_bytes(bytes.try_into().unwrap())),
        _ => f64::from_le_bytes(bytes.try_into().unwrap()),
    }
}

/// Components stored as signed 8-bit integers (§16.3.9.1's fallback
/// packing for dimensioned types wider than the 4-byte inline limit).
fn tuple_from_i8(bytes: &[u8]) -> Value {
    Value::Tuple(
        bytes
            .iter()
            .map(|&b| Value::Float(f64::from(b as i8)))
            .collect(),
    )
}

/// §16.3.9.1: an inlined matrix stores its diagonal as int8s.
fn diagonal_matrix(diag: &[u8]) -> Value {
    let n = diag.len();
    Value::Tuple(
        (0..n)
            .map(|i| {
                Value::Tuple(
                    (0..n)
                        .map(|j| {
                            Value::Float(if i == j {
                                f64::from(diag[i] as i8)
                            } else {
                                0.0
                            })
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

/// §16.3.10.8: IEEE-754 half-precision to f32 (1 sign, 5 exponent,
/// 10 significand bits).
pub fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exp = u32::from((bits >> 10) & 0x1F);
    let frac = u32::from(bits & 0x3FF);
    let out = match (exp, frac) {
        (0, 0) => sign,
        (0, _) => {
            // Denormal: value = frac * 2^-24.
            let f = frac as f32 * (2.0f32).powi(-24);
            return if sign != 0 { -f } else { f };
        }
        (31, 0) => sign | 0x7F80_0000,
        (31, _) => sign | 0x7FC0_0000,
        _ => sign | ((exp + 112) << 23) | (frac << 13),
    };
    f32::from_bits(out)
}

fn specifier_name(code: i32) -> Result<&'static str> {
    // §16.3.10.27.
    Ok(match code {
        0 => "def",
        1 => "over",
        2 => "class",
        other => {
            return Err(invalid(format!(
                "USDC value: specifier code {other} is not def(0)/over(1)/class(2)"
            )))
        }
    })
}

fn permission_name(code: i32) -> Result<&'static str> {
    // §16.3.10.28.
    Ok(match code {
        0 => "public",
        1 => "private",
        other => {
            return Err(invalid(format!(
                "USDC value: permission code {other} is not public(0)/private(1)"
            )))
        }
    })
}

fn variability_name(code: i32) -> Result<&'static str> {
    // §16.3.10.29.
    Ok(match code {
        0 => "varying",
        1 => "uniform",
        other => {
            return Err(invalid(format!(
                "USDC value: variability code {other} is not varying(0)/uniform(1)"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_conversion_matches_ieee_754() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xBC00), -1.0);
        assert_eq!(half_to_f32(0x4000), 2.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        assert_eq!(half_to_f32(0x7C00), f32::INFINITY);
        assert_eq!(half_to_f32(0xFC00), f32::NEG_INFINITY);
        assert!(half_to_f32(0x7C01).is_nan());
        // Smallest denormal: 2^-24.
        assert_eq!(half_to_f32(0x0001), (2.0f32).powi(-24));
        // 65504 = largest finite half.
        assert_eq!(half_to_f32(0x7BFF), 65504.0);
    }

    #[test]
    fn diagonal_matrix_builds_identity() {
        let m = diagonal_matrix(&[1, 1, 1, 1]);
        let Value::Tuple(rows) = m else { panic!() };
        assert_eq!(rows.len(), 4);
        for (i, row) in rows.iter().enumerate() {
            let Value::Tuple(cols) = row else { panic!() };
            for (j, v) in cols.iter().enumerate() {
                let Value::Float(f) = v else { panic!() };
                assert_eq!(*f, if i == j { 1.0 } else { 0.0 });
            }
        }
    }

    #[test]
    fn enum_names_reject_out_of_range_codes() {
        assert_eq!(specifier_name(0).unwrap(), "def");
        assert_eq!(specifier_name(2).unwrap(), "class");
        assert!(specifier_name(3).is_err());
        assert_eq!(permission_name(1).unwrap(), "private");
        assert!(permission_name(2).is_err());
        assert_eq!(variability_name(1).unwrap(), "uniform");
        assert!(variability_name(-1).is_err());
    }
}
