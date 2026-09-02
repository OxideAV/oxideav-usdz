//! Cubic splines — the Core Specification's first-class animation
//! curve value (§7.4.2.4), in its text form (§16.2.13 grammar, the
//! `<attr>.spline = { … }` statement of §16.2.16.5) and its Crate
//! form (§16.3.10.33, `Splines`, Crate ≥ 0.12.0).
//!
//! A [`Spline`] is a curve type (bezier / hermite), pre- and
//! post-extrapolation modes, optional loop parameters, and a knot
//! list — each knot a time, a value (optionally dual-valued with a
//! pre-value), pre / post tangents, the interpolation mode of the
//! segment that follows it, and optional custom data.
//!
//! Two encodings, one model: the text parser (in [`crate::usda`])
//! and the Crate decoder ([`Spline::from_crate_bytes`]) both produce
//! this type; [`Spline::to_usda`] renders the text literal. The Crate
//! encoding's enumerations are pinned by §16.3.10.33 except the
//! per-knot *interpolation mode* codes, which the specification lists
//! as a 2-bit field without a value table — this crate maps them in
//! the §7.4.2.4.2 enumeration order (`none` 0, `held` 1, `linear` 2,
//! `curve` 3), the same order the neighbouring extrapolation table
//! pins for its first three names, and records the raw code alongside
//! so nothing is lost if that inference is ever corrected. Likewise
//! the text tangent pair `(a, b)` is taken as `(width, slope)` — the
//! order the §16.3.10.33.1 knot table uses — with hermite knots
//! carrying a bare `(slope)`.

use std::collections::BTreeMap;

use serde_json::{Map as JMap, Value as JValue};

use crate::error::{invalid, unsupported};
use crate::usda::Value;
use crate::usdc_values::half_to_f32;
use crate::Result;

/// §7.4.2.4.1 curve type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CurveType {
    /// Bezier — knots carry tangent widths.
    #[default]
    Bezier,
    /// Hermite — tangents are slopes only.
    Hermite,
}

impl CurveType {
    /// Text keyword (§16.2.13 `SplineCurveTypeItem`).
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Bezier => "bezier",
            Self::Hermite => "hermite",
        }
    }
}

/// §7.4.2.4.2 segment interpolation mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InterpolationMode {
    /// No value across the segment (a value block).
    None,
    /// Constant value across the segment (the §7.4.2.4.3 default).
    #[default]
    Held,
    /// Linear interpolation.
    Linear,
    /// Bezier / hermite interpolation per the spline's curve type.
    Curve,
}

impl InterpolationMode {
    /// Text keyword (§16.2.13 `SplineInterpMode`).
    pub fn keyword(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Held => "held",
            Self::Linear => "linear",
            Self::Curve => "curve",
        }
    }

    /// Crate 2-bit code, in §7.4.2.4.2 enumeration order (see the
    /// module docs on this inference).
    pub fn from_code(code: u8) -> Self {
        match code & 3 {
            0 => Self::None,
            1 => Self::Held,
            2 => Self::Linear,
            _ => Self::Curve,
        }
    }

    fn from_keyword(word: &str) -> Option<Self> {
        Some(match word {
            "none" => Self::None,
            "held" => Self::Held,
            "linear" => Self::Linear,
            "curve" => Self::Curve,
            _ => return None,
        })
    }
}

/// §7.4.2.4.4 extrapolation mode.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ExtrapolationMode {
    /// No value outside the knots (§16.3.10.33 "Block").
    None,
    /// Constant edge value.
    #[default]
    Held,
    /// Linear from the edge knots.
    Linear,
    /// Linear with the given slope.
    Sloped(f64),
    /// Knot curve repeated, offset so ends meet.
    LoopRepeat,
    /// Curve repeated exactly.
    LoopReset,
    /// Every other copy reversed.
    LoopOscillate,
}

