"""Persistent Studio projects and staged, project-scoped background jobs."""
from __future__ import annotations

import json
import math
import os
import re
import shutil
import threading
import time
import traceback
import uuid
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable
from collections import Counter

import requests
from astropy import units as u
from astropy.coordinates import SkyCoord
from astropy.io import fits
from astroquery.simbad import Simbad

from astroimp import aladin
from astroimp.build import BUILD_VERSION
from astroimp.catalog import catalog, project, snapshot_wcs
from astroimp.cancellation import CancelledError, check_cancelled
from astroimp.mast import write_json
from astroimp import surveys
from astroimp.services import friendly


def read_json(path: Path, default=None):
    for attempt in range(20):
        try:
            return json.loads(path.read_text(encoding="utf-8")) if path.exists() else default
        except PermissionError:
            # Windows can briefly deny access while another thread replaces the file.
            if attempt == 19:
                raise
            time.sleep(.025)


def observations(content: dict) -> dict:
    return {o["obs_id"]: o for g in content.get("groups", []) for o in g["observations"]}


def fallback_name(view: dict, content: dict) -> str:
    targets = Counter(o.get("target") or o.get("target_name") for o in observations(content).values())
    targets.pop(None, None)
    targets.pop("", None)
    if targets:
        return targets.most_common(1)[0][0]
    ra, dec = view.get("center_icrs_deg", (0, 0))
    return f"RA {ra:.2f} Dec {dec:.2f}".replace("-", "−")


def catalog_summary(path: Path, content: dict) -> dict:
    stat = (path / "catalog.json").stat()
    targets = [t for g in content.get("groups", []) for t in g.get("targets", [])]
    return {"signature": [stat.st_mtime_ns, stat.st_size],
            "observations": len(observations(content)), "groups": len(content.get("groups", [])),
            "target_hint": targets[0] if targets else "",
            "fallback_name": fallback_name(read_json(path / "view.json", {}), content)}


def save_catalog_summary(path: Path, content: dict) -> dict:
    """Persist legacy-session summaries without treating a cache fill as a user edit."""
    metadata = path / "session.json"
    stat = (metadata if metadata.exists() else path / "catalog.json").stat()
    saved = read_json(metadata, {})
    saved["catalog_summary"] = catalog_summary(path, content)
    write_json(metadata, saved)
    os.utime(metadata, ns=(stat.st_atime_ns, stat.st_mtime_ns))
    return saved


def simbad_name(view: dict) -> str | None:
    center = SkyCoord(*view["center_icrs_deg"], unit="deg")
    radius = min(math.hypot(*view["field_arcmin"]) / 2, 30)
    query = Simbad()
    query.TIMEOUT = 10
    query.add_votable_fields("ids")
    table = query.query_region(center, radius=radius * u.arcmin)
    candidates = []
    if table is None:
        return None
    designation = re.compile(
        r"(?:M\s*\d+|NGC\s*\d+[A-Za-z]?|IC\s*\d+[A-Za-z]?|Sh\s*2-\s*\d+"
        r"|Barnard\s*\d+|Caldwell\s*\d+)", re.I)
    for row in table:
        identifiers = [re.sub(r"\s+", " ", value.strip())
                       for value in str(row["ids"]).split("|")]
        catalog_ids = [value for value in identifiers if designation.fullmatch(value)]
        names = [value[5:] for value in identifiers if value.startswith("NAME ")]
        if not catalog_ids and not any(re.search(r"\b(nebula|cluster)\b", n, re.I) for n in names):
            continue
        main = re.sub(r"\s+", " ", str(row["main_id"]).strip())
        label = main
        if names:
            common = next((n for n in names if re.search(r"\b(nebula|cluster)\b", n, re.I)), names[0])
            short = next((re.sub(r"\s+", "", value) for value in catalog_ids
                          if re.fullmatch(r"M\s*\d+", value)), main)
            label = f"{common} ({short})"
        separation = center.separation(SkyCoord(float(row["ra"]), float(row["dec"]), unit="deg"))
        candidates.append((separation.deg, label))
    return min(candidates)[1] if candidates else None


