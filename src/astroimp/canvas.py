"""Fast cached canvas mosaics and region-first build-grid planning."""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path

import numpy as np
from astropy.wcs import WCS
from PIL import Image
from reproject import reproject_interp

from astroimp.cancellation import check_cancelled
from astroimp.grid import GridMap, capture_rotation_changed, grid_from_meta, read
from astroimp.mast import write_json
from astroimp import surveys

PREVIEW_SIDE = 1024
PREVIEW_VERSION = 3


def preview_pixels(pixels: np.ndarray, factor: int) -> np.ndarray:
    from astroimp.build import block_mean
    if factor < 8:
        return block_mean(pixels, factor)
    height, width = pixels.shape
    shape = (math.ceil(height/factor), math.ceil(width/factor))
    total, count = np.zeros(shape, np.float32), np.zeros(shape, np.uint8)
    # A stratified proxy avoids scanning tens of GB for a rough framing decision.
    for dy in (.125, .375, .625, .875):
        iy = np.minimum(((np.arange(shape[0])+dy)*factor).astype(int), height-1)
        for dx in (.125, .375, .625, .875):
            ix = np.minimum(((np.arange(shape[1])+dx)*factor).astype(int), width-1)
            values = pixels[np.ix_(iy, ix)].astype(np.float32)
            valid = np.isfinite(values)
            total += np.where(valid, values, 0)
            count += valid
    np.divide(total, count, out=total, where=count > 0)
    total[count == 0] = np.nan
    return total


def area(polygon) -> float:
    p = np.asarray(polygon)
    return float(abs(np.sum(p[:, 0]*np.roll(p[:, 1], -1)-p[:, 1]*np.roll(p[:, 0], -1)))/2) if len(p) >= 3 else 0.


def clip(polygon, rect) -> list:
    result = [list(p) for p in polygon]
    x, y, w, h = (rect[k] for k in ("x", "y", "w", "h"))
    for axis, bound, sign in ((0, x, 1), (0, x+w, -1), (1, y, 1), (1, y+h, -1)):
        source, result = result, []
        if not source:
            break
        previous = source[-1]
        for point in source:
            inside = (point[axis]-bound)*sign >= 0
            before = (previous[axis]-bound)*sign >= 0
            if inside != before:
                t = (bound-previous[axis])/(point[axis]-previous[axis])
                result.append([previous[i]+t*(point[i]-previous[i]) for i in (0, 1)])
            if inside:
                result.append(point)
            previous = point
    return result


def survey_preview_grid(session: Path, chosen: list[dict]) -> dict | None:
    bands = [surveys.band_for(obs) for obs in chosen if surveys.is_survey(obs)]
    if not bands:
        return None
    current = read(session / "project" / "meta.json")
    if current and not capture_rotation_changed(session, current):
        from astroimp.fullres import subgrid
        factor = min(1., PREVIEW_SIDE/max(current["width"], current["height"]))
        shape = tuple(math.ceil(current[k]*factor) for k in ("height", "width"))
        wcs = subgrid(WCS(current["wcs"]), current["height"], (0, 0), shape, factor)
        return {"width": shape[1], "height": shape[0], "wcs": dict(wcs.to_header()),
                "pixel_scale_arcsec": current["pixel_scale_arcsec"]/factor}
    return surveys.view_grid(read(session / "view.json"),
                             min(b.native_scale_arcsec for b in bands), PREVIEW_SIDE)


def sources(session: Path, downloads: Path):
    from astroimp.build import cached_source
    catalog = read(session / "catalog.json", {})
    available = {o["obs_id"]: o for g in catalog.get("groups", []) for o in g["observations"]}
    selected = sorted(set(read(session / "selection.json", {}).get("obs_ids", [])))
    if not selected or not set(selected) <= available.keys():
        raise ValueError("Select at least one catalog image.")
    grid = survey_preview_grid(session, [available[key] for key in selected])
    result = []
    for key in selected:
        path = cached_source(available[key], downloads,
                             grid if surveys.is_survey(available[key]) else None)
        if not path:
            raise ValueError("Download the selected images before choosing a canvas.")
        result.append((available[key], path))
    return result


def _key(session: Path, selected) -> str:
    signatures = [(obs["obs_id"], obs.get("wavelength_nm"), str(path),
                   path.stat().st_size, path.stat().st_mtime_ns) for obs, path in selected]
    current = read(session / "project" / "meta.json")
    return hashlib.sha256(json.dumps([PREVIEW_VERSION, signatures,
        grid_from_meta(current) if current else None,
        read(session / "view.json", {}).get("rotation_deg", 0)], sort_keys=True).encode()).hexdigest()[:24]


