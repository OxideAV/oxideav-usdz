//! `UsdGeomPointInstancer` — vectorised instancing on the typed model.
//!
//! Source: the staged schema digest `docs/3d/usd/usdgeom-usdshade-schema.md`
//! Part 2. One prim scatters many instances of a small ordered set
//! of prototypes; per-instance transform data lives in parallel
//! arrays (§2.2), composed in the exact order §2.3 prescribes, with
//! two masking mechanisms (§2.4).
//!
//! The typed `Scene3D` model has no instancer primitive, so the
//! instancer rides on its carrier node as a [`PointInstancer`]
//! record under `Node::extras[`[`EXTRAS_KEY`]`]` (JSON via
//! [`PointInstancer::to_json`] / [`PointInstancer::from_json`]),
//! while the prototype subtrees are ordinary child nodes of the
//! carrier. The decoder maps every §2.2 array — default value *and*
//! time samples — and the writer re-emits the prim symmetrically,
//! so a package round-trips as a fixed point without ever expanding.
//!
//! [`expand`] is the lossy lift onto plain scene-graph nodes: one
//! child node per (unmasked) instance carrying the §2.3 TRS, with a
//! copy of the prototype subtree underneath sharing the prototype's
//! `MeshId`s (geometry is instanced, not duplicated).

use std::collections::HashMap;

use oxideav_mesh3d::{Node, NodeId, Scene3D, Transform};
use serde_json::{Map as JMap, Value as JValue};

use crate::error::invalid;
use crate::usda::{Attr, Prim, Value};
use crate::Result;

/// `Node::extras` key carrying the JSON-encoded [`PointInstancer`].
pub const EXTRAS_KEY: &str = "usd:pointInstancer";

/// The prim schema token.
pub const TYPE_NAME: &str = "PointInstancer";

/// A per-instance array with its default value and/or time samples
/// (time in the layer's timeCodes, ascending).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sampled<T> {
    /// The attribute's default (time-independent) value.
    pub default: Option<T>,
    /// `.timeSamples`, in authored order.
    pub samples: Vec<(f64, T)>,
}

impl<T> Sampled<T> {
    /// `true` when neither a default nor any sample is authored.
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.samples.is_empty()
    }

    /// The value in force at `time`: the left-bracketing sample
    /// (held) when samples exist — the first sample before the
    /// first sample time — else the default. `None` time selects
    /// the default, falling back to the first sample.
    pub fn at(&self, time: Option<f64>) -> Option<&T> {
        match time {
            None => self
                .default
                .as_ref()
                .or(self.samples.first().map(|(_, v)| v)),
            Some(t) => {
                if self.samples.is_empty() {
                    return self.default.as_ref();
                }
                let mut best = &self.samples[0].1;
                for (st, v) in &self.samples {
                    if *st <= t {
                        best = v;
                    } else {
                        break;
                    }
                }
                Some(best)
            }
        }
    }

    /// Time of the left-bracketing sample at `time` (`None` when
    /// the default is in force).
    pub fn bracket_time(&self, time: Option<f64>) -> Option<f64> {
        let t = time?;
        let mut best = None;
        for (st, _) in &self.samples {
            if *st <= t {
                best = Some(*st);
            } else {
                break;
            }
        }
        best.or(self.samples.first().map(|(st, _)| *st))
    }
}

/// Precision of the authored `orientations` quaternions (§2.2:
/// `quath[]` half or `quatf[]` single).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrientationPrecision {
    /// `quath[] orientations` — the schema's primary spelling.
    #[default]
    Half,
    /// `quatf[] orientationsf`.
    Single,
}

/// The §2.1 / §2.2 / §2.4 surface of one `PointInstancer` prim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointInstancer {
    /// §2.1 `rel prototypes` — absolute prim paths, target order =
    /// prototype index.
    pub prototypes: Vec<String>,
    /// §2.2 `int[] protoIndices` (required; its length is the
    /// instance count).
    pub proto_indices: Sampled<Vec<u32>>,
    /// §2.2 `point3f[] positions`.
    pub positions: Sampled<Vec<[f32; 3]>>,
    /// §2.2 `orientations` / `orientationsf`, stored `xyzw`.
    pub orientations: Sampled<Vec<[f32; 4]>>,
    /// Which spelling `orientations` was authored in.
    pub orientation_precision: OrientationPrecision,
    /// §2.2 `float3[] scales`.
    pub scales: Sampled<Vec<[f32; 3]>>,
    /// §2.2 / §2.4 `int64[] ids`.
    pub ids: Sampled<Vec<i64>>,
    /// §2.4 `int64[] invisibleIds` (animatable mask).
    pub invisible_ids: Sampled<Vec<i64>>,
    /// §2.4 `inactiveIds` prim metadata (uniform, list-edited) —
    /// the effective list after list-edit resolution.
    pub inactive_ids: Vec<i64>,
    /// §2.2 `vector3f[] velocities`, units per second.
    pub velocities: Sampled<Vec<[f32; 3]>>,
    /// §2.2 `vector3f[] angularVelocities`, units per second.
    pub angular_velocities: Sampled<Vec<[f32; 3]>>,
    /// §2.2 `vector3f[] accelerations`.
    pub accelerations: Sampled<Vec<[f32; 3]>>,
}

