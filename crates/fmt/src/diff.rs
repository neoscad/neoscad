//! A unified diff of two texts, line by line (`neoscad fmt --diff`).
//!
//! Myers' O(ND) algorithm on the lines between the common prefix and
//! suffix. Its trace grows with the square of the number of differing
//! lines, so past [`MAX_EDITS`] the middle is shown as one replaced
//! block: still a correct diff, only a coarser one.

const CONTEXT: usize = 3;
const MAX_EDITS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// The edit script turning `a` into `b`: one op per line of either.
fn script(a: &[&str], b: &[&str]) -> Vec<Op> {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut ops = vec![Op::Equal; pre];
    ops.extend(myers(ma, mb).unwrap_or_else(|| {
        let mut v = vec![Op::Delete; ma.len()];
        v.extend(std::iter::repeat_n(Op::Insert, mb.len()));
        v
    }));
    ops.extend(std::iter::repeat_n(Op::Equal, suf));
    ops
}

fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    if max == 0 {
        return Some(Vec::new());
    }
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    // Per round, the part of `v` it can read: (first index, values).
    let mut trace: Vec<(usize, Vec<isize>)> = Vec::new();
    let mut found = None;
    for d in 0..=max.min(MAX_EDITS) as isize {
        let lo = (off - d - 1).max(0) as usize;
        let hi = ((off + d + 1) as usize).min(v.len() - 1);
        trace.push((lo, v[lo..=hi].to_vec()));
        let mut k = -d;
        while k <= d {
            let i = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1]
            } else {
                v[i - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                found = Some(d);
                break;
            }
            k += 2;
        }
        if found.is_some() {
            break;
        }
    }
    let d_end = found?;
    // Walk back through the trace.
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=d_end).rev() {
        let (lo, vals) = &trace[d as usize];
        let at = |k: isize| vals[(k + off) as usize - lo];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let px = at(prev_k);
        let py = px - prev_k;
        while x > px && y > py {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if x == px {
            ops.push(Op::Insert);
            y -= 1;
        } else {
            ops.push(Op::Delete);
            x -= 1;
        }
    }
    while x > 0 && y > 0 {
        ops.push(Op::Equal);
        x -= 1;
        y -= 1;
    }
    ops.reverse();
    Some(ops)
}

/// A unified diff (`--- a`, `+++ b`, `@@` hunks with 3 lines of
/// context); empty when the texts are equal.
pub fn unified(old: &str, new: &str, old_name: &str, new_name: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = script(&a, &b);
    // Positions: for each op, the line numbers in a and b before it.
    let mut pos = Vec::with_capacity(ops.len() + 1);
    let (mut i, mut j) = (0usize, 0usize);
    for op in &ops {
        pos.push((i, j));
        match op {
            Op::Equal => {
                i += 1;
                j += 1;
            }
            Op::Delete => i += 1,
            Op::Insert => j += 1,
        }
    }
    pos.push((i, j));
    let mut out = format!("--- {old_name}\n+++ {new_name}\n");
    let changed: Vec<usize> = (0..ops.len()).filter(|&k| ops[k] != Op::Equal).collect();
    let mut h = 0;
    while h < changed.len() {
        // A hunk: changes closer than twice the context join.
        let start = changed[h].saturating_sub(CONTEXT);
        let mut last = changed[h];
        h += 1;
        while h < changed.len() && changed[h] <= last + 2 * CONTEXT + 1 {
            last = changed[h];
            h += 1;
        }
        let end = (last + CONTEXT + 1).min(ops.len());
        let (a0, b0) = pos[start];
        let (a1, b1) = pos[end];
        let range = |s: usize, n: usize| {
            if n == 1 {
                format!("{}", s + 1)
            } else if n == 0 {
                format!("{s},0")
            } else {
                format!("{},{n}", s + 1)
            }
        };
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(a0, a1 - a0),
            range(b0, b1 - b0)
        ));
        for k in start..end {
            let (ai, bj) = pos[k];
            match ops[k] {
                Op::Equal => out.push_str(&format!(" {}\n", a[ai])),
                Op::Delete => out.push_str(&format!("-{}\n", a[ai])),
                Op::Insert => out.push_str(&format!("+{}\n", b[bj])),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks() {
        let d = unified("a\nb\nc\n", "a\nB\nc\n", "x", "y");
        assert_eq!(d, "--- x\n+++ y\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
        assert_eq!(unified("a\n", "a\n", "x", "y"), "");
        let d = unified("", "new\n", "x", "y");
        assert_eq!(d, "--- x\n+++ y\n@@ -0,0 +1 @@\n+new\n");
    }

    #[test]
    fn script_is_minimal_on_small_inputs() {
        let a = ["a", "b", "c", "d"];
        let b = ["a", "c", "d", "e"];
        let ops = script(&a, &b);
        let edits = ops.iter().filter(|o| **o != Op::Equal).count();
        assert_eq!(edits, 2);
    }
}
