//! Generic text writer — any [`Layer`] as `#usda 1.0` text (§16.2).
//!
//! [`crate::usda_writer`] serialises the typed scene; this module
//! serialises the *layer model* itself, statement for statement, so a
//! layer read from either file format can be written back as text
//! without going through the scene:
//!
//! * layer metadata (`subLayers` with their §16.2.18 layer offsets,
//!   `relocates`, dictionaries, the leading comment string) and
//!   `reorder rootPrims`;
//! * prim specs with their metadata block — list-edited arcs one
//!   statement per operator, reference / payload items with layer
//!   offsets and custom data, `variants`, `variantSets` — and
//!   `reorder nameChildren` / `reorder properties`;
//! * attribute specs with their default (a blocked default prints
//!   `= None`), `.timeSamples`, `.connect` (list-edited where
//!   authored) and `.spline` statements and their metadata;
//! * relationship specs, list-edited where authored;
//! * `variantSet` blocks, nested ones included.
//!
//! Numbers print the shortest digits that read back to the same
//! value at the attribute's precision, so text → Crate → text and
//! Crate → text → Crate are both fixed points.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::usda::{Attr, Layer, Prim, Value, Variant};

/// Serialise a layer as `#usda 1.0` text.
pub fn layer_to_usda(layer: &Layer) -> String {
    let mut w = Emitter {
        out: String::new(),
        depth: 0,
    };
    w.out.push_str("#usda 1.0\n");
    let block = w.metadata_lines(&layer.metadata, Site::Layer);
    if !block.is_empty() {
        w.out.push_str("(\n");
        w.depth = 1;
        for line in block {
            w.line(&line);
        }
        w.depth = 0;
        w.out.push_str(")\n");
    }
    if let Some(order) = layer.metadata.get("primOrder") {
        w.out.push('\n');
        let names = w.name_list(order);
        w.line(&format!("reorder rootPrims = {names}"));
    }
    for prim in &layer.prims {
        w.out.push('\n');
        w.prim(prim);
    }
    w.out
}

/// Prim fields that are list-edited (§16.2.17): a statement without
/// an operator authors the explicit sublist.
const LIST_EDITED: [&str; 6] = [
    "references",
    "payload",
    "inherits",
    "specializes",
    "apiSchemas",
    "variantSets",
];

/// List-edited fields whose grammar accepts a lone item in place of
/// a bracketed list (§16.2.17 `ReferenceList`, `PayloadList`,
/// `InheritsOrSpecializesList`, `NameList`); every other list
/// operation takes `None` or a list (`ListOpValue`).
const SINGLE_ITEM: [&str; 5] = [
    "references",
    "payload",
    "inherits",
    "specializes",
    "variantSets",
];

/// Which spec a metadata block belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    Layer,
    Prim,
    Property,
}

/// How a value's leaves print.
#[derive(Clone, Copy, Default)]
struct Shape {
    /// Single-precision (or half) element type: print `f32` digits.
    single: bool,
    /// Text leaves are asset paths (`@…@`).
    asset: bool,
    /// Text leaves are bare identifiers (`permission = private`).
    bare: bool,
}

impl Shape {
    /// The shape an attribute type token gives its value.
    fn of_type(type_token: &str) -> Self {
        let base = type_token
            .rsplit(' ')
            .next()
            .unwrap_or_default()
            .trim_end_matches("[]");
        let narrow = |suffix: char| {
            base.ends_with(suffix)
                && ["color", "point", "normal", "vector", "texCoord", "quat"]
                    .iter()
                    .any(|p| base.starts_with(p))
        };
        Shape {
            single: base.starts_with("float")
                || base.starts_with("half")
                || narrow('f')
                || narrow('h'),
            asset: base == "asset",
            bare: false,
        }
    }
}

struct Emitter {
    out: String,
    depth: usize,
}

impl Emitter {
    fn pad(&self) -> String {
        "    ".repeat(self.depth)
    }

