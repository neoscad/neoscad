//! Operators, as `Value.cc` defines them.
//!
//! Every operator returns either a value or the reasons its result is
//! undefined ([`Why`]). The expression evaluator prints the reasons as one
//! warning at the operator's location (`Expression::checkUndef`); when an
//! operator runs element-wise inside another (vector addition, say) the
//! inner reasons are dropped and the element is plain `undef`, exactly as
//! OpenSCAD stores an undef it never checks.
//!
//! The comparison wording is deliberately inconsistent, because OpenSCAD's
//! is: mixed types say `undefined operation (number < string)` while
//! `undef < undef` says `operation undefined (undefined < undefined)`.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use lang::ast::BinaryOp;

use crate::fma::mul_add;
use crate::limits::Guard;
use crate::sym::FxBuild;
use crate::value::{Object, Str, Type, Value, Vector};

/// Why an operation produced `undef`: messages joined with `"\n\t"` when
/// printed.
// Boxed so an operator's `Result` stays as small as a `Value` on the hot
// path where nothing goes wrong.
#[allow(clippy::box_collection)]
#[derive(Debug, Clone, PartialEq)]
pub struct Why(pub Box<Vec<String>>);

impl Why {
    pub fn new(s: String) -> Self {
        Why(Box::new(vec![s]))
    }

    fn append(mut self, s: String) -> Self {
        self.0.push(s);
        self
    }

    pub fn message(&self) -> String {
        self.0.join("\n\t")
    }
}

pub type OpResult = Result<Value, Why>;

fn undefined_op(a: &Value, op: &str, b: &Value) -> Why {
    Why::new(format!(
        "undefined operation ({} {op} {})",
        a.type_name(),
        b.type_name()
    ))
}

// --- walking two lists ----------------------------------------------------
//
// `==` and `<` on lists recurse element by element, as `VectorType`'s
// operators do. Lists share their elements, so a tree of depth `d` whose
// halves are one list (`t = [t, t]`, built by a tail-recursive function in
// a few dozen steps and costing nothing) has 2^d paths, and a plain walk
// takes 2^d steps: `t == t` at depth 28 ran for seconds, deeper ones for
// hours, inside one operator call that neither the time limit nor a
// cancel could stop. OpenSCAD walks it the same way; only the time differs.
//
// So the walk is iterative (lists can nest deeper than the stack allows)
// and, once it has taken more than `MEMO_AFTER` steps, it remembers which
// pairs of lists (by address) it has already walked to the end without
// deciding anything, and skips them when they come round again: a shared
// tree then costs its unique pairs, not its paths. It remembers only
// undecided pairs because a decided one (`==` false, `<` either way, or an
// undefined element comparison) decides the whole comparison, which ends
// at once. It polls the request's cancel flag and limits every `POLL`
// steps, and gives up with `Stopped` when they say so.
//
// `Rc::ptr_eq` cannot short-circuit `x == x`: `[0/0] == [0/0]` is false in
// OpenSCAD even for one list compared with itself (NaN is unequal to
// itself), so a list's equality with itself is its content's, and is only
// remembered once walked.

/// Steps a walk takes before it starts remembering pairs: comparisons of
/// ordinary lists (points, matrices) finish well before this and never pay
/// for the hashing.
const MEMO_AFTER: u64 = 1 << 12;

/// Steps between polls of the cancel flag and limits (a poll reads the
/// clock).
const POLL: u64 = 1 << 12;

/// Strings this long are compared once per pair (see [`Walk::strings`]).
const LONG_STR: usize = 64;

/// A comparison gave up: the request was cancelled, ran out of time, or
/// passed the memory limit. The caller reports it (`check_limits` then
/// `check_interrupt`, which name the limit as any other check would).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

/// What a long walk over two values polls to know it should stop.
#[derive(Clone, Copy)]
pub struct Stop<'a> {
    interrupt: Option<&'a AtomicBool>,
    guard: Option<&'a Guard>,
}

impl<'a> Stop<'a> {
    /// Stop on the request's cancel flag, and on its time limit when it has
    /// a guard.
    pub fn new(interrupt: Option<&'a AtomicBool>, guard: Option<&'a Guard>) -> Self {
        Stop { interrupt, guard }
    }

    /// Stop only on the memory limit, for callers without the request's
    /// flags at hand. The memory limit trips the guard from the thread's
    /// count ([`crate::limits::live`]), so it still stops the request.
    pub const MEMORY_ONLY: Stop<'static> = Stop {
        interrupt: None,
        guard: None,
    };

    /// Whether to give up. `own` is the bytes the walk has allocated for
    /// itself (its stack and the pairs it remembers), which are nobody's
    /// values but count against the memory limit all the same: a walk of
    /// millions of pairs would otherwise grow unseen until the time limit.
    ///
    /// Running out of time is left for the caller to record (the guard is
    /// not tripped here): the evaluator's own check then reports the limit
    /// with the call stack, where a tripped guard would unwind as a bare
    /// interruption.
    fn stopped(&self, own: u64) -> bool {
        crate::limits::live::passes(own)
            || self.interrupt.is_some_and(|f| f.load(Ordering::Relaxed))
            || self.guard.is_some_and(Guard::over_time)
    }
}