def auto_name(view: dict, content: dict) -> str:
    # Bound the entire resolver, including TAP capabilities/metadata requests.
    result = []
    finished = threading.Event()

    def resolve():
        try:
            result.append(simbad_name(view))
        except Exception:
            pass
        finally:
            finished.set()
    threading.Thread(target=resolve, name="astroimp-name", daemon=True).start()
    finished.wait(10)
    return (result[0] if result else None) or fallback_name(view, content)


def _finite(value):
    """Archive rows occasionally carry NaN/inf (e.g. unknown exposure); JSON needs null."""
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {k: _finite(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_finite(v) for v in value]
    return value


# Routing uses the longest view side, not the enclosing cone radius.
SURVEY_ONLY_WIDTH_ARCMIN = 30


def search_view(view: dict, cache: Path, progress=lambda *args: None,
                header=None) -> dict:
    search_mast = (not view.get("surveys_only") and
                   max(view["field_arcmin"]) <= SURVEY_ONLY_WIDTH_ARCMIN)
    content = {"groups": [], "observations_found": 0, "observations_in_view": 0}
    if search_mast:
        progress("search", 0, "Searching public HST/JWST observations")
        content = _finite(catalog(view["center_icrs_deg"], view["corners_icrs_deg"],
                                  header, view["pixel_dimensions"]))
    check_cancelled(progress)
    progress("search", .8 if search_mast else 0, "Checking sky-survey coverage")
    try:
        bands = surveys.available(view, cache, progress,
                                  warn=lambda message: content.update(survey_warning=message))
        content["groups"].extend(surveys.groups(view, bands, snapshot_wcs(header) if header else None))
    except CancelledError:
        raise
    except (requests.RequestException, ValueError, RuntimeError) as exc:
        content["survey_warning"] = f"Couldn't check survey coverage. {friendly(exc)}"
    content["mast_searched"] = search_mast
    content["observations_in_view"] = sum(len(g["observations"]) for g in content["groups"])
    return content


def search_surveys(path: Path, progress=lambda *args: None) -> dict:
    """Retry sky-survey coverage only, keeping HST/JWST groups, triage and selection."""
    view = read_json(path / "view.json")
    header = snapshot_header(path)
    if header is not None:
        header = aladin.rotated_wcs(header, view["pixel_dimensions"],
                                    view.get("rotation_deg", 0)).to_header()
    progress("search", 0, "Checking sky-survey coverage")
    content = read_json(path / "catalog.json", {})
    groups = [g for g in content.get("groups", []) if g.get("collection") != "Survey"]
    try:
        content.pop("survey_warning", None)
        bands = surveys.available(view, path.parent.parent / "cache", progress,
                                  warn=lambda message: content.update(survey_warning=message))
        groups += surveys.groups(view, bands, snapshot_wcs(header) if header else None)
    except CancelledError:
        raise
    except (requests.RequestException, ValueError, RuntimeError) as exc:
        content["survey_warning"] = f"Couldn't check survey coverage. {friendly(exc)}"
        groups += [g for g in content.get("groups", []) if g.get("collection") == "Survey"]
    content["groups"] = groups
    content["observations_in_view"] = len(observations(content))
    check_cancelled(progress)
    write_json(path / "catalog.json", content)
    save_catalog_summary(path, content)
    progress("search", 1, content.get("survey_warning") or "Survey coverage updated")
    return {"warning": content.get("survey_warning")}


def search_mast(path: Path, progress=lambda *args: None) -> dict:
    view = read_json(path / "view.json")
    progress("search", 0, "Searching public HST/JWST observations")
    header = snapshot_header(path)
    if header is not None:
        header = aladin.rotated_wcs(header, view["pixel_dimensions"],
                                    view.get("rotation_deg", 0)).to_header()
    found = _finite(catalog(view["center_icrs_deg"], view["corners_icrs_deg"],
                            header, view["pixel_dimensions"]))
    content = read_json(path / "catalog.json", {})
    survey_groups = [g for g in content.get("groups", []) if g.get("collection") == "Survey"]
    # Preserve survey groups, triage and selection; IDs for MAST groups stay stable.
    content.update(found)
    content["groups"] = found["groups"] + survey_groups
    content["mast_searched"] = True
    content["observations_in_view"] = len(observations(content))
    check_cancelled(progress)
    write_json(path / "catalog.json", content)
    save_catalog_summary(path, content)
    progress("search", 1, f"Found {len(found['groups'])} HST/JWST groups")
    return {"groups": len(found["groups"])}


def snapshot_header(path: Path) -> fits.Header | None:
    header = read_json(path / "snapshot-wcs.json")
    if header is None and (path / "snapshot.jpg").exists():
        import base64
        try:
            embedded = "data:image/jpeg;base64," + base64.b64encode(
                (path / "snapshot.jpg").read_bytes()).decode("ascii")
            return aladin.parse_snapshot(embedded).header
        except ValueError:
            pass
    if header is None:
        return None
    return (fits.Header.fromstring(header["fits_header"]) if "fits_header" in header
            else fits.Header(header))


def save_snapshot_header(path: Path, header: fits.Header) -> None:
    # FITS commentary can contain repeated cards, and is not JSON-serializable as a dict.
    write_json(path / "snapshot-wcs.json", {"fits_header": fits.Header(header).tostring()})


def rotate_capture(path: Path, rotation_deg: float, progress=lambda *args: None) -> dict:
    current = read_json(path / "view.json")
    if current is None:
        raise ValueError("This session has no captured view.")
    original = read_json(path / "snapshot-view.json", current)
    header = snapshot_header(path)
    if header is None:
        raise ValueError("This capture has no original WCS. Capture the view again.")
    view = aladin.rotated_view(original, header, rotation_deg)
    rotated = aladin.rotated_wcs(header, view["pixel_dimensions"], rotation_deg)
    content = search_view(view, path.parent.parent / "cache", progress, rotated.to_header())
    old = read_json(path / "catalog.json", {})
    if content.get("survey_warning"):
        bands = [surveys.band_for(o) for o in observations(old).values() if surveys.is_survey(o)]
        content["groups"].extend(surveys.groups(view, bands, rotated))
        content["observations_in_view"] = len(observations(content))
    ids = observations(content).keys()
    selected = read_json(path / "selection.json", {}).get("obs_ids", [])
    check_cancelled(progress)
    write_json(path / "snapshot-view.json", original)
    # Legacy captures may only carry the header in the JPEG.
    if not (path / "snapshot-wcs.json").exists():
        save_snapshot_header(path, header)
    write_json(path / "view.json", view)
    write_json(path / "catalog.json", content)
    write_json(path / "selection.json", {"obs_ids": [key for key in selected if key in ids]})
    save_catalog_summary(path, content)
    progress("search", 1, "Capture rotation updated")
    return {"rotation_deg": rotation_deg, "groups": len(content["groups"])}


def capture(root: Path = Path("data/sessions"), progress=lambda *args: None,
            *, destination: Path | None = None) -> Path:
    reserved = destination is not None
    progress("snapshot", 0, "Reading Aladin view")
    snapshot = aladin.read_snapshot()
    progress("snapshot", 1, "Snapshot received")
    content = search_view(asdict(snapshot.view), root.parent / "cache", progress, snapshot.header)
    root.mkdir(parents=True, exist_ok=True)
    identifier = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    if destination is None:
        destination = root / identifier
        if destination.exists():
            destination = root / f"{identifier}-{uuid.uuid4().hex[:6]}"
    check_cancelled(progress)
    destination.mkdir(exist_ok=True)
    try:
        (destination / "snapshot.jpg").write_bytes(snapshot.jpeg)
        write_json(destination / "view.json", asdict(snapshot.view))
        write_json(destination / "snapshot-view.json", asdict(snapshot.view))
        save_snapshot_header(destination, snapshot.header)
        write_json(destination / "catalog.json", content)
        write_json(destination / "selection.json", {"obs_ids": []})
        write_json(destination / "session.json", {
            "name": auto_name(asdict(snapshot.view), content), "auto_named": True,
            "created": snapshot.view.captured_at_utc,
            "catalog_summary": catalog_summary(destination, content)})
        progress("search", 1, f"Found {len(content['groups'])} groups")
    except BaseException:
        if not reserved:
            shutil.rmtree(destination)
        raise
    return destination


def capture_coordinates(root: Path, ra: float, dec: float, width_arcmin: float = 10,
                        aspect: str = "16:9", rotation_deg: float = 0,
                        name: str = "", surveys_only: bool = False,
                        progress=lambda *args: None, *, destination: Path | None = None) -> Path:
    from astroimp.coordinates import synthetic_view
    original, view, header, rotated = synthetic_view(ra, dec, width_arcmin, aspect, rotation_deg)
    original["surveys_only"] = view["surveys_only"] = surveys_only
    progress("snapshot", 1, "Coordinate frame ready")
    content = search_view(view, root.parent / "cache", progress, rotated.to_header())
    reserved = destination is not None
    destination = destination or root / (
        datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:6])
    check_cancelled(progress)
    destination.mkdir(parents=True, exist_ok=True)
    try:
        write_json(destination / "view.json", view)
        write_json(destination / "snapshot-view.json", original)
        save_snapshot_header(destination, header)
        write_json(destination / "catalog.json", content)
        write_json(destination / "selection.json", {"obs_ids": []})
        write_json(destination / "session.json", {
            "name": name.strip()[:200] or f"RA {ra:.4f} Dec {dec:.4f}",
            "created": view["captured_at_utc"],
            "catalog_summary": catalog_summary(destination, content)})
    except BaseException:
        if not reserved:
            shutil.rmtree(destination)
        raise
    progress("search", 1, f"Found {len(content['groups'])} groups")
    return destination


