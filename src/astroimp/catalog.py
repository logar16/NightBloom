"""Observation-level MAST catalog for a captured Aladin view.

Observation rows already carry the science file URI, a preview JPEG and the footprint,
so browsing needs one query; per-product lookups are unnecessary.
"""
from __future__ import annotations

import math
import re
from collections import defaultdict
from dataclasses import asdict, dataclass

import numpy as np
from astropy import units as u
from astropy.coordinates import SkyCoord
from astropy.io import fits
from astropy.time import Time
from astropy.wcs import WCS
from astroquery.mast import Observations

from astroimp.datasets import SkyFootprint, field_family
from astroimp.filters import describe
from astroimp.mast import Product, text

MAX_RADIUS_ARCMIN = 180.0
IGNORED_FILTERS = {"", "CLEAR", "OPEN", "N/A", "NONE"}
PUBLIC_IMAGE_CRITERIA = {
    "obs_collection": ["HST", "JWST"], "dataproduct_type": "image",
    "intentType": "science", "calib_level": [3], "dataRights": "PUBLIC",
}


@dataclass(frozen=True)
class Observation:
    obsid: str
    obs_id: str
    collection: str
    instrument: str
    filter: str
    proposal: str
    target: str
    title: str
    region: str
    ra: float
    dec: float
    wavelength_nm: float | None
    exptime_s: float
    date: str
    data_uri: str
    preview_uri: str

    @property
    def filename(self) -> str:
        return self.data_uri.rsplit("/", 1)[-1]


def filter_name(value: str) -> str | None:
    parts = [p.strip().upper() for p in re.split(r"[;+]", value)]
    if any(p in {"DETECTION", "TOTAL", "UNKNOWN"} for p in parts):
        return None
    kept = [p for p in parts if p not in IGNORED_FILTERS and not p.startswith(("CLEAR", "OPEN"))]
    return "+".join(dict.fromkeys(kept)) or None


def polygons(region: str) -> list[np.ndarray]:
    shapes = []
    for chunk in re.split(r"(?i)\bPOLYGON\b", region)[1:]:
        tokens = [t for t in chunk.split() if not t.isalpha()]
        try:
            values = np.asarray(tokens, dtype=float)
        except ValueError:
            continue
        if len(values) >= 6 and len(values) % 2 == 0:
            shapes.append(values.reshape(-1, 2))
    return shapes


def view_polygon(corners: list[list[float]] | tuple) -> SkyFootprint:
    return SkyFootprint(np.asarray(corners, dtype=float))


def search_radius_arcmin(center: tuple[float, float], corners) -> float:
    c = SkyCoord(*center, unit="deg")
    edges = SkyCoord([p[0] for p in corners], [p[1] for p in corners], unit="deg")
    return float(min(MAX_RADIUS_ARCMIN, max(1.0, c.separation(edges).max().arcmin * 1.02)))


def query(center: tuple[float, float], radius_arcmin: float) -> list[Observation]:
    Observations.TIMEOUT = 180
    table = Observations.query_criteria(
        coordinates=SkyCoord(*center, unit="deg"),
        radius=radius_arcmin * u.arcmin,
        **PUBLIC_IMAGE_CRITERIA,
    )
    found = []
    for row in table:
        label = filter_name(text(row["filters"]))
        data_uri = text(row["dataURL"])
        if label is None or not data_uri.lower().endswith(".fits") or "_total" in data_uri.lower():
            continue
        lo, hi = row["em_min"], row["em_max"]
        wavelength = None if np.ma.is_masked(lo) or np.ma.is_masked(hi) else float(lo + hi) / 2
        start = row["t_min"]
        found.append(Observation(
            obsid=text(row["obsid"]),
            obs_id=text(row["obs_id"]),
            collection=text(row["obs_collection"]),
            instrument=text(row["instrument_name"]),
            filter=label,
            proposal=text(row["proposal_id"]),
            target=text(row["target_name"]),
            title=text(row["obs_title"]),
            region=text(row["s_region"]),
            ra=float(row["s_ra"]),
            dec=float(row["s_dec"]),
            wavelength_nm=wavelength,
            exptime_s=0.0 if np.ma.is_masked(row["t_exptime"]) else float(row["t_exptime"]),
            date="" if np.ma.is_masked(start) else Time(float(start), format="mjd").to_value("iso", "date"),
            data_uri=data_uri,
            preview_uri=text(row["jpegURL"]),
        ))
    return found


def in_view(obs: Observation, view: SkyFootprint, center: tuple[float, float]) -> bool:
    if view.contains_radec(obs.ra, obs.dec):
        return True
    for shape in polygons(obs.region):
        try:
            if SkyFootprint(shape).contains_radec(*center):
                return True
        except ValueError:
            continue
    return False


def snapshot_wcs(header: fits.Header) -> WCS:
    return WCS(header).celestial


def project(region: str, wcs: WCS, height: int) -> list[list[list[float]]]:
    shapes = []
    for shape in polygons(region):
        x, y = wcs.world_to_pixel(SkyCoord(shape[:, 0], shape[:, 1], unit="deg"))
        if np.all(np.isfinite(x)) and np.all(np.isfinite(y)):
            # FITS rows count up from the bottom; JPEG rows count down from the top.
            shapes.append([[round(float(a), 1), round(float(height - 1 - b), 1)] for a, b in zip(x, y)])
    return shapes


def group(observations: list[Observation], wcs: WCS | None = None, height: int = 0) -> list[dict]:
    buckets: dict[tuple, list[Observation]] = defaultdict(list)
    for obs in observations:
        proxy = Product(obs.filename, obs.data_uri, obs.collection, obs.instrument, obs.filter,
                        obs.obs_id, obs.proposal, 0, obs.region, "product")
        field, family = field_family(proxy)
        buckets[(obs.collection, obs.proposal, obs.instrument, field, family)].append(obs)
    groups = []
    for (collection, proposal, instrument, field, family), members in buckets.items():
        members.sort(key=lambda o: (o.wavelength_nm or math.inf, -o.exptime_s, o.obs_id))
        filters = list(dict.fromkeys(o.filter for o in members))
        entries = []
        for obs in members:
            entry = asdict(obs)
            entry["info"] = describe(obs.filter, obs.wavelength_nm)
            entry["pixels"] = project(obs.region, wcs, height) if wcs is not None else []
            entries.append(entry)
        targets = list(dict.fromkeys(o.target for o in members if o.target))
        groups.append({
            "key": "|".join((collection, proposal, instrument, field, family)),
            "collection": collection,
            "proposal": proposal,
            "instrument": instrument,
            "family": family,
            "field": field,
            "targets": targets[:3],
            "title": members[0].title,
            "date": max(o.date for o in members),
            "filters": filters,
            "observations": entries,
        })
    # Multi-filter fields first: they are what makes colour composition possible.
    groups.sort(key=lambda g: (-len(g["filters"]), g["collection"] != "JWST", g["date"]), reverse=False)
    for index, entry in enumerate(groups, 1):
        entry["id"] = f"G{index:03}"
    return groups


def catalog(center: tuple[float, float], corners, header: fits.Header | None = None,
            jpeg_size: tuple[int, int] | None = None) -> dict:
    radius = search_radius_arcmin(center, corners)
    found = query(center, radius)
    view = view_polygon(corners)
    visible = [o for o in found if in_view(o, view, center)]
    wcs = snapshot_wcs(header) if header is not None else None
    height = jpeg_size[1] if jpeg_size else 0
    return {
        "radius_arcmin": radius,
        "observations_found": len(found),
        "observations_in_view": len(visible),
        "groups": group(visible, wcs, height),
    }
