"""Bounded-memory FITS mosaics and display-ready Studio layers."""
from __future__ import annotations

import copy
import hashlib
import json
import math
import os
import re
import shutil
import threading
import time
import uuid
import warnings
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

import numpy as np
import requests
from astropy import units as u
from astropy.io import fits
from astropy.stats import sigma_clipped_stats
from astropy.wcs import FITSFixedWarning, WCS
from astropy.wcs.utils import proj_plane_pixel_scales
from reproject import reproject_interp
from reproject.mosaicking import find_optimal_celestial_wcs
from scipy import ndimage as ndi

from astroimp.builds import archive_current
from astroimp.cancellation import CancelledError, check_cancelled
from astroimp.grid import (before_checkpoint, capture_rotation_changed, grid_from_meta, locked_grid, read,
                           remap_markers, remap_state, same_grid)
from astroimp.mast import write_json
from astroimp.planes import load_plane
from astroimp import surveys, mosaic
from astroimp.services import friendly

MAX_SIDE = 3072
# Bump whenever per-layer processing changes so cached aligned layers are rebuilt.
BUILD_VERSION = "studio-build-v8-science-only"
Progress = Callable[[str, float, str], None]
_sizes: dict[str, int | None] = {}
_sizes_lock = threading.Lock()
_download_locks = [threading.Lock() for _ in range(64)]
_edge_locks = [threading.Lock() for _ in range(64)]
_wcs_lock = threading.Lock()


def noop(stage: str, fraction: float, message: str) -> None:
    pass


def safe_key(value: str) -> str:
    key = re.sub(r"[^A-Za-z0-9_.-]", "_", value)
    if not key or key in {".", ".."}:
        raise ValueError("Empty or unsafe observation ID.")
    return key


@contextmanager
def science(path: Path):
    """Keep the FITS memory mapping alive only while processing this layer."""
    with fits.open(path, memmap=True) as hdus:
        hdu = next((h for h in hdus if h.name == "SCI"), None)
        if hdu is None:
            hdu = next((h for h in hdus if h.header.get("NAXIS") == 2
                        and isinstance(h, (fits.PrimaryHDU, fits.ImageHDU,
                                           fits.CompImageHDU))), None)
        if hdu is None or hdu.header.get("NAXIS") != 2:
            raise ValueError(f"{path.name}: expected a 2-D science image.")
        header = hdus[0].header.copy()
        header.update(hdu.header)
        # Automatic date/observatory fixes do not affect this celestial projection.
        # Astropy's warning logger scans sys.modules unsafely during parallel reads.
        with _wcs_lock, warnings.catch_warnings():
            warnings.simplefilter("ignore", FITSFixedWarning)
            wcs = WCS(header).celestial
        if not wcs.has_celestial:
            raise ValueError(f"{path.name}: science image has no celestial WCS.")
        weight = next((h.data for h in hdus if h.name == "WHT" and h.shape == hdu.shape), None)
        yield hdu, wcs, weight


# Drizzled HST/JWST mosaics have stepped, low-exposure fringes (few dithers overlap) that are
# visibly noisy. Pixels whose exposure weight is below this fraction of the median are dropped.
MIN_RELATIVE_WEIGHT = 0.4
EDGE_TRIM_PIXELS = 2


def low_weight_cut(pixels: np.ndarray, weight: np.ndarray | None) -> np.ndarray:
    if weight is None:
        return pixels
    sample = weight[::4, ::4]
    positive = sample[np.isfinite(sample) & (sample > 0)]
    if not positive.size:
        return pixels
    low = ~(np.isfinite(weight) & (weight >= MIN_RELATIVE_WEIGHT * float(np.median(positive))))
    # Only wide low-weight regions reaching the outside are fringe. Thin interior streaks
    # (cosmic rays, bad columns) and saturated star cores also have low weight but are kept.
    low = ndi.binary_opening(low, structure=np.ones((3, 3), bool), iterations=3)
    labels, _ = ndi.label(low)
    edge = np.unique(np.concatenate((labels[0], labels[-1], labels[:, 0], labels[:, -1])))
    fringe = np.isin(labels, edge[edge > 0])
    del labels, low
    keep = ndi.binary_erosion(~fringe, iterations=EDGE_TRIM_PIXELS)
    return np.where(keep, pixels, np.float32(np.nan)).astype(np.float32, copy=False)