    /// One statement line at the current depth. Continuation lines of
    /// a multi-line value are already indented relative to it.
    fn line(&mut self, text: &str) {
        let pad = self.pad();
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            if !part.is_empty() {
                self.out.push_str(&pad);
                self.out.push_str(part);
            }
        }
        self.out.push('\n');
    }

    // ---- metadata ------------------------------------------------------

    /// The statements of a `( … )` block (unindented; multi-line
    /// values carry their own relative indentation).
    fn metadata_lines(&self, map: &BTreeMap<String, Value>, site: Site) -> Vec<String> {
        let mut lines = Vec::new();
        // §16.2: the comment is a bare string, conventionally first.
        if let Some(text) = map.get("comment").and_then(Value::as_text) {
            lines.push(quoted(text));
        }
        for (key, value) in map {
            match key.as_str() {
                "comment" if value.as_text().is_some() => {}
                // Emitted as `reorder …` statements by the caller.
                "primOrder" | "propertyOrder" if site != Site::Property => {}
                // Folded into the `subLayers` items.
                "subLayerOffsets" if site == Site::Layer && map.contains_key("subLayers") => {}
                "subLayers" if site == Site::Layer => {
                    lines.push(format!(
                        "subLayers = {}",
                        sublayer_list(value, map.get("subLayerOffsets"))
                    ));
                }
                _ => {
                    let shape = Shape {
                        bare: matches!(
                            key.as_str(),
                            "permission" | "symmetryFunction" | "displayUnit"
                        ),
                        ..Shape::default()
                    };
                    match value {
                        Value::ListOp(op) => {
                            let single = SINGLE_ITEM.contains(&key.as_str());
                            for (edit, sub) in op.entries() {
                                let body = self.list_body(sub, shape, single);
                                lines.push(match edit.keyword() {
                                    Some(word) => format!("{word} {key} = {body}"),
                                    None => format!("{key} = {body}"),
                                });
                            }
                        }
                        // A bare statement of a list-edited field is
                        // its explicit sublist.
                        other if site == Site::Prim && LIST_EDITED.contains(&key.as_str()) => {
                            let single = SINGLE_ITEM.contains(&key.as_str());
                            lines.push(format!("{key} = {}", self.list_body(other, shape, single)))
                        }
                        other => lines.push(format!("{key} = {}", self.value(other, shape))),
                    }
                }
            }
        }
        lines
    }

    /// One sublist of a list-edited field: `None` when empty, a
    /// bracketed list otherwise — except that the fields whose
    /// grammar takes a lone item (`single`: the composition arcs,
    /// `variantSets`, relationship targets and connections) spell a
    /// one-item list as the bare item, whichever way the model holds
    /// it.
    fn list_body(&self, v: &Value, shape: Shape, single: bool) -> String {
        let lone;
        let items: &[Value] = match v {
            Value::None => &[],
            Value::Array(items) => items,
            other => {
                lone = [other.clone()];
                &lone
            }
        };
        {
            match items {
                [] => "None".to_owned(),
                [one] if single => self.value(one, shape),
                many => {
                    let mut s = String::from("[\n");
                    for item in many {
                        for part in self.value(item, shape).split('\n') {
                            s.push_str("    ");
                            s.push_str(part);
                            s.push('\n');
                        }
                        // The separator rides on the item's last line.
                        s.pop();
                        s.push_str(",\n");
                    }
                    s.push(']');
                    s
                }
            }
        }
    }

    /// `["a", "b"]` from a name-vector value.
    fn name_list(&self, v: &Value) -> String {
        let names: Vec<String> = match v {
            Value::Array(items) => items
                .iter()
                .filter_map(|x| x.as_text().map(quoted))
                .collect(),
            other => other.as_text().map(quoted).into_iter().collect(),
        };
        format!("[{}]", names.join(", "))
    }

    // ---- prims -----------------------------------------------------------

    fn prim(&mut self, prim: &Prim) {
        let mut head = prim.spec.clone();
        if !prim.type_name.is_empty() {
            head.push(' ');
            head.push_str(&prim.type_name);
        }
        head.push(' ');
        head.push_str(&quoted(&prim.name));
        let block = self.metadata_lines(&prim.metadata, Site::Prim);
        if block.is_empty() {
            self.line(&head);
        } else {
            self.line(&format!("{head} ("));
            self.depth += 1;
            for line in block {
                self.line(&line);
            }
            self.depth -= 1;
            self.line(")");
        }
        self.line("{");
        self.depth += 1;
        self.body(
            &prim.metadata,
            &prim.attrs,
            &prim.children,
            &prim.variant_sets,
        );
        self.depth -= 1;
        self.line("}");
    }

    /// The contents of a prim or variant body.
    fn body(
        &mut self,
        metadata: &BTreeMap<String, Value>,
        attrs: &BTreeMap<String, Attr>,
        children: &[Prim],
        variant_sets: &BTreeMap<String, BTreeMap<String, Variant>>,
    ) {
        if let Some(order) = metadata.get("primOrder") {
            let names = self.name_list(order);
            self.line(&format!("reorder nameChildren = {names}"));
        }
        if let Some(order) = metadata.get("propertyOrder") {
            let names = self.name_list(order);
            self.line(&format!("reorder properties = {names}"));
        }
        for (name, attr) in attrs {
            // A declaration that says nothing beyond its companion
            // statements (`.connect`, `.timeSamples`, `.spline`) is
            // implied by them.
            if matches!(attr.value, Value::None)
                && attr.metadata.is_empty()
                && !name.contains('.')
                && ["connect", "timeSamples", "spline"]
                    .iter()
                    .any(|c| attrs.contains_key(&format!("{name}.{c}")))
            {
                continue;
            }
            self.property(name, attr);
        }
        for child in children {
            self.out.push('\n');
            self.prim(child);
        }
        for (set, variants) in variant_sets {
            self.line(&format!("variantSet {} = {{", quoted(set)));
            self.depth += 1;
            for (name, variant) in variants {
                let block = self.metadata_lines(&variant.metadata, Site::Prim);
                if block.is_empty() {
                    self.line(&format!("{} {{", quoted(name)));
                } else {
                    self.line(&format!("{} (", quoted(name)));
                    self.depth += 1;
                    for line in block {
                        self.line(&line);
                    }
                    self.depth -= 1;
                    self.line(") {");
                }
                self.depth += 1;
                self.body(
                    &variant.metadata,
                    &variant.attrs,
                    &variant.children,
                    &variant.variant_sets,
                );
                self.depth -= 1;
                self.line("}");
            }
            self.depth -= 1;
            self.line("}");
        }
    }

    // ---- properties ----------------------------------------------------

    fn property(&mut self, name: &str, attr: &Attr) {
        let type_token = attr.type_token.as_str();
        let is_rel = type_token.rsplit(' ').next() == Some("rel");
        let companion = name.rsplit_once('.').map(|(_, c)| c);
        let shape = Shape::of_type(type_token);
        let block = self.metadata_lines(&attr.metadata, Site::Property);
        let tail = if block.is_empty() {
            String::new()
        } else {
            let pad = "    ";
            let mut s = String::from(" (\n");
            for line in &block {
                for part in line.split('\n') {
                    s.push_str(pad);
                    s.push_str(part);
                    s.push('\n');
                }
            }
            s.push(')');
            s
        };
        let decl = if type_token.is_empty() {
            name.to_owned()
        } else {
            format!("{type_token} {name}")
        };
        // Relationship targets and connections are list-edited: one
        // statement per authored operator.
        if is_rel || companion == Some("connect") {
            match &attr.value {
                Value::ListOp(op) => {
                    let mut first = true;
                    for (edit, sub) in op.entries() {
                        let body = self.path_list(sub);
                        let stmt = match edit.keyword() {
                            Some(word) => format!("{word} {decl} = {body}"),
                            None => format!("{decl} = {body}"),
                        };
                        // The metadata block rides on the first statement.
                        if first {
                            self.line(&format!("{stmt}{tail}"));
                        } else {
                            self.line(&stmt);
                        }
                        first = false;
                    }
                    if first {
                        self.line(&format!("{decl}{tail}"));
                    }
                }
                Value::None => self.line(&format!("{decl}{tail}")),
                Value::Block => self.line(&format!("{decl} = None{tail}")),
                other => {
                    let body = self.path_list(other);
                    self.line(&format!("{decl} = {body}{tail}"));
                }
            }
            return;
        }
        match &attr.value {
            // No default authored: the bare declaration.
            Value::None => self.line(&format!("{decl}{tail}")),
            value => {
                let body = self.value(value, shape);
                self.line(&format!("{decl} = {body}{tail}"));
            }
        }
    }

    /// A target list, spelled like any other list-edited sublist.
    fn path_list(&self, v: &Value) -> String {
        self.list_body(v, Shape::default(), true)
    }

    // ---- values ----------------------------------------------------------

    fn value(&self, v: &Value, shape: Shape) -> String {
        let mut out = String::new();
        self.write_value(&mut out, v, shape);
        out
    }

    /// Append a value's spelling to `out`. Numbers, tuples and arrays
    /// — the bulk of any geometry layer — go straight into the
    /// buffer; the structured kinds are composed and appended.
    fn write_value(&self, out: &mut String, v: &Value, shape: Shape) {
        match v {
            Value::Float(f) => push_number(out, *f, shape.single),
            Value::Tuple(items) => {
                out.push('(');
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write_value(out, x, shape);
                }
                out.push(')');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write_value(out, x, shape);
                }
                out.push(']');
            }
            Value::TimeSamples(samples) => {
                out.push_str("{\n");
                for (time, value) in samples {
                    out.push_str("    ");
                    push_number(out, *time, false);
                    out.push_str(": ");
                    self.write_value(out, value, shape);
                    out.push_str(",\n");
                }
                out.push('}');
            }
            other => out.push_str(&self.structured(other, shape)),
        }
    }

    /// The value kinds that are not plain number runs.
    fn structured(&self, v: &Value, shape: Shape) -> String {
        match v {
            Value::Token(s) | Value::String(s) => {
                if shape.bare {
                    s.clone()
                } else if shape.asset {
                    asset(s)
                } else {
                    quoted(s)
                }
            }
            Value::Raw(s) => s.clone(),
            Value::Float(f) => number(*f, shape.single),
            Value::Integer(n) => n.to_string(),
            Value::Unsigned(n) => n.to_string(),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
            Value::Tuple(_) | Value::Array(_) | Value::TimeSamples(_) => self.value(v, shape),
            Value::Asset(s) => asset(s),
            Value::Path(p) => format!("<{p}>"),
            Value::AssetWithPath {
                asset: a,
                prim_path,
            } => arc_item(a, prim_path),
            Value::Reference(r) => {
                let mut s = arc_item(&r.asset, &r.prim_path);
                let mut params: Vec<String> = Vec::new();
                if r.offset != 0.0 {
                    params.push(format!("offset = {}", number(r.offset, false)));
                }
                if r.scale != 1.0 {
                    params.push(format!("scale = {}", number(r.scale, false)));
                }
                if !r.custom_data.is_empty() {
                    params.push(format!("customData = {}", self.dictionary(&r.custom_data)));
                }
                if !params.is_empty() {
                    s.push_str(" (\n");
                    for p in params {
                        for part in p.split('\n') {
                            s.push_str("    ");
                            s.push_str(part);
                            s.push('\n');
                        }
                    }
                    s.push(')');
                }
                s
            }
            Value::Dict(map) => self.dictionary(map),
            Value::ListOp(op) => {
                // A list op nested in a value position has no
                // operator syntax; its strongest sublist stands in.
                match op.entries().next() {
                    Some((_, sub)) => self.value(sub, shape),
                    None => "None".to_owned(),
                }
            }
            Value::Relocates(pairs) => {
                let mut s = String::from("{\n");
                for (from, to) in pairs {
                    let _ = writeln!(s, "    <{from}> : <{to}>,");
                }
                s.push('}');
                s
            }
            Value::Spline(spline) => spline.to_usda(),
            Value::Typed(t) => self.value(&t.value, Shape::of_type(&t.type_name)),
            Value::None | Value::Block => "None".to_owned(),
        }
    }

    /// `{ type name = value … }`, one member per line. The member
    /// type is the one its value's shape encodes as in the Crate form.
    fn dictionary(&self, map: &BTreeMap<String, Value>) -> String {
        if map.is_empty() {
            return "{}".to_owned();
        }
        let mut s = String::from("{\n");
        for (key, value) in map {
            let name = if is_identifier(key) {
                key.clone()
            } else {
                quoted(key)
            };
            let (type_name, value) = match value {
                Value::Typed(t) => (t.type_name.clone(), &t.value),
                other => (
                    crate::usda::shape_type_name(other).unwrap_or_else(|| "string".to_owned()),
                    other,
                ),
            };
            let body = self.value(value, Shape::of_type(&type_name));
            for (i, part) in format!("{type_name} {name} = {body}")
                .split('\n')
                .enumerate()
            {
                if i > 0 || !part.is_empty() {
                    s.push_str("    ");
                    s.push_str(part);
                    s.push('\n');
                }
            }
        }
        s.push('}');
        s
    }
}

