//! Just enough of libc++'s `std::unordered_map` to iterate in its order.
//!
//! When libtess2 leaves part of a polygon uncovered, OpenSCAD's
//! `EdgeDict::triangulateLoops` closes the gap by walking two
//! `std::unordered_map`s, and the triangles it makes depend on their
//! iteration order. That order is libc++'s: one singly linked list of
//! nodes, a new key going to the front of its bucket's run (or to the
//! front of the whole list if the bucket was empty), buckets growing to
//! `2n+1` rounded up to a prime, and a rehash relinking runs as it meets
//! them. The nightly is built against libc++ (macOS), so this reproduces
//! `__hash_table`'s `__emplace_unique_key_args`, `__do_rehash<true>` and
//! `remove` as the SDK headers have them, with `max_load_factor` 1.
//!
//! Hashes are the ones OpenSCAD's maps use: `std::hash<int>` (the value)
//! and `boost::hash<std::pair<int, int>>` (`hash_combine` as Boost has had
//! it since 1.81; the nightly's macOS build uses Boost 1.92).

const NIL: u32 = u32::MAX;
/// The `__first_node_` before-begin sentinel.
const HEAD: u32 = 0;

/// A key hashed the way OpenSCAD's map hashes it.
pub(super) trait CxxHash: PartialEq + Copy + Default {
    fn cxx_hash(&self) -> u64;
}

impl CxxHash for i32 {
    /// `std::hash<int>` in libc++: `static_cast<size_t>(v)`.
    fn cxx_hash(&self) -> u64 {
        i64::from(*self) as u64
    }
}

/// Boost's `hash_detail::hash_mix` for a 64-bit `size_t`.
fn hash_mix(mut x: u64) -> u64 {
    const M: u64 = 0x0e98_46af_9b1a_615d;
    x ^= x >> 32;
    x = x.wrapping_mul(M);
    x ^= x >> 32;
    x = x.wrapping_mul(M);
    x ^= x >> 28;
    x
}

impl CxxHash for (i32, i32) {
    /// `boost::hash_value(std::pair)`: `hash_combine` of both members into
    /// a zero seed; `boost::hash<int>` is the sign-extended value.
    fn cxx_hash(&self) -> u64 {
        let combine = |seed: u64, v: i32| {
            hash_mix(
                seed.wrapping_add(0x9e37_79b9)
                    .wrapping_add(i64::from(v) as u64),
            )
        };
        combine(combine(0, self.0), self.1)
    }
}

struct Node<K, V> {
    next: u32,
    hash: u64,
    key: K,
    val: V,
}

pub(super) struct CxxMap<K, V> {
    nodes: Vec<Node<K, V>>,
    /// Per bucket, the node before its first node (possibly `HEAD`).
    buckets: Vec<u32>,
    size: usize,
}

impl<K: CxxHash, V: Default> Default for CxxMap<K, V> {
    fn default() -> Self {
        let mut m = CxxMap {
            nodes: Vec::new(),
            buckets: Vec::new(),
            size: 0,
        };
        m.reset();
        m
    }
}

/// `std::__constrain_hash`
fn constrain(h: u64, bc: usize) -> usize {
    let bc = bc as u64;
    (if bc & (bc.wrapping_sub(1)) == 0 {
        h & (bc - 1)
    } else if h < bc {
        h
    } else {
        h % bc
    }) as usize
}

/// `std::__next_prime`: the smallest prime at least `n` (0 stays 0).
fn next_prime(n: usize) -> usize {
    if n <= 2 {
        return if n == 0 { 0 } else { 2 };
    }
    let mut p = n;
    loop {
        if !p.is_multiple_of(2)
            && (3..)
                .step_by(2)
                .take_while(|d| d * d <= p)
                .all(|d| !p.is_multiple_of(d))
        {
            return p;
        }
        p += 1;
    }
}

