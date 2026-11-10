"""FITS survey bands, coverage discovery and bounded on-grid cutouts."""
from __future__ import annotations

import hashlib
import json
import logging
import math
import threading
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import requests
from astropy.io import fits
from astropy.wcs import WCS
from astroquery.hips2fits import hips2fitsClass
from scipy.ndimage import gaussian_filter

from astroimp.cancellation import CancelledError, check_cancelled
from astroimp.mast import write_json

MOC_SERVER = "https://alasky.cds.unistra.fr/MocServer/query"
SERVERS = (
    "https://alasky.cds.unistra.fr/hips-image-services/hips2fits",
    "https://alaskybis.cds.unistra.fr/hips-image-services/hips2fits",
)
USER_AGENT = "NightBloom/1.0 (astronomical artwork; CDS HiPS FITS cutouts)"
TILE_SIDE = 4000
IRSA_CACHE_VERSION = "-bilinear-v2"
_requests = threading.BoundedSemaphore(2)
_locks = [threading.Lock() for _ in range(32)]
_cds_down_until = 0.0
log = logging.getLogger(__name__)


def note_cds_health(ok: bool) -> None:
    global _cds_down_until
    _cds_down_until = 0.0 if ok else time.monotonic() + 30


@dataclass(frozen=True)
class Band:
    hips_id: str
    survey: str
    band: str
    wavelength_nm: float
    hips_order: int
    sky_fraction: float
    fwhm_arcsec: float
    title: str = ""

    @property
    def native_scale_arcsec(self) -> float:
        # Mean HEALPix pixel width; HiPS tiles have 512 pixels on each side.
        return math.degrees(math.sqrt(math.pi / 3)) * 3600 / (512 * 2**self.hips_order)

    @property
    def obs_id(self) -> str:
        return "survey-" + slug(self.hips_id)


def slug(hips_id: str) -> str:
    return hips_id.replace("/", "-")


# Verified against MocServer on 2026-10-24: all bands advertise FITS tiles.
# Wavelengths are band centres in nm (Hα uses the line, not the broad passband mean).
# Approximate effective detail, not HiPS pixel sampling. Typical seeing/resolution:
# IPHAS/VPHAS+: https://arxiv.org/abs/1406.4862, https://arxiv.org/abs/1402.7024
# Pan-STARRS: https://archive.stsci.edu/missions-and-data/pan-starrs
# DSS2: https://archive.stsci.edu/dss/ ; 2MASS:
# https://irsa.ipac.caltech.edu/data/2MASS/docs/supplementary/seeing/seesum.html
# DSS2 and 2MASS are star FWHM measured from built planes on 2026-10-28 (Gaussian fits,
# 150 unsaturated stars at 0.67″/px): DSS2 4.6–4.9″, 2MASS K 3.3″, VPHAS+ Hα 1.1″.
# WISE: https://wise2.ipac.caltech.edu/docs/release/allsky/expsup/sec1_1.html
# PACS/SPIRE observer manuals: https://www.cosmos.esa.int/web/herschel/legacy-documentation
# SHASSA/VTSS: https://arxiv.org/abs/astro-ph/0108518 (0.8 / 1.6 arcmin).
BANDS = (
    Band("CDS/P/IPHAS/DR2/halpha", "IPHAS", "Hα", 656.3, 11, .04503, 1.0),
    Band("CDS/P/IPHAS/DR2/r", "IPHAS", "r", 624, 11, .04504, 1.0),
    Band("CDS/P/IPHAS/DR2/i", "IPHAS", "i", 754, 11, .04504, 1.0),
    Band("CDS/P/VPHAS/DR4/Halpha", "VPHAS+", "Hα", 656.3, 11, .04884, 1.0),
    Band("CDS/P/VPHAS/DR4/u", "VPHAS+", "u", 361, 11, .04858, 1.0),
    Band("CDS/P/VPHAS/DR4/g", "VPHAS+", "g", 474, 11, .0487, 1.0),
    Band("CDS/P/VPHAS/DR4/r", "VPHAS+", "r", 633, 11, .04888, 1.0),
    Band("CDS/P/VPHAS/DR4/i", "VPHAS+", "i", 766, 11, .04882, 1.0),
    *(Band(f"CDS/P/allWISE/W{i}", "WISE", f"W{i}", nm, 8, frac, fwhm)
      for i, nm, frac, fwhm in ((1, 3400, 1, 6.1), (2, 4600, 1, 6.4),
                                (3, 12000, .9999, 6.5), (4, 22000, 1, 12.0))),
    Band("CDS/P/DSS2/red", "DSS2", "red", 649, 9, 1, 4.7),
    Band("CDS/P/DSS2/blue", "DSS2", "blue", 480, 9, .9972, 4.7),
    Band("CDS/P/DSS2/NIR", "DSS2", "NIR", 825, 9, .9955, 4.7),
    *(Band(f"CDS/P/PanSTARRS/DR1/{band}", "Pan-STARRS", band, nm, 11, frac, 1.1)
      for band, nm, frac in (("g", 477, .7582), ("r", 621, .7595),
                            ("i", 754, .7628), ("z", 868, .7626))),
    Band("CDS/P/SHASSA/SM", "SHASSA", "Hα", 656.3, 4, .6432, 48),
    Band("CDS/P/VTSS/HaCC", "VTSS", "Hα", 656.3, 3, .3164, 96),
    *(Band(f"ESAVO/P/HERSCHEL/{band}", "Herschel", band, nm, order, frac, fwhm)
      for band, nm, order, frac, fwhm in (
          ("PACS70", 70000, 9, .04222, 5.6), ("PACS160", 160000, 8, .08346, 11.4),
          ("SPIRE-250", 250000, 7, .1544, 18), ("SPIRE-350", 350000, 6, .2489, 25),
          ("SPIRE-500", 500000, 5, .4578, 36))),
    *(Band(f"CDS/P/2MASS/{band}", "2MASS", band, nm, 9, 1, 3.3)
      for band, nm in (("J", 1235), ("H", 1662), ("K", 2159))),
)
BY_ID = {band.hips_id: band for band in BANDS}


