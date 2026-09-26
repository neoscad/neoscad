//! Builtin modules: control flow, CSG, transforms and primitives.
//!
//! Each validates its arguments with OpenSCAD's warnings (the echo goldens
//! include them) and produces a node with the evaluated parameters.
//! Argument frames stay on the context stack while children are
//! instantiated, as OpenSCAD's `Parameters` objects do, which is how
//! `translate(..., $fn = 8) sphere()` passes `$fn` down.

use std::collections::HashMap;
use std::rc::Rc;

use lang::diag::DiagCode;

use crate::call::ArgVal;
use crate::context::{Children, Ctx, CtxKind, ScopeRef};
use crate::eval::Evaluator;
use crate::fma::mul_add;
use crate::message::{Loc, R};
use crate::node::{self, CsgOp, Discretizer, LinearExtrude, Matrix, Node, NodeKind, OffsetJoin};
use crate::sym::{FxBuild, Sym, Syms};
use crate::trig::{cos_degrees, sin_degrees};
use crate::value::{MAX_RANGE_STEPS, Type, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinModule {
    Children,
    Echo,
    Assert,
    For,
    Let,
    IntersectionFor,
    If,
    Group,
    Union,
    Difference,
    Intersection,
    Scale,
    Rotate,
    Mirror,
    Translate,
    Multmatrix,
    Color,
    Render,
    Projection,
    Minkowski,
    Hull,
    Fill,
    Resize,
    Offset,
    LinearExtrude,
    RotateExtrude,
    Cube,
    Sphere,
    Cylinder,
    Polyhedron,
    Square,
    Circle,
    Polygon,
    Surface,
    Import,
    Text,
    /// Experimental: known, but not enabled.
    Roof,
}

impl BuiltinModule {
    pub fn enabled(self) -> bool {
        self != BuiltinModule::Roof
    }
}

pub(crate) fn table(syms: &mut Syms) -> HashMap<Sym, BuiltinModule, FxBuild> {
    use BuiltinModule::*;
    let all = [
        ("children", Children),
        ("echo", Echo),
        ("assert", Assert),
        ("for", For),
        ("let", Let),
        ("intersection_for", IntersectionFor),
        ("if", If),
        ("group", Group),
        ("union", Union),
        ("difference", Difference),
        ("intersection", Intersection),
        ("scale", Scale),
        ("rotate", Rotate),
        ("mirror", Mirror),
        ("translate", Translate),
        ("multmatrix", Multmatrix),
        ("color", Color),
        ("render", Render),
        ("projection", Projection),
        ("minkowski", Minkowski),
        ("hull", Hull),
        ("fill", Fill),
        ("resize", Resize),
        ("offset", Offset),
        ("linear_extrude", LinearExtrude),
        ("rotate_extrude", RotateExtrude),
        ("cube", Cube),
        ("sphere", Sphere),
        ("cylinder", Cylinder),
        ("polyhedron", Polyhedron),
        ("square", Square),
        ("circle", Circle),
        ("polygon", Polygon),
        ("surface", Surface),
        ("import", Import),
        ("text", Text),
        ("roof", Roof),
    ];
    all.into_iter().map(|(n, b)| (syms.intern(n), b)).collect()
}

/// A builtin's bound arguments (`Parameters`), on the context stack.
pub(crate) struct Params {
    frame: Rc<Ctx>,
    loc: Loc,
    mark: usize,
    caller: &'static str,
}

/// `std::max` for doubles: NaN in the first argument wins.
fn cmax(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

const F_MINIMUM: f64 = 0.01;

/// Eigen's `Transform::rotate(m3)` on an identity transform: the linear
/// part becomes `I * m3`, evaluated as a real product. The product is not a
/// copy: `1 * x + 0 * y + 0 * z` turns a `-0` in `m3` into `+0` unless every
/// term is `-0`, and the `.csg` export prints the sign (`rotate([90, 0, 0])`
/// has `-sin(0) = -0` below the diagonal, which OpenSCAD prints as `0`).
fn rot3(m3: [[f64; 3]; 3]) -> Matrix {
    let mut m = node::IDENTITY;
    for (i, row) in m.iter_mut().enumerate().take(3) {
        for (j, x) in row.iter_mut().enumerate().take(3) {
            let id = |k: usize| if k == i { 1.0 } else { 0.0 };
            // Eigen's reduction starts from the first term, not from 0.0,
            // so an all-`-0` sum stays `-0`.
            *x = id(0) * m3[0][j] + id(1) * m3[1][j] + id(2) * m3[2][j];
        }
    }
    m
}

/// `angle_axis_degrees` (degree_trig.cc).
fn angle_axis(a: f64, v: [f64; 3]) -> [[f64; 3]; 3] {
    let mut m = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let s = sin_degrees(a);
    let c = cos_degrees(a);
    let sq = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if sq > 0.0 {
        let f = (1.0 - c) / sq;
        let cv = [v[0] * f, v[1] * f, v[2] * f];
        let n = sq.sqrt();
        let us = [v[0] / n * s, v[1] / n * s, v[2] / n * s];
        m = [
            [cv[0] * v[0] + c, cv[1] * v[0] - us[2], cv[2] * v[0] + us[1]],
            [cv[0] * v[1] + us[2], cv[1] * v[1] + c, cv[2] * v[1] - us[0]],
            [cv[0] * v[2] - us[1], cv[1] * v[2] + us[0], cv[2] * v[2] + c],
        ];
    }
    m
}

impl<'a> Evaluator<'a> {
    fn sym(&mut self, s: &str) -> Sym {
        self.syms.intern(s)
    }

    /// `Parameters::parse(arguments, loc, required, optional)`, pushed.
    fn params(
        &mut self,
        args: Vec<ArgVal>,
        loc: Loc,
        required: &[&str],
        optional: &[&str],
        caller: &'static str,
    ) -> Params {
        let req: Vec<Sym> = required.iter().map(|s| self.sym(s)).collect();
        let opt: Vec<Sym> = optional.iter().map(|s| self.sym(s)).collect();
        let vars = self.bind_builtin(args, loc, &req, &opt, true);
        let frame = Ctx::new(None, CtxKind::Plain);
        *frame.vars.borrow_mut() = vars;
        let mark = self.push(frame.clone());
        Params {
            frame,
            loc,
            mark,
            caller,
        }
    }

    fn end(&mut self, p: Params) {
        self.truncate(p.mark);
    }

    /// `Parameters::lookup`: `$` names come from the stack (this frame is
    /// on it), others from the frame.
    fn lookup_param(&mut self, p: &Params, name: &str) -> Option<Value> {
        let s = self.sym(name);
        if self.syms.is_config(s) {
            self.lookup_special(s)
        } else {
            p.frame.get_local(s)
        }
    }

    fn get(&mut self, p: &Params, name: &str) -> Value {
        self.lookup_param(p, name).unwrap_or_default()
    }

    /// `Parameters::get({a, b})`: the first defined, warning if both are.
    fn get_any(&mut self, p: &Params, names: &[&str]) -> Value {
        let mut found: Option<(&str, Value)> = None;
        for &n in names {
            if let Some(v) = self.lookup_param(p, n).filter(Value::is_defined) {
                match &found {
                    None => found = Some((n, v)),
                    Some((m, _)) => {
                        let t = format!("Specified both \"{m}\" and \"{n}\"");
                        self.warn(p.loc, DiagCode::ArgumentMismatch, t);
                    }
                }
            }
        }
        found.map(|(_, v)| v).unwrap_or_default()
    }

    /// `print_argConvert_warning`.
    fn convert_warning(&mut self, p: &Params, name: &str, v: &Value, expected: Type) {
        let mut t = format!("{}(..., {name}=", p.caller).into_bytes();
        self.write_echo_nothrow(v, &mut t);
        t.extend_from_slice(
            format!(
                ") Invalid type: expected {}, found {}",
                expected.name(),
                v.type_name()
            )
            .as_bytes(),
        );
        self.warn(p.loc, DiagCode::InvalidArgument, t);
    }

    /// `Parameters::valid(name, type)`: absent or undef is fine.
    fn valid(&mut self, p: &Params, name: &str, ty: Type) -> bool {
        match self.lookup_param(p, name) {
            None | Some(Value::Undef) => true,
            Some(v) if v.ty() == ty => true,
            Some(v) => {
                self.convert_warning(p, name, &v, ty);
                false
            }
        }
    }

    /// `Parameters::validate_number`.
    fn validate_number(&mut self, p: &Params, name: &str) -> Option<f64> {
        let v = self.lookup_param(p, name)?;
        match v {
            Value::Undef => None,
            Value::Number(x) if x.is_finite() => Some(x),
            Value::Number(_) => {
                let mut t = format!("{}(..., {name}=", p.caller).into_bytes();
                let _ = self.write_string(&v, &mut t);
                t.extend_from_slice(b") argument cannot be infinite or nan");
                self.warn(p.loc, DiagCode::InvalidArgument, t);
                None
            }
            other => {
                self.convert_warning(p, name, &other, Type::Number);
                None
            }
        }
    }

    /// `Parameters::validate_integral` for unsigned values.
    fn validate_u32(&mut self, p: &Params, name: &str, lo: u32) -> Option<u32> {
        let x = self.validate_number(p, name)?;
        Some(if x < f64::from(lo) {
            lo
        } else if x > f64::from(u32::MAX) {
            u32::MAX
        } else {
            x as u32
        })
    }

    /// `CurveDiscretizer(parameters, loc)`, with its clamping warnings.
    fn discretizer(&mut self, p: &Params) -> Discretizer {
        let mut fn_ = self.get(p, "$fn").to_f64();
        let mut fs = self.get(p, "$fs").to_f64();
        let mut fa = self.get(p, "$fa").to_f64();
        if fn_ < 0.0 {
            self.warn(
                p.loc,
                DiagCode::InvalidArgument,
                "$fn negative - setting to 0",
            );
            fn_ = 0.0;
        }
        if fs < F_MINIMUM {
            self.warn(
                p.loc,
                DiagCode::InvalidArgument,
                format!("$fs too small - clamping to {F_MINIMUM:.6}"),
            );
            fs = F_MINIMUM;
        }
        if fa < F_MINIMUM {
            self.warn(
                p.loc,
                DiagCode::InvalidArgument,
                format!("$fa too small - clamping to {F_MINIMUM:.6}"),
            );
            fa = F_MINIMUM;
        }
        Discretizer { fn_, fa, fs }
    }

    /// `CurveDiscretizer(parameters)`: clamps silently.
    fn discretizer_quiet(&mut self, p: &Params) -> Discretizer {
        let fn_ = cmax(self.get(p, "$fn").to_f64(), 0.0);
        let fs = cmax(self.get(p, "$fs").to_f64(), F_MINIMUM);
        let fa = cmax(self.get(p, "$fa").to_f64(), F_MINIMUM);
        Discretizer { fn_, fa, fs }
    }

    /// `BuiltinModule::noChildren`.
    fn no_children(&mut self, sr: ScopeRef, i: usize) {
        let cs = self.children_scope(sr, i);
        if !self.scope(cs).instantiations.is_empty() {
            let t = format!(
                "module {}() does not support child modules",
                self.name(self.inst_name(sr, i))
            );
            let loc = self.inst_loc(sr, i);
            self.warn(loc, DiagCode::ArgumentMismatch, t);
        }
    }

    fn inst_args(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> R<Vec<ArgVal>> {
        let inst = self.inst(sr, i);
        self.eval_args(sr.unit, &inst.args, ctx)
    }

    /// Instantiate children into `node` and return it.
    fn with_children(
        &mut self,
        mut node: Node,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<Option<Node>> {
        let ch = Children {
            scope: self.children_scope(sr, i),
            ctx: ctx.clone(),
        };
        self.instantiate_children(&ch, &mut node.children, None)?;
        Ok(Some(node))
    }

    pub fn builtin_module(
        &mut self,
        b: BuiltinModule,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<Option<Node>> {
        use BuiltinModule as B;
        // OpenSCAD checks the stack only for user modules, but a chain of
        // builtins can nest as deep as the user modules around it
        // (`children()` of `children()` of ...), so the frame budget is
        // checked here too, with a quarter more room so that a recursive
        // module still stops at its own call, with OpenSCAD's message,
        // rather than at an `if` inside it. Natively the budget is
        // unlimited, and this never fires.
        let budget = self.opts.frame_limit;
        if self.frames >= budget.saturating_add(budget / 4) {
            return Err(self.builtin_recursion(sr, i));
        }
        let loc = self.inst_loc(sr, i);
        match b {
            B::Children => self.children_module(sr, i, ctx),
            B::Echo => {
                let inst = self.inst(sr, i);
                self.echo(sr.unit, &inst.args, ctx)?;
                let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                let node = self.with_children(node, sr, i, ctx)?;
                Ok(node.filter(|n| !n.children.is_empty()))
            }
            B::Assert => {
                let inst = self.inst(sr, i);
                self.perform_assert(sr.unit, &inst.args, inst.span, ctx)?;
                let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                let node = self.with_children(node, sr, i, ctx)?;
                Ok(node.filter(|n| !n.children.is_empty()))
            }
            B::Let => {
                let inst = self.inst(sr, i);
                let c = Ctx::child(ctx);
                let mark = self.push(c.clone());
                let r = self
                    .sequential_assign(sr.unit, &inst.args, inst.span, &c)
                    .and_then(|_| {
                        let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                        self.with_children(node, sr, i, &c)
                    });
                self.truncate(mark);
                r
            }
            B::For | B::IntersectionFor => {
                let kind = if b == B::For {
                    NodeKind::Group { name: None }
                } else {
                    NodeKind::IntersectionFor
                };
                let mut node = self.new_node(kind, sr, i);
                let inst = self.inst(sr, i);
                if !inst.args.is_empty() {
                    let scope = self.children_scope(sr, i);
                    let mut kids = Vec::new();
                    self.for_each(sr.unit, &inst.args, loc, ctx, &mut |ev, c| {
                        ev.instantiate_children(
                            &Children {
                                scope,
                                ctx: c.clone(),
                            },
                            &mut kids,
                            None,
                        )
                    })?;
                    node.children = kids;
                }
                Ok(Some(node))
            }
            B::If => {
                let inst = self.inst(sr, i);
                let args = self.eval_args(sr.unit, &inst.args, ctx)?;
                let branch = if args.first().is_some_and(|a| a.value.to_bool()) {
                    Some(self.children_scope(sr, i))
                } else {
                    self.else_scope(sr, i)
                };
                let Some(scope) = branch else { return Ok(None) };
                let mut node = self.new_node(NodeKind::Group { name: None }, sr, i);
                self.instantiate_children(
                    &Children {
                        scope,
                        ctx: ctx.clone(),
                    },
                    &mut node.children,
                    None,
                )?;
                Ok(Some(node))
            }
            _ => self.geometry_module(b, sr, i, ctx, loc),
        }
    }

    /// `builtin_children`.
    fn children_module(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> R<Option<Node>> {
        let loc = self.inst_loc(sr, i);
        let args = self.inst_args(sr, i, ctx)?;
        self.no_children(sr, i);
        let p = self.params(args, loc, &[], &["index"], "children");
        let r = self.children_module_inner(&p, sr, i, ctx);
        self.end(p);
        r
    }

    fn children_module_inner(
        &mut self,
        p: &Params,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<Option<Node>> {
        let loc = p.loc;
        let Some(children) = ctx.module_children() else {
            return Ok(None);
        };
        let size = self.scope(children.scope).instantiations.len();
        let index = self.lookup_param(p, "index");
        let valid = |ev: &mut Self, n: i32| -> Option<usize> {
            if n < 0 || n as usize >= size {
                ev.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    format!("Children index ({n}) out of bounds ({size} children)"),
                );
                None
            } else {
                Some(n as usize)
            }
        };
        let indices: Option<Vec<usize>> = match index {
            None => None,
            Some(Value::Number(x)) => match valid(self, x as i32) {
                Some(k) => Some(vec![k]),
                None => return Ok(None),
            },
            Some(Value::Vector(v)) => {
                let mut ix = Vec::new();
                for e in v.iter() {
                    match e {
                        Value::Number(x) => {
                            if let Some(k) = valid(self, *x as i32) {
                                ix.push(k);
                            }
                        }
                        other => {
                            let mut t = b"Bad parameter type (".to_vec();
                            let _ = self.write_string(other, &mut t);
                            t.extend_from_slice(
                                b") for children, only accept: empty, number, vector, range.",
                            );
                            self.warn(loc, DiagCode::InvalidArgument, t);
                        }
                    }
                }
                Some(ix)
            }
            Some(Value::Range(r)) => {
                let steps = r.num_values();
                if steps >= MAX_RANGE_STEPS {
                    let t =
                        format!("Bad range parameter for children: too many elements ({steps})");
                    self.warn(loc, DiagCode::IterationLimit, t);
                    return Ok(None);
                }
                let mut ix = Vec::new();
                for d in r.iter() {
                    if let Some(k) = valid(self, d as i32) {
                        ix.push(k);
                    }
                }
                Some(ix)
            }
            Some(other) => {
                let mut t = b"Bad parameter type (".to_vec();
                self.write_echo_nothrow(&other, &mut t);
                t.extend_from_slice(b") for children, only accept: empty, number, vector, range");
                self.warn(loc, DiagCode::InvalidArgument, t);
                return Ok(None);
            }
        };
        let mut node = self.new_node(NodeKind::Group { name: None }, sr, i);
        self.instantiate_children(&children, &mut node.children, indices.as_deref())?;
        Ok(Some(node))
    }

    fn geometry_module(
        &mut self,
        b: BuiltinModule,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
        loc: Loc,
    ) -> R<Option<Node>> {
        use BuiltinModule as B;
        let args = self.inst_args(sr, i, ctx)?;
        let leaf = matches!(
            b,
            B::Cube
                | B::Sphere
                | B::Cylinder
                | B::Polyhedron
                | B::Square
                | B::Circle
                | B::Polygon
                | B::Surface
                | B::Import
                | B::Text
        );
        if leaf {
            self.no_children(sr, i);
        }
        let (req, opt, caller): (&[&str], &[&str], &'static str) = match b {
            B::Group | B::Union | B::Difference | B::Intersection | B::Hull | B::Fill => {
                (&[], &[], "")
            }
            B::Scale | B::Mirror | B::Translate => (&["v"], &[], ""),
            B::Rotate => (&["a", "v"], &[], ""),
            B::Multmatrix => (&["m"], &[], ""),
            B::Color => (&["c", "alpha"], &[], ""),
            B::Render => (&["convexity"], &[], ""),
            B::Projection => (&["cut"], &["convexity"], ""),
            B::Minkowski => (&["convexity"], &[], ""),
            B::Resize => (&["newsize", "auto", "convexity"], &[], ""),
            B::Offset => (&["r"], &["delta", "chamfer"], ""),
            B::LinearExtrude => (
                &[
                    "height", "v", "scale", "center", "twist", "slices", "segments",
                ],
                &["convexity", "h"],
                "linear_extrude",
            ),
            B::RotateExtrude => (&["angle", "start"], &["convexity", "a"], ""),
            B::Cube | B::Square => (&["size", "center"], &[], ""),
            B::Sphere | B::Circle => (&["r"], &["d"], ""),
            B::Cylinder => (&["h", "r1", "r2", "center"], &["r", "d", "d1", "d2"], ""),
            B::Polyhedron => (&["points", "faces", "convexity"], &[], ""),
            B::Polygon => (&["points", "paths", "convexity"], &[], ""),
            B::Surface => (&["file", "center", "convexity"], &["invert"], ""),
            B::Import => (
                &["file", "layer", "convexity", "origin", "scale"],
                &[
                    "width",
                    "height",
                    "filename",
                    "layername",
                    "center",
                    "dpi",
                    "id",
                ],
                "",
            ),
            B::Text => (
                &["text", "size", "font"],
                &[
                    "direction",
                    "language",
                    "script",
                    "halign",
                    "valign",
                    "spacing",
                    "em",
                ],
                "text",
            ),
            _ => (&[], &[], ""),
        };
        let p = self.params(args, loc, req, opt, caller);
        let r = self.geometry_node(b, &p, sr, i, ctx);
        self.end(p);
        r
    }

    fn geometry_node(
        &mut self,
        b: BuiltinModule,
        p: &Params,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<Option<Node>> {
        use BuiltinModule as B;
        let loc = p.loc;
        let kind = match b {
            B::Group => NodeKind::Group { name: None },
            B::Union => NodeKind::Csg(CsgOp::Union),
            B::Difference => NodeKind::Csg(CsgOp::Difference),
            B::Intersection => NodeKind::Csg(CsgOp::Intersection),
            B::Hull => NodeKind::Hull,
            B::Fill => NodeKind::Fill,
            B::Scale => {
                let v = self.get(p, "v");
                let mut s = [1.0, 1.0, 1.0];
                if !v.get_vec3_or2(&mut s, 1.0) {
                    if let Some(n) = v.as_number() {
                        s = [n, n, n];
                    } else {
                        let mut t = b"Unable to convert scale(".to_vec();
                        self.write_echo_nothrow(&v, &mut t);
                        t.extend_from_slice(
                            b") parameter to a number, a vec3 or vec2 of numbers or a number",
                        );
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    }
                }
                if self.opts.check_parameter_ranges && s.iter().any(|&x| x == 0.0 || !x.is_finite())
                {
                    let mut t = b"scale(".to_vec();
                    self.write_echo_nothrow(&v, &mut t);
                    t.push(b')');
                    self.warn(loc, DiagCode::InvalidArgument, t);
                }
                // Eigen's `Transform::scale`: the identity times a diagonal,
                // coefficient by coefficient, so the off-diagonal entries of a
                // negative factor's column are `0 * s = -0`, which the `.csg`
                // export prints (`scale([1, -1, 1])` has `-0`s in column 1).
                let mut m = node::IDENTITY;
                for row in m.iter_mut().take(3) {
                    for k in 0..3 {
                        row[k] *= s[k];
                    }
                }
                NodeKind::Transform {
                    matrix: m,
                    verb: "scale",
                }
            }
            B::Rotate => NodeKind::Transform {
                matrix: self.rotate_matrix(p),
                verb: "rotate",
            },
            B::Mirror => {
                let v = self.get(p, "v");
                let mut xyz = [1.0, 0.0, 0.0];
                if !v.get_vec3_or2(&mut xyz, 0.0) {
                    let mut t = b"Unable to convert mirror(".to_vec();
                    self.write_echo_nothrow(&v, &mut t);
                    t.extend_from_slice(b") parameter to a vec3 or vec2 of numbers");
                    self.warn(loc, DiagCode::InvalidArgument, t);
                }
                let [x, y, z] = xyz;
                let mut m = node::IDENTITY;
                if x != 0.0 || y != 0.0 || z != 0.0 {
                    // `x * x + y * y + z * z` as the arm64 nightly rounds it
                    // (see `fma`).
                    let a = mul_add(z, z, mul_add(x, x, y * y));
                    m = [
                        [
                            1.0 - 2.0 * x * x / a,
                            -2.0 * y * x / a,
                            -2.0 * z * x / a,
                            0.0,
                        ],
                        [
                            -2.0 * x * y / a,
                            1.0 - 2.0 * y * y / a,
                            -2.0 * z * y / a,
                            0.0,
                        ],
                        [
                            -2.0 * x * z / a,
                            -2.0 * y * z / a,
                            1.0 - 2.0 * z * z / a,
                            0.0,
                        ],
                        [0.0, 0.0, 0.0, 1.0],
                    ];
                }
                NodeKind::Transform {
                    matrix: m,
                    verb: "mirror",
                }
            }
            B::Translate => {
                let v = self.get(p, "v");
                let mut t3 = [0.0; 3];
                let ok = v.get_vec3_or2(&mut t3, 0.0) && t3.iter().all(|x| x.is_finite());
                let mut m = node::IDENTITY;
                if ok {
                    // Eigen's `Transform::translate`: `translation += linear *
                    // v` from a zero column, which turns a `-0` component into
                    // `+0` (`translate([-10, -0])` prints `0`).
                    for k in 0..3 {
                        m[k][3] += t3[k];
                    }
                } else {
                    let mut t = b"Unable to convert translate(".to_vec();
                    self.write_echo_nothrow(&v, &mut t);
                    t.extend_from_slice(b") parameter to a vec3 or vec2 of numbers");
                    self.warn(loc, DiagCode::InvalidArgument, t);
                }
                NodeKind::Transform {
                    matrix: m,
                    verb: "translate",
                }
            }
            B::Multmatrix => {
                let mut m = node::IDENTITY;
                if let Value::Vector(rows) = self.get(p, "m") {
                    for (r, row) in rows.iter().take(4).enumerate() {
                        if let Value::Vector(cols) = row {
                            for (c, v) in cols.iter().take(4).enumerate() {
                                v.get_f64(&mut m[r][c]);
                            }
                        }
                    }
                    let w = m[3][3];
                    if w != 1.0 {
                        for row in m.iter_mut() {
                            for x in row.iter_mut() {
                                *x /= w;
                            }
                        }
                    }
                }
                NodeKind::Transform {
                    matrix: m,
                    verb: "multmatrix",
                }
            }
            B::Color => NodeKind::Color {
                rgba: self.color(p),
            },
            B::Render => {
                let c = self.get(p, "convexity");
                NodeKind::Render {
                    convexity: c.as_number().map_or(1, |x| x as i32),
                }
            }
            B::Projection => {
                let convexity = self.get(p, "convexity").to_f64() as i32;
                let cut = matches!(self.get(p, "cut"), Value::Bool(true));
                NodeKind::Projection { cut, convexity }
            }
            B::Minkowski => NodeKind::Minkowski {
                convexity: self.get(p, "convexity").to_f64() as i32,
            },
            B::Resize => {
                let convexity = self.get(p, "convexity").to_f64() as i32;
                let mut newsize = [0.0; 3];
                if let Value::Vector(v) = self.get(p, "newsize") {
                    for (k, x) in v.iter().take(3).enumerate() {
                        newsize[k] = x.to_f64();
                    }
                }
                let mut autosize = [false; 3];
                match self.get(p, "auto") {
                    Value::Vector(v) => {
                        for (k, x) in v.iter().take(3).enumerate() {
                            autosize[k] = x.to_bool();
                        }
                    }
                    Value::Bool(b) => autosize = [b; 3],
                    _ => {}
                }
                NodeKind::Resize {
                    newsize,
                    autosize,
                    convexity,
                }
            }
            B::Offset => {
                let disc = self.discretizer_quiet(p);
                let (r, delta, chamfer) = (
                    self.get(p, "r"),
                    self.get(p, "delta"),
                    self.get(p, "chamfer"),
                );
                let mut kind = (1.0, false, OffsetJoin::Round);
                if let Value::Number(r) = r {
                    if delta.as_number().is_some() {
                        self.warn(
                            loc,
                            DiagCode::ArgumentMismatch,
                            "Ignoring \"delta\" argument as \"r\" is defined too.",
                        );
                    }
                    kind.0 = r;
                } else if let Value::Number(d) = delta {
                    kind = (d, false, OffsetJoin::Miter);
                    if matches!(chamfer, Value::Bool(true)) {
                        kind = (d, true, OffsetJoin::Square);
                    }
                }
                NodeKind::Offset {
                    delta: kind.0,
                    chamfer: kind.1,
                    join: kind.2,
                    disc,
                }
            }
            B::LinearExtrude => NodeKind::LinearExtrude(self.linear_extrude(p)),
            B::RotateExtrude => {
                let disc = self.discretizer(p);
                let convexity = (self.get(p, "convexity").to_f64() as i32).max(2);
                let angle_v = self.get_any(p, &["angle", "a"]);
                let (mut angle, mut start);
                let has_angle = angle_v.as_finite().is_some();
                if let Some(a) = angle_v.as_finite() {
                    angle = a;
                    start = 0.0;
                    if angle <= -360.0 || angle > 360.0 {
                        angle = 360.0;
                    }
                } else {
                    angle = 360.0;
                    start = 180.0;
                }
                let start_v = self.get(p, "start");
                let has_start = start_v.as_finite().is_some();
                if let Some(s) = start_v.as_finite() {
                    start = s;
                }
                if !has_angle && !has_start && (disc.fn_ as i32) & 1 == 1 {
                    self.emit(
                        lang::diag::Severity::Deprecated,
                        DiagCode::Evaluation,
                        b"In future releases, rotational extrusion without \"angle\" will start at zero, the +X axis.  Set start=180 to explicitly start on the -X axis.",
                        None,
                    );
                }
                NodeKind::RotateExtrude {
                    angle,
                    start,
                    convexity,
                    disc,
                }
            }
            B::Cube => {
                let size = self.get(p, "size");
                let mut s = [1.0, 1.0, 1.0];
                if size.is_defined() {
                    let mut converted = false;
                    if let Some(n) = size.as_number() {
                        s = [n, n, n];
                        converted = true;
                    }
                    converted |= size.get_vec3(&mut s);
                    if !converted {
                        let mut t = b"Unable to convert cube(size=".to_vec();
                        self.write_echo_nothrow(&size, &mut t);
                        t.extend_from_slice(b", ...) parameter to a number or a vec3 of numbers");
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    } else if self.opts.check_parameter_ranges
                        && !s.iter().all(|&x| x > 0.0 && x.is_finite())
                    {
                        let mut t = b"cube(size=".to_vec();
                        self.write_echo_nothrow(&size, &mut t);
                        t.extend_from_slice(b", ...)");
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    }
                }
                NodeKind::Cube {
                    size: s,
                    center: self.center(p),
                }
            }
            B::Square => {
                let size = self.get(p, "size");
                let mut s = [1.0, 1.0];
                if size.is_defined() {
                    let mut converted = false;
                    if let Some(n) = size.as_number() {
                        s = [n, n];
                        converted = true;
                    }
                    if let Some(v) = size.as_vec2(false) {
                        s = v;
                        converted = true;
                    }
                    if !converted {
                        let mut t = b"Unable to convert square(size=".to_vec();
                        self.write_echo_nothrow(&size, &mut t);
                        t.extend_from_slice(b", ...) parameter to a number or a vec2 of numbers");
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    } else if self.opts.check_parameter_ranges
                        && !s.iter().all(|&x| x > 0.0 && x.is_finite())
                    {
                        let mut t = b"square(size=".to_vec();
                        self.write_echo_nothrow(&size, &mut t);
                        t.extend_from_slice(b", ...)");
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    }
                }
                NodeKind::Square {
                    size: s,
                    center: self.center(p),
                }
            }
            B::Sphere | B::Circle => {
                let disc = self.discretizer(p);
                let r = self.lookup_radius(p, "d", "r");
                let mut radius = 1.0;
                if let Value::Number(x) = r {
                    radius = x;
                    if self.opts.check_parameter_ranges && (x <= 0.0 || !x.is_finite()) {
                        let what = if b == B::Sphere { "sphere" } else { "circle" };
                        let mut t = format!("{what}(r=").into_bytes();
                        self.write_echo_nothrow(&r, &mut t);
                        t.push(b')');
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    }
                }
                if b == B::Sphere {
                    NodeKind::Sphere { r: radius, disc }
                } else {
                    NodeKind::Circle { r: radius, disc }
                }
            }
            B::Cylinder => self.cylinder(p),
            B::Polyhedron => self.polyhedron(p),
            B::Polygon => self.polygon(p),
            B::Surface => {
                let file = self.get(p, "file");
                let name = if file.is_undef() {
                    Vec::new()
                } else {
                    self.string_of(&file)
                };
                let file = self.lookup_file(&name, loc);
                let center = matches!(self.get(p, "center"), Value::Bool(true));
                let convexity = self.get(p, "convexity").as_number().map_or(1, |x| x as i32);
                let invert = matches!(self.get(p, "invert"), Value::Bool(true));
                NodeKind::Surface {
                    file,
                    center,
                    invert,
                    convexity,
                }
            }
            B::Import => NodeKind::Import(self.import(p)),
            B::Text => NodeKind::Text(self.text(p)),
            _ => NodeKind::Group { name: None },
        };
        let node = self.new_node(kind, sr, i);
        let leaf = matches!(
            b,
            B::Cube
                | B::Sphere
                | B::Cylinder
                | B::Polyhedron
                | B::Square
                | B::Circle
                | B::Polygon
                | B::Surface
                | B::Import
                | B::Text
        );
        if leaf {
            Ok(Some(node))
        } else {
            self.with_children(node, sr, i, ctx)
        }
    }

    fn center(&mut self, p: &Params) -> bool {
        matches!(self.get(p, "center"), Value::Bool(true))
    }

    /// `Value::toString` as bytes.
    fn string_of(&self, v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        let _ = self.write_string(v, &mut out);
        out
    }

    /// `lookup_file`: a path relative to the instantiating file's directory.
    fn lookup_file(&self, name: &[u8], loc: Loc) -> String {
        let name = String::from_utf8_lossy(name).into_owned();
        if name.is_empty() {
            return String::new();
        }
        let path = std::path::Path::new(&name);
        if path.is_absolute() {
            return name;
        }
        let src = &self.units[loc.unit as usize].program.sources;
        match src.path(loc.span.file).parent() {
            Some(dir) => dir.join(path).display().to_string(),
            None => String::new(),
        }
    }

    /// `lookup_radius`: the diameter wins over the radius.
    fn lookup_radius(&mut self, p: &Params, d: &str, r: &str) -> Value {
        let dv = self.get(p, d);
        let rv = self.get(p, r);
        let r_defined = matches!(rv, Value::Number(_));
        if let Value::Number(x) = dv {
            if r_defined {
                let t =
                    format!("Ignoring radius variable \"{r}\" as diameter \"{d}\" is defined too.");
                self.warn(p.loc, DiagCode::ArgumentMismatch, t);
            }
            return Value::Number(x / 2.0);
        }
        if r_defined { rv } else { Value::Undef }
    }

    fn rotate_matrix(&mut self, p: &Params) -> Matrix {
        let loc = p.loc;
        let a = self.get(p, "a");
        let v = self.get(p, "v");
        if let Value::Vector(va) = &a {
            let (mut sx, mut sy, mut sz) = (0.0, 0.0, 0.0);
            let (mut cx, mut cy, mut cz) = (1.0, 1.0, 1.0);
            let mut ok = true;
            let angle = |e: &Value, ok: &mut bool| -> (f64, f64) {
                let mut x = 0.0;
                *ok &= e.get_f64(&mut x);
                *ok &= x.is_finite();
                (sin_degrees(x), cos_degrees(x))
            };
            let n = va.len();
            if n > 3 {
                ok = false;
            }
            if n >= 3 {
                (sz, cz) = angle(&va[2], &mut ok);
            }
            if n >= 2 {
                (sy, cy) = angle(&va[1], &mut ok);
            }
            if n >= 1 {
                (sx, cx) = angle(&va[0], &mut ok);
            }
            let v_supplied = v.is_defined();
            if ok {
                if v_supplied {
                    let mut t =
                        b"When parameter a is supplied as vector, v is ignored rotate(a=".to_vec();
                    self.write_echo_nothrow(&a, &mut t);
                    t.extend_from_slice(b", v=");
                    self.write_echo_nothrow(&v, &mut t);
                    t.push(b')');
                    self.warn(loc, DiagCode::ArgumentMismatch, t);
                }
            } else {
                let mut t = b"Problem converting rotate(a=".to_vec();
                self.write_echo_nothrow(&a, &mut t);
                if v_supplied {
                    t.extend_from_slice(b", v=");
                    self.write_echo_nothrow(&v, &mut t);
                }
                t.extend_from_slice(b") parameter");
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
            // OpenSCAD writes each entry as one expression, so the arm64
            // nightly fuses the first product of each sum (see `fma`).
            // Checked against the nightly on 14,000 angle triples, where
            // fusing the other product flips signs of near-zero entries.
            rot3([
                [
                    cy * cz,
                    mul_add(cz * sx, sy, -(cx * sz)),
                    mul_add(cx * cz, sy, sx * sz),
                ],
                [
                    cy * sz,
                    mul_add(cx, cz, sx * sy * sz),
                    mul_add(-cz, sx, cx * sy * sz),
                ],
                [-sy, cy * sx, cx * cy],
            ])
        } else {
            let mut ang = 0.0;
            let a_ok = a.get_f64(&mut ang) && ang.is_finite();
            let mut axis = [0.0, 0.0, 1.0];
            let v_ok = v.get_vec3_or2(&mut axis, 0.0);
            let m = rot3(angle_axis(if a_ok { ang } else { 0.0 }, axis));
            if v.is_defined() && !v_ok {
                let mut t = Vec::new();
                if a_ok {
                    t.extend_from_slice(b"Problem converting rotate(..., v=");
                } else {
                    t.extend_from_slice(b"Problem converting rotate(a=");
                    self.write_echo_nothrow(&a, &mut t);
                    t.extend_from_slice(b", v=");
                }
                self.write_echo_nothrow(&v, &mut t);
                t.extend_from_slice(b") parameter");
                self.warn(loc, DiagCode::InvalidArgument, t);
            } else if !a_ok {
                let mut t = b"Problem converting rotate(a=".to_vec();
                self.write_echo_nothrow(&a, &mut t);
                t.extend_from_slice(b") parameter");
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
            m
        }
    }

    fn color(&mut self, p: &Params) -> [f32; 4] {
        let loc = p.loc;
        let mut rgba = [-1.0f32; 4];
        match self.get(p, "c") {
            Value::Vector(v) => {
                for (k, slot) in rgba.iter_mut().enumerate() {
                    *slot = if k < v.len() {
                        v[k].to_f64() as f32
                    } else {
                        1.0
                    };
                    if *slot > 1.0 || *slot < 0.0 {
                        let t = format!(
                            "color() expects numbers between 0.0 and 1.0. Value of {:.1} is out of range",
                            *slot
                        );
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    }
                }
            }
            Value::Str(s) => match parse_color(s.as_bytes()) {
                Some(c) => rgba = c,
                None => {
                    let mut t = b"Unable to parse color \"".to_vec();
                    t.extend_from_slice(s.as_bytes());
                    t.push(b'"');
                    self.warn(loc, DiagCode::InvalidArgument, t);
                }
            },
            _ => {}
        }
        if let Value::Number(a) = self.get(p, "alpha") {
            rgba[3] = a as f32;
            if rgba[3] < 0.0 || rgba[3] > 1.0 {
                let t = format!(
                    "color() expects alpha between 0.0 and 1.0. Value of {:.1} is out of range",
                    rgba[3]
                );
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
        }
        rgba
    }

    fn linear_extrude(&mut self, p: &Params) -> LinearExtrude {
        let loc = p.loc;
        let disc = self.discretizer(p);
        let mut height_v = [0.0, 0.0, 1.0];
        let mut height = 100.0;
        let v = self.get(p, "v");
        if v.is_defined() {
            if !v.get_vec3(&mut height_v) {
                self.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    "v when specified should be a 3d vector",
                );
            }
            height = 1.0;
        }
        let hv = self.get_any(p, &["height", "h"]);
        if hv.is_defined() {
            match hv.as_finite() {
                Some(h) => height = h,
                None => {
                    self.warn(
                        loc,
                        DiagCode::InvalidArgument,
                        "height when specified should be a number",
                    );
                    height = 100.0;
                }
            }
            let n =
                (height_v[0] * height_v[0] + height_v[1] * height_v[1] + height_v[2] * height_v[2])
                    .sqrt();
            if n > 0.0 {
                for x in height_v.iter_mut() {
                    *x /= n;
                }
            }
        }
        for x in height_v.iter_mut() {
            *x *= height;
        }
        let convexity = self.get(p, "convexity").as_positive_int().unwrap_or(1);
        let scale = self.get(p, "scale");
        let (mut sx, mut sy) = (1.0, 1.0);
        // getFiniteDouble twice, then getVec2 with finite elements.
        let mut ok = match scale.as_finite() {
            Some(x) => {
                sx = x;
                sy = x;
                true
            }
            None => false,
        };
        ok |= scale.get_vec2(&mut sx, &mut sy, true);
        if scale.is_defined() && (!ok || !sx.is_finite() || !sy.is_finite()) {
            let mut t = b"linear_extrude(..., scale=".to_vec();
            self.write_echo_nothrow(&scale, &mut t);
            t.extend_from_slice(b") could not be converted");
            self.warn(loc, DiagCode::InvalidArgument, t);
        }
        let center = self.center(p);
        if height_v[2] <= 0.0 {
            height_v[2] = 0.0;
        }
        sx = sx.max(0.0);
        sy = sy.max(0.0);
        let slices = self.validate_u32(p, "slices", 1);
        let segments = self.validate_u32(p, "segments", 0);
        let twist = self.get(p, "twist").as_finite().unwrap_or(0.0);
        LinearExtrude {
            height: height_v,
            center,
            convexity,
            twist,
            has_twist: twist != 0.0,
            slices: slices.unwrap_or(1),
            has_slices: slices.is_some(),
            segments: segments.unwrap_or(0),
            has_segments: segments.is_some(),
            scale: [sx, sy],
            disc,
        }
    }

    fn cylinder(&mut self, p: &Params) -> NodeKind {
        let loc = p.loc;
        let disc = self.discretizer(p);
        let hv = self.get(p, "h");
        let h = hv.as_number().unwrap_or(1.0);
        let r = self.lookup_radius(p, "d", "r");
        let r1 = self.lookup_radius(p, "d1", "r1");
        let r2 = self.lookup_radius(p, "d2", "r2");
        let num = |v: &Value| v.as_number();
        if num(&r).is_some() && (num(&r1).is_some() || num(&r2).is_some()) {
            self.warn(
                loc,
                DiagCode::ArgumentMismatch,
                "Cylinder parameters ambiguous",
            );
        }
        let (mut n1, mut n2) = (1.0, 1.0);
        if let Some(x) = num(&r) {
            n1 = x;
            n2 = x;
        }
        if let Some(x) = num(&r1) {
            n1 = x;
        }
        if let Some(x) = num(&r2) {
            n2 = x;
        }
        if self.opts.check_parameter_ranges {
            if h <= 0.0 || !h.is_finite() {
                let mut t = b"cylinder(h=".to_vec();
                self.write_echo_nothrow(&hv, &mut t);
                t.extend_from_slice(b", ...)");
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
            if n1 < 0.0
                || n2 < 0.0
                || (n1 == 0.0 && n2 == 0.0)
                || !n1.is_finite()
                || !n2.is_finite()
            {
                let mut t = b"cylinder(r1=".to_vec();
                let a = if num(&r1).is_some() {
                    r1.clone()
                } else {
                    r.clone()
                };
                self.write_echo_nothrow(&a, &mut t);
                t.extend_from_slice(b", r2=");
                let b = if num(&r2).is_some() {
                    r2.clone()
                } else {
                    r.clone()
                };
                self.write_echo_nothrow(&b, &mut t);
                t.extend_from_slice(b", ...)");
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
        }
        NodeKind::Cylinder {
            h,
            r1: n1,
            r2: n2,
            center: self.center(p),
            disc,
        }
    }

    fn polyhedron(&mut self, p: &Params) -> NodeKind {
        let loc = p.loc;
        let mut points = Vec::new();
        let mut faces = Vec::new();
        let pts = self.get(p, "points");
        let Value::Vector(pv) = &pts else {
            let mut t = b"Unable to convert points = ".to_vec();
            self.write_echo_nothrow(&pts, &mut t);
            t.extend_from_slice(b" to a vector of coordinates");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return NodeKind::Polyhedron {
                points,
                faces,
                convexity: 1,
            };
        };
        for pt in pv.iter() {
            let mut xyz = [0.0; 3];
            if !pt.get_vec3_or2(&mut xyz, 0.0) || !xyz.iter().all(|x| x.is_finite()) {
                let mut t = format!("Unable to convert points[{}] = ", points.len()).into_bytes();
                self.write_echo_nothrow(pt, &mut t);
                t.extend_from_slice(b" to a vec3 of numbers");
                self.warn(loc, DiagCode::InvalidArgument, t);
                points.push([0.0; 3]);
            } else {
                points.push(xyz);
            }
        }
        let fv = self.get(p, "faces");
        let Value::Vector(fl) = &fv else {
            let mut t = b"Unable to convert faces = ".to_vec();
            self.write_echo_nothrow(&fv, &mut t);
            t.extend_from_slice(b" to a vector of vector of point indices");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return NodeKind::Polyhedron {
                points,
                faces,
                convexity: 1,
            };
        };
        for (fi, face) in fl.iter().enumerate() {
            let Value::Vector(ix) = face else {
                let mut t = format!("Unable to convert faces[{fi}] = ").into_bytes();
                self.write_echo_nothrow(face, &mut t);
                t.extend_from_slice(b" to a vector of numbers");
                self.warn(loc, DiagCode::InvalidArgument, t);
                continue;
            };
            let mut f = Vec::new();
            for (k, e) in ix.iter().enumerate() {
                match e {
                    Value::Number(x) => {
                        let pi = *x as usize;
                        if pi < points.len() {
                            f.push(pi);
                        } else {
                            let t = format!(
                                "Point index {pi} is out of bounds (from faces[{fi}][{k}])"
                            );
                            self.warn(loc, DiagCode::InvalidArgument, t);
                        }
                    }
                    other => {
                        let mut t = format!("Unable to convert faces[{fi}][{k}] = ").into_bytes();
                        self.write_echo_nothrow(other, &mut t);
                        t.extend_from_slice(b" to a number");
                        self.warn(loc, DiagCode::InvalidArgument, t);
                    }
                }
            }
            if f.len() >= 3 {
                faces.push(f);
            }
        }
        let convexity = (self.get(p, "convexity").to_f64() as i32).max(1);
        NodeKind::Polyhedron {
            points,
            faces,
            convexity,
        }
    }

    fn polygon(&mut self, p: &Params) -> NodeKind {
        let loc = p.loc;
        let mut points = Vec::new();
        let mut paths = Vec::new();
        let pts = self.get(p, "points");
        let Value::Vector(pv) = &pts else {
            let mut t = b"Unable to convert points = ".to_vec();
            self.write_echo_nothrow(&pts, &mut t);
            t.extend_from_slice(b" to a vector of coordinates");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return NodeKind::Polygon {
                points,
                paths,
                convexity: 1,
            };
        };
        for pt in pv.iter() {
            match pt
                .as_vec2(false)
                .filter(|v| v.iter().all(|x| x.is_finite()))
            {
                Some(xy) => points.push(xy),
                None => {
                    let mut t =
                        format!("Unable to convert points[{}] = ", points.len()).into_bytes();
                    self.write_echo_nothrow(pt, &mut t);
                    t.extend_from_slice(b" to a vec2 of numbers");
                    self.warn(loc, DiagCode::InvalidArgument, t);
                    points.push([0.0; 2]);
                }
            }
        }
        match self.get(p, "paths") {
            Value::Vector(pl) => {
                for (pi, path) in pl.iter().enumerate() {
                    let Value::Vector(ix) = path else {
                        let mut t = format!("Unable to convert paths[{pi}] = ").into_bytes();
                        self.write_echo_nothrow(path, &mut t);
                        t.extend_from_slice(b" to a vector of numbers");
                        self.warn(loc, DiagCode::InvalidArgument, t);
                        continue;
                    };
                    let mut out = Vec::new();
                    for (k, e) in ix.iter().enumerate() {
                        match e {
                            Value::Number(x) => {
                                let idx = *x as usize;
                                if idx < points.len() {
                                    out.push(idx);
                                } else {
                                    let t = format!(
                                        "Point index {idx} is out of bounds (from paths[{pi}][{k}])"
                                    );
                                    self.warn(loc, DiagCode::InvalidArgument, t);
                                }
                            }
                            other => {
                                let mut t =
                                    format!("Unable to convert paths[{pi}][{k}] = ").into_bytes();
                                self.write_echo_nothrow(other, &mut t);
                                t.extend_from_slice(b" to a number");
                                self.warn(loc, DiagCode::InvalidArgument, t);
                            }
                        }
                    }
                    paths.push(out);
                }
            }
            Value::Undef => {}
            other => {
                let mut t = b"Unable to convert paths = ".to_vec();
                self.write_echo_nothrow(&other, &mut t);
                t.extend_from_slice(b" to a vector of vector of point indices");
                self.warn(loc, DiagCode::InvalidArgument, t);
                return NodeKind::Polygon {
                    points,
                    paths,
                    convexity: 1,
                };
            }
        }
        let convexity = (self.get(p, "convexity").to_f64() as i32).max(1);
        NodeKind::Polygon {
            points,
            paths,
            convexity,
        }
    }

    fn import(&mut self, p: &Params) -> node::Import {
        let loc = p.loc;
        let v = self.get(p, "file");
        let file = if v.is_defined() {
            let name = self.string_of(&v);
            self.lookup_file(&name, loc)
        } else {
            let f = self.get(p, "filename");
            if f.is_defined() {
                self.emit(
                    lang::diag::Severity::Deprecated,
                    DiagCode::Evaluation,
                    b"filename= is deprecated. Please use file=",
                    None,
                );
            }
            let name = if f.is_undef() {
                Vec::new()
            } else {
                self.string_of(&f)
            };
            self.lookup_file(&name, loc)
        };
        let ext = std::path::Path::new(&file)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let kind = match ext.as_str() {
            "stl" | "off" | "dxf" | "nef3" | "3mf" | "svg" | "obj" => ext.clone(),
            _ => String::new(),
        };
        let disc = self.discretizer(p);
        let layer = {
            let l = self.get(p, "layer");
            if l.is_defined() {
                Some(String::from_utf8_lossy(&self.string_of(&l)).into_owned())
            } else {
                let l = self.get(p, "layername");
                if l.is_defined() {
                    self.emit(
                        lang::diag::Severity::Deprecated,
                        DiagCode::Evaluation,
                        b"layername= is deprecated. Please use layer=",
                        None,
                    );
                    Some(String::from_utf8_lossy(&self.string_of(&l)).into_owned())
                } else {
                    None
                }
            }
        };
        let id = {
            let v = self.get(p, "id");
            v.is_defined()
                .then(|| String::from_utf8_lossy(&self.string_of(&v)).into_owned())
        };
        let convexity = (self.get(p, "convexity").to_f64() as i32).max(1);
        let convexity = if convexity <= 0 { 1 } else { convexity };
        let origin_v = self.get(p, "origin");
        let mut origin = [0.0, 0.0];
        let ok = match origin_v.as_vec2(false) {
            Some(o) => {
                origin = o;
                o.iter().all(|x| x.is_finite())
            }
            None => false,
        };
        if origin_v.is_defined() && !ok {
            let mut t = b"Unable to convert import(..., origin=".to_vec();
            self.write_echo_nothrow(&origin_v, &mut t);
            t.extend_from_slice(b") parameter to vec2");
            self.warn(loc, DiagCode::InvalidArgument, t);
        }
        let center = matches!(self.get(p, "center"), Value::Bool(true));
        let mut scale = self.get(p, "scale").to_f64();
        if scale <= 0.0 {
            scale = 1.0;
        }
        let mut dpi = 72.0;
        if let Value::Number(d) = self.get(p, "dpi") {
            if d < 0.001 {
                let src = &self.units[loc.unit as usize].program.sources;
                let rel = lang::diag::relative_path(src.path(loc.span.file), &self.main_dir)
                    .display()
                    .to_string();
                let mut t = b"Invalid dpi value giving, using default of ".to_vec();
                self.write_echo_nothrow(&origin_v, &mut t);
                t.extend_from_slice(
                    format!(" dpi. Value must be positive and >= 0.001, file {rel}, import() at line {rel}").as_bytes(),
                );
                self.warn_noloc(DiagCode::InvalidArgument, t);
            } else {
                dpi = d;
            }
        }
        let width = self.get(p, "width").as_number().unwrap_or(-1.0);
        let height = self.get(p, "height").as_number().unwrap_or(-1.0);
        node::Import {
            kind,
            file,
            layer,
            id,
            convexity,
            origin,
            scale,
            center,
            dpi,
            width,
            height,
            disc,
        }
    }

    fn text(&mut self, p: &Params) -> node::Text {
        let disc = self.discretizer_quiet(p);
        for (n, t) in [
            ("size", Type::Number),
            ("em", Type::Number),
            ("text", Type::Str),
            ("spacing", Type::Number),
            ("font", Type::Str),
            ("direction", Type::Str),
            ("language", Type::Str),
            ("script", Type::Str),
            ("halign", Type::Str),
            ("valign", Type::Str),
        ] {
            self.valid(p, n, t);
        }
        let em = self.get(p, "em");
        let size = if em.is_defined() {
            if self.get(p, "size").is_defined() {
                let t = format!("{}: \"size\" ignored when \"em\" is set", p.caller);
                self.warn(p.loc, DiagCode::ArgumentMismatch, t);
            }
            em.to_f64() * 72.0 / 100.0
        } else {
            self.get(p, "size").as_number().unwrap_or(10.0)
        };
        let s = |ev: &mut Self, n: &str, d: &str| match ev.get(p, n) {
            Value::Str(x) => String::from_utf8_lossy(x.as_bytes()).into_owned(),
            _ => d.to_string(),
        };
        node::Text {
            text: s(self, "text", ""),
            size,
            spacing: self.get(p, "spacing").as_number().unwrap_or(1.0),
            font: s(self, "font", ""),
            direction: s(self, "direction", ""),
            language: s(self, "language", "en"),
            script: s(self, "script", ""),
            halign: s(self, "halign", "default"),
            valign: s(self, "valign", "default"),
            disc,
        }
    }
}

/// `OpenSCAD::parse_color`: a CSS or `xkcd:` colour name, or `#rgb[a]` /
/// `#rrggbb[aa]`.
pub(crate) fn parse_color(s: &[u8]) -> Option<[f32; 4]> {
    let lower: String = String::from_utf8_lossy(s).to_lowercase();
    let find = |table: &[(&str, [u8; 4])], name: &str| {
        table
            .binary_search_by(|(n, _)| (*n).cmp(name))
            .ok()
            .map(|i| table[i].1)
    };
    let named = lower
        .strip_prefix("xkcd:")
        .and_then(|n| find(super::colors::XKCD, n))
        .or_else(|| find(super::colors::WEB, &lower));
    if let Some(c) = named {
        return Some(c.map(|x| f32::from(x) / 255.0));
    }
    let short = s.len() == 4 || s.len() == 5;
    let long = s.len() == 7 || s.len() == 9;
    if !(short || long) || s[0] != b'#' || !s[1..].iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let stride = if short { 1 } else { 2 };
    let max = if short { 15.0f32 } else { 255.0 };
    let mut rgba = [0.0f32, 0.0, 0.0, 1.0];
    for (k, slot) in rgba.iter_mut().enumerate().take((s.len() - 1) / stride) {
        let chunk = std::str::from_utf8(&s[1 + k * stride..1 + (k + 1) * stride]).ok()?;
        *slot = u32::from_str_radix(chunk, 16).ok()? as f32 / max;
    }
    Some(rgba)
}