impl PointInstancer {
    /// Number of instances — the length of `protoIndices` at `time`.
    pub fn instance_count(&self, time: Option<f64>) -> usize {
        self.proto_indices.at(time).map_or(0, Vec::len)
    }

    /// The instance's §2.4 identity: its `ids` entry when authored,
    /// else its array position.
    pub fn instance_id(&self, index: usize, time: Option<f64>) -> i64 {
        self.ids
            .at(time)
            .and_then(|ids| ids.get(index).copied())
            .unwrap_or(index as i64)
    }

    /// §2.4 effective mask: `true` when the instance is masked out by
    /// `inactiveIds` or by `invisibleIds` at `time`.
    pub fn is_masked(&self, index: usize, time: Option<f64>) -> bool {
        let id = self.instance_id(index, time);
        self.inactive_ids.contains(&id)
            || self
                .invisible_ids
                .at(time)
                .is_some_and(|inv| inv.contains(&id))
    }

    // ---- prim <-> record -------------------------------------------

    /// Read the record off a parsed `PointInstancer` prim.
    pub fn from_prim(prim: &Prim) -> Result<Self> {
        let mut out = Self {
            prototypes: prim
                .attrs
                .get("prototypes")
                .map(|a| rel_targets(&a.value))
                .unwrap_or_default(),
            ..Self::default()
        };
        out.proto_indices = read_sampled(prim, "protoIndices", read_ints)?;
        if out.proto_indices.is_empty() {
            return Err(invalid(format!(
                "PointInstancer `{}` has no `protoIndices` (the required per-instance array)",
                prim.name
            )));
        }
        out.positions = read_sampled(prim, "positions", read_vec3s)?;
        if prim.attrs.contains_key("orientationsf")
            || prim.attrs.contains_key("orientationsf.timeSamples")
        {
            out.orientation_precision = OrientationPrecision::Single;
            out.orientations = read_sampled(prim, "orientationsf", read_quats)?;
        } else {
            out.orientation_precision = OrientationPrecision::Half;
            out.orientations = read_sampled(prim, "orientations", read_quats)?;
        }
        out.scales = read_sampled(prim, "scales", read_vec3s)?;
        out.ids = read_sampled(prim, "ids", read_i64s)?;
        out.invisible_ids = read_sampled(prim, "invisibleIds", read_i64s)?;
        out.velocities = read_sampled(prim, "velocities", read_vec3s)?;
        out.angular_velocities = read_sampled(prim, "angularVelocities", read_vec3s)?;
        out.accelerations = read_sampled(prim, "accelerations", read_vec3s)?;
        out.inactive_ids = prim
            .metadata
            .get("inactiveIds")
            .map(resolve_int_list)
            .unwrap_or_default();
        // Parallel-array contract (§2.2): every authored default array
        // must match the instance count.
        let n = out.proto_indices.default.as_ref().map(Vec::len);
        if let Some(n) = n {
            let check = |name: &str, len: Option<usize>| -> Result<()> {
                match len {
                    Some(l) if l != n => Err(invalid(format!(
                        "PointInstancer `{}`: `{name}` has {l} entries for {n} instances",
                        prim.name
                    ))),
                    _ => Ok(()),
                }
            };
            check("positions", out.positions.default.as_ref().map(Vec::len))?;
            check(
                "orientations",
                out.orientations.default.as_ref().map(Vec::len),
            )?;
            check("scales", out.scales.default.as_ref().map(Vec::len))?;
            check("ids", out.ids.default.as_ref().map(Vec::len))?;
            check("velocities", out.velocities.default.as_ref().map(Vec::len))?;
            check(
                "angularVelocities",
                out.angular_velocities.default.as_ref().map(Vec::len),
            )?;
            check(
                "accelerations",
                out.accelerations.default.as_ref().map(Vec::len),
            )?;
        }
        Ok(out)
    }

