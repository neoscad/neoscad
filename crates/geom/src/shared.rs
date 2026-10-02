//! The union of a preview product's negatives, computing each repeated
//! subtree once.
//!
//! A render caches every subtree by its key, so the eight translated
//! copies of one `menger_negative` level are computed once and moved. A
//! preview's product has only leaves: the Menger example at depth 4 is one
//! positive minus 1,756 negatives, and their flat union recomputed every
//! copy (28 s in the web core against the render's 8 s). Here the
//! negatives are put back into the tree they came from (each leaf's
//! [`Chain`]), and the union is taken bottom-up over that tree: a subtree
//! whose key and present leaves match an earlier one is the earlier one's
//! union, moved.
//!
//! Which subtrees are equal, the order of every union and the IDs of every
//! conversion are decided before any boolean runs, so the result is the
//! same at any thread count. The unions of one height run in parallel.
//! A product without a repeated subtree that has a union of its own is
//! left to the flat union ([`Plan::new`] gives `None`), so its mesh is
//! exactly what it was.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use manifold_rust::cancel::CancelToken;

use crate::csg::{Chain, Held, Negative, Range, Stop, union_tree};
use crate::manifold_geom::ManifoldGeometry;
use crate::polyset::PolySet;
use crate::{IDENTITY, Matrix, Unsupported};

/// A subtree's geometry in its parent's coordinates: `m` times a base,
/// which is a negative's mesh or a union's result. A subtree with one part
/// (a `translate` of a `group` of one leaf) only composes matrices, so a
/// chain of transforms costs no mesh copy.
#[derive(Clone, Copy, Debug)]
struct Val {
    base: Base,
    m: Matrix,
}

#[derive(Clone, Copy, Debug)]
enum Base {
    /// The mesh of negative `i`.
    Mesh(usize),
    /// The result of union `u`.
    Union(usize),
}

/// One union to compute: its parts in the subtree's parent's coordinates.
#[derive(Debug)]
struct Union {
    parts: Vec<Val>,
    /// 1 + the greatest height of a union among its parts.
    height: usize,
    /// The parts (of later unions, and the final result) that use this
    /// one: the last to use it takes it instead of copying it.
    uses: usize,
}

/// What makes two subtrees equal in a product: the subtree's key, the
/// negatives at its node (their tints, and whether they are slabs) and its
/// children with their positions and classes. Equal keys alone are not
/// enough: pruning by box can drop different leaves from two copies.
#[derive(Hash, PartialEq, Eq)]
struct Sig {
    key: u128,
    leaves: Vec<([u32; 4], bool)>,
    children: Vec<(u32, usize)>,
}

/// The order of a product's unions, decided before any runs.
#[derive(Debug)]
pub(crate) struct Plan {
    unions: Vec<Union>,
    /// The negatives' union in model coordinates.
    root: Val,
}

/// A node of the tree the negatives came from, as far as they reach.
struct TNode {
    link: Arc<Chain>,
    children: Vec<usize>,
    /// Negatives at this node.
    leaves: Vec<usize>,
}

