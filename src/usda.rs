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
    /// VariantSets declared inside the prim body. Outer key is the
    /// variantSet name (`"shapeVariant"`); inner key is each variant
    /// name (`"Capsule"`, `"Cone"`, ...). Each [`Variant`] carries
    /// the inner prim-body block (metadata, attrs, child prims).
    /// Empty when the prim defines no variantSet — the common case.
    ///
    /// USDA syntax (per the OpenUSD glossary `simpleVariantSet.usd`
    /// example):
    ///
    /// ```text
    /// def Xform "Implicits" (
    ///     append variantSets = "shapeVariant"
    /// ) {
    ///     variantSet "shapeVariant" = {
    ///         "Capsule" {
    ///             def Capsule "Pill" { }
    ///         }
    ///         "Cone" (
    ///             prepend references = @other.usd@
    ///         ) {
    ///             def Cone "PartyHat" { }
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// Round 6 captures the structured form so a future composition
    /// engine can switch on `variants = { string foo = "bar" }`
    /// metadata to bring the selected variant's contents up into the
    /// prim's own `attrs` / `children`. The current Scene3D
    /// translator ignores `variant_sets`; the round-trip writer
    /// re-emits the block verbatim.
    pub variant_sets: BTreeMap<String, BTreeMap<String, Variant>>,
}

/// One variant within a [`Prim`]'s `variant_sets`.
///
/// Carries the same shape as a [`Prim`] body: optional `( ... )`
/// metadata, attribute statements, and nested prim children. The
/// variant doesn't have its own `spec` / `type_name` / `name` —
/// the variant name is the outer-map key on
/// [`Prim::variant_sets`] and the type/spec are inherited from the
/// enclosing prim when the variant is selected.
#[derive(Clone, Debug, Default)]
pub struct Variant {
    /// Optional `( ... )` metadata block — `prepend references = ...`,
    /// `kind = "..."`, etc. Same shape as [`Prim::metadata`].
    pub metadata: BTreeMap<String, Value>,
    /// Attribute and relationship statements inside the variant
    /// body. Same shape as [`Prim::attrs`].
    pub attrs: BTreeMap<String, Attr>,
    /// Child prims declared inside the variant body, in declaration
    /// order.
    pub children: Vec<Prim>,
}

