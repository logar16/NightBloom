from __future__ import annotations

import logging
from datetime import datetime, timezone
from pathlib import Path
from queue import Empty, Queue
from threading import Event, Lock
from time import monotonic

from astropy.coordinates import SkyCoord
from pyvo.samp import (
    SAMPHubError, SAMPHubServer, SAMPIntegratedClient, SAMP_STATUS_ERROR, SAMP_STATUS_OK,
)

from astroimp.mast import sky_position

logger = logging.getLogger(__name__)


class AladinListener:
    def __init__(self) -> None:
        self.positions: Queue[SkyCoord] = Queue(maxsize=1)
        self.lock = Lock()
        self.client = SAMPIntegratedClient(
            name="NightBloom", description="Selected sky position to MAST FITS to Siril",
            addr="127.0.0.1",
        )

    def receive(self, private_key, sender_id, msg_id, mtype, params, extra) -> None:
        try:
            coord = sky_position(float(params["ra"]), float(params["dec"]))
        except (KeyError, TypeError, ValueError) as exc:
            logger.error("Invalid SAMP position from %s: %s", sender_id, exc)
            if msg_id:
                self.client.ereply(msg_id, SAMP_STATUS_ERROR, error={"samp.errortxt": str(exc)})
            return
        # Coalesce clicks while an archive query is running, rather than queueing expensive work.
        with self.lock:
            try:
                self.positions.get_nowait()
            except Empty:
                pass
            self.positions.put_nowait(coord)
        if msg_id:
            self.client.ereply(msg_id, SAMP_STATUS_OK, result={})

    def connect(self, hub: SAMPHubServer | None = None) -> None:
        try:
            self.client.connect(hub=hub)
        except SAMPHubError as exc:
            raise RuntimeError("Cannot connect to a SAMP hub. Enable SAMP in Aladin, "
                               "or run 'uv run astroimp hub' in another existing terminal.") from exc
        self.client.bind_receive_message("coord.pointAt.sky", self.receive)

    def disconnect(self) -> None:
        self.client.disconnect()

    def next_position(self, timeout: float = 0.5) -> SkyCoord:
        coord = self.positions.get(timeout=timeout)
        # Aladin can send a stream of pointer positions; wait for a short pause.
        deadline = monotonic() + 0.5
        while True:
            remaining = deadline - monotonic()
            if remaining <= 0:
                return coord
            try:
                coord = self.positions.get(timeout=remaining)
                deadline = monotonic() + 0.5
            except Empty:
                return coord


def run_hub() -> None:
    hub = SAMPHubServer(addr="127.0.0.1", web_profile=False, label="NightBloom")
    hub.start()
    print("Local SAMP hub running. Connect Aladin, then run astroimp listen. Ctrl+C stops it.",
          flush=True)
    try:
        Event().wait()
    finally:
        hub.stop()


def listen(output: Path, *, radius_arcmin: float, max_observations: int,
           once: bool = False, timeout: float | None = None,
           field_arcmin: tuple[float, float] | None = None,
           max_area_ratio: float | None = None, show_all: bool = False) -> None:
    from requests import RequestException

    from astroimp.datasets import Framing, display_groups
    from astroimp.mast import search

    if timeout is not None and (not 0 < timeout < float("inf")):
        raise ValueError("--timeout must be finite and positive.")
    if not 0 < radius_arcmin <= 10800 or max_observations < 1:
        raise ValueError("Use a radius in (0, 10800] arcminutes and positive max-observations.")
    Framing(0, 0, field_arcmin, max_area_ratio)
    listener = AladinListener()
    listener.connect()
    print("Connected as NightBloom. In Aladin SAMP preferences, enable Reticle coordinates, "
          "then recenter the reticle on your target. Ctrl+C stops the listener.", flush=True)
    deadline = monotonic() + timeout if timeout is not None else None
    try:
        while True:
            if deadline is not None and monotonic() >= deadline:
                raise TimeoutError("No Aladin position received before --timeout.")
            try:
                coord = listener.next_position()
            except Empty:
                continue
            stamp = f"{coord.ra.deg:.6f}_{coord.dec.deg:+.6f}".replace("+", "p").replace("-", "m")
            timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
            manifest = output / f"{timestamp}_{stamp}" / "products.json"
            print(f"Searching RA {coord.ra.deg:.6f}, Dec {coord.dec.deg:.6f}...", flush=True)
            try:
                products = search(coord, manifest, radius_arcmin=radius_arcmin,
                                  max_observations=max_observations, field_arcmin=field_arcmin,
                                  max_area_ratio=max_area_ratio)
            except RequestException:
                if once:
                    raise
                logger.exception("MAST request failed for the selected position; select again to retry.")
                continue
            display_groups(products, Framing(float(coord.ra.deg), float(coord.dec.deg),
                                            field_arcmin, max_area_ratio), show_all=show_all)
            print(f"Manifest: {manifest.resolve()}", flush=True)
            if products:
                print(f'Inspect: uv run astroimp groups "{manifest}"', flush=True)
                print(f'Download a chosen set: uv run astroimp fetch "{manifest}" '
                      '--group Gxx --out data\\sessions\\target', flush=True)
            if once:
                return
            deadline = monotonic() + timeout if timeout is not None else None
    finally:
        listener.disconnect()
