//! Point selection and removal: Delete (by group, region or attribute),
//! Subset (every Nth, a random fraction, a range), Group (writes a group
//! bit from any of those rules) and Age (ages points and removes the dead).
//!
//! Removal is stream compaction (design D7): a kernel writes a keep flag per
//! point, an integer prefix sum turns the flags into an index map in the
//! original order, and every point attribute is gathered through it. No
//! atomics decide where a point goes, so the result is deterministic. The
//! live count stays on the GPU (the result is GPU-counted, its capacity the
//! input's point count), so nothing waits for a readback.
//!
//! Per-point expressions ("delete where `@P.y > sin(@id)`") wait for the
//! Kernel DSL (A12); the attribute comparison covers the common cases.

use lsr_core::geo::{AttrClass, AttrType, names};
use lsr_core::registry::{OperatorSchema, ParamSchema};
use lsr_gpu::engine::{GeoCtx, GeoOp, Params};
use lsr_gpu::geo::kernels::{Bind, Compactor, Gather, GatherMap, Packer, Work, pass};
use lsr_gpu::geo::{GeoBuffer, Geometry};

use crate::kit::{self, Missing, PointKernel, menu_index, typed_access, vec3};

/// Rules a selection can use (the uniform's `mode`).
const RULES: &[&str] = &[
    "group",
    "box",
    "sphere",
    "attribute",
    "random",
    "every_nth",
    "range",
];
const DELETE_RULES: &[&str] = &["group", "box", "sphere", "attribute"];
const SUBSET_RULES: &[&str] = &["every_nth", "random", "range"];
const COMPARE: &[&str] = &["<", "<=", ">", ">=", "==", "!="];
const GROUP_OPS: &[&str] = &["replace", "union", "intersect", "subtract"];

/// The selection rule in WGSL: `fn selected(i: u32) -> bool`, over `P`,
/// `X` (an attribute as raw words), `ID` and `gmask(i)` (the group mask,
/// supplied by the operator).
const SELECT: &str = r#"
struct Params {
    // box/sphere centre; w: sphere radius
    centre: vec4f,
    // box half size
    half: vec4f,
    // x: compare value, y: random fraction
    values: vec4f,
    // attribute descriptor (components, stride, kind)
    desc: vec4u,
    mode: u32,
    gbit: u32,
    seed: u32,
    // bit 0: key by `id`; bit 1: invert; bit 2: an `id` is bound
    flags: u32,
    cmp: u32,
    comp: u32,
    nth: u32,
    first: u32,
    count: u32,
    offset: u32,
    // Group: the operation and the bit written
    op: u32,
    tbit: u32,
};

fn key(i: u32) -> u32 {
    if ((params.flags & 5u) == 5u) { return ID[i]; }
    return i;
}

fn selected(i: u32) -> bool {
    var s = false;
    switch params.mode {
        case 0u: { s = (gmask(i) & (1u << params.gbit)) != 0u; }
        case 1u: {
            let d = abs(P[i].xyz - params.centre.xyz);
            s = all(d <= params.half.xyz);
        }
        case 2u: { s = length(P[i].xyz - params.centre.xyz) <= params.centre.w; }
        case 3u: {
            let x = ld_X(i)[min(params.comp, 3u)];
            let y = params.values.x;
            switch params.cmp {
                case 0u: { s = x < y; }
                case 1u: { s = x <= y; }
                case 2u: { s = x > y; }
                case 3u: { s = x >= y; }
                case 4u: { s = x == y; }
                default: { s = x != y; }
            }
        }
        case 4u: { s = lsr_rand(params.seed, key(i)) < params.values.y; }
        case 5u: { s = (key(i) + params.offset) % max(params.nth, 1u) == 0u; }
        default: { s = key(i) >= params.first && key(i) - params.first < params.count; }
    }
    if ((params.flags & 2u) != 0u) { s = !s; }
    return s;
}
"#;

fn select_decls() -> String {
    format!("{SELECT}{}", typed_access("X", "params.desc", true))
}

