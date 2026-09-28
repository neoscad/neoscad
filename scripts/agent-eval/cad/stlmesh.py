"""STL mesh analysis for the agent CAD comparison grader, Python stdlib only.

The grader must not trust any of the tools under test, so it imports none
of NeoSCAD, OpenSCAD or CadQuery (nor numpy/trimesh, which the tools'
environments might share): everything here reads the triangle soup of the
STL file directly.

What it provides:

- `load_stl`: ASCII or binary STL, welded into indexed triangles by exact
  float32 coordinate equality (the tools write a shared vertex with the
  same bits in every triangle; a tolerance weld would hide cracks).
- `topology`: ModelRift's "clean" facts (watertight, non-manifold and
  boundary edges, components, flipped faces, sitting on z = 0) plus
  volume, area, bbox, genus and overhang area.
- `RayIndex`: axis-aligned rays through the solid, returning the inside
  intervals by winding number. Dimensional checks use these rather than
  vertex positions, because a ray measures what a caliper would.
- `section`: a planar cross-section as closed loops, chained by mesh edge
  ids so two faces sharing an edge always meet at the same point.
"""

import math
import struct

# Rays and section planes are nudged off the exact query coordinate by
# these irrational-looking offsets. Without them a ray down the axis of a
# part hits vertices and edges exactly (both neighbouring faces count, or
# neither), and the inside intervals come out doubled or missing.
JITTER_U = 1.7320508e-5
JITTER_V = 2.2360679e-5
PLANE_JITTER = 1.4142135e-5


class MeshError(Exception):
    pass


def _f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def load_stl(path, max_triangles=900_000):
    """Returns (verts, tris). Raises MeshError for an unreadable file or one
    with more than `max_triangles`: the grader holds about 2 KB per
    triangle (a 360k-triangle sphere peaked at 737 MB), so 900k keeps it
    under the harness's 2 GB guard. A part over the limit is graded as
    unreadable rather than killed half-way."""
    with open(path, "rb") as f:
        data = f.read()
    soup = None
    if len(data) >= 84:
        (n,) = struct.unpack_from("<I", data, 80)
        if 84 + 50 * n == len(data):
            if n > max_triangles:
                raise MeshError(f"{n} triangles is over the grader's limit of {max_triangles}")
            soup = []
            off = 84
            for _ in range(n):
                v = struct.unpack_from("<12f", data, off)
                soup.append((v[3:6], v[6:9], v[9:12]))
                off += 50
    if soup is None:
        text = data.decode("ascii", errors="replace")
        if "facet" not in text[:4096] and not text.lstrip().startswith("solid"):
            raise MeshError("not an STL file")
        soup = []
        cur = []
        for line in text.splitlines():
            parts = line.split()
            if parts and parts[0] == "vertex":
                # Round through float32 so ASCII and binary files weld alike.
                cur.append(tuple(_f32(float(p)) for p in parts[1:4]))
                if len(cur) == 3:
                    soup.append(tuple(cur))
                    cur = []
                    if len(soup) > max_triangles:
                        raise MeshError(f"over the grader's limit of {max_triangles} triangles")
    index = {}
    verts = []
    tris = []
    for t in soup:
        ids = []
        for p in t:
            k = (p[0], p[1], p[2])
            i = index.get(k)
            if i is None:
                i = index[k] = len(verts)
                verts.append(k)
            ids.append(i)
        tris.append(tuple(ids))
    return verts, tris


def _sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def _cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def face_normal(verts, t):
    a, b, c = verts[t[0]], verts[t[1]], verts[t[2]]
    return _cross(_sub(b, a), _sub(c, a))


def _signed_vol6(verts, t):
    a, b, c = verts[t[0]], verts[t[1]], verts[t[2]]
    return (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))


