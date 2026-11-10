from __future__ import annotations

import json
import logging
import math
import re
from collections import defaultdict
from dataclasses import dataclass
from functools import cached_property, lru_cache
from pathlib import Path

import numpy as np
from astropy.coordinates import SkyCoord

from astroimp.mast import Product, read_products

logger = logging.getLogger(__name__)
SQ_ARCMIN_PER_STERADIAN = (180 * 60 / math.pi) ** 2
MAX_IMAGE_AREA_RATIO = 10.0
MIN_GOOD_FIELDS = 2


@dataclass(frozen=True)
class SkyFootprint:
    coordinates: np.ndarray

    @cached_property
    def vectors(self) -> np.ndarray:
        return SkyCoord(self.coordinates[:, 0], self.coordinates[:, 1],
                        unit="deg").cartesian.xyz.value.T

    @cached_property
    def origin(self) -> np.ndarray:
        origin = self.vectors.sum(axis=0)
        length = np.linalg.norm(origin)
        if length == 0:
            raise ValueError("Footprint has no local center.")
        origin /= length
        if np.any(self.vectors @ origin <= 0):
            raise ValueError("Footprints spanning a hemisphere are not supported.")
        return origin

    @cached_property
    def area(self) -> float:
        # Sum signed triangle solid angles, preserving concave edges and RA wrap.
        start = self.vectors
        end = np.roll(start, -1, axis=0)
        numerator = np.cross(start, end) @ self.origin
        denominator = 1 + start @ self.origin + end @ self.origin + np.sum(start * end, axis=1)
        area = abs(math.fsum((2 * np.arctan2(numerator, denominator)).tolist()))
        if not 0 < area < 2 * math.pi:
            raise ValueError("Degenerate or nonlocal footprint.")
        return area

    def contains_radec(self, ra: float, dec: float) -> bool:
        point = SkyCoord(ra, dec, unit="deg").cartesian.xyz.value
        if point @ self.origin <= 0:
            return False
        axis = np.eye(3)[np.argmin(np.abs(self.origin))]
        east = np.cross(axis, self.origin)
        east /= np.linalg.norm(east)
        north = np.cross(self.origin, east)
        basis = np.column_stack((east, north))
        # Gnomonic projection maps the polygon's great-circle edges to straight lines.
        vertices = (self.vectors @ basis) / (self.vectors @ self.origin)[:, None]
        start = vertices - (point @ basis) / (point @ self.origin)
        end = np.roll(start, -1, axis=0)
        cross = start[:, 0] * end[:, 1] - end[:, 0] * start[:, 1]
        error = np.finfo(float).eps * (
            np.abs(start[:, 0] * end[:, 1]) + np.abs(end[:, 0] * start[:, 1])
        )
        if np.any((np.abs(cross) <= error) & (np.sum(start * end, axis=1) <= 0)):
            return True
        crossing = (start[:, 1] > 0) != (end[:, 1] > 0)
        x = start[crossing, 0] - start[crossing, 1] * (
            end[crossing, 0] - start[crossing, 0]
        ) / (end[crossing, 1] - start[crossing, 1])
        return bool(np.count_nonzero(x > 0) % 2)


def image_root(filename: str) -> str:
    return re.sub(r"_(i2d|drc|drz)\.fits$", "", filename, flags=re.IGNORECASE)


def spectral_filter(product: Product) -> str | None:
    name = product.filename.lower()
    if "_total_" in name:
        return None
    # Old manifests sometimes label related products with their parent's filter.
    named = re.findall(r"(?:_|-)(f\d{3,4}[a-z0-9]*)(?=[_-])", name)
    if named:
        return "+".join(value.upper() for value in dict.fromkeys(named))
    values = [value.strip().upper() for value in re.split(r"[;+]", product.filters)]
    if not values or any(value in {"", "DETECTION", "TOTAL", "CLEAR", "UNKNOWN"} for value in values):
        return None
    return "+".join(sorted(set(values)))


