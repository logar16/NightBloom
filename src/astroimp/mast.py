from __future__ import annotations

import json
import math
import time
import uuid
import warnings
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import requests
from astropy import units as u
from astropy.coordinates import SkyCoord
from astropy.io import fits
from astropy.table import Row, Table
from astropy.utils.exceptions import AstropyWarning
from astroquery.mast import Observations


@dataclass(frozen=True)
class Product:
    filename: str
    uri: str
    collection: str
    instrument: str
    filters: str
    observation: str
    proposal: str
    size_bytes: int
    footprint: str
    footprint_source: str = "legacy"


def text(value: object) -> str:
    return "" if value is None or np.ma.is_masked(value) else str(value).strip()


def sky_position(ra: float, dec: float) -> SkyCoord:
    if not math.isfinite(ra) or not 0 <= ra < 360:
        raise ValueError("RA must be finite and in [0, 360) degrees.")
    if not math.isfinite(dec) or not -90 <= dec <= 90:
        raise ValueError("Dec must be finite and in [-90, 90] degrees.")
    return SkyCoord(ra, dec, unit="deg", frame="icrs")


def write_json(path: Path, content: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    # Unique temp names let concurrent writers coexist.
    temporary = path.with_name(f"{path.name}.{uuid.uuid4().hex[:8]}.tmp")
    temporary.write_text(json.dumps(content, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    try:
        for attempt in range(20):
            try:
                temporary.replace(path)
                return
            except PermissionError:
                # Windows refuses to replace a file another thread has open (e.g. a status poll reading it).
                if attempt == 19:
                    raise
                time.sleep(.025)
    finally:
        temporary.unlink(missing_ok=True)


def search(
    coord: SkyCoord,
    output: Path,
    *,
    target: str = "",
    radius_arcmin: float = 2,
    max_observations: int = 50,
    field_arcmin: tuple[float, float] | None = None,
    max_area_ratio: float | None = None,
) -> list[Product]:
    if not math.isfinite(radius_arcmin) or not 0 < radius_arcmin <= 10800:
        raise ValueError("Search radius must be in (0, 10800] arcminutes.")
    if max_observations < 1:
        raise ValueError("max-observations must be positive.")
    Observations.TIMEOUT = 60
    observations = Observations.query_criteria(
        coordinates=coord,
        radius=radius_arcmin * u.arcmin,
        obs_collection=["HST", "JWST"],
        dataproduct_type="image",
        intentType="science",
        calib_level=[3],
        dataRights="PUBLIC",
    )
    total = len(observations)
    if total:
        observations.sort("obsid")
    if total > max_observations:
        print(f"Found {total} observations; inspecting the first {max_observations}. "
              "Narrow --radius or increase --max-observations for more.")
    # Keep all returned metadata: related products may have their own observation IDs.
    metadata = {text(row["obsid"]): row for row in observations}
    observations = observations[:max_observations]
    products = []
    if len(observations):
        # Product obsID can differ from the queried parent observation.
        candidates = Observations.get_product_list(observations)
        products = select_products(candidates, metadata)
    write_json(output, {
        "version": 1,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "target": target,
        "ra_deg": float(coord.icrs.ra.deg),
        "dec_deg": float(coord.icrs.dec.deg),
        "radius_arcmin": radius_arcmin,
        "field_arcmin": field_arcmin,
        "max_area_ratio": max_area_ratio,
        "observations_found": total,
        "observations_inspected": len(observations),
        "products": [asdict(product) for product in products],
    })
    return products


def select_products(candidates: Table, metadata: dict[str, Row]) -> list[Product]:
    selected: dict[str, Product] = {}
    for row in candidates:
        name = text(row["productFilename"])
        if (text(row["productType"]) != "SCIENCE"
                or not name.lower().endswith(("_i2d.fits", "_drc.fits", "_drz.fits"))
                or text(row["calib_level"]) != "3"
                or text(row["dataRights"]).upper() != "PUBLIC"):
            continue
        obs = metadata.get(text(row["obsID"]))
        footprint_source = "product"
        if obs is None:
            parent = text(row["parent_obsid"]) if "parent_obsid" in row.colnames else ""
            obs = metadata.get(parent)
            footprint_source = "parent"
        if obs is None:
            warnings.warn(f"No observation metadata for {name}; excluding it.", stacklevel=2)
            continue
        uri = text(row["dataURI"])
        size = text(row["size"])
        selected.setdefault(uri, Product(
            filename=name,
            uri=uri,
            collection=text(obs["obs_collection"]),
            instrument=text(obs["instrument_name"]),
            filters=(text(row["filters"]) if "filters" in row.colnames else "") or text(obs["filters"]),
            observation=(text(row["obs_id"]) if "obs_id" in row.colnames else "") or text(obs["obs_id"]),
            proposal=text(obs["proposal_id"]),
            size_bytes=int(size) if size else 0,
            footprint=text(obs["s_region"]),
            footprint_source=footprint_source,
        ))
    return sorted(selected.values(), key=lambda p: (
        p.collection, p.proposal, p.instrument, p.filters, p.observation, p.filename
    ))


def read_products(path: Path) -> list[Product]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("version") != 1:
        raise ValueError(f"Unsupported manifest version in {path}.")
    return [Product(**entry) for entry in manifest["products"]]


def display_products(products: list[Product]) -> None:
    if not products:
        print("No public combined HST/JWST images found in this search.")
        return
    print(" ID  Archive  Instrument       Filter           MiB  Proposal  File", flush=True)
    for index, product in enumerate(products, 1):
        size = f"{product.size_bytes / 1024**2:.1f}" if product.size_bytes else "?"
        print(f"{index:3}  {product.collection:7}  {product.instrument:15}  "
              f"{product.filters:12}  {size:>7}  {product.proposal:8}  {product.filename}")
    print("Choose matching field/program and different filters; overlap is not guaranteed.", flush=True)


def verify_fits(path: Path) -> None:
    with warnings.catch_warnings():
        warnings.simplefilter("error", AstropyWarning)
        with fits.open(path, memmap=False) as hdus:
            hdus.verify("exception")


def download(product: Product, directory: Path) -> Path:
    name = product.filename
    if (not name or name in {".", ".."} or any(c in name for c in '<>:"/\\|?*\r\n')
            or not name.lower().endswith(".fits") or name.endswith((" ", "."))):
        raise ValueError(f"Unsafe FITS filename: {name!r}")
    if not product.uri.startswith("mast:"):
        raise ValueError(f"Expected a MAST data URI for {name}.")
    if product.size_bytes < 0:
        raise ValueError(f"Invalid archive file size for {name}.")
    directory.mkdir(parents=True, exist_ok=True)
    destination = directory / name
    if destination.exists():
        if product.size_bytes and destination.stat().st_size != product.size_bytes:
            raise ValueError(f"Existing file has the wrong size: {destination}. "
                             "Move it aside before retrying.")
        verify_fits(destination)
        print(f"Reusing {destination}", flush=True)
        return destination
    temporary = destination.with_name(name + ".part")
    print(f"Downloading {name}", flush=True)
    try:
        with requests.get(
            "https://mast.stsci.edu/api/v0.1/Download/file",
            params={"uri": product.uri},
            stream=True,
            timeout=(20, 120),
        ) as response:
            response.raise_for_status()
            with temporary.open("wb") as stream:
                for chunk in response.iter_content(chunk_size=1024 * 1024):
                    stream.write(chunk)
        size = temporary.stat().st_size
        if size == 0 or (product.size_bytes and size != product.size_bytes):
            raise ValueError(f"Incomplete download of {name}: got {size} bytes, "
                             f"expected {product.size_bytes or 'a nonempty FITS file'}.")
        verify_fits(temporary)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)
    return destination
