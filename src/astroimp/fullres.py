"""Native-resolution, crop-local exports; all large intermediates live on disk."""
from __future__ import annotations

import copy
import ctypes
import functools
import gc
import json
import math
import os
import re
import shutil
import struct
import time
import uuid
import zlib
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
from astropy.io import fits as astrofits
from astropy.wcs import WCS
from PIL import Image
from scipy import ndimage as ndi

from astroimp.build import (EDGE_HOLE_AREA, fill_small_holes, block_mean, prepare_science, noop, pixel_scale,
                           report_details, science)
from astroimp.mast import write_json
from astroimp.cancellation import check_cancelled
from astroimp.render import brush_maps, edge_noise, output, render
from astroimp.edits import normalise
from astroimp.view import inside_crop, view_frame
from reproject import reproject_interp

TILE = 768


def linear_wcs(wcs: WCS) -> WCS:
    wcs = wcs.deepcopy()
    if wcs.wcs.has_cd():
        # Astropy slicing scales CDELT, which wcslib ignores while CD is present.
        matrix = wcs.wcs.cd.copy()
        del wcs.wcs.cd
        wcs.wcs.pc = matrix
        wcs.wcs.cdelt = [1, 1]
    return wcs


def subgrid(wcs: WCS, build_height: int, origin: tuple[int, int],
            shape: tuple[int, int], factor: float) -> WCS:
    """Top-down array rectangle to a FITS (bottom-up) WCS, preserving pixel edges."""
    result = linear_wcs(wcs)
    x, y = origin
    start = np.array([x / factor, build_height - (y + shape[0]) / factor])
    result.wcs.crpix = (result.wcs.crpix - start - .5) * factor + .5
    result.wcs.cdelt /= factor
    return result


def _inverse(points, width, height, transform):
    p = np.asarray(points, np.float64) - [width * (.5 + transform.get("x", 0)),
                                            height * (.5 + transform.get("y", 0))]
    angle = math.radians(transform.get("rotation", 0))
    c, s = math.cos(angle), math.sin(angle)
    return (p @ np.array([[c, -s], [s, c]]) / max(.001, transform.get("scale", 1))
            + [width / 2, height / 2])


def view_points(meta, state, x, y):
    """Current theta-space output coordinates to source pixels (flip after inverse rotation)."""
    w, h = meta["width"], meta["height"]
    g = state["global"]
    angle = math.radians(g.get("rotation", 0))
    c, s = math.cos(angle), math.sin(angle)
    px, py = (x - .5) * w, (y - .5) * h
    qx, qy = c * px + s * py, -s * px + c * py
    if g.get("flipX"):
        qx = -qx
    return qx + w / 2, qy + h / 2


def required_layers(meta, state):
    keys = set()
    all_keys = {l["key"] for l in meta["layers"]}
    for layer in state["layers"]:
        if not layer.get("visible", True):
            continue
        keys.add(layer["source"])
        if (layer.get("mask") or {}).get("source"):
            keys.add(layer["mask"]["source"])
        if layer["mode"] == "spectral":
            keys |= all_keys
    if keys - all_keys:
        keys |= all_keys  # Luminance is the coverage-weighted mean of every source.
    return [l for l in meta["layers"] if l["key"] in keys]