class _UF:
    def __init__(self, n):
        self.p = list(range(n))

    def find(self, x):
        p = self.p
        while p[x] != x:
            p[x] = p[p[x]]
            x = p[x]
        return x

    def union(self, a, b):
        a, b = self.find(a), self.find(b)
        if a != b:
            self.p[max(a, b)] = min(a, b)


def topology(verts, tris, bed_tol=0.01):
    """The mesh facts ModelRift's checker reported, and their "clean"
    verdict: watertight, one component, no non-manifold or boundary edges,
    and sitting on z = 0 (min z within `bed_tol`)."""
    n = len(tris)
    if n == 0:
        return {"triangles": 0, "clean": False, "reasons": ["empty mesh"]}
    edges = {}
    degenerate = 0
    for fi, t in enumerate(tris):
        if t[0] == t[1] or t[1] == t[2] or t[0] == t[2]:
            degenerate += 1
            continue
        nx, ny, nz = face_normal(verts, t)
        if nx * nx + ny * ny + nz * nz < 1e-24:
            degenerate += 1
        for a, b in ((t[0], t[1]), (t[1], t[2]), (t[2], t[0])):
            key = (a, b) if a < b else (b, a)
            edges.setdefault(key, []).append((fi, 1 if a < b else -1))
    boundary = sum(1 for e in edges.values() if len(e) == 1)
    nonmanifold = sum(1 for e in edges.values() if len(e) > 2)

    # Components: faces joined by any shared edge (a mesh touching itself
    # only at a vertex is two components, as two printed bodies would be).
    uf = _UF(n)
    for e in edges.values():
        for f, _ in e[1:]:
            uf.union(e[0][0], f)
    comp_of = [uf.find(i) for i in range(n)]
    comps = {}
    for i, c in enumerate(comp_of):
        comps.setdefault(c, []).append(i)

    # Orientation: propagate across manifold edges. Two faces sharing an
    # edge are consistent when they traverse it in opposite directions.
    parity = [None] * n
    adj = [[] for _ in range(n)]
    for e in edges.values():
        if len(e) == 2:
            (f1, d1), (f2, d2) = e
            same = d1 == d2  # same direction: one of them is flipped
            adj[f1].append((f2, same))
            adj[f2].append((f1, same))
    conflicts = 0
    for start in range(n):
        if parity[start] is not None:
            continue
        parity[start] = 0
        stack = [start]
        while stack:
            f = stack.pop()
            for g, same in adj[f]:
                want = parity[f] ^ (1 if same else 0)
                if parity[g] is None:
                    parity[g] = want
                    stack.append(g)
                elif parity[g] != want:
                    conflicts += 1
    flipped = 0
    comp_info = []
    for c, faces in comps.items():
        vol_as_is = sum(_signed_vol6(verts, tris[f]) for f in faces) / 6
        vol_fixed = sum(_signed_vol6(verts, tris[f]) * (-1 if parity[f] else 1) for f in faces) / 6
        ones = sum(1 for f in faces if parity[f])
        # After making the component consistent, a negative volume means
        # the parity-0 faces point inwards.
        wrong = (len(faces) - ones) if vol_fixed < 0 else ones
        flipped += wrong
        comp_info.append({"faces": len(faces), "volume": round(abs(vol_fixed), 4),
                          "volume_as_written": round(vol_as_is, 4), "flipped_faces": wrong})
    comp_info.sort(key=lambda c: -c["faces"])

    xs = [v[0] for v in verts]
    ys = [v[1] for v in verts]
    zs = [v[2] for v in verts]
    bmin = (min(xs), min(ys), min(zs))
    bmax = (max(xs), max(ys), max(zs))
    area = 0.0
    vol6 = 0.0
    bed_area = 0.0
    overhang_area = 0.0
    cos45 = math.cos(math.radians(45))
    zmin = bmin[2]
    for t in tris:
        nx, ny, nz = face_normal(verts, t)
        ln = math.sqrt(nx * nx + ny * ny + nz * nz)
        if ln == 0:
            continue
        a = ln / 2
        area += a
        vol6 += _signed_vol6(verts, t)
        uz = nz / ln
        if uz < -0.999 and all(verts[i][2] <= zmin + 1e-3 for i in t):
            bed_area += a
        elif uz < -cos45 and min(verts[i][2] for i in t) > zmin + 0.1:
            overhang_area += a
    used = {i for t in tris for i in t}
    V, E, F = len(used), len(edges), n
    watertight = boundary == 0 and nonmanifold == 0
    genus = None
    if watertight and len(comps) == 1:
        genus = (2 - (V - E + F)) / 2
    on_bed = abs(bmin[2]) <= bed_tol
    reasons = []
    if boundary:
        reasons.append(f"{boundary} boundary edges")
    if nonmanifold:
        reasons.append(f"{nonmanifold} non-manifold edges")
    if len(comps) != 1:
        reasons.append(f"{len(comps)} components")
    if not on_bed:
        reasons.append(f"min z is {bmin[2]:.4f}, not 0")
    return {
        "triangles": n,
        "vertices": V,
        "edges": E,
        "bbox_min": [round(x, 4) for x in bmin],
        "bbox_max": [round(x, 4) for x in bmax],
        "size": [round(bmax[i] - bmin[i], 4) for i in range(3)],
        "volume": round(vol6 / 6, 4),
        "area": round(area, 4),
        "watertight": watertight,
        "boundary_edges": boundary,
        "nonmanifold_edges": nonmanifold,
        "degenerate_faces": degenerate,
        "flipped_faces": flipped,
        "orientation_conflicts": conflicts,
        "components": len(comps),
        "component_detail": comp_info[:10],
        "genus": genus,
        "on_bed": on_bed,
        "bed_contact_area": round(bed_area, 3),
        "overhang_area_45": round(overhang_area, 3),
        "clean": watertight and len(comps) == 1 and on_bed,
        "reasons": reasons,
    }