def prepare(session: Path, downloads: Path, progress=None) -> dict:
    from astroimp.build import (download_all, noop, normalize, optimal_grid,
                               pixel_scale, science)
    from astroimp.fullres import linear_wcs, subgrid
    progress = progress or noop
    available = {o["obs_id"]: o for g in read(session / "catalog.json", {})["groups"] for o in g["observations"]}
    selected_ids = sorted(set(read(session / "selection.json", {}).get("obs_ids", [])))
    if not selected_ids or not set(selected_ids) <= available.keys():
        raise ValueError("Select at least one catalog image.")
    try:
        selected = sources(session, downloads)
    except ValueError:
        chosen = [available[k] for k in selected_ids]
        grid = survey_preview_grid(session, chosen)
        download_all(chosen, downloads, progress, grid=grid)
        selected = sources(session, downloads)
    key = _key(session, selected)
    cache = session / ".canvas-preview"
    cached = read(cache / f"{key}.json")
    if cached and (cache / f"{key}.png").exists():
        progress("canvas", 1, "Canvas preview ready")
        return cached
    if hasattr(progress, "heavy"):
        progress.heavy("canvas")
    inputs, native = [], []
    for obs, path in selected:
        with science(path) as (hdu, wcs, _):
            inputs.append((hdu.shape, wcs))
            native.append(surveys.band_for(obs).native_scale_arcsec
                          if surveys.is_survey(obs) else pixel_scale(wcs)*3600)
    current = read(session / "project" / "meta.json")
    reference = grid_from_meta(current) if current else None
    view = read(session / "view.json", {})
    if capture_rotation_changed(session, current) or (
            reference is None and float(view.get("rotation_deg", 0)) != 0):
        reference = surveys.view_grid(view, min(native))
    if reference:
        bounds = []
        for (h, w), wcs in inputs:
            source = {"width": w, "height": h, "wcs": dict(wcs.to_header()), "pixel_scale_arcsec": pixel_scale(wcs)*3600}
            bounds.extend(GridMap(source, reference).points([[0, 0], [w, 0], [w, h], [0, h]]))
        bounds.extend([[0, 0], [reference["width"], reference["height"]]])
        lo, hi = np.floor(np.min(bounds, axis=0)), np.ceil(np.max(bounds, axis=0))
        factor = min(1., PREVIEW_SIDE/max(hi-lo))
        width, height = np.maximum(1, np.ceil((hi-lo)*factor-1e-8)).astype(int)
        target = subgrid(WCS(reference["wcs"]), reference["height"], tuple(lo*factor),
                         (int(height), int(width)), factor)
    else:
        target, shape = optimal_grid(inputs, PREVIEW_SIDE)
        height, width = shape
    meta = {"width": int(width), "height": int(height), "wcs": dict(target.to_header()),
            "pixel_scale_arcsec": pixel_scale(target)*3600}
    if reference is None:
        reference = grid_from_meta(meta)
    shape = (int(height), int(width))
    rgb, count = np.zeros((*shape, 3), np.float32), np.zeros(shape, np.float32)
    footprints = []
    for i, ((obs, path), ((h, w), wcs)) in enumerate(zip(selected, inputs)):
        check_cancelled(progress)
        progress("canvas", i/len(selected), f"Preview {obs['filter']}")
        source = {"width": w, "height": h, "wcs": dict(wcs.to_header()), "pixel_scale_arcsec": native[i]}
        points = GridMap(source, meta).points([[0, 0], [w, 0], [w, h], [0, h]])
        nm = obs.get("wavelength_nm") or 550
        t = np.clip(math.log(nm/200)/math.log(125), 0, 1)
        color = np.array([.3+.7*t, .8-.35*abs(2*t-1), 1-.75*t], np.float32)
        footprints.append({"key": obs["obs_id"], "filter": obs["filter"],
                           "color": "#"+"".join(f"{round(v*255):02x}" for v in color),
                           "points": points.tolist(), "native_scale_arcsec": native[i]})
        with science(path) as (hdu, wcs, _):
            # Sample only the low-resolution proxy; science-quality repair stays in Build.
            factor = max(1, math.floor(pixel_scale(target)/pixel_scale(wcs)))
            pixels = preview_pixels(hdu.data, factor)
            wcs = linear_wcs(wcs).slice((slice(None, None, factor),)*2)
            image = reproject_interp((pixels, wcs), target, shape_out=shape,
                                     return_footprint=False, output_array=np.empty(shape, np.float32))
        valid = np.isfinite(image)[::-1]
        image, _, _ = normalize(image)
        light = np.arcsinh(np.maximum(image[::-1], 0)*12)/np.arcsinh(12)
        rgb += light[..., None]*color*valid[..., None]
        count += valid
    np.divide(rgb, count[..., None], out=rgb, where=count[..., None] > 0)
    image = np.uint8(np.clip(rgb, 0, 1)*255)
    cache.mkdir(exist_ok=True)
    temporary = cache / f"{key}.part.png"
    Image.fromarray(image).save(temporary)
    temporary.replace(cache / f"{key}.png")
    rect = GridMap(reference, meta).rect([0, 0, reference["width"], reference["height"]])
    current_rect = {"x": float(rect[0]), "y": float(rect[1]),
                    "w": float(rect[2]-rect[0]), "h": float(rect[3]-rect[1])}
    result = {**meta, "id": key, "image_url": f"/api/build/canvas/preview/{key}.png",
              "footprints": footprints, "current_rect": current_rect if current else None,
              "reference": reference, "current_scale_arcsec": current.get("pixel_scale_arcsec") if current else None,
              "default_region": current_rect if current else {"x": 0, "y": 0, "w": int(width), "h": int(height)}}
    write_json(cache / f"{key}.json", result)
    progress("canvas", 1, "Canvas preview ready")
    return result