def plan(session: Path, scale_factor: float | None = None, state: dict | None = None,
         long_side: int | None = None) -> dict:
    session = Path(session)
    meta = json.loads((session / "project" / "meta.json").read_text(encoding="utf-8"))
    state = state or json.loads((session / "edits.json").read_text(encoding="utf-8"))
    needed = required_layers(meta, state)
    visible = {l["source"] for l in state["layers"] if l.get("visible", True)}
    combined = bool(visible - {l["key"] for l in meta["layers"]}) or any(
        l.get("visible", True) and l["mode"] == "spectral" for l in state["layers"])
    scales = []
    for layer in needed:
        if layer["key"] in visible or combined:
            from astroimp import surveys
            if surveys.is_survey(layer):
                scales.append(surveys.band_for(layer).native_scale_arcsec / 3600)
            else:
                for tile in layer.get("tiles", [layer]):
                    with science(Path(tile["source"])) as (_, wcs, _):
                        scales.append(pixel_scale(wcs))
    if not scales:
        raise ValueError("Make at least one source layer visible before exporting.")
    native = max(1., pixel_scale(WCS(meta["wcs"])) / min(scales))
    crop = view_frame(meta, state["global"])
    preview = [max(1, math.floor(crop[k] + .5)) for k in ("width", "height")]
    factor = native if scale_factor is None else float(scale_factor)
    if long_side is not None:
        if scale_factor is not None:
            raise ValueError("Supply long_side or scale_factor, not both.")
        if not 1 <= long_side <= math.floor(max(preview)*native+.5):
            raise ValueError("long_side must be positive and no larger than native.")
        factor = min(native, long_side / max(preview))
    elif not math.isfinite(factor) or not 1 <= factor <= native * (1 + 1e-7):
        raise ValueError(f"scale_factor must be between 1 and native ({native:.6g}).")
    size = [max(1, math.floor(v * factor + .5)) for v in preview]
    x = np.array([crop["x"], crop["x"] + crop["w"]] * 2)
    y = np.repeat([crop["y"], crop["y"] + crop["h"]], 2)
    qx, qy = view_points(meta, state, x, y)
    corners = np.stack([qx, qy], -1)
    regions = [corners]
    for layer in state["layers"]:
        if layer.get("visible", True):
            regions.append(_inverse(corners, meta["width"], meta["height"], layer.get("transform") or {}))
    points = np.concatenate(regions)
    padding = max([8., *[(l.get("edge") or {}).get("feather", 0) * 1.8 + 2
                         for l in state["layers"] if l.get("visible", True)]])
    padding = max(padding, max((l.get("feather_pixels", 64)+2 for l in needed if l.get("tiles")), default=0))
    for layer in state["layers"]:
        detail = layer.get("detail") or {}
        if not layer.get("visible", True) or detail.get("amount", 0) <= 0:
            continue
        transform = layer.get("transform") or {}
        angle = math.radians(transform.get("rotation", 0))
        # A canvas-space Gaussian halo can sample farther away in a scaled/rotated source.
        reach = max(1, (abs(math.cos(angle))+abs(math.sin(angle))) / max(.001, transform.get("scale", 1)))
        padding = max(padding, 4 * min(64, max(1, detail.get("radius", 8))) * reach + 2)
    # Include complete build pixels and enough source context for edge fades.
    lo = np.maximum(0, np.floor(points.min(0) - padding))
    hi = np.minimum([meta["width"], meta["height"]], np.ceil(points.max(0) + padding))
    if np.any(hi <= lo):
        raise ValueError("The cropped view is outside the image.")
    origin = np.floor(lo * factor).astype(int)
    end = np.ceil(hi * factor).astype(int)
    fixed, scaled = estimate_parts(needed, int(math.prod(end-origin)))
    calibration = estimate_calibration(session.parent)
    return {"factor": factor, "native_factor": native, "width": size[0], "height": size[1],
            "preview_width": preview[0], "preview_height": preview[1],
            "estimated_s": (fixed + scaled) * calibration,
            # Source preparation does not shrink with the output; the dialog rescales only `scaled`.
            "estimate": {"fixed_s": fixed * calibration, "scaled_s": scaled * calibration,
                         "model_s": fixed + scaled, "calibration": calibration},
            "origin": origin.tolist(), "shape": (end - origin)[::-1].tolist(),
            "layers": [l["key"] for l in needed]}


# NNLS fit over 16 recorded exports (leave-one-out error 33% vs 49% for pixels × layers alone).
ESTIMATE_LAYER_MP_S = .655
ESTIMATE_SOURCE_MP_S = .122
ESTIMATE_TILE_S = 5.6
CALIBRATION_HISTORY = 8


@functools.lru_cache(maxsize=512)
def _source_pixels(path: str, mtime: float) -> int:
    with astrofits.open(path, memmap=True) as hdus:
        hdu = next((h for h in hdus if h.name == "SCI"), None) or next(
            (h for h in hdus if h.header.get("NAXIS") == 2), None)
        return int(hdu.header["NAXIS1"] * hdu.header["NAXIS2"]) if hdu is not None else 0


def source_pixels(layer: dict) -> int:
    from astroimp import surveys
    if surveys.is_survey(layer):
        return 0
    total = 0
    for tile in layer.get("tiles") or [layer]:
        path = Path(tile["source"])
        try:
            total += _source_pixels(str(path), path.stat().st_mtime)
        except (OSError, KeyError, ValueError):
            pass
    return total


def estimate_parts(layers: list[dict], work_pixels: int) -> tuple[float, float]:
    """(fixed, scaled) seconds; `scaled` grows with output area, `fixed` is source-resolution work."""
    scaled = work_pixels / 1e6 * len(layers) * ESTIMATE_LAYER_MP_S
    fixed = sum(source_pixels(l) / 1e6 * ESTIMATE_SOURCE_MP_S
                + len(l.get("tiles") or []) * ESTIMATE_TILE_S for l in layers)
    return float(fixed), float(scaled)