def recapture(path: Path, progress=lambda *args: None) -> dict:
    """Replace an unbuilt Aladin capture with Aladin's current view, in the same project."""
    if (path / "project" / "meta.json").exists():
        raise LookupError("This project is built, so its view is locked. Use New project for a new view.")
    if read_json(path / "view.json", {}).get("source") == "coordinates":
        raise ValueError("Coordinate projects have no Aladin view to recapture.")
    progress("snapshot", 0, "Reading Aladin view")
    snapshot = aladin.read_snapshot()
    progress("snapshot", 1, "Snapshot received")
    view = asdict(snapshot.view)
    content = search_view(view, path.parent.parent / "cache", progress, snapshot.header)
    ids = observations(content).keys()
    selected = read_json(path / "selection.json", {}).get("obs_ids", [])
    meta = read_json(path / "session.json", {})
    old_summary = meta.get("catalog_summary", {})
    # Keep names the user typed; refresh ones AstroImp generated.
    generated = meta.get("auto_named") or meta.get("name") in {
        None, "New capture", old_summary.get("fallback_name")}
    check_cancelled(progress)
    (path / "snapshot.jpg").write_bytes(snapshot.jpeg)
    write_json(path / "view.json", view)
    write_json(path / "snapshot-view.json", view)
    save_snapshot_header(path, snapshot.header)
    write_json(path / "catalog.json", content)
    write_json(path / "selection.json", {"obs_ids": [key for key in selected if key in ids]})
    if generated:
        meta.update(name=auto_name(view, content), auto_named=True)
    meta["catalog_summary"] = catalog_summary(path, content)
    write_json(path / "session.json", meta)
    progress("search", 1, f"Found {len(content['groups'])} groups")
    return {"session": path.name, "name": meta.get("name")}