impl ExtrapolationMode {
    /// §16.3.10.33 3-bit code (`Sloped` needs its trailing slope).
    fn from_code(code: u8, slope: Option<f64>) -> Result<Self> {
        Ok(match code & 7 {
            0 => Self::None,
            1 => Self::Held,
            2 => Self::Linear,
            3 => Self::Sloped(slope.unwrap_or(0.0)),
            4 => Self::LoopRepeat,
            5 => Self::LoopReset,
            6 => Self::LoopOscillate,
            other => {
                return Err(invalid(format!(
                "USDC Splines: extrapolation code {other} is unassigned (§16.3.10.33 defines 0–6)"
            )))
            }
        })
    }

    /// §16.2.13 `SplineExtrapolation` text.
    pub fn to_usda(self) -> String {
        match self {
            Self::None => "none".into(),
            Self::Held => "held".into(),
            Self::Linear => "linear".into(),
            Self::Sloped(s) => format!("sloped({})", fmt_f(s)),
            Self::LoopRepeat => "loop repeat".into(),
            Self::LoopReset => "loop reset".into(),
            Self::LoopOscillate => "loop oscillate".into(),
        }
    }
}

/// §16.2.13 `SplineLoopItem` / §16.3.10.33 loop parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoopParams {
    /// Prototype interval start (timeCode).
    pub proto_start: f64,
    /// Prototype interval end (timeCode).
    pub proto_end: f64,
    /// Number of pre-loop intervals.
    pub pre_loops: u32,
    /// Number of post-loop intervals.
    pub post_loops: u32,
    /// Value offset per iteration.
    pub value_offset: f64,
}

/// A knot tangent: slope, plus the time width for bezier knots.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tangent {
    /// Time width (bezier only; `None` for hermite knots).
    pub width: Option<f64>,
    /// Slope.
    pub slope: f64,
}

/// §7.4.2.4.3 spline knot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Knot {
    /// Knot time (timeCode).
    pub time: f64,
    /// Value at the knot (the post-value when dual-valued).
    pub value: f64,
    /// Pre-value of a dual-valued knot.
    pub pre_value: Option<f64>,
    /// Pre-tangent.
    pub pre_tangent: Option<Tangent>,
    /// Post-tangent.
    pub post_tangent: Option<Tangent>,
    /// Interpolation of the segment following this knot.
    pub post_interpolation: InterpolationMode,
    /// The Crate flag byte's raw 2-bit interpolation code (see the
    /// module docs); `None` for text-parsed knots.
    pub interpolation_code: Option<u8>,
    /// Crate per-knot curve-type bit (`true` = hermite).
    pub hermite: bool,
    /// Crate "pre tangent is in Maya form" flag (carried verbatim).
    pub pre_tangent_maya_form: bool,
    /// Crate "post tangent is in Maya form" flag (carried verbatim).
    pub post_tangent_maya_form: bool,
    /// Custom data dictionary.
    pub custom_data: BTreeMap<String, Value>,
}

/// §16.3.10.33 spline data type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DataType {
    /// Unspecified (treated as double; carries no knots in Crate).
    #[default]
    Unspecified,
    /// 64-bit values.
    Double,
    /// 32-bit values.
    Float,
    /// 16-bit values.
    Half,
}

impl DataType {
    fn width(self) -> usize {
        match self {
            Self::Unspecified | Self::Double => 8,
            Self::Float => 4,
            Self::Half => 2,
        }
    }

    /// From an attribute's type token (`double` / `float` / `half`).
    pub fn from_type_token(token: &str) -> Self {
        match token.rsplit(' ').next().unwrap_or(token) {
            "double" => Self::Double,
            "float" => Self::Float,
            "half" => Self::Half,
            _ => Self::Unspecified,
        }
    }
}

/// The spline value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spline {
    /// Curve type.
    pub curve_type: CurveType,
    /// Value data type.
    pub data_type: DataType,
    /// Crate "timed value" flag (carried verbatim).
    pub timed_value: bool,
    /// Pre-extrapolation.
    pub pre: ExtrapolationMode,
    /// Post-extrapolation.
    pub post: ExtrapolationMode,
    /// Loop parameters when looping.
    pub loop_params: Option<LoopParams>,
    /// Knots in authored order.
    pub knots: Vec<Knot>,
    /// Crate custom-data entries whose time matched no knot.
    pub extra_custom_data: Vec<(f64, BTreeMap<String, Value>)>,
}