def estimate_calibration(sessions: Path) -> float:
    """Median actual/model ratio of this machine's recent exports."""
    records = []
    for path in sessions.glob("*/exports/*.json"):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
            model, wall = float(data["estimate"]["model_s"]), float(data["wall_s"])
        except (OSError, ValueError, KeyError, TypeError):
            continue
        if model > 0 and wall > 0:
            records.append((path.stat().st_mtime, wall / model))
    if not records:
        return 1.
    ratios = [ratio for _, ratio in sorted(records)[-CALIBRATION_HISTORY:]]
    return float(np.clip(np.median(ratios), .5, 4))


def fit_tone(raw: np.ndarray, preview: np.ndarray, valid: np.ndarray | None = None) -> tuple[float, float]:
    """Robust raw = offset + scale * normalized fit, excluding invalid pixels and mismatches."""
    ok = np.isfinite(raw) & np.isfinite(preview)
    if valid is not None:
        ok &= valid
    x, y = preview[ok].astype(np.float64), raw[ok].astype(np.float64)
    if len(x) < 8 or np.ptp(x) < 1e-8:
        raise ValueError("Insufficient overlapping tonal range to match the preview.")
    # Deterministic subsampling bounds the least-squares allocation.
    stride = max(1, len(x) // 200_000)
    x, y = x[::stride], y[::stride]
    lo, hi = np.percentile(x, [2, 95])
    keep = (x >= lo) & (x <= hi)
    for _ in range(6):
        design = np.column_stack([np.ones(keep.sum()), x[keep]])
        a, b = np.linalg.lstsq(design, y[keep], rcond=None)[0]
        residual = y - (a + b * x)
        median = np.median(residual[keep])
        sigma = max(1.4826 * np.median(np.abs(residual[keep] - median)), np.ptp(y) * 1e-7, 1e-12)
        new = (x >= lo) & (x <= hi) & (np.abs(residual - median) < 3 * sigma)
        if new.sum() < 8:
            break
        keep = new
    if not np.isfinite(b) or b <= 0:
        raise ValueError("The source cannot be tone-matched to the preview.")
    return float(a), float(b)


def area_preview(raw, origin, factor, build_shape):
    """Fractional-area block means aligned to build pixel edges, without a full integral image."""
    bh, bw = build_shape
    ox, oy = origin
    h, w = raw.shape
    x0, y0 = max(0, math.ceil(ox / factor)), max(0, math.ceil(oy / factor))
    x1, y1 = min(bw, math.floor((ox + w) / factor)), min(bh, math.floor((oy + h) / factor))
    result = np.full((bh, bw), np.nan, np.float32)
    for by in range(y0, y1, 192):
        for bx in range(x0, x1, 192):
            ex, ey = min(x1, bx + 192), min(y1, by + 192)
            xs = np.arange(bx, ex + 1) * factor - ox
            ys = np.arange(by, ey + 1) * factor - oy
            sx, sy = math.floor(xs[0]), math.floor(ys[0])
            a = np.asarray(raw[sy:math.ceil(ys[-1]), sx:math.ceil(xs[-1])], np.float32)
            valid = np.isfinite(a)
            def integral(values):
                table = np.pad(values.astype(np.float64).cumsum(0).cumsum(1), ((1, 0), (1, 0)))
                yy, xx = np.meshgrid(ys - sy, xs - sx, indexing="ij")
                v = ndi.map_coordinates(table, [yy, xx], order=1, mode="nearest")
                return np.diff(np.diff(v, axis=0), axis=1)
            sums, counts = integral(np.where(valid, a, 0)), integral(valid)
            result[by:ey, bx:ex] = np.where(counts > factor * factor * .98,
                                                           sums / np.maximum(counts, 1e-12), np.nan)
    return result


def _tiles(shape, size=None):
    size = size or TILE
    for y in range(0, shape[0], size):
        for x in range(0, shape[1], size):
            yield y, min(y + size, shape[0]), x, min(x + size, shape[1])


def reproject_image(pixels, source_wcs, target, destination, progress=noop, *,
                    workers=8, block_size=512):
    """Bounded parallel WCS blocks, written directly to the export's disk map."""
    blocks = iter(_tiles(destination.shape, block_size))

    def compute(bounds):
        y, ey, x, ex = bounds
        tile = np.empty((ey-y, ex-x), np.float32)
        reproject_interp((pixels, source_wcs), target.slice((slice(y,ey), slice(x,ex))),
                         shape_out=tile.shape, output_array=tile, return_footprint=False)
        return tile

    with ThreadPoolExecutor(max_workers=workers) as executor:
        pending = {}
        for bounds in blocks:
            pending[executor.submit(compute, bounds)] = bounds
            if len(pending) == workers:
                break
        while pending:
            done, _ = wait(pending, return_when=FIRST_COMPLETED)
            for future in done:
                check_cancelled(progress)
                y, ey, x, ex = pending.pop(future)
                destination[y:ey,x:ex] = future.result()
                bounds = next(blocks, None)
                if bounds is not None:
                    pending[executor.submit(compute, bounds)] = bounds


def _map(folder, name, shape, dtype="<f2", mode="w+"):
    return np.memmap(folder / name, dtype=dtype, mode=mode, shape=tuple(shape))


def _close(array):
    if isinstance(array, np.memmap) and not array._mmap.closed:
        if array.mode != "r":
            array.flush()
        array._mmap.close()


def _sample(image, y, x, order=1):
    """SciPy cannot interpolate float16; convert only the tile's sampled rectangle."""
    h, w = image.shape
    x = np.clip(x, 0, w-1)
    y = np.clip(y, 0, h-1)
    x0, x1 = max(0, math.floor(x.min())), min(w, math.ceil(x.max())+2)
    y0, y1 = max(0, math.floor(y.min())), min(h, math.ceil(y.max())+2)
    return ndi.map_coordinates(np.asarray(image[y0:y1,x0:x1], np.float32),
                               [y-y0,x-x0], order=order, mode="nearest")


class TileProject:
    """The ordinary compositor, sampling on-disk subgrids in full-canvas coordinates."""
    def __init__(self, folder, meta, info):
        self.meta, self.info, self.folder = meta, info, folder
        self.keys = [l["key"] for l in meta["layers"]]
        self.full = []
        self.lum = None
        self.edges = {}
        self.detail_scale = info["factor"]
        self.brushes = {}

    def tile(self, y, ey, x, ex, *, halo=0):
        folder, shape = self.folder, self.info["shape"]
        self.output_bounds = (y,ey,x,ex)
        y,ey,x,ex = max(0,y-halo),min(shape[0],ey+halo),max(0,x-halo),min(shape[1],ex+halo)
        self.full = [_map(folder, f"{k}.full", shape, mode="r") for k in self.keys]
        self.lum = _map(folder, "lum.full", shape, mode="r")
        self.edges = {k: _map(folder, f"{k}.edge", shape, mode="r") for k in [*self.keys, "lum"]}
        self.shape = (ey-y, ex-x)
        self.yy, self.xx = np.mgrid[y:ey, x:ex].astype(np.float32)
        self.xx += self.info["origin"][0] + .5
        self.yy += self.info["origin"][1] + .5
        self.coords = {}
        self.samples = {}
        self.tile_bounds = (y, ey, x, ex)

    def coordinates(self, transform):
        key = tuple(transform.get(k, d) for k, d in (("x", 0), ("y", 0), ("scale", 1), ("rotation", 0)))
        if key not in self.coords:
            f = self.info["factor"]
            w, h = self.meta["width"] * f, self.meta["height"] * f
            px, py = self.xx - w * (.5 + key[0]), self.yy - h * (.5 + key[1])
            c, s = math.cos(math.radians(key[3])), math.sin(math.radians(key[3]))
            qx = (c * px + s * py) / max(.001, key[2]) + w / 2
            qy = (-s * px + c * py) / max(.001, key[2]) + h / 2
            inside = (qx >= 0) & (qx <= w) & (qy >= 0) & (qy <= h)
            self.coords[key] = (qx, qy, inside)
        return self.coords[key]

    def warp(self, image, transform, order=1):
        key = (id(image), tuple(transform.get(k, d) for k, d in
                               (("x", 0), ("y", 0), ("scale", 1), ("rotation", 0))), order)
        if key not in self.samples:
            qx, qy, inside = self.coordinates(transform)
            if key[1] == (0, 0, 1, 0):
                y, ey, x, ex = self.tile_bounds
                value = np.asarray(image[y:ey, x:ex], np.float32)
            else:
                ox, oy = self.info["origin"]
                value = _sample(image, qy-oy-.5, qx-ox-.5, order)
            self.samples[key] = value if np.all(inside) else np.where(inside, value, 0)
        return self.samples[key]

    def edge_fade(self, layer):
        qx, qy, inside = self.coordinates(layer.get("transform") or {})
        ox, oy = self.info["origin"]
        distance = self.edges.get(layer["source"], self.edges["lum"])
        dist = _sample(distance, qy-oy-.5, qx-ox-.5)
        feather = layer["edge"]["feather"] * self.info["factor"]
        noise = edge_noise(qx / max(feather, self.info["factor"]), qy / max(feather, self.info["factor"]))
        effective = feather * (1 + layer["edge"].get("irregularity", 0) * (noise-.5) * np.float32(1.6))
        t = np.clip(dist / effective, 0, 1)
        return np.where(inside, t*t*(3-2*t), 0)

    def brush_data(self, layer):
        key = json.dumps(layer["brush"], sort_keys=True)
        if key not in self.brushes:
            self.brushes[key] = brush_maps(self.meta["width"], self.meta["height"],
                                            layer["brush"]["strokes"]).astype("<f2").astype(np.float32)
        maps = self.brushes[key]
        qx, qy, inside = self.coordinates(layer.get("transform") or {})
        f = self.info["factor"]
        coordinates = [qy/(self.meta["height"]*f)*maps.shape[1]-.5,
                       qx/(self.meta["width"]*f)*maps.shape[2]-.5]
        sampled = [ndi.map_coordinates(m, coordinates, order=1, mode="nearest") for m in maps]
        sampled[1] = np.where(inside, sampled[1], 1)
        sampled[2] = np.where(inside, sampled[2], 0)
        return np.stack(sampled)

    def brush_fade(self, layer):
        return self.brush_data(layer)[0]

    def close(self):
        for a in [*self.full, self.lum, *self.edges.values()]:
            _close(a)


def peak_rss_bytes():
    if os.name == "nt":
        from ctypes import wintypes
        class Counters(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                        *[(name, ctypes.c_size_t) for name in ("PeakWorkingSetSize", "WorkingSetSize",
                          "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage",
                          "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage")]]
        counters = Counters()
        counters.cb = ctypes.sizeof(counters)
        ctypes.windll.kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        measure = ctypes.windll.psapi.GetProcessMemoryInfo
        measure.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
        measure.restype = wintypes.BOOL
        handle = ctypes.windll.kernel32.GetCurrentProcess()
        if not measure(handle, ctypes.byref(counters), counters.cb):
            raise ctypes.WinError()
        return int(counters.PeakWorkingSetSize)
    import resource
    return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024)


