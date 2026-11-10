"""Position resolution and bounded, cached archive reconnaissance."""
from __future__ import annotations

from collections import Counter
from copy import deepcopy
from dataclasses import asdict
import json
import math
import re
import threading
import time

import numpy as np
import requests
from astropy.coordinates import SkyCoord
from astropy.wcs import WCS

from astroimp.aladin import AladinView, rotated_view, rotated_wcs
from astroimp.catalog import PUBLIC_IMAGE_CRITERIA, polygons
from astroimp.datasets import SkyFootprint
from astroimp.services import friendly

MAST_URL = "https://mast.stsci.edu/api/v0/invoke"
ASPECTS = {"16:9": 16/9, "3:2": 1.5, "1:1": 1, "9:16": 9/16}
_cache: dict[tuple, tuple[float, list[dict], str | None]] = {}
_inflight: dict[tuple, threading.Event] = {}
_lock = threading.Lock()
COVERAGE_WAIT = 3


def mast_request(service: str, params: dict, timeout: float = 6) -> dict:
    response = requests.post(MAST_URL, data={"request": json.dumps({
        "service": service, "params": params, "format": "json", "pagesize": 10000,
        "page": 1})}, timeout=timeout)
    response.raise_for_status()
    result = response.json()
    if result.get("status") in {"ERROR", "EXECUTING"}:
        raise requests.Timeout(f"MAST isn't responding: {result.get('msg', '')}")
    return result