def field_family(product: Product) -> tuple[str, str]:
    name = image_root(product.filename).lower()
    jwst = re.match(r"jw\d{5}-([oc]\d+_t\d+)(?:_|$)", name)
    if jwst:
        return jwst[1], "JWST mosaic"
    skycell = re.match(r"hst_(skycell-[^_]+)_", name)
    if skycell:
        return skycell[1], "HAP skycell"
    hst = re.match(r"hst_\d+_([a-z0-9]+)_", name)
    if hst:
        last = name.rsplit("_", 1)[-1]
        if last in {"pc", "wf"}:
            return f"visit-{hst[1]}", f"HLA {last.upper()}"
        return f"visit-{hst[1]}", "HAP visit mosaic"
    if product.collection.upper() == "HST" and re.fullmatch(r"[a-z0-9]{9}", name):
        return f"visit-{name[4:6]}", "Standard"
    # Unknown naming conventions must not silently merge unrelated fields.
    return product.observation or name, "Other"


@lru_cache(maxsize=512)
def parse_footprint(region: str) -> SkyFootprint | None:
    if not region.strip():
        return None
    tokens = region.split()
    if tokens[0].upper() != "POLYGON":
        logger.warning("Unsupported footprint shape %s; sky area is unknown.", tokens[0])
        return None
    if len(tokens) > 1 and tokens[1].upper() == "ICRS":
        tokens.pop(1)
    try:
        coordinates = np.asarray(tokens[1:], dtype=float).reshape(-1, 2)
        if len(coordinates) < 3 or not np.isfinite(coordinates).all():
            raise ValueError("Expected at least three finite coordinate pairs.")
        if np.any(np.abs(coordinates[:, 1]) > 90):
            raise ValueError("Invalid footprint declination.")
        coordinates[:, 0] %= 360
        coordinates = coordinates[np.r_[True, np.any(np.diff(coordinates, axis=0), axis=1)]]
        while len(coordinates) > 1 and np.array_equal(coordinates[0], coordinates[-1]):
            coordinates = coordinates[:-1]
        if len(coordinates) < 3:
            raise ValueError("Degenerate footprint.")
        polygon = SkyFootprint(coordinates)
        if not math.isfinite(polygon.area):
            raise ValueError("Nonfinite footprint area.")
        return polygon
    except (ValueError, RuntimeError) as exc:
        logger.warning("Cannot interpret archive footprint; sky area is unknown: %s", exc)
        return None


def footprint(product: Product) -> SkyFootprint | None:
    if not (product.footprint_source == "product"
            or (product.footprint_source == "legacy"
                and image_root(product.filename).lower() == product.observation.lower())):
        return None
    return parse_footprint(product.footprint)


@dataclass(frozen=True)
class Framing:
    ra: float
    dec: float
    field_arcmin: tuple[float, float] | None = None
    max_area_ratio: float | None = None

    def __post_init__(self) -> None:
        from astroimp.mast import sky_position

        sky_position(self.ra, self.dec)
        if self.field_arcmin is not None:
            if len(self.field_arcmin) != 2 or any(
                not math.isfinite(v) or not 0 < v < 10800 for v in self.field_arcmin
            ):
                raise ValueError("Field width and height must be finite and in (0, 10800) arcminutes.")
        if self.max_area_ratio is not None:
            if not math.isfinite(self.max_area_ratio) or self.max_area_ratio <= 0:
                raise ValueError("--max-area-ratio must be finite and positive.")
            if self.field_arcmin is None:
                raise ValueError("--max-area-ratio requires --field-arcmin WIDTH HEIGHT.")

    @cached_property
    def area(self) -> float | None:
        if self.field_arcmin is None:
            return None
        width, height = (value / 120 for value in self.field_arcmin)
        # Rotation to the target's coordinates preserves spherical area.
        rectangle = SkyFootprint(np.array([
            [-width, -height], [width, -height], [width, height], [-width, height],
        ]))
        return rectangle.area * SQ_ARCMIN_PER_STERADIAN