def write_png16(path, rgb, progress=noop):
    """Stream RGB16 PNG directly from uint16 pixels; Pillow's RGB path is only 8-bit."""
    def chunk(stream, kind, data):
        stream.write(struct.pack(">I", len(data)) + kind + data)
        stream.write(struct.pack(">I", zlib.crc32(kind+data) & 0xffffffff))
    with Path(path).open("wb") as stream:
        stream.write(b"\x89PNG\r\n\x1a\n")
        chunk(stream, b"IHDR", struct.pack(">IIBBBBB", rgb.shape[1], rgb.shape[0], 16, 2, 0, 0, 0))
        compressor = zlib.compressobj()
        pending = bytearray()
        for row in rgb:
            check_cancelled(progress)
            pending.extend(compressor.compress(b"\0" + row.astype(">u2").tobytes()))
            if len(pending) >= 1024*1024:
                chunk(stream, b"IDAT", bytes(pending))
                pending.clear()
        pending.extend(compressor.flush())
        if pending:
            chunk(stream, b"IDAT", bytes(pending))
        chunk(stream, b"IEND", b"")


def export_path(directory: Path, name: str, width: int, height: int, extension: str) -> Path:
    stem = re.sub(r"[^\w.-]+", "-", name).strip("-.")[:80] or "nightbloom"
    stem = f"{stem}-{width}x{height}"
    destination, n = directory / f"{stem}.{extension}", 2
    while destination.exists():
        destination, n = directory / f"{stem}-{n}.{extension}", n + 1
    return destination