def parse_position(target: str) -> SkyCoord | None:
    value = target.strip().replace(",", " ")
    if not value:
        raise ValueError("Enter a target or coordinates.")
    parts = value.split()
    try:
        if len(parts) == 2:
            if all(re.fullmatch(r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)", p) for p in parts):
                ra, dec = map(float, parts)
                if not 0 <= ra < 360 or not -90 <= dec <= 90:
                    raise ValueError("Coordinates must have RA 0–360° and Dec −90–90°.")
                return SkyCoord(ra, dec, unit="deg")
            if ":" in value or re.search(r"\d[hHdD]", value):
                return SkyCoord(parts[0], parts[1], unit=("hourangle", "deg"))
    except ValueError as exc:
        raise ValueError("Invalid coordinates. Use RA and Dec, in degrees or sexagesimal.") from exc
    return None


def _sesame(target: str) -> SkyCoord:
    # Astropy may make several HTTP requests; bound their combined wall time.
    done = threading.Event()
    result = []

    def lookup():
        try:
            result.append(SkyCoord.from_name(target))
        except Exception as exc:
            result.append(exc)
        finally:
            done.set()
    threading.Thread(target=lookup, name="astroimp-resolve", daemon=True).start()
    if not done.wait(6):
        raise requests.Timeout("CDS isn't responding.")
    if isinstance(result[0], BaseException):
        raise result[0]
    return result[0]


def resolve(target: str) -> dict:
    target = target.strip()
    if len(target) > 200:
        raise ValueError("Target is too long.")
    position = parse_position(target)
    name = None
    if position is None:
        error = None
        try:
            found = mast_request("Mast.Name.Lookup", {"input": target, "format": "json"})
            matches = found.get("resolvedCoordinate", [])
            if matches:
                match = matches[0]
                position = SkyCoord(match["ra"], match["decl"], unit="deg")
                name = re.sub(r"\s+", " ", match.get("canonicalName") or target)
        except (requests.RequestException, ValueError) as exc:
            error = exc
        if position is None:
            try:
                position = _sesame(target)
                name = target
            except Exception as exc:
                if error is not None:
                    raise RuntimeError(friendly(error)) from error
                # NameResolveError also wraps timeouts; keep the useful distinction.
                if any(word in str(exc).lower() for word in ("timed out", "responding", "connection")):
                    raise RuntimeError(friendly(requests.Timeout("CDS isn't responding."))) from exc
                raise ValueError(f"No match for '{target}'") from exc
    return {"name": name or f"RA {position.ra.deg:.4f} Dec {position.dec.deg:.4f}",
            "ra": float(position.ra.deg), "dec": float(position.dec.deg)}


def synthetic_view(ra: float, dec: float, width_arcmin: float = 10,
                   aspect: str = "16:9", rotation_deg: float = 0):
    if (aspect not in ASPECTS or not all(math.isfinite(v) for v in (ra, dec, width_arcmin, rotation_deg))
            or not 0 <= ra < 360 or not -90 <= dec <= 90
            or not 1 <= width_arcmin <= 180 or not -180 <= rotation_deg <= 180):
        raise ValueError("Invalid position or frame.")
    ratio = ASPECTS[aspect]
    width, height = (1600, round(1600/ratio)) if ratio >= 1 else (round(1600*ratio), 1600)
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = [ra, dec]
    wcs.wcs.crpix = [(width+1)/2, (height+1)/2]
    # Set the angular width exactly (rather than the tangent-plane approximation).
    scale = math.degrees(2*math.tan(math.radians(width_arcmin/60)/2))/width
    wcs.wcs.cdelt = [-scale, scale]
    wcs.pixel_shape = (width, height)
    corners = wcs.pixel_to_world([-.5, width-.5, width-.5, -.5],
                                [-.5, -.5, height-.5, height-.5]).icrs
    original = asdict(AladinView(
        (ra, dec), (width_arcmin, math.degrees(2*math.atan(
            math.radians(scale*height)/2))*60),
        tuple(zip(corners.ra.deg, corners.dec.deg)), (width, height), (width, height),
        ("RA---TAN", "DEC--TAN"), 0, source="coordinates"))
    header = wcs.to_header()
    header["NAXIS"] = 2
    header["NAXIS1"], header["NAXIS2"] = width, height
    return original, rotated_view(original, header, rotation_deg), header, rotated_wcs(
        header, (width, height), rotation_deg)


def suggest_frame(center: tuple[float, float], rows: list[dict], aspect: str = "16:9") -> dict:
    if aspect not in ASPECTS:
        raise ValueError("Unknown aspect ratio.")
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = center
    wcs.wcs.crpix = [1, 1]
    wcs.wcs.cdelt = [-1/60, 1/60]
    containing = []
    for row in rows:
        for shape in polygons(row.get("s_region") or ""):
            try:
                if SkyFootprint(shape).contains_radec(*center):
                    x, y = wcs.world_to_pixel(SkyCoord(shape[:, 0], shape[:, 1], unit="deg"))
                    if np.isfinite(x).all() and np.isfinite(y).all():
                        containing.append(np.column_stack((x, y)))
            except ValueError:
                continue
    ratio = ASPECTS[aspect]
    width = 10.0
    if containing:
        points = np.concatenate(containing)
        # Keep the requested position at frame centre, even for asymmetric footprints.
        extent = 2*np.max(np.abs(points), axis=0)*1.1
        width = max(float(extent[0]), float(extent[1])*ratio)
    # Both sides stay in 1–30 arcminutes while preserving the chosen aspect.
    width = min(30*min(1, ratio), max(max(1, ratio), width))
    return {"width_arcmin": round(width, 3), "height_arcmin": round(width/ratio, 3),
            "aspect": aspect, "covering": len(containing)}


def coverage(ra: float, dec: float, aspect: str = "16:9", *, refresh: bool = False) -> dict:
    synthetic_view(ra, dec, aspect=aspect)  # Validate even when a cache entry exists.
    key = (round(ra, 6), round(dec, 6))
    def fetch(done):
        rows, warning = [], None
        try:
            result = mast_request("Mast.Caom.Filtered.Position", {
                "columns": "obs_id,obs_collection,instrument_name,s_region",
                "filters": [{"paramName": field, "values": [
                    str(value) for value in (values if isinstance(values, list) else [values])]}
                    for field, values in PUBLIC_IMAGE_CRITERIA.items()],
                "position": f"{ra},{dec},0.1"}, timeout=12)
            rows = list({row["obs_id"]: row for row in result.get("data", [])}.values())
        except (requests.RequestException, ValueError, KeyError, TypeError) as exc:
            rows, warning = [], friendly(exc)
        finally:
            with _lock:
                if len(_cache) >= 128:
                    _cache.pop(next(iter(_cache)))
                _cache[key] = (time.monotonic(), deepcopy(rows), warning)
                _inflight.pop(key, None)
            done.set()
    with _lock:
        cached = _cache.get(key)
        if refresh and cached and cached[2]:
            _cache.pop(key, None)
            cached = None
        valid = cached and time.monotonic()-cached[0] < (30 if cached[2] else 300)
        done = _inflight.get(key)
        if not valid and done is None and len(_inflight) < 8:
            _cache.pop(key, None)
            cached = None
            done = _inflight[key] = threading.Event()
            threading.Thread(target=fetch, args=(done,), name="astroimp-coverage", daemon=True).start()
    if not valid and done is not None:
        done.wait(COVERAGE_WAIT)
        with _lock:
            cached = _cache.get(key)
    pending = bool(not cached and done is not None and not done.is_set())
    rows = cached[1] if cached else []
    warning = cached[2] if cached else (None if pending else "Archive checks are busy. Try again.")
    counts = Counter(row["obs_collection"] for row in rows)
    instruments = Counter((row["obs_collection"], row["instrument_name"]) for row in rows)
    return {"counts": {"HST": counts["HST"], "JWST": counts["JWST"]},
            "instruments": [{"mission": mission, "instrument": instrument, "count": count}
                            for (mission, instrument), count in sorted(instruments.items())],
            "frame": suggest_frame((ra, dec), rows, aspect), "warning": warning, "pending": pending}