/// Two lists (or two objects' values) being walked side by side, and the
/// next index.
struct Pair<'v> {
    x: &'v [Value],
    y: &'v [Value],
    i: usize,
    /// The first object, when these are two objects' values: its methods
    /// are never equal to the other's (see [`Object::is_method`]).
    object: Option<&'v Object>,
}

impl<'v> Pair<'v> {
    fn new(x: &'v Vector, y: &'v Vector) -> Self {
        Pair {
            x: x.as_slice(),
            y: y.as_slice(),
            i: 0,
            object: None,
        }
    }

    fn objects(x: &'v Object, y: &'v Object) -> Self {
        Pair {
            x: x.values(),
            y: y.values(),
            i: 0,
            object: Some(x),
        }
    }

    /// Whether the elements last returned are methods of two different
    /// objects (the walk never enters an object paired with itself).
    fn at_method(&self) -> bool {
        self.object.is_some_and(|o| o.is_method(self.i - 1))
    }

    /// The pair's identity for [`Walk`]'s memory: the lists' element
    /// buffers. Two live lists never share a buffer, and every empty list
    /// may have the same dangling one, which is harmless: empty lists have
    /// one content, so remembering one pair of them is right for all.
    fn key(&self) -> (usize, usize) {
        (self.x.as_ptr() as usize, self.y.as_ptr() as usize)
    }

    /// The next pair of elements, up to the shorter list's end.
    fn next(&mut self) -> Option<(&'v Value, &'v Value)> {
        let i = self.i;
        let pq = (self.x.get(i)?, self.y.get(i)?);
        self.i += 1;
        Some(pq)
    }
}

/// The state of one walk: the pairs of lists entered and not finished,
/// the undecided pairs it remembers, and its step count.
struct Walk<'v, 's> {
    top: Pair<'v>,
    stack: Vec<Pair<'v>>,
    done: HashSet<(usize, usize), FxBuild>,
    steps: u64,
    next_poll: u64,
    stop: Stop<'s>,
}

impl<'v, 's> Walk<'v, 's> {
    fn new(top: Pair<'v>, stop: Stop<'s>) -> Self {
        Walk {
            top,
            stack: Vec::new(),
            done: HashSet::default(),
            steps: 0,
            next_poll: POLL,
            stop,
        }
    }

    /// The next pair of elements of the innermost pair of lists, or `None`
    /// when it has run out (see [`Walk::leave`]).
    fn next(&mut self) -> Result<Option<(&'v Value, &'v Value)>, Stopped> {
        self.steps += 1;
        if self.steps >= self.next_poll {
            self.next_poll = self.steps + POLL;
            let own = self.done.capacity() as u64 * 24
                + self.stack.capacity() as u64 * std::mem::size_of::<Pair>() as u64;
            if self.stop.stopped(own) {
                return Err(Stopped);
            }
        }
        Ok(self.top.next())
    }

    /// Walk into a pair of lists, unless it is one already walked to the
    /// end undecided.
    fn enter(&mut self, x: &'v Vector, y: &'v Vector) {
        self.enter_pair(Pair::new(x, y));
    }

    fn enter_pair(&mut self, p: Pair<'v>) {
        if self.steps >= MEMO_AFTER && self.done.contains(&p.key()) {
            return;
        }
        self.stack.push(std::mem::replace(&mut self.top, p));
    }

    /// Walk into two different objects for `==`, unless they are a pair
    /// already walked to the end undecided; false when their keys already
    /// tell them apart. The keys are compared here, before the values, and
    /// count as steps as the values do.
    fn enter_objects(&mut self, x: &'v Object, y: &'v Object) -> bool {
        if x.len() != y.len() {
            return false;
        }
        let p = Pair::objects(x, y);
        if self.steps >= MEMO_AFTER && self.done.contains(&p.key()) {
            return true;
        }
        if !self.keys_equal(x, y) {
            return false;
        }
        self.stack.push(std::mem::replace(&mut self.top, p));
        true
    }

    /// Whether two objects have the same keys in the same order
    /// (`ObjectType::operator==` compares them by position).
    fn keys_equal(&mut self, x: &Object, y: &Object) -> bool {
        x.keys().iter().zip(y.keys()).all(|(a, b)| {
            self.steps += 1 + (a.as_bytes().len() / LONG_STR) as u64;
            a.as_bytes() == b.as_bytes()
        })
    }

    /// The innermost pair of lists ran out undecided: remember it, and go
    /// back to its parent. False when it was the outermost.
    fn leave(&mut self) -> bool {
        if self.steps >= MEMO_AFTER {
            self.done.insert(self.top.key());
        }
        match self.stack.pop() {
            Some(p) => {
                self.top = p;
                true
            }
            None => false,
        }
    }

    /// The order of two strings' bytes. A long string can be shared by
    /// every element of a list (`[for (i = [0:1e5]) s]`) and compared with
    /// another one equal to it at each of them, so long strings found equal
    /// are remembered by pair (their buffers, which no two live strings
    /// share) from the first time, and the steps count their length, so
    /// that the polls keep their pace.
    fn strings(&mut self, a: &Str, b: &Str) -> std::cmp::Ordering {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() < LONG_STR || b.len() < LONG_STR {
            return a.cmp(b);
        }
        let key = (a.as_ptr() as usize, b.as_ptr() as usize);
        if key.0 == key.1 || self.done.contains(&key) {
            return std::cmp::Ordering::Equal;
        }
        self.steps += (a.len().min(b.len()) / LONG_STR) as u64;
        let o = a.cmp(b);
        if o.is_eq() {
            self.done.insert(key);
        }
        o
    }

