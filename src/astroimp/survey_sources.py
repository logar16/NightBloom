"""Independent FITS survey sources; no stretched display tiles are used."""
from __future__ import annotations

import hashlib
import io
import logging
import math
import threading
import time
from pathlib import Path
from typing import TYPE_CHECKING

import numpy as np
import requests
from astropy import units as u
from astropy.coordinates import SkyCoord
from astropy.io import fits
from astropy.table import Table
from astropy.wcs import WCS
from astropy.wcs.utils import proj_plane_pixel_scales
from astropy_healpix import HEALPix
from reproject import reproject_interp

from astroimp.cancellation import check_cancelled

if TYPE_CHECKING:
    from astroimp.surveys import Band

IRSA = "https://irsa.ipac.caltech.edu/data/hips/CDS"
DSS = "https://archive.stsci.edu/cgi-bin/dss_search"
PS1 = "https://ps1images.stsci.edu/cgi-bin"
HEADERS = {"User-Agent": "NightBloom/1.0 (independent survey FITS cutouts)"}
DSS_BANDS = {"red": "poss2ukstu_red", "blue": "poss2ukstu_blue", "NIR": "poss2ukstu_ir"}
IRSA_BANDS = {**{f"CDS/P/allWISE/W{i}": f"{IRSA}/AllWISE/W{i}" for i in range(1, 5)},
              "CDS/P/DSS2/red": f"{IRSA}/DSS2/red"}
_locks = [threading.Lock() for _ in range(32)]
log = logging.getLogger(__name__)


def providers(band: Band) -> list[str]:
    if band.survey == "DSS2":
        return ["stsci-dss-v1"] + (["irsa-hips-v1"] if band.hips_id in IRSA_BANDS else [])
    if band.survey == "Pan-STARRS":
        return ["stsci-ps1-v1"]
    if band.survey == "WISE":
        return ["irsa-hips-v1"]
    return []


def get(url: str, *, params=None, timeout=30, progress=lambda *args: None) -> requests.Response:
    attempt = 0
    while True:
        check_cancelled(progress)
        try:
            response = requests.get(url, params=params, timeout=(6, timeout), headers=HEADERS)
            response.raise_for_status()
            return response
        except requests.RequestException as exc:
            transient = (isinstance(exc, requests.ConnectionError)
                         and not isinstance(exc, requests.ConnectTimeout)) or (
                isinstance(exc, requests.HTTPError) and exc.response is not None
                and exc.response.status_code in {500, 502, 503, 504})
            if not transient or attempt >= 2:
                raise
            log.warning("Retrying survey request %s after %s", url, exc)
            for _ in range(10 * 2**attempt):
                check_cancelled(progress)
                time.sleep(.1)
            attempt += 1


def fits_image(content: bytes) -> tuple[np.ndarray, WCS]:
    with fits.open(io.BytesIO(content)) as hdus:
        image = np.array(hdus[0].data, dtype=np.float32)
        wcs = WCS(hdus[0].header).celestial
    if image.ndim != 2 or not wcs.has_celestial:
        raise ValueError("The survey returned an image without a celestial FITS/WCS.")
    return image, wcs


def samples(wcs: WCS) -> SkyCoord:
    width, height = wcs.pixel_shape
    cx, cy = (width-1)/2, (height-1)/2
    return wcs.pixel_to_world([cx, -.5, width-.5, width-.5, -.5, cx, width-.5, cx, -.5],
                              [cy, -.5, -.5, height-.5, height-.5, -.5, cy, height-.5, cy]).icrs


def ps1_files(points: SkyCoord, filters: str, progress, timeout: float) -> Table:
    from astropy.table import vstack, unique
    tables = []
    for point in points:
        check_cancelled(progress)
        response = get(f"{PS1}/ps1filenames.py", params={
            "ra": float(point.ra.deg), "dec": float(point.dec.deg),
            "filters": filters, "type": "stack"}, timeout=timeout, progress=progress)
        table = Table.read(response.text, format="ascii")
        if not {"filename", "filter"} <= set(table.colnames):
            raise ValueError("Pan-STARRS returned invalid image metadata.")
        if len(table):
            tables.append(table)
    if not tables:
        return Table(names=["filename", "filter"], dtype=["U1", "U1"])
    return unique(vstack(tables), keys="filename")


def dss_image(band: Band, center: SkyCoord, size_arcmin: float, timeout: float,
              progress=lambda *args: None):
    return fits_image(get(DSS, params={
        "v": DSS_BANDS[band.band], "r": float(center.ra.deg), "d": float(center.dec.deg),
        "e": "J2000", "h": size_arcmin, "w": size_arcmin, "f": "fits", "c": "none",
    }, timeout=timeout, progress=progress).content)