def is_survey(observation: dict) -> bool:
    return observation.get("hips_id") in BY_ID


def band_for(observation: dict) -> Band:
    return BY_ID[observation["hips_id"]]


REPAIR_VERSION = 2


def repair_defects(pixels: np.ndarray) -> np.ndarray:
    """Fill interior chip gaps and replace bad detector columns left in HiPS mosaics.

    VPHAS+ cutouts carry small blank (0/NaN) CCD gaps and 1–3 px columns offset over
    hundreds of rows. Blanks touching the border are real no-coverage and stay NaN.
    """
    a = np.asarray(pixels, dtype=np.float32)
    height = a.shape[0]
    if height <= REPAIR_STRIP + 2 * REPAIR_OVERLAP:
        return _repair_strip(a, True, True)
    # Overlapping strips bound scratch memory for native-scale exports (tens of Mpx).
    out = np.empty_like(a)
    for y in range(0, height, REPAIR_STRIP):
        y0, y1 = max(0, y - REPAIR_OVERLAP), min(height, y + REPAIR_STRIP + REPAIR_OVERLAP)
        strip = _repair_strip(a[y0:y1], y0 == 0, y1 == height)
        out[y:min(height, y + REPAIR_STRIP)] = strip[y - y0:y - y0 + min(REPAIR_STRIP, height - y)]
    return out


REPAIR_STRIP, REPAIR_OVERLAP = 2048, 256