    /// The indices of the elements being compared, innermost first.
    fn path(&self) -> impl Iterator<Item = usize> + '_ {
        std::iter::once(&self.top)
            .chain(self.stack.iter().rev())
            .map(|p| p.i - 1)
    }
}

// --- equality -------------------------------------------------------------

/// `==`, which is always defined. Stops only on the memory limit (see
/// [`Stop::MEMORY_ONLY`]), when the answer is `false` and meaningless.
pub fn equals(a: &Value, b: &Value) -> bool {
    equals_in(a, b, Stop::MEMORY_ONLY).unwrap_or(false)
}

/// [`equals`], stopping when `stop` says so.
pub fn equals_in(a: &Value, b: &Value, stop: Stop<'_>) -> Result<bool, Stopped> {
    match (a, b) {
        (Value::Vector(x), Value::Vector(y)) => vectors_equal(x, y, stop),
        (Value::Object(x), Value::Object(y)) => objects_equal(x, y, stop),
        _ => Ok(scalars_equal(a, b)),
    }
}

/// `ObjectType::operator==`: the same object, or the same number of
/// entries with equal keys and values position by position (so key order
/// matters). Unlike a list, an object is equal to itself even when it
/// holds a NaN, as OpenSCAD compares their addresses first.
fn objects_equal(x: &Object, y: &Object, stop: Stop<'_>) -> Result<bool, Stopped> {
    if Object::ptr_eq(x, y) {
        return Ok(true);
    }
    if x.len() != y.len() {
        return Ok(false);
    }
    let mut w = Walk::new(Pair::objects(x, y), stop);
    if !w.keys_equal(x, y) {
        return Ok(false);
    }
    walk_equal(w)
}

/// `==` on anything but two lists.
fn scalars_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undef, Value::Undef) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x.as_bytes() == y.as_bytes(),
        (Value::Range(x), Value::Range(y)) => x.equals(y),
        // Function literals are equal only to themselves (FunctionType
        // compares addresses).
        (Value::Function(x), Value::Function(y)) => Rc::ptr_eq(x, y),
        // Sketch entity handles are equal when they name the same entity
        // (`.start` of a line drawn from point `a` is `a`).
        (Value::Entity(x), Value::Entity(y)) => x.same(y),
        _ => false,
    }
}

/// `VectorType::operator==`: equal lengths and equal elements. (OpenSCAD
/// compares the common elements before the lengths; as `==` is never
/// undefined, the order cannot show.)
fn vectors_equal(x: &Vector, y: &Vector, stop: Stop<'_>) -> Result<bool, Stopped> {
    if x.len() != y.len() {
        return Ok(false);
    }
    walk_equal(Walk::new(Pair::new(x, y), stop))
}

/// The `==` walk from its first pair: lists and objects nested in each
/// other, element by element.
fn walk_equal(mut w: Walk<'_, '_>) -> Result<bool, Stopped> {
    loop {
        match w.next()? {
            None => {
                if !w.leave() {
                    return Ok(true);
                }
            }
            Some((Value::Vector(p), Value::Vector(q))) => {
                if p.len() != q.len() {
                    return Ok(false);
                }
                w.enter(p, q);
            }
            Some((Value::Object(p), Value::Object(q))) => {
                if !Object::ptr_eq(p, q) && !w.enter_objects(p, q) {
                    return Ok(false);
                }
            }
            Some((Value::Function(p), Value::Function(q))) => {
                if !Rc::ptr_eq(p, q) || w.top.at_method() {
                    return Ok(false);
                }
            }
            Some((Value::Str(p), Value::Str(q))) => {
                if w.strings(p, q).is_ne() {
                    return Ok(false);
                }
            }
            Some((p, q)) => {
                if !scalars_equal(p, q) {
                    return Ok(false);
                }
            }
        }
    }
}

// --- ordering -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl Cmp {
    fn symbol(self) -> &'static str {
        match self {
            Cmp::Less => "<",
            Cmp::LessEqual => "<=",
            Cmp::Greater => ">",
            Cmp::GreaterEqual => ">=",
        }
    }
}

#[cfg(test)]
/// `<`, `<=`, `>`, `>=`. Stops only on the memory limit (see
/// [`Stop::MEMORY_ONLY`]), when the answer is `false` and meaningless.
pub fn compare(a: &Value, b: &Value, op: Cmp) -> Result<bool, Why> {
    compare_in(a, b, op, Stop::MEMORY_ONLY).unwrap_or(Ok(false))
}

