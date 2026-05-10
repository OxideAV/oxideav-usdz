//! ASCII USDA tokenizer + prim-tree parser.
//!
//! The grammar covered by round 1 is the subset used by USDZ
//! Default Layers in the wild: a `#usda 1.0` magic line, an
//! optional layer-level metadata block, then a forest of prim
//! definitions:
//!
//! ```text
//! def Xform "Root" {
//!     def Mesh "Body" (
//!         prepend apiSchemas = ["MaterialBindingAPI"]
//!     ) {
//!         int[] faceVertexCounts = [3, 3]
//!         point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
//!         rel material:binding = </Root/M>
//!     }
//! }
//! ```
//!
//! Each prim becomes a [`Prim`] holding `(spec, type_name, name,
//! metadata, attrs, children)`. Values are deliberately kept as
//! [`Value`] strings (or arrays of them) — `usd_to_scene.rs` does
//! the type coercion when it knows the schema.
//!
//! Out of scope for r1: variant sets, payloads, references that
//! pull in *external* layers, the rich relational expression
//! syntax. Anything we don't recognise is preserved verbatim as a
//! [`Value::Raw`] so a future encoder round-trip can reproduce it.

use std::collections::BTreeMap;

use crate::error::invalid;
use crate::Result;

/// USDA magic banner — every layer starts with `#usda <version>`.
pub const USDA_MAGIC: &[u8] = b"#usda";

/// One parsed USD prim.
///
/// Children appear in declaration order; metadata + attrs are
/// stored in `BTreeMap` for deterministic iteration in tests.
#[derive(Clone, Debug)]
pub struct Prim {
    /// `def`, `over`, or `class`. Round 1 only acts on `def`; the
    /// other two are preserved for round-trip but ignored by
    /// `usd_to_scene`.
    pub spec: String,
    /// Schema name — `Xform`, `Mesh`, `Scope`, `Material`,
    /// `Shader`, ... Empty when the prim has no schema (rare in
    /// USDZ assets).
    pub type_name: String,
    /// Quoted prim name (without the surrounding quotes).
    pub name: String,
    /// Optional `( ... )` metadata block contents — only parsed
    /// shallowly into name → value pairs.
    pub metadata: BTreeMap<String, Value>,
    /// Attribute and relationship statements inside the prim body.
    /// Keyed by full attribute name (`primvars:st`,
    /// `material:binding`, `inputs:diffuseColor.connect`, ...).
    pub attrs: BTreeMap<String, Attr>,
    /// Child prims, declaration-ordered.
    pub children: Vec<Prim>,
}

/// One attribute or relationship statement inside a prim body.
#[derive(Clone, Debug)]
pub struct Attr {
    /// Type token as written, e.g. `point3f[]`, `color3f`,
    /// `uniform token`, `rel`. Preserved verbatim so a writer
    /// round-trip keeps the original spelling.
    pub type_token: String,
    /// Right-hand side of `=` after the value/relationship parser.
    pub value: Value,
    /// Optional `( ... )` interpolation/displayName block trailing
    /// the value; preserved as a name → value map.
    pub metadata: BTreeMap<String, Value>,
}

/// USDA value variants relevant to the round-1 schema mappings.
#[derive(Clone, Debug)]
pub enum Value {
    /// Token / identifier without quotes (`"none"` ↔ raw `none`).
    Token(String),
    /// Quoted string.
    String(String),
    /// Decimal numeric scalar (we keep the parsed `f64` since USDA
    /// permits `1.0`, `1`, `1e-3` interchangeably).
    Float(f64),
    /// Boolean literal.
    Bool(bool),
    /// Vector / tuple `(x, y, z)` of any arity. Components are
    /// recursively parsed.
    Tuple(Vec<Value>),
    /// Array `[v, v, ...]`. Per USD any uniform sequence — ints,
    /// floats, vectors, strings — uses the same outer brackets.
    Array(Vec<Value>),
    /// Asset path `@uri@` or `@@@uri@@@`. The wrapping `@` markers
    /// are stripped; what's left is the raw inner reference, which
    /// for USDZ is a relative file path inside the archive.
    Asset(String),
    /// Prim/property path `</A/B/C.attr>`.
    Path(String),
    /// Anything we didn't recognise; preserved verbatim.
    Raw(String),
    /// Empty value — attribute declared but not assigned (`token
    /// outputs:surface`).
    None,
}

