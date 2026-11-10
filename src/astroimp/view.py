"""Canvas geometry shared by exports and contact sheets; mirrored in editor/view.ts.

Forward order: image-relative X flip, anchor rotation, crop, delta rotation about
the crop centre. Frame coordinates live in current-rotation output space, so
sampling still uses the legacy inverse rotation followed by source-X flip.
"""
import math

import numpy as np

FULL_CROP = {"x": 0, "y": 0, "w": 1, "h": 1}


def crop_rotation(g):
    crop = g.get("crop") or FULL_CROP
    return (g.get("rotation", 0) if all(crop[k] == v for k, v in FULL_CROP.items())
            else crop.get("rotation", g.get("rotation", 0)))


def view_frame(meta, g):
    crop = g.get("crop") or FULL_CROP
    w, h = meta["width"], meta["height"]
    delta = g.get("rotation", 0) - crop_rotation(g)
    if not delta:
        return {**crop, "width": w * crop["w"], "height": h * crop["h"]}
    angle = math.radians(delta)
    c, s = math.cos(angle), math.sin(angle)
    abs_c, abs_s = (0 if abs(v) < 1e-12 else abs(v) for v in (c, s))
    width = w * crop["w"] * abs_c + h * crop["h"] * abs_s
    height = w * crop["w"] * abs_s + h * crop["h"] * abs_c
    cx, cy = (crop["x"] + crop["w"] / 2 - .5) * w, (crop["y"] + crop["h"] / 2 - .5) * h
    return {"x": .5 + (c * cx - s * cy - width / 2) / w,
            "y": .5 + (s * cx + c * cy - height / 2) / h,
            "w": width / w, "h": height / h, "width": width, "height": height}


def inside_crop(meta, g, x, y):
    delta = g.get("rotation", 0) - crop_rotation(g)
    if not delta:
        return np.ones(np.broadcast_shapes(np.shape(x), np.shape(y)), dtype=bool)
    crop = g["crop"]
    w, h = meta["width"], meta["height"]
    angle = math.radians(delta)
    c, s = math.cos(angle), math.sin(angle)
    px, py = (x - .5) * w, (y - .5) * h
    qx, qy = (c * px + s * py) / w + .5, (-s * px + c * py) / h + .5
    return ((qx >= crop["x"] - 1e-12) & (qx <= crop["x"] + crop["w"] + 1e-12)
            & (qy >= crop["y"] - 1e-12) & (qy <= crop["y"] + crop["h"] + 1e-12))