/// [`compare`], stopping when `stop` says so.
pub fn compare_in(
    a: &Value,
    b: &Value,
    op: Cmp,
    stop: Stop<'_>,
) -> Result<Result<bool, Why>, Stopped> {
    let (Value::Vector(x), Value::Vector(y)) = (a, b) else {
        return Ok(compare_scalars(a, b, op));
    };
    // `>` is `y < x`, and `<=` and `>=` negate `>` and `<`
    // (`VectorType::operator<=`), so every one is a `<` walk, with the
    // operands swapped for `>` and `<=`. The order matters beyond the
    // result: an undefined element comparison is reported with the types
    // in the walk's order (`[1, "a"] <= [1, 2]` says `number < string`).
    let (x, y, less) = match op {
        Cmp::Less => (x, y, true),
        Cmp::GreaterEqual => (x, y, false),
        Cmp::Greater => (y, x, true),
        Cmp::LessEqual => (y, x, false),
    };
    Ok(vector_order(x, y, stop)?.map(|o| (o == Order::Less) == less))
}

/// The ordering operators on anything but two lists.
fn compare_scalars(a: &Value, b: &Value, op: Cmp) -> Result<bool, Why> {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => Ok(match op {
            Cmp::Less => x < y,
            Cmp::LessEqual => x <= y,
            Cmp::Greater => x > y,
            Cmp::GreaterEqual => x >= y,
        }),
        (Value::Number(x), Value::Number(y)) => Ok(match op {
            Cmp::Less => x < y,
            Cmp::LessEqual => x <= y,
            Cmp::Greater => x > y,
            Cmp::GreaterEqual => x >= y,
        }),
        (Value::Str(x), Value::Str(y)) => {
            let (x, y) = (x.as_bytes(), y.as_bytes());
            Ok(match op {
                Cmp::Less => x < y,
                Cmp::LessEqual => x <= y,
                Cmp::Greater => x > y,
                Cmp::GreaterEqual => x >= y,
            })
        }
        (Value::Range(x), Value::Range(y)) => Ok(match op {
            Cmp::Less => x.less(y, false),
            Cmp::LessEqual => x.less(y, true),
            Cmp::Greater => x.greater(y, false),
            Cmp::GreaterEqual => x.greater(y, true),
        }),
        (Value::Undef, Value::Undef) => Err(Why::new(format!(
            "operation undefined (undefined {} undefined)",
            op.symbol()
        ))),
        (Value::Function(_), Value::Function(_)) => Err(Why::new(format!(
            "operation undefined (function {} function)",
            op.symbol()
        ))),
        (Value::Object(_), Value::Object(_)) => Err(Why::new(format!(
            "operation undefined (object {} object)",
            op.symbol()
        ))),
        _ => Err(undefined_op(a, op.symbol(), b)),
    }
}

/// Where a `<` walk of two lists ends up: the first is less, the second
/// is less, or neither (equal, or unordered like NaN).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Less,
    Greater,
    Neither,
}

/// `VectorType::operator<`, as one walk.
///
/// OpenSCAD's `x < y` asks, at each index, `x[i] < y[i]` (true: less;
/// undefined: undefined, with the index appended) and then `y[i] < x[i]`
/// (true: not less), and at the end compares the lengths. With nested
/// lists that is two recursive walks per level, 2^depth steps even for a
/// list nested thirty deep with no sharing at all. Both questions are
/// answered by one walk that says which side is less, if either: the
/// reverse comparison meets the same element pairs in the same order,
/// swapped, so it decides at the same index the other way, and it is
/// undefined exactly when the forward one is (undefinedness depends only
/// on the two types), which is why OpenSCAD's `y[i] < x[i]` never has an
/// undefined result to discard.
fn vector_order(x: &Vector, y: &Vector, stop: Stop<'_>) -> Result<Result<Order, Why>, Stopped> {
    let mut w = Walk::new(Pair::new(x, y), stop);
    loop {
        let (p, q) = match w.next()? {
            Some(pq) => pq,
            None => {
                // The common elements tie: the shorter list is less.
                let (n, m) = (w.top.x.len(), w.top.y.len());
                if n != m {
                    return Ok(Ok(if n < m { Order::Less } else { Order::Greater }));
                }
                if !w.leave() {
                    return Ok(Ok(Order::Neither));
                }
                continue;
            }
        };
        match (p, q) {
            (Value::Vector(p), Value::Vector(q)) => {
                w.enter(p, q);
                continue;
            }
            (Value::Str(p), Value::Str(q)) => match w.strings(p, q) {
                std::cmp::Ordering::Less => return Ok(Ok(Order::Less)),
                std::cmp::Ordering::Greater => return Ok(Ok(Order::Greater)),
                std::cmp::Ordering::Equal => continue,
            },
            _ => {}
        }
        match compare_scalars(p, q, Cmp::Less) {
            Ok(true) => return Ok(Ok(Order::Less)),
            // As undefinedness is symmetric, this is defined.
            Ok(false) if compare_scalars(q, p, Cmp::Less) == Ok(true) => {
                return Ok(Ok(Order::Greater));
            }
            Ok(false) => {}
            Err(why) => {
                let why = w.path().fold(why, |why, i| {
                    why.append(format!("in vector comparison at index {i}"))
                });
                return Ok(Err(why));
            }
        }
    }
}