def _repair_strip(a: np.ndarray, top: bool, bottom: bool) -> np.ndarray:
    from scipy import ndimage as ndi
    bad = ~np.isfinite(a) | (a == 0)
    if bad.all():
        return a
    fill = np.where(bad, np.float32(np.median(a[~bad])), a)
    horizontal = ndi.median_filter(fill, size=(1, 7))
    # A 3 px median leaves only single-column steps; smooth filaments nearly cancel.
    residual = fill - ndi.median_filter(fill, size=(1, 3))
    noise = 1.4826 * float(np.median(np.abs(residual[~bad])))
    # A vertical median removes stars and filaments; only column-coherent offsets survive.
    persistent = ndi.median_filter(residual, size=(61, 1))
    column = ndi.binary_opening(np.abs(persistent) > noise, structure=np.ones((31, 1), bool)) & ~bad
    labels, _ = ndi.label(column)
    keep = []
    for i, s in enumerate(ndi.find_objects(labels), 1):
        if s is None or s[0].stop - s[0].start < 60 or s[1].stop - s[1].start > 3:
            continue
        core = float(np.abs(persistent[s][labels[s] == i]).mean())
        left, right = max(0, s[1].start - 2), min(a.shape[1] - 1, s[1].stop + 1)
        side = float(np.abs(persistent[s[0], [left, right]]).mean())
        # Detector columns are sharp steps; real filaments fade over neighbouring columns.
        if side < .3 * core:
            keep.append(i)
    columns = np.isin(labels, keep)
    del labels, column, persistent, residual
    labels, count = ndi.label(bad)
    # Strip seams are not image borders; holes cut there are interior in the neighbour strip.
    edges = [labels[:, 0], labels[:, -1]] + ([labels[0]] if top else []) + ([labels[-1]] if bottom else [])
    border = np.unique(np.concatenate(edges))
    hole = np.bincount(labels.ravel(), minlength=count + 1) <= 20000
    hole[0] = False
    hole[border] = False
    holes = hole[labels]
    del labels
    if not columns.any() and not holes.any():
        return a
    out = np.where(columns, horizontal, a).astype(np.float32)
    out[bad] = np.nan
    smooth = ndi.median_filter(fill, size=5)
    if holes.any():
        # Interpolate from a star-suppressed sky so stars on a gap edge don't smear inward.
        source = np.where(bad, np.nan, ndi.median_filter(np.where(bad, smooth, out), size=9))
        for sigma in (2, 4, 8, 16, 32):
            need = holes & ~np.isfinite(out)
            if not need.any():
                break
            known = np.isfinite(source)
            weight = ndi.gaussian_filter(known.astype(np.float32), sigma)
            value = ndi.gaussian_filter(np.where(known, source, 0), sigma)
            ready = need & (weight > .2)
            out = np.where(ready, value / np.maximum(weight, 1e-6), out)
            source = np.where(ready, out, source)
    if holes.any():
        # A smooth fill reads as a smudge; add grain with the sky's measured level and
        # pixel correlation (HiPS resampling softens noise). Seeded for repeatable builds.
        sky = fill - smooth
        valid = ~bad & (np.abs(sky) < 5 * 1.4826 * float(np.median(np.abs(sky[~bad]))))
        level = float(np.std(sky[valid]))
        shifted = sky[:, 1:][valid[:, 1:] & valid[:, :-1]], sky[:, :-1][valid[:, 1:] & valid[:, :-1]]
        rho = float(np.clip(np.corrcoef(*shifted)[0, 1], 0, .9))
        blur = np.sqrt(-1 / (4 * np.log(rho))) if rho > .05 else 0
        grain = np.random.default_rng(a.shape[0] * 7919 + a.shape[1]).normal(size=a.shape).astype(np.float32)
        if blur:
            grain = ndi.gaussian_filter(grain, blur)
        grain *= level / max(float(grain.std()), 1e-9)
        out = np.where(holes & np.isfinite(out), out + grain, out)
    return np.where(bad & ~holes, a, out).astype(np.float32, copy=False)


def detail_for_view(band: Band, view: dict) -> dict:
    """Keep two samples across typical detail on the long side of a 4k export."""
    fwhm = band.fwhm_arcsec
    soft = fwhm > 4.0 and fwhm > 2 * max(view["field_arcmin"]) * 60 / 4096
    minimum_degrees = fwhm / 2 * 4096 / 3600
    return {"fwhm_arcsec": fwhm, "soft": soft,
            "soft_reason": (f"~{fwhm:g}″ detail; crisp only for views ≥ {minimum_degrees:.1f}°"
                            if soft else "")}


def annotate_detail(content: dict, view: dict) -> None:
    """Backfill legacy catalogs in memory without recapture or rewriting user data."""
    for group in content.get("groups", []):
        for observation in group["observations"]:
            if is_survey(observation):
                observation.update(detail_for_view(band_for(observation), view))