/// Bindings 3-6 of every selection kernel (after the output at 2).
const SELECT_BINDINGS: &str = "@group(0) @binding(3) var<storage, read> P: array<vec4f>;
@group(0) @binding(4) var<storage, read> X: array<u32>;
@group(0) @binding(5) var<storage, read> ID: array<u32>;
";

/// Packs a selection's uniforms (112 bytes).
struct Rule<'a> {
    p: &'a Params<'a>,
    mode: u32,
    gbit: u32,
    desc: [u32; 4],
    has_id: bool,
    invert: bool,
    op: u32,
    /// Group: the bit written.
    target: u32,
}

impl Rule<'_> {
    fn pack(&self) -> Packer<112> {
        let p = self.p;
        let c = vec3(p, "centre");
        let s = vec3(p, "size");
        let mut u = Packer::<112>::new();
        let mut flags = 0u32;
        if p.bool("by_id") {
            flags |= 1;
        }
        if self.invert {
            flags |= 2;
        }
        if self.has_id {
            flags |= 4;
        }
        u.vec4([c[0], c[1], c[2], p.f32("radius").max(0.0)])
            .vec4([s[0] * 0.5, s[1] * 0.5, s[2] * 0.5, 0.0])
            .vec4([p.f32("value"), p.f32("fraction").clamp(0.0, 1.0), 0.0, 0.0]);
        for d in self.desc {
            u.u32(d);
        }
        u.u32(self.mode)
            .u32(self.gbit)
            .u32(kit::seed(p))
            .u32(flags)
            .u32(menu_index(COMPARE, p.str("compare")))
            .u32(p.i64("component").clamp(0, 3) as u32)
            .u32(p.i64("nth").clamp(1, i64::from(u32::MAX)) as u32)
            .u32(p.i64("first").clamp(0, i64::from(u32::MAX)) as u32)
            .u32(p.i64("count").clamp(0, i64::from(u32::MAX)) as u32)
            .u32(p.i64("offset").clamp(0, i64::from(u32::MAX)) as u32)
            .u32(self.op)
            .u32(self.target);
        u
    }
}

/// The buffers a selection reads: `P`, the compared attribute and `id`
/// (a small placeholder for each one the rule does not use).
struct Inputs {
    pos: wgpu::Buffer,
    attr: wgpu::Buffer,
    id: wgpu::Buffer,
    desc: [u32; 4],
    has_id: bool,
}

fn inputs(
    ctx: &GeoCtx<'_>,
    g: &Geometry,
    rule: &str,
    dummy: &wgpu::Buffer,
) -> Result<Inputs, String> {
    let needs_p = matches!(rule, "box" | "sphere");
    let pos = if needs_p {
        kit::readable(ctx, g, names::P, AttrType::VEC3)?
    } else {
        dummy.clone()
    };
    let (attr, desc) = if rule == "attribute" {
        let name = ctx.params.str("attribute");
        let (b, ty) = kit::readable_any(ctx, g, name)?;
        (b, kit::desc(ty))
    } else {
        (dummy.clone(), [1, 1, 0, 0])
    };
    let id = g.point_typed(names::ID, AttrType::U32);
    let has_id = id.is_some();
    let id = match id {
        Some(a) => a.ensure_gpu(ctx.queue, ctx.buffers).clone(),
        None => dummy.clone(),
    };
    Ok(Inputs {
        pos,
        attr,
        id,
        desc,
        has_id,
    })
}

fn dummy(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pnt.select.unused"),
        size: 256,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    })
}

/// Removes the points whose flag is 0: compaction and a gather of every
/// point attribute. Sized to the input's point count (re-made only when it
/// changes, an edit).
pub(crate) struct Compact {
    sized: Option<(u32, Compactor, wgpu::Buffer)>,
    gather: Gather,
    /// The compaction's header: the kernels bind this fixed buffer (so their
    /// bind groups do not change with the pooled output header, which gets a
    /// copy).
    work: wgpu::Buffer,
}