/// The comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`), stopping
/// when `stop` says so.
pub fn relation(op: BinaryOp, a: &Value, b: &Value, stop: Stop<'_>) -> Result<OpResult, Stopped> {
    let cmp = match op {
        BinaryOp::Equal => return Ok(Ok(Value::Bool(equals_in(a, b, stop)?))),
        BinaryOp::NotEqual => return Ok(Ok(Value::Bool(!equals_in(a, b, stop)?))),
        BinaryOp::Less => Cmp::Less,
        BinaryOp::LessEqual => Cmp::LessEqual,
        BinaryOp::Greater => Cmp::Greater,
        BinaryOp::GreaterEqual => Cmp::GreaterEqual,
        _ => unreachable!("not a comparison: {op:?}"),
    };
    Ok(compare_in(a, b, cmp, stop)?.map(Value::Bool))
}

// --- arithmetic -----------------------------------------------------------

/// What an element-wise operator makes of one element: a value (`undef`
/// where OpenSCAD's would be undefined, whose reasons it drops), or a
/// nested list (or pair of lists) to walk into, which becomes a list.
enum Elem<T> {
    Leaf(Value),
    Descend(T),
}

/// An element-wise operator over `root` and every list nested in it: `f`
/// decides for each element whether it is a value or a list to walk into,
/// which the walk does with the same `f`, as OpenSCAD's operators recurse.
///
/// The walk is a loop over an explicit stack of the lists being built: a
/// list can nest as deep as a recursion can build it (100,000 levels at
/// the counted limit, any depth through a tail call), and the recursive
/// walk this replaced held native stack per level, which a browser
/// worker's stack (about 512 KiB in WebKit) ran out of within a few
/// thousand levels. Lists are made in the recursive walk's order, inner
/// before outer, so the memory limit sees the same.
///
/// It runs in one Rust call that the evaluator's checks cannot interrupt,
/// and a list whose halves are shared (`t = [t, t]` a few dozen times)
/// costs nothing until an operator like `-t` copies it into 2^depth lists.
/// So once the memory limit has passed (see `crate::limits::live`, which
/// trips it as the lists are made), each list from then on is `undef`,
/// and the evaluator reports the limit as soon as the operator returns.
// Out of line, as the recursive walk was: inlined into the operators, it
// made them too big to inline into the evaluator's hot paths.
#[inline(never)]
fn map_tree<'v>(root: &'v [Value], mut f: impl FnMut(&'v Value) -> Elem<&'v [Value]>) -> Value {
    if crate::limits::live::over() {
        return Value::Undef;
    }
    let mut open: Vec<(&'v [Value], usize, Vec<Value>)> =
        vec![(root, 0, Vec::with_capacity(root.len()))];
    loop {
        let (items, i, out) = open.last_mut().expect("a list being walked");
        let Some(e) = items.get(*i) else {
            let (_, _, out) = open.pop().expect("a list being walked");
            let v = Value::vector(out);
            match open.last_mut() {
                Some((_, _, parent)) => parent.push(v),
                None => return v,
            }
            continue;
        };
        *i += 1;
        match f(e) {
            Elem::Leaf(v) => out.push(v),
            Elem::Descend(sub) => {
                // `map_vec`'s check, where the nested call would make it.
                if crate::limits::live::over() {
                    out.push(Value::Undef);
                } else {
                    open.push((sub, 0, Vec::with_capacity(sub.len())));
                }
            }
        }
    }
}

/// [`map_tree`] over two lists in step, as far as the shorter one goes.
#[allow(clippy::type_complexity)]
fn zip_tree<'v>(
    x: &'v [Value],
    y: &'v [Value],
    mut f: impl FnMut(&'v Value, &'v Value) -> Elem<(&'v [Value], &'v [Value])>,
) -> Value {
    if crate::limits::live::over() {
        return Value::Undef;
    }
    let n = x.len().min(y.len());
    let mut open: Vec<(&'v [Value], &'v [Value], usize, Vec<Value>)> =
        vec![(x, y, 0, Vec::with_capacity(n))];
    loop {
        let (xs, ys, i, out) = open.last_mut().expect("a list being walked");
        let (Some(p), Some(q)) = (xs.get(*i), ys.get(*i)) else {
            let (_, _, _, out) = open.pop().expect("a list being walked");
            let v = Value::vector(out);
            match open.last_mut() {
                Some((_, _, _, parent)) => parent.push(v),
                None => return v,
            }
            continue;
        };
        *i += 1;
        match f(p, q) {
            Elem::Leaf(v) => out.push(v),
            Elem::Descend((a, b)) => {
                if crate::limits::live::over() {
                    out.push(Value::Undef);
                } else {
                    open.push((a, b, 0, Vec::with_capacity(a.len().min(b.len()))));
                }
            }
        }
    }
}

/// `+` or `-` on two lists, element by element.
#[inline(never)]
fn zip_arith(x: &Vector, y: &Vector, op: fn(f64, f64) -> f64) -> Value {
    zip_tree(x, y, |p, q| match (p, q) {
        (Value::Number(a), Value::Number(b)) => Elem::Leaf(Value::Number(op(*a, *b))),
        (Value::Vector(a), Value::Vector(b)) => Elem::Descend((a.as_slice(), b.as_slice())),
        _ => Elem::Leaf(Value::Undef),
    })
}