impl<K: CxxHash, V: Default> CxxMap<K, V> {
    /// A freshly constructed map: no buckets.
    pub(super) fn reset(&mut self) {
        self.nodes.clear();
        self.nodes.push(Node {
            next: NIL,
            hash: 0,
            key: K::default(),
            val: V::default(),
        });
        self.buckets.clear();
        self.size = 0;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The first node in iteration order, or `None`.
    pub(super) fn first(&self) -> Option<u32> {
        let n = self.nodes[HEAD as usize].next;
        (n != NIL).then_some(n)
    }

    pub(super) fn next(&self, n: u32) -> Option<u32> {
        let n = self.nodes[n as usize].next;
        (n != NIL).then_some(n)
    }

    pub(super) fn key(&self, n: u32) -> K {
        self.nodes[n as usize].key
    }

    pub(super) fn val(&self, n: u32) -> &V {
        &self.nodes[n as usize].val
    }

    pub(super) fn val_mut(&mut self, n: u32) -> &mut V {
        &mut self.nodes[n as usize].val
    }

    /// `find`
    pub(super) fn find(&self, k: &K) -> Option<u32> {
        let bc = self.buckets.len();
        if bc == 0 || self.size == 0 {
            return None;
        }
        let hash = k.cxx_hash();
        let chash = constrain(hash, bc);
        let mut nd = self.buckets[chash];
        if nd == NIL {
            return None;
        }
        nd = self.nodes[nd as usize].next;
        while nd != NIL {
            let node = &self.nodes[nd as usize];
            if node.hash != hash && constrain(node.hash, bc) != chash {
                break;
            }
            if node.hash == hash && node.key == *k {
                return Some(nd);
            }
            nd = node.next;
        }
        None
    }

    /// `operator[]`: the node for `k`, inserting a default value if absent.
    pub(super) fn entry(&mut self, k: K) -> u32 {
        if let Some(n) = self.find(&k) {
            return n;
        }
        let hash = k.cxx_hash();
        let nd = self.nodes.len() as u32;
        self.nodes.push(Node {
            next: NIL,
            hash,
            key: k,
            val: V::default(),
        });
        let mut bc = self.buckets.len();
        if (self.size + 1) as f32 > bc as f32 || bc == 0 {
            let pow2 = bc > 2 && bc & (bc - 1) == 0;
            self.rehash((2 * bc + usize::from(!pow2)).max(self.size + 1));
            bc = self.buckets.len();
        }
        let chash = constrain(hash, bc);
        let pn = self.buckets[chash];
        if pn == NIL {
            self.nodes[nd as usize].next = self.nodes[HEAD as usize].next;
            self.nodes[HEAD as usize].next = nd;
            self.buckets[chash] = HEAD;
            let nx = self.nodes[nd as usize].next;
            if nx != NIL {
                let b = constrain(self.nodes[nx as usize].hash, bc);
                self.buckets[b] = nd;
            }
        } else {
            self.nodes[nd as usize].next = self.nodes[pn as usize].next;
            self.nodes[pn as usize].next = nd;
        }
        self.size += 1;
        nd
    }

    /// `__rehash<true>` for growth (the only way inserts call it).
    fn rehash(&mut self, n: usize) {
        let n = if n == 1 {
            2
        } else if n & (n - 1) != 0 {
            next_prime(n)
        } else {
            n
        };
        if n > self.buckets.len() {
            self.do_rehash(n);
        }
    }

    /// `__do_rehash<true>`
    fn do_rehash(&mut self, nbc: usize) {
        self.buckets.clear();
        self.buckets.resize(nbc, NIL);
        let mut pp = HEAD;
        let mut cp = self.nodes[pp as usize].next;
        if cp == NIL {
            return;
        }
        let mut phash = constrain(self.nodes[cp as usize].hash, nbc);
        self.buckets[phash] = pp;
        pp = cp;
        cp = self.nodes[cp as usize].next;
        while cp != NIL {
            let chash = constrain(self.nodes[cp as usize].hash, nbc);
            if chash == phash {
                pp = cp;
            } else if self.buckets[chash] == NIL {
                self.buckets[chash] = pp;
                pp = cp;
                phash = chash;
            } else {
                // Move cp to the front of its bucket's run.
                let b = self.buckets[chash];
                self.nodes[pp as usize].next = self.nodes[cp as usize].next;
                self.nodes[cp as usize].next = self.nodes[b as usize].next;
                self.nodes[b as usize].next = cp;
            }
            cp = self.nodes[pp as usize].next;
        }
    }

    /// `erase(iterator)` (`remove`).
    pub(super) fn erase(&mut self, cn: u32) {
        let bc = self.buckets.len();
        let chash = constrain(self.nodes[cn as usize].hash, bc);
        let mut pn = self.buckets[chash];
        while self.nodes[pn as usize].next != cn {
            pn = self.nodes[pn as usize].next;
        }
        let cnext = self.nodes[cn as usize].next;
        if (pn == HEAD || constrain(self.nodes[pn as usize].hash, bc) != chash)
            && (cnext == NIL || constrain(self.nodes[cnext as usize].hash, bc) != chash)
        {
            self.buckets[chash] = NIL;
        }
        if cnext != NIL {
            let nhash = constrain(self.nodes[cnext as usize].hash, bc);
            if nhash != chash {
                self.buckets[nhash] = pn;
            }
        }
        self.nodes[pn as usize].next = cnext;
        self.nodes[cn as usize].next = NIL;
        self.size -= 1;
    }
}
