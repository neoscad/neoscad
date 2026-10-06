//! Builtin functions (builtin_functions.cc), with OpenSCAD's messages.

use std::collections::HashMap;
use std::rc::Rc;

use lang::ast::{Arg, ExprId, ExprKind};
use lang::diag::DiagCode;

use crate::call::ArgVal;
use crate::context::Ctx;
use crate::eval::Evaluator;
use crate::features::{Feature, Features};
use crate::fma::{mul_add, mul_sub_mul};
use crate::message::{Loc, R, UnwindKind};
use crate::print::Exhausted;
use crate::rng::hash_float;
use crate::sym::{FxBuild, Sym, Syms};
use crate::trig;
use crate::utf8;
use crate::value::{FunctionValue, Growable, MAX_RANGE_STEPS, ObjectBuilder, Str, Type, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Builtin {
    Abs,
    Sign,
    Rands,
    Min,
    Max,
    Sin,
    Cos,
    Asin,
    Acos,
    Tan,
    Atan,
    Atan2,
    Round,
    Ceil,
    Floor,
    Pow,
    Sqrt,
    Exp,
    Len,
    Log,
    Ln,
    Str,
    Chr,
    Ord,
    Concat,
    Lookup,
    Search,
    Version,
    VersionNum,
    Norm,
    Cross,
    ParentModule,
    IsUndef,
    IsList,
    IsNum,
    IsBool,
    IsString,
    IsFunction,
    DxfDim,
    DxfCross,
    // Experimental: a call warns that it is not enabled unless its
    // feature is on (see `enabled`).
    TextMetrics,
    FontMetrics,
    IsObject,
    Object,
    HasKey,
    Import,
}

impl Builtin {
    /// Experimental functions are known but disabled unless their feature
    /// is on (`--enable`), as in OpenSCAD: `is_object` goes with
    /// `textmetrics` there, not with `object`.
    #[inline]
    pub fn enabled(self, f: Features) -> bool {
        match self {
            Builtin::TextMetrics | Builtin::FontMetrics | Builtin::IsObject => {
                f.has(Feature::TextMetrics)
            }
            Builtin::Object | Builtin::HasKey => f.has(Feature::ObjectFunction),
            Builtin::Import => f.has(Feature::ImportFunction),
            _ => true,
        }
    }

    /// The feature that enables it, for an experimental function.
    pub fn feature(self) -> Option<Feature> {
        match self {
            Builtin::TextMetrics | Builtin::FontMetrics | Builtin::IsObject => {
                Some(Feature::TextMetrics)
            }
            Builtin::Object | Builtin::HasKey => Some(Feature::ObjectFunction),
            Builtin::Import => Some(Feature::ImportFunction),
            _ => None,
        }
    }
}

/// Every registered builtin function, in registration order.
pub(crate) const ALL: [(&str, Builtin); 46] = [
    ("abs", Builtin::Abs),
    ("sign", Builtin::Sign),
    ("rands", Builtin::Rands),
    ("min", Builtin::Min),
    ("max", Builtin::Max),
    ("sin", Builtin::Sin),
    ("cos", Builtin::Cos),
    ("asin", Builtin::Asin),
    ("acos", Builtin::Acos),
    ("tan", Builtin::Tan),
    ("atan", Builtin::Atan),
    ("atan2", Builtin::Atan2),
    ("round", Builtin::Round),
    ("ceil", Builtin::Ceil),
    ("floor", Builtin::Floor),
    ("pow", Builtin::Pow),
    ("sqrt", Builtin::Sqrt),
    ("exp", Builtin::Exp),
    ("len", Builtin::Len),
    ("log", Builtin::Log),
    ("ln", Builtin::Ln),
    ("str", Builtin::Str),
    ("chr", Builtin::Chr),
    ("ord", Builtin::Ord),
    ("concat", Builtin::Concat),
    ("lookup", Builtin::Lookup),
    ("search", Builtin::Search),
    ("version", Builtin::Version),
    ("version_num", Builtin::VersionNum),
    ("norm", Builtin::Norm),
    ("cross", Builtin::Cross),
    ("parent_module", Builtin::ParentModule),
    ("is_undef", Builtin::IsUndef),
    ("is_list", Builtin::IsList),
    ("is_num", Builtin::IsNum),
    ("is_bool", Builtin::IsBool),
    ("is_string", Builtin::IsString),
    ("is_function", Builtin::IsFunction),
    ("dxf_dim", Builtin::DxfDim),
    ("dxf_cross", Builtin::DxfCross),
    ("textmetrics", Builtin::TextMetrics),
    ("fontmetrics", Builtin::FontMetrics),
    ("is_object", Builtin::IsObject),
    ("object", Builtin::Object),
    ("has_key", Builtin::HasKey),
    ("import", Builtin::Import),
];

pub(crate) fn table(syms: &mut Syms) -> HashMap<Sym, Builtin, FxBuild> {
    ALL.into_iter().map(|(n, b)| (syms.intern(n), b)).collect()
}

impl<'a> Evaluator<'a> {
    /// `print_argCnt_warning`.
    fn arg_count_warning(&mut self, name: &str, found: usize, expected: &str, loc: Loc) {
        let t = format!(
            "{name}() number of parameters does not match: expected {expected}, found {found}"
        );
        self.warn(loc, DiagCode::ArgumentMismatch, t);
    }

    /// `print_argConvert_positioned_warning`.
    fn arg_type_warning(
        &mut self,
        name: &str,
        what: &str,
        found: &Value,
        expected: Type,
        loc: Loc,
    ) {
        let mut t = format!(
            "{name}() parameter could not be converted: {what}: expected {}, found {} (",
            expected.name(),
            found.type_name()
        )
        .into_bytes();
        self.write_echo_nothrow(found, &mut t);
        t.push(b')');
        self.warn(loc, DiagCode::InvalidArgument, t);
    }

    /// `check_arguments(name, arguments, loc, {types...})`.
    fn check(&mut self, name: &str, args: &[ArgVal], loc: Loc, types: &[Type]) -> bool {
        if args.len() != types.len() {
            self.arg_count_warning(name, args.len(), &types.len().to_string(), loc);
            return false;
        }
        for (i, (a, &t)) in args.iter().zip(types).enumerate() {
            if a.value.ty() != t {
                let v = a.value.clone();
                self.arg_type_warning(name, &format!("argument {i}"), &v, t, loc);
                return false;
            }
        }
        true
    }

    fn check_count(&mut self, name: &str, args: &[ArgVal], loc: Loc, n: usize) -> bool {
        if args.len() != n {
            self.arg_count_warning(name, args.len(), &n.to_string(), loc);
            return false;
        }
        true
    }

    /// A one-number function.
    fn num1(&mut self, name: &str, args: &[ArgVal], loc: Loc, f: impl Fn(f64) -> f64) -> Value {
        if self.check(name, args, loc, &[Type::Number]) {
            Value::Number(f(args[0].value.to_f64()))
        } else {
            Value::Undef
        }
    }

    pub fn call_builtin(
        &mut self,
        b: Builtin,
        u: u32,
        call: ExprId,
        args: &'a [Arg],
        ctx: &Rc<Ctx>,
    ) -> R<Value> {
        let loc = self.expr_loc(u, call);
        if b == Builtin::Object {
            return self.object_function(u, args, ctx, loc);
        }
        if b == Builtin::IsUndef {
            if args.len() != 1 {
                self.arg_count_warning("is_undef", args.len(), "1", loc);
                return Ok(Value::Undef);
            }
            let ast = self.units[u as usize].ast;
            if let ExprKind::Var(n) = ast.expr(args[0].expr).kind {
                let s = self.units[u as usize].sym(n);
                return Ok(Value::Bool(
                    self.read_var(u, args[0].expr, s, ctx)
                        .is_none_or(|v| v.is_undef()),
                ));
            }
            let v = self.eval(u, args[0].expr, ctx)?;
            return Ok(Value::Bool(v.is_undef()));
        }
        // The argument vector comes from `arg_pool`, as a user call's does
        // (`call_frame`): a builtin call is the commonest call there is
        // (`min`, `abs`, `len` in every BOSL2 loop), and a fresh vector
        // per call was a malloc and free on each. It goes back to the pool
        // on every path, errors included, or the pool would drain.
        let mut argv = self.arg_pool.pop().unwrap_or_default();
        let r = match self.eval_args_into(u, args, ctx, &mut argv) {
            Ok(()) => self.apply_builtin(b, loc, &mut argv),
            Err(e) => Err(e),
        };
        argv.clear();
        self.arg_pool.push(argv);
        r
    }

    /// Builtin `b` applied to its evaluated arguments. They are borrowed
    /// from the caller's pooled vector; `concat` drains it, because it must
    /// own its first list to grow it in place, and the DXF builtins take it.
    /// Forced inline: it was `call_builtin`'s body until the vector was
    /// pooled, and the evaluator's speed is sensitive to how LLVM inlines
    /// its hot functions (`docs/audits/bytecode-vm.md` §3.6).
    #[inline(always)]
    pub(crate) fn apply_builtin(&mut self, b: Builtin, loc: Loc, a: &mut Vec<ArgVal>) -> R<Value> {
        use Builtin::*;
        Ok(match b {
            Abs => self.num1("abs", a, loc, f64::abs),
            Sign => self.num1("sign", a, loc, |x| {
                if x < 0.0 {
                    -1.0
                } else if x > 0.0 {
                    1.0
                } else {
                    0.0
                }
            }),
            Sin => self.num1("sin", a, loc, trig::sin_degrees),
            Cos => self.num1("cos", a, loc, trig::cos_degrees),
            Asin => self.num1("asin", a, loc, trig::asin_degrees),
            Acos => self.num1("acos", a, loc, trig::acos_degrees),
            Tan => self.num1("tan", a, loc, trig::tan_degrees),
            Atan => self.num1("atan", a, loc, trig::atan_degrees),
            Round => self.num1("round", a, loc, f64::round),
            Ceil => self.num1("ceil", a, loc, f64::ceil),
            Floor => self.num1("floor", a, loc, f64::floor),
            Sqrt => self.num1("sqrt", a, loc, f64::sqrt),
            Exp => self.num1("exp", a, loc, f64::exp),
            Ln => self.num1("ln", a, loc, f64::ln),
            Atan2 => {
                if self.check("atan2", a, loc, &[Type::Number, Type::Number]) {
                    Value::Number(trig::atan2_degrees(
                        a[0].value.to_f64(),
                        a[1].value.to_f64(),
                    ))
                } else {
                    Value::Undef
                }
            }
            Pow => {
                if self.check("pow", a, loc, &[Type::Number, Type::Number]) {
                    Value::Number(a[0].value.to_f64().powf(a[1].value.to_f64()))
                } else {
                    Value::Undef
                }
            }
            Log => {
                let (base, x) = if a.len() == 1 {
                    if !self.check("log", a, loc, &[Type::Number]) {
                        return Ok(Value::Undef);
                    }
                    (10.0, a[0].value.to_f64())
                } else {
                    if !self.check("log", a, loc, &[Type::Number, Type::Number]) {
                        return Ok(Value::Undef);
                    }
                    (a[0].value.to_f64(), a[1].value.to_f64())
                };
                Value::Number(x.ln() / f64::ln(base))
            }
            Len => match (a.len(), a.first().map(|x| &x.value)) {
                (1, Some(Value::Vector(v))) => Value::Number(v.len() as f64),
                (1, Some(Value::Object(o))) => Value::Number(o.len() as f64),
                _ => {
                    if self.check("len", a, loc, &[Type::Str]) {
                        let s = a[0].value.as_str().map_or(0, |s| s.char_count());
                        Value::Number(s as f64)
                    } else {
                        Value::Undef
                    }
                }
            },
            Min | Max => self.min_max(b == Min, a, loc),
            Rands => self.rands(a, loc),
            Str => {
                let mut out = Vec::new();
                for x in a.iter() {
                    match self.write_string(&x.value, &mut out) {
                        Ok(()) => {}
                        Err(Exhausted::Stack) => {
                            self.log_exhausted();
                            return Err(self.unwind(UnwindKind::EchoStack));
                        }
                        Err(Exhausted::Long(n)) => {
                            self.printed_too_long(n, Some(loc), "str()");
                            return Ok(Value::Undef);
                        }
                        Err(Exhausted::Stopped) => {
                            self.check_limits(Some(loc))?;
                            self.check_interrupt()?;
                            return Ok(Value::Undef);
                        }
                    }
                    // Each argument is bounded by the limits already, so
                    // the text built so far is at most one over them.
                    if !self.string_fits(out.len(), loc, "str()") {
                        return Ok(Value::Undef);
                    }
                }
                Value::Str(crate::value::Str::from_vec(out))
            }
            Chr => {
                let mut out = Vec::new();
                let mut w = ChrWalk::default();
                for x in a.iter() {
                    self.chr_into(&x.value, &mut out, &mut w);
                }
                if w.stopped {
                    self.check_limits(Some(loc))?;
                    self.check_interrupt()?;
                }
                if !self.string_fits(out.len(), loc, "chr()") {
                    return Ok(Value::Undef);
                }
                Value::Str(crate::value::Str::from_vec(out))
            }
            Ord => {
                if !self.check("ord", a, loc, &[Type::Str]) {
                    return Ok(Value::Undef);
                }
                let s = a[0]
                    .value
                    .as_str()
                    .map(|s| s.as_bytes())
                    .unwrap_or_default();
                if !utf8::validate(s) {
                    let mut t = b"ord() argument '".to_vec();
                    t.extend_from_slice(s);
                    t.extend_from_slice(b"' is not a valid utf8 string");
                    self.warn(loc, DiagCode::InvalidArgument, t);
                    return Ok(Value::Undef);
                }
                if utf8::char_count(s) == 0 {
                    return Ok(Value::Undef);
                }
                Value::Number(f64::from(utf8::first_char(s)))
            }
            Concat => {
                // Before anything is copied: doubling a list forty times
                // asks for a trillion elements.
                let n: usize = a
                    .iter()
                    .map(|x| match &x.value {
                        Value::Vector(v) => v.len(),
                        _ => 1,
                    })
                    .fold(0usize, usize::saturating_add);
                if !self.list_fits(n, loc, "concat()")
                    || !self.memory_fits((n * std::mem::size_of::<Value>()) as u64, loc, "concat()")
                {
                    return Ok(Value::Undef);
                }
                // A first list that nothing else holds is appended to in
                // place (see `value::Growable`), which keeps a tail-recursive
                // `concat(acc, [x])` linear; any other is copied into a list
                // of exactly the final size.
                let mut a = a.drain(..);
                let mut out = match a.next().map(|x| x.value) {
                    None => return Ok(Value::vector(Vec::new())),
                    Some(Value::Vector(v)) => match v.into_growable() {
                        Ok(g) => g,
                        Err(v) => {
                            let mut g = Growable::with_capacity(n);
                            g.extend(v.iter().cloned());
                            g
                        }
                    },
                    Some(other) => {
                        let mut g = Growable::with_capacity(n);
                        g.push(other);
                        g
                    }
                };
                out.reserve(n - out.len());
                for x in a {
                    match x.value {
                        Value::Vector(v) => out.extend(v.into_vec()),
                        other => out.push(other),
                    }
                }
                Value::Vector(out.finish())
            }
            Lookup => self.lookup_fn(a, loc),
            Search => self.search(a, loc),
            Version => self.version_value(),
            VersionNum => {
                let v = if a.is_empty() {
                    self.version_value()
                } else {
                    a[0].value.clone()
                };
                let mut ymd = [0.0; 3];
                if !v.get_vec3_or2(&mut ymd, 0.0) {
                    return Ok(Value::Undef);
                }
                Value::Number(ymd[0] * 10000.0 + ymd[1] * 100.0 + ymd[2])
            }
            ParentModule => {
                let d = if a.is_empty() {
                    1.0
                } else if !self.check("parent_module", a, loc, &[Type::Number]) {
                    return Ok(Value::Undef);
                } else {
                    a[0].value.to_f64()
                };
                let n = d.trunc() as i32;
                let s = self.module_names.len() as i32;
                if n < 0 {
                    self.warn(
                        loc,
                        DiagCode::InvalidArgument,
                        format!("Negative parent module index ({n}) not allowed"),
                    );
                    return Ok(Value::Undef);
                }
                if n >= s {
                    let t = format!(
                        "Parent module index ({n}) greater than the number of modules on the stack"
                    );
                    self.warn(loc, DiagCode::InvalidArgument, t);
                    return Ok(Value::Undef);
                }
                // The name found belongs to a caller, which a memoised
                // call's key does not see (`crate::callmemo`).
                self.cm.module_read((s - 1 - n) as usize);
                let name = self.module_names[(s - 1 - n) as usize];
                Value::str(self.name(name).as_bytes())
            }
            Norm => {
                if !self.check("norm", a, loc, &[Type::Vector]) {
                    return Ok(Value::Undef);
                }
                let mut sum = 0.0;
                for e in a[0]
                    .value
                    .as_vector()
                    .map(|v| v.as_slice())
                    .unwrap_or_default()
                {
                    match e {
                        Value::Number(x) => sum = mul_add(*x, *x, sum),
                        _ => {
                            self.warn(
                                loc,
                                DiagCode::InvalidArgument,
                                "Incorrect arguments to norm()",
                            );
                            return Ok(Value::Undef);
                        }
                    }
                }
                Value::Number(sum.sqrt())
            }
            Cross => self.cross(a, loc),
            IsList => self.is_type(a, loc, "is_list", |v| matches!(v, Value::Vector(_))),
            IsNum => self.is_type(
                a,
                loc,
                "is_num",
                |v| matches!(v, Value::Number(x) if !x.is_nan()),
            ),
            IsBool => self.is_type(a, loc, "is_bool", |v| matches!(v, Value::Bool(_))),
            IsString => self.is_type(a, loc, "is_string", |v| matches!(v, Value::Str(_))),
            IsFunction => self.is_type(a, loc, "is_function", |v| matches!(v, Value::Function(_))),
            DxfDim | DxfCross => self.dxf(b == DxfDim, std::mem::take(a), loc),
            IsObject => self.is_type(a, loc, "is_object", |v| matches!(v, Value::Object(_))),
            HasKey => {
                if self.check("has_key", a, loc, &[Type::Object, Type::Str]) {
                    match (&a[0].value, &a[1].value) {
                        (Value::Object(o), Value::Str(k)) => Value::Bool(o.contains(k.as_bytes())),
                        _ => Value::Undef,
                    }
                } else {
                    Value::Undef
                }
            }
            TextMetrics => self.metrics(std::mem::take(a), loc, false),
            FontMetrics => self.metrics(std::mem::take(a), loc, true),
            Import => self.import_function(std::mem::take(a), loc),
            // Evaluated from their unevaluated arguments in `call_builtin`.
            IsUndef | Object => Value::Undef,
        })
    }

    /// `dxf_dim()` and `dxf_cross()` (io/dxfdim.cc), with its multiply-adds
    /// rounded as the platform's OpenSCAD build rounds them (see `fma`).
    fn dxf(&mut self, dim: bool, a: Vec<ArgVal>, loc: Loc) -> Value {
        // Reads a file, whose content no fingerprint covers.
        self.untracked();
        let fname = if dim { "dxf_dim" } else { "dxf_cross" };
        let names = ["file", "layer", "origin", "scale", "name"];
        let syms: Vec<Sym> = names.iter().map(|n| self.syms.intern(n)).collect();
        let frame = self.bind_builtin(a, loc, &[], &syms, true);
        let get = |i: usize| frame.get(syms[i]).cloned();
        let mut raw = Vec::new();
        if let Some(f) = get(0)
            && let Err(e @ Exhausted::Long(_)) = self.write_string(&f, &mut raw)
        {
            self.print_failed(e, &format!("{fname}()"));
        }
        let file = self.lookup_file_bytes(&raw, loc);
        let (mut xo, mut yo) = (0.0, 0.0);
        if let Some(o) = get(2) {
            let ok = o.get_vec2(&mut xo, &mut yo, false) && xo.is_finite() && yo.is_finite();
            if !ok {
                let mut t = format!("{fname}(..., origin=").into_bytes();
                self.write_echo_nothrow(&o, &mut t);
                t.extend_from_slice(b") could not be converted");
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
        }
        let text = |v: Option<Value>| match v {
            Some(Value::Str(s)) => String::from_utf8_lossy(s.as_bytes()).into_owned(),
            _ => String::new(),
        };
        let layer = text(get(1));
        let scale = get(3).and_then(|v| v.as_number()).unwrap_or(1.0);
        let name = text(get(4));
        let raw_s = String::from_utf8_lossy(&raw).into_owned();
        let path = std::path::Path::new(&file);
        let fs = self.opts.fs.clone();
        if file.is_empty() || !fs.exists(path) {
            let mut t = b"Can't open DXF file '".to_vec();
            t.extend_from_slice(&raw);
            t.extend_from_slice(b"'!");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return Value::Undef;
        }
        let bytes = if fs.is_dir(path) {
            None
        } else {
            fs.read(path).ok()
        };
        let display = lang::diag::relative_display(path, &self.main_dir, &*fs);
        let mut warnings = Vec::new();
        let req = io::dxf::Request {
            file: &file,
            display: &display,
            layer: &layer,
            origin: [xo, yo],
            scale,
        };
        // `dxf_dim`/`dxf_cross` read with `CurveDiscretizer(36)` (dxfdim.cc).
        let data = io::dxf::read(bytes.as_deref(), &req, &io::dxf::Fixed36, &mut |w| {
            warnings.push(w)
        });
        for w in warnings {
            self.warn_noloc(DiagCode::InvalidArgument, w);
        }
        if dim {
            for d in &data.dims {
                if !name.is_empty() && d.name != name {
                    continue;
                }
                let c = &d.coords;
                let v = match d.ty & 7 {
                    0 => {
                        let (x, y) = (c[4][0] - c[3][0], c[4][1] - c[3][1]);
                        Some(
                            mul_add(
                                x,
                                trig::cos_degrees(d.angle),
                                y * trig::sin_degrees(d.angle),
                            )
                            .abs(),
                        )
                    }
                    1 => {
                        let (x, y) = (c[4][0] - c[3][0], c[4][1] - c[3][1]);
                        Some(mul_add(x, x, y * y).sqrt())
                    }
                    2 => {
                        let a1 = trig::atan2_degrees(c[0][0] - c[5][0], c[0][1] - c[5][1]);
                        let a2 = trig::atan2_degrees(c[4][0] - c[3][0], c[4][1] - c[3][1]);
                        Some((a1 - a2).abs())
                    }
                    3 | 4 => {
                        let (x, y) = (c[5][0] - c[0][0], c[5][1] - c[0][1]);
                        Some(mul_add(x, x, y * y).sqrt())
                    }
                    6 => Some(if d.ty & 64 != 0 { c[3][0] } else { c[3][1] }),
                    _ => None,
                };
                if let Some(v) = v {
                    return Value::Number(v);
                }
                let t = format!(
                    "Dimension '{name}' in '{raw_s}', layer '{layer}' has unsupported type!"
                );
                self.warn(loc, DiagCode::InvalidArgument, t);
                return Value::Undef;
            }
            let t = format!("Can't find dimension '{name}' in '{raw_s}', layer '{layer}'!");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return Value::Undef;
        }
        let mut coords = [[0.0f64; 2]; 4];
        let mut j = 0;
        for path in &data.paths {
            if path.indices.len() != 2 {
                continue;
            }
            coords[j] = data.points[path.indices[0]];
            coords[j + 1] = data.points[path.indices[1]];
            j += 2;
            if j == 4 {
                let [[x1, y1], [x2, y2], [x3, y3], [x4, y4]] = coords;
                let dem = mul_sub_mul(y4 - y3, x2 - x1, x4 - x3, y2 - y1);
                if dem == 0.0 {
                    break;
                }
                let ua = mul_sub_mul(x4 - x3, y1 - y3, y4 - y3, x1 - x3) / dem;
                return Value::vector(vec![
                    Value::Number(mul_add(ua, x2 - x1, x1)),
                    Value::Number(mul_add(ua, y2 - y1, y1)),
                ]);
            }
        }
        let t = format!("Can't find cross in '{raw_s}', layer '{layer}'!");
        self.warn(loc, DiagCode::InvalidArgument, t);
        Value::Undef
    }

    /// `builtin_object`: named arguments set their key; an unnamed one
    /// copies an object's entries, or applies a list of `[key, value]`
    /// (set) and `[key]` (delete) entries. Arguments are evaluated one at
    /// a time and the first bad one ends the call, with the rest never
    /// evaluated, as in OpenSCAD.
    #[inline(never)]
    fn object_function(&mut self, u: u32, args: &'a [Arg], ctx: &Rc<Ctx>, loc: Loc) -> R<Value> {
        let mut b = ObjectBuilder::new();
        for (n, a) in args.iter().enumerate() {
            let v = self.eval(u, a.expr, ctx)?;
            if !self.object_arg(&mut b, u, a, n, v, loc) {
                return Ok(Value::Undef);
            }
        }
        Ok(self.object_finish(b))
    }

    /// `object()`'s argument `n`, evaluated to `v`, applied to `b`; false
    /// (with the warning) when it is a bad one, which ends the call with
    /// `undef`. Shared with the heap evaluator (`heap_expr`), which
    /// evaluates the arguments that may call on its own stack.
    pub(crate) fn object_arg(
        &mut self,
        b: &mut ObjectBuilder,
        u: u32,
        a: &Arg,
        n: usize,
        v: Value,
        loc: Loc,
    ) -> bool {
        match a.name {
            Some(name) => {
                let ast = self.units[u as usize].ast;
                b.set(Str::new(ast.name(name).as_bytes()), v);
                true
            }
            None => match object_unnamed(b, &v, n) {
                Ok(()) => true,
                Err(e) => {
                    self.warn(loc, DiagCode::InvalidArgument, e);
                    false
                }
            },
        }
    }

    /// `object()`'s result, once every argument is applied.
    pub(crate) fn object_finish(&self, b: ObjectBuilder) -> Value {
        Value::Object(b.finish(|f| self.is_method_literal(f)))
    }

    /// Whether a function literal has a parameter named `this`, which
    /// makes it a method when stored in an object (see `value::Object`).
    fn is_method_literal(&self, f: &FunctionValue) -> bool {
        let ast = self.units[f.unit as usize].ast;
        match &ast.expr(f.expr).kind {
            ExprKind::Function(params, _) => params.iter().any(|p| ast.name(p.name) == "this"),
            _ => false,
        }
    }

    /// `textmetrics()` and `fontmetrics()` (`builtin_textmetrics`,
    /// `builtin_fontmetrics`): `text()`'s parameter handling, then the
    /// measurements `text()` would draw with, as an object.
    fn metrics(&mut self, a: Vec<ArgVal>, loc: Loc, font_only: bool) -> Value {
        // The result depends on font files, which no fingerprint covers.
        self.untracked();
        let t = self.text_params(a, loc, font_only);
        let db =
            self.opts.fonts.clone().unwrap_or_else(|| {
                std::sync::Arc::new(text::FontDb::with_fs(self.opts.fs.clone()))
            });
        let num = Value::Number;
        let pair = |x: [f64; 2]| Value::vector(vec![Value::Number(x[0]), Value::Number(x[1])]);
        let object = |entries: Vec<(&str, Value)>| {
            let mut b = ObjectBuilder::new();
            for (k, v) in entries {
                b.set(Str::new(k.as_bytes()), v);
            }
            Value::Object(b.finish(|_| false))
        };
        if font_only {
            let (m, msgs) = text::font_metrics(&db, &t.font, t.size);
            self.text_messages(msgs, loc);
            let Some(m) = m else {
                return Value::Undef;
            };
            return object(vec![
                (
                    "nominal",
                    object(vec![
                        ("ascent", num(m.nominal_ascent)),
                        ("descent", num(m.nominal_descent)),
                    ]),
                ),
                (
                    "max",
                    object(vec![
                        ("ascent", num(m.max_ascent)),
                        ("descent", num(m.max_descent)),
                    ]),
                ),
                ("interline", num(m.interline)),
                (
                    "font",
                    object(vec![
                        ("family", Value::str(m.family.as_bytes())),
                        ("style", Value::str(m.style.as_bytes())),
                    ]),
                ),
            ]);
        }
        let (script, direction) = crate::text_props::resolve(&t);
        let p = text::Params {
            text: &t.text,
            size: t.size,
            spacing: t.spacing,
            font: &t.font,
            direction,
            language: &t.language,
            script: &script,
            halign: &t.halign,
            valign: &t.valign,
            segments: text::segments_for(None),
        };
        let (m, msgs) = text::text_metrics(&db, &p);
        self.text_messages(msgs, loc);
        let Some(m) = m else {
            return Value::Undef;
        };
        object(vec![
            ("position", pair(m.position)),
            ("size", pair(m.size)),
            ("ascent", num(m.ascent)),
            ("descent", num(m.descent)),
            ("offset", pair(m.offset)),
            ("advance", pair(m.advance)),
        ])
    }

    /// The font and shaping messages of a metrics call, at the call.
    fn text_messages(&mut self, msgs: Vec<text::Message>, loc: Loc) {
        for m in msgs {
            match m.level {
                text::Level::Warning => self.warn(loc, DiagCode::InvalidArgument, m.text),
                // OpenSCAD prints `FONT-WARNING:` lines with no location,
                // a message group the evaluator's output has no severity
                // for; only an unparseable font name gives one, and its
                // "Can't get font" warning follows it regardless.
                text::Level::FontWarning => {}
            }
        }
    }

    /// `import()` as a function (`builtin_import`): a JSON file's values.
    fn import_function(&mut self, a: Vec<ArgVal>, loc: Loc) -> Value {
        // Reads a file, whose content no fingerprint covers.
        self.untracked();
        let file_sym = self.syms.intern("file");
        let type_sym = self.syms.intern("type");
        let frame = self.bind_builtin(a, loc, &[], &[file_sym, type_sym], true);
        // `Parameters::get(name, "")`: a string, or empty.
        let string = |s| match frame.get(s) {
            Some(Value::Str(x)) => x.as_bytes().to_vec(),
            _ => Vec::new(),
        };
        let raw = string(file_sym);
        let mut ty = string(type_sym);
        let file = self.lookup_file_bytes(&raw, loc);
        let raw = String::from_utf8_lossy(&raw).into_owned();
        if ty.is_empty() {
            let ext = extension(&file).to_ascii_lowercase();
            if ext == ".json" {
                ty = b"json".to_vec();
            } else if ext.is_empty() {
                let t = format!("No file extension or type while trying to import '{raw}'");
                self.warn(loc, DiagCode::InvalidArgument, t);
                return Value::Undef;
            } else {
                let t =
                    format!("Unsupported file extension '{ext}' while trying to import '{raw}'");
                self.warn(loc, DiagCode::InvalidArgument, t);
                return Value::Undef;
            }
        }
        if ty != b"json" {
            let t = format!(
                "Unsupported file type '{}' while trying to import '{raw}'",
                String::from_utf8_lossy(&ty)
            );
            self.warn(loc, DiagCode::InvalidArgument, t);
            return Value::Undef;
        }
        // `std::ifstream` opens a directory and reads nothing from it.
        let fs = self.opts.fs.clone();
        let path = std::path::Path::new(&file);
        let bytes = if file.is_empty() {
            None
        } else if fs.is_dir(path) {
            Some(Vec::new())
        } else {
            fs.read(path).ok()
        };
        let Some(bytes) = bytes else {
            self.warn(
                loc,
                DiagCode::InvalidArgument,
                format!("Could not read file '{file}'"),
            );
            return Value::Undef;
        };
        match crate::json::parse(&bytes) {
            Ok(v) => v,
            Err(crate::json::Failed::Parse(e)) => {
                let mut t = format!("Failed to parse file '{file}': ").into_bytes();
                t.extend_from_slice(&e);
                self.warn(loc, DiagCode::InvalidArgument, t);
                Value::Undef
            }
            // The evaluator's next check reports the limit.
            Err(crate::json::Failed::Memory) => Value::Undef,
        }
    }

    /// `lookup_file`: a path relative to the calling file's directory.
    pub(crate) fn lookup_file_bytes(&self, name: &[u8], loc: Loc) -> String {
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

    fn is_type(&mut self, a: &[ArgVal], loc: Loc, name: &str, f: impl Fn(&Value) -> bool) -> Value {
        if self.check_count(name, a, loc, 1) {
            Value::Bool(f(&a[0].value))
        } else {
            Value::Undef
        }
    }

    fn version_value(&self) -> Value {
        Value::vector(
            self.opts
                .version
                .iter()
                .map(|&x| Value::Number(x))
                .collect(),
        )
    }

    /// `min_max_arguments` and the reduction.
    fn min_max(&mut self, is_min: bool, a: &[ArgVal], loc: Loc) -> Value {
        let name = if is_min { "min" } else { "max" };
        let mut values = Vec::new();
        if a.is_empty() {
            self.arg_count_warning(name, 0, "at least 1", loc);
            return Value::Undef;
        } else if let (1, Value::Vector(v)) = (a.len(), &a[0].value) {
            if v.is_empty() {
                self.arg_count_warning(name, 0, "at least 1 vector element", loc);
                return Value::Undef;
            }
            for (i, e) in v.iter().enumerate() {
                match e {
                    Value::Number(x) => values.push(*x),
                    _ => {
                        let e = e.clone();
                        self.arg_type_warning(
                            name,
                            &format!("vector element {i}"),
                            &e,
                            Type::Number,
                            loc,
                        );
                        return Value::Undef;
                    }
                }
            }
        } else {
            for (i, x) in a.iter().enumerate() {
                match x.value {
                    Value::Number(n) => values.push(n),
                    _ => {
                        let v = x.value.clone();
                        self.arg_type_warning(
                            name,
                            &format!("argument {i}"),
                            &v,
                            Type::Number,
                            loc,
                        );
                        return Value::Undef;
                    }
                }
            }
        }
        // std::min_element / max_element: the first extreme by `<`.
        let mut best = values[0];
        for &v in &values[1..] {
            if (is_min && v < best) || (!is_min && best < v) {
                best = v;
            }
        }
        Value::Number(best)
    }

    fn rands(&mut self, a: &[ArgVal], loc: Loc) -> Value {
        // The generator is shared by the whole evaluation: a seed resets it
        // for later statements, and a draw moves it on (see `crate::memo`).
        self.untracked();
        if a.len() < 3 || a.len() > 4 {
            self.arg_count_warning("rands", a.len(), "3 or 4", loc);
            return Value::Undef;
        }
        let types = [Type::Number; 4];
        if !self.check("rands", a, loc, &types[..a.len()]) {
            return Value::Undef;
        }
        let mut min = a[0].value.to_f64();
        if !min.is_finite() {
            self.warn(
                loc,
                DiagCode::InvalidArgument,
                "rands() range min cannot be infinite",
            );
            min = -f64::MAX / 2.0;
            self.warn_noloc(DiagCode::InvalidArgument, format!("resetting to {min:.6}"));
        }
        let mut max = a[1].value.to_f64();
        if !max.is_finite() {
            self.warn(
                loc,
                DiagCode::InvalidArgument,
                "rands() range max cannot be infinite",
            );
            max = f64::MAX / 2.0;
            self.warn_noloc(DiagCode::InvalidArgument, format!("resetting to {max:.6}"));
        }
        if max < min {
            std::mem::swap(&mut min, &mut max);
        }
        let mut n = a[2].value.to_f64().abs();
        if !n.is_finite() {
            self.warn(
                loc,
                DiagCode::InvalidArgument,
                "rands() cannot create an infinite number of results",
            );
            self.warn_noloc(
                DiagCode::InvalidArgument,
                "resetting number of results to 1",
            );
            n = 1.0;
        }
        // Before allocating: `rands(0, 1, 1e9)` is 16 GB of numbers.
        if n > self.caps.rands {
            self.over_limit(crate::limits::Limit::Rands, n.floor(), loc, "rands()");
            return Value::Undef;
        }
        let n = n as usize;
        if !self.list_fits(n, loc, "rands()")
            || !self.memory_fits((n * std::mem::size_of::<Value>()) as u64, loc, "rands()")
        {
            return Value::Undef;
        }
        if a.len() > 3 {
            let seed = hash_float(a[3].value.to_f64()) as u32;
            self.rng.seed(seed);
        }
        let mut out = Vec::with_capacity(n.min(1 << 20));
        if min >= max {
            out.resize(n, Value::Number(min));
        } else {
            for _ in 0..n {
                out.push(Value::Number(self.rng.uniform(min, max)));
            }
        }
        Value::vector(out)
    }

    /// `Value::chrString`.
    ///
    /// Lists share their elements, so a list can have 2^depth paths
    /// (`t = [t, t]`): `chr()` of such a tree of zeros, which prints
    /// nothing, walked every path in one uninterruptible call. A list
    /// found to print nothing is remembered (by address; empty output is
    /// empty whatever else holds the list) and skipped after that, unless
    /// it warned (a range too long to expand warns at every occurrence,
    /// as in OpenSCAD), and the
    /// walk polls the cancel flag, the time limit and the memory limit (the
    /// text being built is not a value yet, so nothing else counts it),
    /// and stops once the text is past the string limit, which it could
    /// only fail.
    ///
    /// The walk keeps the lists it is inside on a stack of its own rather
    /// than recursing: a list can nest as deep as a recursion can build it
    /// (`[nest(n - 1)]` to the counted limit), deeper than a browser
    /// worker's stack holds a recursive walk.
    fn chr_into(&mut self, v: &Value, out: &mut Vec<u8>, w: &mut ChrWalk) {
        /// A list being walked: its elements, the next one, its key, and
        /// the text's length and the warnings when it started.
        struct Open<'v> {
            items: &'v [Value],
            i: usize,
            key: usize,
            start: usize,
            warnings: u32,
        }
        if w.stopped {
            return;
        }
        let mut open: Vec<Open<'_>> = Vec::new();
        let mut next = Some(v);
        loop {
            match next.take() {
                Some(Value::Number(x)) => {
                    if *x > 0.0 {
                        utf8::encode(*x as u32, out);
                    }
                }
                Some(Value::Vector(items)) => {
                    let key = items.as_slice().as_ptr() as usize;
                    if !w.silent.contains(&key) {
                        open.push(Open {
                            items,
                            i: 0,
                            key,
                            start: out.len(),
                            warnings: w.warnings,
                        });
                    }
                }
                Some(Value::Range(r)) => {
                    let steps = r.num_values();
                    if steps >= MAX_RANGE_STEPS {
                        let t = format!(
                            "Bad range parameter in for statement: too many elements ({steps})."
                        );
                        self.warn_noloc(DiagCode::IterationLimit, t);
                        w.warnings += 1;
                    } else {
                        for d in r.iter() {
                            if d > 0.0 {
                                utf8::encode(d as u32, out);
                            }
                        }
                    }
                }
                _ => {}
            }
            let Some(top) = open.last_mut() else {
                return;
            };
            let Some(e) = top.items.get(top.i) else {
                if out.len() == top.start && w.warnings == top.warnings {
                    w.silent.insert(top.key);
                }
                open.pop();
                continue;
            };
            top.i += 1;
            w.steps += 1;
            if w.steps.is_multiple_of(4096) && self.chr_stopped(out.capacity()) {
                w.stopped = true;
            }
            if w.stopped || out.len() > self.caps.string {
                // Once stopped nothing more is written, so the lists still
                // open are not remembered as silent.
                w.stopped = true;
                return;
            }
            next = Some(e);
        }
    }

    /// Whether `chr()` should stop: cancelled, out of time, or the text
    /// built so far passes the memory limit.
    fn chr_stopped(&self, text: usize) -> bool {
        crate::limits::live::passes(text as u64)
            || self.interrupted()
            || self
                .opts
                .guard
                .as_deref()
                .is_some_and(crate::limits::Guard::over_time)
    }

    fn lookup_fn(&mut self, a: &[ArgVal], loc: Loc) -> Value {
        if !self.check("lookup", a, loc, &[Type::Number, Type::Vector]) {
            return Value::Undef;
        }
        let p = a[0].value.to_f64();
        if !p.is_finite() {
            let mut t = b"lookup(".to_vec();
            let v = a[0].value.clone();
            self.write_echo_nothrow(&v, &mut t);
            t.extend_from_slice(b", ...) first argument is not a number");
            self.warn(loc, DiagCode::InvalidArgument, t);
            return Value::Undef;
        }
        let empty = crate::value::Vector::empty();
        let vec = a[1].value.as_vector().unwrap_or(&empty);
        let Some(first) = vec.first() else {
            return Value::Undef;
        };
        if first.as_vector().map_or(0, |v| v.len()) < 2 {
            return Value::Undef;
        }
        let Some([mut low_p, mut low_v]) = first.as_vec2(false) else {
            return Value::Undef;
        };
        let (mut high_p, mut high_v) = (low_p, low_v);
        for e in &vec[1..] {
            if let Some([tp, tv]) = e.as_vec2(false) {
                if tp <= p && (tp > low_p || low_p > p) {
                    low_p = tp;
                    low_v = tv;
                }
                if tp >= p && (tp < high_p || high_p < p) {
                    high_p = tp;
                    high_v = tv;
                }
            }
        }
        if p <= low_p {
            return Value::Number(high_v);
        }
        if p >= high_p {
            return Value::Number(low_v);
        }
        let f = (p - low_p) / (high_p - low_p);
        // `high_v * f + low_v * (1 - f)`: clang fuses the first product on
        // arm64 (see `fma`).
        Value::Number(mul_add(high_v, f, low_v * (1.0 - f)))
    }

    fn search(&mut self, a: &[ArgVal], loc: Loc) -> Value {
        if a.len() < 2 || a.len() > 4 {
            self.arg_count_warning("search", a.len(), "between 2 and 4", loc);
            return Value::Undef;
        }
        let find = &a[0].value;
        let table = &a[1].value;
        // `(unsigned int)double`: truncation, negative and NaN become 0.
        let per_match = if a.len() > 2 {
            a[2].value.to_f64() as u32
        } else {
            1
        };
        let col = if a.len() > 3 {
            a[3].value.to_f64() as u32
        } else {
            0
        } as usize;
        let empty = crate::value::Vector::empty();
        let rows = table.as_vector().unwrap_or(&empty);
        let matches_row = |needle: &Value, row: &Value| {
            (col == 0 && crate::ops::equals(needle, row))
                || row
                    .as_vector()
                    .is_some_and(|r| col < r.len() && crate::ops::equals(needle, &r[col]))
        };
        let mut out = Vec::new();
        match find {
            Value::Number(_) => {
                let mut count = 0;
                for (j, row) in rows.iter().enumerate() {
                    if matches_row(find, row) {
                        out.push(Value::Number(j as f64));
                        count += 1;
                        if per_match != 0 && count >= per_match {
                            break;
                        }
                    }
                }
            }
            Value::Str(s) => {
                let n_find = s.char_count();
                if let Value::Str(t) = table {
                    let hay: Vec<&[u8]> = (0..t.char_count())
                        .map(|j| t.char_at(j).unwrap_or_default())
                        .collect();
                    for i in 0..n_find {
                        let ft = s.char_at(i).unwrap_or_default();
                        let mut count = 0;
                        let mut res = Vec::new();
                        for (j, st) in hay.iter().enumerate() {
                            if !ft.is_empty()
                                && !st.is_empty()
                                && utf8::first_char(ft) == utf8::first_char(st)
                            {
                                count += 1;
                                if per_match == 1 {
                                    out.push(Value::Number(j as f64));
                                    break;
                                }
                                res.push(Value::Number(j as f64));
                                if per_match > 1 && count >= per_match {
                                    break;
                                }
                            }
                        }
                        if per_match != 1 {
                            out.push(Value::vector(res));
                        }
                    }
                } else {
                    for i in 0..n_find {
                        let ft = s.char_at(i).unwrap_or_default();
                        let mut count = 0;
                        let mut res = Vec::new();
                        for (j, row) in rows.iter().enumerate() {
                            let entry = row.as_vector().unwrap_or(&empty);
                            if entry.len() <= col {
                                let mut t = format!(
                                    "Invalid entry in search vector at index {j}, required number of values in the entry: {}. Invalid entry: ",
                                    col + 1
                                )
                                .into_bytes();
                                let row = row.clone();
                                self.write_echo_nothrow(&row, &mut t);
                                self.warn(loc, DiagCode::InvalidArgument, t);
                                return Value::vector(Vec::new());
                            }
                            let Value::Str(e) = &entry[col] else { continue };
                            if !ft.is_empty()
                                && utf8::first_char(ft) == utf8::first_char(e.as_bytes())
                            {
                                count += 1;
                                if per_match == 1 {
                                    out.push(Value::Number(j as f64));
                                    break;
                                }
                                res.push(Value::Number(j as f64));
                                if per_match > 1 && count >= per_match {
                                    break;
                                }
                            }
                        }
                        if per_match != 1 {
                            out.push(Value::vector(res));
                        }
                    }
                }
            }
            Value::Vector(fv) => {
                for needle in fv.iter() {
                    let mut count = 0;
                    let mut res = Vec::new();
                    for (j, row) in rows.iter().enumerate() {
                        if matches_row(needle, row) {
                            count += 1;
                            if per_match == 1 {
                                out.push(Value::Number(j as f64));
                                break;
                            }
                            res.push(Value::Number(j as f64));
                            if per_match > 1 && count >= per_match {
                                break;
                            }
                        }
                    }
                    if (per_match == 1 && count == 0) || per_match != 1 {
                        out.push(Value::vector(res));
                    }
                }
            }
            _ => return Value::Undef,
        }
        Value::vector(out)
    }

    fn cross(&mut self, a: &[ArgVal], loc: Loc) -> Value {
        if !self.check("cross", a, loc, &[Type::Vector, Type::Vector]) {
            return Value::Undef;
        }
        let (Some(v0), Some(v1)) = (a[0].value.as_vector(), a[1].value.as_vector()) else {
            return Value::Undef;
        };
        if v0.len() == 2 && v1.len() == 2 {
            return Value::Number(mul_sub_mul(
                v0[0].to_f64(),
                v1[1].to_f64(),
                v0[1].to_f64(),
                v1[0].to_f64(),
            ));
        }
        if v0.len() != 3 || v1.len() != 3 {
            self.warn(
                loc,
                DiagCode::InvalidArgument,
                "Invalid vector size of parameter for cross()",
            );
            return Value::Undef;
        }
        for i in 0..3 {
            let (Value::Number(d0), Value::Number(d1)) = (&v0[i], &v1[i]) else {
                self.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    "Invalid value in parameter vector for cross()",
                );
                return Value::Undef;
            };
            if d0.is_nan() || d1.is_nan() {
                self.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    "Invalid value (NaN) in parameter vector for cross()",
                );
                return Value::Undef;
            }
            if d0.is_infinite() || d1.is_infinite() {
                self.warn(
                    loc,
                    DiagCode::InvalidArgument,
                    "Invalid value (INF) in parameter vector for cross()",
                );
                return Value::Undef;
            }
        }
        let f = |v: &crate::value::Vector, i: usize| v[i].to_f64();
        // Fused on arm64 like the nightly's `a * b - c * d` (see `fma`).
        let x = mul_sub_mul(f(v0, 1), f(v1, 2), f(v0, 2), f(v1, 1));
        let y = mul_sub_mul(f(v0, 2), f(v1, 0), f(v0, 0), f(v1, 2));
        let z = mul_sub_mul(f(v0, 0), f(v1, 1), f(v0, 1), f(v1, 0));
        Value::vector(vec![Value::Number(x), Value::Number(y), Value::Number(z)])
    }
}