pub fn add(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x + y)),
        (Value::Vector(x), Value::Vector(y)) => Ok(zip_arith(x, y, |p, q| p + q)),
        _ => Err(undefined_op(a, "+", b)),
    }
}

pub fn sub(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x - y)),
        (Value::Vector(x), Value::Vector(y)) => Ok(zip_arith(x, y, |p, q| p - q)),
        _ => Err(undefined_op(a, "-", b)),
    }
}

/// Vector times number, element by element (`multvecnum`: the element is
/// always the left operand).
fn mul_vec_num(v: &Vector, n: &Value) -> Value {
    let Value::Number(n) = *n else {
        unreachable!("a vector times a number")
    };
    map_tree(v, |e| match e {
        Value::Number(x) => Elem::Leaf(Value::Number(x * n)),
        Value::Vector(w) => Elem::Descend(w.as_slice()),
        _ => Elem::Leaf(Value::Undef),
    })
}

/// Matrix times vector (`multmatvec`).
fn mul_mat_vec(m: &Vector, v: &Vector) -> OpResult {
    let mut out = Vec::with_capacity(m.len());
    for (i, row) in m.iter().enumerate() {
        let row = match row {
            Value::Vector(r) if r.len() == v.len() => r,
            _ => {
                return Err(Why::new(format!(
                    "Matrix must be rectangular. Problem at row {i}"
                )));
            }
        };
        let mut sum = 0.0;
        for (j, e) in row.iter().enumerate() {
            let Value::Number(a) = e else {
                return Err(Why::new(format!(
                    "Matrix must contain only numbers. Problem at row {i}, col {j}"
                )));
            };
            let Value::Number(b) = v[j] else {
                return Err(Why::new(format!(
                    "Vector must contain only numbers. Problem at index {j}"
                )));
            };
            sum = mul_add(*a, b, sum);
        }
        out.push(Value::Number(sum));
    }
    Ok(Value::vector(out))
}

/// Vector times matrix (`multvecmat`). OpenSCAD also logs each failure as
/// a warning without a location; those go to `warn`.
fn mul_vec_mat(v: &Vector, m: &Vector, warn: &mut Vec<String>) -> OpResult {
    let first = m[0].as_vector().map_or(0, |r| r.len());
    let mut out = Vec::with_capacity(first);
    for i in 0..first {
        let mut sum = 0.0;
        for j in 0..v.len() {
            let row = match &m[j] {
                Value::Vector(r) if r.len() == first => r,
                _ => {
                    let s = format!("Matrix must be rectangular. Problem at row {j}");
                    warn.push(s.clone());
                    return Err(Why::new(s));
                }
            };
            let Value::Number(a) = v[j] else {
                let s = format!("Vector must contain only numbers. Problem at index {j}");
                warn.push(s.clone());
                return Err(Why::new(s));
            };
            let Value::Number(b) = row[i] else {
                let s = format!("Matrix must contain only numbers. Problem at row {j}, col {i}");
                warn.push(s.clone());
                return Err(Why::new(s));
            };
            sum = mul_add(a, b, sum);
        }
        out.push(Value::Number(sum));
    }
    Ok(Value::vector(out))
}

/// `*`. `warn` collects the location-less warnings of `multvecmat`.
pub fn mul(a: &Value, b: &Value, warn: &mut Vec<String>) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x * y)),
        (Value::Number(_), Value::Vector(v)) => Ok(mul_vec_num(v, a)),
        (Value::Vector(v), Value::Number(_)) => Ok(mul_vec_num(v, b)),
        (Value::Vector(x), Value::Vector(y)) => mul_vectors(x, y, warn),
        _ => Err(undefined_op(a, "*", b)),
    }
}

fn mul_vectors(x: &Vector, y: &Vector, warn: &mut Vec<String>) -> OpResult {
    if x.is_empty() || y.is_empty() {
        return Err(Why::new(
            "Multiplication is undefined on empty vectors".into(),
        ));
    }
    let (t1, t2) = (x[0].ty(), y[0].ty());
    match (t1, t2) {
        (Type::Number, Type::Number) => {
            if x.len() != y.len() {
                return Err(Why::new(format!(
                    "vector*vector requires matching lengths ({} != {})",
                    x.len(),
                    y.len()
                )));
            }
            let mut r = 0.0;
            for (p, q) in x.iter().zip(y.iter()) {
                match (p, q) {
                    (Value::Number(p), Value::Number(q)) => r = mul_add(*p, *q, r),
                    _ => {
                        return Err(Why::new(format!(
                            "undefined operation ({} * {})",
                            p.type_name(),
                            q.type_name()
                        )));
                    }
                }
            }
            Ok(Value::Number(r))
        }
        (Type::Number, Type::Vector) => {
            if x.len() != y.len() {
                return Err(Why::new(format!(
                    "vector*matrix requires vector length to match matrix row count ({} != {})",
                    x.len(),
                    y.len()
                )));
            }
            mul_vec_mat(x, y, warn)
        }
        (Type::Vector, Type::Number) => {
            let cols = x[0].as_vector().map_or(0, |r| r.len());
            if cols != y.len() {
                return Err(Why::new(format!(
                    "matrix*vector requires matrix column count to match vector length ({} != {})",
                    cols,
                    y.len()
                )));
            }
            mul_mat_vec(x, y)
        }
        (Type::Vector, Type::Vector) => {
            let cols = x[0].as_vector().map_or(0, |r| r.len());
            if cols != y.len() {
                return Err(Why::new(format!(
                    "matrix*matrix requires left operand column count to match right operand row count ({} != {})",
                    cols,
                    y.len()
                )));
            }
            let mut out = Vec::with_capacity(x.len());
            for (i, row) in x.iter().enumerate() {
                let empty = Vector::empty();
                let r = row.as_vector().unwrap_or(&empty);
                if r.len() != y.len() {
                    return Err(Why::new(format!(
                        "matrix*matrix left operand row length does not match right operand row count ({} != {}) at row {i}",
                        r.len(),
                        y.len()
                    )));
                }
                match mul_vec_mat(r, y, warn) {
                    Ok(v) => out.push(v),
                    Err(w) => {
                        return Err(w.append(format!("while processing left operand at row {i}")));
                    }
                }
            }
            Ok(Value::vector(out))
        }
        _ => Err(Why::new(format!(
            "undefined vector*vector multiplication where first elements are types {} and {}",
            t1.name(),
            t2.name()
        ))),
    }
}