impl Compact {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            sized: None,
            gather: Gather::new(device),
            work: kit::work_header(device),
        }
    }

    /// The flags buffer for `capacity` points (one `u32` each).
    pub fn flags(&mut self, device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
        if self.sized.as_ref().is_none_or(|s| s.0 != capacity) {
            let flags = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pnt.compact.flags"),
                size: lsr_core::geo::align_up(u64::from(capacity) * 4),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.sized = Some((capacity, Compactor::new(device, capacity), flags));
        }
        self.sized.as_ref().expect("sized above").2.clone()
    }

    /// Records the compaction of `g` by the flags written earlier this
    /// frame (see [`Self::flags`]): every point attribute is replaced by its
    /// kept elements, in order, and `g` becomes GPU-counted. Primitives are
    /// dropped (their point numbers would no longer match).
    pub fn apply(&self, ctx: &mut GeoCtx<'_>, g: &mut Geometry) -> Result<(), String> {
        let (capacity, compactor, flags) = self.sized.as_ref().expect("flags() first");
        let capacity = *capacity;
        // The new attributes, allocated before the pass borrows the encoder.
        let n = g.attrs(AttrClass::Point).len();
        let mut moves: [Option<(wgpu::Buffer, wgpu::Buffer, u32)>; MAX_ATTRS] = Default::default();
        if n > MAX_ATTRS {
            return Err(format!(
                "{n} point attributes; this operator moves at most {MAX_ATTRS}"
            ));
        }
        let header: std::sync::Arc<GeoBuffer> = kit::new_header(ctx);
        let hdr = header.buffer().clone();
        for (k, slot) in moves.iter_mut().enumerate().take(n) {
            let a = g.attrs(AttrClass::Point)[k].clone();
            let src = a.ensure_gpu(ctx.queue, ctx.buffers).clone();
            let out = ctx.alloc(a.name_arc().clone(), a.ty(), capacity);
            let dst = out.gpu_buffer().expect("on the GPU").clone();
            *slot = Some((src, dst, a.ty().stride()));
            g.set_point(out);
        }
        let device = ctx.device;
        let mut p = pass(ctx.encoder);
        compactor.record(device, ctx.queue, &mut p, flags, 0, &self.work);
        let map = GatherMap {
            header: &self.work,
            map: compactor.map(),
        };
        for (src, dst, stride) in moves.iter().flatten() {
            self.gather.record(device, &mut p, map, *stride, src, dst);
        }
        drop(p);
        ctx.encoder
            .copy_buffer_to_buffer(&self.work, 0, &hdr, 0, lsr_core::geo::header::BYTES);
        g.set_gpu_counted(capacity, header);
        drop_prims(g);
        Ok(())
    }
}

/// Most point attributes an operator that moves every attribute handles.
pub(crate) const MAX_ATTRS: usize = 24;

/// Removes primitives and their attributes (after the points changed).
pub(crate) fn drop_prims(g: &mut Geometry) {
    g.set_topology(None);
    while let Some(a) = g.attrs(AttrClass::Prim).first() {
        let name = a.name_arc().clone();
        g.remove(AttrClass::Prim, &name);
    }
}