impl Prim {
    /// Return a [`Prim`] with every selected variant composed into
    /// the prim body — the variant's `attrs` and `children` are
    /// merged in **under** the prim's own opinions (Local Wins per
    /// the OpenUSD glossary's LIVRPS strength order: Local,
    /// Inherits, VariantSets, References, Payloads, Specializes).
    ///
    /// The selection is read from the prim's own `metadata` under
    /// the `variants` key — the standard USDA shape:
    ///
    /// ```text
    /// def Xform "Implicits" (
    ///     variants = { string shapeVariant = "Capsule" }
    /// ) {
    ///     variantSet "shapeVariant" = {
    ///         "Capsule" { def Capsule "Pill" {} }
    ///         "Cone"    { def Cone "PartyHat" {} }
    ///     }
    /// }
    /// ```
    ///
    /// With `shapeVariant = "Capsule"`, the resolved prim picks up
    /// `def Capsule "Pill"` as a child (in addition to any children
    /// authored directly on the prim).
    ///
    /// **What's resolved:**
    ///
    /// * `metadata["variants"] = { string SET = "VARIANT", ... }`
    ///   names which variant to pull in for each declared variantSet.
    /// * For each `(SET, VARIANT)` pair, the matching
    ///   `variant_sets[SET][VARIANT]` is composed in:
    ///   - `Variant::children` are appended to the prim's own
    ///     `children` (variant-authored children are weaker than
    ///     local children sharing the same name; we de-dup by prim
    ///     name + spec, keeping the local one).
    ///   - `Variant::attrs` are merged into the prim's `attrs` only
    ///     for keys not already present locally.
    ///   - `Variant::metadata` is merged into the prim's `metadata`
    ///     for keys not already present locally — *except* the
    ///     `references` / `payload` keys, which round 7 records on
    ///     the resolved prim's metadata as
    ///     `usd:variantReferences` so the Scene3D extras layer
    ///     surfaces the unresolved arc to the caller (we don't
    ///     follow external references in round 7).
    /// * Nested children are *not* recursed into — call
    ///   [`Prim::resolved_variants`] again on each child to
    ///   compose its variants in turn. The top-level translator in
    ///   [`crate::usd_to_scene::translate`] handles that walk.
    ///
    /// **What's preserved:**
    ///
    /// * `variant_sets` stays on the resolved prim verbatim so a
    ///   writer round-trip can re-emit the block.
    /// * `metadata["variants"]` stays so the selection itself
    ///   round-trips.
    ///
    /// **Edge cases:**
    ///
    /// * No `variants` metadata key → prim returned unchanged
    ///   (variant_sets are present but no selection authored).
    /// * Selection naming a variant that doesn't exist → that
    ///   `(SET, VARIANT)` pair is silently skipped (per USD's
    ///   "variant selections are not required" tolerance rule).
    /// * Selection naming a variantSet that doesn't exist → also
    ///   silently skipped.
    pub fn resolved_variants(&self) -> Prim {
        let Some(Value::Dict(selections)) = self.metadata.get("variants") else {
            return self.clone();
        };
        if self.variant_sets.is_empty() {
            return self.clone();
        }

        let mut out_attrs = self.attrs.clone();
        let mut out_children = self.children.clone();
        let mut out_metadata = self.metadata.clone();

        for (set_name, sel_value) in selections {
            let variant_name = match sel_value {
                Value::String(s) | Value::Token(s) => s.as_str(),
                _ => continue,
            };
            let Some(set) = self.variant_sets.get(set_name) else {
                continue;
            };
            let Some(variant) = set.get(variant_name) else {
                continue;
            };

            // Variant attrs lose to local opinions.
            for (k, v) in &variant.attrs {
                out_attrs.entry(k.clone()).or_insert_with(|| v.clone());
            }

            // Variant metadata loses to local; composition arcs are
            // mirrored into a side-channel so the Scene3D extras
            // layer surfaces them.
            for (k, v) in &variant.metadata {
                if matches!(k.as_str(), "references" | "payload") {
                    let bucket_key = format!("usd:variantReferences:{set_name}:{variant_name}:{k}");
                    out_metadata.insert(bucket_key, v.clone());
                    continue;
                }
                out_metadata.entry(k.clone()).or_insert_with(|| v.clone());
            }

            // Variant children are appended; collisions on
            // (spec, name) lose to local children.
            for child in &variant.children {
                let key = (child.spec.as_str(), child.name.as_str());
                let collision = out_children
                    .iter()
                    .any(|c| (c.spec.as_str(), c.name.as_str()) == key);
                if !collision {
                    out_children.push(child.clone());
                }
            }
        }

        Prim {
            spec: self.spec.clone(),
            type_name: self.type_name.clone(),
            name: self.name.clone(),
            metadata: out_metadata,
            attrs: out_attrs,
            children: out_children,
            variant_sets: self.variant_sets.clone(),
        }
    }
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
    /// Typed dictionary `{ TYPE NAME = VALUE; ... }`. Apple's
    /// `usdzconvert` writes `customLayerData` and similar in this
    /// shape. We parse-and-discard via empty map for round 1: the
    /// decoder doesn't need the values, and storing every entry
    /// would force a typed-value variant we can ignore. The
    /// presence of the key is preserved (so a `customLayerData`
    /// metadata entry round-trips as `Value::Dict`).
    Dict(BTreeMap<String, Value>),
    /// Composition arc target — an asset path with an optional
    /// in-asset prim selector. Apple emits `references = @file@</Prim>`
    /// or `payload = @file@</Prim>`. We capture both pieces so
    /// future composition support has them; round 1 only needs
    /// the parser to accept the syntax.
    AssetWithPath {
        /// `@...@` portion (empty for path-only refs).
        asset: String,
        /// `<...>` portion (empty when omitted).
        prim_path: String,
    },
    /// A list-edited metadata field — `prepend references = ...`,
    /// `append references = ...`, `delete references = ...`,
    /// `add references = ...`, `reorder references = ...`, or an
    /// *explicit* (unqualified) `references = ...` reset.
    ///
    /// USDA composition-arc fields (`references`, `payload`,
    /// `inherits`, `specializes`, `apiSchemas`, `variantSets`, …) are
    /// **list-edited**: the same field can be authored several times in
    /// one prim block with different operators, and each operator
    /// contributes to a different sublist of the resolved
    /// `SdfListOp`. The strength order this crate honours is
    /// *prepended* (strongest) → *explicit* → *appended* → with
    /// *deleted* entries removed from whatever the additive sublists
    /// produced. Capturing the operator (instead of discarding it, as
    /// the round-1 parser did) is what lets composition treat
    /// `delete references = @x@` as a *removal* rather than an *add*,
    /// and lets the writer re-emit the operator the author wrote.
    ///
    /// The wrapped [`ListOp`] keeps each sublist separate so multiple
    /// authored operators on the same field merge losslessly.
    ListOp(Box<ListOp>),
    /// Anything we didn't recognise; preserved verbatim.
    Raw(String),
    /// Empty value — attribute declared but not assigned (`token
    /// outputs:surface`).
    None,
}