pub fn div(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x / y)),
        (Value::Vector(v), Value::Number(y)) => Ok(map_tree(v, |e| match e {
            Value::Number(x) => Elem::Leaf(Value::Number(x / y)),
            Value::Vector(w) => Elem::Descend(w.as_slice()),
            _ => Elem::Leaf(Value::Undef),
        })),
        (Value::Number(x), Value::Vector(v)) => Ok(map_tree(v, |e| match e {
            Value::Number(y) => Elem::Leaf(Value::Number(x / y)),
            Value::Vector(w) => Elem::Descend(w.as_slice()),
            _ => Elem::Leaf(Value::Undef),
        })),
        _ => Err(undefined_op(a, "/", b)),
    }
}

/// `%` is C's `fmod`.
pub fn rem(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x % y)),
        _ => Err(undefined_op(a, "%", b)),
    }
}

pub fn pow(a: &Value, b: &Value) -> OpResult {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x.powf(*y))),
        _ => Err(undefined_op(a, "^", b)),
    }
}

/// `Value::toInt64`: truncate, saturating like the hardware conversion.
fn to_i64(x: f64) -> i64 {
    x.trunc() as i64
}

#[derive(Clone, Copy)]
pub enum Bitwise {
    And,
    Or,
    Shl,
    Shr,
}

pub fn bitwise(a: &Value, b: &Value, op: Bitwise) -> OpResult {
    let sym = match op {
        Bitwise::And => "&",
        Bitwise::Or => "|",
        Bitwise::Shl => "<<",
        Bitwise::Shr => ">>",
    };
    let (Value::Number(x), Value::Number(y)) = (a, b) else {
        return Err(undefined_op(a, sym, b));
    };
    let (l, r) = (to_i64(*x), to_i64(*y));
    let v = match op {
        Bitwise::And => l & r,
        Bitwise::Or => l | r,
        Bitwise::Shl | Bitwise::Shr => {
            if r < 0 {
                return Err(Why::new("negative shift".into()));
            }
            if r >= 64 {
                return Err(Why::new("shift too large".into()));
            }
            if matches!(op, Bitwise::Shl) {
                l << r
            } else {
                l >> r
            }
        }
    };
    Ok(Value::Number(v as f64))
}

pub fn neg(a: &Value) -> OpResult {
    match a {
        Value::Number(x) => Ok(Value::Number(-x)),
        Value::Vector(v) => Ok(map_tree(v, |e| match e {
            Value::Number(x) => Elem::Leaf(Value::Number(-x)),
            Value::Vector(w) => Elem::Descend(w.as_slice()),
            _ => Elem::Leaf(Value::Undef),
        })),
        _ => Err(Why::new(format!(
            "undefined operation (-{})",
            a.type_name()
        ))),
    }
}

pub fn bit_not(a: &Value) -> OpResult {
    match a {
        Value::Number(x) => Ok(Value::Number(!to_i64(*x) as f64)),
        _ => Err(Why::new(format!(
            "undefined operation (~{})",
            a.type_name()
        ))),
    }
}

/// `convert_to_uint32`: an index, or `u32::MAX` when it is not a finite
/// number in range (boost::numeric_cast truncates toward zero).
fn to_index(d: f64) -> u32 {
    if d.is_finite() && d > -1.0 && d < 4_294_967_296.0 {
        d as u32
    } else {
        u32::MAX
    }
}

/// `a[i]`. An out-of-range or ill-typed index gives `undef`; its reason is
/// never printed by OpenSCAD, so none is kept.
pub fn index(a: &Value, i: &Value) -> Value {
    let Value::Number(d) = i else {
        return index_by_key(a, i);
    };
    let i = to_index(*d) as usize;
    match a {
        Value::Str(s) => s.char_at(i).map_or(Value::Undef, Value::str),
        Value::Vector(v) => v.get(i).cloned().unwrap_or(Value::Undef),
        Value::Range(r) => match i {
            0 => Value::Number(r.begin),
            1 => Value::Number(r.step),
            2 => Value::Number(r.end),
            _ => Value::Undef,
        },
        _ => Value::Undef,
    }
}

