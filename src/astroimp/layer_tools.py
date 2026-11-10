"""Layer tonal tools shared by analysis, contact sheets and tiled native exports."""
from __future__ import annotations

import math
import numpy as np


def curve_lut(points=()):
    p = sorted(((min(1., max(0., float(v["x"]))), min(1., max(0., float(v["y"]))))
                for v in points if math.isfinite(v["x"]) and math.isfinite(v["y"])), key=lambda v: v[0])
    unique = [v for i, v in enumerate(p) if not i or v[0] - p[i-1][0] > 1e-6]
    if not unique or unique[0][0] != 0:
        unique.insert(0, (0., 0.))
    if unique[-1][0] != 1:
        unique.append((1., 1.))
    p = np.asarray(unique, np.float64)
    h = np.diff(p[:, 0])
    d = np.diff(p[:, 1]) / h
    m = np.r_[d[0], (d[:-1] + d[1:]) / 2, d[-1]]
    for i in range(1, len(p)-1):
        if d[i-1] * d[i] <= 0:
            m[i] = 0
    for i in range(len(d)):
        if d[i] == 0:
            m[i:i+2] = 0
            continue
        a, b = m[i] / d[i], m[i+1] / d[i]
        r = math.hypot(a, b)
        if r > 3:
            m[i], m[i+1] = 3*a/r*d[i], 3*b/r*d[i]
    x = np.arange(256, dtype=np.float64) / 255
    j = np.clip(np.searchsorted(p[:, 0], x, side="left")-1, 0, len(p)-2)
    t = (x-p[j, 0]) / h[j]
    y = ((2*t**3-3*t**2+1)*p[j, 1] + (t**3-2*t**2+t)*h[j]*m[j]
         + (-2*t**3+3*t**2)*p[j+1, 1] + (t**3-t**2)*h[j]*m[j+1])
    return np.clip(y, 0, 1).astype(np.float32)


def curve_enabled(points):
    return any(p["x"] != p["y"] for p in points or ())


def apply_curve(value, points):
    lut = curve_lut(points)
    x = np.clip(value, 0, 1) * np.float32(255)
    lo = np.floor(x).astype(np.int32)
    f = x - lo
    return lut[lo] * (1-f) + lut[np.minimum(lo+1, 255)] * f


def clip_coverage(x, y, clip, width, height):
    """Canvas-space crop, unaffected by layer transforms; inward feather = fraction of short side."""
    sx, sy = clip["w"] * width, clip["h"] * height
    qx, qy = (x - clip["x"]) / max(clip["w"], 1e-6), (y - clip["y"]) / max(clip["h"], 1e-6)
    short = min(sx, sy)
    if clip.get("shape", "rect") == "ellipse":
        distance = (1 - np.sqrt(((qx-.5)*2)**2 + ((qy-.5)*2)**2)) * short * .5
    else:
        distance = np.minimum(np.minimum(qx*sx, (1-qx)*sx), np.minimum(qy*sy, (1-qy)*sy))
    feather = clip.get("feather", 0) * short
    if feather <= 0:
        return (distance >= 0).astype(np.float32)
    t = np.clip(distance / feather, 0, 1)
    return (t*t*(3-2*t)).astype(np.float32)


def canvas_coordinates(project):
    w, h = project.meta["width"], project.meta["height"]
    if hasattr(project, "detail_scale"):
        return project.xx / (w*project.detail_scale), project.yy / (h*project.detail_scale)
    yy, xx = np.mgrid[:project.shape[0], :project.shape[1]].astype(np.float32)
    if project.window:
        xx += project.window[0] + .5
        yy += project.window[1] + .5
    else:
        xx, yy = (xx+.5)*project.step, (yy+.5)*project.step
    return xx / w, yy / h