/// A single USD list-edit operator keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListEditOp {
    /// `prepend` — strongest additive sublist (composes ahead of
    /// explicit + appended entries).
    Prepend,
    /// `append` — weakest additive sublist (composes behind explicit +
    /// prepended entries).
    Append,
    /// `delete` — removes the named targets from whatever the additive
    /// sublists produced.
    Delete,
    /// `add` — the legacy additive operator; treated as an `append`
    /// for composition (it adds at the back if absent).
    Add,
    /// `reorder` — reorders existing entries without adding; carried
    /// through for round-trip but does not change the composed target
    /// set.
    Reorder,
    /// No operator keyword — an *explicit* (reset) authoring that
    /// replaces any weaker opinion's list outright.
    Explicit,
}

impl ListEditOp {
    /// Parse the leading keyword of a list-edited field. Returns
    /// [`ListEditOp::Explicit`] for any non-keyword (the unqualified
    /// reset form).
    pub fn from_keyword(word: &str) -> Self {
        match word {
            "prepend" => Self::Prepend,
            "append" => Self::Append,
            "delete" => Self::Delete,
            "add" => Self::Add,
            "reorder" => Self::Reorder,
            _ => Self::Explicit,
        }
    }

    /// The USDA keyword for this operator, or `None` for
    /// [`ListEditOp::Explicit`] (which is emitted bare).
    pub fn keyword(self) -> Option<&'static str> {
        match self {
            Self::Prepend => Some("prepend"),
            Self::Append => Some("append"),
            Self::Delete => Some("delete"),
            Self::Add => Some("add"),
            Self::Reorder => Some("reorder"),
            Self::Explicit => None,
        }
    }
}

/// A list-edited metadata field, mirroring USD's `SdfListOp`: each
/// operator's authored value is kept in its own sublist so several
/// operators on the same field (`delete references = @a@` followed by
/// `prepend references = @b@`) merge losslessly. Entries are stored in
/// authoring order within each sublist.
#[derive(Clone, Debug, Default)]
pub struct ListOp {
    /// `prepend`-authored value (the strongest additive sublist).
    pub prepended: Option<Value>,
    /// `append`/`add`-authored value (the weakest additive sublist).
    pub appended: Option<Value>,
    /// `delete`-authored value (targets removed from the additive
    /// result).
    pub deleted: Option<Value>,
    /// Unqualified (explicit reset) authored value.
    pub explicit: Option<Value>,
    /// `reorder`-authored value (round-trip only).
    pub reordered: Option<Value>,
}

