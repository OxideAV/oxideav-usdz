//! `.usdc` → [`usda::Layer`] — materialise a Crate file into the same
//! prim-tree model the USDA text parser produces, so the whole
//! downstream pipeline (composition, `usd_to_scene`) consumes binary
//! layers unchanged.
//!
//! The join is the one §16.3.8.4.6 describes: each spec row is a
//! `(path, form, field set)` triple. Paths come from the
//! §16.3.8.4.5.4 construction algorithm, field values from the
//! §16.3.9/§16.3.10 [`ValueDecoder`], and the spec *forms* map onto
//! the text model as:
//!
//! * **Layer** (7, the pseudo-root at `/`) → [`Layer::metadata`],
//! * **Prim** (6) → a [`Prim`] (specifier / typeName / metadata),
//! * **Attribute** (1) → an [`Attr`] on its parent prim, including
//!   `<name>.timeSamples` and `<name>.connect` companion statements,
//! * **Relationship** (8) → a `rel` [`Attr`] with its target paths,
//! * **Variant / VariantSet** (10 / 11) → not yet bridged (refused
//!   precisely),
//! * the §16.3.8.4.6 compatibility forms (2–5, 9) and Unknown (0) →
//!   inert, per the spec ("may be processed … but are inert").

use std::collections::BTreeMap;

use crate::error::{invalid, unsupported};
use crate::usda::{Attr, Layer, ListOp, Prim, Value};
use crate::usdc::{NamedSpec, UsdcFile, ValueRep};
use crate::usdc_values::ValueDecoder;
use crate::Result;

/// §16.3.8.4.6 spec forms.
const FORM_ATTRIBUTE: i32 = 1;
const FORM_PRIM: i32 = 6;
const FORM_LAYER: i32 = 7;
const FORM_RELATIONSHIP: i32 = 8;
const FORM_VARIANT: i32 = 10;
const FORM_VARIANT_SET: i32 = 11;

/// Decode a `.usdc` byte buffer into a [`Layer`].
pub fn layer_from_usdc(bytes: &[u8]) -> Result<Layer> {
    let file = UsdcFile::parse(bytes)?;
    let decoder = ValueDecoder::new(&file, bytes)?;
    let specs = file.decode_named_specs(bytes)?;
    let paths = decoder.paths();

    let mut layer_metadata: BTreeMap<String, Value> = BTreeMap::new();
    // Prim builds keyed by full path, in spec order.
    let mut prims: BTreeMap<String, PrimBuild> = BTreeMap::new();
    let mut prim_order: Vec<String> = Vec::new();
    // Property specs deferred until every prim exists.
    let mut properties: Vec<(&NamedSpec, &str)> = Vec::new();

    for spec in &specs {
        let path = paths
            .get(usize::try_from(spec.path_index).unwrap_or(usize::MAX))
            .ok_or_else(|| {
                invalid(format!(
                    "USDC layer: spec path index {} outside the {}-entry path table",
                    spec.path_index,
                    paths.len()
                ))
            })?
            .as_str();
        match spec.spec_type {
            FORM_LAYER => {
                for (name, rep) in &spec.fields {
                    if name == "primChildren" {
                        // Ordering info — the root prims already appear
                        // in walk order; the token vector adds nothing
                        // the text form would carry.
                        continue;
                    }
                    let value = decoder.decode(ValueRep::from_raw(*rep))?;
                    layer_metadata.insert(name.clone(), value);
                }
            }
            FORM_PRIM => {
                let build = build_prim(path, spec, &decoder)?;
                if prims.insert(path.to_owned(), build).is_some() {
                    return Err(invalid(format!(
                        "USDC layer: two prim specs claim the path {path}"
                    )));
                }
                prim_order.push(path.to_owned());
            }
            FORM_ATTRIBUTE | FORM_RELATIONSHIP => properties.push((spec, path)),
            FORM_VARIANT | FORM_VARIANT_SET => {
                return Err(unsupported(format!(
                    "USDC layer: variant spec (form {}) at {path} — Crate variant blocks are not bridged to the text model yet",
                    spec.spec_type
                )));
            }
            // §16.3.8.4.6: unknown and compatibility forms are inert.
            _ => {}
        }
    }

    // Attach properties to their prims.
    for (spec, path) in properties {
        let Some(dot) = path.rfind('.') else {
            return Err(invalid(format!(
                "USDC layer: property spec at {path} has no `.` component"
            )));
        };
        let (prim_path, attr_name) = (&path[..dot], &path[dot + 1..]);
        let Some(prim) = prims.get_mut(prim_path) else {
            // A property whose parent is not a prim spec (e.g. hung off
            // a compatibility form) has no slot in the text model.
            continue;
        };
        if spec.spec_type == FORM_RELATIONSHIP {
            attach_relationship(prim, attr_name, spec, &decoder)?;
        } else {
            attach_attribute(prim, attr_name, spec, &decoder)?;
        }
    }

    // Assemble the tree: children attach to parents, deepest first so
    // every child is complete before its parent consumes it.
    let mut order_deepest_first = prim_order.clone();
    order_deepest_first.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));
    for path in order_deepest_first {
        let build = prims.remove(&path).expect("still present");
        let prim = finish_prim(build);
        let parent_path = match path.rfind('/') {
            Some(0) => "/".to_owned(),
            Some(i) => path[..i].to_owned(),
            None => "/".to_owned(),
        };
        if parent_path == "/" {
            // Root prim — re-insert finished, consumed below.
            prims.insert(
                path,
                PrimBuild {
                    finished: Some(prim),
                    ..PrimBuild::empty()
                },
            );
        } else if let Some(parent) = prims.get_mut(&parent_path) {
            parent.children.push(prim);
        } else {
            return Err(invalid(format!(
                "USDC layer: prim {path} has no parent prim spec at {parent_path}"
            )));
        }
    }

    // Roots in original spec order.
    let mut roots = Vec::new();
    for path in &prim_order {
        if let Some(build) = prims.remove(path) {
            if let Some(prim) = build.finished {
                roots.push(prim);
            }
        }
    }

    Ok(Layer {
        metadata: layer_metadata,
        prims: roots,
    })
}