def view_grid(view: dict, native_scale: float, max_side: int = 3072) -> dict:
    """Keep the captured frame's axis lengths; rotation does not enlarge its pixel array."""
    width_arcsec, height_arcsec = np.asarray(view["field_arcmin"], float) * 60
    scale = max(native_scale, max(width_arcsec, height_arcsec) / max_side)
    width, height = np.maximum(2, np.ceil(np.array([width_arcsec, height_arcsec]) / scale)).astype(int)
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = view["center_icrs_deg"]
    wcs.wcs.crpix = [(width + 1) / 2, (height + 1) / 2]
    wcs.wcs.cdelt = [-scale / 3600, scale / 3600]
    angle = float(view.get("rotation_deg", 0))
    if angle:
        # Position angle of snapshot up, east of north; +90° puts north right.
        radians = math.radians(angle)
        c, s = math.cos(radians), math.sin(radians)
        wcs.wcs.pc = [[c, -s], [s, c]]
    return {"width": int(width), "height": int(height), "wcs": dict(wcs.to_header()),
            "pixel_scale_arcsec": float(scale),
            "region_sky": view.get("corners_icrs_deg", [])}


def available(view: dict, cache: Path, progress=lambda *args: None, *, warn=log.warning) -> list[Band]:
    """MOC intersection with the captured view's enclosing cone, cached for one day."""
    from astropy.coordinates import SkyCoord
    bands = tuple(band for band in BANDS if band.survey != "Pan-STARRS")
    center = view["center_icrs_deg"]
    corners = view.get("corners_icrs_deg")
    radius = (SkyCoord(*center, unit="deg").separation(
        SkyCoord(np.asarray(corners)[:, 0], np.asarray(corners)[:, 1], unit="deg")).max().deg
        if corners else math.hypot(*view["field_arcmin"]) / 120)
    params = {"RA": center[0], "DEC": center[1], "SR": float(radius),
              "expr": " || ".join(f"ID={b.hips_id}" for b in bands),
              "get": "record", "fmt": "json", "fields": "ID"}
    key = hashlib.sha256(json.dumps(params, sort_keys=True).encode()).hexdigest()
    path = cache / "surveys" / f"{key}.json"
    if path.exists() and time.time() - path.stat().st_mtime < 86400:
        ids = json.loads(path.read_text(encoding="utf-8"))
    else:
        check_cancelled(progress)
        fallback = path.with_name(f"{key}-independent-v1.json")
        try:
            if time.monotonic() < _cds_down_until:
                raise requests.ConnectionError("CDS is currently unreachable.")
            response = requests.get(MOC_SERVER, params=params, timeout=(6, 40),
                                    headers={"User-Agent": USER_AGENT})
            response.raise_for_status()
            records = response.json()
            if not isinstance(records, list) or any(not isinstance(row, dict) for row in records):
                raise ValueError("CDS returned an invalid survey availability response.")
            ids = [row["ID"] for row in records if row.get("ID") in BY_ID]
            note_cds_health(True)
        except (requests.RequestException, ValueError) as exc:
            from astroimp import survey_sources
            note_cds_health(False)
            if fallback.exists() and time.time() - fallback.stat().st_mtime < 300:
                result = json.loads(fallback.read_text(encoding="utf-8"))
            else:
                progress("search", 0, "CDS unavailable; checking independent survey archives")
                ids, errors = survey_sources.available(view, cache, bands, progress)
                warning = ("CDS is unavailable. Showing independently checked DSS2 and WISE "
                           "coverage only; other surveys could not be checked.")
                if errors:
                    warning += " Some independent coverage checks also failed: " + "; ".join(errors)
                result = {"ids": ids, "warning": warning}
                if not ids:
                    raise RuntimeError(warning) from exc
                check_cancelled(progress)
                if not errors:
                    fallback.parent.mkdir(parents=True, exist_ok=True)
                    write_json(fallback, result)
            warn(result["warning"])
            return [band for band in bands if band.hips_id in result["ids"]]
        check_cancelled(progress)
        path.parent.mkdir(parents=True, exist_ok=True)
        write_json(path, ids)
    return [band for band in bands if band.hips_id in ids]