impl ListOp {
    /// Build a single-operator list-op from one authored value.
    pub fn single(op: ListEditOp, value: Value) -> Self {
        let mut out = Self::default();
        out.set(op, value);
        out
    }

    /// Store `value` under `op`'s sublist, merging additively with any
    /// value already present (multiple authored statements of the same
    /// operator concatenate, matching USDA's "the field may appear more
    /// than once" rule).
    pub fn set(&mut self, op: ListEditOp, value: Value) {
        let slot = match op {
            ListEditOp::Prepend => &mut self.prepended,
            ListEditOp::Append | ListEditOp::Add => &mut self.appended,
            ListEditOp::Delete => &mut self.deleted,
            ListEditOp::Explicit => &mut self.explicit,
            ListEditOp::Reorder => &mut self.reordered,
        };
        *slot = Some(match slot.take() {
            Some(existing) => merge_listop_values(existing, value),
            None => value,
        });
    }

    /// Every populated sublist with its operator, in a stable order
    /// (prepend, explicit, append, delete, reorder). Used for the JSON
    /// extras view and round-trip emission.
    pub fn entries(&self) -> impl Iterator<Item = (ListEditOp, &Value)> {
        [
            (ListEditOp::Prepend, self.prepended.as_ref()),
            (ListEditOp::Explicit, self.explicit.as_ref()),
            (ListEditOp::Append, self.appended.as_ref()),
            (ListEditOp::Delete, self.deleted.as_ref()),
            (ListEditOp::Reorder, self.reordered.as_ref()),
        ]
        .into_iter()
        .filter_map(|(op, v)| v.map(|val| (op, val)))
    }

    /// The strength-ordered additive sublists, strongest first:
    /// prepended, then explicit, then appended. Used by the writer to
    /// re-emit each authored operator and by composition to flatten the
    /// list to its resolved target sequence.
    pub fn additive_in_strength_order(&self) -> impl Iterator<Item = (ListEditOp, &Value)> {
        [
            (ListEditOp::Prepend, self.prepended.as_ref()),
            (ListEditOp::Explicit, self.explicit.as_ref()),
            (ListEditOp::Append, self.appended.as_ref()),
        ]
        .into_iter()
        .filter_map(|(op, v)| v.map(|val| (op, val)))
    }
}

/// Concatenate two list-op sublist values that share an operator. Two
/// arrays merge into one array; otherwise the values wrap into an
/// array preserving authoring order.
fn merge_listop_values(existing: Value, incoming: Value) -> Value {
    let mut items = match existing {
        Value::Array(v) => v,
        other => vec![other],
    };
    match incoming {
        Value::Array(v) => items.extend(v),
        other => items.push(other),
    }
    Value::Array(items)
}