impl Plan {
    /// The plan for `negatives`, or `None` when it would compute what the
    /// flat union computes: a negative without a chain, or no subtree
    /// with a union of its own that occurs twice.
    pub(crate) fn new(negatives: &[Negative]) -> Option<Plan> {
        if negatives.len() < 4 {
            return None;
        }
        // The tree of the negatives' ancestors, children in first-seen
        // order (the product's order).
        let mut nodes: Vec<TNode> = Vec::new();
        let mut at: HashMap<usize, usize> = HashMap::new();
        let mut path: Vec<Arc<Chain>> = Vec::new();
        for (i, n) in negatives.iter().enumerate() {
            path.clear();
            let mut link = Some(n.chain.clone()?);
            while let Some(l) = link {
                link = l.parent.clone();
                path.push(l);
            }
            let mut parent: Option<usize> = None;
            for l in path.iter().rev() {
                let t = match at.get(&l.index) {
                    Some(&t) => t,
                    None => {
                        let t = nodes.len();
                        nodes.push(TNode {
                            link: l.clone(),
                            children: Vec::new(),
                            leaves: Vec::new(),
                        });
                        at.insert(l.index, t);
                        match parent {
                            Some(p) => nodes[p].children.push(t),
                            // A second top: not one tree.
                            None if t != 0 => return None,
                            None => {}
                        }
                        t
                    }
                };
                parent = Some(t);
            }
            nodes[parent?].leaves.push(i);
        }

        // Classes bottom-up (children before parents), with each class's
        // value; a class seen again is a copy.
        let mut classes: HashMap<Sig, usize> = HashMap::new();
        let mut vals: Vec<Val> = Vec::new();
        let mut class_of = vec![usize::MAX; nodes.len()];
        let mut unions: Vec<Union> = Vec::new();
        let mut shared = false;
        let mut order = Vec::with_capacity(nodes.len());
        let mut stack = vec![(0usize, false)];
        while let Some((t, done)) = stack.pop() {
            if done {
                order.push(t);
            } else {
                stack.push((t, true));
                stack.extend(nodes[t].children.iter().rev().map(|&c| (c, false)));
            }
        }
        for t in order {
            let node = &nodes[t];
            let sig = Sig {
                key: node.link.key,
                leaves: node
                    .leaves
                    .iter()
                    .map(|&i| (negatives[i].tint.key(), negatives[i].slab))
                    .collect(),
                children: node
                    .children
                    .iter()
                    .map(|&c| (nodes[c].link.pos, class_of[c]))
                    .collect(),
            };
            if let Some(&c) = classes.get(&sig) {
                shared |= matches!(vals[c].base, Base::Union(_));
                class_of[t] = c;
                continue;
            }
            let own = node.link.own;
            let mut parts: Vec<Val> = node
                .leaves
                .iter()
                .map(|&i| Val {
                    base: Base::Mesh(i),
                    m: if negatives[i].slab {
                        scaled_z(&IDENTITY)
                    } else {
                        IDENTITY
                    },
                })
                .chain(node.children.iter().map(|&c| vals[class_of[c]]))
                .collect();
            if own != IDENTITY {
                for p in &mut parts {
                    p.m = mul(&own, &p.m);
                }
            }
            let val = if parts.len() == 1 {
                parts[0]
            } else {
                let height = 1 + parts
                    .iter()
                    .filter_map(|p| match p.base {
                        Base::Union(u) => Some(unions[u].height),
                        Base::Mesh(_) => None,
                    })
                    .max()
                    .unwrap_or(0);
                for p in &parts {
                    if let Base::Union(u) = p.base {
                        unions[u].uses += 1;
                    }
                }
                unions.push(Union {
                    parts,
                    height,
                    uses: 0,
                });
                Val {
                    base: Base::Union(unions.len() - 1),
                    m: IDENTITY,
                }
            };
            let c = vals.len();
            vals.push(val);
            classes.insert(sig, c);
            class_of[t] = c;
        }
        if !shared {
            return None;
        }
        let root = vals[class_of[0]];
        if let Base::Union(u) = root.base {
            unions[u].uses += 1;
        }
        Some(Plan { unions, root })
    }

    /// What the plan computes, into a product's cache key
    /// ([`crate::csg::product_key`]): each union's parts in
    /// order, which negative or earlier union each is and its placement,
    /// and the result's. Heights and use counts follow from these. The
    /// subtree keys the plan was found by are left out: they only decided
    /// which subtrees are copies, which is what the parts say.
    pub(crate) fn hash_into(&self, h: &mut sha2::Sha256) {
        use sha2::Digest as _;
        let val = |h: &mut sha2::Sha256, v: &Val| {
            let (tag, i) = match v.base {
                Base::Mesh(i) => (0u8, i),
                Base::Union(u) => (1u8, u),
            };
            h.update([tag]);
            h.update((i as u64).to_le_bytes());
            crate::csg::hash_matrix(h, &v.m);
        };
        h.update((self.unions.len() as u64).to_le_bytes());
        for u in &self.unions {
            h.update((u.parts.len() as u64).to_le_bytes());
            for p in &u.parts {
                val(h, p);
            }
        }
        val(h, &self.root);
    }