def groups(view: dict, bands: list[Band], wcs: WCS | None = None) -> list[dict]:
    from astroimp.catalog import project
    from astroimp.filters import describe
    corners = view.get("corners_icrs_deg")
    if not corners:
        grid = view_grid(view, 1, 512)
        target = WCS(grid["wcs"])
        world = target.calc_footprint(axes=(grid["width"], grid["height"]))
        corners = world.tolist()
    region = "POLYGON ICRS " + " ".join(str(v) for point in corners for v in point)
    result = {}
    for band in bands:
        group = result.setdefault(band.survey, {
            "id": "S-" + band.survey, "key": "Survey|" + band.survey,
            "collection": "Survey", "proposal": "", "instrument": band.survey,
            "family": "survey", "field": band.survey, "targets": [],
            "title": band.survey, "date": "", "filters": [], "observations": []})
        group["filters"].append(band.band)
        info = describe(band.band, band.wavelength_nm)
        if band.band == "Hα":
            info.update(line="Hα", shows="Ionised hydrogen · diffuse emission")
        elif band.survey == "WISE" and band.band in {"W3", "W4"}:
            info["shows"] = "Warm dust · wispy infrared clouds"
        elif band.survey == "DSS2":
            info["shows"] = "Photographic sky survey · non-linear intensity"
        if band.survey in {"IPHAS", "VPHAS+"}:
            info["shows"] = (info.get("shows") or "Optical sky") + " · raw frame seams possible"
        group["observations"].append({
            "obs_id": band.obs_id, "obsid": band.obs_id, "collection": "Survey",
            "instrument": band.survey, "filter": band.band, "name": f"{band.survey} {band.band}",
            "wavelength_nm": band.wavelength_nm, "native_scale_arcsec": band.native_scale_arcsec,
            "hips_id": band.hips_id, "data_uri": "hips:" + band.hips_id,
            "preview_uri": "hips:" + band.hips_id, "region": region,
            "pixels": project(region, wcs, view["pixel_dimensions"][1]) if wcs else [],
            "ra": view["center_icrs_deg"][0], "dec": view["center_icrs_deg"][1],
            "proposal": "", "target": "", "title": f"{band.survey} {band.band}",
            "date": "", "exptime_s": 0, "info": info,
            **detail_for_view(band, view),
        })
    return list(result.values())


def grid_hash(grid: dict) -> str:
    return hashlib.sha256(json.dumps(
        [grid["width"], grid["height"], dict(WCS(grid["wcs"]).to_header())],
        sort_keys=True).encode()).hexdigest()[:32]


def sources(observation: dict, *, reachable=False) -> list[str]:
    from astroimp import survey_sources
    fallbacks = survey_sources.providers(band_for(observation))
    explicit = observation.get("survey_source")
    if explicit is not None:
        if explicit not in ["cds", *fallbacks]:
            raise ValueError(f"Unknown source for this survey: {explicit}")
        return [explicit]
    return ([] if reachable and fallbacks and time.monotonic() < _cds_down_until else ["cds"]) + fallbacks


def cache_path(observation: dict, downloads: Path, grid: dict, *, source: str | None = None) -> Path:
    folder = downloads / "surveys" / slug(band_for(observation).hips_id)
    if source is not None:
        version = IRSA_CACHE_VERSION if source == "irsa-hips-v1" else ""
        return (folder if source == "cds" else folder / source) / f"{grid_hash(grid)}{version}.fits"
    candidates = [cache_path(observation, downloads, grid, source=s) for s in sources(observation)]
    return next((path for path in candidates if path.is_file()), candidates[0])


def provenance(path: Path) -> str:
    with fits.open(path, memmap=False) as hdus:
        source = hdus[0].header.get("AIPROV", "cds")
    if source not in {"cds", "stsci-dss-v1", "stsci-ps1-v1", "irsa-hips-v1"}:
        raise ValueError(f"Unknown cached survey provenance: {source}")
    return source