def load(session: Path, identifier: str, downloads: Path) -> dict:
    if len(identifier) != 24 or any(c not in "0123456789abcdef" for c in identifier):
        raise ValueError("Invalid canvas preview.")
    metadata = read(session / ".canvas-preview" / f"{identifier}.json")
    if metadata is None or _key(session, sources(session, downloads)) != identifier:
        raise ValueError("The selected images or canvas changed. Open Canvas again.")
    return metadata


def plan(metadata: dict, region: dict, max_side: int = 3072) -> dict:
    from astroimp.fullres import subgrid
    values = np.array([region.get(k, np.nan) for k in ("x", "y", "w", "h")], float)
    if not np.all(np.isfinite(values)) or np.any(values[2:] <= 0):
        raise ValueError("Draw a canvas rectangle.")
    x, y, w, h = values
    if x < -1e-6 or y < -1e-6 or x+w > metadata["width"]+1e-6 or y+h > metadata["height"]+1e-6:
        raise ValueError("Keep the canvas inside the preview.")
    kept, scales = [], []
    for footprint in metadata["footprints"]:
        overlap = area(clip(footprint["points"], region))
        kept.append({"key": footprint["key"], "kept_fraction": min(1., overlap/max(area(footprint["points"]), 1e-20))})
        # At least 1% of the chosen canvas, not a fine image touching one tiny corner.
        if overlap >= w*h*.01:
            scales.append(footprint["native_scale_arcsec"])
    if not scales:
        raise ValueError("Choose a region containing selected image data.")
    reference = metadata["reference"]
    bounds = GridMap(metadata, reference).rect([x, y, x+w, y+h])
    lo, hi = np.round(bounds[:2]), np.round(bounds[2:])
    extent = np.maximum(1, hi-lo)
    scale = max(min(scales), max(extent)*reference["pixel_scale_arcsec"]/max_side)
    if abs(scale/reference["pixel_scale_arcsec"]-1) < 1e-8:
        scale = reference["pixel_scale_arcsec"]
        width, height = extent.astype(int)
        grid = grid_from_meta(reference)
        grid.update(width=int(width), height=int(height))
        grid["wcs"]["CRPIX1"] -= lo[0]
        grid["wcs"]["CRPIX2"] += height-reference["height"]+lo[1]
    else:
        factor = reference["pixel_scale_arcsec"]/scale
        width, height = np.maximum(1, np.ceil(extent*factor-1e-7)).astype(int)
        wcs = subgrid(WCS(reference["wcs"]), reference["height"], tuple(lo*factor),
                      (int(height), int(width)), factor)
        grid = {"width": int(width), "height": int(height), "wcs": dict(wcs.to_header()),
                "pixel_scale_arcsec": float(scale)}
    target = WCS(grid["wcs"])
    world = target.pixel_to_world(np.array([0, grid["width"], grid["width"], 0])-.5,
                                  grid["height"]-np.array([0, 0, grid["height"], grid["height"]])-.5).icrs
    grid["region_sky"] = np.column_stack((world.ra.deg, world.dec.deg)).tolist()
    return {"grid": grid, "kept": kept, "detail_ratio": scale/metadata["current_scale_arcsec"]
            if metadata.get("current_scale_arcsec") else 1.}