def cached_fits(url: str, cache: Path, progress, timeout: float, *, max_age=None) -> bytes:
    path = cache / f"{hashlib.sha256(url.encode()).hexdigest()}.fits"
    lock = _locks[int(path.stem[:4], 16) % len(_locks)]
    with lock:
        check_cancelled(progress)
        if path.exists() and (max_age is None or time.time() - path.stat().st_mtime < max_age):
            return path.read_bytes()
        content = get(url, timeout=timeout, progress=progress).content
        with fits.open(io.BytesIO(content)) as hdus:
            hdus.verify("exception")
            for hdu in hdus:
                _ = hdu.data
        check_cancelled(progress)
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(".part")
        try:
            temporary.write_bytes(content)
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
        return content


def moc_intersects(content: bytes, center: SkyCoord, radius: u.Quantity) -> bool:
    with fits.open(io.BytesIO(content)) as hdus:
        header, data = hdus[1].header, hdus[1].data
        if header.get("ORDERING") != "NUNIQ" or header.get("COORDSYS") != "C":
            raise ValueError("The survey returned an unsupported coverage map.")
        if not {"UNIQ", "NPIX"} & set(data.names):
            raise ValueError("The survey returned a coverage map without NUNIQ pixels.")
        column = "UNIQ" if "UNIQ" in data.names else "NPIX"
        uniq = np.asarray(data[column], dtype=np.int64)
    limits = 4 * 4**np.arange(30, dtype=np.int64)
    orders = np.searchsorted(limits, uniq, side="right") - 1
    if np.any(orders < 0) or np.any(uniq >= 4*4**30):
        raise ValueError("The survey returned invalid NUNIQ coverage pixels.")
    for order in np.unique(orders):
        pixels = uniq[orders == order] - limits[order]
        candidates = HEALPix(nside=2**int(order), order="nested", frame="icrs").cone_search_skycoord(
            center, radius)
        if np.intersect1d(pixels, candidates).size:
            return True
    return False


def available(view: dict, cache: Path, bands: tuple[Band, ...], progress) -> tuple[list[str], list[str]]:
    from astroimp.surveys import view_grid
    grid = view_grid(view, 1, 32)
    wcs = WCS(grid["wcs"])
    wcs.pixel_shape = (grid["width"], grid["height"])
    points = samples(wcs)
    center = points[0]
    radius = center.separation(points[1:]).max()
    found, warnings = set(), []
    for band in bands:
        check_cancelled(progress)
        if band.hips_id not in IRSA_BANDS:
            continue
        try:
            content = cached_fits(f"{IRSA_BANDS[band.hips_id]}/Moc.fits",
                                  cache / "survey-coverage", progress, 15, max_age=86400)
            if moc_intersects(content, center, radius):
                found.add(band.hips_id)
        except (requests.RequestException, OSError, ValueError) as exc:
            warnings.append(f"{band.survey} {band.band}: {str(exc)[:160]}")
    for band in bands:
        if band.survey != "DSS2":
            continue
        try:
            # Probe real native coverage rather than assuming every photographic plate exists.
            for point in points:
                check_cancelled(progress)
                image, _ = dss_image(band, point, .1, 15, progress)
                if np.isfinite(image).any():
                    found.add(band.hips_id)
                    break
        except (requests.RequestException, OSError, ValueError) as exc:
            warnings.append(f"DSS2 {band.band}: {str(exc)[:160]}")
    if any(band.survey == "Pan-STARRS" for band in bands):
        try:
            files = ps1_files(points, "griz", progress, 15)
            found.update(band.hips_id for band in bands
                         if band.survey == "Pan-STARRS" and band.band in set(files["filter"]))
        except (requests.RequestException, OSError, ValueError) as exc:
            warnings.append(f"Pan-STARRS: {str(exc)[:160]}")
    return [band.hips_id for band in bands if band.hips_id in found], warnings


def native_cutout(band: Band, wcs: WCS, progress, timeout: float) -> np.ndarray:
    from astroimp.surveys import subwindow
    width, height = wcs.pixel_shape
    scale = max(proj_plane_pixel_scales(wcs)) * 3600
    # Bound native downloads to the same 4000-pixel side as the main survey pipeline.
    radius_arcsec = 4000 * .25 / 2 if band.survey == "Pan-STARRS" else 60 * 60 / 2
    side = max(1, int(radius_arcsec * math.sqrt(2) / scale) - 4)
    out = np.full((height, width), np.nan, np.float32)
    for y in range(0, height, side):
        for x in range(0, width, side):
            check_cancelled(progress)
            target = subwindow(wcs, x, y, min(side, width-x), min(side, height-y))
            points = samples(target)
            radius = points[0].separation(points[1:]).max().arcsec
            image = np.full(target.array_shape, np.nan, np.float32)
            for pixels, source_wcs in native_images(band, points, radius, progress, timeout):
                check_cancelled(progress)
                projected, _ = reproject_interp((pixels, source_wcs), target,
                                                shape_out=target.array_shape)
                valid = np.isfinite(projected) & ~np.isfinite(image)
                image[valid] = projected[valid]
            out[y:y+image.shape[0], x:x+image.shape[1]] = image
    return out


