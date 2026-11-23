"""A small PlayStation-1-style renderer for the Boxen app icon.

Low-poly meshes, per-vertex lighting, vertices snapped to whole pixels, affine Gouraud shading,
15-bit colour with 4x4 Bayer dithering, glow as stepped bands, nearest-neighbour upscale.
Pure Python plus Pillow. `render` returns a transparent RGBA image of the chosen materials;
materials left out still occlude, so separate layers line up exactly."""
import math
from PIL import Image

RES = 192
BAYER = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]]


# ------------------------------------------------------------------ vectors
def add(a, b): return (a[0] + b[0], a[1] + b[1], a[2] + b[2])
def sub(a, b): return (a[0] - b[0], a[1] - b[1], a[2] - b[2])
def mul(a, k): return (a[0] * k, a[1] * k, a[2] * k)
def dot(a, b): return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
def cross(a, b): return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])
def norm(a):
    l = math.sqrt(dot(a, a)) or 1
    return (a[0] / l, a[1] / l, a[2] / l)


def rot(v, yaw=0, pitch=0, roll=0):
    x, y, z = v
    c, s = math.cos(roll), math.sin(roll)
    x, y = x * c - y * s, x * s + y * c
    c, s = math.cos(yaw), math.sin(yaw)
    x, z = x * c + z * s, -x * s + z * c
    c, s = math.cos(pitch), math.sin(pitch)
    y, z = y * c - z * s, y * s + z * c
    return (x, y, z)


# ------------------------------------------------------------------ meshes
class Mesh:
    def __init__(self):
        self.tris = []  # (v0, v1, v2, material)

    def quad(self, a, b, c, d, m):
        self.tris.append((a, b, c, m))
        self.tris.append((a, c, d, m))

    def extend(self, other):
        self.tris += other.tris
        return self

    def transform(self, f):
        out = Mesh()
        out.tris = [(f(a), f(b), f(c), m) for a, b, c, m in self.tris]
        return out


def box(cx, cy, cz, w, h, d, m, sub_div=1):
    """Axis-aligned box, faces subdivided so vertex lighting can pool on them."""
    mesh = Mesh()
    x0, x1, y0, y1, z0, z1 = cx - w / 2, cx + w / 2, cy - h / 2, cy + h / 2, cz - d / 2, cz + d / 2
    faces = [
        ((x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1)),  # front (+z, toward camera)
        ((x1, y0, z0), (x0, y0, z0), (x0, y1, z0), (x1, y1, z0)),  # back
        ((x1, y0, z1), (x1, y0, z0), (x1, y1, z0), (x1, y1, z1)),  # right
        ((x0, y0, z0), (x0, y0, z1), (x0, y1, z1), (x0, y1, z0)),  # left
        ((x0, y1, z1), (x1, y1, z1), (x1, y1, z0), (x0, y1, z0)),  # top
        ((x0, y0, z0), (x1, y0, z0), (x1, y0, z1), (x0, y0, z1)),  # bottom
    ]
    for a, b, c, dd in faces:
        n = sub_div
        for i in range(n):
            for j in range(n):
                def p(u, v):
                    top = add(mul(a, 1 - u), mul(b, u))
                    bot = add(mul(dd, 1 - u), mul(c, u))
                    return add(mul(top, 1 - v), mul(bot, v))
                u0, u1, v0, v1 = i / n, (i + 1) / n, j / n, (j + 1) / n
                mesh.quad(p(u0, v0), p(u1, v0), p(u1, v1), p(u0, v1), m)
    return mesh


def plane(y, size, n, m):
    mesh = Mesh()
    step = size * 2 / n
    for i in range(n):
        for j in range(n):
            x0, z0 = -size + i * step, -size + j * step
            mesh.quad((x0, y, z0 + step), (x0 + step, y, z0 + step), (x0 + step, y, z0), (x0, y, z0), m)
    return mesh