/// A prim under construction.
struct PrimBuild {
    spec: String,
    type_name: String,
    metadata: BTreeMap<String, Value>,
    attrs: BTreeMap<String, Attr>,
    children: Vec<Prim>,
    /// The authored `primChildren` name order, used to sort `children`.
    child_order: Vec<String>,
    name: String,
    finished: Option<Prim>,
}

impl PrimBuild {
    fn empty() -> Self {
        PrimBuild {
            spec: String::new(),
            type_name: String::new(),
            metadata: BTreeMap::new(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
            child_order: Vec::new(),
            name: String::new(),
            finished: None,
        }
    }
}

fn build_prim(path: &str, spec: &NamedSpec, decoder: &ValueDecoder<'_>) -> Result<PrimBuild> {
    let name = path.rsplit('/').next().unwrap_or_default().to_owned();
    let mut build = PrimBuild::empty();
    build.name = name;
    build.spec = "def".to_owned();
    for (fname, rep) in &spec.fields {
        let rep = ValueRep::from_raw(*rep);
        match fname.as_str() {
            "specifier" => {
                let Value::Token(word) = decoder.decode(rep)? else {
                    return Err(invalid(format!(
                        "USDC layer: specifier field on {path} is not a specifier token"
                    )));
                };
                build.spec = word;
            }
            "typeName" => {
                let Value::Token(word) = decoder.decode(rep)? else {
                    return Err(invalid(format!(
                        "USDC layer: typeName field on {path} is not a token"
                    )));
                };
                build.type_name = word;
            }
            "primChildren" => {
                if let Value::Array(items) = decoder.decode(rep)? {
                    for item in items {
                        if let Value::Token(t) = item {
                            build.child_order.push(t);
                        }
                    }
                }
            }
            // The `properties` token vector orders the prim's own
            // attributes; `Prim::attrs` is a BTreeMap so statement
            // order is not modelled — nothing to carry.
            "properties" => {}
            other => {
                build
                    .metadata
                    .insert(other.to_owned(), decoder.decode(rep)?);
            }
        }
    }
    Ok(build)
}

fn finish_prim(mut build: PrimBuild) -> Prim {
    // Order children per the authored `primChildren` vector; names not
    // listed keep their walk order after the listed ones.
    if !build.child_order.is_empty() {
        let order: BTreeMap<&str, usize> = build
            .child_order
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        build
            .children
            .sort_by_key(|c| order.get(c.name.as_str()).copied().unwrap_or(usize::MAX));
    }
    Prim {
        spec: build.spec,
        type_name: build.type_name,
        name: build.name,
        metadata: build.metadata,
        attrs: build.attrs,
        children: build.children,
        variant_sets: BTreeMap::new(),
    }
}

/// Attach a §16.3.8.4.6 Attribute spec as the text-model statements it
/// corresponds to: the typed declaration with its default value, plus
/// `<name>.timeSamples` / `<name>.connect` companion statements when
/// those fields are authored.
fn attach_attribute(
    prim: &mut PrimBuild,
    attr_name: &str,
    spec: &NamedSpec,
    decoder: &ValueDecoder<'_>,
) -> Result<()> {
    let mut type_token = String::new();
    let mut custom = false;
    let mut uniform = false;
    let mut default = Value::None;
    let mut metadata: BTreeMap<String, Value> = BTreeMap::new();
    let mut time_samples: Option<Value> = None;
    let mut connect: Option<Value> = None;

    for (fname, rep) in &spec.fields {
        let rep = ValueRep::from_raw(*rep);
        match fname.as_str() {
            "typeName" => {
                if let Value::Token(t) = decoder.decode(rep)? {
                    type_token = t;
                }
            }
            "custom" => {
                if let Value::Bool(b) = decoder.decode(rep)? {
                    custom = b;
                }
            }
            "variability" => {
                uniform = decoder.decode(rep)? == Value::Token("uniform".to_owned());
            }
            "default" => default = decoder.decode(rep)?,
            "timeSamples" => time_samples = Some(decoder.decode(rep)?),
            "connectionPaths" => connect = Some(flatten_path_listop(decoder.decode(rep)?)),
            other => {
                metadata.insert(other.to_owned(), decoder.decode(rep)?);
            }
        }
    }

    // Fall back to the rep-derived spelling when the file authors no
    // typeName (rare; conforming exporters write it).
    if type_token.is_empty() {
        if let Some((_, rep)) = spec.fields.iter().find(|(n, _)| n == "default") {
            let rep = ValueRep::from_raw(*rep);
            if let Some(vt) = rep.value_type() {
                type_token = if rep.is_array() {
                    format!("{}[]", vt.name())
                } else {
                    vt.name().to_owned()
                };
            }
        }
    }
    let mut spelled = String::new();
    if custom {
        spelled.push_str("custom ");
    }
    if uniform {
        spelled.push_str("uniform ");
    }
    spelled.push_str(&type_token);

    prim.attrs.insert(
        attr_name.to_owned(),
        Attr {
            type_token: spelled.clone(),
            value: default,
            metadata,
        },
    );
    if let Some(ts) = time_samples {
        prim.attrs.insert(
            format!("{attr_name}.timeSamples"),
            Attr {
                type_token: spelled.clone(),
                value: ts,
                metadata: BTreeMap::new(),
            },
        );
    }
    if let Some(c) = connect {
        prim.attrs.insert(
            format!("{attr_name}.connect"),
            Attr {
                type_token: spelled,
                value: c,
                metadata: BTreeMap::new(),
            },
        );
    }
    Ok(())
}

/// Attach a §16.3.8.4.6 Relationship spec as a `rel` statement.
fn attach_relationship(
    prim: &mut PrimBuild,
    attr_name: &str,
    spec: &NamedSpec,
    decoder: &ValueDecoder<'_>,
) -> Result<()> {
    let mut targets = Value::None;
    let mut metadata: BTreeMap<String, Value> = BTreeMap::new();
    let mut custom = false;
    for (fname, rep) in &spec.fields {
        let rep = ValueRep::from_raw(*rep);
        match fname.as_str() {
            "targetPaths" => targets = flatten_path_listop(decoder.decode(rep)?),
            // Relationships are uniform by definition (§16.2.16.7);
            // the authored variability adds nothing to the text form.
            "variability" => {}
            "custom" => {
                if let Value::Bool(b) = decoder.decode(rep)? {
                    custom = b;
                }
            }
            other => {
                metadata.insert(other.to_owned(), decoder.decode(rep)?);
            }
        }
    }
    prim.attrs.insert(
        attr_name.to_owned(),
        Attr {
            type_token: if custom { "custom rel" } else { "rel" }.to_owned(),
            value: targets,
            metadata,
        },
    );
    Ok(())
}

/// Reduce a decoded Path list-op to the value shape the text parser
/// produces for the same statement: an explicit single target is a
/// bare `Value::Path`, an explicit multi-target list is an array, and
/// any operator-authored form keeps the full [`ListOp`] so composition
/// sees the operators.
fn flatten_path_listop(v: Value) -> Value {
    let Value::ListOp(op) = v else {
        return v;
    };
    let ListOp {
        prepended: None,
        appended: None,
        deleted: None,
        explicit: Some(explicit),
        reordered: None,
    } = &*op
    else {
        return Value::ListOp(op);
    };
    match explicit {
        Value::Array(items) if items.len() == 1 => items[0].clone(),
        other => other.clone(),
    }
}

/// `true` when the payload bytes are a Crate file (`PXR-USDC` magic) —
/// the dispatch a generic `.usd` extension needs (§16.1: the header
/// byte run, not the extension, decides text vs Crate).
pub fn is_usdc_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..8] == b"PXR-USDC"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usdc_magic_sniffs() {
        assert!(is_usdc_magic(b"PXR-USDC\x00\x08\x00"));
        assert!(!is_usdc_magic(b"#usda 1.0\n"));
        assert!(!is_usdc_magic(b"PXR-USD"));
    }

    #[test]
    fn flatten_path_listop_unwraps_explicit_single() {
        let op = ListOp::single(
            crate::usda::ListEditOp::Explicit,
            Value::Array(vec![Value::Path("/A".into())]),
        );
        assert_eq!(
            flatten_path_listop(Value::ListOp(Box::new(op))),
            Value::Path("/A".into())
        );
        // Operator-authored forms keep the ListOp.
        let op = ListOp::single(
            crate::usda::ListEditOp::Prepend,
            Value::Array(vec![Value::Path("/A".into())]),
        );
        let v = flatten_path_listop(Value::ListOp(Box::new(op)));
        assert!(matches!(v, Value::ListOp(_)));
    }
}