impl Value {
    /// Return the string form of a `Token` or `String` value.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Token(s) | Self::String(s) | Self::Asset(s) | Self::Path(s) | Self::Raw(s) => {
                Some(s.as_str())
            }
            Self::AssetWithPath { asset, .. } => Some(asset.as_str()),
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

    /// Compute `(line, column)` (both 1-indexed) for the current
    /// cursor position. Linear scan — only called on the error path
    /// so the cost is irrelevant.
    fn line_col(&self) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, c) in self.src.char_indices() {
            if i >= self.pos {
                break;
            }
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Render the current position as `line L:C (offset N)` for use
    /// inside error messages — much more actionable than a raw byte
    /// offset when the source is hand-authored USDA spanning dozens
    /// of lines.
    fn pos_display(&self) -> String {
        let (l, c) = self.line_col();
        format!("line {l}:{c} (offset {})", self.pos)
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
        // Read an optional `prepend` / `append` / `delete` / `add` /
        // `reorder` list-edit operator. These select which sublist of
        // an `SdfListOp`-style field the authored value contributes to
        // (a `delete` is a removal, not an add) and must be preserved
        // so composition resolves the right target set and the writer
        // re-emits the operator the author wrote.
        let mut list_edit_op: Option<ListEditOp> = None;
        let saved = t.pos;
        if let Some(word) = peek_ident(t) {
            if matches!(
                word.as_str(),
                "prepend" | "append" | "delete" | "add" | "reorder"
            ) {
                let _ = read_ident(t)?;
                t.skip_trivia();
                list_edit_op = Some(ListEditOp::from_keyword(&word));
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
        insert_metadata(&mut out, name, list_edit_op, value);
    }
}

/// Insert a parsed metadata field into the block map, folding
/// list-edited fields into a [`Value::ListOp`].
///
/// * A field with an explicit list-edit operator (`prepend` / `append`
///   / `delete` / `add` / `reorder`) is stored as a [`Value::ListOp`].
///   If the field was already authored in the same block (with any
///   operator), the new sublist merges into the existing [`ListOp`] so
///   `delete references = @a@` then `prepend references = @b@` keep
///   both opinions.
/// * A field with **no** operator is stored as the bare value, unless
///   the field already holds a [`Value::ListOp`] — in which case the
///   unqualified statement is its *explicit* (reset) sublist.
fn insert_metadata(
    out: &mut BTreeMap<String, Value>,
    name: String,
    op: Option<ListEditOp>,
    value: Value,
) {
    match (op, out.remove(&name)) {
        // First/only authoring, no operator → plain value.
        (None, None) => {
            out.insert(name, value);
        }
        // Operator present → fold into a ListOp.
        (Some(op), prev) => {
            let mut list = match prev {
                Some(Value::ListOp(existing)) => *existing,
                Some(other) => {
                    // A prior unqualified authoring becomes the
                    // explicit sublist.
                    ListOp::single(ListEditOp::Explicit, other)
                }
                None => ListOp::default(),
            };
            list.set(op, value);
            out.insert(name, Value::ListOp(Box::new(list)));
        }
        // No operator but the field already holds a ListOp → the
        // unqualified value is the explicit (reset) sublist.
        (None, Some(Value::ListOp(mut existing))) => {
            existing.set(ListEditOp::Explicit, value);
            out.insert(name, Value::ListOp(existing));
        }
        // No operator, prior plain value → last write wins (the
        // round-1 behaviour for non-list-edited keys).
        (None, Some(_)) => {
            out.insert(name, value);
        }
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
            "expected identifier, found `{first}` at {}",
            t.pos_display()
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
            "expected attribute name at {}",
            t.pos_display()
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
            "expected `,` or `)` in tuple at {}",
            t.pos_display()
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
            "expected `,` or `]` in array at {}",
            t.pos_display()
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
            "expected numeric literal at {}",
            t.pos_display()
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
        '@' => {
            // Asset path, possibly followed by a `</prim>` selector
            // (composition arc: `references = @file@</Prim>` /
            // `payload = @file@</Prim>`). When the selector is
            // present we surface a richer `AssetWithPath` so the
            // syntax round-trips losslessly; otherwise fall back to
            // a plain `Asset`.
            let asset = read_asset(t)?;
            if t.peek_char() == Some('<') {
                let prim_path = read_path(t)?;
                Ok(Value::AssetWithPath { asset, prim_path })
            } else {
                Ok(Value::Asset(asset))
            }
        }
        '<' => Ok(Value::Path(read_path(t)?)),
        '(' => Ok(Value::Tuple(read_tuple(t)?)),
        '[' => Ok(Value::Array(read_array(t)?)),
        '{' => Ok(Value::Dict(read_dict(t)?)),
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
            "unexpected character `{c}` at {} where value expected",
            t.pos_display()
        ))),
    }
}