def icosphere(cx, cy, cz, r, m):
    t = (1 + 5 ** 0.5) / 2
    vs = [norm(v) for v in [(-1, t, 0), (1, t, 0), (-1, -t, 0), (1, -t, 0), (0, -1, t), (0, 1, t), (0, -1, -t),
                            (0, 1, -t), (t, 0, -1), (t, 0, 1), (-t, 0, -1), (-t, 0, 1)]]
    fs = [(0, 11, 5), (0, 5, 1), (0, 1, 7), (0, 7, 10), (0, 10, 11), (1, 5, 9), (5, 11, 4), (11, 10, 2), (10, 7, 6),
          (7, 1, 8), (3, 9, 4), (3, 4, 2), (3, 2, 6), (3, 6, 8), (3, 8, 9), (4, 9, 5), (2, 4, 11), (6, 2, 10),
          (8, 6, 7), (9, 8, 1)]
    mesh = Mesh()
    for a, b, c in fs:
        mesh.tris.append((add(mul(vs[a], r), (cx, cy, cz)), add(mul(vs[b], r), (cx, cy, cz)),
                          add(mul(vs[c], r), (cx, cy, cz)), m))
    return mesh


def octahedron(cx, cy, cz, rx, ry, m):
    pts = [(rx, 0, 0), (-rx, 0, 0), (0, ry, 0), (0, -ry, 0), (0, 0, rx), (0, 0, -rx)]
    fs = [(2, 4, 0), (2, 1, 4), (2, 5, 1), (2, 0, 5), (3, 0, 4), (3, 4, 1), (3, 1, 5), (3, 5, 0)]
    mesh = Mesh()
    for a, b, c in fs:
        mesh.tris.append((add(pts[a], (cx, cy, cz)), add(pts[b], (cx, cy, cz)), add(pts[c], (cx, cy, cz)), m))
    return mesh


def prism(cx, cy, cz, r, h, sides, m, r_top=None):
    r_top = r if r_top is None else r_top
    mesh = Mesh()
    y0, y1 = cy - h / 2, cy + h / 2
    ring = [(math.cos(2 * math.pi * i / sides), math.sin(2 * math.pi * i / sides)) for i in range(sides)]
    for i in range(sides):
        (c0, s0), (c1, s1) = ring[i], ring[(i + 1) % sides]
        a, b = (cx + c0 * r, y0, cz + s0 * r), (cx + c1 * r, y0, cz + s1 * r)
        c, d = (cx + c1 * r_top, y1, cz + s1 * r_top), (cx + c0 * r_top, y1, cz + s0 * r_top)
        mesh.quad(a, b, c, d, m)
        mesh.tris.append(((cx, y1, cz), d, c, m))
    return mesh


def square_ring(outer, inner, depth, m):
    """A square frame in the XY plane (rotate it 45° for a diamond seal), bevelled faces."""
    mesh = Mesh()
    o, i_, d = outer / 2, inner / 2, depth / 2
    corners_o = [(-o, -o), (o, -o), (o, o), (-o, o)]
    corners_i = [(-i_, -i_), (i_, -i_), (i_, i_), (-i_, i_)]
    for k in range(4):
        (ox0, oy0), (ox1, oy1) = corners_o[k], corners_o[(k + 1) % 4]
        (ix0, iy0), (ix1, iy1) = corners_i[k], corners_i[(k + 1) % 4]
        mesh.quad((ox0, oy0, d), (ox1, oy1, d), (ix1, iy1, d), (ix0, iy0, d), m)       # front
        mesh.quad((ix0, iy0, -d), (ix1, iy1, -d), (ox1, oy1, -d), (ox0, oy0, -d), m)   # back
        mesh.quad((ox1, oy1, d), (ox0, oy0, d), (ox0, oy0, -d), (ox1, oy1, -d), m)     # outer wall
        mesh.quad((ix0, iy0, d), (ix1, iy1, d), (ix1, iy1, -d), (ix0, iy0, -d), m)     # inner wall
    return mesh