def repair_weight_streaks(pixels: np.ndarray, weight: np.ndarray | None) -> np.ndarray:
    """Repair thin low-exposure detector seams, not compact saturated stellar cores."""
    if weight is None:
        return pixels
    sample = weight[::4, ::4]
    positive = sample[np.isfinite(sample) & (sample > 0)]
    if not positive.size:
        return pixels
    low = ~(np.isfinite(weight) & (weight >= MIN_RELATIVE_WEIGHT * float(np.median(positive))))
    opened = ndi.binary_opening(low, structure=np.ones((3, 3), bool), iterations=3)
    labels, _ = ndi.label(opened)
    edge = np.unique(np.concatenate((labels[0], labels[-1], labels[:, 0], labels[:, -1])))
    fringe = np.isin(labels, edge[edge > 0])
    del labels, opened
    interior = low & ~ndi.binary_dilation(fringe, iterations=EDGE_TRIM_PIXELS)
    labels, count = ndi.label(interior, structure=np.ones((3, 3), bool))
    areas = np.bincount(labels.ravel(), minlength=count + 1)
    selected = np.zeros(count + 1, bool)
    for ident, bounds in enumerate(ndi.find_objects(labels), 1):
        if bounds is None:
            continue
        extent = max(s.stop - s.start for s in bounds)
        # Extent/area admits bent and branching chip gaps; covariance alone rejects Ls.
        if extent < 16 or areas[ident] > 12 * extent or areas[ident] > extent**2 / 4:
            continue
        component = labels[bounds] == ident
        distance = ndi.distance_transform_edt(np.pad(component, 1))[1:-1, 1:-1]
        # Median half-width tolerates wider junctions (WFPC2 gaps reach ~20 px there).
        selected[ident] = np.median(distance[component]) <= 2
    streak = selected[labels]
    del labels, interior
    if not streak.any():
        return pixels
    # Do not change separate compact low-weight components, even next to a seam.
    protected = ndi.binary_dilation(low & ~streak & ~fringe, iterations=2)
    repair = ndi.binary_dilation(streak, iterations=2) & ~fringe & ~protected
    result = np.array(pixels, dtype=np.float32, copy=True)
    height, width = pixels.shape
    # Local windows bound Gaussian/median scratch space even for native JWST mosaics.
    for y in range(0, height, 256):
        for x in range(0, width, 256):
            core = np.s_[y:min(y + 256, height), x:min(x + 256, width)]
            if not repair[core].any():
                continue
            y0, x0 = max(0, y - 24), max(0, x - 24)
            window = np.s_[y0:min(y + 280, height), x0:min(x + 280, width)]
            local = pixels[window]
            good = np.isfinite(local) & ~low[window] & ~repair[window]
            support = ndi.gaussian_filter(good.astype(np.float32), 4)
            filled = ndi.gaussian_filter(np.where(good, local, 0).astype(np.float32), 4)
            np.divide(filled, support, out=filled, where=support > 0)
            change = repair[window] & (support > 0)
            # Bleed columns can connect directly to a saturated core. Protect its broad,
            # compact light, not isolated cosmic-ray hits or the widened gap junction.
            smooth = ndi.median_filter(np.where(np.isfinite(local), local, filled), size=5)
            contrast = smooth - ndi.gaussian_filter(smooth, 8)
            residual = contrast[good]
            noise = (1.4826 * np.median(np.abs(residual - np.median(residual)))
                     if residual.size else 0)
            core_light = (contrast > 6 * noise) & ndi.binary_erosion(low[window], iterations=3)
            cores, count = ndi.label(core_light, structure=np.ones((3, 3), bool))
            broad = np.bincount(cores.ravel(), minlength=count + 1) >= 49
            broad[0] = False
            core_light = broad[cores]
            change &= ~ndi.binary_dilation(core_light, iterations=4)
            patched = np.where(change, filled, local)
            median = ndi.median_filter(patched, size=7)
            mad = ndi.median_filter(np.abs(patched - median), size=7)
            outlier = change & (np.abs(patched - median) > 6 * 1.4826 * mad)
            patched[outlier] = median[outlier]
            inner = np.s_[y - y0:y - y0 + core[0].stop - y,
                          x - x0:x - x0 + core[1].stop - x]
            np.copyto(result[core], patched[inner], where=change[inner])
    return result


def prepare_science(pixels: np.ndarray, weight: np.ndarray | None, factor: int = 1, *,
                    return_reference: bool = False, survey: bool = False):
    """Share source-resolution seam repair and the existing resampled fringe cut."""
    if survey:
        pixels = surveys.repair_defects(pixels)
    original = pixels
    pixels = repair_weight_streaks(pixels, weight)
    changed = pixels is not original
    if factor > 1:
        pixels = block_mean(pixels, factor)
        weight = None if weight is None else block_mean(weight, factor)
    pixels = low_weight_cut(pixels, weight)
    if not return_reference:
        return pixels
    if changed:
        original = block_mean(original, factor) if factor > 1 else original
        original = low_weight_cut(original, weight)
    return pixels, original if changed else None


def report_details(progress: Progress, **values) -> None:
    update = getattr(progress, "update", None)
    if update:
        update(**values)


def filename(observation: dict) -> str:
    if surveys.is_survey(observation):
        return surveys.slug(observation["hips_id"]) + ".fits"
    uri = observation["data_uri"]
    name = uri.rsplit("/", 1)[-1]
    if (not uri.startswith("mast:") or not name.lower().endswith(".fits")
            or any(c in name for c in '<>:"/\\|?*\r\n') or name in {".", ".."}):
        raise ValueError("Expected a safe MAST FITS URI.")
    return name


def cached_source(observation: dict, cache: Path, grid: dict | None = None) -> Path | None:
    if surveys.is_survey(observation):
        if grid is None:
            return None
        path = surveys.cache_path(observation, cache, grid)
        return path.resolve() if path.is_file() else None
    name = filename(observation)
    if cache.exists():
        for path in cache.rglob(name):
            if path.is_file():
                try:
                    with science(path):
                        pass
                except (OSError, ValueError):
                    continue
                return path.resolve()
    return None


