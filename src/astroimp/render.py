"""CPU mirror of the React editor's WebGL compositor, for analysis and agent-driven edits.

Fidelity notes: crop/rotation of the canvas are
ignored for statistics. Gradients use the same 256-entry 8-bit lookup as the GPU;
edge fades share its uint32 noise hash and full-resolution image-pixel coordinates.
"""
from __future__ import annotations

import json
import math
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage as ndi

from astroimp.build import ensure_edge_map
from astroimp.planes import load_plane
from astroimp.edits import normalise
from astroimp.probe_cache import brush_coverage
from astroimp.layer_tools import apply_curve, canvas_coordinates, clip_coverage, curve_enabled

BLENDS = ["normal", "add", "screen", "multiply", "overlay", "softlight", "lighten", "difference", "subtract", "darken"]


def log_position(nm: float | None) -> float:
    return max(0.0, min(1.0, math.log((nm or 550) / 200) / math.log(125)))


def hex_rgb(value: str) -> np.ndarray:
    return np.array([int(value[i:i + 2], 16) / 255 for i in (1, 3, 5)], np.float32)


def gradient_lut(stops: list[dict]) -> np.ndarray:
    """Same 8-bit lookup as gradientBytes() in gl.js."""
    stops = sorted(stops, key=lambda s: s["t"])
    t = np.arange(256) / 255
    xs = [s["t"] for s in stops]
    lut = np.stack([np.interp(t, xs, [hex_rgb(s["color"])[c] for s in stops]) for c in range(3)], 1)
    return np.round(lut * 255) / 255


def sample_lut(lut: np.ndarray, v: np.ndarray) -> np.ndarray:
    x = np.clip(v, 0, 1) * 255
    lo = np.floor(x).astype(int)
    hi = np.minimum(lo + 1, 255)
    f = (x - lo)[..., None]
    return lut[lo] * (1 - f) + lut[hi] * f


