"""Completed builds per session: the active one lives in project/, older ones in builds/<id>/."""
from __future__ import annotations

import json
import re
import shutil
from datetime import datetime
from pathlib import Path

from astroimp.mast import write_json
from astroimp.grid import grid_from_meta, read, remap_markers, remap_state, same_grid


def _read(path: Path) -> dict | None:
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


def build_id(meta: dict) -> str:
    stamp = meta.get("built_at", "")
    try:
        return datetime.fromisoformat(stamp).strftime("%Y%m%dT%H%M%SZ")
    except ValueError:
        return re.sub(r"[^0-9A-Za-z]", "", stamp) or "unknown"


def summary(meta: dict, identifier: str, current: bool) -> dict:
    return {"id": identifier, "built_at": meta.get("built_at"), "current": current,
            "layers": [{"obs_id": layer.get("obs_id"), "filter": layer.get("filter"),
                        "wavelength_nm": layer.get("wavelength_nm")} for layer in meta.get("layers", [])],
            "width": meta.get("width"), "height": meta.get("height")}


def list_builds(session: Path) -> list[dict]:
    builds = []
    meta = _read(session / "project" / "meta.json")
    if meta:
        builds.append(summary(meta, build_id(meta), True))
    root = session / "builds"
    if root.exists():
        for path in sorted(root.iterdir(), reverse=True):
            archived = _read(path / "project" / "meta.json")
            if archived:
                builds.append(summary(archived, path.name, False))
    return builds


def archive_current(session: Path) -> str | None:
    """Move the active build (and its edits) into builds/ so a new build won't overwrite it."""
    project = session / "project"
    meta = _read(project / "meta.json")
    if not meta:
        return None
    identifier = build_id(meta)
    destination = session / "builds" / identifier
    if destination.exists():
        identifier = f"{identifier}-{len(list(destination.parent.iterdir()))}"
        destination = session / "builds" / identifier
    destination.mkdir(parents=True)
    project.rename(destination / "project")
    edits = session / "edits.json"
    if edits.exists():
        # Copy, not move: the next build starts from the current look.
        shutil.copy2(edits, destination / "edits.json")
    for name in ("grid.json", "markers.json", "edits.rev.json"):
        if (session / name).exists():
            shutil.copy2(session / name, destination / name)
    if meta.get("wcs") and not (destination / "grid.json").exists():
        write_json(destination / "grid.json", grid_from_meta(meta))
    return identifier


def restore(session: Path, identifier: str, *, state: dict | None = None) -> None:
    if not identifier or Path(identifier).name != identifier or identifier in {".", ".."}:
        raise ValueError("Invalid build ID.")
    source = session / "builds" / identifier
    if not (source / "project" / "meta.json").exists():
        raise FileNotFoundError(identifier)
    target = _read(source / "project" / "meta.json")
    previous = _read(session / "project" / "meta.json")
    edits = state if state is not None else _read(source / "edits.json")
    saved_grid = read(source / "grid.json", target)
    if state is None and edits is not None and saved_grid.get("wcs") and target.get("wcs") and not same_grid(saved_grid, target):
        # A repaired legacy archive can record edits made on an earlier canvas.
        edits = remap_state(edits, saved_grid, target)
    markers = read(session / "markers.json")
    if previous and previous.get("wcs") and target.get("wcs"):
        if edits is None and (current_edits := read(session / "edits.json")) is not None:
            edits = remap_state(current_edits, previous, target, geometry=not same_grid(previous, target))
        if markers is not None and not same_grid(previous, target):
            markers = remap_markers(markers, previous, target)
    archive_current(session)
    (source / "project").rename(session / "project")
    if edits is not None:
        from astroimp.edits import discard_star_state
        edits = discard_star_state(edits)
        write_json(session / "edits.json", edits)
    else:
        (session / "edits.json").unlink(missing_ok=True)
    if target.get("wcs"):
        write_json(session / "grid.json", grid_from_meta(target))
    if markers is not None:
        write_json(session / "markers.json", markers)
    revision = read(session / "edits.rev.json", {}).get("revision", 0)
    write_json(session / "edits.rev.json", {"revision": revision+1, "source": "restore",
                                          "note": "Restored build"})
    shutil.rmtree(source)