    /// The prim-body statements the writer emits for this record,
    /// in a deterministic order (relationship first, then the §2.2
    /// arrays; each array's default then its `.timeSamples`).
    pub fn to_usda_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.prototypes.is_empty() {
            let targets: Vec<String> = self.prototypes.iter().map(|p| format!("<{p}>")).collect();
            out.push(if targets.len() == 1 {
                format!("rel prototypes = {}", targets[0])
            } else {
                format!("rel prototypes = [{}]", targets.join(", "))
            });
        }
        emit_sampled(
            &mut out,
            "int[]",
            "protoIndices",
            &self.proto_indices,
            |v| fmt_ints(v),
        );
        emit_sampled(&mut out, "point3f[]", "positions", &self.positions, |v| {
            fmt_vec3s(v)
        });
        match self.orientation_precision {
            OrientationPrecision::Half => emit_sampled(
                &mut out,
                "quath[]",
                "orientations",
                &self.orientations,
                |v| fmt_quats(v),
            ),
            OrientationPrecision::Single => emit_sampled(
                &mut out,
                "quatf[]",
                "orientationsf",
                &self.orientations,
                |v| fmt_quats(v),
            ),
        }
        emit_sampled(&mut out, "float3[]", "scales", &self.scales, |v| {
            fmt_vec3s(v)
        });
        emit_sampled(&mut out, "int64[]", "ids", &self.ids, |v| fmt_i64s(v));
        emit_sampled(
            &mut out,
            "int64[]",
            "invisibleIds",
            &self.invisible_ids,
            |v| fmt_i64s(v),
        );
        emit_sampled(
            &mut out,
            "vector3f[]",
            "velocities",
            &self.velocities,
            |v| fmt_vec3s(v),
        );
        emit_sampled(
            &mut out,
            "vector3f[]",
            "angularVelocities",
            &self.angular_velocities,
            |v| fmt_vec3s(v),
        );
        emit_sampled(
            &mut out,
            "vector3f[]",
            "accelerations",
            &self.accelerations,
            |v| fmt_vec3s(v),
        );
        out
    }

    // ---- JSON --------------------------------------------------------

    /// Serialise for `Node::extras`.
    pub fn to_json(&self) -> JValue {
        let mut o = JMap::new();
        o.insert(
            "prototypes".into(),
            JValue::Array(
                self.prototypes
                    .iter()
                    .map(|p| JValue::String(p.clone()))
                    .collect(),
            ),
        );
        o.insert(
            "protoIndices".into(),
            sampled_to_json(&self.proto_indices, |v| {
                JValue::Array(v.iter().map(|&i| JValue::from(i)).collect())
            }),
        );
        let vec3s = |v: &Vec<[f32; 3]>| JValue::Array(v.iter().map(f32s_json).collect());
        let vec4s = |v: &Vec<[f32; 4]>| JValue::Array(v.iter().map(f32s_json).collect());
        let i64s = |v: &Vec<i64>| JValue::Array(v.iter().map(|&i| JValue::from(i)).collect());
        o.insert("positions".into(), sampled_to_json(&self.positions, vec3s));
        o.insert(
            "orientations".into(),
            sampled_to_json(&self.orientations, vec4s),
        );
        o.insert(
            "orientationPrecision".into(),
            JValue::String(
                match self.orientation_precision {
                    OrientationPrecision::Half => "half",
                    OrientationPrecision::Single => "single",
                }
                .into(),
            ),
        );
        o.insert("scales".into(), sampled_to_json(&self.scales, vec3s));
        o.insert("ids".into(), sampled_to_json(&self.ids, i64s));
        o.insert(
            "invisibleIds".into(),
            sampled_to_json(&self.invisible_ids, i64s),
        );
        o.insert(
            "inactiveIds".into(),
            JValue::Array(self.inactive_ids.iter().map(|&i| JValue::from(i)).collect()),
        );
        o.insert(
            "velocities".into(),
            sampled_to_json(&self.velocities, vec3s),
        );
        o.insert(
            "angularVelocities".into(),
            sampled_to_json(&self.angular_velocities, vec3s),
        );
        o.insert(
            "accelerations".into(),
            sampled_to_json(&self.accelerations, vec3s),
        );
        JValue::Object(o)
    }

    /// Inverse of [`Self::to_json`]; malformed input yields an empty
    /// record (zero instances).
    pub fn from_json(v: &JValue) -> Self {
        let Some(o) = v.as_object() else {
            return Self::default();
        };
        let ints = |v: &JValue| -> Option<Vec<u32>> {
            v.as_array()?
                .iter()
                .map(|x| x.as_u64().map(|i| i as u32))
                .collect()
        };
        let i64s =
            |v: &JValue| -> Option<Vec<i64>> { v.as_array()?.iter().map(JValue::as_i64).collect() };
        let vec3s = |v: &JValue| -> Option<Vec<[f32; 3]>> {
            v.as_array()?.iter().map(json_f32s::<3>).collect()
        };
        let vec4s = |v: &JValue| -> Option<Vec<[f32; 4]>> {
            v.as_array()?.iter().map(json_f32s::<4>).collect()
        };
        let get = |k: &str| o.get(k).unwrap_or(&JValue::Null);
        Self {
            prototypes: get("prototypes")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            proto_indices: sampled_from_json(get("protoIndices"), ints),
            positions: sampled_from_json(get("positions"), vec3s),
            orientations: sampled_from_json(get("orientations"), vec4s),
            orientation_precision: match get("orientationPrecision").as_str() {
                Some("single") => OrientationPrecision::Single,
                _ => OrientationPrecision::Half,
            },
            scales: sampled_from_json(get("scales"), vec3s),
            ids: sampled_from_json(get("ids"), i64s),
            invisible_ids: sampled_from_json(get("invisibleIds"), i64s),
            inactive_ids: i64s(get("inactiveIds")).unwrap_or_default(),
            velocities: sampled_from_json(get("velocities"), vec3s),
            angular_velocities: sampled_from_json(get("angularVelocities"), vec3s),
            accelerations: sampled_from_json(get("accelerations"), vec3s),
        }
    }

    /// Read the record off a node's extras, if the node carries one.
    pub fn from_node(node: &Node) -> Option<Self> {
        node.extras.get(EXTRAS_KEY).map(Self::from_json)
    }

    // ---- §2.3 evaluation --------------------------------------------

    /// The instance's local transform (steps 2–4 of §2.3: scale,
    /// rotation, translation) at `time`; the prototype's own root
    /// transform (step 1) and the instancer's (step 5) are the
    /// surrounding nodes'.
    ///
    /// Velocity handling per §2.3: when `velocities` is authored the
    /// translation is `positions[left] + velocities[left] · Δt`,
    /// with Δt measured from the left-bracketing `positions` sample
    /// in seconds (`1 / timeCodesPerSecond` scaling); when neither
    /// velocity attribute is authored and the bracketing samples
    /// have equal length, positions / orientations interpolate
    /// linearly. `angularVelocities` are carried but not applied —
    /// the staged digest does not state their angular unit (see the
    /// crate README's docs-asks).
    pub fn instance_transform(
        &self,
        index: usize,
        time: Option<f64>,
        time_codes_per_second: f64,
    ) -> Transform {
        let scale = self
            .scales
            .at(time)
            .and_then(|s| s.get(index).copied())
            .unwrap_or([1.0; 3]);
        let velocity_authored = !self.velocities.is_empty() || !self.angular_velocities.is_empty();
        let rotation = if velocity_authored || time.is_none() {
            self.orientations
                .at(time)
                .and_then(|q| q.get(index).copied())
                .unwrap_or([0.0, 0.0, 0.0, 1.0])
        } else {
            interpolate(
                &self.orientations,
                index,
                time.unwrap_or(0.0),
                [0.0, 0.0, 0.0, 1.0],
            )
        };
        let mut translation = if velocity_authored || time.is_none() {
            self.positions
                .at(time)
                .and_then(|p| p.get(index).copied())
                .unwrap_or([0.0; 3])
        } else {
            interpolate(&self.positions, index, time.unwrap_or(0.0), [0.0; 3])
        };
        if let (Some(t), Some(vel)) = (
            time,
            self.velocities.at(time).and_then(|v| v.get(index).copied()),
        ) {
            let left = self.positions.bracket_time(Some(t)).unwrap_or(t);
            let tcps = if time_codes_per_second > 0.0 {
                time_codes_per_second
            } else {
                24.0
            };
            let dt = ((t - left) / tcps) as f32;
            for k in 0..3 {
                translation[k] += vel[k] * dt;
            }
        }
        Transform::Trs {
            translation,
            rotation,
            scale,
        }
    }
}