def reproject_mosaic(layer, target, destination, work, progress=noop, *, scale=1):
    """Apply build calibration unchanged; feather distances scale with output pixels."""
    from astroimp.mosaic import feather_weight, FEATHER
    shape = destination.shape
    total = _map(work, "mosaic.total", shape, "<f4")
    weights = _map(work, "mosaic.weights", shape, "<f4")
    tile_image = _map(work, "mosaic.tile", shape, "<f4")
    total[:] = 0
    weights[:] = 0
    try:
        for tile in layer["tiles"]:
            check_cancelled(progress)
            with science(Path(tile["source"])) as (hdu, wcs, weight):
                block = max(1, math.floor(pixel_scale(target) / pixel_scale(wcs) + 1e-8))
                pixels = prepare_science(hdu.data, weight, block)
                if block > 1:
                    wcs = linear_wcs(wcs).slice((slice(None, None, block),)*2)
                reproject_image(pixels, wcs, target, tile_image, progress)
                del pixels
            local = feather_weight(np.isfinite(tile_image), layer.get("feather_pixels", FEATHER)*scale)
            for y, ey, x, ex in _tiles(shape):
                check_cancelled(progress)
                section = np.s_[y:ey, x:ex]
                values = tile_image[section]
                total[section] += np.where(np.isfinite(values),
                    values*tile["gain"] + tile["offset"], 0)*local[section]
                weights[section] += local[section]
            del local
        for y, ey, x, ex in _tiles(shape):
            section = np.s_[y:ey, x:ex]
            np.divide(total[section], weights[section], out=destination[section],
                      where=weights[section] > 0)
            destination[section][weights[section] == 0] = np.nan
    finally:
        for array in (total, weights, tile_image):
            _close(array)
        for name in ("mosaic.total", "mosaic.weights", "mosaic.tile"):
            (work / name).unlink(missing_ok=True)