# ------------------------------------------------------------------ lighting
class Scene:
    def __init__(self, mesh, materials, lights, camera, fog=None, glow=None):
        self.mesh, self.materials, self.lights, self.camera = mesh, materials, lights, camera
        self.fog, self.glow = fog, glow or []


def shade(p, n, mat, lights, eye):
    """Per-vertex colour: ambient + directional Lambert + point lights + a little Blinn sheen."""
    if mat.get("emit"):
        return mat["emit"]
    base = mat["color"]
    col = [base[k] * mat.get("ambient", 0.18) for k in range(3)]
    view = norm(sub(eye, p))
    for L in lights:
        if L["type"] == "dir":
            ld = norm(mul(L["dir"], -1))
            atten = 1.0
        else:
            to = sub(L["pos"], p)
            dist = math.sqrt(dot(to, to))
            ld = mul(to, 1 / (dist or 1))
            atten = L["power"] / (1 + L.get("falloff", 1.0) * dist * dist)
        lam = max(0.0, dot(n, ld))
        h = norm(add(ld, view))
        spec = max(0.0, dot(n, h)) ** mat.get("shine", 24) * mat.get("spec", 0.4)
        for k in range(3):
            col[k] += (base[k] * lam + spec * 255) * L["color"][k] / 255 * atten
    return tuple(min(255.0, c) for c in col)