def refresh_snapshot(path: Path) -> dict:
    """Retake only the Aladin image (e.g. after changing survey/contrast) and re-project footprints.

    The archive search stays valid only while Aladin still shows the captured area.
    """
    old = read_json(path / "view.json")
    if old is None:
        raise ValueError("This session has no captured Aladin view.")
    if old.get("source") == "coordinates":
        raise ValueError("Coordinate projects have no Aladin snapshot.")
    snapshot = aladin.read_snapshot()
    base = asdict(snapshot.view)
    header = aladin.rotated_wcs(snapshot.header, snapshot.view.pixel_dimensions,
                               old.get("rotation_deg", 0)).to_header()
    rotated = aladin.rotated_view(base, snapshot.header, old.get("rotation_deg", 0))
    view = snapshot.view
    moved = SkyCoord(*old["center_icrs_deg"], unit="deg").separation(
        SkyCoord(*view.center_icrs_deg, unit="deg")).arcmin
    scale = math.hypot(*view.field_arcmin) / math.hypot(*old["field_arcmin"])
    if moved > 0.1 * min(old["field_arcmin"]) or not 0.8 <= scale <= 1.25:
        raise LookupError("Aladin no longer shows the captured area. Return to it, "
                          "or use New project to search the new area.")
    content = read_json(path / "catalog.json", {})
    wcs = snapshot_wcs(header)
    for observation in observations(content).values():
        observation["pixels"] = project(observation.get("region", ""), wcs, view.pixel_dimensions[1])
    (path / "snapshot.jpg").write_bytes(snapshot.jpeg)
    save_snapshot_header(path, snapshot.header)
    write_json(path / "snapshot-view.json", base)
    write_json(path / "view.json", rotated)
    write_json(path / "catalog.json", content)
    save_catalog_summary(path, content)
    return rotated