def survey_size(grid: dict | None) -> int | None:
    """Exact size of the float32 FITS cutout written for a grid (2880-byte header + padded data)."""
    if not grid:
        return None
    data = grid["width"] * grid["height"] * 4
    return 2880 + -(-data // 2880) * 2880


def file_size(observation: dict, grid: dict | None = None) -> int | None:
    if surveys.is_survey(observation):
        return survey_size(grid)
    for key in ("size_bytes", "size", "sizeBytes"):
        value = observation.get(key)
        if value is not None and float(value) > 0:
            return int(value)
    uri = observation["data_uri"]
    with _sizes_lock:
        if uri in _sizes:
            return _sizes[uri]
    total = None
    try:
        with requests.head("https://mast.stsci.edu/api/v0.1/Download/file",
                           params={"uri": uri}, timeout=(3, 3), allow_redirects=True) as response:
            response.raise_for_status()
            total = int(response.headers.get("Content-Length", 0)) or None
    except (requests.RequestException, ValueError):
        pass
    with _sizes_lock:
        _sizes[uri] = total
    return total


def build_plan(chosen: list[dict], cache: Path, *, session: Path | None = None,
               grid: dict | None = None) -> dict:
    locked = locked_grid(session) if session else None
    survey_grid = grid or locked
    if session and not survey_grid and chosen and all(surveys.is_survey(obs) for obs in chosen):
        survey_grid = surveys.view_grid(read(session / "view.json"),
                                       min(surveys.band_for(obs).native_scale_arcsec for obs in chosen))
    # Mixed HST/JWST + survey builds pick their grid at build time; estimate from the view at native scale.
    size_grid = survey_grid
    if session and not size_grid and any(surveys.is_survey(obs) for obs in chosen):
        size_grid = surveys.view_grid(read(session / "view.json"), min(
            surveys.band_for(obs).native_scale_arcsec for obs in chosen if surveys.is_survey(obs)))
    def entry(obs):
        path = (None if session and surveys.is_survey(obs) and not survey_grid
                else cached_source(obs, cache, survey_grid))
        result = {"obs_id": obs["obs_id"], "filename": filename(obs), "filter": obs.get("filter"),
                "size_bytes": path.stat().st_size if path else file_size(obs, size_grid),
                "cached": path is not None}
        if locked:
            result["outside_fraction"] = outside_canvas(obs, path, locked)
        return result
    with ThreadPoolExecutor(max_workers=4) as pool:
        files = list(pool.map(entry, chosen))
    if session:
        proposals = {o["obs_id"]: g.get("proposal") for g in read(session / "catalog.json", {}).get("groups", [])
                     for o in g.get("observations", [])}
        chosen = [{**o, "proposal": o.get("proposal") or proposals.get(o["obs_id"])} for o in chosen]
    enabled = read(session / "session.json", {}).get("build", {}).get("mosaic", True) if session else True
    return {"files": files, "layer_count": len(mosaic.groups(chosen, enabled)),
            "total_bytes": sum(f["size_bytes"] or 0 for f in files),
            "cached_bytes": sum(f["size_bytes"] or 0 for f in files if f["cached"]),
            "grid_locked": locked is not None, "grid": locked}


def outside_canvas(observation: dict, path: Path | None, grid: dict) -> float | None:
    """Approximate footprint area clipped by the canvas, not science-pixel coverage."""
    from astropy.coordinates import SkyCoord
    from astroimp.catalog import polygons
    from skimage.measure import points_in_poly
    from astroimp.grid import GridMap

    if path:
        with science(path) as (hdu, wcs, _):
            h, w = hdu.shape
            source = {"wcs": dict(wcs.to_header()), "height": h, "width": w,
                      "pixel_scale_arcsec": pixel_scale(wcs)*3600}
            points = GridMap(source, grid).points([[0, 0], [w, 0], [w, h], [0, h]])
        footprints = [points]
    else:
        footprints = []
        target = WCS(grid["wcs"]).celestial
        for polygon in polygons(observation.get("region", "")):
            x, y = target.world_to_pixel(SkyCoord(polygon[:, 0]*u.deg, polygon[:, 1]*u.deg))
            footprints.append(np.column_stack((x+.5, grid["height"]-y-.5)))
    if not footprints:
        return None
    # A bounded area sample also handles non-rectangular catalog footprints.
    points = np.concatenate(footprints)
    lo, hi = points.min(axis=0), points.max(axis=0)
    x = lo[0] + (np.arange(160)+.5)/160*(hi[0]-lo[0])
    y = lo[1] + (np.arange(160)+.5)/160*(hi[1]-lo[1])
    xx, yy = np.meshgrid(x, y)
    samples = np.column_stack((xx.ravel(), yy.ravel()))
    inside = np.logical_or.reduce([points_in_poly(samples, p) for p in footprints])
    canvas = ((samples[:, 0] >= 0) & (samples[:, 0] <= grid["width"])
              & (samples[:, 1] >= 0) & (samples[:, 1] <= grid["height"]))
    return float(np.mean(~canvas[inside])) if inside.any() else None


def download(observation: dict, cache: Path, progress: Progress = noop, *, on_bytes=None,
             grid: dict | None = None) -> Path:
    if surveys.is_survey(observation):
        check_cancelled(progress)
        path = cached_source(observation, cache, grid)
        if path:
            if on_bytes:
                on_bytes(path.stat().st_size, path.stat().st_size, "cached")
            return path
        if grid is None:
            raise ValueError("Survey downloads require a target sky grid.")
        if on_bytes:
            on_bytes(0, None, "downloading")
        path = surveys.fetch(observation, cache, grid, progress)
        if on_bytes:
            on_bytes(path.stat().st_size, path.stat().st_size, "done")
        return path
    name = filename(observation)
    digest = hashlib.sha1(name.encode()).digest()
    with _download_locks[digest[0] % len(_download_locks)]:
        return _download(observation, cache, progress, on_bytes=on_bytes)


def _download(observation: dict, cache: Path, progress: Progress, *, on_bytes) -> Path:
    check_cancelled(progress)
    name = filename(observation)
    path = cached_source(observation, cache)
    if path:
        size = path.stat().st_size
        if on_bytes:
            on_bytes(size, size, "cached")
        progress("download", 1, f"Reusing {name} ({size:,} bytes)")
        return path
    destination = cache / safe_key(observation.get("collection", "other")) / name
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(name + ".part")
    try:
        with requests.get("https://mast.stsci.edu/api/v0.1/Download/file",
                          params={"uri": observation["data_uri"]}, stream=True,
                          timeout=(30, 180)) as response:
            response.raise_for_status()
            total = int(response.headers.get("Content-Length", 0))
            received = 0
            if on_bytes:
                on_bytes(0, total or None, "downloading")
            with temporary.open("wb") as stream:
                for chunk in response.iter_content(1024 * 1024):
                    check_cancelled(progress)
                    if not chunk:
                        continue
                    stream.write(chunk)
                    received += len(chunk)
                    if on_bytes:
                        on_bytes(received, total or None, "downloading")
                    progress("download", received / total if total else 0,
                             f"{name}: {received:,} / {total or '?'} bytes")
            if not received or (total and received != total):
                raise ValueError(f"Incomplete download of {name}: {received} / {total} bytes")
        with science(temporary):
            pass
        check_cancelled(progress)
        temporary.replace(destination)
        if on_bytes:
            on_bytes(received, received, "done")
    finally:
        temporary.unlink(missing_ok=True)
    return destination.resolve()


def download_all(chosen: list[dict], cache: Path, progress: Progress,
                 *, grid: dict | None = None) -> list[Path]:
    if not chosen:
        return []
    plan = build_plan(chosen, cache, grid=grid)
    files = [{**{k: v for k, v in f.items() if k != "cached"},
              "done_bytes": f["size_bytes"] if f["cached"] else 0,
              "state": "cached" if f["cached"] else "queued"} for f in plan["files"]]
    lock = threading.RLock()
    samples = deque([(time.monotonic(), 0)])
    started = samples[0][0]
    network = [0] * len(files)

    def publish(message=None):
        now = time.monotonic()
        done = sum(f["done_bytes"] for f in files)
        total = sum(f["size_bytes"] or 0 for f in files)
        samples.append((now, sum(network)))
        while len(samples) > 2 and samples[1][0] < now - 5:
            samples.popleft()
        elapsed = now - samples[0][0]
        speed = (sum(network) - samples[0][1]) / elapsed if elapsed > 0 else 0
        known = all(f["size_bytes"] is not None for f in files)
        eta = max(0, total - done) / speed if known and speed > 0 and now > started else None
        report_details(progress, phase="download", files=[dict(f) for f in files], eta_s=eta)
        fraction = done / total if known and total else sum(
            f["state"] in {"cached", "done"} for f in files) / len(files)
        progress("download", fraction, message or
                 f"Downloaded {done:,} / {total if known else '?'} bytes")

    publish()

    def fetch(index):
        def received(done, total, state):
            with lock:
                row = files[index]
                row.update(done_bytes=done, state=state)
                if total is not None:
                    row["size_bytes"] = total
                if state != "cached":
                    network[index] = done
                publish(f"{row['filename']}: {done:,} / {row['size_bytes'] or '?'} bytes ({state})")
        try:
            def file_progress(*args):
                check_cancelled(progress)
            file_progress.check_cancelled = lambda: check_cancelled(progress)
            return download(chosen[index], cache, progress=file_progress, on_bytes=received, grid=grid)
        except CancelledError:
            raise
        except Exception as exc:
            with lock:
                files[index].update(state="error", error=friendly(exc))
                publish()
            raise
    with ThreadPoolExecutor(max_workers=4) as pool:
        return list(pool.map(fetch, range(len(chosen))))


def block_mean(data: np.ndarray, factor: int) -> np.ndarray:
    """Nan-aware block means, including partial edge blocks, in bounded row strips."""
    if factor < 1:
        raise ValueError("Downsample factor must be positive.")
    height, width = data.shape
    out = np.empty((math.ceil(height / factor), math.ceil(width / factor)), np.float32)
    # Copies never exceed ~16 MiB, apart from a single block row on unusually wide images.
    rows = max(factor, (16 * 1024**2 // (width * 4) // factor) * factor)
    columns = np.arange(0, width, factor)
    for y in range(0, height, rows):
        strip = np.array(data[y:y + rows], dtype=np.float32, copy=True)
        finite = np.isfinite(strip)
        strip[~finite] = 0
        sums = np.add.reduceat(strip, columns, axis=1, dtype=np.float64)
        counts = np.add.reduceat(finite, columns, axis=1, dtype=np.int32)
        starts = np.arange(0, len(strip), factor)
        sums = np.add.reduceat(sums, starts, axis=0)
        counts = np.add.reduceat(counts, starts, axis=0)
        values = np.full(sums.shape, np.nan, np.float32)
        np.divide(sums, counts, out=values, where=counts > 0)
        out[y // factor:y // factor + len(values)] = values
    return out


def pixel_scale(wcs: WCS) -> float:
    return float(np.min(np.abs(proj_plane_pixel_scales(wcs))))


def optimal_grid(inputs: list[tuple[tuple[int, int], WCS]], max_side: int,
                 resolution: float | None = None) -> tuple[WCS, tuple]:
    if max_side < 2:
        raise ValueError("max_side must be at least 2.")
    resolution = resolution or min(pixel_scale(wcs) for _, wcs in inputs)
    wcs, shape = find_optimal_celestial_wcs(inputs, resolution=resolution * u.deg,
                                           auto_rotate=False)
    # Rounding of the footprint bounds can add a pixel; recheck after resizing.
    while max(shape) > max_side:
        resolution *= max(shape) / max_side * (1 + 1e-8)
        wcs, shape = find_optimal_celestial_wcs(inputs, resolution=resolution * u.deg,
                                               auto_rotate=False)
    return wcs, shape


def expand_grid(session: Path, inputs: list[tuple[tuple[int, int], WCS]],
                max_side: int = MAX_SIDE) -> dict:
    previous = locked_grid(session) or read(session / "project" / "meta.json")
    if not previous:
        raise ValueError("Build a canvas before expanding it.")
    matrix = WCS(previous["wcs"]).pixel_scale_matrix
    if abs(matrix[0, 1]) + abs(matrix[1, 0]) > 1e-12:
        from astroimp.fullres import subgrid
        from astroimp.grid import GridMap
        bounds = [[0, 0], [previous["width"], previous["height"]]]
        for (height, width), wcs in inputs:
            source = {"width": width, "height": height, "wcs": dict(wcs.to_header()),
                      "pixel_scale_arcsec": pixel_scale(wcs)*3600}
            bounds.extend(GridMap(source, previous).points(
                [[0, 0], [width, 0], [width, height], [0, height]]))
        lo, hi = np.floor(np.min(bounds, axis=0)), np.ceil(np.max(bounds, axis=0))
        factor = min(1., max_side / max(hi-lo))
        shape = tuple(np.maximum(2, np.ceil((hi-lo)[::-1]*factor-1e-8)).astype(int))
        target = subgrid(WCS(previous["wcs"]), previous["height"],
                         tuple(lo*factor), shape, factor)
        return {"width": int(shape[1]), "height": int(shape[0]), "wcs": dict(target.to_header()),
                "pixel_scale_arcsec": previous["pixel_scale_arcsec"]/factor}
    # Keep the existing field as well, even when some original images were deselected.
    inputs = [*inputs, ((previous["height"], previous["width"]), WCS(previous["wcs"]))]
    target, shape = optimal_grid(inputs, max_side, previous["pixel_scale_arcsec"]/3600)
    return {"width": shape[1], "height": shape[0], "wcs": dict(target.to_header()),
            "pixel_scale_arcsec": pixel_scale(target)*3600}


def expansion_plan(session: Path, downloads: Path, max_side: int = MAX_SIDE) -> dict:
    catalog = read(session / "catalog.json", {})
    observations = {o["obs_id"]: o for g in catalog.get("groups", []) for o in g["observations"]}
    selected = read(session / "selection.json", {}).get("obs_ids", [])
    if not selected or not set(selected) <= observations.keys():
        raise ValueError("Select at least one catalog observation before expanding.")
    inputs = []
    for key in sorted(set(selected)):
        if surveys.is_survey(observations[key]):
            grid = surveys.view_grid(read(session / "view.json"),
                                     surveys.band_for(observations[key]).native_scale_arcsec, max_side)
            inputs.append(((grid["height"], grid["width"]), WCS(grid["wcs"])))
            continue
        path = cached_source(observations[key], downloads)
        if not path:
            raise ValueError("Download & Build the selected images before expanding the canvas.")
        with science(path) as (hdu, wcs, _):
            inputs.append((hdu.shape, wcs))
    return expand_grid(session, inputs, max_side)


def normalize(image: np.ndarray, reference: np.ndarray | None = None) -> tuple[np.ndarray, dict, float]:
    valid = np.isfinite(image)
    coverage = float(np.mean(valid))
    sample = (image if reference is None else reference).ravel()[::max(1, image.size // 1_000_000)]
    sample = sample[np.isfinite(sample)]
    if not len(sample):
        return np.zeros(image.shape, np.float32), dict.fromkeys(
            ("p01", "p50", "p99", "p999", "max"), 0.0), coverage
    background = float(sigma_clipped_stats(sample, maxiters=5)[1])
    image -= background
    positive = sample[sample > background] - background
    scale = float(np.percentile(positive, 99.5)) if len(positive) else 1.0
    image /= scale if scale > 0 else 1
    image[~valid] = 0
    np.clip(image, -65504, 65504, out=image)
    values = image[valid]
    percentiles = np.percentile(values, [1, 50, 99, 99.9])
    stats = dict(zip(("p01", "p50", "p99", "p999"), map(float, percentiles)))
    stats["max"] = float(values.max())
    stats["background"] = background
    stats["scale"] = scale if scale > 0 else 1
    return image, stats, coverage


def aligned_source(path, obs, target, shape, progress=noop):
    with science(path) as (hdu, wcs, weight):
        factor = max(1, math.floor(pixel_scale(target) / pixel_scale(wcs) + 1e-8))
        pixels, reference = prepare_science(hdu.data, weight, factor, return_reference=True,
                                            survey=surveys.is_survey(obs))
        if factor >= 2:
            if wcs.wcs.has_cd():
                matrix = wcs.wcs.cd.copy()
                del wcs.wcs.cd
                wcs.wcs.pc = matrix
                wcs.wcs.cdelt = [1, 1]
            wcs = wcs.slice((slice(None, None, factor),)*2)
        if surveys.is_survey(obs):
            image = pixels
        else:
            image, footprint = reproject_interp(
                (pixels, wcs), target, shape_out=shape, return_footprint=True,
                block_size=(512, 512), output_array=np.empty(shape, np.float32))
            image[footprint == 0] = np.nan
        if reference is not None:
            reference = reproject_interp(
                (reference, wcs), target, shape_out=shape, return_footprint=False,
                block_size=(512, 512), output_array=np.empty(shape, np.float32))
    check_cancelled(progress)
    return image, reference


def write_array(path: Path, array: np.ndarray, dtype: str) -> None:
    temporary = path.with_name(path.name + ".part")
    try:
        np.asarray(array, dtype=dtype).tofile(temporary)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


EDGE_VERSION = 2
# Interior blanks up to this area (project px) are bad pixels or saturated cores, not edges.
EDGE_HOLE_AREA = 4096


def fill_small_holes(covered: np.ndarray, max_area: float = EDGE_HOLE_AREA) -> np.ndarray:
    """Treat small uncovered islands inside coverage as covered so edge softening follows outer edges."""
    labels, count = ndi.label(~covered)
    if not count:
        return covered
    border = np.unique(np.concatenate((labels[0], labels[-1], labels[:, 0], labels[:, -1])))
    small = np.bincount(labels.ravel(), minlength=count + 1) <= max_area
    small[0] = False
    small[border] = False
    return covered | small[labels]


def edge_distance(covered: np.ndarray) -> np.ndarray:
    """Distance to uncovered pixels inside the grid; the canvas boundary is not an edge."""
    covered = fill_small_holes(covered)
    if np.all(covered):
        return np.full(covered.shape, 1024, dtype="<f2")
    return np.minimum(ndi.distance_transform_edt(covered), 1024).astype("<f2")


def ensure_edge_map(project: Path, meta: dict, layer: dict | None = None, *, force: bool = False) -> Path:
    """Atomically create an old build's cached edge map, shared by HTTP and CPU rendering."""
    key = (layer.get("key") or Path(layer["file"]).stem) if layer is not None else "luminance"
    name = layer.get("edge_file", f"{safe_key(key)}.edge.f16") if layer is not None else "luminance.edge.f16"
    if Path(name).name != name:
        raise ValueError("Unsafe edge filename.")
    path = project / name
    shape = (meta["height"], meta["width"])
    with _edge_locks[hash(str(path.resolve())) % len(_edge_locks)]:
        version = path.with_name(path.name + ".v")
        if (not force and path.is_file() and path.stat().st_size == math.prod(shape) * 2
                and version.is_file() and version.read_text().strip() == str(EDGE_VERSION)):
            return path
        covered = np.zeros(shape, bool)
        for source in [layer] if layer is not None else meta["layers"]:
            source_key = source.get("key") or Path(source["file"]).stem
            coverage = source.get("coverage_file", f"{safe_key(source_key)}.coverage.u8")
            if Path(coverage).name != coverage:
                raise ValueError("Unsafe coverage filename.")
            if (project / coverage).is_file():
                covered |= np.fromfile(project / coverage, dtype="u1").reshape(shape).astype(bool)
            else:
                # Pre-coverage builds cannot distinguish blank science from missing coverage.
                covered[:] = True
        write_array(path, edge_distance(covered), "<f2")
        version.write_text(str(EDGE_VERSION))
    return path


def downstream_signature(project: Path, layers: list[dict], grid_hash: str) -> str | None:
    """Version the ordered, quantized inputs to every post-alignment build product."""
    inputs = []
    for layer in layers:
        files = []
        for key in ("file", "coverage_file"):
            path = project / layer[key]
            if not path.is_file():
                return None
            stat = path.stat()
            files.append([layer[key], stat.st_mtime_ns, stat.st_size])
        inputs.append([layer["key"], layer["source_signature"], layer.get("wavelength_nm"), files])
    return hashlib.sha256(json.dumps(
        [BUILD_VERSION, EDGE_VERSION, grid_hash, inputs],
        sort_keys=True).encode()).hexdigest()


def downstream_products(project: Path, meta: dict) -> list | None:
    """Check both existence and size; the manifest also detects externally replaced products."""
    pixels = meta["width"] * meta["height"]
    names = {}
    for layer in [*meta["layers"], meta["luminance"]]:
        names[layer["edge_file"]] = pixels * 2
    names[meta["luminance"]["file"]] = pixels * 2
    result = []
    for name, size in sorted(names.items()):
        path = project / name
        if not name or not path.is_file():
            return None
        stat = path.stat()
        if size is not None and stat.st_size != size:
            return None
        result.append([name, stat.st_mtime_ns, stat.st_size])
    return result


def build(session: Path, *, downloads: Path = Path("data/downloads"), max_side: int = MAX_SIDE,
          progress: Progress = noop, expand: dict | None = None,
          checkpoint_name: str = "Before expand") -> dict:
    session = Path(session)
    project = session / "project"
    staging = session / f".build-{uuid.uuid4().hex}"
    backup = session / f".build-old-{uuid.uuid4().hex}"
    previous = json.loads((project / "meta.json").read_text(encoding="utf-8")) if (
        project / "meta.json").exists() else None
    reframed = capture_rotation_changed(session, previous)
    grid = grid_from_meta(expand) if expand else locked_grid(session)
    edits = read(session / "edits.json")
    markers = read(session / "markers.json")
    selected = set(json.loads((session / "selection.json").read_text(encoding="utf-8"))["obs_ids"])
    enabled = read(session / "session.json", {}).get("build", {}).get("mosaic", True)
    changed = previous and ({t.get("obs_id") for layer in previous["layers"]
                             for t in layer.get("tiles", [layer])} != selected
                            or previous.get("mosaic", False) != enabled
                            or reframed or (grid is not None and not same_grid(previous, grid)))
    try:
        check_cancelled(progress)
        if project.exists() and not changed:
            # Every write below is atomic replacement, so hard links safely reuse cached arrays.
            shutil.copytree(project, staging, copy_function=os.link)
        else:
            staging.mkdir()
        meta = _build(session, staging, downloads=downloads, max_side=max_side, progress=progress,
                      grid=grid)
        next_edits, next_markers = edits, markers
        if previous and edits and (expand or reframed or same_grid(previous, meta)):
            next_edits = remap_state(
                before_checkpoint(edits, "Before view rotation" if reframed else checkpoint_name)
                if expand or reframed else edits,
                previous, meta, geometry=bool(expand or reframed))
        if previous:
            next_edits = mosaic.remap_edits(next_edits, previous, meta)
        if (expand or reframed) and previous and markers is not None:
            next_markers = remap_markers(markers, previous, meta)
        if next_edits:
            from astroimp.edits import discard_star_state
            next_edits = discard_star_state(next_edits)
        check_cancelled(progress)
        archived = archive_current(session) if changed else None
        old = session / "builds" / archived / "project" if archived else backup
        if project.exists():
            project.rename(backup)
        try:
            staging.rename(project)
        except BaseException:
            if old.exists():
                old.rename(project)
            raise
        if grid is not None or reframed:
            write_json(session / "grid.json", grid_from_meta(meta))
        if next_edits != edits:
            write_json(session / "edits.json", next_edits)
            revision = read(session / "edits.rev.json", {}).get("revision", 0)
            write_json(session / "edits.rev.json", {"revision": revision+1, "source": "build",
                                                  "note": ("Rotated capture canvas" if reframed else
                                                           "Expanded canvas" if expand else "Updated build")})
        if next_markers != markers:
            write_json(session / "markers.json", next_markers)
        return meta
    finally:
        shutil.rmtree(staging, ignore_errors=True)
        shutil.rmtree(backup, ignore_errors=True)


def _build(session: Path, project: Path, *, downloads: Path, max_side: int,
           progress: Progress, grid: dict | None = None) -> dict:
    session = Path(session)
    catalog = json.loads((session / "catalog.json").read_text(encoding="utf-8"))
    enabled = read(session / "session.json", {}).get("build", {}).get("mosaic", True)
    selected = set(json.loads((session / "selection.json").read_text(encoding="utf-8"))["obs_ids"])
    observations = {o["obs_id"]: {**o, "proposal": o.get("proposal") or g.get("proposal")}
                    for g in catalog["groups"] for o in g["observations"]}
    if not selected or not selected <= observations.keys():
        raise ValueError("Select at least one catalog observation before building.")
    chosen = [observations[key] for key in sorted(selected)]
    previous_sources = {layer["obs_id"]: layer.get("survey_source") or "cds"
                        for layer in read(session / "project" / "meta.json", {}).get("layers", [])
                        if surveys.is_survey(layer)}
    for observation in chosen:
        if surveys.is_survey(observation):
            observation["fwhm_arcsec"] = surveys.band_for(observation).fwhm_arcsec
            if observation["obs_id"] in previous_sources:
                observation["survey_source"] = previous_sources[observation["obs_id"]]
    source_keys = [safe_key(o["obs_id"]) for o in chosen]
    if len(set(source_keys)) != len(source_keys):
        raise ValueError("Selected observation IDs have colliding safe filenames.")
    groups = mosaic.groups(chosen, enabled)
    keys = [mosaic.key(group) for group in groups]
    steps = [{"key": key, "label": f"Align {obs['filter']}", "state": "pending", "progress": 0}
             for key, obs in zip(keys, (group[0] for group in groups))]
    steps.append({"key": "write-layers", "label": "Write layers", "state": "pending", "progress": 0})
    report_details(progress, phase="download", steps=steps, eta_s=None)
    mast_chosen = [obs for obs in chosen if not surveys.is_survey(obs)]
    mast_sources = download_all(mast_chosen, Path(downloads), progress)
    report_details(progress, phase="align", eta_s=None)
    inputs = []
    for path in mast_sources:
        with science(path) as (hdu, wcs, _):
            inputs.append((hdu.shape, wcs))
    view = read(session / "view.json", {})
    captured_frame = (view.get("source") == "coordinates"
                      or float(view.get("rotation_deg", 0)) != 0
                      or capture_rotation_changed(session))
    if grid:
        target, shape = WCS(grid["wcs"]).celestial, (grid["height"], grid["width"])
        header = grid["wcs"]
    elif not inputs or captured_frame:
        scales = [pixel_scale(wcs)*3600 for _, wcs in inputs]
        scales.extend(surveys.band_for(obs).native_scale_arcsec
                      for obs in chosen if surveys.is_survey(obs))
        grid = surveys.view_grid(view, min(scales), max_side)
        target, shape = WCS(grid["wcs"]), (grid["height"], grid["width"])
        header = grid["wcs"]
    else:
        target, shape = optimal_grid(inputs, max_side)
        header = dict(target.to_header())
    source_by_id = {obs["obs_id"]: path for obs, path in zip(mast_chosen, mast_sources)}
    survey_chosen = [obs for obs in chosen if surveys.is_survey(obs)]
    survey_grid = {"wcs": header, "width": shape[1], "height": shape[0],
                   "pixel_scale_arcsec": pixel_scale(target)*3600}
    survey_sources = download_all(survey_chosen, Path(downloads), progress, grid=survey_grid)
    source_by_id.update({obs["obs_id"]: path for obs, path in zip(survey_chosen, survey_sources)})
    for observation, path in zip(survey_chosen, survey_sources):
        observation["survey_source"] = surveys.provenance(path)
    sources = [source_by_id[obs["obs_id"]] for obs in chosen]
    if hasattr(progress, "heavy"):
        progress.heavy()
    report_details(progress, phase="align", eta_s=None)
    grid_hash = hashlib.sha256(json.dumps(
        [BUILD_VERSION, shape, header, enabled, mosaic.VERSION], sort_keys=True).encode()).hexdigest()
    project.mkdir(exist_ok=True)
    old = {}
    if (project / "meta.json").exists():
        previous = json.loads((project / "meta.json").read_text(encoding="utf-8"))
        if previous.get("grid_hash") == grid_hash:
            old = {layer["key"]: layer for layer in previous["layers"]}
    reusable = len(old) == len(keys) and list(old) == keys
    def source_signature(group):
        entries = []
        for obs in group:
            path = source_by_id[obs["obs_id"]]
            stat = path.stat()
            entry = [str(path), stat.st_size, stat.st_mtime_ns]
            if surveys.is_survey(obs):
                entry.append(f"repair-{surveys.REPAIR_VERSION}")
            entries.append([obs["obs_id"], entry])
        return [enabled, mosaic.VERSION, entries]
    for key, group in zip(keys, groups):
        if old.get(key, {}).get("source_signature") != source_signature(group):
            reusable = False
    refresh_edges = False
    if reusable:
        layers = [{**old[key], **{k: obs.get(k) for k in (
            "obs_id", "filter", "collection", "instrument", "wavelength_nm", "info",
            "hips_id", "native_scale_arcsec", "fwhm_arcsec", "name", "survey_source")}}
                  for key, obs in zip(keys, (group[0] for group in groups))]
        signature = downstream_signature(project, layers, grid_hash)
        products = downstream_products(project, previous)
        if (signature is not None and previous.get("downstream_signature") == signature
                and products is not None and previous.get("downstream_products") == products):
            meta = {**previous, "layers": layers, "built_at": datetime.now(timezone.utc).isoformat(),
                    "view_rotation_deg": float(view.get("rotation_deg", 0))}
            write_json(project / "meta.json", meta)
            for item in steps:
                item.update(progress=1, state="done")
            report_details(progress, phase="done", steps=steps, eta_s=0)
            progress("publish", 1, "Reused unchanged aligned layers")
            return meta
        refresh_edges = previous.get("downstream_signature") != signature
    layers = []
    luminance = np.zeros(shape, np.float32)
    coverage_count = np.zeros(shape, np.uint16)
    timings = []
    def step(index, fraction, phase, eta=None):
        steps[index].update(progress=fraction, state="done" if fraction == 1 else "running")
        report_details(progress, phase=phase, steps=[dict(s) for s in steps], eta_s=eta)

    for index, (group, key) in enumerate(zip(groups, keys)):
        obs = group[0]
        path = source_by_id[obs["obs_id"]]
        started = time.monotonic()
        display_name = obs.get("name", path.name) if surveys.is_survey(obs) else path.name
        if len(group) > 1:
            display_name = f"{obs['filter']} mosaic ({len(group)} tiles)"
        eta = (float(np.mean(timings)) * math.prod(shape) * (len(groups) - index)
               if timings else None)
        step(index, 0, "align", eta)
        progress("reproject", index / len(groups), f"Aligning {display_name}")
        signature = source_signature(group)
        previous = old.get(key, {})
        output = project / f"{key}.f16"
        coverage_file = project / f"{key}.coverage.u8"
        cached = (previous.get("source_signature") == signature and output.exists()
                and output.stat().st_size == math.prod(shape) * 2
                and coverage_file.exists() and coverage_file.stat().st_size == math.prod(shape))
        if cached:
            image = load_plane(output, shape)
            valid = np.fromfile(coverage_file, dtype="u1").reshape(shape).astype(bool)
            layer = {**previous, **{k: obs.get(k) for k in (
                "obs_id", "filter", "collection", "instrument", "wavelength_nm", "info",
                "hips_id", "native_scale_arcsec", "fwhm_arcsec", "name", "survey_source")}}
            progress("reproject", (index + 1) / len(groups), f"Cached {display_name}")
        else:
            tiles = None
            if len(group) == 1:
                image, reference = aligned_source(path, obs, target, shape, progress)
            else:
                images, references = [], []
                for source in group:
                    check_cancelled(progress)
                    image, reference = aligned_source(
                        source_by_id[source["obs_id"]], source, target, shape, progress)
                    images.append(image)
                    references.append(reference)
                calibration = mosaic.calibrate(images)
                image = mosaic.combine(images, calibration)
                reference = (mosaic.combine(
                    [r if r is not None else p for p, r in zip(images, references)], calibration)
                    if any(r is not None for r in references) else None)
                tiles = [{**source, "key": safe_key(source["obs_id"]),
                          "source": str(source_by_id[source["obs_id"]]), **fit}
                         for source, fit in zip(group, calibration)]
                del images, references
            valid = np.isfinite(image)[::-1].copy()
            image, stats, coverage = normalize(image, reference)
            del reference
            # First and cached builds accumulate luminance from the same quantized pixels.
            image = image[::-1].astype("<f2").astype(np.float32)
            write_array(output, image, "<f2")
            write_array(coverage_file, valid, "u1")
            layer = {k: obs.get(k) for k in (
                "obs_id", "filter", "collection", "instrument", "wavelength_nm", "info",
                "hips_id", "native_scale_arcsec", "fwhm_arcsec", "name", "survey_source")}
            layer.update(key=key, file=output.name,
                         stats=stats, coverage=coverage, source=str(path),
                         source_signature=signature, coverage_file=coverage_file.name)
            if tiles:
                layer.update(tiles=tiles, feather_pixels=mosaic.FEATHER)
        layer["edge_file"] = ensure_edge_map(
            project, {"height": shape[0], "width": shape[1]}, layer,
            force=not cached or refresh_edges).name
        luminance += image
        coverage_count += valid
        layers.append(layer)
        del image, valid
        timings.append((time.monotonic() - started) / math.prod(shape))
        eta = float(np.mean(timings)) * math.prod(shape) * (len(groups) - index - 1)
        step(index, 1, "align", eta)
        progress("reproject", (index + 1) / len(groups), f"Aligned {display_name}")
    meta = {"version": 1, "build_version": BUILD_VERSION, "width": shape[1], "height": shape[0], "wcs": header,
            "pixel_scale_arcsec": grid["pixel_scale_arcsec"] if grid else pixel_scale(target) * 3600,
            "view_rotation_deg": float(view.get("rotation_deg", 0)),
            "grid_hash": grid_hash, "layers": layers, "mosaic": enabled,
            "luminance": {"file": "luminance.f16"}}
    if grid and grid.get("region_sky"):
        meta["region_sky"] = grid["region_sky"]
    step(len(groups), 0, "publish")
    check_cancelled(progress)
    np.divide(luminance, coverage_count, out=luminance, where=coverage_count > 0)
    write_array(project / "luminance.f16", luminance, "<f2")
    meta["built_at"] = datetime.now(timezone.utc).isoformat()
    meta["luminance"]["edge_file"] = ensure_edge_map(project, meta, force=True).name
    check_cancelled(progress)
    meta["downstream_signature"] = downstream_signature(project, layers, meta["grid_hash"])
    meta["downstream_products"] = downstream_products(project, meta)
    write_json(project / "meta.json", meta)
    step(len(groups), 1, "done", 0)
    progress("publish", 1, f"Built {len(layers)} layers")
    return meta
