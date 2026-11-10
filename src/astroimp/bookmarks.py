"""Project-independent archive bookmarks and their sky frames."""
from __future__ import annotations

from copy import deepcopy
from datetime import datetime, timezone
import json
from pathlib import Path
import threading
import uuid

import numpy as np
from astropy.coordinates import SkyCoord
from astropy.wcs import WCS

from astroimp.catalog import polygons
from astroimp.coordinates import ASPECTS
from astroimp.mast import write_json


def _points(items: list[dict]) -> np.ndarray:
    points = []
    for item in items:
        shapes = polygons(item.get("region") or "")
        if shapes:
            points.extend(shapes)
        else:
            center = item.get("center_icrs_deg") or [item.get("ra"), item.get("dec")]
            points.append(np.asarray([center], dtype=float))
    if not points:
        raise ValueError("Select at least one bookmark.")
    result = np.concatenate(points)
    if (not np.isfinite(result).all() or np.any(np.abs(result[:, 1]) > 90)):
        raise ValueError("This observation has no usable sky footprint.")
    return result


def _bounds(items: list[dict]):
    points = _points(items)
    sky = SkyCoord(points[:, 0], points[:, 1], unit="deg")
    vector = sky.cartesian.xyz.value.mean(axis=1)
    if np.linalg.norm(vector) < 1e-8:
        raise ValueError("These footprints are too far apart. Narrow the selection.")
    center = SkyCoord(*vector, representation_type="cartesian").spherical
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = [center.lon.deg, center.lat.deg]
    wcs.wcs.crpix = [1, 1]
    wcs.wcs.cdelt = [-1/60, 1/60]
    x, y = wcs.world_to_pixel(sky)
    if not np.isfinite([x, y]).all():
        raise ValueError("These footprints are too far apart. Narrow the selection.")
    midpoint = wcs.pixel_to_world((x.min()+x.max())/2, (y.min()+y.max())/2)
    wcs.wcs.crval = [midpoint.ra.deg, midpoint.dec.deg]
    x, y = wcs.world_to_pixel(sky)
    extent = 2*np.max(np.abs([x, y]), axis=1)
    return [float(midpoint.ra.deg), float(midpoint.dec.deg)], extent


def frame(items: list[dict], aspect: str = "16:9") -> dict:
    if aspect not in ASPECTS:
        raise ValueError("Unknown aspect ratio.")
    center, extent = _bounds(items)
    ratio = ASPECTS[aspect]
    required = max(float(extent[0]), float(extent[1])*ratio)*1.1
    limit = 30*min(1, ratio)
    width = min(limit, max(max(1, ratio), required))
    exceeds = required > limit
    return {"ra": center[0], "dec": center[1], "aspect": aspect,
            "width_arcmin": round(width, 3), "height_arcmin": round(width/ratio, 3),
            "required_width_arcmin": round(required, 3), "exceeds_limit": exceeds,
            "warning": "Footprints exceed 30′. This frame is cropped; narrow the selection to include every image."
                       if exceeds else None}


class Bookmarks:
    _locks: dict[Path, threading.RLock] = {}
    _guard = threading.Lock()

    def __init__(self, data: Path):
        self.path = Path(data) / "bookmarks.json"
        with self._guard:
            # Windows resolve() can switch to extended paths while a parent is being created.
            self.lock = self._locks.setdefault(self.path.absolute(), threading.RLock())

    def list(self) -> list[dict]:
        with self.lock:
            if not self.path.exists():
                return []
            return json.loads(self.path.read_text(encoding="utf-8"))

    def get(self, identifier: str) -> dict:
        with self.lock:
            for item in self.list():
                if item["id"] == identifier:
                    return item
        raise KeyError(identifier)

    def add(self, rows: list[dict], source_session: str, source_name: str) -> list[dict]:
        with self.lock:
            items = self.list()
            known = {item["obs_id"] for item in items}
            added = []
            for row in rows:
                if row["collection"] not in {"HST", "JWST"}:
                    raise ValueError("Only HST and JWST images can be bookmarked.")
                if row["obs_id"] in known:
                    continue
                center, extent = _bounds([row])
                item = {key: deepcopy(row.get(key)) for key in (
                    "obs_id", "collection", "instrument", "filter", "target", "wavelength_nm", "region")}
                item.update(id=uuid.uuid4().hex, center_icrs_deg=center,
                            size_arcmin=[float(v) for v in extent],
                            preview=row.get("preview_uri"), source_session=source_session,
                            source_name=source_name, note="",
                            created=datetime.now(timezone.utc).isoformat())
                items.append(item)
                added.append(item)
                known.add(row["obs_id"])
            if added:
                write_json(self.path, items)
            return items

    def note(self, identifier: str, note: str) -> dict:
        if len(note) > 200:
            raise ValueError("Notes can contain at most 200 characters.")
        with self.lock:
            items = self.list()
            item = next((item for item in items if item["id"] == identifier), None)
            if item is None:
                raise KeyError(identifier)
            item["note"] = note
            write_json(self.path, items)
            return item

    def remove(self, identifier: str) -> None:
        with self.lock:
            items = self.list()
            kept = [item for item in items if item["id"] != identifier]
            if len(items) == len(kept):
                raise KeyError(identifier)
            write_json(self.path, kept)
