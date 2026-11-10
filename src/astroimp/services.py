"""Background reachability of the remote archives AstroImp depends on."""
from __future__ import annotations

import re
import threading
import time
from datetime import datetime, timezone

import requests

from astroimp.surveys import MOC_SERVER, note_cds_health

# Any HTTP answer below 500 means the service is up; timeouts and 5xx mean it is not.
SERVICES = {
    "cds": f"{MOC_SERVER}?ID=CDS/P/DSS2/red&get=id&fmt=json",
    "mast": "https://mast.stsci.edu/api/v0/",
}
UP_INTERVAL, DOWN_INTERVAL, TIMEOUT = 120, 30, 8


HOSTS = (("cds.unistra.fr", "CDS"), ("u-strasbg.fr", "CDS"), ("stsci.edu", "MAST"))


def friendly(exc: BaseException) -> str:
    """One plain sentence for archive failures instead of urllib3's retry chain."""
    chain, seen = [], set()
    while exc is not None and id(exc) not in seen:
        seen.add(id(exc))
        chain.append(exc)
        exc = exc.__cause__ or exc.__context__
    text = " ".join(str(e) for e in chain)
    service = next((name for host, name in HOSTS if host in text), "The archive")
    for error in chain:
        if isinstance(error, requests.HTTPError) and error.response is not None:
            return f"{service} returned an error ({error.response.status_code}). Try again later."
        if isinstance(error, (requests.ConnectionError, requests.Timeout, TimeoutError, ConnectionError)):
            return f"{service} isn't responding. Try again in a few minutes."
    first = re.split(r"[\r\n]", str(chain[0]))[0] if chain else "Unknown error"
    return first if len(first) <= 200 else first[:197] + "…"


class Health:
    def __init__(self, services: dict[str, str] = SERVICES):
        self.services = services
        self.state: dict[str, dict] = {name: {"ok": None, "checked": None} for name in services}
        self.next_check = 0.0
        self.lock = threading.Lock()

    def snapshot(self) -> dict:
        """Current state; starts a background probe when one is due (never blocks)."""
        now = time.monotonic()
        if now >= self.next_check and self.lock.acquire(blocking=False):
            self.next_check = now + DOWN_INTERVAL
            threading.Thread(target=self._probe, name="astroimp-services", daemon=True).start()
        return {name: dict(value) for name, value in self.state.items()}

    def _probe(self) -> None:
        try:
            for name, url in self.services.items():
                try:
                    ok = requests.get(url, timeout=TIMEOUT).status_code < 500
                except requests.RequestException:
                    ok = False
                if name == "cds" and url == SERVICES["cds"]:
                    note_cds_health(ok)
                self.state[name] = {"ok": ok, "checked": datetime.now(timezone.utc).isoformat()}
            healthy = all(v["ok"] for v in self.state.values())
            self.next_check = time.monotonic() + (UP_INTERVAL if healthy else DOWN_INTERVAL)
        finally:
            self.lock.release()
