"""Reversible survey surroundings for a locked HST/JWST composition."""
from __future__ import annotations

import copy
import math
import uuid
from pathlib import Path

import numpy as np
from astropy.wcs import WCS

from astroimp import surveys
from astroimp.build import MAX_SIDE, build, noop
from astroimp.cancellation import check_cancelled
from astroimp.grid import grid_from_meta, read
from astroimp.mast import write_json
from astroimp.render import stretch


def margin_grid(meta: dict, margin: float) -> dict:
    if not math.isfinite(margin) or not 0 < margin <= 1:
        raise ValueError("Margin must be greater than 0 and at most 100% per side.")
    grid = grid_from_meta(meta)
    dx, dy = math.ceil(meta["width"]*margin), math.ceil(meta["height"]*margin)
    grid.update(width=meta["width"]+2*dx, height=meta["height"]+2*dy)
    grid["wcs"]["CRPIX1"] += dx
    grid["wcs"]["CRPIX2"] += dy
    grid.pop("region_sky", None)
    if max(grid["width"], grid["height"]) > MAX_SIDE:
        from astroimp.fullres import subgrid
        factor = MAX_SIDE/max(grid["width"], grid["height"])
        shape = tuple(max(1, math.ceil(grid[k]*factor-1e-8)) for k in ("height", "width"))
        target = subgrid(WCS(grid["wcs"]), grid["height"], (0, 0), shape, factor)
        grid.update(width=shape[1], height=shape[0], wcs=dict(target.to_header()),
                    pixel_scale_arcsec=grid["pixel_scale_arcsec"]/factor)
    return grid


def grid_view(grid: dict) -> dict:
    wcs = WCS(grid["wcs"])
    width, height = grid["width"], grid["height"]
    center = wcs.pixel_to_world((width-1)/2, (height-1)/2).icrs
    up = wcs.pixel_to_world((width-1)/2, (height-1)/2+1).icrs
    angle = float(center.position_angle(up).deg)
    return {"center_icrs_deg": [float(center.ra.deg), float(center.dec.deg)],
            "field_arcmin": [width*grid["pixel_scale_arcsec"]/60,
                             height*grid["pixel_scale_arcsec"]/60],
            "corners_icrs_deg": wcs.calc_footprint(axes=(width, height)).tolist(),
            "rotation_deg": (angle+180) % 360-180,
            "pixel_dimensions": [width, height]}