/// Read a `{ TYPE NAME = VALUE ; ... }` typed-dictionary literal.
///
/// Apple's `usdzconvert` emits these in `customLayerData` /
/// `customData` / similar metadata positions:
///
/// ```text
/// customLayerData = {
///     string apple_metadata = "value"
///     dictionary nested = {
///         int count = 3
///     }
///     double[3] vec = (0, 1, 2)
/// }
/// ```
///
/// Each entry has shape `<TYPENAME> <name> = <value>`, with the
/// special case of `dictionary <name> = { ... }` for nested dicts.
/// Entries are separated by whitespace (newlines or semicolons).
///
/// Round 1 strategy: parse-and-discard. We need to *consume* the
/// dictionary so the parser advances past Apple's headers, but the
/// decoder doesn't read the contents — `Scene3D` translation
/// ignores layer-metadata entries other than `defaultPrim` /
/// `upAxis` / `metersPerUnit`, which are never wrapped in dicts.
/// Returning the parsed entries (rather than truly discarding) lets
/// future rounds plug in custom-data extraction without re-tokenising.
fn read_dict(t: &mut Tokenizer<'_>) -> Result<BTreeMap<String, Value>> {
    if !t.eat('{') {
        return Err(invalid("expected `{` to start dictionary"));
    }
    let mut out = BTreeMap::new();
    loop {
        t.skip_trivia();
        if t.eat('}') {
            return Ok(out);
        }
        if t.eof() {
            return Err(invalid("EOF inside `{ ... }` dictionary"));
        }
        // Tolerate a stray `;` separator between entries.
        if t.eat(';') {
            continue;
        }
        // Each entry leads with a type token (`string`, `int`,
        // `double[3]`, `dictionary`, `token[]`, ...). We reuse the
        // existing type-then-name reader so all USD type-spelling
        // quirks (`uniform`, `[]` array suffix, etc.) are handled
        // uniformly.
        let (_type_token, name) = read_type_then_name(t)?;
        t.skip_trivia();
        if t.eat('=') {
            t.skip_trivia();
            let value = parse_value(t)?;
            out.insert(name, value);
        } else {
            // Bare entry without `=` — store as `Value::None` so
            // the key still round-trips.
            out.insert(name, Value::None);
        }
    }
}

/// Parse the contents of a `variantSet "name" = { ... }` block —
/// a sequence of `"variantName" ( optional metadata ) { body }`
/// entries.
///
/// Each body has the same shape as a prim body (attrs + nested
/// prims + nested variantSets), so we recurse into
/// [`parse_prim_body`] for it.
fn parse_variant_set_body(t: &mut Tokenizer<'_>) -> Result<BTreeMap<String, Variant>> {
    let mut out: BTreeMap<String, Variant> = BTreeMap::new();
    loop {
        t.skip_trivia();
        if t.eat('}') {
            return Ok(out);
        }
        if t.eof() {
            return Err(invalid("EOF inside variantSet body"));
        }
        let variant_name = if matches!(t.peek_char(), Some('"' | '\'')) {
            read_quoted_string(t)?
        } else {
            return Err(invalid(format!(
                "expected quoted variant name at {}",
                t.pos_display()
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
                "expected `{{` to start variant `{variant_name}` body at {}",
                t.pos_display()
            )));
        }
        // The inner block is structurally identical to a prim body
        // (attrs + nested prims + nested variantSets allowed). The
        // glossary's simpleVariantSet.usd shows the common case
        // (one nested `def`); composition examples nest references
        // in the variant metadata which is also handled.
        let (attrs, children, _nested_vsets) = parse_prim_body(t)?;
        // Per the USD spec a variant can itself declare nested
        // variantSets. We don't surface them through `Variant`'s
        // shape (avoids unbounded recursion in the public type) —
        // documented limitation.
        out.insert(
            variant_name,
            Variant {
                metadata,
                attrs,
                children,
            },
        );
    }
}