class Project:
    def __init__(self, folder: Path, max_side: int = 900, window: tuple[int, int, int, int] | None = None):
        """window=(x0, y0, x1, y1) in full-resolution pixels loads just that region at full resolution."""
        self.folder = Path(folder)
        self.meta = json.loads((self.folder / "meta.json").read_text(encoding="utf-8"))
        w, h = self.meta["width"], self.meta["height"]
        self.window = window
        self.step = 1 if window else max(1, math.ceil(max(w, h) / max_side))
        self.shape = (window[3] - window[1], window[2] - window[0]) if window else (h // self.step, w // self.step)
        self.keys = [layer["key"] for layer in self.meta["layers"]]
        self.edges: dict[str, np.ndarray] = {}
        self.full = [self._load(layer["file"]) for layer in self.meta["layers"]]
        lum = self.meta.get("luminance")
        self.lum = self._load(lum["file"]) if lum else None

    def _load(self, name: str) -> np.ndarray:
        h, w = self.meta["height"], self.meta["width"]
        return load_plane(self.folder / name, (h, w), window=self.window, step=self.step)

    def edge(self, key: str) -> np.ndarray:
        if key not in self.edges:
            layer = next((l for l in self.meta["layers"] if l["key"] == key), None)
            path = ensure_edge_map(self.folder, self.meta, layer)
            self.edges[key] = np.fromfile(path, dtype="<f2").reshape(self.meta["height"], self.meta["width"])
        return self.edges[key]


def edge_noise(x: np.ndarray, y: np.ndarray) -> np.ndarray:
    """Three-octave value noise; the uint32 lattice hash is identical to compositor.ts."""
    def value(x, y):
        ix, iy = np.floor(x), np.floor(y)
        fx, fy = x - ix, y - iy
        fx, fy = fx * fx * (3 - 2 * fx), fy * fy * (3 - 2 * fy)

        def hashed(dx, dy):
            h = ((ix + dx).astype(np.uint32) * np.uint32(374761393)
                 + (iy + dy).astype(np.uint32) * np.uint32(668265263))
            h = (h ^ (h >> np.uint32(13))) * np.uint32(1274126177)
            h ^= h >> np.uint32(16)
            return (h & np.uint32(65535)).astype(np.float32) / np.float32(65535)

        a, b, c, d = hashed(0, 0), hashed(1, 0), hashed(0, 1), hashed(1, 1)
        low, high = a + (b - a) * fx, c + (d - c) * fx
        return low + (high - low) * fy

    return (value(x, y) * 4 + value(x * 2, y * 2) * 2 + value(x * 4, y * 4)) / np.float32(7)


def brush_maps(width: int, height: int, strokes: list[dict]) -> np.ndarray:
    """Visibility and source-signal affine maps, with max-combined dabs per stroke."""
    scale = max(1, math.ceil(max(width, height) / 1024))
    mw, mh = math.ceil(width / scale), math.ceil(height / scale)
    sx, sy = width / mw, height / mh
    maps = np.zeros((3, mh, mw), np.float32)
    maps[:2] = 1
    mask, gain, bias = maps
    for stroke in strokes:
        mode = stroke["mode"]
        if mode not in ("hide", "reveal", "black", "white"):
            raise ValueError(f"Unsupported brush stroke mode: {mode}")
        points = stroke["points"]
        if not points:
            continue
        radius = max(.5, stroke["size"] / 2)
        spacing = max(1, stroke["size"] * .15)
        hardness = np.clip(stroke["hardness"], 0, 1)
        strength = np.clip(stroke["strength"], 0, 1)
        dabs = [points[0]]
        for a, b in zip(points, points[1:]):
            steps = max(1, math.ceil(math.hypot(b[0] - a[0], b[1] - a[1]) / spacing))
            dabs.extend((a[0] + (b[0] - a[0]) * j / steps, a[1] + (b[1] - a[1]) * j / steps)
                        for j in range(1, steps + 1))
        footprint = np.zeros_like(mask)
        for x, y in dabs:
            x0, x1 = max(0, math.ceil((x - radius) / sx - .5)), min(mw - 1, math.floor((x + radius) / sx - .5))
            y0, y1 = max(0, math.ceil((y - radius) / sy - .5)), min(mh - 1, math.floor((y + radius) / sy - .5))
            if x1 < x0 or y1 < y0:
                continue
            yy, xx = np.mgrid[y0:y1 + 1, x0:x1 + 1]
            r = np.hypot((xx + .5) * sx - x, (yy + .5) * sy - y) / radius
            t = np.clip((r - hardness) / max(1e-12, 1 - hardness), 0, 1)
            f = np.where(r <= hardness, 1, np.where(r >= 1, 0, 1 - t * t * (3 - 2 * t))) * strength
            region = footprint[y0:y1 + 1, x0:x1 + 1]
            np.maximum(region, f.astype(np.float32), out=region)
        if mode == "hide":
            mask *= 1 - footprint
        elif mode == "reveal":
            mask += (1 - mask) * footprint
        else:
            gain *= 1 - footprint
            bias *= 1 - footprint
            if mode == "white":
                bias += footprint
    return np.clip(maps, 0, 1)


def brush_mask(width: int, height: int, strokes: list[dict]) -> np.ndarray:
    return brush_maps(width, height, strokes)[0]


def brush_data(project: Project, layer: dict) -> np.ndarray:
    maps = brush_coverage(project.meta["width"], project.meta["height"], layer["brush"]["strokes"], brush_maps)
    # RGBA16F upload on the GPU quantizes before linear texture filtering.
    maps = maps.astype(np.float16).astype(np.float32)
    h, w = project.meta["height"], project.meta["width"]
    yy, xx = np.mgrid[:project.shape[0], :project.shape[1]].astype(np.float32)
    if project.window:
        xx += project.window[0] + .5
        yy += project.window[1] + .5
    else:
        xx, yy = (xx + .5) * project.step, (yy + .5) * project.step
    transform = layer.get("transform") or {}
    px, py = xx - w * (.5 + transform.get("x", 0)), yy - h * (.5 + transform.get("y", 0))
    r = math.radians(transform.get("rotation", 0))
    c, s = np.float32(math.cos(r)), np.float32(math.sin(r))
    scale = max(transform.get("scale", 1), .001)
    qx, qy = (c * px + s * py) / scale + w * .5, (-s * px + c * py) / scale + h * .5
    inside = (qx >= 0) & (qx <= w) & (qy >= 0) & (qy <= h)
    x = np.clip(qx / w * maps.shape[2] - .5, 0, maps.shape[2] - 1)
    y = np.clip(qy / h * maps.shape[1] - .5, 0, maps.shape[1] - 1)
    sampled = [ndi.map_coordinates(m, [y, x], order=1, mode="nearest") for m in maps]
    sampled[1] = np.where(inside, sampled[1], 1)
    sampled[2] = np.where(inside, sampled[2], 0)
    return np.stack(sampled)


def brush_fade(project: Project, layer: dict) -> np.ndarray:
    return brush_data(project, layer)[0]


def edge_fade(project: Project, layer: dict) -> np.ndarray:
    edge = layer.get("edge") or {}
    feather = edge.get("feather", 0)
    if feather <= 0:
        return np.ones(project.shape, np.float32)
    h, w = project.meta["height"], project.meta["width"]
    yy, xx = np.mgrid[:project.shape[0], :project.shape[1]].astype(np.float32)
    if project.window:
        xx += project.window[0] + .5
        yy += project.window[1] + .5
    else:
        xx, yy = (xx + .5) * project.step, (yy + .5) * project.step
    transform = layer.get("transform") or {}
    px, py = xx - w * (.5 + transform.get("x", 0)), yy - h * (.5 + transform.get("y", 0))
    r = math.radians(transform.get("rotation", 0))
    c, s = np.float32(math.cos(r)), np.float32(math.sin(r))
    scale = max(transform.get("scale", 1), .001)
    qx, qy = (c * px + s * py) / scale + w * .5, (-s * px + c * py) / scale + h * .5
    inside = (qx >= 0) & (qx <= w) & (qy >= 0) & (qy <= h)
    x, y = np.clip(qx - .5, 0, w - 1), np.clip(qy - .5, 0, h - 1)
    x0, y0 = np.floor(x).astype(int), np.floor(y).astype(int)
    x1, y1 = np.minimum(x0 + 1, w - 1), np.minimum(y0 + 1, h - 1)
    fx, fy = x - x0, y - y0
    distance = project.edge(layer["source"])
    a, b = distance[y0, x0].astype(np.float32), distance[y0, x1].astype(np.float32)
    c, d = distance[y1, x0].astype(np.float32), distance[y1, x1].astype(np.float32)
    dist = (a + (b - a) * fx) * (1 - fy) + (c + (d - c) * fx) * fy
    n = edge_noise(qx / max(feather, 1), qy / max(feather, 1))
    effective = feather * (1 + edge.get("irregularity", 0) * (n - .5) * np.float32(1.6))
    t = np.clip(dist / effective, 0, 1).astype(np.float32)
    return np.where(inside, t * t * (3 - 2 * t), 0)


def warp(image: np.ndarray, transform: dict, order: int = 1) -> np.ndarray:
    """Per-layer move/scale/rotate exactly as the layer shader maps output to source pixels."""
    x, y, scale, rotation = (transform.get(k, d) for k, d in
                             (("x", 0), ("y", 0), ("scale", 1), ("rotation", 0)))
    if not (x or y or rotation) and scale == 1:
        return image
    h, w = image.shape
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    px, py = (xx + .5) / w - .5 - x, (yy + .5) / h - .5 - y
    c, s = math.cos(math.radians(rotation)), math.sin(math.radians(rotation))
    qx = (c * px * w + s * py * h) / w / max(scale, .001) + .5
    qy = (-s * px * w + c * py * h) / h / max(scale, .001) + .5
    out = ndi.map_coordinates(image, [qy * h - .5, qx * w - .5], order=order, mode="nearest")
    inside = (qx >= 0) & (qx <= 1) & (qy >= 0) & (qy <= 1)
    return np.where(inside, out, 0).astype(np.float32)


def stretch(value: np.ndarray, layer: dict, pixel_scale: float = 1) -> np.ndarray:
    black, white, k = layer["black"], layer["white"], max(.001, layer["k"])
    x = np.maximum(0, (value - black) / max(white - black, 1e-5))
    mode = layer["stretch"]
    if mode == "asinh":
        x = np.arcsinh(k * x) / math.asinh(k)
    elif mode == "log":
        x = np.log1p(k * x) / math.log1p(k)
    elif mode == "gamma":
        x = np.power(x, 1 / max(k, .01))
    detail = layer.get("detail") or {}
    if detail.get("amount", 0) > 0:
        # Post-stretch contrast reveals faint tendrils without bright linear-flux peaks dominating.
        x = np.clip(x, 0, 1)
        sigma = min(64, max(1, detail.get("radius", 8))) * pixel_scale
        x = np.clip(x + detail["amount"] * (x - ndi.gaussian_filter(x, sigma, mode="nearest")), 0, 1)
    if layer.get("invert", False):
        x = 1 - np.clip(x, 0, 1)
    if curve_enabled(layer.get("curve")):
        x = apply_curve(x, layer["curve"])
    x = np.clip(x * layer["intensity"], 0, 1)
    match = layer.get("toneMatch") or {}
    return np.clip(x * match.get("gain", 1) + match.get("offset", 0), 0, 1)


def blend(a: np.ndarray, b: np.ndarray, mode: str) -> np.ndarray:
    if mode == "add":
        return a + b
    if mode == "screen":
        return 1 - (1 - a) * (1 - b)
    if mode == "multiply":
        return a * b
    if mode == "overlay":
        return np.where(a >= .5, 1 - 2 * (1 - a) * (1 - b), 2 * a * b)
    if mode == "softlight":
        return np.where(b >= .5, a + (2 * b - 1) * (np.sqrt(np.maximum(a, 0)) - a), a - (1 - 2 * b) * a * (1 - a))
    if mode == "lighten":
        return np.maximum(a, b)
    if mode == "difference":
        return np.abs(a - b)
    if mode == "subtract":
        return np.maximum(a - b, 0)
    if mode == "darken":
        return np.minimum(a, b)
    return b


def output(color: np.ndarray, global_state: dict) -> np.ndarray:
    o = global_state.get("output") or {}
    col = np.maximum(color, 0) * 2 ** o.get("exposure", 0)
    h = o.get("highlights", 0)
    if h > 0:
        t = 1 - .85 * h
        m = col.max(-1, keepdims=True)
        shoulder = t + (1 - t) * (1 - np.exp(-(m - t) / (1 - t)))
        col = np.where(m > t, col * shoulder / np.maximum(m, 1e-6), col)
    black, white, midtones = (o.get(key, default) for key, default in
                             (("black", 0), ("white", 1), ("midtones", 1)))
    for key, value, low, high in (("black", black, 0, 1), ("white", white, .01, 3),
                                 ("midtones", midtones, .3, 4)):
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not low <= value <= high:
            raise ValueError(f"output {key} must be a finite number in {low}..{high}")
    if black != 0 or white != 1 or midtones != 1:
        col = np.maximum((col - black) / max(white - black, 1e-5), 0)
        if midtones != 1:
            col = col ** midtones
    return np.clip(col, 0, 1)


def render(project: Project, state: dict, *, collect: bool = True) -> tuple[np.ndarray, list[dict]]:
    """Return pre-output accumulation and contributions; collect=False avoids per-layer buffers."""
    state = normalise(state)

    log_wave = [log_position(layer.get("wavelength_nm")) for layer in project.meta["layers"]]

    acc = np.zeros(project.shape + (3,), np.float32)
    contributions = []
    sample_warp = getattr(project, "warp", warp)
    for layer in reversed(state["layers"]):
        if not layer.get("visible", True):
            continue
        transform = layer.get("transform") or {}
        samples = {}
        brush = None
        if (layer.get("brush") or {}).get("strokes"):
            brush = getattr(project, "brush_data", lambda l: brush_data(project, l))(layer)

        def value(key):
            if key not in samples:
                if key in project.keys:
                    index = project.keys.index(key)
                    full = project.full[index]
                elif project.lum is not None:
                    full = project.lum
                else:
                    samples[key] = np.mean([sample_warp(f, transform) for f in project.full], 0)
                    return samples[key]
                samples[key] = sample_warp(full, transform)
            return samples[key]

        def signal(key):
            sampled = value(key)
            if brush is not None:
                return np.nan_to_num(sampled, nan=0, posinf=0, neginf=0) * brush[1] + brush[2]
            return sampled

        pixel_scale = getattr(project, "detail_scale", 1 / getattr(project, "step", 1))
        v = stretch(signal(layer["source"]), layer, pixel_scale)
        bright = v
        if layer["mode"] == "gradient":
            hue = sample_lut(gradient_lut(layer["gradient"]), v)
            col = hue * layer["intensity"]
        elif layer["mode"] == "spectral":
            weights = np.maximum(0, np.stack([value(key) for key in project.keys]))
            total = weights.sum(0)
            t = np.where(total > 1e-6, np.tensordot(log_wave, weights, 1) / np.maximum(total, 1e-6), .5)
            stops = [{"t": log_position(s["nm"]), "color": s["color"]} for s in layer["spectral"]["stops"]]
            hue = sample_lut(gradient_lut(stops), t)
            bright = stretch(signal("lum"), layer, pixel_scale)
            col = hue * bright[..., None]
        else:
            hue = np.broadcast_to(hex_rgb(layer["color"]), project.shape + (3,))
            col = hex_rgb(layer["color"]) * v[..., None]
        alpha = np.full(project.shape, layer["opacity"], np.float32)
        if layer.get("clip"):
            xx, yy = canvas_coordinates(project)
            alpha *= clip_coverage(xx, yy, layer["clip"], project.meta["width"], project.meta["height"])
        if layer.get("invert", False) or curve_enabled(layer.get("curve")):
            coverage_layer = {**layer, "edge": {"feather": 1, "irregularity": 0}}
            covered = getattr(project, "edge_fade", lambda l: edge_fade(project, l))(coverage_layer)
            alpha *= np.maximum(covered, 1 - brush[1]) if brush is not None else covered
        if layer["blend"] == "normal":
            # Brightness doubles as coverage so faint areas let lower layers show through.
            col = hue
            alpha = alpha * bright
        mask = layer.get("mask") or {}
        if mask.get("source"):
            m = np.clip((value(mask["source"]) - mask["black"])
                        / max(1e-5, mask["white"] - mask["black"]), 0, 1)
            alpha *= 1 - m if mask.get("invert") else m
        if (layer.get("edge") or {}).get("feather", 0) > 0:
            covered = getattr(project, "edge_fade", lambda l: edge_fade(project, l))(layer)
            alpha *= np.maximum(covered, 1 - brush[1]) if brush is not None else covered
        if brush is not None:
            alpha *= brush[0]
        before = acc
        acc = before + (blend(before, col, layer["blend"]) - before) * alpha[..., None]
        if collect:
            contributions.append({"layer": layer, "added": acc - before, "self_saturated": v >= .999})
    return acc, contributions


def to_png(rgb: np.ndarray, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    Image.fromarray(np.round(np.clip(rgb, 0, 1) * 255).astype(np.uint8)).save(path)


def probe(folder: Path, state: dict, region: dict) -> dict:
    """Explain the colour of a small region: each layer's added RGB and share of the final light.

    region: {"x", "y", "radius"} (circle) or {"x1", "y1", "x2", "y2"} (rectangle), full-res pixels.
    """
    meta = json.loads((Path(folder) / "meta.json").read_text(encoding="utf-8"))
    w, h = meta["width"], meta["height"]
    if "x1" in region:
        x0, x1 = sorted((int(region["x1"]), int(region["x2"])))
        y0, y1 = sorted((int(region["y1"]), int(region["y2"])))
        x1, y1 = x1 + 1, y1 + 1
    else:
        r = max(0, int(region.get("radius", 4)))
        x0, x1 = int(region["x"]) - r, int(region["x"]) + r + 1
        y0, y1 = int(region["y"]) - r, int(region["y"]) + r + 1
    x0, y0, x1, y1 = max(0, x0), max(0, y0), min(w, x1), min(h, y1)
    if x1 <= x0 or y1 <= y0:
        raise ValueError("Region is outside the image.")
    transformed = any((l.get("transform") or {}) not in ({}, {"x": 0, "y": 0, "scale": 1, "rotation": 0})
                      for l in state["layers"])
    if transformed:
        # Layer transforms sample other pixels, so render everything and cut the window afterwards.
        project = Project(folder, max(w, h))
        cut = (slice(y0, y1), slice(x0, x1))
    else:
        halo = max([0, *[math.ceil(4 * min(64, max(1, (l.get("detail") or {}).get("radius", 8))))
                         for l in state["layers"] if l.get("visible", True)
                         and (l.get("detail") or {}).get("amount", 0) > 0]])
        bounds = (max(0, x0-halo), max(0, y0-halo), min(w, x1+halo), min(h, y1+halo))
        project = Project(folder, window=bounds)
        cut = (slice(y0-bounds[1], y1-bounds[1]), slice(x0-bounds[0], x1-bounds[0]))
    acc, contributions = render(project, state)
    if "radius" in region or "x1" not in region:
        yy, xx = np.mgrid[y0:y1, x0:x1]
        inside = np.hypot(xx - region["x"], yy - region["y"]) <= max(.5, region.get("radius", 4))
    else:
        inside = np.ones((y1 - y0, x1 - x0), bool)
    pick = lambda a: a[cut][inside]
    final = output(acc[cut], state["global"])[inside].mean(0)
    filters = {layer["key"]: layer["filter"] for layer in meta["layers"]}
    total = sum(float(np.clip(pick(c["added"]), 0, None).sum()) for c in contributions) or 1
    layers = []
    for c in contributions:
        added = pick(c["added"]).mean(0)
        layers.append({"id": c["layer"]["id"], "name": c["layer"]["name"], "source": filters.get(c["layer"]["source"], "luminance"),
                       "blend": c["layer"]["blend"], "color": c["layer"].get("color"),
                       "added_rgb": [round(float(v), 3) for v in added],
                       "share": round(float(np.clip(pick(c["added"]), 0, None).sum()) / total, 3)})
    layers.sort(key=lambda entry: -entry["share"])
    return {"region": {"x0": x0, "y0": y0, "x1": x1 - 1, "y1": y1 - 1, "pixels": int(inside.sum())},
            "final_rgb": [round(float(v), 3) for v in final],
            "final_hex": "#" + "".join(f"{round(float(v) * 255):02x}" for v in final),
            "pre_output_peak": round(float(acc[cut][inside].max(-1).mean()), 3),
            "layers": layers}


def analyze(folder: Path, state: dict, out_dir: Path, max_side: int = 900) -> dict:
    project = Project(folder, max_side)
    filters = {layer["key"]: layer["filter"] for layer in project.meta["layers"]}
    acc, contributions = render(project, state)
    final = output(acc, state["global"])
    exposure = 2 ** ((state["global"].get("output") or {}).get("exposure", 0))
    peak = (acc * exposure).max(-1)
    clipped = peak >= 1
    covered = acc.max(-1) > 1e-4
    to_png(final, out_dir / "preview.png")
    gray = final.mean(-1, keepdims=True) * .6
    to_png(np.where(clipped[..., None], np.array([1, .15, .1]), gray.repeat(3, -1)), out_dir / "clipping.png")
    layers = []
    total_in_clipped = sum(float(c["added"].max(-1)[clipped].clip(0).sum()) for c in contributions) or 1
    for c in contributions:
        layer, added = c["layer"], c["added"].max(-1)
        layers.append({
            "id": layer["id"], "name": layer["name"],
            "source": filters.get(layer["source"], "luminance"),
            "mode": layer["mode"], "blend": layer["blend"],
            "intensity": layer["intensity"], "white": layer["white"], "black": layer["black"],
            "share_of_clipped_light": round(float(added[clipped].clip(0).sum()) / total_in_clipped, 3),
            "mean_added_in_clipped": round(float(added[clipped].mean()) if clipped.any() else 0, 3),
            "self_saturated_fraction": round(float(c["self_saturated"][covered].mean()) if covered.any() else 0, 3),
        })
    layers.sort(key=lambda entry: -entry["share_of_clipped_light"])
    return {
        "resolution": list(project.shape[::-1]),
        "clipped_fraction": round(float(clipped[covered].mean()) if covered.any() else 0, 4),
        "peak_brightness_p99": round(float(np.percentile(peak[covered], 99)) if covered.any() else 0, 3),
        "highlight_rolloff": (state["global"].get("output") or {}).get("highlights", 0),
        "layers": layers,
        "preview": str(out_dir / "preview.png"),
        "clipping_map": str(out_dir / "clipping.png"),
    }