/// Linear interpolation between the bracketing samples when both
/// have equal length (§2.3); the left sample (held) otherwise.
fn interpolate<const N: usize>(
    s: &Sampled<Vec<[f32; N]>>,
    index: usize,
    t: f64,
    fallback: [f32; N],
) -> [f32; N] {
    if s.samples.is_empty() {
        return s
            .default
            .as_ref()
            .and_then(|v| v.get(index).copied())
            .unwrap_or(fallback);
    }
    let mut left = 0usize;
    for (i, (st, _)) in s.samples.iter().enumerate() {
        if *st <= t {
            left = i;
        } else {
            break;
        }
    }
    let (lt, lv) = &s.samples[left];
    let lval = lv.get(index).copied().unwrap_or(fallback);
    let Some((rt, rv)) = s.samples.get(left + 1) else {
        return lval;
    };
    if t < *lt || rv.len() != lv.len() || rt <= lt {
        return lval;
    }
    let rval = rv.get(index).copied().unwrap_or(fallback);
    let f = ((t - lt) / (rt - lt)) as f32;
    let mut out = [0f32; N];
    for k in 0..N {
        out[k] = lval[k] + (rval[k] - lval[k]) * f;
    }
    if N == 4 {
        // Rotations: renormalise the lerped quaternion.
        let n = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            out.iter_mut().for_each(|x| *x /= n);
        }
    }
    out
}

