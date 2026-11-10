"""Loopback-only HTTP interface for NightBloom."""
from __future__ import annotations

import hashlib
import io
import math
import os
import re
import threading
import time
import uuid
from contextvars import ContextVar
from pathlib import Path
from typing import Callable, Literal
from urllib.parse import urlencode

import requests
import numpy as np
from fastapi import Depends, FastAPI, HTTPException, Request
from fastapi.responses import FileResponse, HTMLResponse, Response
from fastapi.staticfiles import StaticFiles
from PIL import Image
from pydantic import BaseModel, Field
from pyvo.samp import SAMPIntegratedClient
from starlette.concurrency import run_in_threadpool

from astroimp import aladin
from astroimp.aladin_fits import FitsResponse, cached_fits
from astroimp.build import build, build_plan, ensure_edge_map, expansion_plan
from astroimp.builds import list_builds, restore
from astroimp.canvas import load as load_canvas, plan as plan_canvas, prepare as prepare_canvas
from astroimp.edits import apply as apply_edits, discard_star_state
from astroimp.contactsheet import export_contact_sheet
from astroimp.fullres import export_full, export_path, plan as fullres_plan
from astroimp.render import analyze, probe
from astroimp.catalog import polygons
from astroimp.mast import write_json
from astroimp.session import (BusyError, Jobs, Sessions, capture, search_mast, search_surveys,
                              capture_coordinates, observations, read_json, recapture, refresh_snapshot,
                              rotate_capture, snapshot_header, fallback_name)
from astroimp import coordinates
from astroimp import bookmarks, gallery, previews
from astroimp import surveys
from astroimp.services import Health, friendly
from astroimp.thumbnails import thumbnail
from astroimp.storage import Storage


class StorageDelete(BaseModel):
    downloads: list[str] = []
    trash: list[str] = []
    backups: list[str] = []
    caches: list[str] = []


class Selection(BaseModel):
    obs_ids: list[str]


class BookmarkAdd(Selection):
    session: str


class BookmarkNote(BaseModel):
    note: str = Field(max_length=200)


class BookmarkFrame(BaseModel):
    ids: list[str] = Field(min_length=1)
    aspect: str = "16:9"


class ExpandBuild(BaseModel):
    grid: dict


class FillAround(BaseModel):
    margin: float = .5
    obs_id: str | None = None


class CanvasRegion(BaseModel):
    preview_id: str
    region: dict


class CanvasBuild(CanvasRegion):
    grid: dict


class Goto(BaseModel):
    target: str
    fov_arcmin: float | None = None


class Rename(BaseModel):
    name: str


class CaptureRotation(BaseModel):
    rotation_deg: float = Field(ge=-180, le=180, allow_inf_nan=False)


class CoordinateCapture(BaseModel):
    ra: float = Field(ge=0, lt=360, allow_inf_nan=False)
    dec: float = Field(ge=-90, le=90, allow_inf_nan=False)
    width_arcmin: float = Field(default=10, ge=1, le=180, allow_inf_nan=False)
    aspect: str = "16:9"
    rotation_deg: float = Field(default=0, ge=-180, le=180, allow_inf_nan=False)
    name: str = Field(default="", max_length=200)
    surveys_only: bool = False


class Triage(BaseModel):
    favorites: list[str]
    dismissed: list[str]


class Layers(BaseModel):
    keys: list[str]


class EditStep(BaseModel):
    target: str
    ops: list[str]


class EditRequest(BaseModel):
    steps: list[EditStep]
    note: str = ""


class ExportCheckpoint(BaseModel):
    state: dict
    meta: dict


class GalleryRestore(BaseModel):
    filename: str


class FullExport(BaseModel):
    scale_factor: float | None = None
    long_side: int | None = None
    format: str = "png8"
    quality: int = 92


def notify(messages: list[dict]) -> None:
    if not messages:
        return
    client = SAMPIntegratedClient(name="NightBloom", addr="127.0.0.1")
    client.connect()
    try:
        recipients = aladin.clients(client, messages[0]["samp.mtype"])
        if len(recipients) != 1:
            raise RuntimeError(f"Expected one Aladin; found {len(recipients)}.")
        for message in messages:
            client.notify(recipients[0], message)
    finally:
        client.disconnect()


def scripts(script: str) -> None:
    notify([{"samp.mtype": "script.aladin.send", "samp.params": {"script": script}}])


def goto(target: str, fov_arcmin: float | None = None) -> None:
    if not target.strip() or any(c in target for c in '\r\n;"'):
        raise ValueError("Supply a single Aladin target.")
    if fov_arcmin is not None and (not math.isfinite(fov_arcmin) or not 0 < fov_arcmin <= 10800):
        raise ValueError("Field of view must be in (0, 10800] arcminutes.")
    client = SAMPIntegratedClient(name="NightBloom navigation", addr="127.0.0.1")
    client.connect()
    try:
        recipients = aladin.clients(client, "script.aladin.send")
        if len(recipients) != 1:
            raise RuntimeError(f"Expected one Aladin; found {len(recipients)}.")
        scripts = [target.strip()]
        if fov_arcmin is not None:
            scripts.append(f"zoom {fov_arcmin:g}arcmin")
        for index, script in enumerate(scripts):
            if index:
                time.sleep(1)
            client.notify(recipients[0], {"samp.mtype": "script.aladin.send",
                                          "samp.params": {"script": script}})
    finally:
        client.disconnect()