impl Value {
    /// Return the string form of a `Token` or `String` value.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Token(s) | Self::String(s) | Self::Asset(s) | Self::Path(s) | Self::Raw(s) => {
                Some(s.as_str())
            }
            _ => None,
        }
    }

    /// Coerce a numeric scalar to `f32`.
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::Float(f) => Some(*f as f32),
            _ => None,
        }
    }

    /// Borrow inner array elements (works for both `Array` and
    /// `Tuple`).
    pub fn as_seq(&self) -> Option<&[Value]> {
        match self {
            Self::Array(v) | Self::Tuple(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Extract `[f32; N]` from a `Tuple` of numerics. Returns
    /// `None` if any component isn't `Float`/`Token`-numeric or
    /// the arity doesn't match.
    pub fn as_floatn<const N: usize>(&self) -> Option<[f32; N]> {
        let seq = self.as_seq()?;
        if seq.len() != N {
            return None;
        }
        let mut out = [0f32; N];
        for (i, v) in seq.iter().enumerate() {
            out[i] = v.as_f32()?;
        }
        Some(out)
    }
}

/// Top-level parsed USDA layer.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Layer-level `( ... )` metadata block — `defaultPrim`,
    /// `upAxis`, `metersPerUnit`, custom keys.
    pub metadata: BTreeMap<String, Value>,
    /// Root-level prims, declaration-ordered.
    pub prims: Vec<Prim>,
}

/// Parse a USDA layer from its UTF-8 source bytes.
pub fn parse(source: &[u8]) -> Result<Layer> {
    let text =
        std::str::from_utf8(source).map_err(|_| invalid("USDA source is not valid UTF-8"))?;
    if !text.starts_with("#usda") {
        return Err(invalid("missing USDA magic banner (`#usda <version>`)"));
    }
    let mut tokens = Tokenizer::new(text);
    // Consume the `#usda <version>` line.
    tokens.eat_line();
    tokens.skip_trivia();

    let metadata = if tokens.peek_char() == Some('(') {
        tokens.advance();
        parse_metadata_block(&mut tokens)?
    } else {
        BTreeMap::new()
    };

    let mut prims = Vec::new();
    while !tokens.eof() {
        tokens.skip_trivia();
        if tokens.eof() {
            break;
        }
        prims.push(parse_prim(&mut tokens)?);
    }
    Ok(Layer { metadata, prims })
}

// --------------------------------------------------------------------
// Tokenizer + helpers
// --------------------------------------------------------------------

/// Position-tracked cursor over the source. We keep this
/// home-grown rather than pulling a parser combinator dep — the
/// grammar is small enough that a hand-rolled cursor is clearer
/// and keeps the dependency footprint at zero.
struct Tokenizer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek_char(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek_char()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume the rest of the current line (used for `#usda` and
    /// `#`-prefixed comments).
    fn eat_line(&mut self) {
        while let Some(c) = self.peek_char() {
            self.advance();
            if c == '\n' {
                break;
            }
        }
    }

    /// Skip whitespace (any Unicode whitespace) and `#`-comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek_char() {
                Some(c) if c.is_whitespace() => {
                    self.advance();
                }
                Some('#') => self.eat_line(),
                _ => break,
            }
        }
    }
}

/// Parse a `( name = value\n name = value )` block (used for both
/// layer metadata and the per-attribute trailing block).
fn parse_metadata_block(t: &mut Tokenizer<'_>) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    loop {
        t.skip_trivia();
        if t.eat(')') {
            return Ok(out);
        }
        if t.eof() {
            return Err(invalid("unexpected EOF inside `( ... )` metadata block"));
        }
        // Skip an optional `prepend` / `append` / `delete` /
        // `add` / `reorder` modifier. These are USD list-edit
        // operators and only matter to a composition engine; for
        // our shallow parse we ignore them.
        let saved = t.pos;
        if let Some(word) = peek_ident(t) {
            if matches!(
                word.as_str(),
                "prepend" | "append" | "delete" | "add" | "reorder"
            ) {
                let _ = read_ident(t)?;
                t.skip_trivia();
            } else {
                t.pos = saved;
            }
        }
        let name = read_attr_name(t)?;
        t.skip_trivia();
        if !t.eat('=') {
            // Some metadata is bare (e.g. `customData`) — tolerate
            // by storing `Value::None`.
            out.insert(name, Value::None);
            continue;
        }
        t.skip_trivia();
        let value = parse_value(t)?;
        out.insert(name, value);
    }
}

