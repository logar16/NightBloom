from __future__ import annotations

import base64
import math
import struct
from copy import deepcopy
from dataclasses import dataclass, field
from datetime import datetime, timezone

from astropy.io import fits
from astropy.wcs import WCS
import numpy as np
from pyvo.samp import SAMPHubError, SAMPIntegratedClient


@dataclass(frozen=True)
class AladinView:
    center_icrs_deg: tuple[float, float]
    field_arcmin: tuple[float, float]
    corners_icrs_deg: tuple[tuple[float, float], ...]
    pixel_dimensions: tuple[int, int]
    header_pixel_dimensions: tuple[int, int]
    projection: tuple[str, str]
    snapshot_bytes: int
    source: str = "Aladin snapshot.get.jpg embedded FITS/WCS"
    captured_at_utc: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())
    rotation_deg: float = 0


@dataclass(frozen=True)
class Snapshot:
    view: AladinView
    jpeg: bytes
    header: fits.Header


def rotated_wcs(header: fits.Header, pixel_dimensions, rotation_deg: float = 0) -> WCS:
    """Correct Aladin's unrotated WCS about the JPEG centre, never about its CRPIX.

    Positive angles rotate the compass clockwise in the snapshot: +90° puts N
    right and E up. Equivalently, snapshot up has position angle east of north
    for a north-up source header. Zero leaves the supplied WCS exactly intact.
    """
    wcs = WCS(header).celestial
    angle = float(rotation_deg)
    if not math.isfinite(angle) or not -180 <= angle <= 180:
        raise ValueError("Rotation must be between −180° and 180°.")
    if angle:
        radians = math.radians(angle)
        c, s = math.cos(radians), math.sin(radians)
        rotation = np.array([[c, s], [-s, c]])
        center = (np.asarray(pixel_dimensions, float)+1)/2
        original = wcs.pixel_scale_matrix
        matrix = rotation @ original
        wcs.wcs.crpix = center + np.linalg.solve(matrix, original @ (wcs.wcs.crpix-center))
        if wcs.wcs.has_cd():
            del wcs.wcs.cd
        wcs.wcs.cdelt = [1, 1]
        wcs.wcs.pc = matrix
    return wcs


def rotated_view(original: dict, header: fits.Header, rotation_deg: float) -> dict:
    """Derive each angle from immutable capture metadata; reset is lossless."""
    wcs = rotated_wcs(header, original["pixel_dimensions"], rotation_deg)
    result = deepcopy(original)
    result["rotation_deg"] = float(rotation_deg)
    if not rotation_deg:
        return result
    width, height = original["pixel_dimensions"]
    cx, cy = (width-1)/2, (height-1)/2
    points = wcs.pixel_to_world(
        [cx, -.5, width-.5, cx, cx, -.5, width-.5, width-.5, -.5],
        [cy, cy, cy, -.5, height-.5, -.5, -.5, height-.5, height-.5]).icrs
    result["center_icrs_deg"] = [float(points[0].ra.deg), float(points[0].dec.deg)]
    result["field_arcmin"] = [float(points[1].separation(points[2]).arcmin),
                            float(points[3].separation(points[4]).arcmin)]
    result["corners_icrs_deg"] = [[float(p.ra.deg), float(p.dec.deg)] for p in points[5:]]
    return result


def snapshot_geometry(value: str) -> AladinView:
    return parse_snapshot(value).view