fn rule_params(rules: &[&str], default: &str, summary_rule: &str) -> Vec<ParamSchema> {
    vec![
        ParamSchema::menu("rule", "Rule", rules, default).help(summary_rule),
        ParamSchema::string("group", "Group", "").help("group: the point group's name."),
        ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("box, sphere: the centre."),
        ParamSchema::vec("size", "Size", &[1.0, 1.0, 1.0]).help("box: the extent along X, Y and Z."),
        ParamSchema::float("radius", "Radius", 0.5, Some(0.0), None).help("sphere: the radius."),
        ParamSchema::string("attribute", "Attribute", "P").help("attribute: the point attribute compared."),
        ParamSchema::int("component", "Component", 0, Some(0), Some(3)).help("attribute: which component (0 x, 1 y, 2 z, 3 w)."),
        ParamSchema::menu("compare", "Compare", COMPARE, ">").help("attribute: the comparison, attribute on the left."),
        ParamSchema::float("value", "Value", 0.0, None, None).help("attribute: the value compared with."),
        ParamSchema::float("fraction", "Fraction", 0.5, Some(0.0), Some(1.0)).help("random: the fraction of points selected."),
        ParamSchema::int("nth", "Nth", 2, Some(1), None).help("every_nth: one point in this many."),
        ParamSchema::int("offset", "Offset", 0, Some(0), None).help("every_nth: shifts which points are picked."),
        ParamSchema::int("first", "First", 0, Some(0), None).help("range: the first point number."),
        ParamSchema::int("count", "Count", 100, Some(0), None).help("range: how many points from the first."),
        ParamSchema::toggle("by_id", "By ID", true).help("random, every_nth, range: number points by their `id` attribute when they have one (a particle keeps its fate from frame to frame), else by position in the list."),
        ParamSchema::int("seed", "Seed", 0, Some(0), None).help("random: selects a different set."),
    ]
}

pub fn delete_schema() -> OperatorSchema {
    let mut params = rule_params(
        DELETE_RULES,
        "box",
        "group: members of a point group; box, sphere: points inside the region; attribute: points where the comparison holds.",
    );
    params.push(ParamSchema::toggle("invert", "Invert", false).help(
        "Deletes the points the rule does not select instead (keeps the selection: a split).",
    ));
    crate::pnt(
        "delete",
        "Delete",
        "Removes the points a rule selects: members of a group, points inside a box or sphere, or points whose attribute passes a comparison. Invert keeps them instead, so two Deletes split a geometry. Every attribute follows; the count stays on the GPU.",
        &[("in", false)],
        params,
        200,
    )
}

pub fn subset_schema() -> OperatorSchema {
    let mut params = rule_params(
        SUBSET_RULES,
        "every_nth",
        "every_nth: one point in Nth; random: a seeded random fraction; range: Count points from First.",
    );
    params.push(
        ParamSchema::toggle("invert", "Invert", false)
            .help("Keeps the points the rule does not pick instead."),
    );
    crate::pnt(
        "subset",
        "Subset",
        "Keeps a subset of the points: one in every Nth, a seeded random fraction, or a range of point numbers (by `id` when the points have one, so particles keep their fate). Every attribute follows; the count stays on the GPU.",
        &[("in", false)],
        params,
        200,
    )
}

pub fn group_schema() -> OperatorSchema {
    let mut params = rule_params(
        RULES,
        "box",
        "group: members of another group; box, sphere: inside the region; attribute: where the comparison holds; random: a seeded fraction; every_nth: one in Nth; range: Count from First.",
    );
    params.insert(
        0,
        ParamSchema::string("name", "Name", "group1").help(
            "Name of the point group written (bits in the `group` attribute; at most 32 groups).",
        ),
    );
    params.push(
        ParamSchema::menu("operation", "Operation", GROUP_OPS, "replace").help(
            "replace: the group becomes the selection; union: adds it; intersect: keeps only members also selected; subtract: removes the selection.",
        ),
    );
    params.push(
        ParamSchema::toggle("invert", "Invert", false)
            .help("Uses the points the rule does not select."),
    );
    crate::pnt(
        "group",
        "Group",
        "Puts points into a named group by a rule (inside a box or sphere, an attribute comparison, a random fraction, every Nth, a range, or another group), as a bit of the `group` attribute. Delete, and other operators' group options, read it.",
        &[("in", false)],
        params,
        80,
    )
}

/// Delete and Subset: flags from the rule, then compaction.
pub struct Remove {
    rules: &'static [&'static str],
    flags: PointKernel,
    compact: Compact,
    dummy: wgpu::Buffer,
}