/// Try to read an identifier without committing.
fn peek_ident(t: &Tokenizer<'_>) -> Option<String> {
    let mut iter = t.rest().chars();
    let first = iter.next()?;
    if !is_ident_start(first) {
        return None;
    }
    let mut s = String::from(first);
    for c in iter {
        if is_ident_continue(c) {
            s.push(c);
        } else {
            break;
        }
    }
    Some(s)
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == ':'
}

/// Read a bare identifier (`def`, `Mesh`, `points`, ...). Errors
/// if the next non-trivia character isn't an identifier start.
fn read_ident(t: &mut Tokenizer<'_>) -> Result<String> {
    t.skip_trivia();
    let Some(first) = t.peek_char() else {
        return Err(invalid("expected identifier, found EOF"));
    };
    if !is_ident_start(first) {
        return Err(invalid(format!(
            "expected identifier, found `{first}` at offset {}",
            t.pos
        )));
    }
    let mut s = String::new();
    while let Some(c) = t.peek_char() {
        if is_ident_continue(c) {
            s.push(c);
            t.advance();
        } else {
            break;
        }
    }
    Ok(s)
}

/// Read an attribute name — like `read_ident` but tolerates `:`
/// and `.` so we can capture `primvars:st` and
/// `inputs:diffuseColor.connect`.
fn read_attr_name(t: &mut Tokenizer<'_>) -> Result<String> {
    t.skip_trivia();
    let mut s = String::new();
    while let Some(c) = t.peek_char() {
        if c.is_alphanumeric() || c == '_' || c == ':' || c == '.' {
            s.push(c);
            t.advance();
        } else {
            break;
        }
    }
    if s.is_empty() {
        return Err(invalid(format!(
            "expected attribute name at offset {}",
            t.pos
        )));
    }
    Ok(s)
}

/// Read a `"..."`-quoted string with backslash escapes. Triple
/// `"""..."""` strings are also handled (USD permits them).
fn read_quoted_string(t: &mut Tokenizer<'_>) -> Result<String> {
    let quote = t
        .peek_char()
        .filter(|c| *c == '"' || *c == '\'')
        .ok_or_else(|| invalid("expected `\"` or `'` to start a string"))?;
    t.advance();
    // Triple-quoted?
    let triple = t.rest().starts_with(quote) && t.rest()[1..].starts_with(quote);
    if triple {
        t.advance();
        t.advance();
        let mut s = String::new();
        loop {
            if t.eof() {
                return Err(invalid("EOF inside triple-quoted string"));
            }
            if t.rest().starts_with(quote)
                && t.rest()[1..].starts_with(quote)
                && t.rest()[2..].starts_with(quote)
            {
                t.advance();
                t.advance();
                t.advance();
                return Ok(s);
            }
            s.push(t.advance().unwrap());
        }
    }
    let mut s = String::new();
    while let Some(c) = t.peek_char() {
        if c == quote {
            t.advance();
            return Ok(s);
        }
        if c == '\\' {
            t.advance();
            match t.advance() {
                Some('n') => s.push('\n'),
                Some('t') => s.push('\t'),
                Some('r') => s.push('\r'),
                Some('"') => s.push('"'),
                Some('\'') => s.push('\''),
                Some('\\') => s.push('\\'),
                Some(c2) => s.push(c2),
                None => return Err(invalid("EOF after `\\` escape in string")),
            }
            continue;
        }
        s.push(c);
        t.advance();
    }
    Err(invalid("EOF inside quoted string"))
}

