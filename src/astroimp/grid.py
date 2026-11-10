"""Persistent build canvases and sky-coordinate migration of editor geometry."""
from __future__ import annotations

from copy import deepcopy
from datetime import datetime, timezone
import json
from pathlib import Path
import uuid

import numpy as np
from astropy.wcs import WCS
from astroimp.view import FULL_CROP, crop_rotation


def read(path: Path, default=None):
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else default


def grid_from_meta(meta: dict) -> dict:
    return {key: deepcopy(meta[key]) for key in ("width", "height", "wcs", "pixel_scale_arcsec", "region_sky")
            if key in meta}


def locked_grid(session: Path) -> dict | None:
    if capture_rotation_changed(session):
        return None
    grid = read(session / "grid.json")
    if grid:
        return grid_from_meta(grid)
    meta = read(session / "project" / "meta.json")
    if meta and (session / "edits.json").exists():
        return grid_from_meta(meta)
    return None


def capture_rotation_changed(session: Path, meta: dict | None = None) -> bool:
    """A manual capture-angle change replaces, rather than reuses, the old canvas."""
    meta = meta if meta is not None else read(session / "project" / "meta.json")
    return bool(meta and float(meta.get("view_rotation_deg", 0)) !=
                float(read(session / "view.json", {}).get("rotation_deg", 0)))


def same_grid(old: dict, new: dict) -> bool:
    return all(old.get(k) == new.get(k) for k in ("width", "height", "wcs"))


class GridMap:
    def __init__(self, old: dict, new: dict):
        self.old, self.new = old, new
        self.source, self.target = WCS(old["wcs"]).celestial, WCS(new["wcs"]).celestial
        self.ratio = old["pixel_scale_arcsec"] / new["pixel_scale_arcsec"]
        a, b = old["wcs"], new["wcs"]
        keys = (a.keys() | b.keys()) - {"CRPIX1", "CRPIX2"}
        self.translation = None
        if self.ratio == 1 and all(a.get(k) == b.get(k) for k in keys):
            delta = np.array([b["CRPIX1"]-a["CRPIX1"],
                              new["height"]-old["height"]-(b["CRPIX2"]-a["CRPIX2"])])
            if np.allclose(delta, np.round(delta), atol=1e-8, rtol=0):
                self.translation = np.round(delta)

    def points(self, points) -> np.ndarray:
        """Map top-down pixel-edge coordinates, including WCS reference-frame conversion."""
        p = np.asarray(points, dtype=float).reshape(-1, 2)
        if self.translation is not None:
            return p+self.translation
        world = self.source.pixel_to_world(p[:, 0] - .5, self.old["height"] - p[:, 1] - .5)
        x, y = self.target.world_to_pixel(world)
        result = np.column_stack((x + .5, self.new["height"] - y - .5))
        if not np.all(np.isfinite(result)):
            raise ValueError("Edits cannot be mapped onto this sky projection.")
        return result

    def rect(self, rect) -> np.ndarray:
        x1, y1, x2, y2 = rect
        mapped = self.points([[x1, y1], [x1, y2], [x2, y1], [x2, y2]])
        return np.concatenate((mapped.min(axis=0), mapped.max(axis=0)))

def _output_points(points, meta, settings, inverse=False):
    center = np.array([meta["width"], meta["height"]]) / 2
    angle = np.deg2rad(settings.get("rotation", 0))
    c, s = np.cos(angle), np.sin(angle)
    rotation = np.array([[c, -s], [s, c]])
    flip = np.diag([-1 if settings.get("flipX") else 1, 1])
    matrix = rotation @ flip
    return (np.asarray(points) - center) @ (matrix.T if inverse else matrix) + center