# ---------------------------------------------------------------------------
# Rays


def _axes(a):
    """(u, v) for rays along axis a, a cyclic order so u x v = a."""
    return (a + 1) % 3, (a + 2) % 3


class RayIndex:
    """Axis-aligned rays through a mesh, bucketed on a grid in the plane
    across the axis so a query tests only nearby triangles."""

    def __init__(self, verts, tris, axis):
        self.verts, self.tris, self.axis = verts, tris, axis
        u, v = _axes(axis)
        self.u, self.v = u, v
        us = [p[u] for p in verts] or [0]
        vs = [p[v] for p in verts] or [0]
        self.u0, self.v0 = min(us), min(vs)
        du, dv = max(us) - self.u0, max(vs) - self.v0
        cells = max(16, min(512, int(math.sqrt(max(len(tris), 1)) / 2)))
        self.n = cells
        self.cu = max(du, 1e-9) / cells
        self.cv = max(dv, 1e-9) / cells
        grid = {}
        for fi, t in enumerate(tris):
            pu = [verts[i][u] for i in t]
            pv = [verts[i][v] for i in t]
            i0, i1 = self._cell(min(pu), self.u0, self.cu), self._cell(max(pu), self.u0, self.cu)
            j0, j1 = self._cell(min(pv), self.v0, self.cv), self._cell(max(pv), self.v0, self.cv)
            for i in range(i0, i1 + 1):
                for j in range(j0, j1 + 1):
                    grid.setdefault(i * (cells + 1) + j, []).append(fi)
        self.grid = grid

    def _cell(self, x, x0, c):
        return min(self.n, max(0, int((x - x0) / c)))

    def hits(self, pu, pv):
        """(t, direction) for each face the line at (pu, pv) crosses:
        direction +1 where the ray (towards +axis) enters the solid."""
        pu += JITTER_U
        pv += JITTER_V
        i, j = self._cell(pu, self.u0, self.cu), self._cell(pv, self.v0, self.cv)
        out = []
        verts, a, u, v = self.verts, self.axis, self.u, self.v
        for fi in self.grid.get(i * (self.n + 1) + j, ()):
            t = self.tris[fi]
            A, B, C = verts[t[0]], verts[t[1]], verts[t[2]]
            # 2D barycentrics in the (u, v) plane.
            d = (B[u] - A[u]) * (C[v] - A[v]) - (C[u] - A[u]) * (B[v] - A[v])
            if d == 0:
                continue
            w1 = ((pu - A[u]) * (C[v] - A[v]) - (C[u] - A[u]) * (pv - A[v])) / d
            w2 = ((B[u] - A[u]) * (pv - A[v]) - (pu - A[u]) * (B[v] - A[v])) / d
            w0 = 1 - w1 - w2
            if w0 < 0 or w1 < 0 or w2 < 0:
                continue
            ta = w0 * A[a] + w1 * B[a] + w2 * C[a]
            # d > 0: the triangle winds counter-clockwise seen from +axis, so
            # its normal points along +axis and the ray leaves the solid.
            out.append((ta, -1 if d > 0 else 1))
        out.sort()
        return out

    def intervals(self, pu, pv):
        """Inside intervals [(t0, t1)] along the line, by winding number."""
        w = 0
        res = []
        start = None
        for t, d in self.hits(pu, pv):
            w += d
            if w > 0 and start is None:
                start = t
            elif w <= 0 and start is not None:
                if res and abs(start - res[-1][1]) < 1e-7:
                    res[-1] = (res[-1][0], t)
                else:
                    res.append((start, t))
                start = None
        return res