/// Read an `@asset_path@` or `@@@asset_path@@@` reference.
fn read_asset(t: &mut Tokenizer<'_>) -> Result<String> {
    if !t.eat('@') {
        return Err(invalid("expected `@` to start an asset path"));
    }
    let triple = t.rest().starts_with("@@");
    if triple {
        t.advance();
        t.advance();
        let mut s = String::new();
        loop {
            if t.eof() {
                return Err(invalid("EOF inside `@@@...@@@` asset path"));
            }
            if t.rest().starts_with("@@@") {
                t.advance();
                t.advance();
                t.advance();
                return Ok(s);
            }
            s.push(t.advance().unwrap());
        }
    }
    let mut s = String::new();
    while let Some(c) = t.peek_char() {
        if c == '@' {
            t.advance();
            return Ok(s);
        }
        if c == '\n' {
            return Err(invalid("newline inside single-`@` asset path"));
        }
        s.push(c);
        t.advance();
    }
    Err(invalid("EOF inside asset path"))
}

/// Read a `</prim/path.attr>` reference.
fn read_path(t: &mut Tokenizer<'_>) -> Result<String> {
    if !t.eat('<') {
        return Err(invalid("expected `<` to start a path"));
    }
    let mut s = String::new();
    while let Some(c) = t.peek_char() {
        if c == '>' {
            t.advance();
            return Ok(s);
        }
        s.push(c);
        t.advance();
    }
    Err(invalid("EOF inside `<...>` path"))
}

/// Read a `(a, b, c)` tuple — vector / colour / matrix row.
fn read_tuple(t: &mut Tokenizer<'_>) -> Result<Vec<Value>> {
    if !t.eat('(') {
        return Err(invalid("expected `(`"));
    }
    let mut out = Vec::new();
    loop {
        t.skip_trivia();
        if t.eat(')') {
            return Ok(out);
        }
        out.push(parse_value(t)?);
        t.skip_trivia();
        if t.eat(',') {
            continue;
        }
        if t.eat(')') {
            return Ok(out);
        }
        return Err(invalid(format!(
            "expected `,` or `)` in tuple at offset {}",
            t.pos
        )));
    }
}

/// Read a `[v1, v2, ...]` array.
fn read_array(t: &mut Tokenizer<'_>) -> Result<Vec<Value>> {
    if !t.eat('[') {
        return Err(invalid("expected `[`"));
    }
    let mut out = Vec::new();
    loop {
        t.skip_trivia();
        if t.eat(']') {
            return Ok(out);
        }
        out.push(parse_value(t)?);
        t.skip_trivia();
        if t.eat(',') {
            continue;
        }
        if t.eat(']') {
            return Ok(out);
        }
        return Err(invalid(format!(
            "expected `,` or `]` in array at offset {}",
            t.pos
        )));
    }
}

/// Parse a numeric literal — int or float, optional sign + exponent.
fn read_number(t: &mut Tokenizer<'_>) -> Result<Value> {
    let mut s = String::new();
    if matches!(t.peek_char(), Some('+' | '-')) {
        s.push(t.advance().unwrap());
    }
    let mut saw_digit = false;
    while let Some(c) = t.peek_char() {
        if c.is_ascii_digit() {
            s.push(c);
            t.advance();
            saw_digit = true;
        } else {
            break;
        }
    }
    if t.peek_char() == Some('.') {
        s.push('.');
        t.advance();
        while let Some(c) = t.peek_char() {
            if c.is_ascii_digit() {
                s.push(c);
                t.advance();
                saw_digit = true;
            } else {
                break;
            }
        }
    }
    if matches!(t.peek_char(), Some('e' | 'E')) {
        s.push(t.advance().unwrap());
        if matches!(t.peek_char(), Some('+' | '-')) {
            s.push(t.advance().unwrap());
        }
        while let Some(c) = t.peek_char() {
            if c.is_ascii_digit() {
                s.push(c);
                t.advance();
            } else {
                break;
            }
        }
    }
    if !saw_digit {
        return Err(invalid(format!(
            "expected numeric literal at offset {}",
            t.pos
        )));
    }
    let f: f64 = s
        .parse()
        .map_err(|_| invalid(format!("malformed numeric literal `{s}`")))?;
    Ok(Value::Float(f))
}