def fit_match(x: np.ndarray, y: np.ndarray) -> dict:
    x, y = np.asarray(x, np.float64), np.asarray(y, np.float64)
    valid = np.isfinite(x) & np.isfinite(y)
    a, b = x[valid][::max(1, int(valid.sum())//50000)], y[valid][::max(1, int(valid.sum())//50000)]
    if len(a) < 32 or np.std(a) < 1e-5:
        return {"gain": 1., "offset": 0.}
    lo, hi = np.percentile(a, [5, 95])
    keep = (a >= lo) & (a <= hi)
    gain, offset = np.linalg.lstsq(np.column_stack((a[keep], np.ones(keep.sum()))),
                                  b[keep], rcond=None)[0]
    # Unrelated dust/optical morphology can be anticorrelated. Match its robust
    # contrast instead of inverting the sky when the regression has negative gain.
    if gain <= 0:
        gain = np.std(b[keep])/max(np.std(a[keep]), 1e-5)
        offset = np.median(b[keep])-gain*np.median(a[keep])
    return {"gain": float(np.clip(gain, .05, 20)), "offset": float(np.clip(offset, -1, 1))}


def hide_missing_sources(state: dict, meta: dict, template: dict) -> None:
    """Keep omitted sources omitted when the editor reconciles the rebuilt project."""
    present = {layer["source"] for layer in state["layers"]}
    for source in meta["layers"]:
        if source["key"] not in present:
            hidden = copy.deepcopy(template)
            hidden.update(id=uuid.uuid4().hex, source=source["key"], visible=False,
                          name=source.get("name") or source["filter"])
            hidden.pop("toneMatch", None)
            state["layers"].insert(0, hidden)


def sample_layer(path: Path, dtype: str, shape: tuple, stride: int) -> np.ndarray:
    mapped = np.memmap(path, dtype=dtype, mode="r", shape=shape)
    try:
        return np.array(mapped[::stride, ::stride], dtype=np.float32, copy=True)
    finally:
        mapped._mmap.close()


def fill_around(session: Path, downloads: Path, *, margin: float = .5,
                obs_id: str | None = None, progress=noop) -> dict:
    old_meta = read(session / "project" / "meta.json")
    edits = read(session / "edits.json")
    if not old_meta or not edits or not any(
            layer.get("collection") in {"HST", "JWST"} for layer in old_meta["layers"]):
        raise ValueError("Build and open an HST/JWST composition before using Fill around.")
    grid = margin_grid(old_meta, margin)
    view = grid_view(grid)
    progress("search", 0, "Finding a survey for the surrounding sky")
    bands = surveys.available(view, downloads.parent / "cache", progress)
    selection = read(session / "selection.json", {"obs_ids": []})
    choice = obs_id or next((identifier for identifier in reversed(selection["obs_ids"])
                            if any(b.obs_id == identifier for b in bands)), None)
    recommended = next((band for band in bands if band.obs_id == choice), None) if choice else next(
        (band for survey, name in (("IPHAS", "Hα"), ("VPHAS+", "Hα"), ("DSS2", "red"))
         for band in bands if band.survey == survey and band.band == name), None)
    if recommended is None:
        raise ValueError("No suitable survey covers the expanded field.")
    content = read(session / "catalog.json", {})
    existing = {o["obs_id"] for g in content.get("groups", []) for o in g["observations"]}
    additions = surveys.groups(view, [band for band in bands if band.obs_id not in existing])
    for group in additions:
        previous = next((g for g in content["groups"] if g.get("key") == group["key"]), None)
        if previous:
            previous["observations"].extend(group["observations"])
            previous["filters"] = list(dict.fromkeys(previous["filters"]+group["filters"]))
        else:
            content["groups"].append(group)
    check_cancelled(progress)
    write_json(session / "catalog.json", content)
    from astroimp.session import save_catalog_summary
    save_catalog_summary(session, content)
    write_json(session / "selection.json", {"obs_ids": list(dict.fromkeys(
        selection["obs_ids"] + [recommended.obs_id]))})
    try:
        meta = build(session, downloads=downloads, progress=progress, expand=grid,
                     checkpoint_name="Before fill")
    except BaseException:
        write_json(session / "selection.json", selection)
        raise
    state = read(session / "edits.json", edits)
    source = next(layer for layer in meta["layers"] if layer["obs_id"] == recommended.obs_id)
    sources = {layer["key"]: layer for layer in meta["layers"]}
    target = min((layer for layer in state["layers"]
                  if layer.get("visible", True) and sources.get(layer["source"], {}).get("collection") in {"HST", "JWST"}),
                 key=lambda layer: abs(math.log((sources[layer["source"]].get("wavelength_nm") or 656) /
                                                recommended.wavelength_nm)), default=None)
    if target is None:
        return meta
    layer = copy.deepcopy(target)
    layer.update(id=uuid.uuid4().hex, name=f"{recommended.survey} {recommended.band}",
                 source=source["key"], visible=True, black=0, white=1, k=10,
                 stretch="asinh", intensity=1, opacity=1, blend="add")
    layer.pop("toneMatch", None)
    layer.update(mask={"source": None, "black": 0, "white": 1, "invert": False},
                 transform={"x": 0, "y": 0, "scale": 1, "rotation": 0},
                 edge={"feather": 0, "irregularity": 0}, brush={"strokes": []})
    shape = (meta["height"], meta["width"])
    stride = max(1, math.ceil(max(shape)/768))
    def load(name, dtype="<f2"):
        return sample_layer(session / "project" / name, dtype, shape, stride)
    target_source = sources[target["source"]]
    a, b = load(source["file"]), load(target_source["file"])
    ca, cb = load(source["coverage_file"], "u1"), load(target_source["coverage_file"], "u1")
    overlap = (ca > 0) & (cb > 0)
    layer["toneMatch"] = fit_match(stretch(a[overlap], layer), stretch(b[overlap], target))
    ratio = old_meta["pixel_scale_arcsec"]/meta["pixel_scale_arcsec"]
    for old in state["layers"]:
        if sources.get(old["source"], {}).get("collection") in {"HST", "JWST"}:
            old["edge"] = {"feather": max(old.get("edge", {}).get("feather", 0),
                                        min(old_meta["width"], old_meta["height"])*ratio*.05),
                           "irregularity": old.get("edge", {}).get("irregularity", 0)}
    state["layers"] = [old for old in state["layers"] if old["source"] != source["key"]] + [layer]
    hide_missing_sources(state, meta, layer)
    checkpoint = next((entry for entry in reversed(state.get("checkpoints", []))
                       if entry.get("name") == "Before fill"), None)
    if checkpoint:
        # Reconciliation adds new sources on reload. Keep them hidden in the
        # checkpoint so restoring the old look stays restored.
        hide_missing_sources(checkpoint["state"], meta, layer)
    state["global"]["crop"] = {"x": 0, "y": 0, "w": 1, "h": 1}
    write_json(session / "edits.json", state)
    revision = read(session / "edits.rev.json", {}).get("revision", 0)
    write_json(session / "edits.rev.json", {"revision": revision+1, "source": "fill",
                                         "note": "Filled surrounding sky"})
    return meta