fn flags_kernel(device: &wgpu::Device, label: &str) -> PointKernel {
    use Bind::*;
    PointKernel::with_main(
        device,
        label,
        &format!(
            "{}@group(0) @binding(2) var<storage, read_write> flags: array<u32>;
{SELECT_BINDINGS}@group(0) @binding(6) var<storage, read> G: array<u32>;
fn gmask(i: u32) -> u32 {{ return G[i]; }}
",
            select_decls()
        ),
        &[ReadWrite, Read, Read, Read, Read],
        112,
        "if (i < arrayLength(&flags)) {
        var keep = 0u;
        if (i < hdr.count && !selected(i)) { keep = 1u; }
        flags[i] = keep;
    }",
    )
}

pub fn create_delete(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Remove {
        rules: DELETE_RULES,
        flags: flags_kernel(device, "pnt.delete"),
        compact: Compact::new(device),
        dummy: dummy(device),
    })
}

pub fn create_subset(device: &wgpu::Device) -> Box<dyn GeoOp> {
    Box::new(Remove {
        rules: SUBSET_RULES,
        flags: flags_kernel(device, "pnt.subset"),
        compact: Compact::new(device),
        dummy: dummy(device),
    })
}

/// The bit of point group `name` in `g`, or a node error.
fn group_bit(g: &Geometry, name: &str) -> Result<u32, String> {
    g.groups(AttrClass::Point)
        .iter()
        .position(|n| &**n == name)
        .map(|b| b as u32)
        .ok_or_else(|| format!("the input has no point group `{name}`"))
}

impl GeoOp for Remove {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let p = ctx.params;
        let rule = p.str("rule");
        let rule = if self.rules.contains(&rule) {
            rule
        } else {
            self.rules[0]
        };
        let hdr = kit::header_of(&g)?;
        let inp = inputs(ctx, &g, rule, &self.dummy)?;
        let (gbit, groups) = if rule == "group" {
            let bit = group_bit(&g, p.str("group"))?;
            let ga = kit::readable(ctx, &g, names::GROUP, AttrType::U32)?;
            (bit, ga)
        } else {
            (0, self.dummy.clone())
        };
        // Delete removes the selection; Subset keeps it (the kernel keeps
        // what is not selected, so Subset inverts).
        let subset = self.rules == SUBSET_RULES;
        let rule_u = Rule {
            p: &p,
            mode: menu_index(RULES, rule),
            gbit,
            desc: inp.desc,
            has_id: inp.has_id,
            invert: p.bool("invert") != subset,
            op: 0,
            target: 0,
        };
        let flags = self.compact.flags(ctx.device, g.points());
        self.flags.write(ctx.queue, rule_u.pack().bytes());
        {
            let device = ctx.device;
            let mut ps = pass(ctx.encoder);
            self.flags.dispatch_in(
                device,
                &mut ps,
                &hdr,
                &[&flags, &inp.pos, &inp.attr, &inp.id, &groups],
                Work::Elements(g.points()),
            );
        }
        self.compact.apply(ctx, &mut g)?;
        *ctx.output = g;
        Ok(())
    }
}

/// Group: writes the rule's result into a bit of `group`.
pub struct Group {
    kernel: PointKernel,
    dummy: wgpu::Buffer,
}

pub fn create_group(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Group {
        kernel: PointKernel::new(
            device,
            "pnt.group",
            &format!(
                "{}@group(0) @binding(2) var<storage, read_write> grp: array<u32>;
{SELECT_BINDINGS}fn gmask(i: u32) -> u32 {{ return grp[i]; }}
fn point(i: u32) {{
    let s = selected(i);
    let bit = 1u << params.tbit;
    let old = grp[i];
    var on = s;
    switch params.op {{
        case 1u: {{ on = s || (old & bit) != 0u; }}
        case 2u: {{ on = s && (old & bit) != 0u; }}
        case 3u: {{ on = !s && (old & bit) != 0u; }}
        default: {{}}
    }}
    grp[i] = select(old & ~bit, old | bit, on);
}}
",
                select_decls()
            ),
            &[ReadWrite, Read, Read, Read],
            112,
        ),
        dummy: dummy(device),
    })
}

