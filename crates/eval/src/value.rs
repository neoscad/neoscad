//! OpenSCAD values.
//!
//! A [`Value`] is 16 bytes and cheap to clone: numbers and booleans are
//! inline, strings, vectors, objects and function literals are reference
//! counted, and a range is kept lazy as `(begin, step, end)`. Values never borrow the
//! AST (a function literal names its expression by unit and id), so they
//! carry no lifetime and could later be cached across evaluations.
//!
//! OpenSCAD's `UndefType` also carries "reasons" for how an undef came to
//! be. They can only be observed by the operator that produced them (it
//! prints them as a warning), so here operators return them separately
//! (see `ops`) and a stored `Undef` carries nothing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::{Rc, Weak};

use lang::ast::ExprId;

use crate::context::Ctx;
use crate::utf8;

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undef,
    Bool(bool),
    Number(f64),
    Str(Str),
    Vector(Vector),
    Range(RangeRef),
    Function(Rc<FunctionValue>),
    /// OpenSCAD's experimental `ObjectType`: only the experimental
    /// features make one (`object()`, `textmetrics()`, JSON `import()`).
    Object(Object),
    /// NeoSCAD's `sketch()` extension: a handle to a point, line, arc or
    /// circle of a sketch (`docs/language-extensions.md`, section 4.2).
    /// Only the sketch vocabulary makes one, so OpenSCAD's programs never
    /// hold one.
    Entity(Rc<crate::sketch::Entity>),
}

/// The value kinds, named as OpenSCAD names them in messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Undef,
    Bool,
    Number,
    Str,
    Vector,
    Range,
    Function,
    Object,
    Entity,
}

impl Type {
    pub fn name(self) -> &'static str {
        match self {
            Type::Undef => "undefined",
            Type::Bool => "bool",
            Type::Number => "number",
            Type::Str => "string",
            Type::Vector => "vector",
            Type::Range => "range",
            Type::Function => "function",
            Type::Object => "object",
            Type::Entity => "sketch entity",
        }
    }
}

impl Value {
    pub fn ty(&self) -> Type {
        match self {
            Value::Undef => Type::Undef,
            Value::Bool(_) => Type::Bool,
            Value::Number(_) => Type::Number,
            Value::Str(_) => Type::Str,
            Value::Vector(_) => Type::Vector,
            Value::Range(_) => Type::Range,
            Value::Function(_) => Type::Function,
            Value::Object(_) => Type::Object,
            Value::Entity(_) => Type::Entity,
        }
    }