def ray_intervals(index, **coords):
    """Convenience: intervals along index.axis at named coordinates, e.g.
    ray_intervals(ix, y=3, z=4) for an x-ray."""
    names = "xyz"
    return index.intervals(coords[names[index.u]], coords[names[index.v]])


# ---------------------------------------------------------------------------
# Sections


class SliceIndex:
    """Triangles bucketed along one axis, for repeated planar sections."""

    def __init__(self, verts, tris, axis, bins=256):
        self.verts, self.tris, self.axis = verts, tris, axis
        zs = [p[axis] for p in verts] or [0]
        self.z0 = min(zs)
        self.dz = max(max(zs) - self.z0, 1e-9) / bins
        self.bins = bins
        self.b = [[] for _ in range(bins + 1)]
        for fi, t in enumerate(tris):
            lo = min(verts[i][axis] for i in t)
            hi = max(verts[i][axis] for i in t)
            for k in range(self._bin(lo), self._bin(hi) + 1):
                self.b[k].append(fi)

    def _bin(self, z):
        return min(self.bins, max(0, int((z - self.z0) / self.dz)))

    def section(self, z):
        """Closed loops of the section at axis = z, as lists of (u, v)
        points with (u, v) = _axes(axis). Solid lies to the left of each
        loop, so outer boundaries have positive area and holes negative
        (for a consistently oriented mesh). Unclosed chains (an open mesh)
        are returned separately."""
        z += PLANE_JITTER
        a = self.axis
        u, v = _axes(a)
        verts, tris = self.verts, self.tris
        nxt = {}
        pts = {}
        for fi in self.b[self._bin(z)]:
            t = tris[fi]
            d = [verts[i][a] - z for i in t]
            if (d[0] > 0) == (d[1] > 0) == (d[2] > 0):
                continue
            crossings = []
            for k in range(3):
                i, j = t[k], t[(k + 1) % 3]
                di, dj = d[k], d[(k + 1) % 3]
                if (di > 0) != (dj > 0):
                    key = (i, j) if i < j else (j, i)
                    if key not in pts:
                        p, q = verts[key[0]], verts[key[1]]
                        dp, dq = p[a] - z, q[a] - z
                        s = dp / (dp - dq)
                        pts[key] = (p[u] + s * (q[u] - p[u]), p[v] + s * (q[v] - p[v]))
                    # Going around the triangle, the edge where it goes
                    # from below the plane to above ends the segment.
                    crossings.append((key, di <= 0))
            if len(crossings) != 2:
                continue
            (k1, up1), (k2, up2) = crossings
            # The segment runs from the edge crossed going down to the edge
            # crossed going up, which keeps the solid on its left.
            if up1:
                nxt[k2] = k1
            else:
                nxt[k1] = k2
        loops, chains = [], []
        seen = set()
        for start in list(nxt):
            if start in seen:
                continue
            path = [start]
            seen.add(start)
            cur = nxt.get(start)
            closed = False
            while cur is not None:
                if cur == start:
                    closed = True
                    break
                if cur in seen:
                    break
                seen.add(cur)
                path.append(cur)
                cur = nxt.get(cur)
            poly = [pts[k] for k in path]
            (loops if closed and len(poly) >= 3 else chains).append(poly)
        return loops, chains