def subwindow(wcs: WCS, x: int, y: int, width: int, height: int) -> WCS:
    result = wcs.deepcopy()
    result.wcs.crpix -= [x, y]
    result.pixel_shape = (width, height)
    return result


def windows(grid: dict, side: int = TILE_SIDE):
    wcs = WCS(grid["wcs"])
    for y in range(0, grid["height"], side):
        for x in range(0, grid["width"], side):
            width, height = min(side, grid["width"]-x), min(side, grid["height"]-y)
            yield x, y, subwindow(wcs, x, y, width, height)


def cutout(band: Band, wcs: WCS, progress=lambda *args: None, *, format="fits",
           attempts: int = 4, timeout: float = 120, source="cds", cache: Path | None = None):
    """Request or locally reproject one source onto the complete, possibly rotated WCS."""
    if source != "cds":
        from astroimp import survey_sources
        if format != "fits" or cache is None:
            raise ValueError("Independent survey sources require FITS output and a tile cache.")
        while not _requests.acquire(timeout=.2):
            check_cancelled(progress)
        try:
            return survey_sources.cutout(band, wcs, source, cache, progress, timeout)
        finally:
            _requests.release()
    error = None
    for attempt in range(attempts):
        check_cancelled(progress)
        if attempt:
            for _ in range(2**(attempt-1)*10):
                check_cancelled(progress)
                time.sleep(.1)
        try:
            while not _requests.acquire(timeout=.2):
                check_cancelled(progress)
            try:
                client = hips2fitsClass()
                client.server = SERVERS[attempt % len(SERVERS)]
                client.timeout = (6, timeout)
                client._session.headers["User-Agent"] = USER_AGENT
                response = client.query_with_wcs(
                    band.hips_id, wcs, format=format, stretch="asinh",
                    min_cut=.5, max_cut=99.5)
                check_cancelled(progress)
                if response is None:
                    raise ValueError("CDS returned no cutout.")
                if format == "fits":
                    with response:
                        pixels = np.array(response[0].data, dtype=np.float32)
                    if pixels.shape != wcs.array_shape:
                        raise ValueError(f"CDS cutout shape {pixels.shape} != {wcs.array_shape}")
                    note_cds_health(True)
                    return pixels
                return response
            finally:
                _requests.release()
        except CancelledError:
            raise
        except Exception as exc:
            error = exc
    note_cds_health(False)
    raise RuntimeError(f"CDS hips2fits failed for {band.survey} {band.band}: {error}") from error


def fetch(observation: dict, downloads: Path, grid: dict, progress=lambda *args: None) -> Path:
    check_cancelled(progress)
    cached = cache_path(observation, downloads, grid)
    if cached.is_file():
        return cached.resolve()
    errors = []
    for source in sources(observation, reachable=True):
        try:
            return _fetch_source(observation, downloads, grid, progress, source)
        except CancelledError:
            raise
        except (requests.RequestException, OSError, ValueError, RuntimeError) as exc:
            log.warning("Survey %s through %s failed: %s", observation["hips_id"], source, exc)
            errors.append(f"{source}: {exc}")
            progress("download", 0, f"{source} unavailable; trying the next survey source")
    pinned = (f" This built layer stays pinned to {observation['survey_source']}."
              if observation.get("survey_source") else "")
    raise RuntimeError(f"Couldn't download {band_for(observation).survey} {band_for(observation).band}."
                       + pinned + " " + "; ".join(errors))