# ------------------------------------------------------------------ raster
def render(scene, res=RES, out=1024, keep=None, with_glow=True):
    """Render the scene; `keep` limits output to those materials (others still occlude), `with_glow` adds the falloff."""
    cam = scene.camera
    eye = (0, 0, cam["dist"])
    zbuf = [[1e9] * res for _ in range(res)]
    rgb = [[None] * res for _ in range(res)]
    emis = [[0.0] * res for _ in range(res)]
    mats = [[None] * res for _ in range(res)]

    def view(p):
        q = rot(p, cam.get("yaw", 0), cam.get("pitch", 0), cam.get("roll", 0))
        return add(q, cam.get("shift", (0, 0, 0)))

    def project(q):
        z = cam["dist"] - q[2]
        f = cam["focal"] / z
        return (res / 2 + q[0] * f * res / 2, res / 2 - q[1] * f * res / 2, z)

    for a, b, c, mname in scene.mesh.tris:
        mat = scene.materials[mname]
        va, vb, vc = view(a), view(b), view(c)
        n = norm(cross(sub(vb, va), sub(vc, va)))
        if dot(n, sub(eye, va)) <= 0 and not mat.get("double"):
            continue
        cols = [shade(v, n, mat, scene.lights, eye) for v in (va, vb, vc)]
        if scene.fog:
            fc, near, far = scene.fog
            cols2 = []
            for v, col in zip((va, vb, vc), cols):
                t = min(1, max(0, (cam["dist"] - v[2] - near) / (far - near)))
                cols2.append(tuple(col[k] * (1 - t) + fc[k] * t for k in range(3)) + (1 - t * 0.85,))
            cols = cols2
        else:
            cols = [col + (1.0,) for col in cols]
        pa, pb, pc = project(va), project(vb), project(vc)
        # PS1: vertices snap to whole pixels
        pts = [(round(p[0]), round(p[1]), p[2]) for p in (pa, pb, pc)]
        (x0, y0, z0), (x1, y1, z1), (x2, y2, z2) = pts
        area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0)
        if area == 0:
            continue
        minx, maxx = max(0, min(x0, x1, x2)), min(res - 1, max(x0, x1, x2))
        miny, maxy = max(0, min(y0, y1, y2)), min(res - 1, max(y0, y1, y2))
        em = 1.0 if mat.get("emit") else 0.0
        for y in range(miny, maxy + 1):
            for x in range(minx, maxx + 1):
                px, py = x + 0.5, y + 0.5
                w0 = ((x1 - px) * (y2 - py) - (x2 - px) * (y1 - py)) / area
                w1 = ((x2 - px) * (y0 - py) - (x0 - px) * (y2 - py)) / area
                w2 = 1 - w0 - w1
                if w0 < 0 or w1 < 0 or w2 < 0:
                    continue
                z = w0 * z0 + w1 * z1 + w2 * z2
                if z >= zbuf[y][x]:
                    continue
                zbuf[y][x] = z
                rgb[y][x] = tuple(w0 * cols[0][k] + w1 * cols[1][k] + w2 * cols[2][k] for k in range(4))
                emis[y][x] = em
                mats[y][x] = mname

    if keep is not None:
        for y in range(res):
            for x in range(res):
                if mats[y][x] not in keep:
                    rgb[y][x] = None

    # glow: stepped, dithered bands around emissive pixels (no blur, PS1-style additive sprite falloff)
    glow = [[0.0] * res for _ in range(res)]
    for gcol, radius, strength in (scene.glow if with_glow else []):
        src = [(x, y) for y in range(res) for x in range(res) if emis[y][x] > 0]
        if not src:
            continue
        dist = [[1e9] * res for _ in range(res)]
        for (x, y) in src:
            dist[y][x] = 0
        # chamfer distance transform
        for y in range(res):
            for x in range(res):
                d = dist[y][x]
                if x > 0: d = min(d, dist[y][x - 1] + 1)
                if y > 0: d = min(d, dist[y - 1][x] + 1)
                if x > 0 and y > 0: d = min(d, dist[y - 1][x - 1] + 1.414)
                if x < res - 1 and y > 0: d = min(d, dist[y - 1][x + 1] + 1.414)
                dist[y][x] = d
        for y in range(res - 1, -1, -1):
            for x in range(res - 1, -1, -1):
                d = dist[y][x]
                if x < res - 1: d = min(d, dist[y][x + 1] + 1)
                if y < res - 1: d = min(d, dist[y + 1][x] + 1)
                if x < res - 1 and y < res - 1: d = min(d, dist[y + 1][x + 1] + 1.414)
                if x > 0 and y < res - 1: d = min(d, dist[y + 1][x - 1] + 1.414)
                dist[y][x] = d
        for y in range(res):
            for x in range(res):
                d = dist[y][x]
                if 0 < d <= radius:
                    band = math.ceil((1 - d / radius) * 4) / 4  # four stepped bands
                    a = band * strength
                    if a > glow[y][x]:
                        glow[y][x] = a
        glow_col = gcol

    img = Image.new("RGBA", (res, res), (0, 0, 0, 0))
    px = img.load()
    for y in range(res):
        for x in range(res):
            c = rgb[y][x]
            g = glow[y][x] if (scene.glow and with_glow) else 0
            thr = (BAYER[y % 4][x % 4] + 0.5) / 16 - 0.5
            if c is None and g <= 0:
                continue
            if c is None:
                r_, g_, b_, a_ = glow_col[0], glow_col[1], glow_col[2], g
            else:
                r_, g_, b_, a_ = c
                if g > 0 and emis[y][x] == 0:
                    r_ = r_ + glow_col[0] * g
                    g_ = g_ + glow_col[1] * g
                    b_ = b_ + glow_col[2] * g
            # 15-bit colour (5 bits per channel) with ordered dithering
            q = []
            for v in (r_, g_, b_):
                v = min(255, max(0, v + thr * 8))
                q.append(min(255, int(round(v / 8)) * 8))
            alpha = min(255, max(0, int(round((a_ + thr * 0.12) * 255))))
            if c is None:
                alpha = (alpha // 32) * 32
            px[x, y] = (q[0], q[1], q[2], alpha)
    return img.resize((out, out), Image.NEAREST)