class Sessions:
    def __init__(self, root: Path = Path("data/sessions")):
        self.root = Path(root)
        self.current: Path | None = None
        saved = read_json(self.root / "current.json", {})
        if saved.get("id") and (self.root / saved["id"] / "catalog.json").is_file():
            self.current = self.root / saved["id"]

    def resolve(self, identifier: str) -> Path:
        if (not identifier or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", identifier)
                or identifier.startswith("_backup_")):
            raise ValueError("Invalid session ID.")
        destination = self.root / identifier
        if not (destination / "catalog.json").is_file():
            raise FileNotFoundError(identifier)
        return destination

    def open(self, identifier: str) -> Path:
        destination = self.resolve(identifier)
        self.current = destination
        write_json(self.root / "current.json", {"id": identifier})
        return destination

    def delete(self, identifier: str) -> Path:
        """Move a session to `.trash` (recoverable) and pick a new current session if needed."""
        if not identifier or Path(identifier).name != identifier or identifier.startswith("."):
            raise ValueError("Invalid session ID.")
        source = self.root / identifier
        if not (source / "catalog.json").is_file():
            raise FileNotFoundError(identifier)
        trash = self.root / ".trash"
        trash.mkdir(exist_ok=True)
        destination = trash / f"{identifier}-{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
        shutil.move(str(source), str(destination))
        if self.current is not None and self.current.name == identifier:
            remaining = self.list()
            self.current = self.root / remaining[0]["id"] if remaining else None
            write_json(self.root / "current.json", {"id": remaining[0]["id"]} if remaining else {})
        return destination

    def payload(self, path: Path | None = None) -> dict:
        path = path or self.current
        if path is None:
            raise FileNotFoundError("No current session.")
        content = read_json(path / "catalog.json", {})
        warning = content.get("survey_warning", "")
        if warning.startswith("Survey coverage unavailable:"):
            # Older captures stored the raw urllib3 retry chain.
            content["survey_warning"] = "Couldn't check survey coverage. " + (
                "CDS isn't responding. Try again in a few minutes."
                if "timed out" in warning or "Max retries" in warning else "Try again later.")
        view = read_json(path / "view.json", {})
        edits = read_json(path / "edits.json")
        meta = read_json(path / "project" / "meta.json")
        surveys.annotate_detail(content, view)
        if meta:
            for layer in meta.get("layers", []):
                if surveys.is_survey(layer):
                    layer["fwhm_arcsec"] = surveys.band_for(layer).fwhm_arcsec
        if edits:
            from astroimp.edits import discard_star_state
            edits = discard_star_state(edits)
        return {"id": path.name, "name": read_json(path / "session.json", {}).get(
                    "name", fallback_name(view, content)), "view": view,
                "catalog": content,
                "selection": read_json(path / "selection.json", {"obs_ids": []}),
                "triage": read_json(path / "triage.json", {"favorites": [], "dismissed": []}),
                "meta": meta,
                "build_version": BUILD_VERSION,
                "rebuild_required": bool(meta and float(meta.get("view_rotation_deg", 0)) !=
                                         float(view.get("rotation_deg", 0))),
                "build": {"mosaic": read_json(path / "session.json", {}).get("build", {}).get("mosaic", True)},
                "edits": edits}

    def list(self) -> list[dict]:
        result = []
        if not self.root.exists():
            return result
        for path in sorted(self.root.iterdir(), reverse=True):
            if (not path.is_dir() or path.name.startswith("_backup_")
                    or not (path / "catalog.json").exists()):
                continue
            view = read_json(path / "view.json", {})
            saved = read_json(path / "session.json", {})
            summary = saved.get("catalog_summary", {})
            stat = (path / "catalog.json").stat()
            if summary.get("signature") != [stat.st_mtime_ns, stat.st_size]:
                saved = save_catalog_summary(path, read_json(path / "catalog.json", {}))
                summary = saved["catalog_summary"]
            created = saved.get("created") or view.get("captured_at_utc")
            if not created:
                created = datetime.fromtimestamp(path.stat().st_ctime, timezone.utc).isoformat()
            updated = datetime.fromtimestamp(max(
                source.stat().st_mtime for name in (
                    "edits.json", "selection.json", "session.json", "catalog.json")
                if (source := path / name).exists()), timezone.utc).isoformat()
            result.append({"id": path.name, "created": created,
                           "updated": updated,
                           "name": saved.get("name", summary["fallback_name"]),
                           "observations": summary["observations"],
                           "target_hint": summary["target_hint"],
                           "groups": summary["groups"],
                           "selected": len(read_json(path / "selection.json", {}).get("obs_ids", [])),
                           "built": (path / "project" / "meta.json").exists()})
        return sorted(result, key=lambda item: item["updated"], reverse=True)