def native_images(band: Band, points: SkyCoord, radius: float, progress, timeout: float):
    if band.survey == "DSS2":
        yield dss_image(band, points[0], min(60, (2*radius+4)/60), timeout, progress)
    else:
        files = ps1_files(points, band.band, progress, timeout)
        for row in files:
            check_cancelled(progress)
            content = get(f"{PS1}/fitscut.cgi", params={
                "ra": float(points[0].ra.deg), "dec": float(points[0].dec.deg),
                "size": min(4000, math.ceil(2*radius/.25)+4),
                "format": "fits", "red": str(row["filename"])},
                timeout=timeout, progress=progress).content
            yield fits_image(content)


def hips_cutout(band: Band, wcs: WCS, cache: Path, progress, timeout: float) -> np.ndarray:
    width, height = wcs.pixel_shape
    scale = min(proj_plane_pixel_scales(wcs)) * 3600
    order = max(0, band.hips_order - max(0, math.floor(math.log2(scale/band.native_scale_arcsec))))
    hp = HEALPix(nside=512*2**order, order="nested", frame="icrs")
    out = np.full((height, width), np.nan, np.float32)
    rows = max(1, 262144 // width)  # Four interpolation indices + weights use at most 16 MiB.
    for y0 in range(0, height, rows):
        check_cancelled(progress)
        yy, xx = np.mgrid[y0:min(height, y0+rows), :width]
        sky = wcs.pixel_to_world(xx, yy).icrs
        indices, weights = hp.bilinear_interpolation_weights(sky.ra, sky.dec)
        polar = ~np.isfinite(weights).all(axis=0) & np.isfinite(sky.ra.deg) & np.isfinite(sky.dec.deg)
        if polar.any():
            indices[:, polar] = hp.skycoord_to_healpix(sky[polar])
            weights[:, polar] = 0
            weights[0, polar] = 1
        values = np.zeros(xx.shape, np.float64)
        weight_sum = np.zeros(xx.shape, np.float64)
        for neighbor, weight in zip(indices, weights):
            tile_ids = neighbor // (512*512)
            for tile_id in np.unique(tile_ids[neighbor >= 0]):
                check_cancelled(progress)
                selected = (tile_ids == tile_id) & (weight > 0)
                if not selected.any():
                    continue
                url = (f"{IRSA_BANDS[band.hips_id]}/Norder{order}/Dir{int(tile_id)//10000*10000}"
                       f"/Npix{int(tile_id)}.fits")
                try:
                    content = cached_fits(url, cache, progress, timeout)
                except requests.HTTPError as exc:
                    if exc.response is None or exc.response.status_code != 404:
                        raise
                    continue  # Sparse HiPS omit tiles outside their coverage.
                with fits.open(io.BytesIO(content)) as hdus:
                    tile = np.asarray(hdus[0].data, dtype=np.float32)
                    if tile.shape != (512, 512):
                        raise ValueError("IRSA returned an invalid FITS HiPS tile.")
                    local = neighbor[selected] % (512*512)
                    tx, ty = np.zeros_like(local), np.zeros_like(local)
                    for bit in range(9):
                        tx |= ((local >> (2*bit)) & 1) << bit
                        ty |= ((local >> (2*bit+1)) & 1) << bit
                    # HiPS FITS columns follow nested Y; rows follow reversed X.
                    samples_ = tile[511-tx, ty]
                valid = np.isfinite(samples_)
                weighted = np.where(valid, samples_, 0) * weight[selected]
                values[selected] += weighted
                weight_sum[selected] += np.where(valid, weight[selected], 0)
        np.divide(values, weight_sum, out=out[y0:y0+xx.shape[0]], where=weight_sum > 0)
    return out


def cutout(band: Band, wcs: WCS, source: str, cache: Path, progress, timeout: float) -> np.ndarray:
    if source not in providers(band):
        raise ValueError(f"{source} does not supply {band.survey} {band.band}.")
    if source == "irsa-hips-v1":
        return hips_cutout(band, wcs, cache, progress, timeout)
    return native_cutout(band, wcs, progress, timeout)