/// `o["key"]`: an object's value (a method bound to it), or `undef`.
#[inline(never)]
fn index_by_key(a: &Value, i: &Value) -> Value {
    match (a, i) {
        (Value::Object(o), Value::Str(k)) => o.get(k.as_bytes()),
        _ => Value::Undef,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: f64) -> Value {
        Value::Number(x)
    }
    fn v(x: &[f64]) -> Value {
        Value::vector(x.iter().map(|&e| n(e)).collect())
    }

    #[test]
    fn vector_arithmetic_truncates_to_shortest() {
        let r = add(&v(&[1.0, 2.0, 3.0]), &v(&[10.0, 20.0])).unwrap();
        assert!(equals(&r, &v(&[11.0, 22.0])));
        let r = add(&n(1.0), &Value::str(b"a")).unwrap_err();
        assert_eq!(r.message(), "undefined operation (number + string)");
    }

    #[test]
    fn matrix_products() {
        let m = Value::vector(vec![v(&[1.0, 2.0]), v(&[3.0, 4.0])]);
        let mut w = Vec::new();
        assert!(equals(
            &mul(&m, &v(&[1.0, 1.0]), &mut w).unwrap(),
            &v(&[3.0, 7.0])
        ));
        assert!(equals(
            &mul(&v(&[1.0, 1.0]), &m, &mut w).unwrap(),
            &v(&[4.0, 6.0])
        ));
        assert!(equals(
            &mul(&v(&[1.0, 2.0]), &v(&[3.0, 4.0]), &mut w).unwrap(),
            &n(11.0)
        ));
        let e = mul(&v(&[1.0]), &v(&[1.0, 2.0]), &mut w).unwrap_err();
        assert_eq!(
            e.message(),
            "vector*vector requires matching lengths (1 != 2)"
        );
        assert!(w.is_empty());
    }

    #[test]
    fn comparisons_and_their_messages() {
        assert_eq!(
            compare(&v(&[1.0, 2.0]), &v(&[1.0, 3.0]), Cmp::Less),
            Ok(true)
        );
        assert_eq!(compare(&v(&[1.0]), &v(&[1.0, 0.0]), Cmp::Less), Ok(true));
        let e = compare(&Value::Undef, &Value::Undef, Cmp::Less).unwrap_err();
        assert_eq!(e.message(), "operation undefined (undefined < undefined)");
        let e = compare(
            &Value::vector(vec![n(1.0), Value::str(b"a")]),
            &v(&[1.0, 2.0]),
            Cmp::Less,
        )
        .unwrap_err();
        assert_eq!(
            e.message(),
            "undefined operation (string < number)\n\tin vector comparison at index 1"
        );
        assert!(equals(&Value::Undef, &Value::Undef));
        assert!(!equals(&n(f64::NAN), &n(f64::NAN)));
    }

    #[test]
    fn shared_trees_compare_by_their_distinct_lists() {
        // 2^60 paths each; built separately, so no pair of lists is one.
        let tree = |leaf: Value| {
            let mut t = leaf;
            for _ in 0..60 {
                t = Value::vector(vec![t.clone(), t]);
            }
            t
        };
        let (a, b, c) = (tree(v(&[1.0])), tree(v(&[1.0])), tree(v(&[2.0])));
        assert!(equals(&a, &a) && equals(&a, &b) && !equals(&a, &c));
        assert_eq!(compare(&a, &b, Cmp::Less), Ok(false));
        assert_eq!(compare(&a, &b, Cmp::LessEqual), Ok(true));
        assert_eq!(compare(&a, &c, Cmp::Less), Ok(true));
        assert_eq!(compare(&c, &a, Cmp::GreaterEqual), Ok(true));
        // NaN is unequal to itself, so a list holding one is too, even
        // compared with itself.
        let nan = tree(v(&[f64::NAN]));
        assert!(!equals(&nan, &nan));
        let e = compare(&tree(Value::Undef), &tree(Value::Undef), Cmp::Less).unwrap_err();
        assert_eq!(e.0.len(), 61, "the message and one index per level");
    }

    #[test]
    fn bitwise_and_shifts() {
        assert!(equals(
            &bitwise(&n(6.0), &n(3.0), Bitwise::And).unwrap(),
            &n(2.0)
        ));
        assert!(equals(
            &bitwise(&n(1.0), &n(4.0), Bitwise::Shl).unwrap(),
            &n(16.0)
        ));
        assert_eq!(
            bitwise(&n(1.0), &n(-1.0), Bitwise::Shl)
                .unwrap_err()
                .message(),
            "negative shift"
        );
        assert!(equals(&bit_not(&n(0.0)).unwrap(), &n(-1.0)));
    }

    #[test]
    fn indexing() {
        assert!(equals(&index(&v(&[1.0, 2.0]), &n(1.9)), &n(2.0)));
        assert!(index(&v(&[1.0, 2.0]), &n(-1.0)).is_undef());
        assert!(equals(
            &index(&Value::range(1.0, 2.0, 9.0), &n(2.0)),
            &n(9.0)
        ));
    }
}