/// Options for [`expand`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExpandOptions {
    /// Evaluation time in the layer's timeCodes; `None` evaluates
    /// the default values.
    pub time: Option<f64>,
    /// The layer's `timeCodesPerSecond` (§2.3 velocity time-scaling).
    pub time_codes_per_second: f64,
}

impl Default for ExpandOptions {
    fn default() -> Self {
        Self {
            time: None,
            time_codes_per_second: 24.0,
        }
    }
}

/// Expand the `PointInstancer` carried by `instancer` into plain
/// scene-graph nodes: one child node per unmasked instance, named
/// `<instancer>_inst<index>`, carrying the §2.3 TRS and a copy of
/// the prototype subtree (nodes duplicated, `MeshId`s shared). The
/// prototype roots are detached from the carrier's children (they
/// were templates, not scene content), the record is removed and
/// the carrier becomes an ordinary `Xform`, so a subsequent encode
/// writes the expanded geometry. Returns the new instance node ids.
///
/// This is a lossy flatten — like composition flattening — of an
/// otherwise round-trip-stable representation.
pub fn expand(scene: &mut Scene3D, instancer: NodeId, opts: ExpandOptions) -> Result<Vec<NodeId>> {
    let node = scene
        .node(instancer)
        .ok_or_else(|| invalid("PointInstancer expand: unknown node id"))?;
    let record = PointInstancer::from_node(node).ok_or_else(|| {
        invalid("PointInstancer expand: node carries no `usd:pointInstancer` record")
    })?;
    let carrier_name = node.name.clone().unwrap_or_else(|| "PointInstancer".into());
    // Prototype path → node id, resolved over the whole graph (§2.1:
    // any prim may be a prototype).
    let paths = node_paths(scene);
    let mut proto_ids = Vec::with_capacity(record.prototypes.len());
    for p in &record.prototypes {
        let id = paths.get(p).copied().ok_or_else(|| {
            invalid(format!(
                "PointInstancer `{carrier_name}`: prototype `{p}` names no node in the scene"
            ))
        })?;
        proto_ids.push(id);
    }
    let time = opts.time;
    let Some(indices) = record.proto_indices.at(time).cloned() else {
        return Ok(Vec::new());
    };
    let mut new_ids = Vec::new();
    for (i, &pi) in indices.iter().enumerate() {
        if record.is_masked(i, time) {
            continue;
        }
        let Some(&proto) = proto_ids.get(pi as usize) else {
            return Err(invalid(format!(
                "PointInstancer `{carrier_name}`: protoIndices[{i}] = {pi} but only {} prototypes",
                proto_ids.len()
            )));
        };
        let copy = clone_subtree(scene, proto);
        let mut inst = Node::new()
            .with_name(format!("{carrier_name}_inst{i}"))
            .with_transform(record.instance_transform(i, time, opts.time_codes_per_second));
        inst.children = vec![copy];
        let mut meta = JMap::new();
        meta.insert("index".into(), JValue::from(i as u64));
        meta.insert("id".into(), JValue::from(record.instance_id(i, time)));
        meta.insert(
            "prototype".into(),
            JValue::String(record.prototypes[pi as usize].clone()),
        );
        inst.extras
            .insert("usd:instance".into(), JValue::Object(meta));
        new_ids.push(scene.add_node(inst));
    }
    // Rewire the carrier: detach the prototype roots from wherever
    // they live (§2.1: any prim may be a prototype — typically a
    // `Prototypes` scope under the instancer), prune the container
    // chain they leave empty beneath the instancer, attach the
    // instances, drop the record.
    let parents = parent_map(scene);
    for &proto in &proto_ids {
        let mut child = proto;
        let mut parent = parents.get(&child).copied();
        while let Some(p) = parent {
            let Some(pn) = scene.nodes.get_mut(p.0 as usize) else {
                break;
            };
            pn.children.retain(|&c| c != child);
            let empty = pn.children.is_empty()
                && pn.mesh.is_none()
                && pn.camera.is_none()
                && pn.light.is_none()
                && pn.audio_emitter.is_none();
            if p == instancer || !empty {
                break;
            }
            child = p;
            parent = parents.get(&child).copied();
        }
    }
    let node = scene
        .nodes
        .get_mut(instancer.0 as usize)
        .expect("checked above");
    node.children.extend(new_ids.iter().copied());
    node.extras.remove(EXTRAS_KEY);
    node.extras.remove("usd:type");
    node.extras.insert(
        "usd:pointInstancer:expanded".into(),
        JValue::from(new_ids.len() as u64),
    );
    Ok(new_ids)
}