    pub fn type_name(&self) -> &'static str {
        self.ty().name()
    }

    pub fn is_undef(&self) -> bool {
        matches!(self, Value::Undef)
    }

    pub fn is_defined(&self) -> bool {
        !self.is_undef()
    }

    /// `Value::toBool`.
    pub fn to_bool(&self) -> bool {
        match self {
            Value::Undef => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Vector(v) => !v.is_empty(),
            Value::Range(_) | Value::Function(_) | Value::Entity(_) => true,
            Value::Object(o) => !o.is_empty(),
        }
    }

    /// `Value::toDouble`: the number, or 0.
    pub fn to_f64(&self) -> f64 {
        match self {
            Value::Number(n) => *n,
            _ => 0.0,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_finite(&self) -> Option<f64> {
        self.as_number().filter(|n| n.is_finite())
    }

    pub fn as_vector(&self) -> Option<&Vector> {
        match self {
            Value::Vector(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&Str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `Value::getUnsignedInt`.
    pub fn as_unsigned(&self) -> Option<u32> {
        self.as_finite()
            .filter(|&v| (0.0..=f64::from(u32::MAX)).contains(&v))
            .map(|v| v as u32)
    }

    /// `Value::getPositiveInt`.
    pub fn as_positive_int(&self) -> Option<u32> {
        self.as_finite()
            .filter(|&v| (1.0..=f64::from(u32::MAX)).contains(&v))
            .map(|v| v as u32)
    }

    /// `getVec2`: a two-element vector of numbers (finite ones when
    /// `finite` is set).
    pub fn as_vec2(&self, finite: bool) -> Option<[f64; 2]> {
        let v = self.as_vector()?;
        if v.len() != 2 {
            return None;
        }
        let get = |x: &Value| if finite { x.as_finite() } else { x.as_number() };
        Some([get(&v[0])?, get(&v[1])?])
    }

    /// `getDouble`: store the number in `out` if this is one.
    pub fn get_f64(&self, out: &mut f64) -> bool {
        match self {
            Value::Number(n) => {
                *out = *n;
                true
            }
            _ => false,
        }
    }

    /// `getVec2(x, y, ignoreInfinite)`: stores both or neither.
    pub fn get_vec2(&self, x: &mut f64, y: &mut f64, finite: bool) -> bool {
        match self.as_vec2(finite) {
            Some([a, b]) => {
                *x = a;
                *y = b;
                true
            }
            None => false,
        }
    }

    /// `getVec3(x, y, z)`: exactly three numbers. Like OpenSCAD it stores
    /// the leading numbers even when a later element fails, and callers
    /// that ignore the failure see them.
    pub fn get_vec3(&self, out: &mut [f64; 3]) -> bool {
        let Some(v) = self.as_vector() else {
            return false;
        };
        v.len() == 3 && v.iter().zip(out.iter_mut()).all(|(e, o)| e.get_f64(o))
    }

    /// `getVec3(x, y, z, defaultval)`: three numbers, or two with the third
    /// defaulted. A two-element vector succeeds even when its elements are
    /// not numbers; `x` and `y` are then left untouched.
    pub fn get_vec3_or2(&self, out: &mut [f64; 3], default: f64) -> bool {
        let Some(v) = self.as_vector() else {
            return false;
        };
        if v.len() == 2 {
            let (mut x, mut y) = (out[0], out[1]);
            if self.get_vec2(&mut x, &mut y, false) {
                out[0] = x;
                out[1] = y;
            }
            out[2] = default;
            return true;
        }
        self.get_vec3(out)
    }

    pub fn str(s: &[u8]) -> Value {
        Value::Str(Str::new(s))
    }

    pub fn vector(v: Vec<Value>) -> Value {
        Value::Vector(Vector::from(v))
    }

    pub fn range(begin: f64, step: f64, end: f64) -> Value {
        crate::limits::live::charge(crate::limits::live::BOX);
        Value::Range(RangeRef(Rc::new(RangeData(Range { begin, step, end }))))
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

/// A byte string with a cached character count.
#[derive(Clone)]
pub struct Str(Rc<StrData>);

struct StrData {
    /// `usize::MAX` until first needed.
    chars: Cell<usize>,
    bytes: Box<[u8]>,
}

/// A string counts towards the evaluator's memory estimate while it lives
/// (see `crate::limits::live`), whatever its length: a million short
/// strings are as much memory as one long one.
impl Drop for StrData {
    fn drop(&mut self) {
        crate::limits::live::credit(str_bytes(self.bytes.len()));
    }
}

/// Bytes a string of `len` bytes counts for.
#[inline(always)]
fn str_bytes(len: usize) -> u64 {
    crate::limits::live::BOX + len as u64
}

impl Str {
    pub fn new(bytes: &[u8]) -> Self {
        Self::from_vec(bytes.to_vec())
    }

    pub fn from_vec(bytes: Vec<u8>) -> Self {
        crate::limits::live::charge(str_bytes(bytes.len()));
        Str(Rc::new(StrData {
            chars: Cell::new(usize::MAX),
            bytes: bytes.into_boxed_slice(),
        }))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }

    /// The number of characters as GLib counts them.
    pub fn char_count(&self) -> usize {
        let c = self.0.chars.get();
        if c != usize::MAX {
            return c;
        }
        let c = utf8::char_count(&self.0.bytes);
        self.0.chars.set(c);
        c
    }

    /// Character `i`, as a string.
    pub fn char_at(&self, i: usize) -> Option<&[u8]> {
        let b = self.as_bytes();
        if i < b.len() && self.char_count() == b.len() {
            return Some(&b[i..=i]);
        }
        utf8::char_at(b, i)
    }
}

impl fmt::Debug for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(self.as_bytes()))
    }
}

impl PartialEq for Str {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

/// A list of values, shared on clone.
#[derive(Clone, Debug)]
pub struct Vector(Rc<Vec<Value>>);

impl Vector {
    pub fn empty() -> Self {
        Vector::from(Vec::new())
    }

    pub fn as_slice(&self) -> &[Value] {
        &self.0
    }

    /// This list to append to in place, when this is the only reference
    /// (see [`Growable`]); otherwise the list back.
    pub fn into_growable(mut self) -> Result<Growable, Vector> {
        match Rc::get_mut(&mut self.0) {
            // The elements' count moves with them; `self` drops empty and
            // takes its box's share with it.
            Some(v) => Ok(Growable {
                charged: slots_bytes(v.len()),
                items: std::mem::take(v),
            }),
            None => Err(self),
        }
    }

    /// The elements, without copying when this is the only reference.
    pub fn into_vec(mut self) -> Vec<Value> {
        match Rc::get_mut(&mut self.0) {
            Some(v) => {
                // They leave the count with the list; whoever builds a new
                // list from them counts them again.
                crate::limits::live::credit(slots_bytes(v.len()));
                std::mem::take(v)
            }
            None => (*self.0).clone(),
        }
    }
}

/// A list being built or appended to, with its share of the memory
/// estimate kept nearly current as it grows (see [`list_bytes`]): charged
/// in batches of [`GROWABLE_BATCH`] bytes, so appending one element costs
/// an add and a compare rather than a thread-local access each.
///
/// This is what makes `concat(acc, [x])` and `[each acc, x]` linear in a
/// tail-recursive accumulator: when the evaluator hands over the only
/// reference to `acc` (`Vector::into_growable`), its buffer is appended to
/// in place. Values are immutable in the language, so that is only done
/// when nothing else can see the list.
#[derive(Debug, Default)]
pub struct Growable {
    items: Vec<Value>,
    /// Bytes of `items` charged so far: a multiple of the slot size, and
    /// at most `slots_bytes(items.len())`.
    charged: u64,
}

/// How far a [`Growable`] may run ahead of its charge. Small, because a
/// deep recursion can hold one unfinished list per frame: a thousand
/// frames each 1 KiB behind are only a megabyte uncounted.
const GROWABLE_BATCH: u64 = 1024;

impl Growable {
    pub fn with_capacity(n: usize) -> Growable {
        Growable {
            items: Vec::with_capacity(n),
            charged: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Room for `n` more elements, growing geometrically (as `Vec` does),
    /// so a list appended to one element at a time is not copied each time.
    pub fn reserve(&mut self, n: usize) {
        self.items.reserve(n);
    }

    #[inline]
    pub fn push(&mut self, v: Value) {
        self.items.push(v);
        let b = slots_bytes(self.items.len());
        if b - self.charged >= GROWABLE_BATCH {
            crate::limits::live::charge(b - self.charged);
            self.charged = b;
        }
    }

    pub fn extend(&mut self, items: impl IntoIterator<Item = Value>) {
        for v in items {
            self.push(v);
        }
    }

    /// The list, counted in full (its `Drop` credits it).
    pub fn finish(mut self) -> Vector {
        let items = std::mem::take(&mut self.items);
        crate::limits::live::charge(list_bytes(items.len()) - self.charged);
        self.charged = 0;
        Vector(Rc::new(items))
    }
}

/// A list dropped unfinished (an error while building it) leaves the count.
impl Drop for Growable {
    fn drop(&mut self) {
        crate::limits::live::credit(self.charged);
    }
}

/// Bytes `n` value slots count for.
#[inline(always)]
fn slots_bytes(n: usize) -> u64 {
    n as u64 * crate::limits::live::SLOT
}

/// Bytes a list of `n` elements counts for towards the evaluator's memory
/// estimate (`crate::limits::live`): its box and its slots. The lists and
/// strings it holds count for themselves when they are made, so a list
/// holding the same small list a million times counts that list once, as
/// it is allocated once. Every list counts, however short (the `live`
/// module describes the program that counting only long ones let through).
/// A list's length never changes once it is shared, so this is the same
/// number when the list is built and when it is freed.
#[inline(always)]
fn list_bytes(n: usize) -> u64 {
    crate::limits::live::BOX + slots_bytes(n)
}

/// Frees nested vectors with a loop instead of recursion. A tail-recursive
/// function can nest a vector a million levels deep (`f(n, acc) = n == 0 ?
/// acc : f(n - 1, [acc])`) without using any stack, and freeing it
/// recursively would then overflow the stack: natively only at depths far
/// beyond that, but in a WASM engine at a few thousand. (The nightly
/// crashes on such a value even natively.)
impl Drop for Vector {
    // Out of line, as it was while it did its own loop: inlined into every
    // value's drop glue it made the evaluator's small hot functions too
    // big to inline themselves (1-4% more instructions on fib, a tail
    // loop and nested `for`s).
    #[inline(never)]
    fn drop(&mut self) {
        let Some(items) = Rc::get_mut(&mut self.0) else {
            return;
        };
        crate::limits::live::credit(list_bytes(items.len()));
        if !items.iter().any(nests) {
            return;
        }
        drop_nested(std::mem::take(items));
    }
}

/// Whether dropping `v` may free values nested inside it (see
/// [`drop_nested`]).
#[inline(always)]
fn nests(v: &Value) -> bool {
    matches!(v, Value::Vector(_) | Value::Object(_))
}

/// Drop `items` and every list and object they hold alone, with a loop
/// instead of recursion (see `Drop for Vector`): an object nesting a list
/// nesting an object a million levels deep is as easy to build as nested
/// lists.
fn drop_nested(items: Vec<Value>) {
    let mut pending = vec![items];
    while let Some(mut items) = pending.pop() {
        for v in items.drain(..) {
            match v {
                Value::Vector(mut inner) => {
                    if let Some(inner) = Rc::get_mut(&mut inner.0)
                        && !inner.is_empty()
                    {
                        // The slots leave the count here; the emptied
                        // list's own drop, at the end of this arm, credits
                        // its box.
                        crate::limits::live::credit(slots_bytes(inner.len()));
                        pending.push(std::mem::take(inner));
                    }
                }
                Value::Object(mut o) => {
                    // The object's own drop credits everything it was
                    // charged, whatever its values have become.
                    if let Some(d) = Rc::get_mut(&mut o.0)
                        && d.values.iter().any(nests)
                    {
                        pending.push(std::mem::take(&mut d.values));
                    }
                }
                _ => {}
            }
        }
    }
}

impl From<Vec<Value>> for Vector {
    #[inline]
    fn from(v: Vec<Value>) -> Self {
        crate::limits::live::charge(list_bytes(v.len()));
        Vector(Rc::new(v))
    }
}

impl std::ops::Deref for Vector {
    type Target = [Value];
    fn deref(&self) -> &[Value] {
        &self.0
    }
}

/// A shared [`Range`], counted towards the evaluator's memory estimate
/// while it lives: a list of three million ranges is 200 MB, most of it
/// the ranges' own allocations, which the list's slots do not cover.
#[derive(Clone, Debug)]
pub struct RangeRef(Rc<RangeData>);

#[derive(Debug)]
struct RangeData(Range);

impl Drop for RangeData {
    fn drop(&mut self) {
        crate::limits::live::credit(crate::limits::live::BOX);
    }
}

impl std::ops::Deref for RangeRef {
    type Target = Range;
    fn deref(&self) -> &Range {
        &self.0.0
    }
}

/// `RangeType`: a lazy arithmetic sequence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub begin: f64,
    pub step: f64,
    pub end: f64,
}

/// `RangeType::MAX_RANGE_STEPS`: the limit for `children()` and `chr()`.
pub const MAX_RANGE_STEPS: u32 = 10000;

impl Range {
    /// `RangeType::numValues`, saturating at `u32::MAX`.
    pub fn num_values(&self) -> u32 {
        let (b, s, e) = (self.begin, self.step, self.end);
        if b.is_nan() || e.is_nan() || s.is_nan() {
            return 0;
        }
        if s < 0.0 {
            if b < e {
                return 0;
            }
        } else if b > e {
            return 0;
        }
        if b == e || s.is_infinite() {
            return 1;
        }
        if b.is_infinite() || e.is_infinite() || s == 0.0 {
            return u32::MAX;
        }
        // nextafter compensates for a quotient just below a whole number.
        let q = next_after((e - b) / s, f64::from(u32::MAX));
        let steps = q as u32;
        if steps == u32::MAX {
            u32::MAX
        } else {
            steps + 1
        }
    }

    /// The values, as `RangeType::iterator` produces them.
    pub fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        (0..self.iter_len()).map(move |i| self.iter_at(i))
    }

    /// How many values [`Range::iter`] gives.
    #[inline(always)]
    pub fn iter_len(&self) -> u32 {
        if self.begin.is_nan() || self.end.is_nan() || self.step.is_nan() || self.step == 0.0 {
            0
        } else {
            self.num_values()
        }
    }

    /// [`Range::iter`]'s value `i`.
    #[inline(always)]
    pub fn iter_at(&self, i: u32) -> f64 {
        if i == 0 {
            self.begin
        } else {
            // `begin_val + step_val * ++i_step`, fused on arm64 like
            // the nightly (see `fma`).
            crate::fma::mul_add(self.step, f64::from(i), self.begin)
        }
    }

    fn cmp_key(&self) -> (f64, f64, u32) {
        (self.begin, self.step, self.num_values())
    }

    /// `RangeType::operator==`.
    pub fn equals(&self, o: &Range) -> bool {
        let (n1, n2) = (self.num_values(), o.num_values());
        if n1 == 0 {
            return n2 == 0;
        }
        if n2 == 0 {
            return false;
        }
        self.begin == o.begin && self.step == o.step && n1 == n2
    }

    /// `RangeType::operator<` (`or_equal`: `<=`). Empty ranges sort
    /// first; otherwise by begin, then step, then count.
    pub fn less(&self, o: &Range, or_equal: bool) -> bool {
        let (b1, s1, n1) = self.cmp_key();
        let (b2, s2, n2) = o.cmp_key();
        if n1 == 0 {
            return or_equal || n2 > 0;
        }
        if n2 == 0 {
            return false;
        }
        b1 < b2
            || (b1 == b2 && (s1 < s2 || (s1 == s2 && if or_equal { n1 <= n2 } else { n1 < n2 })))
    }

    /// `RangeType::operator>` (`or_equal`: `>=`).
    pub fn greater(&self, o: &Range, or_equal: bool) -> bool {
        let (b1, s1, n1) = self.cmp_key();
        let (b2, s2, n2) = o.cmp_key();
        if n2 == 0 {
            return or_equal || n1 > 0;
        }
        if n1 == 0 {
            return false;
        }
        b1 > b2
            || (b1 == b2 && (s1 > s2 || (s1 == s2 && if or_equal { n1 >= n2 } else { n1 > n2 })))
    }
}

/// C's `nextafter`.
pub fn next_after(x: f64, toward: f64) -> f64 {
    if x.is_nan() || toward.is_nan() {
        return f64::NAN;
    }
    if x == toward {
        return toward;
    }
    if x == 0.0 {
        let tiny = f64::from_bits(1);
        return if toward > 0.0 { tiny } else { -tiny };
    }
    let bits = x.to_bits();
    let up = (toward > x) == (x > 0.0);
    f64::from_bits(if up { bits + 1 } else { bits - 1 })
}

/// OpenSCAD's experimental `ObjectType`: string keys in insertion order,
/// each with a value, shared on clone like a list.
///
/// A function stored in an object whose literal has a `this` parameter is
/// a *method*: called through the object (`o.f()`, `o["f"]()`), its
/// `this` is the object. OpenSCAD makes the method when the object is
/// built, giving the function a context that holds the object, which is a
/// reference cycle its garbage collector breaks. Here the object keeps the
/// plain function and binds it when it is read ([`Object::get`]), so
/// nothing refers back to the object: what reads the method holds the
/// object, never the other way round. The bound function is remembered
/// weakly, so reading `o.f` twice gives the same function while either is
/// alive (`o.f == o.f` is true in OpenSCAD, as functions are equal only to
/// themselves). Everything else OpenSCAD's form can show is the same:
/// copying an object into another (`object(o)`) rebinds its methods to the
/// copy, which is why two copies are never equal when they hold one (see
/// [`Object::is_method`]).
#[derive(Clone)]
pub struct Object(Rc<ObjectData>);

pub(crate) struct ObjectData {
    keys: Vec<Str>,
    pub(crate) values: Vec<Value>,
    /// Key to position, once there are more than [`INDEX_AFTER`] keys:
    /// `object()` of a 100,000-entry list, then random access to it, is
    /// one of OpenSCAD's tests.
    index: Option<HashMap<Key, u32, KeyHash>>,
    /// The methods' positions, ascending, each with its bound form while
    /// something holds it.
    methods: Vec<(u32, RefCell<Weak<FunctionValue>>)>,
    /// What this object added to the memory estimate, credited on drop.
    charged: u64,
}

/// Keys hash with SipHash under fixed keys: deterministic (so a WASM build
/// needs no entropy), and not trivially collided by a JSON file's keys,
/// which an agent's model may read from anywhere.
type KeyHash = std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

/// Objects with at most this many keys are searched in order.
const INDEX_AFTER: usize = 8;

/// A key in an object's index, compared and hashed by its bytes. (A
/// `Str`'s only interior mutability is its cached character count, which
/// neither hashing nor comparison reads, hence the `mutable_key_type`
/// allowances where the index is built and read.)
#[derive(Debug)]
struct Key(Str);

impl PartialEq for Key {
    fn eq(&self, o: &Key) -> bool {
        self.0.as_bytes() == o.0.as_bytes()
    }
}

impl Eq for Key {}

impl std::hash::Hash for Key {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.0.as_bytes().hash(h);
    }
}

impl std::borrow::Borrow<[u8]> for Key {
    fn borrow(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

#[allow(clippy::mutable_key_type)]
fn index_of(keys: &[Str]) -> HashMap<Key, u32, KeyHash> {
    keys.iter()
        .enumerate()
        .map(|(i, k)| (Key(k.clone()), i as u32))
        .collect()
}

#[allow(clippy::mutable_key_type)]
fn find_in(keys: &[Str], index: Option<&HashMap<Key, u32, KeyHash>>, key: &[u8]) -> Option<usize> {
    match index {
        Some(m) => m.get(key).map(|&i| i as usize),
        None => keys.iter().position(|k| k.as_bytes() == key),
    }
}

/// Bytes an object of `n` entries (and `methods` methods) counts for: its
/// box, a value and a key slot per entry, and its index when it has one.
/// The keys and values count for themselves, as a list's elements do.
fn object_bytes(n: usize, methods: usize) -> u64 {
    let index = if n > INDEX_AFTER { n as u64 * 32 } else { 0 };
    crate::limits::live::BOX
        + n as u64 * (crate::limits::live::SLOT + 8)
        + index
        + methods as u64 * 24
}

impl Drop for ObjectData {
    fn drop(&mut self) {
        crate::limits::live::credit(self.charged);
        if self.values.iter().any(nests) {
            drop_nested(std::mem::take(&mut self.values));
        }
    }
}

impl Object {
    pub fn len(&self) -> usize {
        self.0.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.values.is_empty()
    }

    /// The keys, in order.
    pub fn keys(&self) -> &[Str] {
        &self.0.keys
    }

    /// The values as stored, in key order (methods unbound).
    pub fn values(&self) -> &[Value] {
        &self.0.values
    }

    pub fn ptr_eq(a: &Object, b: &Object) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    /// The object's identity while it lives, for walks that remember what
    /// they have seen (`memo`'s digests).
    pub fn addr(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }

    fn find(&self, key: &[u8]) -> Option<usize> {
        find_in(&self.0.keys, self.0.index.as_ref(), key)
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        self.find(key).is_some()
    }

    /// Whether the value at `i` is a method (see [`Object`]). Two objects
    /// that are not the same object never hold equal methods: OpenSCAD
    /// makes each object its own.
    pub fn is_method(&self, i: usize) -> bool {
        self.0
            .methods
            .binary_search_by_key(&(i as u32), |m| m.0)
            .is_ok()
    }

    /// `o[key]`: the value, a method bound to this object, or `undef`.
    pub fn get(&self, key: &[u8]) -> Value {
        let Some(i) = self.find(key) else {
            return Value::Undef;
        };
        let v = &self.0.values[i];
        if let Value::Function(f) = v
            && let Ok(m) = self.0.methods.binary_search_by_key(&(i as u32), |m| m.0)
        {
            let cell = &self.0.methods[m].1;
            if let Some(b) = cell.borrow().upgrade() {
                return Value::Function(b);
            }
            let b = Rc::new(FunctionValue::bound(f, self.clone()));
            *cell.borrow_mut() = Rc::downgrade(&b);
            return Value::Function(b);
        }
        v.clone()
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.keys.iter().zip(self.0.values.iter()))
            .finish()
    }
}

/// An object being built: `ObjectType::set` and `del` on a fresh object.
#[derive(Default, Debug)]
pub struct ObjectBuilder {
    keys: Vec<Str>,
    values: Vec<Value>,
    /// Kept current while it has one; a deletion drops it, as OpenSCAD's
    /// `del` clears its map, and the next search rebuilds it.
    index: Option<HashMap<Key, u32, KeyHash>>,
}

impl ObjectBuilder {
    pub fn new() -> ObjectBuilder {
        ObjectBuilder::default()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn find(&mut self, key: &[u8]) -> Option<usize> {
        if self.index.is_none() && self.keys.len() > INDEX_AFTER {
            self.index = Some(index_of(&self.keys));
        }
        find_in(&self.keys, self.index.as_ref(), key)
    }

    /// Set `key`: an existing key keeps its position.
    pub fn set(&mut self, key: Str, value: Value) {
        match self.find(key.as_bytes()) {
            Some(i) => self.values[i] = value,
            None => {
                if let Some(m) = &mut self.index {
                    m.insert(Key(key.clone()), self.keys.len() as u32);
                }
                self.keys.push(key);
                self.values.push(value);
            }
        }
    }

    /// Remove `key`, if present; later keys move up.
    pub fn del(&mut self, key: &[u8]) {
        if let Some(i) = self.find(key) {
            self.keys.remove(i);
            self.values.remove(i);
            self.index = None;
        }
    }

    /// Every entry of `o`, in its order (`object(o)`).
    pub fn extend_from(&mut self, o: &Object) {
        for (k, v) in o.0.keys.iter().zip(o.0.values.iter()) {
            self.set(k.clone(), v.clone());
        }
    }

    /// The object. `is_method` says which stored functions are methods (a
    /// literal with a `this` parameter; see [`Object`]).
    pub fn finish(self, is_method: impl Fn(&FunctionValue) -> bool) -> Object {
        let methods: Vec<(u32, RefCell<Weak<FunctionValue>>)> = self
            .values
            .iter()
            .enumerate()
            .filter(|(_, v)| matches!(v, Value::Function(f) if is_method(f)))
            .map(|(i, _)| (i as u32, RefCell::new(Weak::new())))
            .collect();
        let n = self.values.len();
        let index = match self.index {
            Some(m) => Some(m),
            None if n > INDEX_AFTER => Some(index_of(&self.keys)),
            None => None,
        };
        let charged = object_bytes(n, methods.len());
        crate::limits::live::charge(charged);
        Object(Rc::new(ObjectData {
            keys: self.keys,
            values: self.values,
            index,
            methods,
            charged,
        }))
    }
}

/// A function literal: its expression and the context it closed over.
pub struct FunctionValue {
    pub(crate) unit: u32,
    /// The `ExprKind::Function` expression.
    pub(crate) expr: ExprId,
    pub(crate) ctx: Rc<Ctx>,
    /// A method's object (see [`Object`]): a call binds its `this`
    /// parameter to it.
    pub(crate) this: Option<Object>,
}

/// Bytes a function literal counts for towards the memory estimate: its
/// own allocation and the context it keeps alive. Each evaluation of a
/// literal inside a loop captures that iteration's context, so a list of
/// three million of them measured 760 MB, about 250 bytes each, where the
/// list's slots alone count 16.
const FUNCTION_BYTES: u64 = 256;

impl FunctionValue {
    pub(crate) fn new(unit: u32, expr: ExprId, ctx: Rc<Ctx>) -> FunctionValue {
        crate::limits::live::charge(FUNCTION_BYTES);
        FunctionValue {
            unit,
            expr,
            ctx,
            this: None,
        }
    }

    /// `f` as a method of `this`.
    fn bound(f: &FunctionValue, this: Object) -> FunctionValue {
        crate::limits::live::charge(FUNCTION_BYTES);
        FunctionValue {
            unit: f.unit,
            expr: f.expr,
            ctx: f.ctx.clone(),
            this: Some(this),
        }
    }
}

impl Drop for FunctionValue {
    fn drop(&mut self) {
        crate::limits::live::credit(FUNCTION_BYTES);
        // The last reference to the context it was made in: freed by a
        // loop rather than inside this drop, as a chain of closures can
        // be a million deep (see `context::free_later`).
        if Rc::strong_count(&self.ctx) == 1 {
            let ctx = std::mem::replace(&mut self.ctx, crate::context::empty_ctx());
            crate::context::free_later(ctx);
        }
    }
}

impl fmt::Debug for FunctionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FunctionValue(unit {}, expr {})", self.unit, self.expr.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_counts_like_openscad() {
        let r = |b, s, e| Range {
            begin: b,
            step: s,
            end: e,
        };
        assert_eq!(r(0.0, 1.0, 5.0).num_values(), 6);
        assert_eq!(r(0.0, 0.1, 1.0).num_values(), 11);
        assert_eq!(r(5.0, 1.0, 0.0).num_values(), 0);
        assert_eq!(r(5.0, -1.0, 0.0).num_values(), 6);
        assert_eq!(r(0.0, 0.0, 1.0).num_values(), u32::MAX);
        assert_eq!(r(1.0, 0.0, 1.0).num_values(), 1);
        assert_eq!(r(0.0, 1.0, f64::INFINITY).num_values(), u32::MAX);
        assert_eq!(
            r(0.0, 1.0, 5.0).iter().collect::<Vec<_>>(),
            vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]
        );
        assert_eq!(r(0.0, 0.0, 1.0).iter().count(), 0);
        assert!(r(1.0, 1.0, 0.0).equals(&r(2.0, 1.0, 0.0)));
    }

    #[test]
    fn strings_count_characters() {
        let s = Str::new("ab\u{e4}".as_bytes());
        assert_eq!(s.char_count(), 3);
        assert_eq!(s.char_at(2), Some("\u{e4}".as_bytes()));
        assert_eq!(Str::new(b"abc").char_at(1), Some(&b"b"[..]));
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Undef.to_bool());
        assert!(!Value::Number(0.0).to_bool());
        assert!(Value::Number(f64::NAN).to_bool());
        assert!(!Value::str(b"").to_bool());
        assert!(Value::range(0.0, 1.0, -1.0).to_bool());
        assert!(!Value::vector(vec![]).to_bool());
    }

    /// What `v` adds to the memory estimate when nothing in it is shared.
    fn owned_bytes(v: &Value) -> u64 {
        match v {
            Value::Vector(x) => list_bytes(x.len()) + x.iter().map(owned_bytes).sum::<u64>(),
            Value::Str(s) => str_bytes(s.as_bytes().len()),
            _ => 0,
        }
    }

    #[test]
    fn a_list_grown_in_place_is_counted_as_one_built_whole() {
        // The memory estimate must not drift: a list appended to through
        // `Growable` counts what building it whole counts, and leaves the
        // count when freed, however it was built. Unit tests run on their
        // own threads, so the thread-local count is this test's.
        use crate::limits::live;
        live::reset();
        let item = |i: usize| match i % 3 {
            0 => Value::Number(i as f64),
            1 => Value::vector(vec![Value::Number(1.0); 3]),
            _ => Value::str(b"abc"),
        };
        let mut g = Growable::with_capacity(0);
        for i in 0..3000 {
            g.push(item(i));
            if i % 500 == 0 {
                let v = g.finish();
                assert_eq!(
                    live::get(),
                    owned_bytes(&Value::Vector(v.clone())),
                    "at {i}"
                );
                g = v.into_growable().expect("the only reference");
            }
        }
        let whole = Vector::from((0..3000).map(item).collect::<Vec<_>>());
        let one = owned_bytes(&Value::Vector(whole.clone()));
        drop(whole);
        let kept = g.finish();
        assert_eq!(live::get(), one);
        // Shared, it is not handed out, and it still counts once.
        let other = kept.clone();
        let kept = kept.into_growable().expect_err("shared");
        drop(other);
        assert_eq!(live::get(), one);
        // Taken apart and rebuilt, it counts once too.
        let rebuilt = Vector::from(kept.into_vec());
        assert_eq!(live::get(), one);
        drop(rebuilt);
        assert_eq!(live::get(), 0);
        // Dropped unfinished (an error while building).
        let mut g = Growable::with_capacity(0);
        g.extend((0..2000).map(item));
        assert!(live::get() > 0);
        drop(g);
        assert_eq!(live::get(), 0);
    }

    #[test]
    fn objects_count_and_free_everything() {
        // An object counts its box, slots and index while it lives; its
        // keys and values count for themselves. Everything leaves the
        // count when the last reference goes, bound methods included, and
        // however deep objects and lists nest in each other (freed with a
        // loop: a recursive drop would overflow the stack).
        use crate::limits::live;
        live::reset();
        let mut b = ObjectBuilder::new();
        for i in 0..20 {
            b.set(
                Str::new(format!("k{i}").as_bytes()),
                Value::vector(vec![Value::Number(1.0); 3]),
            );
        }
        b.del(b"k3");
        let o = b.finish(|_| false);
        assert_eq!(o.len(), 19);
        assert!(o.contains(b"k19") && !o.contains(b"k3"));
        assert!(live::get() > object_bytes(19, 0));
        let copy = Value::Object(o.clone());
        drop(o);
        drop(copy);
        assert_eq!(live::get(), 0);
        let mut v = Value::Number(0.0);
        for i in 0..200_000 {
            let mut b = ObjectBuilder::new();
            b.set(Str::new(b"a"), v);
            let o = Value::Object(b.finish(|_| false));
            v = if i % 2 == 0 {
                Value::vector(vec![o])
            } else {
                o
            };
        }
        assert!(live::get() > 0);
        drop(v);
        assert_eq!(live::get(), 0);
    }

    #[test]
    fn small_lists_count_and_sharing_is_free() {
        // The fuzzer's program: a tree of two-element lists sharing their
        // halves costs one list a level, but materialised (as `-t` does)
        // it is 2^depth lists, and every one of them must count.
        use crate::limits::live;
        live::reset();
        let mut t = Value::vector(vec![Value::Number(1.0)]);
        for _ in 0..10 {
            t = Value::vector(vec![t.clone(), t]);
        }
        let shared = live::get();
        assert_eq!(shared, list_bytes(1) + 10 * list_bytes(2));
        let neg = crate::ops::neg(&t).unwrap();
        assert_eq!(live::get() - shared, owned_bytes(&neg));
        assert!(owned_bytes(&neg) > 1024 * list_bytes(1));
        // Freed (iteratively, however it nests), it all leaves the count.
        drop(neg);
        drop(t);
        assert_eq!(live::get(), 0);
    }
}