@dataclass
class Job:
    id: str
    kind: str
    session: str = ""
    name: str = ""
    state: str = "running"
    stage: str = ""
    progress: float = 0
    message: str = ""
    log: list[str] = field(default_factory=list)
    error: str | None = None
    phase: str = "download"
    files: list[dict] = field(default_factory=list)
    steps: list[dict] = field(default_factory=list)
    eta_s: float | None = None
    result: dict | None = None
    cancel_requested: bool = False
    waiting_for: dict | None = None


class BusyError(RuntimeError):
    pass


class JobManager:
    def __init__(self):
        self.lock = threading.RLock()
        self.job: Job | None = None
        self.thread: threading.Thread | None = None
        self.jobs: dict[str, Job] = {}
        self.threads: dict[str, threading.Thread] = {}
        self.condition = threading.Condition(self.lock)
        self.heavy_owner: Job | None = None
        self.queue: list[Job] = []

    def busy(self, session: str | None = None) -> bool:
        with self.lock:
            return any(j.state in {"running", "queued"} and
                       (session is None or j.session == session) for j in self.jobs.values())

    def payload(self, session: str | None = None) -> dict | None:
        with self.lock:
            job = next((j for j in reversed(list(self.jobs.values()))
                        if session is None or j.session == session), None)
            return self._payload(job) if job else None

    def _payload(self, job: Job) -> dict:
        result = asdict(job)
        if job.state == "queued" and self.heavy_owner:
            owner = self.heavy_owner
            result["waiting_for"] = {"session": owner.session, "name": owner.name,
                                     "stage": owner.phase, "progress": owner.progress}
            result["message"] = f"Waiting for {owner.name}"
        return result

    def all(self) -> list[dict]:
        with self.lock:
            return [self._payload(j) for j in self.jobs.values()]

    def cancel(self, identifier: str | None = None, *, session: str | None = None) -> dict | None:
        with self.lock:
            job = self.jobs.get(identifier) if identifier else next(
                (j for j in reversed(list(self.jobs.values()))
                 if session is None or j.session == session), None)
            if job and job.state in {"running", "queued"}:
                job.cancel_requested = True
                self.condition.notify_all()
            return self._payload(job) if job else None

    def start(self, kind: str, work: Callable, *, session: str = "", name: str = "") -> dict:
        with self.lock:
            if self.busy(session):
                raise BusyError(f"{name or session or 'This project'} already has an active job.")
            # Keep active jobs and a bounded recent history.
            finished = [j.id for j in self.jobs.values() if j.state not in {"running", "queued"}]
            for identifier in finished[:-39]:
                self.jobs.pop(identifier, None)
                self.threads.pop(identifier, None)
            self.job = Job(uuid.uuid4().hex, kind, session=session, name=name or session)
            job = self.job
            self.jobs[job.id] = job

            def check():
                if job.cancel_requested:
                    raise CancelledError("Cancelled")

            def progress(stage, fraction, message):
                check()
                if stage in {"reproject", "edges", "publish", "canvas", "render", "write"}:
                    heavy(stage)
                with self.lock:
                    previous = job.message
                    job.stage = stage
                    job.progress = min(1.0, max(0.0, float(fraction)))
                    job.message = message
                    # Download messages update per MiB; keep the job response bounded.
                    if message != previous:
                        job.log.append(message)
                        job.log[:] = job.log[-200:]

            def heavy(stage="align"):
                with self.condition:
                    if self.heavy_owner is job:
                        return
                    if job not in self.queue:
                        job.stage = job.phase = stage
                        job.progress = 0
                        job.eta_s = None
                        self.queue.append(job)
                    try:
                        while self.heavy_owner is not None or self.queue[0] is not job:
                            check()
                            job.state = "queued"
                            self.condition.wait(.1)
                        check()
                        self.queue.remove(job)
                        self.heavy_owner = job
                        job.state = "running"
                    except BaseException:
                        self.queue.remove(job)
                        self.condition.notify_all()
                        raise

            def update(**values):
                with self.lock:
                    for key, value in values.items():
                        if key in {"phase", "files", "steps", "eta_s", "name"}:
                            setattr(job, key, value)
            progress.update = update
            progress.check_cancelled = check
            progress.heavy = heavy

            def run():
                try:
                    check()
                    result = work(progress)
                    check()
                    with self.lock:
                        if isinstance(result, dict):
                            job.result = result
                        job.state = "done"
                        job.progress = 1
                        job.phase = "done"
                        job.eta_s = 0
                except CancelledError:
                    with self.lock:
                        job.state = "cancelled"
                        job.stage = job.phase = "cancelled"
                        job.message = "Cancelled"
                        job.eta_s = None
                        job.log.append("Cancelled")
                except Exception as exc:
                    with self.lock:
                        job.state = "error"
                        job.error = friendly(exc)
                        job.phase = "error"
                        job.eta_s = None
                        job.log.append(traceback.format_exc())
                finally:
                    with self.condition:
                        if self.heavy_owner is job:
                            self.heavy_owner = None
                        self.condition.notify_all()

            self.thread = threading.Thread(target=run, name=f"astroimp-{kind}", daemon=True)
            self.threads[job.id] = self.thread
            initial = asdict(self.job)
            self.thread.start()
            return initial


Jobs = JobManager