/// The state of one `chr()` call's walk (see `Evaluator::chr_into`).
#[derive(Default)]
struct ChrWalk {
    steps: u64,
    /// Lists (by the address of their elements) that print nothing and
    /// warn of nothing.
    silent: std::collections::HashSet<usize, FxBuild>,
    /// Warnings printed so far.
    warnings: u32,
    /// Gave up: past a limit, or cancelled.
    stopped: bool,
}

/// `builtin_object_unnamed`: apply one unnamed argument of `object()`, or
/// OpenSCAD's message for why it cannot be.
fn object_unnamed(b: &mut ObjectBuilder, v: &Value, arg: usize) -> Result<(), String> {
    const HELP: &str = "In an unnamed list, entries must be [key,value] to set or [key] to \
                        delete. The key must be <string>.";
    let prior_args = format!("Argument {arg} ");
    match v {
        Value::Object(o) => {
            b.extend_from(o);
            Ok(())
        }
        Value::Vector(items) => {
            for (i, member) in items.iter().enumerate() {
                let prior = format!("Element {i} ");
                let Value::Vector(entry) = member else {
                    let t = member.type_name();
                    return Err(format!(
                        "object( {prior_args}[{prior}<{t}>] ) Entry type is not a list, it is <{t}>. {HELP}"
                    ));
                };
                match entry.len() {
                    1 | 2 => {
                        let Value::Str(key) = &entry[0] else {
                            let t = entry[0].type_name();
                            let es = if entry.len() == 1 { "" } else { ",value" };
                            return Err(format!(
                                "object({prior_args}[{prior}[<{t}>{es}]]) The key of the entry is not <string> but <{t}>. {HELP}"
                            ));
                        };
                        if entry.len() == 1 {
                            b.del(key.as_bytes());
                        } else {
                            b.set(key.clone(), entry[1].clone());
                        }
                    }
                    0 => {
                        return Err(format!(
                            "object({prior_args}[{prior}[]]) Entry is empty. {HELP}"
                        ));
                    }
                    n => {
                        return Err(format!(
                            "object({prior_args}[{prior}[...]]) Entry length is {n}, must be 1 [key] or 2 [key,value]. {HELP}"
                        ));
                    }
                }
            }
            Ok(())
        }
        other => {
            let t = other.type_name();
            Err(format!(
                "object({prior_args}<{t}>) An unnamed argument must be either <object> or <list>, it is <{t}>. "
            ))
        }
    }
}

/// `std::filesystem::path::extension`, with its dot: empty for a name
/// without one, a name that is only a leading dot (`.bashrc`), `.` and
/// `..`, and a path ending in a separator.
fn extension(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "." || name == ".." {
        return "";
    }
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i..],
    }
}