LOOPBACK = re.compile(r"^(127\.0\.0\.1|localhost|\[::1\]|testserver)(:\d+)?$", re.I)
LOOPBACK_ORIGIN = re.compile(r"^https?://(127\.0\.0\.1|localhost|\[::1\])(:\d+)?$", re.I)


def code_fingerprint() -> str:
    """Hash of the server's Python sources; differs from startup when the running server is stale."""
    digest = hashlib.sha1()
    for path in sorted(Path(__file__).parent.glob("*.py")):
        digest.update(path.name.encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def create_app(data: Path = Path("data"),
               frontend: Path | None = None, restart: Callable[[], None] | None = None,
               health: Health | None = None) -> FastAPI:
    health = health or Health()
    scoped_session: ContextVar[Path | None] = ContextVar("astroimp_session", default=None)

    async def session_for(request: Request):
        global_route = request.url.path in {
            "/api/sessions", "/api/storage", "/api/storage/delete", "/api/capture", "/api/aladin/goto",
            "/api/capture/coordinates", "/api/coordinates/resolve", "/api/coordinates/coverage",
            "/api/bookmarks",
        } or request.url.path.startswith(("/api/sessions/", "/api/bookmarks/"))
        identifier = None if global_route else (
            request.headers.get("X-AstroImp-Session") or request.query_params.get("session"))
        try:
            path = sessions.resolve(identifier) if identifier else sessions.current
        except (ValueError, FileNotFoundError) as exc:
            raise HTTPException(404, "Unknown project.") from exc
        token = scoped_session.set(path)
        try:
            yield path
        finally:
            scoped_session.reset(token)

    app = FastAPI(title="NightBloom", dependencies=[Depends(session_for)])

    @app.middleware("http")
    async def loopback_only(request: Request, call_next):
        # Block DNS rebinding and cross-site requests: the API is only for pages served from loopback.
        if not LOOPBACK.match(request.headers.get("host", "")):
            return Response("Forbidden host", status_code=403)
        origin = request.headers.get("origin")
        if request.method not in {"GET", "HEAD", "OPTIONS"} and origin and not LOOPBACK_ORIGIN.match(origin):
            return Response("Forbidden origin", status_code=403)
        return await call_next(request)
    started_code = code_fingerprint()
    data = Path(data)
    sessions = Sessions(data / "sessions")
    jobs = Jobs()
    storage = Storage(data)
    bookmark_store = bookmarks.Bookmarks(data)
    app.state.sessions = sessions
    app.state.jobs = jobs
    ping_lock = threading.Lock()
    status_cache = {"time": -math.inf, "aladin": False, "stale": False}

    def current() -> Path:
        path = scoped_session.get() or sessions.current
        if path is None:
            raise HTTPException(404, "No current session.")
        return path

    def idle(identifier: str | None = None):
        if jobs.busy(identifier or current().name):
            raise HTTPException(409, "This project has an active job. Wait or cancel it first.")

    def edits_idle():
        idle()

    def start(kind, work, path=None):
        path = path or current()
        try:
            return jobs.start(kind, work, session=path.name,
                              name=sessions.payload(path)["name"])
        except BusyError as exc:
            raise HTTPException(409, str(exc)) from exc

    @app.get("/api/storage")
    def storage_inventory():
        try:
            return storage.inventory()
        except (OSError, ValueError, KeyError, TypeError) as exc:
            raise HTTPException(500, "Cannot read storage references; cleanup is unavailable.") from exc

    @app.post("/api/restart")
    def restart_server():
        if restart is None:
            raise HTTPException(409, "Restart is only available when NightBloom runs via `uv run astroimp`.")
        with jobs.lock:
            if jobs.busy():
                raise HTTPException(409, "Finish or cancel running jobs before restarting.")
            restart()
        return {"restarting": True}

    @app.post("/api/storage/delete")
    def storage_delete(request: StorageDelete):
        with jobs.lock:
            if jobs.busy():
                raise HTTPException(409, "Cleanup is unavailable while projects have active jobs.")
            try:
                return storage.delete(**request.model_dump())
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
            except OSError as exc:
                raise HTTPException(500, "Cleanup could not finish. Refresh storage and try again.") from exc

    @app.get("/api/status")
    def status():
        if time.monotonic() - status_cache["time"] >= 2 and ping_lock.acquire(blocking=False):
            def refresh():
                try:
                    status_cache["aladin"] = bool(aladin.ping())
                except Exception:
                    status_cache["aladin"] = False
                try:
                    status_cache["stale"] = code_fingerprint() != started_code
                except OSError:
                    pass
                finally:
                    status_cache["time"] = time.monotonic()
                    ping_lock.release()
            threading.Thread(target=refresh, name="astroimp-status", daemon=True).start()
        path = scoped_session.get()
        return {"aladin": status_cache["aladin"], "services": health.snapshot(),
                "stale": status_cache["stale"], "restartable": restart is not None, "pid": os.getpid(),
                "edits": read_json(path / "edits.rev.json", {"revision": 0}) if path else None,
                "session": path.name if path else None,
                "job": jobs.payload(path.name) if path else jobs.payload(),
                "jobs": jobs.all()}

    @app.get("/api/sessions")
    def listing():
        with jobs.lock:
            return sessions.list()

    @app.get("/api/gallery")
    def gallery_inventory():
        return gallery.inventory(sessions)

    def restore_export_look(folder: Path, identifier: str):
        from astroimp import export_history
        if jobs.busy(folder.name):
            raise HTTPException(409, "Wait for this project's current job before restoring edits.")
        try:
            export_history.restore(folder, identifier)
        except (ValueError, FileNotFoundError) as exc:
            raise HTTPException(409, str(exc)) from exc
        sessions.open(folder.name)
        return sessions.payload(folder)

    @app.post("/api/gallery/{identifier}/restore")
    def gallery_restore(identifier: str, body: GalleryRestore):
        from astroimp import export_history
        with jobs.lock:
            try:
                folder = gallery.project(sessions, identifier)
                source = gallery.export_file(folder, body.filename)
                saved = export_history.image_checkpoint(folder, source)
            except (ValueError, FileNotFoundError) as exc:
                raise HTTPException(409, str(exc)) from exc
            return restore_export_look(folder, saved)

    @app.post("/api/sessions/{identifier}/export-checkpoints/{checkpoint}/restore")
    def checkpoint_restore(identifier: str, checkpoint: str):
        with jobs.lock:
            try:
                folder = gallery.project(sessions, identifier)
            except (ValueError, FileNotFoundError) as exc:
                raise HTTPException(404, str(exc)) from exc
            return restore_export_look(folder, checkpoint)

    @app.get("/api/gallery/{identifier}/{filename:path}")
    def gallery_image(identifier: str, filename: str,
                      view: Literal["original", "thumbnail", "preview", "clipboard"] = "thumbnail"):
        try:
            folder = gallery.project(sessions, identifier)
            source = gallery.export_file(folder, filename)
        except (ValueError, FileNotFoundError) as exc:
            raise HTTPException(404, str(exc)) from exc
        if view == "original":
            return FileResponse(source, filename=source.name)
        try:
            content = gallery.preview(source, data / "cache" / "gallery", view)
        except (OSError, ValueError, Image.DecompressionBombError) as exc:
            raise HTTPException(400, f"Cannot display this export: {exc}") from exc
        return Response(content, media_type="image/png" if view == "clipboard" else "image/jpeg",
                        headers={"Cache-Control": "no-store" if view == "clipboard" else "private, max-age=3600"})

    @app.get("/api/sessions/{identifier}/thumbnail.jpg")
    def session_thumbnail(identifier: str):
        if (not identifier or Path(identifier).name != identifier
                or identifier in {".", ".."} or identifier.startswith(".")):
            raise HTTPException(404, "Unknown session.")
        path = sessions.root / identifier
        if not (path / "catalog.json").is_file():
            raise HTTPException(404, "Unknown session.")
        try:
            return FileResponse(thumbnail(path), media_type="image/jpeg",
                                headers={"Cache-Control": "no-cache"})
        except FileNotFoundError as exc:
            raise HTTPException(404, str(exc)) from exc

    @app.post("/api/sessions/{identifier}/open")
    def open_session(identifier: str):
        with jobs.lock:
            try:
                path = sessions.open(identifier)
            except (FileNotFoundError, ValueError) as exc:
                raise HTTPException(404, str(exc)) from exc
            return sessions.payload(path)

    @app.delete("/api/sessions/{identifier}")
    def delete_session(identifier: str):
        with jobs.lock:
            idle(identifier)
            try:
                sessions.delete(identifier)
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
            except FileNotFoundError as exc:
                raise HTTPException(404, str(exc)) from exc
            return {"current": sessions.current.name if sessions.current else None}

    @app.get("/api/session")
    def session():
        return sessions.payload(current())

    @app.patch("/api/session")
    def rename(body: Rename):
        name = body.name.strip()
        if not name or len(name) > 200 or any(ord(c) < 32 for c in name):
            raise HTTPException(400, "Supply a session name of 1–200 printable characters.")
        with jobs.lock:
            path = current() / "session.json"
            content = {**read_json(path, {}), "name": name, "auto_named": False}
            write_json(path, content)
            for job in jobs.jobs.values():
                if job.session == path.parent.name:
                    job.name = name
        return {"name": name}

    @app.put("/api/session/build")
    def build_options(body: dict):
        enabled = body.get("mosaic")
        if not isinstance(enabled, bool):
            raise HTTPException(400, "mosaic must be true or false.")
        with jobs.lock:
            idle()
            path = current() / "session.json"
            settings = read_json(path, {})
            write_json(path, {**settings, "build": {**settings.get("build", {}), "mosaic": enabled}})
        return {"mosaic": enabled}

    @app.put("/api/triage")
    def triage(body: Triage):
        with jobs.lock:
            path = current()
            available = observations(read_json(path / "catalog.json", {}))
            if not (set(body.favorites) | set(body.dismissed)) <= available.keys():
                raise HTTPException(400, "Triage contains unknown observations.")
            content = {"favorites": list(dict.fromkeys(body.favorites)),
                       "dismissed": list(dict.fromkeys(body.dismissed))}
            write_json(path / "triage.json", content)
        return content

    @app.get("/api/session/snapshot.jpg")
    def snapshot():
        path = current() / "snapshot.jpg"
        if not path.exists():
            raise HTTPException(404, "Snapshot unavailable.")
        return FileResponse(path, media_type="image/jpeg", headers={"Cache-Control": "no-cache"})

    @app.get("/api/session/backdrop.jpg")
    def coordinate_backdrop():
        path = current()
        view = read_json(path / "view.json", {})
        if view.get("source") != "coordinates":
            raise HTTPException(404, "No coordinate frame.")
        try:
            band = surveys.BY_ID["CDS/P/DSS2/red"]
            return FileResponse(surveys.preview({"hips_id": band.hips_id}, view,
                                               data / "cache", 1000, quick=True),
                                media_type="image/jpeg")
        except Exception as exc:
            raise HTTPException(502, friendly(exc)) from exc

    @app.post("/api/session/snapshot/refresh")
    def refresh_image():
        idle()
        path = current()
        if read_json(path / "view.json", {}).get("source") == "coordinates":
            raise HTTPException(400, "Coordinate projects have no Aladin snapshot.")
        try:
            # Keep our footprint overlay out of the re-captured image.
            try:
                scripts("hide AstroImp;hide NightBloom")
                time.sleep(.6)
            except Exception:
                pass
            try:
                return {"view": refresh_snapshot(path)}
            finally:
                try:
                    scripts("show AstroImp;show NightBloom")
                except Exception:
                    pass
        except LookupError as exc:
            raise HTTPException(409, str(exc)) from exc
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc
        except RuntimeError as exc:
            raise HTTPException(503, friendly(exc)) from exc

    @app.post("/api/session/recapture")
    def recapture_view():
        path = current()
        if (path / "project" / "meta.json").exists():
            raise HTTPException(409, "This project is built, so its view is locked. Use New project for a new view.")
        def work(progress):
            # Keep our footprint overlay out of the new snapshot.
            try:
                scripts("rm AstroImp;rm NightBloom")
            except Exception:
                pass
            result = recapture(path, progress)
            progress.update(name=sessions.payload(path)["name"])
            return result
        return start("capture", work, path)

    @app.post("/api/aladin/goto")
    def navigate(body: Goto):
        try:
            goto(body.target, body.fov_arcmin)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc
        except Exception as exc:
            raise HTTPException(503, friendly(exc)) from exc
        return {"ok": True}

    @app.post("/api/aladin/footprints")
    def footprints(body: Selection):
        available = observations(read_json(current() / "catalog.json", {}))
        if not set(body.obs_ids) <= available.keys():
            raise HTTPException(400, "Unknown observations.")
        commands = ["rm AstroImp", "rm NightBloom"]
        count = 0
        if body.obs_ids:
            commands += ["setconf frame=ICRS", "draw mode(radec)", "draw newtool(NightBloom)",
                         "select NightBloom"]
            for identifier in dict.fromkeys(body.obs_ids):
                for polygon in polygons(available[identifier].get("region", "")):
                    if not np.isfinite(polygon).all():
                        continue
                    coords = ",".join(f"{value:.10f}" for value in polygon.ravel())
                    commands.append(f"draw polygon({coords})")
                    count += 1
        try:
            scripts(";".join(commands))
        except Exception as exc:
            raise HTTPException(503, friendly(exc)) from exc
        return {"ok": True, "polygons": count}

    @app.post("/api/capture")
    def capture_view():
        identifier = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + uuid.uuid4().hex[:6]
        path = sessions.root / identifier
        write_json(path / "catalog.json", {"groups": []})
        write_json(path / "selection.json", {"obs_ids": []})
        write_json(path / "session.json", {"name": "New capture"})
        with jobs.lock:
            sessions.open(identifier)
        def work(progress):
            capture(sessions.root, progress, destination=path)
            progress.update(name=sessions.payload(path)["name"])
            return {"session": path.name}
        return start("capture", work, path)

    @app.get("/api/coordinates/resolve")
    def resolve_position(target: str):
        try:
            return coordinates.resolve(target)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc
        except Exception as exc:
            raise HTTPException(503, friendly(exc)) from exc

    @app.get("/api/bookmarks")
    def list_bookmarks():
        return bookmark_store.list()

    @app.post("/api/bookmarks")
    def add_bookmarks(body: BookmarkAdd):
        try:
            path = sessions.resolve(body.session)
            catalog = observations(read_json(path / "catalog.json", {}))
            if not set(body.obs_ids) <= catalog.keys():
                raise ValueError("Bookmarks contain unknown observations.")
            name = read_json(path / "session.json", {}).get("name") or fallback_name(
                read_json(path / "view.json", {}), read_json(path / "catalog.json", {}))
            return bookmark_store.add([catalog[key] for key in body.obs_ids], path.name, name)
        except FileNotFoundError as exc:
            raise HTTPException(404, "Unknown project.") from exc
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.post("/api/bookmarks/frame")
    def bookmark_frame(body: BookmarkFrame):
        try:
            return bookmarks.frame([bookmark_store.get(key) for key in dict.fromkeys(body.ids)], body.aspect)
        except KeyError as exc:
            raise HTTPException(404, "Unknown bookmark.") from exc
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.patch("/api/bookmarks/{identifier}")
    def bookmark_note(identifier: str, body: BookmarkNote):
        try:
            return bookmark_store.note(identifier, body.note)
        except KeyError as exc:
            raise HTTPException(404, "Unknown bookmark.") from exc

    @app.delete("/api/bookmarks/{identifier}")
    def remove_bookmark(identifier: str):
        try:
            bookmark_store.remove(identifier)
        except KeyError as exc:
            raise HTTPException(404, "Unknown bookmark.") from exc
        return {"removed": identifier}

    @app.get("/api/bookmarks/{identifier}/preview.jpg")
    def bookmark_preview(identifier: str):
        try:
            item = bookmark_store.get(identifier)
        except KeyError as exc:
            raise HTTPException(404, "Unknown bookmark.") from exc
        if not item.get("preview"):
            raise HTTPException(404, "Preview unavailable.")
        try:
            return FileResponse(previews.archive(item["preview"], data / "cache" / "previews"),
                                media_type="image/jpeg")
        except Exception as exc:
            raise HTTPException(502, friendly(exc)) from exc

    @app.get("/api/coordinates/coverage")
    def position_coverage(ra: float, dec: float, aspect: str = "16:9", refresh: bool = False):
        try:
            return coordinates.coverage(ra, dec, aspect, refresh=refresh)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.post("/api/capture/coordinates")
    def capture_position(body: CoordinateCapture):
        try:
            coordinates.synthetic_view(body.ra, body.dec, body.width_arcmin,
                                       body.aspect, body.rotation_deg)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc
        identifier = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + uuid.uuid4().hex[:6]
        path = sessions.root / identifier
        write_json(path / "catalog.json", {"groups": []})
        write_json(path / "selection.json", {"obs_ids": []})
        write_json(path / "session.json", {"name": body.name or "New project",
                                         })
        with jobs.lock:
            sessions.open(identifier)
        def work(progress):
            capture_coordinates(sessions.root, **body.model_dump(), progress=progress, destination=path)
            progress.update(name=sessions.payload(path)["name"])
            return {"session": path.name}
        return start("capture", work, path)

    @app.post("/api/session/search-mast")
    def also_search_mast():
        path = current()
        return start("search-mast", lambda progress: search_mast(path, progress))

    @app.post("/api/session/search-surveys")
    def retry_survey_search():
        path = current()
        return start("search-surveys", lambda progress: search_surveys(path, progress))

    @app.put("/api/session/rotation")
    def rotate_view(body: CaptureRotation):
        path = current()
        idle()
        if not read_json(path / "view.json") or snapshot_header(path) is None:
            raise HTTPException(400, "This capture has no original WCS. Capture the view again.")
        return start("view-rotation", lambda progress: rotate_capture(path, body.rotation_deg, progress))

    @app.get("/api/preview/{obs_id}")
    def preview(obs_id: str, size: str = "thumb"):
        if size not in {"thumb", "large"}:
            raise HTTPException(400, "Size must be thumb or large.")
        obs = observations(read_json(current() / "catalog.json", {})).get(obs_id)
        if obs is None or not obs.get("preview_uri"):
            raise HTTPException(404, "Preview unavailable.")
        if surveys.is_survey(obs):
            try:
                path = surveys.preview(obs, read_json(current() / "view.json"),
                                       data / "cache", 1400 if size == "large" else 320)
                return FileResponse(path, media_type="image/jpeg")
            except Exception as exc:
                raise HTTPException(502, friendly(exc)) from exc
        if not obs["preview_uri"].startswith("mast:"):
            raise HTTPException(400, "Expected a MAST preview URI.")
        try:
            path = previews.archive(obs["preview_uri"], data / "cache" / "previews", size)
        except Exception as exc:
            raise HTTPException(502, friendly(exc)) from exc
        if size == "large":
            return FileResponse(path, media_type="image/jpeg")
        with Image.open(path) as image:
            image.thumbnail((320, 320))
            output = io.BytesIO()
            image.save(output, format="JPEG", quality=85)
        return Response(output.getvalue(), media_type="image/jpeg",
                        headers={"Cache-Control": "private, max-age=86400"})

    @app.put("/api/selection")
    def selection(body: Selection):
        with jobs.lock:
            idle()
            path = current()
            available = observations(read_json(path / "catalog.json", {}))
            if not set(body.obs_ids) <= available.keys():
                raise HTTPException(400, "Selection contains unknown observations.")
            content = {"obs_ids": list(dict.fromkeys(body.obs_ids))}
            write_json(path / "selection.json", content)
            return content

    @app.post("/api/build")
    def build_project():
        with jobs.lock:
            idle()
            path = current()
            if not read_json(path / "selection.json", {}).get("obs_ids"):
                raise HTTPException(400, "Select at least one observation.")
            return start("build", lambda progress: build(
                path, downloads=data / "downloads", progress=progress))

    @app.get("/api/build/plan")
    def plan():
        path = current()
        available = observations(read_json(path / "catalog.json", {}))
        selected = read_json(path / "selection.json", {}).get("obs_ids", [])
        if not set(selected) <= available.keys():
            raise HTTPException(400, "Selection contains unknown observations.")
        return build_plan([available[key] for key in sorted(set(selected))], data / "downloads",
                          session=path)

    @app.post("/api/build/fill-around")
    def fill_surroundings(body: FillAround):
        from astroimp.fill import fill_around
        with jobs.lock:
            idle()
            path = current()
            if not (path / "edits.json").exists():
                raise HTTPException(400, "Open the composition in Edit before filling its surroundings.")
            return start("build", lambda progress: fill_around(
                path, data / "downloads", margin=body.margin, obs_id=body.obs_id, progress=progress))

    @app.get("/api/build/fill-around/plan")
    def fill_plan(margin: float = .5):
        from astroimp.fill import margin_grid
        meta = read_json(current() / "project" / "meta.json")
        if not meta:
            raise HTTPException(400, "Build a composition before filling its surroundings.")
        try:
            return margin_grid(meta, margin)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.get("/api/build/expand/plan")
    def expand_plan():
        try:
            return expansion_plan(current(), data / "downloads")
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.post("/api/build/expand")
    def expand_project(body: ExpandBuild):
        with jobs.lock:
            idle()
            path = current()
            try:
                grid = expansion_plan(path, data / "downloads")
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
            if body.grid != grid:
                raise HTTPException(409, "The selected images or canvas changed. Preview expansion again.")
            return start("build", lambda progress: build(
                path, downloads=data / "downloads", progress=progress, expand=grid))

    @app.post("/api/build/canvas/prepare")
    def canvas_prepare():
        with jobs.lock:
            idle()
            path = current()
            return start("canvas", lambda progress: prepare_canvas(path, data / "downloads", progress))

    @app.get("/api/build/canvas/preview")
    def canvas_preview():
        with jobs.lock:
            idle()
            try:
                cached = list((current() / ".canvas-preview").glob("*.json"))
                for entry in cached:
                    try:
                        return load_canvas(current(), entry.stem, data / "downloads")
                    except ValueError:
                        continue
                raise ValueError("Prepare the canvas preview first.")
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc

    @app.get("/api/build/canvas/preview/{identifier}.png")
    def canvas_image(identifier: str):
        try:
            load_canvas(current(), identifier, data / "downloads")
        except ValueError as exc:
            raise HTTPException(409, str(exc)) from exc
        return FileResponse(current() / ".canvas-preview" / f"{identifier}.png", media_type="image/png")

    @app.post("/api/build/canvas/plan")
    def canvas_plan(body: CanvasRegion):
        try:
            return plan_canvas(load_canvas(current(), body.preview_id, data / "downloads"), body.region)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.post("/api/build/canvas")
    def canvas_build(body: CanvasBuild):
        with jobs.lock:
            idle()
            path = current()
            try:
                grid = plan_canvas(load_canvas(path, body.preview_id, data / "downloads"), body.region)["grid"]
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
            if grid != body.grid:
                raise HTTPException(409, "The canvas changed. Review its size before applying.")
            return start("build", lambda progress: build(
                path, downloads=data / "downloads", progress=progress, expand=grid,
                checkpoint_name="Before canvas change"))

    @app.get("/api/builds")
    def builds_list():
        return list_builds(current())

    @app.post("/api/builds/{identifier}/restore")
    def builds_restore(identifier: str):
        with jobs.lock:
            idle()
            path = current()
            try:
                restore(path, identifier)
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
            except FileNotFoundError as exc:
                raise HTTPException(404, "Unknown build.") from exc
            # Restoring a build also restores the images it was made from as the selection.
            meta = read_json(path / "project" / "meta.json", {})
            write_json(path / "selection.json", {"obs_ids": [
                tile["obs_id"] for layer in meta.get("layers", [])
                for tile in layer.get("tiles", [layer])]})
            settings = read_json(path / "session.json", {})
            write_json(path / "session.json", {**settings, "build": {
                **settings.get("build", {}), "mosaic": meta.get("mosaic", False)}})
            return {"ok": True}

    @app.get("/api/job")
    def job():
        path = scoped_session.get()
        return jobs.payload(path.name) if path else jobs.payload()

    @app.post("/api/job/cancel")
    def cancel_job(id: str | None = None):
        path = scoped_session.get()
        return jobs.cancel(id, session=path.name if path else None)

    def project_layer(key):
        directory = current() / "project"
        meta = read_json(directory / "meta.json", {})
        layer = next((l for l in meta.get("layers", []) if l.get("key") == key), None)
        if layer is None or Path(layer["file"]).name != layer["file"]:
            raise HTTPException(404, "Unknown project layer.")
        path = directory / layer["file"]
        if not path.is_file():
            raise HTTPException(404, "Layer unavailable.")
        return path, meta, layer

    @app.get("/api/project/fits/{key}.fits")
    def project_fits(key: str):
        path, meta, _ = project_layer(key)
        return FitsResponse(cached_fits(path, meta), media_type="application/fits")

    @app.post("/api/aladin/layers")
    def aladin_layers(body: Layers, request: Request):
        messages = []
        for key in dict.fromkeys(body.keys):
            _, _, layer = project_layer(key)
            port = request.url.port or request.scope["server"][1]
            messages.append({"samp.mtype": "image.load.fits", "samp.params": {
                "url": f"http://127.0.0.1:{port}/api/project/fits/{key}.fits?{urlencode({'session': current().name})}",
                "name": f"NightBloom {layer['filter']}"}})
        try:
            notify(messages)
        except Exception as exc:
            raise HTTPException(503, friendly(exc)) from exc
        return {"ok": True, "count": len(messages)}

    @app.get("/api/project/{file}")
    def project_file(file: str):
        directory = current() / "project"
        meta = read_json(directory / "meta.json", {})
        allowed = {"meta.json"} if meta else set()
        for layer in meta.get("layers", []):
            allowed.add(layer["file"])
            if layer.get("coverage_file"):
                allowed.add(layer["coverage_file"])
            key = layer.get("key") or Path(layer["file"]).stem
            edge_file = layer.get("edge_file", f"{key}.edge.f16")
            allowed.add(edge_file)
            if file == edge_file and Path(file).name == file:
                ensure_edge_map(directory, meta, layer)
        if meta.get("layers"):
            allowed.add("luminance.edge.f16")
            if file == "luminance.edge.f16":
                ensure_edge_map(directory, meta)
        if meta.get("luminance"):
            allowed.add(meta["luminance"]["file"])
            if meta["luminance"].get("edge_file"):
                allowed.add(meta["luminance"]["edge_file"])
        if file not in allowed or Path(file).name != file or not (directory / file).is_file():
            raise HTTPException(404, "Unknown project file.")
        # Rebuilds reuse file names with new grid sizes; always revalidate (ETag makes unchanged files a cheap 304).
        return FileResponse(directory / file, media_type="application/octet-stream",
                            headers={"Cache-Control": "no-cache"})

    def revision_path(session: Path | None = None) -> Path:
        return (session or current()) / "edits.rev.json"

    def bump_revision(source: str, note: str = "", session: Path | None = None) -> int:
        path = revision_path(session)
        previous = read_json(path, {}).get("revision", 0)
        write_json(path, {"revision": previous + 1, "source": source, "note": note})
        return previous + 1

    def target_session(request: Request) -> Path:
        return current()

    @app.put("/api/edits")
    async def save_edits(request: Request):
        try:
            content = await request.json()
        except ValueError as exc:
            raise HTTPException(400, "Expected JSON edits.") from exc
        def save():
            with jobs.lock:
                edits_idle()
                path = target_session(request)
                write_json(path / "edits.json",
                           discard_star_state(content) if isinstance(content, dict) else content)
                return bump_revision("browser", session=path)
        return {"revision": await run_in_threadpool(save)}

    @app.get("/api/edits")
    def edits():
        path = current()
        state = read_json(path / "edits.json")
        return discard_star_state(state) if state else state

    @app.get("/api/edits/revision")
    def edits_revision():
        return read_json(revision_path(), {"revision": 0, "source": "", "note": ""})

    @app.post("/api/edits/apply")
    def edits_apply(body: EditRequest):
        with jobs.lock:
            edits_idle()
            path = current()
            state = read_json(path / "edits.json")
            meta = read_json(path / "project" / "meta.json")
            if state is None or meta is None:
                raise HTTPException(409, "Open this build in the editor once before applying edits.")
            state = discard_star_state(state)
            changes = []
            try:
                for step in body.steps:
                    changes += apply_edits(state, meta, step.target, step.ops)
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
            write_json(path / "edits.json", state)
            revision = bump_revision("assistant", body.note or "; ".join(changes)[:200], session=path)
        return {"revision": revision, "changes": changes}

    @app.get("/api/markers")
    def markers():
        return read_json(current() / "markers.json", [])

    @app.put("/api/markers")
    async def save_markers(request: Request):
        content = await request.json()
        if not isinstance(content, list):
            raise HTTPException(400, "Expected a list of markers.")
        with jobs.lock:
            edits_idle()
            write_json(target_session(request) / "markers.json", content)
        return {"count": len(content)}

    @app.post("/api/probe")
    async def probe_region(request: Request):
        try:
            region = await request.json()
        except ValueError as exc:
            raise HTTPException(400, "Expected a JSON probe region.") from exc
        if not isinstance(region, dict):
            raise HTTPException(400, "Expected a probe region object.")
        path = current()
        state = region.pop("state", read_json(path / "edits.json"))
        if state is None or not (path / "project" / "meta.json").exists():
            raise HTTPException(409, "Open this build in the editor once before probing it.")
        if not isinstance(state, dict) or not isinstance(state.get("layers"), list) or not isinstance(state.get("global"), dict):
            raise HTTPException(400, "Expected edits state with layers and global settings.")
        if "marker" in region:
            found = next((m for m in read_json(path / "markers.json", [])
                          if str(m.get("n")) == str(region["marker"])), None)
            if found is None:
                raise HTTPException(404, f"No marker #{region['marker']}.")
            region = {**found, **({"radius": found.get("radius", 4)} if found.get("kind") != "rect" else {})}
        try:
            result = await run_in_threadpool(probe, path / "project", state, region)
        except (KeyError, ValueError, TypeError, AttributeError) as exc:
            raise HTTPException(400, f"Invalid probe region or edits state: {exc}") from exc
        return result

    @app.get("/api/analyze")
    def analyze_edits(max_side: int = 900):
        path = current()
        state = read_json(path / "edits.json")
        if state is None or not (path / "project" / "meta.json").exists():
            raise HTTPException(409, "Open this build in the editor once before analysing it.")
        return analyze(path / "project", state, path / "analysis", max(200, min(max_side, 3072)))

    @app.post("/api/export/contact-sheet")
    def contact_sheet():
        with jobs.lock:
            idle()
            path = current()
            state = read_json(path / "edits.json")
            if state is None or not (path / "project" / "meta.json").is_file():
                raise HTTPException(409, "Open this build in the editor before exporting a contact sheet.")
            destination = export_contact_sheet(path, state)
        return FileResponse(destination, media_type="image/png", filename=destination.name,
                            headers={"X-Export-Path": str(destination.resolve())})

    @app.get("/api/export/full/plan")
    def full_export_plan():
        try:
            path = current()
            return fullres_plan(path, state=read_json(path / "edits.json"))
        except (ValueError, FileNotFoundError, KeyError) as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.post("/api/export/full")
    def full_export(body: FullExport):
        with jobs.lock:
            idle()
            path = current()
            try:
                if body.format not in {"png", "png8", "png16", "tiff16", "jpeg"}:
                    raise ValueError("format must be png8, png16, tiff16 or jpeg.")
                if not 1 <= body.quality <= 100:
                    raise ValueError("JPEG quality must be between 1 and 100.")
                state = read_json(path / "edits.json")
                if state is None:
                    raise ValueError("Open this build in the editor before exporting.")
                fullres_plan(path, body.scale_factor, state, body.long_side)
            except (ValueError, FileNotFoundError, KeyError) as exc:
                raise HTTPException(400, str(exc)) from exc
            name = sessions.payload(path)["name"]
            def work(progress):
                result = export_full(path, scale_factor=body.scale_factor,
                                     long_side=body.long_side, quality=body.quality,
                                     format=body.format, progress=progress, state=state, name=name)
                result["download_url"] = (f"/api/export/full/download/{Path(result['path']).name}"
                                          f"?{urlencode({'session': path.name})}")
                return result
            return start("export-full", work)

    @app.get("/api/export/full/download/{filename}")
    def full_export_download(filename: str, session_id: str | None = None):
        if (Path(filename).name != filename or filename.startswith(".")
                or Path(filename).suffix not in {".png", ".tiff", ".jpg"}):
            raise HTTPException(404, "Unknown export.")
        if session_id is not None and (Path(session_id).name != session_id or session_id in {".", ".."}):
            raise HTTPException(404, "Unknown session.")
        try:
            folder = sessions.resolve(session_id) if session_id is not None else current()
        except (ValueError, FileNotFoundError) as exc:
            raise HTTPException(404, "Unknown project.") from exc
        path = folder / "exports" / filename
        if not path.is_file():
            raise HTTPException(404, "Unknown export.")
        return FileResponse(path, filename=filename, media_type={
            ".tiff":"image/tiff", ".png":"image/png", ".jpg":"image/jpeg"}[path.suffix])

    @app.post("/api/export/checkpoint")
    def export_checkpoint(body: ExportCheckpoint) -> dict[str, str]:
        from astroimp import export_history
        with jobs.lock:
            edits_idle()
            path = current()
            meta = read_json(path / "project" / "meta.json")
            if not export_history.same_meta(body.meta, sessions.payload(path)["meta"]):
                raise HTTPException(409, "The build changed. Reload the project before exporting.")
            try:
                identifier = export_history.save_checkpoint(path, body.state, meta)
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc
        return {"checkpoint": identifier}

    @app.post("/api/export")
    async def export(request: Request):
        path = current()
        directory = path / "exports"
        content = await request.body()
        if not content.startswith(b"\x89PNG\r\n\x1a\n"):
            raise HTTPException(400, "Expected PNG bytes.")
        try:
            with Image.open(io.BytesIO(content)) as image:
                size = image.size
                image.verify()
        except Exception as exc:
            raise HTTPException(400, "Invalid PNG.") from exc
        name = request.headers.get("X-Name", "nightbloom")
        name = re.sub(r"-\d+x\d+$", "", name.removesuffix(".png"))
        destination = export_path(directory, name, *size, "png")
        checkpoint = request.headers.get("X-Export-Checkpoint")
        if checkpoint:
            from astroimp.export_history import checkpoint as read_checkpoint
            try:
                read_checkpoint(path, checkpoint)
            except ValueError as exc:
                raise HTTPException(400, str(exc)) from exc

        def save():
            directory.mkdir(parents=True, exist_ok=True)
            temporary = destination.with_suffix(".part")
            try:
                temporary.write_bytes(content)
                if checkpoint:
                    from astroimp.export_history import record
                    record(path, temporary, checkpoint, filename=destination.name)
                temporary.replace(destination)
            finally:
                temporary.unlink(missing_ok=True)
        try:
            await run_in_threadpool(save)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc
        return {"path": str(destination.resolve())}

    dist = frontend or Path(__file__).resolve().parents[2] / "frontend" / "dist"
    if (dist / "index.html").is_file():
        app.mount("/", StaticFiles(directory=dist, html=True), name="studio")
    else:
        @app.get("/", response_class=HTMLResponse, include_in_schema=False)
        def missing_frontend():
            return HTMLResponse(
                "<!doctype html><html><head><title>NightBloom frontend unavailable</title></head>"
                "<body><h1>NightBloom frontend is not built</h1>"
                "<p>Run <code>npm run build</code> in the <code>frontend</code> directory, "
                "then restart NightBloom.</p></body></html>",
                status_code=503,
            )
    return app


app = create_app()


RESTART_EXIT_CODE = 75


def serve(port: int = 8765, no_browser: bool = False, data: Path = Path("data"),
          *, no_build: bool = False, supervised: bool = False) -> None:
    import uvicorn
    from astroimp.frontend import build_frontend

    if not no_build:
        build_frontend()

    restart_requested = threading.Event()

    def restart():
        # Respond first, then stop; the supervisor in cli.py starts a fresh server process.
        restart_requested.set()
        threading.Timer(.3, lambda: setattr(server, "should_exit", True)).start()

    server = uvicorn.Server(uvicorn.Config(
        create_app(data, restart=restart if supervised else None),
        host="127.0.0.1", port=port, log_level="info"))
    if not no_browser:
        def open_when_ready():
            import webbrowser

            while not server.started and not server.should_exit:
                time.sleep(.1)
            if server.started:
                webbrowser.open(f"http://127.0.0.1:{port}")
        threading.Thread(target=open_when_ready, daemon=True).start()
    server.run()
    if restart_requested.is_set():
        raise SystemExit(RESTART_EXIT_CODE)