@dataclass
class Dataset:
    identifier: str
    collection: str
    proposal: str
    instrument: str
    field: str
    family: str
    members: list[tuple[int, Product]]

    def suggested(self, framing: Framing) -> list[tuple[int, Product]]:
        by_filter: dict[str, list[tuple[int, Product]]] = defaultdict(list)
        for entry in self.members:
            label = spectral_filter(entry[1])
            if label is not None:
                by_filter[label].append(entry)

        def score(entry: tuple[int, Product]) -> tuple:
            _, product = entry
            polygon = footprint(product)
            center = polygon.contains_radec(framing.ra, framing.dec) if polygon is not None else None
            room = (polygon.area * SQ_ARCMIN_PER_STERADIAN >= framing.area
                    if polygon is not None and framing.area is not None else None)
            suitable_size = (
                polygon.area * SQ_ARCMIN_PER_STERADIAN <= MAX_IMAGE_AREA_RATIO * framing.area
                if polygon is not None and framing.area is not None else None
            )
            return (
                {True: 0, None: 1, False: 2}[center],
                {True: 0, None: 1, False: 2}[room],
                {True: 0, None: 1, False: 2}[suitable_size],
                # Prefer charge-transfer-corrected drc over its drz alternative.
                product.filename.lower().endswith("_drz.fits"),
                product.size_bytes if product.size_bytes > 0 else math.inf,
                product.filename,
            )

        return [min(by_filter[label], key=score) for label in sorted(by_filter)]


def load_selection(path: Path, field_arcmin: list[float] | None = None,
                   max_area_ratio: float | None = None) -> tuple[list[Product], Framing]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    field = field_arcmin if field_arcmin is not None else manifest.get("field_arcmin")
    if field is not None and len(field) != 2:
        raise ValueError("Expected exactly two field dimensions.")
    threshold = max_area_ratio if max_area_ratio is not None else manifest.get("max_area_ratio")
    framing = Framing(float(manifest["ra_deg"]), float(manifest["dec_deg"]),
                      tuple(field) if field is not None else None, threshold)
    return read_products(path), framing


def group_datasets(products: list[Product]) -> list[Dataset]:
    groups: dict[tuple[str, ...], list[tuple[int, Product]]] = defaultdict(list)
    for index, product in enumerate(products, 1):
        if spectral_filter(product) is not None:
            field, family = field_family(product)
            instrument = product.instrument
            if instrument.startswith("WFPC2/") and family in {"HLA PC", "HLA WF"}:
                instrument = f"WFPC2/{family.split()[-1]}"
            groups[(product.collection, product.proposal, instrument,
                    field, family)].append((index, product))
    return [Dataset(f"G{index:02}", *key, members)
            for index, (key, members) in enumerate(sorted(groups.items()), 1)]


def get_dataset(groups: list[Dataset], identifier: str) -> Dataset:
    for group in groups:
        if group.identifier == identifier.upper():
            return group
    raise ValueError(f"Unknown dataset {identifier!r}. Run 'astroimp groups' to see available IDs.")


def size_status(selected: list[tuple[int, Product]], framing: Framing) -> tuple[bool, bool]:
    wanted = framing.area
    if wanted is None:
        return False, False
    polygons = [footprint(product) for _, product in selected]
    known = [polygon for polygon in polygons if polygon is not None]
    oversized = any(polygon.area * SQ_ARCMIN_PER_STERADIAN > MAX_IMAGE_AREA_RATIO * wanted
                    for polygon in known)
    good = len(selected) >= 2 and len(known) == len(selected) and all(
        polygon.contains_radec(framing.ra, framing.dec)
        and wanted <= polygon.area * SQ_ARCMIN_PER_STERADIAN <= MAX_IMAGE_AREA_RATIO * wanted
        for polygon in known
    )
    return oversized, good


def filter_oversized_groups(groups: list[Dataset], framing: Framing, *,
                            show_all: bool = False) -> tuple[list[Dataset], int, int]:
    oversized_ids = set()
    good_fields = set()
    for group in groups:
        oversized, good = size_status(group.suggested(framing), framing)
        if oversized:
            oversized_ids.add(group.identifier)
        if good:
            # Reductions of the same field are alternatives, not independent good results.
            good_fields.add((group.collection, group.proposal, group.instrument, group.field))
    good_count = len(good_fields)
    if show_all or good_count < MIN_GOOD_FIELDS:
        return groups, good_count, len(oversized_ids)
    visible = [group for group in groups if group.identifier not in oversized_ids]
    return visible, good_count, len(oversized_ids)