def remap_state(state: dict, old: dict, new: dict, *, geometry=True) -> dict:
    result = deepcopy(state)
    mapping = GridMap(old, new)

    def snapshot(value):
        for key in ("starOverrides", "starWeights", "starClasses", "starsMigration"):
            value.pop(key, None)
        value.get("global", {}).pop("stars", None)
        value.get("global", {}).pop("parallax", None)
        if not geometry:
            return
        settings = value.get("global", {})
        crop = settings.get("crop")
        if crop:
            # The stored rectangle belongs to its application angle, not today's angle.
            crop_settings = {**settings, "rotation": crop_rotation(settings)}
            if all(crop[k] == v for k, v in FULL_CROP.items()):
                crop.pop("rotation", None)
            w, h = old["width"], old["height"]
            x, y, cw, ch = (crop[k] for k in ("x", "y", "w", "h"))
            if mapping.translation is not None:
                old_center = np.array([w, h])/2
                delta = _output_points([old_center+mapping.translation], new, crop_settings, inverse=True)[0]-old_center
                lo = np.array([x*w, y*h])+delta
                hi = lo+[cw*w, ch*h]
            else:
                corners = [[x*w, y*h], [(x+cw)*w, y*h], [x*w, (y+ch)*h], [(x+cw)*w, (y+ch)*h]]
                source = _output_points(corners, old, crop_settings)
                target = _output_points(mapping.points(source), new, crop_settings, inverse=True)
                lo, hi = target.min(axis=0), target.max(axis=0)
            size = np.array([new["width"], new["height"]])
            crop.update(zip(("x", "y", "w", "h"), np.concatenate((lo / size, (hi-lo) / size)).tolist()))
        for layer in value.get("layers", []):
            transform = layer.get("transform")
            if transform:
                old_center = np.array([old["width"], old["height"]]) / 2
                new_center = np.array([new["width"], new["height"]]) / 2
                # Conjugate the layer transform around the new canvas centre.
                a = np.deg2rad(transform.get("rotation", 0))
                matrix = np.array([[np.cos(a), -np.sin(a)], [np.sin(a), np.cos(a)]])
                offset = np.array([transform.get("x", 0)*old["width"], transform.get("y", 0)*old["height"]])
                if mapping.translation is not None:
                    displacement = old_center+mapping.translation-new_center
                    delta = (offset+(np.eye(2)-matrix*transform.get("scale", 1)) @ displacement)/(new_center*2)
                else:
                    inverse = GridMap(new, old).points([new_center])[0]
                    moved = (inverse-old_center) @ matrix.T * transform.get("scale", 1) + old_center + offset
                    delta = (mapping.points([moved])[0]-new_center) / (new_center*2)
                transform.update(x=float(delta[0]), y=float(delta[1]))
            for stroke in layer.get("brush", {}).get("strokes", []):
                if stroke.get("points"):
                    stroke["points"] = mapping.points(stroke["points"]).tolist()
                stroke["size"] *= mapping.ratio
            edge = layer.get("edge")
            if edge and "feather" in edge:
                edge["feather"] *= mapping.ratio
        # Rotation/flip are output-space controls, not sky-coordinate geometry.

    snapshot(result)
    for checkpoint in result.get("checkpoints", []):
        snapshot(checkpoint["state"])
    return result


def remap_markers(markers: list[dict], old: dict, new: dict) -> list[dict]:
    result, mapping = deepcopy(markers), GridMap(old, new)
    for marker in result:
        if marker.get("kind") == "rect":
            marker.update(zip(("x1", "y1", "x2", "y2"),
                              mapping.rect([marker[k] for k in ("x1", "y1", "x2", "y2")]).tolist()))
        else:
            x, y = mapping.points([[marker["x"], marker["y"]]])[0]
            marker.update(x=float(x), y=float(y))
            if "radius" in marker:
                marker["radius"] *= mapping.ratio
    return result


def before_checkpoint(state: dict, name: str) -> dict:
    result = deepcopy(state)
    snapshot = {k: v for k, v in deepcopy(state).items() if k != "checkpoints"}
    result.setdefault("checkpoints", []).append({
        "id": uuid.uuid4().hex, "name": name,
        "created": datetime.now(timezone.utc).isoformat(), "state": snapshot,
    })
    return result