def parse_snapshot(value: str) -> Snapshot:
    prefix = "data:image/jpeg;base64,"
    if not value.startswith(prefix):
        raise ValueError("Aladin returned an unexpected snapshot format.")
    encoded = value[len(prefix):].translate(str.maketrans("", "", " \t\r\n"))
    image = base64.b64decode(encoded, validate=True)
    if not image.startswith(b"\xff\xd8"):
        raise ValueError("Aladin snapshot is not a JPEG.")
    position = 2
    header = None
    dimensions = None
    while position < len(image):
        if image[position] != 255:
            raise ValueError("Invalid JPEG marker.")
        while position < len(image) and image[position] == 255:
            position += 1
        if position >= len(image):
            raise ValueError("Truncated JPEG marker.")
        marker = image[position]
        position += 1
        if marker in (0xda, 0xd9):
            break
        if marker == 0x01 or 0xd0 <= marker <= 0xd7:
            continue
        if position + 2 > len(image):
            raise ValueError("Truncated JPEG segment length.")
        length = struct.unpack_from(">H", image, position)[0]
        if length < 2 or position + length > len(image):
            raise ValueError("Invalid JPEG segment length.")
        payload = image[position + 2:position + length]
        if marker == 0xfe and payload.startswith(b"SIMPLE"):
            header = fits.Header.fromstring(payload.decode("ascii"), sep="\n")
        if marker in (0xc0, 0xc1, 0xc2):
            if len(payload) < 6:
                raise ValueError("Truncated JPEG dimensions.")
            height, width = struct.unpack_from(">HH", payload, 1)
            dimensions = width, height
        position += length
    if header is None or dimensions is None:
        raise ValueError("Snapshot lacks FITS/WCS metadata or pixel dimensions.")
    if min(dimensions) <= 0:
        raise ValueError("Snapshot has invalid pixel dimensions.")
    header_dimensions = int(header["NAXIS1"]), int(header["NAXIS2"])
    if "RADECSYS" in header and "RADESYS" not in header:
        header.rename_keyword("RADECSYS", "RADESYS")
    wcs = WCS(header).celestial
    if not wcs.has_celestial:
        raise ValueError("Snapshot has no celestial WCS.")
    width, height = dimensions
    cx, cy = (width - 1) / 2, (height - 1) / 2
    # Aladin 12.060's header NAXIS values are one pixel too large; use the JPEG dimensions.
    points = wcs.pixel_to_world(
        [cx, -.5, width - .5, cx, cx, -.5, width - .5, width - .5, -.5],
        [cy, cy, cy, -.5, height - .5, -.5, -.5, height - .5, height - .5],
    ).icrs
    center = float(points[0].ra.deg), float(points[0].dec.deg)
    angular_size = (float(points[1].separation(points[2]).arcmin),
                    float(points[3].separation(points[4]).arcmin))
    corners = tuple((float(p.ra.deg), float(p.dec.deg)) for p in points[5:])
    if (not all(math.isfinite(v) for pair in (center, *corners) for v in pair)
            or not all(math.isfinite(v) and v > 0 for v in angular_size)):
        raise ValueError("Snapshot has invalid celestial coordinates or angular dimensions.")
    view = AladinView(
        center, angular_size, corners, dimensions, header_dimensions,
        (wcs.wcs.ctype[0], wcs.wcs.ctype[1]), len(image),
    )
    return Snapshot(view, image, header)


def read_view() -> AladinView:
    return read_snapshot().view


def clients(client, mtype: str, *, check: bool = False) -> list[str]:
    recipients = [r for r in client.get_subscribed_clients(mtype)
                  if "aladin" in client.get_metadata(r).get("samp.name", "").lower()]
    if len(recipients) <= 1 and not check:
        return recipients
    responsive = []
    for recipient in recipients:
        try:
            response = client.call_and_wait(recipient, {
                "samp.mtype": "samp.app.ping", "samp.params": {}}, "1")
            if response.get("samp.status") == "samp.ok":
                responsive.append(recipient)
        except Exception:
            continue
    return responsive


def ping() -> bool:
    try:
        client = SAMPIntegratedClient(name="NightBloom status", addr="127.0.0.1")
        client.connect()
    except Exception:
        return False
    try:
        return bool(clients(client, "snapshot.get.jpg", check=True))
    finally:
        client.disconnect()


def read_snapshot() -> Snapshot:
    client = SAMPIntegratedClient(name="NightBloom view readback", addr="127.0.0.1")
    try:
        client.connect()
    except SAMPHubError as exc:
        raise RuntimeError("Cannot connect to a SAMP hub. Enable SAMP in Aladin, "
                           "or run 'uv run astroimp hub' in another existing terminal.") from exc
    try:
        recipients = clients(client, "snapshot.get.jpg")
        if len(recipients) != 1:
            raise RuntimeError(f"Expected one SAMP-connected Aladin; found {len(recipients)}. "
                               "Connect exactly one Aladin using 'Connect with SAMP'.")
        response = client.call_and_wait(recipients[0], {
            "samp.mtype": "snapshot.get.jpg", "samp.params": {},
        }, "10")
        if response.get("samp.status") != "samp.ok":
            raise RuntimeError(f"Aladin snapshot failed: {response}")
        value = response.get("samp.result", {}).get("data")
        if not isinstance(value, str):
            raise RuntimeError("Aladin snapshot response is missing its image data.")
        return parse_snapshot(value)
    finally:
        client.disconnect()