def poly_area(p):
    s = 0.0
    for i in range(len(p)):
        x1, y1 = p[i]
        x2, y2 = p[(i + 1) % len(p)]
        s += x1 * y2 - x2 * y1
    return s / 2


def poly_centroid(p):
    a = poly_area(p)
    if a == 0:
        return (sum(q[0] for q in p) / len(p), sum(q[1] for q in p) / len(p))
    cx = cy = 0.0
    for i in range(len(p)):
        x1, y1 = p[i]
        x2, y2 = p[(i + 1) % len(p)]
        c = x1 * y2 - x2 * y1
        cx += (x1 + x2) * c
        cy += (y1 + y2) * c
    return (cx / (6 * a), cy / (6 * a))


def poly_bbox(p):
    xs = [q[0] for q in p]
    ys = [q[1] for q in p]
    return (min(xs), min(ys), max(xs), max(ys))


def point_in_poly(pt, p):
    x, y = pt
    inside = False
    j = len(p) - 1
    for i in range(len(p)):
        xi, yi = p[i]
        xj, yj = p[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def dist_point_poly(pt, p):
    best = float("inf")
    x, y = pt
    for i in range(len(p)):
        x1, y1 = p[i]
        x2, y2 = p[(i + 1) % len(p)]
        dx, dy = x2 - x1, y2 - y1
        L = dx * dx + dy * dy
        s = 0 if L == 0 else max(0, min(1, ((x - x1) * dx + (y - y1) * dy) / L))
        px, py = x1 + s * dx - x, y1 + s * dy - y
        best = min(best, math.hypot(px, py))
    return best


def ray2d_first_hit(origin, direction, loops):
    """Distance along a 2D ray to the first loop edge it crosses."""
    ox, oy = origin
    dx, dy = direction
    best = None
    for p in loops:
        for i in range(len(p)):
            x1, y1 = p[i]
            x2, y2 = p[(i + 1) % len(p)]
            ex, ey = x2 - x1, y2 - y1
            den = dx * ey - dy * ex
            if den == 0:
                continue
            t = ((x1 - ox) * ey - (y1 - oy) * ex) / den
            s = ((x1 - ox) * dy - (y1 - oy) * dx) / den
            if t >= 0 and 0 <= s <= 1 and (best is None or t < best):
                best = t
    return best


def width_range(p, steps=72):
    """(min, max) caliper width of a polygon over directions 0..180 deg."""
    ws = []
    for k in range(steps):
        th = math.pi * k / steps
        c, s = math.cos(th), math.sin(th)
        proj = [x * c + y * s for x, y in p]
        ws.append(max(proj) - min(proj))
    return min(ws), max(ws)


def equiv_diameter(p):
    return 2 * math.sqrt(abs(poly_area(p)) / math.pi)


def median(xs):
    xs = sorted(xs)
    if not xs:
        return None
    m = len(xs) // 2
    return xs[m] if len(xs) % 2 else (xs[m - 1] + xs[m]) / 2