/// A value in its metadata-position spelling (no element type to
/// direct it) — what the scene writer prints for the value kinds it
/// does not spell itself.
pub(crate) fn metadata_value_text(v: &Value) -> String {
    Emitter {
        out: String::new(),
        depth: 0,
    }
    .value(v, Shape::default())
}

/// `subLayers = [ @a@ (offset = 1; scale = 2), … ]`.
fn sublayer_list(layers: &Value, offsets: Option<&Value>) -> String {
    let items: Vec<&Value> = match layers {
        Value::Array(items) => items.iter().collect(),
        Value::ListOp(op) => op
            .additive_in_strength_order()
            .flat_map(|(_, v)| match v {
                Value::Array(items) => items.iter().collect::<Vec<_>>(),
                other => vec![other],
            })
            .collect(),
        other => vec![other],
    };
    let offsets: &[Value] = match offsets {
        Some(Value::Array(items)) => items,
        _ => &[],
    };
    let mut s = String::from("[\n");
    for (i, item) in items.iter().enumerate() {
        s.push_str("    ");
        s.push_str(&asset(item.as_text().unwrap_or_default()));
        if let Some(pair) = offsets.get(i).and_then(Value::as_seq)
            && let [Value::Float(offset), Value::Float(scale)] = pair
        {
            let mut params = Vec::new();
            if *offset != 0.0 {
                params.push(format!("offset = {}", number(*offset, false)));
            }
            if *scale != 1.0 {
                params.push(format!("scale = {}", number(*scale, false)));
            }
            if !params.is_empty() {
                let _ = write!(s, " ({})", params.join("; "));
            }
        }
        s.push_str(",\n");
    }
    s.push(']');
    s
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// `@asset@</prim>` in whichever of its three spellings applies.
fn arc_item(asset_path: &str, prim_path: &str) -> String {
    match (asset_path.is_empty(), prim_path.is_empty()) {
        (true, _) => format!("<{prim_path}>"),
        (false, true) => asset(asset_path),
        (false, false) => format!("{}<{prim_path}>", asset(asset_path)),
    }
}

/// §16.2.8 asset reference: `@…@`, or `@@@…@@@` when the path itself
/// holds an `@`.
fn asset(path: &str) -> String {
    if path.contains('@') {
        format!("@@@{path}@@@")
    } else {
        format!("@{path}@")
    }
}

/// §16.2.5 double-quoted string with the `Escaped` production's set.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
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
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// §16.2.5 Number: the shortest digits that read back to `f` — at
/// single precision when the element type is and `f` is exactly a
/// single-precision value.
fn number(f: f64, single: bool) -> String {
    let mut out = String::new();
    push_number(&mut out, f, single);
    out
}

/// `true` when an attribute type token names a single-precision (or
/// half) element type.
pub(crate) fn is_single_precision(type_token: &str) -> bool {
    Shape::of_type(type_token).single
}

/// Append [`number`]'s spelling to `out`.
pub(crate) fn push_number(out: &mut String, f: f64, single: bool) {
    if f.is_nan() {
        out.push_str("nan");
        return;
    }
    if f.is_infinite() {
        out.push_str(if f < 0.0 { "-inf" } else { "inf" });
        return;
    }
    if f == 0.0 {
        out.push_str(if f.is_sign_negative() { "-0" } else { "0" });
        return;
    }
    let magnitude = f.abs();
    if f == f.trunc() && magnitude < 1e16 {
        let _ = write!(out, "{}", f as i64);
        return;
    }
    let exponent_form = !(1e-5..1e16).contains(&magnitude);
    if single {
        let narrow = f as f32;
        if f64::from(narrow) == f {
            let start = out.len();
            let _ = if exponent_form {
                write!(out, "{narrow:e}")
            } else {
                write!(out, "{narrow}")
            };
            // The reader parses at double precision and narrows; keep
            // the short spelling only when that lands on the same bits.
            if out[start..]
                .parse::<f64>()
                .is_ok_and(|back| (back as f32).to_bits() == narrow.to_bits())
            {
                return;
            }
            out.truncate(start);
        }
    }
    let _ = if exponent_form {
        write!(out, "{f:e}")
    } else {
        write!(out, "{f}")
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_back_exactly() {
        for f in [
            0.1f64,
            -0.0,
            1e300,
            1.5e-9,
            123456789.125,
            f64::from(0.1f32),
            f64::from(f32::MAX),
            f64::from(f32::MIN_POSITIVE),
            9_007_199_254_740_993.0,
        ] {
            for single in [false, true] {
                let text = number(f, single);
                let back: f64 = text.parse().unwrap();
                if single {
                    assert_eq!((back as f32).to_bits(), (f as f32).to_bits(), "{text}");
                } else {
                    assert_eq!(back.to_bits(), f.to_bits(), "{text}");
                }
            }
        }
        assert_eq!(number(f64::from(0.1f32), true), "0.1");
        assert_eq!(number(f64::from(0.1f32), false), "0.10000000149011612");
        assert_eq!(number(-0.0, false), "-0");
        assert_eq!(number(1e300, false), "1e300");
    }

    #[test]
    fn quoting_and_assets() {
        assert_eq!(quoted("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(asset("./a.usd"), "@./a.usd@");
        assert_eq!(asset("a@b"), "@@@a@b@@@");
        assert_eq!(arc_item("", "/P"), "</P>");
        assert_eq!(arc_item("a.usd", ""), "@a.usd@");
        assert_eq!(arc_item("a.usd", "/P"), "@a.usd@</P>");
    }
}