impl Spline {
    /// Decode the §16.3.10.33 spline-data byte run (the bytes after
    /// the leading byte count). `custom` pairs each Crate custom-data
    /// dictionary with its timeCode; matched by time onto knots.
    pub fn from_crate_bytes(
        data: &[u8],
        custom: Vec<(f64, BTreeMap<String, Value>)>,
    ) -> Result<Self> {
        let mut r = Reader { data, pos: 0 };
        let f0 = r.u8()?;
        let version = f0 & 0x0F;
        if version != 1 {
            return Err(unsupported(format!(
                "USDC Splines: format version {version} (§16.3.10.33 documents version 1)"
            )));
        }
        let data_type = match (f0 & 0x30) >> 4 {
            0 => DataType::Unspecified,
            1 => DataType::Double,
            2 => DataType::Float,
            _ => DataType::Half,
        };
        let timed_value = f0 & 0x40 != 0;
        let curve_type = if f0 & 0x80 != 0 {
            CurveType::Hermite
        } else {
            CurveType::Bezier
        };
        let f1 = r.u8()?;
        let pre_code = f1 & 0x07;
        let post_code = (f1 & 0x18) >> 3;
        let looping = f1 & 0x40 != 0;
        let pre_slope = if pre_code == 3 { Some(r.f64()?) } else { None };
        let post_slope = if post_code == 3 { Some(r.f64()?) } else { None };
        let pre = ExtrapolationMode::from_code(pre_code, pre_slope)?;
        let post = ExtrapolationMode::from_code(post_code, post_slope)?;
        let loop_params = if looping {
            Some(LoopParams {
                proto_start: r.f64()?,
                proto_end: r.f64()?,
                pre_loops: r.u32()?,
                post_loops: r.u32()?,
                value_offset: r.f64()?,
            })
        } else {
            None
        };
        let mut knots = Vec::new();
        if data_type != DataType::Unspecified {
            let count = r.u32()? as usize;
            if count > data.len() {
                return Err(invalid(format!(
                    "USDC Splines: {count} knots cannot fit in {} bytes",
                    data.len()
                )));
            }
            for _ in 0..count {
                let flags = r.u8()?;
                let dual = flags & 0x01 != 0;
                let interp_code = (flags & 0x06) >> 1;
                let hermite = flags & 0x08 != 0;
                let pre_maya = flags & 0x10 != 0;
                let post_maya = flags & 0x20 != 0;
                let time = r.f64()?;
                let value = r.value(data_type)?;
                let pre_value = if dual {
                    Some(r.value(data_type)?)
                } else {
                    None
                };
                let (pre_width, post_width) = if hermite {
                    (None, None)
                } else {
                    (Some(r.f64()?), Some(r.f64()?))
                };
                let pre_slope = r.value(data_type)?;
                let post_slope = r.value(data_type)?;
                knots.push(Knot {
                    time,
                    value,
                    pre_value,
                    pre_tangent: Some(Tangent {
                        width: pre_width,
                        slope: pre_slope,
                    }),
                    post_tangent: Some(Tangent {
                        width: post_width,
                        slope: post_slope,
                    }),
                    post_interpolation: InterpolationMode::from_code(interp_code),
                    interpolation_code: Some(interp_code),
                    hermite,
                    pre_tangent_maya_form: pre_maya,
                    post_tangent_maya_form: post_maya,
                    custom_data: BTreeMap::new(),
                });
            }
        }
        if r.pos != data.len() {
            return Err(invalid(format!(
                "USDC Splines: {} trailing bytes after the knot data",
                data.len() - r.pos
            )));
        }
        let mut extra = Vec::new();
        for (time, dict) in custom {
            match knots.iter_mut().find(|k| k.time == time) {
                Some(k) => k.custom_data = dict,
                None => extra.push((time, dict)),
            }
        }
        Ok(Self {
            curve_type,
            data_type,
            timed_value,
            pre,
            post,
            loop_params,
            knots,
            extra_custom_data: extra,
        })
    }