    /// The IDs each union's conversions take, in union order, `need`
    /// being what converting one mesh takes.
    pub(crate) fn needs(&self, negatives: &[Negative], need: impl Fn(&PolySet) -> u32) -> Vec<u32> {
        self.unions
            .iter()
            .map(|u| {
                u.parts
                    .iter()
                    .map(|p| match p.base {
                        Base::Mesh(i) => need(&negatives[i].mesh),
                        Base::Union(_) => 0,
                    })
                    .sum()
            })
            .collect()
    }

    /// The union of `negatives` in model coordinates, each union's
    /// conversions drawing IDs from `firsts` (one start per union, sized
    /// by [`Plan::needs`]). The unions of one height run in parallel;
    /// each is a function of its parts and its IDs only, so the thread
    /// count changes nothing.
    pub(crate) fn union<'s>(
        &self,
        negatives: &[Negative],
        firsts: &[u32],
        stop: &'s Stop,
        token: Option<&CancelToken>,
    ) -> Result<Option<Held<'s>>, Unsupported> {
        let top = self.unions.iter().map(|u| u.height).max().unwrap_or(0);
        let mut by_height: Vec<Vec<usize>> = vec![Vec::new(); top + 1];
        for (i, u) in self.unions.iter().enumerate() {
            by_height[u.height].push(i);
        }
        // Each computed union, with the uses it has left. A copy is made
        // under the lock, so the last use (which takes the union, saving a
        // copy of what can be most of the result) comes after every other.
        let memo: Vec<Mutex<(Option<Held<'s>>, usize)>> = self
            .unions
            .iter()
            .map(|u| Mutex::new((None, u.uses)))
            .collect();
        for level in by_height.iter().skip(1) {
            stop.check()?;
            let done = {
                let memo = &memo;
                let compute = |&u: &usize| -> Result<Option<Held<'s>>, Unsupported> {
                    let ids = Range(Cell::new(firsts[u]));
                    let parts = self.unions[u]
                        .parts
                        .iter()
                        .map(|p| materialize(p, negatives, memo, &ids, stop))
                        .collect::<Result<Vec<_>, _>>()?;
                    union_tree(parts, stop, token)
                };
                #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
                let done: Vec<Option<Held<'s>>> = {
                    use rayon::prelude::*;
                    level.par_iter().map(compute).collect::<Result<_, _>>()?
                };
                #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
                let done: Vec<Option<Held<'s>>> =
                    level.iter().map(compute).collect::<Result<_, _>>()?;
                done
            };
            for (&u, r) in level.iter().zip(done) {
                lock(&memo[u]).0 = r;
            }
        }
        materialize(&self.root, negatives, &memo, &Range(Cell::new(0)), stop).map(Some)
    }
}

/// `v` as a solid where its matrix puts it: a negative's mesh moved and
/// converted (IDs from `ids`), or a computed union copied and moved. A
/// copy keeps the union's original IDs, as a render's cached subtree does.
fn materialize<'s>(
    v: &Val,
    negatives: &[Negative],
    memo: &[Mutex<(Option<Held<'s>>, usize)>],
    ids: &Range,
    stop: &'s Stop,
) -> Result<Held<'s>, Unsupported> {
    match v.base {
        Base::Mesh(i) => {
            let mut ps = PolySet::clone(&negatives[i].mesh);
            ps.transform(&v.m);
            let (mut warnings, mut errors) = (Vec::new(), Vec::new());
            stop.hold(ManifoldGeometry::from_polyset(
                &ps,
                ids,
                &mut warnings,
                &mut errors,
            ))
        }
        Base::Union(u) => {
            let mut g = {
                let mut slot = lock(&memo[u]);
                slot.1 -= 1;
                if slot.1 == 0 {
                    slot.0.take().map(|mut h| h.take())
                } else {
                    slot.0.as_ref().and_then(Held::get).cloned()
                }
            }
            .unwrap_or_default();
            if v.m != IDENTITY {
                g.transform(&v.m);
            }
            stop.hold(g)
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `m * n`.
fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|k| a[r][k] * b[k][c]).sum()))
}

/// A 2D slab's stretch in z, as the preview draws subtracted slabs.
fn scaled_z(m: &Matrix) -> Matrix {
    let mut out = *m;
    for row in &mut out {
        row[2] *= 1.1;
    }
    out
}