impl GeoOp for Group {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let p = ctx.params;
        let name = p.str("name");
        if !lsr_core::geo::valid_name(name) {
            return Err(format!("`{name}` is not a valid group name"));
        }
        let rule = p.str("rule");
        if rule == "attribute" && p.str("attribute") == names::GROUP {
            return Err("compare another attribute than `group` (Group writes it)".into());
        }
        let hdr = kit::header_of(&g)?;
        let inp = inputs(ctx, &g, rule, &self.dummy)?;
        // "group": select by another group's bit, read from the same mask.
        let source_bit = if rule == "group" {
            group_bit(&g, p.str("group"))?
        } else {
            0
        };
        let bit = g
            .add_group(AttrClass::Point, name)
            .ok_or("the geometry has 32 point groups already")?;
        let grp = kit::writable(ctx, &mut g, names::GROUP, AttrType::U32, Missing::Zero)?;
        let rule_u = Rule {
            p: &p,
            mode: menu_index(RULES, rule),
            gbit: source_bit,
            desc: inp.desc,
            has_id: inp.has_id,
            invert: p.bool("invert"),
            op: menu_index(GROUP_OPS, p.str("operation")),
            target: bit,
        };
        self.kernel.run(
            ctx,
            &hdr,
            &[&grp, &inp.pos, &inp.attr, &inp.id],
            rule_u.pack().bytes(),
        );
        *ctx.output = g;
        Ok(())
    }
}

const REGIONS: &[&str] = &[
    "none",
    "inside_box",
    "outside_box",
    "inside_sphere",
    "outside_sphere",
];

pub fn age_schema() -> OperatorSchema {
    crate::pnt(
        "age",
        "Age",
        "Ages points and removes them by rules: `age += dt` each frame (created at zero when missing), then removes points that reached their `life` (or the Life parameter when they have none), points inside or outside a region, and points slower than a speed. For simulation loops (Solver, or a feedback chain); Particles ages and removes its own particles.",
        &[("in", false)],
        vec![
            ParamSchema::toggle("step", "Step", true).help("Adds the frame's time step to `age`."),
            ParamSchema::toggle("kill_dead", "Kill Dead", true)
                .help("Removes points whose `age` reached their life."),
            ParamSchema::float("life", "Life", 2.0, Some(0.0), None)
                .help("The life, in seconds, of points without a `life` attribute."),
            ParamSchema::menu("region", "Region", REGIONS, "none")
                .help("Also removes points inside or outside a box or a sphere."),
            ParamSchema::vec("centre", "Centre", &[0.0, 0.0, 0.0]).help("The region's centre."),
            ParamSchema::vec("size", "Size", &[4.0, 4.0, 4.0])
                .help("The box's extent along X, Y and Z."),
            ParamSchema::float("radius", "Radius", 2.0, Some(0.0), None)
                .help("The sphere's radius."),
            ParamSchema::float("min_speed", "Min Speed", 0.0, Some(0.0), None).help(
                "Also removes points slower than this (units per second; 0: off; needs `v`).",
            ),
        ],
        200,
    )
}

const AGE: &str = "struct Params {
    centre: vec4f,
    half: vec4f,
    // dt, default life, minimum speed, unused
    values: vec4f,
    region: u32,
    // bit 0: step, 1: kill dead, 2: has life, 3: has v
    flags_in: u32,
    pad0: u32,
    pad1: u32,
};
@group(0) @binding(2) var<storage, read_write> flags: array<u32>;
@group(0) @binding(3) var<storage, read_write> age: array<f32>;
@group(0) @binding(4) var<storage, read> life: array<f32>;
@group(0) @binding(5) var<storage, read> P: array<vec4f>;
@group(0) @binding(6) var<storage, read> v: array<vec4f>;
";