/// Parent of every node reachable from the roots.
fn parent_map(scene: &Scene3D) -> HashMap<NodeId, NodeId> {
    fn walk(scene: &Scene3D, id: NodeId, out: &mut HashMap<NodeId, NodeId>) {
        let Some(node) = scene.node(id) else { return };
        for &c in &node.children {
            out.entry(c).or_insert(id);
            walk(scene, c, out);
        }
    }
    let mut out = HashMap::new();
    for &r in &scene.roots {
        walk(scene, r, &mut out);
    }
    out
}

/// Absolute prim path (`/A/B`) of every node reachable from the
/// roots, built from node names (unnamed nodes use `node_<id>` —
/// the writer's convention).
fn node_paths(scene: &Scene3D) -> HashMap<String, NodeId> {
    fn walk(scene: &Scene3D, id: NodeId, parent: &str, out: &mut HashMap<String, NodeId>) {
        let Some(node) = scene.node(id) else { return };
        let name = node
            .name
            .clone()
            .unwrap_or_else(|| format!("node_{}", id.0));
        let path = format!("{parent}/{name}");
        for &c in &node.children {
            walk(scene, c, &path, out);
        }
        out.entry(path).or_insert(id);
    }
    let mut out = HashMap::new();
    for &r in &scene.roots {
        walk(scene, r, "", &mut out);
    }
    out
}

/// Duplicate a node subtree (new `Node`s, shared mesh / skin /
/// camera / light / emitter ids).
fn clone_subtree(scene: &mut Scene3D, id: NodeId) -> NodeId {
    let Some(src) = scene.node(id).cloned() else {
        return id;
    };
    let mut copy = src.clone();
    copy.children = src
        .children
        .iter()
        .map(|&c| clone_subtree(scene, c))
        .collect();
    scene.add_node(copy)
}

// ---- readers ---------------------------------------------------------

fn read_sampled<T>(
    prim: &Prim,
    name: &str,
    read: impl Fn(&Value) -> Option<T>,
) -> Result<Sampled<T>> {
    let mut out = Sampled {
        default: None,
        samples: Vec::new(),
    };
    if let Some(attr) = prim.attrs.get(name) {
        if !matches!(attr.value, Value::None) {
            out.default = Some(read(&attr.value).ok_or_else(|| {
                invalid(format!(
                    "PointInstancer `{}`: `{name}` is not a well-formed array",
                    prim.name
                ))
            })?);
        }
    }
    if let Some(Attr {
        value: Value::TimeSamples(samples),
        ..
    }) = prim.attrs.get(&format!("{name}.timeSamples"))
    {
        for (t, v) in samples {
            let value = read(v).ok_or_else(|| {
                invalid(format!(
                    "PointInstancer `{}`: `{name}.timeSamples[{t}]` is not a well-formed array",
                    prim.name
                ))
            })?;
            out.samples.push((*t, value));
        }
    }
    Ok(out)
}

fn read_ints(v: &Value) -> Option<Vec<u32>> {
    v.as_seq()?
        .iter()
        .map(|x| {
            let f = x.as_f32()?;
            (f >= 0.0 && f == f.trunc()).then_some(f as u32)
        })
        .collect()
}

fn read_i64s(v: &Value) -> Option<Vec<i64>> {
    v.as_seq()?
        .iter()
        .map(|x| match x {
            Value::Float(f) if *f == f.trunc() => Some(*f as i64),
            _ => None,
        })
        .collect()
}

fn read_vec3s(v: &Value) -> Option<Vec<[f32; 3]>> {
    v.as_seq()?.iter().map(|x| x.as_floatn::<3>()).collect()
}

/// USDA quaternion literal `(real, i, j, k)` → `xyzw`.
fn read_quats(v: &Value) -> Option<Vec<[f32; 4]>> {
    v.as_seq()?
        .iter()
        .map(|x| x.as_floatn::<4>().map(|w| [w[1], w[2], w[3], w[0]]))
        .collect()
}