def _fetch_source(observation: dict, downloads: Path, grid: dict, progress, source: str) -> Path:
    band = band_for(observation)
    destination = cache_path(observation, downloads, grid, source=source)
    with _locks[int(grid_hash(grid)[:4], 16) % len(_locks)]:
        check_cancelled(progress)
        if destination.exists():
            return destination.resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        temporary = destination.with_suffix(".part")
        raw = destination.with_suffix(".raw")
        image = np.memmap(raw, mode="w+", dtype=np.float32,
                          shape=(grid["height"], grid["width"]))
        try:
            tiles = list(windows(grid))
            for index, (x, y, wcs) in enumerate(tiles):
                progress("download", index/len(tiles),
                         f"Survey {band.survey} {band.band}: tile {index+1}/{len(tiles)}")
                pixels = (cutout(band, wcs, progress) if source == "cds" else cutout(
                    band, wcs, progress, source=source, cache=destination.parent / "tiles"))
                h, w = pixels.shape
                image[y:y+h, x:x+w] = pixels
            if not np.isfinite(image).any():
                raise ValueError(f"{source} has no usable pixels in this sky frame.")
            # Coarse surveys only need smoothing on the small build grid;
            # native export requests never sample finer than native.
            ratio = band.native_scale_arcsec / grid["pixel_scale_arcsec"]
            if ratio > 1.5:
                valid = np.isfinite(image)
                weights = gaussian_filter(valid.astype(np.float32), ratio*.25)
                smoothed = gaussian_filter(np.where(valid, image, 0), ratio*.25)
                np.divide(smoothed, weights, out=image, where=valid & (weights > 0))
                image[~valid] = np.nan
            check_cancelled(progress)
            header = WCS(grid["wcs"]).to_header()
            header["AIPROV"] = source
            fits.PrimaryHDU(image, header=header).writeto(temporary, overwrite=True)
            check_cancelled(progress)
            temporary.replace(destination)
        finally:
            image._mmap.close()
            raw.unlink(missing_ok=True)
            temporary.unlink(missing_ok=True)
        progress("download", 1, f"Survey {band.survey} {band.band}: {destination.stat().st_size:,} bytes")
    return destination.resolve()


def preview(observation: dict, view: dict, cache: Path, side: int = 320, *, quick: bool = False) -> Path:
    band = band_for(observation)
    grid = view_grid(view, band.native_scale_arcsec, side)
    digest = hashlib.sha256((observation["hips_id"] + grid_hash(grid)).encode()).hexdigest()
    def path_for(source):
        suffix = "" if source == "cds" else f"-{source}"
        if source == "irsa-hips-v1":
            suffix += IRSA_CACHE_VERSION
        return cache / "previews" / f"survey-{digest}-r{REPAIR_VERSION}{suffix}.jpg"
    for source in sources(observation):
        if path_for(source).is_file():
            return path_for(source)
    errors = []
    for source in sources(observation, reachable=True):
        try:
            return _preview_source(observation, grid, cache, digest, path_for(source), source, quick)
        except CancelledError:
            raise
        except (requests.RequestException, OSError, ValueError, RuntimeError) as exc:
            log.warning("Survey preview through %s failed: %s", source, exc)
            errors.append(f"{source}: {exc}")
    raise RuntimeError("Couldn't load the survey preview. " + "; ".join(errors))


def _preview_source(observation, grid, cache, digest, path, source, quick):
    from PIL import Image
    from astropy.visualization import AsinhStretch, AsymmetricPercentileInterval
    with _locks[int(digest[:4], 16) % len(_locks)]:
        if not path.exists():
            wcs = WCS(grid["wcs"])
            wcs.pixel_shape = (grid["width"], grid["height"])
            # Fetch floats and repair them so previews show what a build will contain.
            options = {"attempts": 1, "timeout": 6} if quick else {}
            if source != "cds":
                options.update(source=source, cache=cache / "survey-tiles")
            pixels = repair_defects(cutout(band_for(observation), wcs, **options))
            if not np.isfinite(pixels).any():
                raise ValueError(f"{source} has no usable pixels in this sky frame.")
            valid = np.isfinite(pixels) & (pixels != 0)
            scaled = np.zeros(pixels.shape, np.float32)
            if valid.any():
                # Same cut and stretch hips2fits used for the JPEG previews.
                lo, hi = AsymmetricPercentileInterval(.5, 99.5).get_limits(pixels[valid])
                scaled[valid] = AsinhStretch()(np.clip((pixels[valid] - lo) / max(hi - lo, 1e-12), 0, 1))
            path.parent.mkdir(parents=True, exist_ok=True)
            Image.fromarray(np.flipud(np.round(scaled * 255)).astype(np.uint8)).convert("RGB").save(
                path, quality=88)
    return path