const AGE_MAIN: &str = "if (i < arrayLength(&flags)) {
        var keep = 0u;
        if (i < hdr.count) {
            var a = age[i];
            if ((params.flags_in & 1u) != 0u) {
                a = a + params.values.x;
                age[i] = a;
            }
            var l = params.values.y;
            if ((params.flags_in & 4u) != 0u) { l = life[i]; }
            keep = 1u;
            if ((params.flags_in & 2u) != 0u && a >= l) { keep = 0u; }
            let p = P[i].xyz;
            let in_box = all(abs(p - params.centre.xyz) <= params.half.xyz);
            let in_ball = length(p - params.centre.xyz) <= params.centre.w;
            switch params.region {
                case 1u: { if (in_box) { keep = 0u; } }
                case 2u: { if (!in_box) { keep = 0u; } }
                case 3u: { if (in_ball) { keep = 0u; } }
                case 4u: { if (!in_ball) { keep = 0u; } }
                default: {}
            }
            if ((params.flags_in & 8u) != 0u && length(v[i].xyz) < params.values.z) { keep = 0u; }
        }
        flags[i] = keep;
    }";

pub struct Age {
    kernel: PointKernel,
    compact: Compact,
    dummy: wgpu::Buffer,
}

pub fn create_age(device: &wgpu::Device) -> Box<dyn GeoOp> {
    use Bind::*;
    Box::new(Age {
        kernel: PointKernel::with_main(
            device,
            "pnt.age",
            AGE,
            &[ReadWrite, ReadWrite, Read, Read, Read],
            64,
            AGE_MAIN,
        ),
        compact: Compact::new(device),
        dummy: dummy(device),
    })
}

impl GeoOp for Age {
    fn cook(&mut self, ctx: &mut GeoCtx<'_>) -> Result<(), String> {
        let Some(mut g) = ctx.take_input(0) else {
            ctx.output.clear();
            return Ok(());
        };
        if g.points() == 0 {
            *ctx.output = g;
            return Ok(());
        }
        let p = ctx.params;
        let hdr = kit::header_of(&g)?;
        let region = menu_index(REGIONS, p.str("region"));
        let min_speed = p.f32("min_speed").max(0.0);
        let kill_dead = p.bool("kill_dead");
        let life = g
            .point_typed(names::LIFE, AttrType::F32)
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        let vel = g
            .point_typed(names::V, AttrType::VEC3)
            .map(|a| a.ensure_gpu(ctx.queue, ctx.buffers).clone());
        let pos = if region != 0 {
            kit::readable(ctx, &g, names::P, AttrType::VEC3)?
        } else {
            self.dummy.clone()
        };
        let age = kit::writable(ctx, &mut g, names::AGE, AttrType::F32, Missing::Zero)?;
        let mut bits = 0u32;
        if p.bool("step") {
            bits |= 1;
        }
        if kill_dead {
            bits |= 2;
        }
        if life.is_some() {
            bits |= 4;
        }
        let speed_rule = min_speed > 0.0 && vel.is_some();
        if speed_rule {
            bits |= 8;
        }
        let c = vec3(&p, "centre");
        let s = vec3(&p, "size");
        let mut u = Packer::<64>::new();
        u.vec4([c[0], c[1], c[2], p.f32("radius").max(0.0)])
            .vec4([s[0] * 0.5, s[1] * 0.5, s[2] * 0.5, 0.0])
            .vec4([ctx.dt as f32, p.f32("life").max(0.0), min_speed, 0.0])
            .u32(region)
            .u32(bits);
        let flags = self.compact.flags(ctx.device, g.points());
        self.kernel.write(ctx.queue, u.bytes());
        {
            let life = life.as_ref().unwrap_or(&self.dummy);
            let vel = vel.as_ref().unwrap_or(&self.dummy);
            let device = ctx.device;
            let mut ps = pass(ctx.encoder);
            self.kernel.dispatch_in(
                device,
                &mut ps,
                &hdr,
                &[&flags, &age, life, &pos, vel],
                Work::Elements(g.points()),
            );
        }
        if kill_dead || region != 0 || speed_rule {
            self.compact.apply(ctx, &mut g)?;
        }
        *ctx.output = g;
        Ok(())
    }
}