fn rel_targets(v: &Value) -> Vec<String> {
    match v {
        Value::Path(p) => vec![p.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|x| match x {
                Value::Path(p) => Some(p.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Resolve a (possibly list-edited) integer-list metadata value to
/// its effective items: additive sublists in strength order, deleted
/// entries removed.
fn resolve_int_list(v: &Value) -> Vec<i64> {
    fn items(v: &Value) -> Vec<i64> {
        match v {
            Value::Array(seq) => seq.iter().filter_map(one).collect(),
            other => one(other).into_iter().collect(),
        }
    }
    fn one(v: &Value) -> Option<i64> {
        match v {
            Value::Float(f) if *f == f.trunc() => Some(*f as i64),
            _ => None,
        }
    }
    match v {
        Value::ListOp(list) => {
            let mut out: Vec<i64> = Vec::new();
            for (_, sub) in list.additive_in_strength_order() {
                for i in items(sub) {
                    if !out.contains(&i) {
                        out.push(i);
                    }
                }
            }
            if let Some(del) = &list.deleted {
                let deleted = items(del);
                out.retain(|i| !deleted.contains(i));
            }
            out
        }
        other => items(other),
    }
}

// ---- writers ---------------------------------------------------------

fn emit_sampled<T>(
    out: &mut Vec<String>,
    type_token: &str,
    name: &str,
    s: &Sampled<T>,
    fmt: impl Fn(&T) -> String,
) {
    if let Some(d) = &s.default {
        out.push(format!("{type_token} {name} = {}", fmt(d)));
    }
    if !s.samples.is_empty() {
        let body: Vec<String> = s
            .samples
            .iter()
            .map(|(t, v)| format!("{}: {}", fmt_f(*t), fmt(v)))
            .collect();
        out.push(format!(
            "{type_token} {name}.timeSamples = {{ {} }}",
            body.join(", ")
        ));
    }
}

fn fmt_f(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.into();
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{}", f as i64);
    }
    let s = format!("{f:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn fmt_ints(v: &[u32]) -> String {
    let items: Vec<String> = v.iter().map(u32::to_string).collect();
    format!("[{}]", items.join(", "))
}

fn fmt_i64s(v: &[i64]) -> String {
    let items: Vec<String> = v.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(", "))
}

fn fmt_vec3s(v: &[[f32; 3]]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|p| {
            format!(
                "({}, {}, {})",
                fmt_f(p[0] as f64),
                fmt_f(p[1] as f64),
                fmt_f(p[2] as f64)
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

/// `xyzw` → USDA `(real, i, j, k)`.
fn fmt_quats(v: &[[f32; 4]]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|q| {
            format!(
                "({}, {}, {}, {})",
                fmt_f(q[3] as f64),
                fmt_f(q[0] as f64),
                fmt_f(q[1] as f64),
                fmt_f(q[2] as f64)
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

// ---- JSON helpers ------------------------------------------------------

fn sampled_to_json<T>(s: &Sampled<T>, f: impl Fn(&T) -> JValue) -> JValue {
    let mut o = JMap::new();
    if let Some(d) = &s.default {
        o.insert("default".into(), f(d));
    }
    if !s.samples.is_empty() {
        o.insert(
            "samples".into(),
            JValue::Array(
                s.samples
                    .iter()
                    .map(|(t, v)| JValue::Array(vec![JValue::from(*t), f(v)]))
                    .collect(),
            ),
        );
    }
    JValue::Object(o)
}

fn sampled_from_json<T>(v: &JValue, f: impl Fn(&JValue) -> Option<T>) -> Sampled<T> {
    let Some(o) = v.as_object() else {
        return Sampled {
            default: None,
            samples: Vec::new(),
        };
    };
    Sampled {
        default: o.get("default").and_then(&f),
        samples: o
            .get("samples")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|pair| {
                        let p = pair.as_array()?;
                        Some((p.first()?.as_f64()?, f(p.get(1)?)?))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn f32s_json<const N: usize>(x: &[f32; N]) -> JValue {
    JValue::Array(x.iter().map(|&f| JValue::from(f as f64)).collect())
}

fn json_f32s<const N: usize>(v: &JValue) -> Option<[f32; N]> {
    let arr = v.as_array()?;
    if arr.len() != N {
        return None;
    }
    let mut out = [0f32; N];
    for (i, x) in arr.iter().enumerate() {
        out[i] = x.as_f64()? as f32;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prim(body: &str) -> Prim {
        let src = format!("#usda 1.0\ndef PointInstancer \"PI\" {{\n{body}\n}}\n");
        crate::usda::parse(src.as_bytes()).unwrap().prims.remove(0)
    }

    #[test]
    fn reads_arrays_and_round_trips_json() {
        let p = prim(
            r#"    rel prototypes = [</PI/Protos/A>, </PI/Protos/B>]
    int[] protoIndices = [0, 1, 0]
    point3f[] positions = [(0,0,0), (1,0,0), (2,0,0)]
    quath[] orientations = [(1,0,0,0), (0,0,1,0), (1,0,0,0)]
    float3[] scales = [(1,1,1), (2,2,2), (3,3,3)]
    int64[] ids = [10, 11, 12]
    int64[] invisibleIds = [11]
    point3f[] positions.timeSamples = { 0: [(0,0,0), (1,0,0), (2,0,0)], 10: [(0,1,0), (1,1,0), (2,1,0)] }
"#,
        );
        let pi = PointInstancer::from_prim(&p).unwrap();
        assert_eq!(pi.prototypes.len(), 2);
        assert_eq!(pi.instance_count(None), 3);
        assert_eq!(pi.positions.samples.len(), 2);
        // USDA `(real, i, j, k)` → stored xyzw.
        assert_eq!(
            pi.orientations.default.as_ref().unwrap()[1],
            [0.0, 1.0, 0.0, 0.0]
        );
        assert!(pi.is_masked(1, None), "id 11 is invisible");
        assert!(!pi.is_masked(0, None));
        let back = PointInstancer::from_json(&pi.to_json());
        assert_eq!(back, pi);
        // Writer lines re-parse to the same record.
        let lines = pi.to_usda_lines().join("\n");
        let again = PointInstancer::from_prim(&prim(&lines)).unwrap();
        assert_eq!(again, pi);
    }

    #[test]
    fn missing_proto_indices_is_an_error() {
        let p = prim("    rel prototypes = </X>\n");
        assert!(PointInstancer::from_prim(&p).is_err());
    }

    #[test]
    fn parallel_array_length_mismatch_is_an_error() {
        let p = prim("    int[] protoIndices = [0, 0]\n    point3f[] positions = [(0,0,0)]\n");
        let err = PointInstancer::from_prim(&p).unwrap_err();
        assert!(err.to_string().contains("positions"));
    }

    #[test]
    fn inactive_ids_list_edit_resolves() {
        let src = "#usda 1.0\ndef PointInstancer \"PI\" (\n    inactiveIds = [1, 2, 3]\n    delete inactiveIds = [2]\n) {\n    int[] protoIndices = [0, 0, 0, 0]\n}\n";
        let p = crate::usda::parse(src.as_bytes()).unwrap().prims.remove(0);
        let pi = PointInstancer::from_prim(&p).unwrap();
        assert_eq!(pi.inactive_ids, vec![1, 3]);
        // No `ids` → identity is array position.
        assert!(pi.is_masked(1, None));
        assert!(!pi.is_masked(2, None));
    }

    #[test]
    fn sampled_held_and_interpolated() {
        let s = Sampled {
            default: Some(vec![[9.0f32, 9.0, 9.0]]),
            samples: vec![
                (0.0, vec![[0.0f32, 0.0, 0.0]]),
                (10.0, vec![[10.0f32, 0.0, 0.0]]),
            ],
        };
        assert_eq!(s.at(None), Some(&vec![[9.0, 9.0, 9.0]]));
        assert_eq!(s.at(Some(-1.0)), Some(&vec![[0.0, 0.0, 0.0]]));
        assert_eq!(s.at(Some(5.0)), Some(&vec![[0.0, 0.0, 0.0]]));
        assert_eq!(s.at(Some(10.0)), Some(&vec![[10.0, 0.0, 0.0]]));
        assert_eq!(interpolate(&s, 0, 5.0, [0.0; 3]), [5.0, 0.0, 0.0]);
        assert_eq!(interpolate(&s, 0, 20.0, [0.0; 3]), [10.0, 0.0, 0.0]);
    }

    #[test]
    fn velocity_suppresses_interpolation_and_scales_by_tcps() {
        let mut pi = PointInstancer {
            proto_indices: Sampled {
                default: Some(vec![0]),
                samples: Vec::new(),
            },
            ..Default::default()
        };
        pi.positions.samples = vec![
            (0.0, vec![[0.0, 0.0, 0.0]]),
            (24.0, vec![[100.0, 0.0, 0.0]]),
        ];
        pi.velocities.default = Some(vec![[48.0, 0.0, 0.0]]);
        // t = 12 timeCodes at 24 tcps = 0.5 s → 0 + 48 * 0.5 = 24,
        // not the interpolated 50.
        let t = pi.instance_transform(0, Some(12.0), 24.0);
        assert!(
            matches!(t, Transform::Trs { translation, .. } if (translation[0] - 24.0).abs() < 1e-5)
        );
        pi.velocities = Sampled::default();
        let t = pi.instance_transform(0, Some(12.0), 24.0);
        assert!(
            matches!(t, Transform::Trs { translation, .. } if (translation[0] - 50.0).abs() < 1e-5)
        );
    }
}