/// Parse one [`Value`] dispatched on the leading character.
fn parse_value(t: &mut Tokenizer<'_>) -> Result<Value> {
    t.skip_trivia();
    let Some(c) = t.peek_char() else {
        return Err(invalid("EOF where value expected"));
    };
    match c {
        '"' | '\'' => Ok(Value::String(read_quoted_string(t)?)),
        '@' => Ok(Value::Asset(read_asset(t)?)),
        '<' => Ok(Value::Path(read_path(t)?)),
        '(' => Ok(Value::Tuple(read_tuple(t)?)),
        '[' => Ok(Value::Array(read_array(t)?)),
        '+' | '-' => read_number(t),
        c if c.is_ascii_digit() => read_number(t),
        c if is_ident_start(c) => {
            let id = read_ident(t)?;
            match id.as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                "None" | "none" => Ok(Value::None),
                _ => Ok(Value::Token(id)),
            }
        }
        _ => Err(invalid(format!(
            "unexpected character `{c}` at offset {} where value expected",
            t.pos
        ))),
    }
}

/// Parse a single `def Type "name" ( ... ) { ... }` prim block.
fn parse_prim(t: &mut Tokenizer<'_>) -> Result<Prim> {
    t.skip_trivia();
    let spec = read_ident(t)?;
    if !matches!(spec.as_str(), "def" | "over" | "class") {
        return Err(invalid(format!(
            "expected `def`, `over`, or `class` at offset {}, got `{spec}`",
            t.pos
        )));
    }
    t.skip_trivia();
    // Optional schema name (`def Mesh ...`); a bare `def "Name" {}`
    // is also legal in USD though rare in USDZ.
    let mut type_name = String::new();
    if matches!(t.peek_char(), Some(c) if is_ident_start(c)) {
        type_name = read_ident(t)?;
        t.skip_trivia();
    }
    // Quoted prim name.
    let name = if matches!(t.peek_char(), Some('"' | '\'')) {
        read_quoted_string(t)?
    } else {
        return Err(invalid(format!(
            "expected quoted prim name at offset {}",
            t.pos
        )));
    };
    t.skip_trivia();
    let metadata = if t.eat('(') {
        parse_metadata_block(t)?
    } else {
        BTreeMap::new()
    };
    t.skip_trivia();
    if !t.eat('{') {
        return Err(invalid(format!(
            "expected `{{` to start prim body at offset {}",
            t.pos
        )));
    }
    let (attrs, children) = parse_prim_body(t)?;
    Ok(Prim {
        spec,
        type_name,
        name,
        metadata,
        attrs,
        children,
    })
}

/// Parse the inside of a `{ ... }` prim body — a mix of attribute
/// statements and nested prim definitions.
fn parse_prim_body(t: &mut Tokenizer<'_>) -> Result<(BTreeMap<String, Attr>, Vec<Prim>)> {
    let mut attrs = BTreeMap::new();
    let mut children = Vec::new();
    loop {
        t.skip_trivia();
        if t.eat('}') {
            return Ok((attrs, children));
        }
        if t.eof() {
            return Err(invalid("EOF inside prim body"));
        }
        // Decide between nested `def Foo "Name" { ... }` vs an
        // attribute statement by peeking at the leading keyword.
        let saved = t.pos;
        let lead = peek_ident(t).unwrap_or_default();
        if matches!(lead.as_str(), "def" | "over" | "class") {
            let prim = parse_prim(t)?;
            children.push(prim);
            continue;
        }
        let _ = saved;
        // Attribute / relationship — first read the type tokens up
        // to the attribute name. Examples:
        //   point3f[] points = ...
        //   uniform token info:id = "UsdPreviewSurface"
        //   color3f inputs:diffuseColor = (1, 0, 0)
        //   color3f inputs:diffuseColor.connect = </path>
        //   rel material:binding = </path>
        let (type_token, attr_name) = read_type_then_name(t)?;
        t.skip_trivia();
        let value = if t.eat('=') {
            t.skip_trivia();
            parse_value(t)?
        } else {
            // Attribute declared without a value (`token outputs:surface`).
            Value::None
        };
        t.skip_trivia();
        let metadata = if t.eat('(') {
            parse_metadata_block(t)?
        } else {
            BTreeMap::new()
        };
        attrs.insert(
            attr_name,
            Attr {
                type_token,
                value,
                metadata,
            },
        );
    }
}