    /// Render the §16.2.13 `SplineMap` literal.
    pub fn to_usda(&self) -> String {
        let mut items: Vec<String> = Vec::new();
        items.push(self.curve_type.keyword().to_owned());
        items.push(format!("pre: {}", self.pre.to_usda()));
        items.push(format!("post: {}", self.post.to_usda()));
        if let Some(l) = &self.loop_params {
            items.push(format!(
                "loop: ({}, {}, {}, {}, {})",
                fmt_f(l.proto_start),
                fmt_f(l.proto_end),
                l.pre_loops,
                l.post_loops,
                fmt_f(l.value_offset)
            ));
        }
        for k in &self.knots {
            let mut s = format!("{}: ", fmt_f(k.time));
            if let Some(pre) = k.pre_value {
                s.push_str(&format!("{} & ", fmt_f(pre)));
            }
            s.push_str(&fmt_f(k.value));
            if let Some(t) = &k.pre_tangent {
                s.push_str(&format!("; pre {}", fmt_tangent(t)));
            }
            s.push_str(&format!("; post {}", k.post_interpolation.keyword()));
            if let Some(t) = &k.post_tangent {
                s.push(' ');
                s.push_str(&fmt_tangent(t));
            }
            if !k.custom_data.is_empty() {
                s.push_str("; ");
                s.push_str(&crate::usda_writer::format_metadata_value(&Value::Dict(
                    k.custom_data.clone(),
                )));
            }
            items.push(s);
        }
        format!("{{ {} }}", items.join(", "))
    }

    /// JSON for the extras / variant codec.
    pub fn to_json(&self) -> JValue {
        let mut o = JMap::new();
        o.insert(
            "curveType".into(),
            JValue::String(self.curve_type.keyword().into()),
        );
        o.insert(
            "dataType".into(),
            JValue::String(
                match self.data_type {
                    DataType::Unspecified => "unspecified",
                    DataType::Double => "double",
                    DataType::Float => "float",
                    DataType::Half => "half",
                }
                .into(),
            ),
        );
        o.insert("timedValue".into(), JValue::Bool(self.timed_value));
        o.insert("pre".into(), extrap_to_json(self.pre));
        o.insert("post".into(), extrap_to_json(self.post));
        if let Some(l) = &self.loop_params {
            o.insert(
                "loop".into(),
                JValue::Array(vec![
                    JValue::from(l.proto_start),
                    JValue::from(l.proto_end),
                    JValue::from(l.pre_loops),
                    JValue::from(l.post_loops),
                    JValue::from(l.value_offset),
                ]),
            );
        }
        o.insert(
            "knots".into(),
            JValue::Array(self.knots.iter().map(knot_to_json).collect()),
        );
        if !self.extra_custom_data.is_empty() {
            o.insert(
                "extraCustomData".into(),
                JValue::Array(
                    self.extra_custom_data
                        .iter()
                        .map(|(t, d)| {
                            JValue::Array(vec![
                                JValue::from(*t),
                                crate::variant_codec::encode_btree_value(d),
                            ])
                        })
                        .collect(),
                ),
            );
        }
        JValue::Object(o)
    }