/// Parse a single `def Type "name" ( ... ) { ... }` prim block.
fn parse_prim(t: &mut Tokenizer<'_>) -> Result<Prim> {
    t.skip_trivia();
    let spec = read_ident(t)?;
    if !matches!(spec.as_str(), "def" | "over" | "class") {
        return Err(invalid(format!(
            "expected `def`, `over`, or `class` at {}, got `{spec}`",
            t.pos_display()
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
            "expected quoted prim name at {}",
            t.pos_display()
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
            "expected `{{` to start prim body at {}",
            t.pos_display()
        )));
    }
    let (attrs, children, variant_sets) = parse_prim_body(t)?;
    Ok(Prim {
        spec,
        type_name,
        name,
        metadata,
        attrs,
        children,
        variant_sets,
    })
}

/// Return shape of [`parse_prim_body`] — `(attrs, children,
/// variant_sets)`. Aliased to keep the function signature under
/// clippy's `type_complexity` cap.
type PrimBody = (
    BTreeMap<String, Attr>,
    Vec<Prim>,
    BTreeMap<String, BTreeMap<String, Variant>>,
);

/// Parse the inside of a `{ ... }` prim body — a mix of attribute
/// statements, nested prim definitions, and `variantSet "name" =
/// { ... }` blocks.
fn parse_prim_body(t: &mut Tokenizer<'_>) -> Result<PrimBody> {
    let mut attrs = BTreeMap::new();
    let mut children = Vec::new();
    let mut variant_sets: BTreeMap<String, BTreeMap<String, Variant>> = BTreeMap::new();
    loop {
        t.skip_trivia();
        if t.eat('}') {
            return Ok((attrs, children, variant_sets));
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
        if lead == "variantSet" {
            // `variantSet "name" = { "variantA" { ... } "variantB" { ... } }`.
            // Per the OpenUSD glossary `simpleVariantSet.usd` example.
            let _ = read_ident(t)?; // consume `variantSet`
            t.skip_trivia();
            let vset_name = if matches!(t.peek_char(), Some('"' | '\'')) {
                read_quoted_string(t)?
            } else {
                return Err(invalid(format!(
                    "expected quoted variantSet name at {}",
                    t.pos_display()
                )));
            };
            t.skip_trivia();
            if !t.eat('=') {
                return Err(invalid(format!(
                    "expected `=` after variantSet name at {}",
                    t.pos_display()
                )));
            }
            t.skip_trivia();
            if !t.eat('{') {
                return Err(invalid(format!(
                    "expected `{{` to start variantSet body at {}",
                    t.pos_display()
                )));
            }
            let variants = parse_variant_set_body(t)?;
            // Merge into any pre-existing entry for the same vset
            // name (USDA permits re-opening a variantSet — the
            // sparse-edit model — though it's rare).
            let entry = variant_sets.entry(vset_name).or_default();
            for (k, v) in variants {
                entry.insert(k, v);
            }
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
            return Err(invalid(format!(
                "expected type token at {}",
                t.pos_display()
            )));
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
        // Type suffix: `[]` for arbitrary-length arrays, or `[N]`
        // for fixed-arity vector types (`double[3]`, `int[2]`).
        // Apple's `usdzconvert` writes `double[3]` in dict literals
        // for vector metadata; the `N` is the arity, captured for
        // round-tripping in the type-token string.
        if t.rest().starts_with("[]") {
            full.push_str("[]");
            t.advance();
            t.advance();
        } else if t.peek_char() == Some('[') {
            full.push('[');
            t.advance();
            while let Some(c) = t.peek_char() {
                if c == ']' {
                    full.push(']');
                    t.advance();
                    break;
                }
                full.push(c);
                t.advance();
            }
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