def area_summary(selected: list[tuple[int, Product]], framing: Framing) -> tuple[str, str]:
    polygons = [footprint(product) for _, product in selected]
    known = [polygon for polygon in polygons if polygon is not None]
    centers = [polygon.contains_radec(framing.ra, framing.dec) for polygon in known]
    center = "no" if not all(centers) else ("yes" if len(known) == len(polygons) else "?")
    oversized, _ = size_status(selected, framing)
    if len(known) != len(polygons):
        return "? OVERSIZED" if oversized else "?", center
    areas = [polygon.area * SQ_ARCMIN_PER_STERADIAN for polygon in known]
    wanted = framing.area
    if wanted is None:
        return f"{min(areas):.1f}-{max(areas):.1f}", center
    ratios = [area / wanted for area in areas]
    label = f"{min(ratios):.1f}-{max(ratios):.1f}x"
    if framing.max_area_ratio is not None and max(ratios) > framing.max_area_ratio:
        label += " LARGE"
    if oversized:
        label += " OVERSIZED"
    if min(ratios) < 1:
        label += " SMALL"
    return label, center


def display_groups(products: list[Product], framing: Framing, identifier: str | None = None, *,
                   show_all: bool = False) -> None:
    groups = group_datasets(products)
    excluded = sum(spectral_filter(product) is None for product in products)
    print(f"{len(products)} files -> {len(groups)} candidate sets; "
          f"{excluded} detection/unknown-filter files excluded from suggestions.", flush=True)
    if identifier is not None:
        group = get_dataset(groups, identifier)
        chosen = group.suggested(framing)
        print(f"{group.identifier}: {group.collection} {group.proposal} {group.instrument} "
              f"{group.field}, {group.family}")
        if size_status(chosen, framing)[0]:
            print("OVERSIZED: a suggested image exceeds 10x the view area. "
                  "Explicit --group inspection bypasses the summary filter.", flush=True)
        print("Suggested file IDs:", " ".join(str(index) for index, _ in chosen))
        print("All alternatives (original manifest IDs):")
        for index, product in group.members:
            print(f"  {index:3}  {spectral_filter(product):14}  "
                  f"{product.size_bytes / 1024**2:8.1f} MiB  {product.filename}")
        return
    if not groups:
        return
    visible, good_count, oversized_count = filter_oversized_groups(groups, framing, show_all=show_all)
    if framing.area is None:
        print("Size filter inactive: view dimensions are unknown. "
              "Use --field-arcmin WIDTH HEIGHT.", flush=True)
    elif oversized_count:
        if show_all:
            print(f"--show-all: retaining {oversized_count} oversized sets.", flush=True)
        elif good_count < MIN_GOOD_FIELDS:
            print(f"Only {good_count} good multi-filter fields (need {MIN_GOOD_FIELDS}); "
                  f"retaining {oversized_count} oversized sets as fallbacks.", flush=True)
        else:
            print(f"Hid {oversized_count} oversized sets: a suggested image exceeds 10x view area. "
                  "Use --show-all to include them.", flush=True)
    print("Set  Archive Program Instrument   Field         Product family      "
          "Filters                     GiB    Sky area / needed    Center  File IDs")
    rows = []
    for group in visible:
        selected = group.suggested(framing)
        total = sum(product.size_bytes for _, product in selected)
        size = f"{total / 1024**3:.2f}" + ("+?" if any(p.size_bytes <= 0 for _, p in selected) else "")
        area, center = area_summary(selected, framing)
        rows.append((group, selected, total, size, area, center))
    rows.sort(key=lambda row: (
        {"yes": 0, "?": 1, "no": 2}[row[5]], len(row[1]) < 2,
        any(p.size_bytes <= 0 for _, p in row[1]), row[2], row[0].identifier,
    ))
    for group, selected, total, size, area, center in rows:
        labels = ",".join(spectral_filter(p) or "?" for _, p in selected)
        ids = " ".join(str(index) for index, _ in selected)
        print(f"{group.identifier:4} {group.collection:7} {group.proposal:7} {group.instrument:12} "
              f"{group.field:13} {group.family:19} {labels:26} {size:>7} "
              f"{area:>20} {center:>7}  {ids}", flush=True)
    print("One suggested file per filter; product families stay separate. "
          "Use groups --group Gxx for alternatives; list retains every raw file ID.", flush=True)
    print("Sorted by confirmed center coverage, multiple filters, then download size. "
          "This is not an image-quality/SNR ranking.", flush=True)
    print("Sky area is observed footprint area (arcmin^2 without --field-arcmin). "
          "Center coverage does not guarantee coverage of the whole desired frame.", flush=True)
    if any(area.startswith("?") for _, _, _, _, area, _ in rows):
        print("? = product footprint unavailable/untrusted; parent observation footprints "
              "are not used to claim an image's size.", flush=True)