    /// Inverse of [`Self::to_json`].
    pub fn from_json(v: &JValue) -> Self {
        let Some(o) = v.as_object() else {
            return Self::default();
        };
        let get = |k: &str| o.get(k).unwrap_or(&JValue::Null);
        Self {
            curve_type: match get("curveType").as_str() {
                Some("hermite") => CurveType::Hermite,
                _ => CurveType::Bezier,
            },
            data_type: match get("dataType").as_str() {
                Some("double") => DataType::Double,
                Some("float") => DataType::Float,
                Some("half") => DataType::Half,
                _ => DataType::Unspecified,
            },
            timed_value: get("timedValue").as_bool().unwrap_or(false),
            pre: extrap_from_json(get("pre")),
            post: extrap_from_json(get("post")),
            loop_params: get("loop").as_array().and_then(|a| {
                Some(LoopParams {
                    proto_start: a.first()?.as_f64()?,
                    proto_end: a.get(1)?.as_f64()?,
                    pre_loops: a.get(2)?.as_u64()? as u32,
                    post_loops: a.get(3)?.as_u64()? as u32,
                    value_offset: a.get(4)?.as_f64()?,
                })
            }),
            knots: get("knots")
                .as_array()
                .map(|a| a.iter().map(knot_from_json).collect())
                .unwrap_or_default(),
            extra_custom_data: get("extraCustomData")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            let p = e.as_array()?;
                            Some((
                                p.first()?.as_f64()?,
                                crate::variant_codec::decode_btree_value(p.get(1)?),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

fn fmt_tangent(t: &Tangent) -> String {
    match t.width {
        Some(w) => format!("({}, {})", fmt_f(w), fmt_f(t.slope)),
        None => format!("({})", fmt_f(t.slope)),
    }
}

fn extrap_to_json(e: ExtrapolationMode) -> JValue {
    match e {
        ExtrapolationMode::Sloped(s) => {
            JValue::Array(vec![JValue::String("sloped".into()), JValue::from(s)])
        }
        other => JValue::String(other.to_usda()),
    }
}

fn extrap_from_json(v: &JValue) -> ExtrapolationMode {
    if let Some(a) = v.as_array() {
        return ExtrapolationMode::Sloped(a.get(1).and_then(|s| s.as_f64()).unwrap_or(0.0));
    }
    match v.as_str() {
        Some("none") => ExtrapolationMode::None,
        Some("linear") => ExtrapolationMode::Linear,
        Some("loop repeat") => ExtrapolationMode::LoopRepeat,
        Some("loop reset") => ExtrapolationMode::LoopReset,
        Some("loop oscillate") => ExtrapolationMode::LoopOscillate,
        _ => ExtrapolationMode::Held,
    }
}

fn tangent_to_json(t: &Tangent) -> JValue {
    let mut o = JMap::new();
    if let Some(w) = t.width {
        o.insert("width".into(), JValue::from(w));
    }
    o.insert("slope".into(), JValue::from(t.slope));
    JValue::Object(o)
}

fn tangent_from_json(v: &JValue) -> Option<Tangent> {
    let o = v.as_object()?;
    Some(Tangent {
        width: o.get("width").and_then(|w| w.as_f64()),
        slope: o.get("slope").and_then(|s| s.as_f64()).unwrap_or(0.0),
    })
}

fn knot_to_json(k: &Knot) -> JValue {
    let mut o = JMap::new();
    o.insert("time".into(), JValue::from(k.time));
    o.insert("value".into(), JValue::from(k.value));
    if let Some(p) = k.pre_value {
        o.insert("preValue".into(), JValue::from(p));
    }
    if let Some(t) = &k.pre_tangent {
        o.insert("pre".into(), tangent_to_json(t));
    }
    if let Some(t) = &k.post_tangent {
        o.insert("post".into(), tangent_to_json(t));
    }
    o.insert(
        "interp".into(),
        JValue::String(k.post_interpolation.keyword().into()),
    );
    if let Some(c) = k.interpolation_code {
        o.insert("interpCode".into(), JValue::from(c));
    }
    if k.hermite {
        o.insert("hermite".into(), JValue::Bool(true));
    }
    if k.pre_tangent_maya_form {
        o.insert("preMaya".into(), JValue::Bool(true));
    }
    if k.post_tangent_maya_form {
        o.insert("postMaya".into(), JValue::Bool(true));
    }
    if !k.custom_data.is_empty() {
        o.insert(
            "customData".into(),
            crate::variant_codec::encode_btree_value(&k.custom_data),
        );
    }
    JValue::Object(o)
}

fn knot_from_json(v: &JValue) -> Knot {
    let Some(o) = v.as_object() else {
        return Knot::default();
    };
    let get = |k: &str| o.get(k).unwrap_or(&JValue::Null);
    Knot {
        time: get("time").as_f64().unwrap_or(0.0),
        value: get("value").as_f64().unwrap_or(0.0),
        pre_value: get("preValue").as_f64(),
        pre_tangent: tangent_from_json(get("pre")),
        post_tangent: tangent_from_json(get("post")),
        post_interpolation: get("interp")
            .as_str()
            .and_then(InterpolationMode::from_keyword)
            .unwrap_or_default(),
        interpolation_code: get("interpCode").as_u64().map(|c| c as u8),
        hermite: get("hermite").as_bool().unwrap_or(false),
        pre_tangent_maya_form: get("preMaya").as_bool().unwrap_or(false),
        post_tangent_maya_form: get("postMaya").as_bool().unwrap_or(false),
        custom_data: o
            .get("customData")
            .map(crate::variant_codec::decode_btree_value)
            .unwrap_or_default(),
    }
}

/// §16.2.5 number spelling shared with the writers.
fn fmt_f(f: f64) -> String {
    crate::usda_writer::format_metadata_value(&Value::Float(f))
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| invalid("USDC Splines: offset overflow"))?;
        if end > self.data.len() {
            return Err(invalid(format!(
                "USDC Splines: truncated spline data ({n} bytes needed at {}, {} available)",
                self.pos,
                self.data.len()
            )));
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn value(&mut self, dt: DataType) -> Result<f64> {
        let bytes = self.take(dt.width())?;
        Ok(match dt {
            DataType::Unspecified | DataType::Double => {
                f64::from_le_bytes(bytes.try_into().unwrap())
            }
            DataType::Float => f64::from(f32::from_le_bytes(bytes.try_into().unwrap())),
            DataType::Half => f64::from(half_to_f32(u16::from_le_bytes(bytes.try_into().unwrap()))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-assemble a §16.3.10.33 spline-data run.
    fn bezier_double_bytes() -> Vec<u8> {
        let mut b = Vec::new();
        // version 1, data type double (1 << 4), not timed, bezier.
        b.push(0x01 | (1 << 4));
        // pre sloped (3), post held (1), looping (0x40).
        b.push(3 | (1 << 3) | 0x40);
        b.extend_from_slice(&0.5f64.to_le_bytes()); // pre slope
        b.extend_from_slice(&1.0f64.to_le_bytes()); // proto start
        b.extend_from_slice(&10.0f64.to_le_bytes()); // proto end
        b.extend_from_slice(&2u32.to_le_bytes()); // pre loops
        b.extend_from_slice(&3u32.to_le_bytes()); // post loops
        b.extend_from_slice(&0.25f64.to_le_bytes()); // value offset
        b.extend_from_slice(&2u32.to_le_bytes()); // knot count
                                                  // Knot 0: dual valued, interp curve (3 << 1), bezier.
        b.push(0x01 | (3 << 1));
        b.extend_from_slice(&1.0f64.to_le_bytes()); // time
        b.extend_from_slice(&0.0f64.to_le_bytes()); // value
        b.extend_from_slice(&(-1.0f64).to_le_bytes()); // pre value
        b.extend_from_slice(&1.0f64.to_le_bytes()); // pre width
        b.extend_from_slice(&1.5f64.to_le_bytes()); // post width
        b.extend_from_slice(&2.0f64.to_le_bytes()); // pre slope
        b.extend_from_slice(&2.5f64.to_le_bytes()); // post slope
                                                    // Knot 1: single valued, interp held (1 << 1), hermite (bit 3),
                                                    // post tangent in Maya form (0x20).
        b.push((1 << 1) | 0x08 | 0x20);
        b.extend_from_slice(&5.0f64.to_le_bytes());
        b.extend_from_slice(&3.25f64.to_le_bytes());
        b.extend_from_slice(&0.0f64.to_le_bytes()); // pre slope (no widths)
        b.extend_from_slice(&0.0f64.to_le_bytes()); // post slope
        b
    }

    #[test]
    fn decodes_bezier_double_with_loop_and_dual_knot() {
        let mut custom = BTreeMap::new();
        custom.insert("note".to_owned(), Value::String("hi".into()));
        let s =
            Spline::from_crate_bytes(&bezier_double_bytes(), vec![(5.0, custom.clone())]).unwrap();
        assert_eq!(s.curve_type, CurveType::Bezier);
        assert_eq!(s.data_type, DataType::Double);
        assert_eq!(s.pre, ExtrapolationMode::Sloped(0.5));
        assert_eq!(s.post, ExtrapolationMode::Held);
        assert_eq!(
            s.loop_params,
            Some(LoopParams {
                proto_start: 1.0,
                proto_end: 10.0,
                pre_loops: 2,
                post_loops: 3,
                value_offset: 0.25
            })
        );
        assert_eq!(s.knots.len(), 2);
        let k0 = &s.knots[0];
        assert_eq!((k0.time, k0.value, k0.pre_value), (1.0, 0.0, Some(-1.0)));
        assert_eq!(
            k0.pre_tangent,
            Some(Tangent {
                width: Some(1.0),
                slope: 2.0
            })
        );
        assert_eq!(
            k0.post_tangent,
            Some(Tangent {
                width: Some(1.5),
                slope: 2.5
            })
        );
        assert_eq!(k0.post_interpolation, InterpolationMode::Curve);
        let k1 = &s.knots[1];
        assert!(k1.hermite && k1.post_tangent_maya_form && !k1.pre_tangent_maya_form);
        assert_eq!(k1.pre_tangent.unwrap().width, None);
        assert_eq!(k1.post_interpolation, InterpolationMode::Held);
        assert_eq!(k1.custom_data, custom);
        assert!(s.extra_custom_data.is_empty());
        // JSON round trip.
        assert_eq!(Spline::from_json(&s.to_json()), s);
        // Text rendering re-parses to the same knots.
        let text = s.to_usda();
        assert!(text.starts_with("{ bezier, pre: sloped(0.5), post: held, loop: (1, 10, 2, 3, 0.25), 1: -1 & 0; pre (1, 2); post curve (1.5, 2.5), 5: 3.25; pre (0); post held (0); { string note = \"hi\" } }"), "{text}");
    }

    #[test]
    fn half_values_and_unspecified_type() {
        // Half data type: 0x3C00 = 1.0.
        let mut b = vec![0x01 | (3 << 4), 1 | (1 << 3)];
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(0); // single, interp none, bezier
        b.extend_from_slice(&2.0f64.to_le_bytes());
        b.extend_from_slice(&0x3C00u16.to_le_bytes()); // value 1.0
        b.extend_from_slice(&0.0f64.to_le_bytes());
        b.extend_from_slice(&0.0f64.to_le_bytes());
        b.extend_from_slice(&0x0000u16.to_le_bytes());
        b.extend_from_slice(&0x3C00u16.to_le_bytes());
        let s = Spline::from_crate_bytes(&b, Vec::new()).unwrap();
        assert_eq!(s.data_type, DataType::Half);
        assert_eq!(s.knots[0].value, 1.0);
        assert_eq!(s.knots[0].post_tangent.unwrap().slope, 1.0);
        assert_eq!(s.knots[0].post_interpolation, InterpolationMode::None);
        // Unspecified data type: no knot block follows.
        let s = Spline::from_crate_bytes(&[0x01, 1 | (1 << 3)], Vec::new()).unwrap();
        assert!(s.knots.is_empty());
        assert_eq!(s.data_type, DataType::Unspecified);
    }

    #[test]
    fn malformed_runs_are_errors() {
        // Unknown version.
        assert!(Spline::from_crate_bytes(&[0x02, 0], Vec::new()).is_err());
        // Truncated knot.
        let mut b = vec![0x01 | (1 << 4), 1 | (1 << 3)];
        b.extend_from_slice(&1u32.to_le_bytes());
        b.push(0);
        b.extend_from_slice(&2.0f64.to_le_bytes());
        assert!(Spline::from_crate_bytes(&b, Vec::new()).is_err());
        // Trailing bytes.
        let mut b = bezier_double_bytes();
        b.push(0);
        assert!(Spline::from_crate_bytes(&b, Vec::new()).is_err());
        // Unassigned extrapolation code 7.
        assert!(Spline::from_crate_bytes(&[0x01, 7], Vec::new()).is_err());
    }
}