def export_full(session: Path, *, scale_factor: float | None = None, format: str = "png",
                progress=noop, state: dict | None = None, long_side: int | None = None,
                quality: int = 92, name: str = "nightbloom") -> dict:
    format = "png8" if format == "png" else format
    if format not in {"png8", "png16", "tiff16", "jpeg"}:
        raise ValueError("format must be png8, png16, tiff16 or jpeg.")
    if not 1 <= quality <= 100:
        raise ValueError("JPEG quality must be between 1 and 100.")
    started = time.monotonic()
    session = Path(session)
    state = copy.deepcopy(state or json.loads((session / "edits.json").read_text(encoding="utf-8")))
    meta = json.loads((session / "project" / "meta.json").read_text(encoding="utf-8"))
    export_state = copy.deepcopy(state)
    state = normalise(state)
    info = plan(session, scale_factor, state, long_side)
    shape, f, origin = info["shape"], info["factor"], info["origin"]
    target = subgrid(WCS(meta["wcs"]), meta["height"], origin, shape, f)
    needed = [l for l in meta["layers"] if l["key"] in info["layers"]]
    source_paths = {}
    from astroimp import surveys
    for layer in needed:
        check_cancelled(progress)
        source_path = Path(layer["source"])
        if surveys.is_survey(layer):
            native_scale = surveys.band_for(layer).native_scale_arcsec
            if native_scale < meta["pixel_scale_arcsec"] * (1 - 1e-6):
                ratio = min(1., pixel_scale(target)*3600/native_scale)
                survey_wcs = linear_wcs(target)
                survey_wcs.wcs.crpix = (survey_wcs.wcs.crpix-.5)*ratio+.5
                survey_wcs.wcs.cdelt /= ratio
                survey_shape = np.maximum(1, np.ceil(np.array(shape)*ratio)).astype(int)
                source_path = surveys.fetch(
                    {**layer, "survey_source": layer.get("survey_source") or surveys.provenance(source_path)},
                    session.parent.parent / "downloads", {
                    "wcs": dict(survey_wcs.to_header()), "height": int(survey_shape[0]),
                    "width": int(survey_shape[1]),
                    "pixel_scale_arcsec": pixel_scale(survey_wcs)*3600}, progress)
        source_paths[layer["key"]] = source_path
    queued = time.monotonic()
    if hasattr(progress, "heavy"):
        progress.heavy("render")
    render_started = time.monotonic()
    queued = render_started - queued
    work = session / f".fullres-{uuid.uuid4().hex}"
    work.mkdir()
    maps = []
    renderer = None
    destination = None
    def mapped(name, shape, dtype="<f2", mode="w+", folder=work):
        array = _map(folder, name, shape, dtype, mode)
        maps.append(array)
        return array
    steps = [{"key": l["key"], "label": f"Export {l.get('filter', l['key'])}",
              "state": "pending", "progress": 0} for l in needed]
    steps += [{"key": k, "label": k.title(), "state": "pending", "progress": 0} for k in ("render", "write")]
    def report(index, fraction, stage, message):
        done = (index + fraction) / len(steps)
        elapsed = time.monotonic() - render_started
        steps[index].update(state="done" if fraction == 1 else "running", progress=fraction)
        report_details(progress, phase=stage, steps=copy.deepcopy(steps),
                       eta_s=elapsed*(1-done)/done if done > 0 else None)
        progress(stage, done, message)
    try:
        check_cancelled(progress)
        lum = mapped("lum.full", shape, "<f4")
        count = mapped("count", shape, "<u2")
        fits = []
        for index, layer in enumerate(needed):
            key = layer["key"]
            report(index, 0, "reproject", f"Reprojecting {layer.get('filter', key)}")
            raw = mapped("raw", shape, "<f4")
            try:
                source_path = source_paths[key]
                if layer.get("tiles"):
                    reproject_mosaic(layer, target, raw, work, progress, scale=f)
                else:
                    with science(source_path) as (hdu, source_wcs, weight):
                        block = max(1, math.floor(pixel_scale(target) / pixel_scale(source_wcs) + 1e-8))
                        pixels = prepare_science(hdu.data, weight, block, survey=surveys.is_survey(layer))
                        if block > 1:
                            source_wcs = linear_wcs(source_wcs).slice((slice(None, None, block),)*2)
                        reproject_image(pixels, source_wcs, target, raw, progress)
                        check_cancelled(progress)
                        del pixels
                    del hdu, source_wcs, weight
                # Reprojection is bottom-up; swap rows in place using bounded buffers.
                for y in range((shape[0]+1)//2):
                    check_cancelled(progress)
                    other = shape[0]-y-1
                    row = raw[y].copy()
                    raw[y] = raw[other]
                    raw[other] = row
                small = area_preview(raw, origin, f, (meta["height"], meta["width"]))
                preview = mapped(layer["file"], small.shape, mode="r", folder=session / "project")
                valid_fit = np.ones(small.shape, bool)
                coverage_name = layer.get("coverage_file")
                old_coverage = None
                if coverage_name:
                    old_coverage = mapped(coverage_name, small.shape, "u1", "r", folder=session / "project")
                    valid_fit &= np.asarray(old_coverage) > 0
                if layer.get("tiles") and "background" in layer["stats"]:
                    a, b = layer["stats"]["background"], layer["stats"]["scale"]
                    fits.append({"key": key, "offset": a, "scale": b})
                elif np.isfinite(small).any():
                    a, b = fit_tone(small, preview, valid_fit)
                    fits.append({"key": key, "offset": a, "scale": b})
                else:
                    a, b = 0., 1.
                    fits.append({"key": key, "offset": a, "scale": b, "no_overlap": True})
                for m in (preview, old_coverage):
                    _close(m)
                del small, valid_fit
                full = mapped(f"{key}.full", shape)
                covered = mapped(f"{key}.coverage", shape, "u1")
                for y, ey, x, ex in _tiles(shape):
                    check_cancelled(progress)
                    tile = raw[y:ey, x:ex]
                    valid = np.isfinite(tile)
                    normalized = np.where(valid, np.clip((tile-a)/b, -65504, 65504), 0).astype("<f2")
                    full[y:ey, x:ex] = normalized
                    covered[y:ey, x:ex] = valid
                    lum[y:ey, x:ex] += normalized.astype(np.float32)
                    count[y:ey, x:ex] += valid
            finally:
                _close(raw)
                (work / "raw").unlink(missing_ok=True)
            report(index, .6, "edges", f"Preparing edges: {layer.get('filter', key)}")
            # Exact EDT is sequential per source, bounded to one float64 work image.
            edge = mapped(f"{key}.edge", shape)
            interior = fill_small_holes(np.asarray(covered, bool), EDGE_HOLE_AREA * f * f)
            if np.all(interior):
                edge[:] = 1024 * f
            else:
                distances = ndi.distance_transform_edt(interior)
                for y, ey, x, ex in _tiles(shape):
                    check_cancelled(progress)
                    edge[y:ey,x:ex] = np.minimum(distances[y:ey,x:ex], 1024*f)
                del distances
            del interior
            for m in (full, covered, edge):
                _close(m)
            gc.collect()
            report(index, 1, "edges", f"Prepared {layer.get('filter', key)}")
        for plane, acc in [("lum.full", lum)]:
            temporary = mapped("quantized", shape)
            for y, ey, x, ex in _tiles(shape):
                check_cancelled(progress)
                temporary[y:ey,x:ex] = acc[y:ey,x:ex] / np.maximum(count[y:ey,x:ex], 1)
            _close(temporary)
            _close(acc)
            (work / plane).unlink()
            (work / "quantized").replace(work / plane)
        edge = mapped("lum.edge", shape)
        lum_covered = fill_small_holes(count > 0, EDGE_HOLE_AREA * f * f)
        distances = ndi.distance_transform_edt(lum_covered) if not np.all(lum_covered) else None
        for y, ey, x, ex in _tiles(shape):
            check_cancelled(progress)
            edge[y:ey,x:ex] = np.minimum(distances[y:ey,x:ex], 1024*f) if distances is not None else 1024*f
        del distances
        _close(edge)
        local_meta = {**meta, "layers": needed}
        renderer = TileProject(work, local_meta, info)
        accumulation = mapped("acc", (*shape, 3), "<f4")
        tiles = list(_tiles(shape))
        for i, tile in enumerate(tiles):
            check_cancelled(progress)
            halo = max([0, *[math.ceil(4 * (l.get("detail") or {}).get("radius", 8) * f) + 1
                            for l in state["layers"] if l.get("visible", True)
                            and (l.get("detail") or {}).get("amount", 0) > 0]])
            renderer.tile(*tile, halo=halo)
            rgb, _ = render(renderer, state, collect=False)
            renderer.close()
            y, ey, x, ex = tile
            sy,sey,sx,sex = renderer.tile_bounds
            accumulation[y:ey,x:ex] = rgb[y-sy:ey-sy,x-sx:ex-sx]
            report(len(needed), (i+1)/len(tiles), "render", f"Rendering tile {i+1}/{len(tiles)}")
        renderer.close()
        renderer = None
        sixteen = format in {"png16", "tiff16"}
        dtype = "<u2" if sixteen else "u1"
        final = mapped("final", (info["height"], info["width"], 3), dtype)
        g = state["global"]
        crop = view_frame(meta, g)
        tiles = list(_tiles(final.shape[:2]))
        for i, (y, ey, x, ex) in enumerate(tiles):
            check_cancelled(progress)
            yy, xx = np.mgrid[y:ey,x:ex].astype(np.float64)
            ox = crop["x"]+(xx+.5)/info["width"]*crop["w"]
            oy = crop["y"]+(yy+.5)/info["height"]*crop["h"]
            qx, qy = view_points(meta, state, ox, oy)
            sampled = np.stack([ndi.map_coordinates(accumulation[...,c],
                        [qy*f-origin[1]-.5, qx*f-origin[0]-.5], order=1, mode="nearest")
                               for c in range(3)], -1)
            rgb = output(sampled, g)
            inside = ((qx>=0)&(qx<=meta["width"])&(qy>=0)&(qy<=meta["height"])
                      & inside_crop(meta, g, ox, oy))
            rgb[~inside] = [.0784,.0824,.0902]
            final[y:ey,x:ex] = np.round(rgb*(65535 if sixteen else 255)).astype(dtype)
            report(len(needed)+1, .7*(i+1)/len(tiles), "write", "Transforming cropped frame")
        directory = session / "exports"
        directory.mkdir(exist_ok=True)
        extension = {"png8":"png","png16":"png","tiff16":"tiff","jpeg":"jpg"}[format]
        destination = export_path(directory, name, info["width"], info["height"], extension)
        destination.touch()
        if format == "tiff16":
            import tifffile
            tifffile.imwrite(destination, final, photometric="rgb")
        elif format == "png16":
            write_png16(destination, final, progress)
        elif format == "jpeg":
            Image.fromarray(np.asarray(final)).save(destination, quality=quality, subsampling=0)
        else:
            Image.fromarray(np.asarray(final)).save(destination)
        report(len(needed)+1, 1, "write", f"Saved {destination.name}")
        from astroimp.export_history import capture
        checkpoint = capture(session, destination, export_state, meta)
        check_cancelled(progress)
        result = {**info, "path": str(destination.resolve()), "format": format, "tone_fits": fits,
                  "checkpoint": checkpoint}
    except BaseException:
        if destination:
            from astroimp.export_history import forget
            try:
                forget(session, destination.name)
            finally:
                destination.unlink(missing_ok=True)
        raise
    finally:
        if renderer:
            renderer.close()
        for array in maps:
            if isinstance(array, np.memmap) and not array._mmap.closed:
                _close(array)
        gc.collect()
        shutil.rmtree(work)
    # Total time, including source fetching, so calibration matches what the user waits for.
    result.update(wall_s=time.monotonic()-started-queued, peak_rss_bytes=peak_rss_bytes(),
                  estimate={"model_s": info["estimate"]["model_s"]})
    write_json(destination.with_suffix(".json"), result)
    return result