/// Consume an attribute declaration prefix:
///
/// ```text
/// [variability] [custom] type[/[]] attr_name
/// ```
///
/// where `variability ∈ {uniform, varying, config}`. Returns the
/// joined type-token string (kept verbatim so a future writer can
/// reproduce the spelling) plus the parsed attribute name.
fn read_type_then_name(t: &mut Tokenizer<'_>) -> Result<(String, String)> {
    let mut type_parts: Vec<String> = Vec::new();
    // Consume any number of leading modifier keywords. USD only
    // recognises a small fixed set in the spelling slot before
    // the type — we list them explicitly so a typo doesn't
    // silently swallow an attribute name.
    loop {
        t.skip_trivia();
        let Some(word) = peek_ident(t) else {
            return Err(invalid(format!("expected type token at offset {}", t.pos)));
        };
        if matches!(
            word.as_str(),
            "uniform" | "varying" | "config" | "custom" | "rel"
        ) {
            let _ = read_ident(t)?;
            type_parts.push(word);
            // Special case: `rel` is the relationship keyword and
            // is itself a complete type slot — no element type
            // follows.
            if type_parts.last().map(|s| s.as_str()) == Some("rel") {
                let attr_name = read_attr_name(t)?;
                return Ok((type_parts.join(" "), attr_name));
            }
            continue;
        }
        // Not a modifier — this is the actual element type.
        let ty = read_ident(t)?;
        let mut full = ty;
        if t.rest().starts_with("[]") {
            full.push_str("[]");
            t.advance();
            t.advance();
        }
        type_parts.push(full);
        break;
    }
    let attr_name = read_attr_name(t)?;
    Ok((type_parts.join(" "), attr_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_layer() {
        let src = b"#usda 1.0\n(\n    upAxis = \"Y\"\n    metersPerUnit = 1.0\n)\n";
        let layer = parse(src).expect("parse ok");
        assert_eq!(
            layer.metadata.get("upAxis").and_then(|v| v.as_text()),
            Some("Y")
        );
        assert_eq!(
            layer.metadata.get("metersPerUnit").and_then(|v| v.as_f32()),
            Some(1.0)
        );
        assert!(layer.prims.is_empty());
    }

    #[test]
    fn parse_xform_with_mesh_child() {
        let src = br#"#usda 1.0
def Xform "Root" {
    def Mesh "M" {
        int[] faceVertexCounts = [3, 3]
        int[] faceVertexIndices = [0, 1, 2, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0)]
        uniform token subdivisionScheme = "none"
    }
}
"#;
        let layer = parse(src).expect("parse ok");
        assert_eq!(layer.prims.len(), 1);
        let root = &layer.prims[0];
        assert_eq!(root.spec, "def");
        assert_eq!(root.type_name, "Xform");
        assert_eq!(root.name, "Root");
        assert_eq!(root.children.len(), 1);
        let m = &root.children[0];
        assert_eq!(m.type_name, "Mesh");
        assert_eq!(
            m.attrs
                .get("faceVertexCounts")
                .unwrap()
                .value
                .as_seq()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            m.attrs.get("points").unwrap().value.as_seq().unwrap().len(),
            4
        );
        let scheme = m.attrs.get("subdivisionScheme").unwrap();
        assert_eq!(scheme.value.as_text(), Some("none"));
    }

    #[test]
    fn parse_asset_path() {
        let src = b"#usda 1.0\ndef Shader \"S\" {\n    asset inputs:file = @./diffuse.png@\n}\n";
        let layer = parse(src).expect("parse ok");
        let shader = &layer.prims[0];
        let attr = shader.attrs.get("inputs:file").expect("file attr");
        match &attr.value {
            Value::Asset(s) => assert_eq!(s, "./diffuse.png"),
            other => panic!("expected Asset, got {other:?}"),
        }
    }

    #[test]
    fn parse_relationship() {
        let src = b"#usda 1.0\ndef Mesh \"M\" {\n    rel material:binding = </Root/Mat>\n}\n";
        let layer = parse(src).expect("parse ok");
        let m = &layer.prims[0];
        let rel = m.attrs.get("material:binding").unwrap();
        match &rel.value {
            Value::Path(p) => assert_eq!(p, "/Root/Mat"),
            other => panic!("expected Path, got {other:?}"),
        }
    }
}
