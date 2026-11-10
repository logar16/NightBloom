"""Immutable export-time edits, shared by every size of the same saved look."""
from __future__ import annotations

import copy
import hashlib
import json
import threading
from datetime import datetime, timezone
from pathlib import Path

from astroimp.mast import write_json
from astroimp.storage import linked

_lock = threading.RLock()


def _read(path: Path, default):
    if path.is_symlink() or path.is_junction():
        raise ValueError("Linked export history is not supported.")
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else default


def same_meta(left, right) -> bool:
    """Compare metadata after a browser's binary64 JSON round trip."""
    if isinstance(left, dict) and isinstance(right, dict):
        return left.keys() == right.keys() and all(same_meta(left[key], right[key]) for key in left)
    if isinstance(left, list) and isinstance(right, list):
        return len(left) == len(right) and all(same_meta(a, b) for a, b in zip(left, right))
    if type(left) in (int, float) and type(right) in (int, float):
        return float(left) == float(right)
    return type(left) is type(right) and left == right


def _validate(state, meta) -> None:
    if (not isinstance(state, dict) or not isinstance(state.get("layers"), list)
            or not isinstance(state.get("global"), dict)):
        raise ValueError("Expected complete export edits.")
    if (not isinstance(meta, dict) or any(type(meta.get(key)) is not int or meta[key] <= 0
                                        for key in ("width", "height"))):
        raise ValueError("Expected the export's build metadata.")


def save_checkpoint(session: Path, state: dict, meta: dict) -> str:
    _validate(state, meta)
    snapshot = copy.deepcopy(state)
    snapshot.pop("checkpoints", None)
    identity = {key: value for key, value in snapshot.items() if key != "selectedLayer"}
    content = json.dumps({"state": identity, "meta": meta}, sort_keys=True,
                         separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    identifier = hashlib.sha256(content.encode()).hexdigest()
    with _lock:
        root = session / "export-checkpoints"
        root.mkdir(exist_ok=True)
        if root.is_symlink() or root.is_junction():
            raise ValueError("Unsafe export checkpoint folder.")
        destination = root / f"{identifier}.json"
        if _read(destination, None) is None:
            write_json(destination, {"version": 1, "id": identifier,
                                    "created": datetime.now(timezone.utc).isoformat(),
                                    "state": snapshot, "meta": meta})
    return identifier


def checkpoint(session: Path, identifier: str) -> dict:
    if len(identifier) != 64 or any(char not in "0123456789abcdef" for char in identifier):
        raise ValueError("Invalid export checkpoint.")
    root = session / "export-checkpoints"
    if root.is_symlink() or root.is_junction():
        raise ValueError("Unsafe export checkpoint folder.")
    saved = _read(root / f"{identifier}.json", None)
    if not saved or saved.get("version") != 1 or saved.get("id") != identifier:
        raise ValueError("Export checkpoint is unavailable.")
    _validate(saved.get("state"), saved.get("meta"))
    return saved


def records(session: Path) -> dict:
    directory = session / "exports"
    if directory.is_symlink() or directory.is_junction():
        raise ValueError("Unsafe export history folder.")
    value = _read(session / "exports" / ".history.json", {})
    if not isinstance(value, dict):
        raise ValueError("Invalid export history.")
    return value


def record(session: Path, image: Path, identifier: str, *, filename: str | None = None) -> None:
    checkpoint(session, identifier)
    from astroimp.png_links import attach, link
    attach(image, link(session.name, identifier))
    with image.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    with _lock:
        history = records(session)
        history[filename or image.name] = {
            "checkpoint": identifier, "sha256": digest, "size_bytes": image.stat().st_size,
        }
        write_json(session / "exports" / ".history.json", history)


def capture(session: Path, image: Path, state: dict, meta: dict) -> str:
    identifier = save_checkpoint(session, state, meta)
    record(session, image, identifier)
    return identifier


def forget(session: Path, filename: str) -> None:
    with _lock:
        history = records(session)
        history.pop(filename, None)
        destination = session / "exports" / ".history.json"
        if history:
            write_json(destination, history)
        else:
            destination.unlink(missing_ok=True)


def image_checkpoint(session: Path, image: Path) -> str:
    filename = image.relative_to(session / "exports").as_posix()
    entry = records(session).get(filename)
    if not isinstance(entry, dict):
        raise ValueError("This export has no saved edit checkpoint.")
    if image.stat().st_size != entry.get("size_bytes"):
        raise ValueError("This image has changed since it was exported.")
    with image.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if digest != entry.get("sha256"):
        raise ValueError("This image has changed since it was exported.")
    identifier = entry.get("checkpoint")
    if not isinstance(identifier, str):
        raise ValueError("This export has no saved edit checkpoint.")
    checkpoint(session, identifier)
    return identifier


def _build_available(folder: Path, meta: dict) -> bool:
    if linked(folder) or linked(folder / "meta.json") or not folder.is_dir():
        return False
    if _read(folder / "meta.json", None) != meta:
        return False
    pixels = meta["width"] * meta["height"]
    for layer in [*meta.get("layers", []), meta.get("luminance") or {}]:
        for field, size in (("file", pixels * 2), ("coverage_file", pixels)):
            filename = layer.get(field)
            if filename is not None:
                if not isinstance(filename, str) or Path(filename).name != filename:
                    return False
                plane = folder / filename
                if linked(plane) or not plane.is_file() or plane.stat().st_size != size:
                    return False
    return True


def matching_build(session: Path, meta: dict) -> str | None:
    if _build_available(session / "project", meta):
        return None
    root = session / "builds"
    if root.is_dir() and not linked(root):
        for archive in sorted(root.iterdir()):
            if not linked(archive) and _build_available(archive / "project", meta):
                return archive.name
    raise ValueError("The original build or its image planes are no longer available. Current edits were not changed.")


def restore(session: Path, identifier: str) -> None:
    """Promote working edits to a return checkpoint before restoring a saved look."""
    from astroimp import builds
    from astroimp.edits import discard_star_state
    with _lock:
        saved = checkpoint(session, identifier)
        meta = saved["meta"]
        build = matching_build(session, meta)
        current = _read(session / "edits.json", None)
        previous = _read(session / "project" / "meta.json", None)
        if current is not None:
            _validate(current, previous)
            if current.get("checkpoints") is not None and not isinstance(current["checkpoints"], list):
                raise ValueError("Invalid checkpoint history. Current edits were not changed.")
        points = copy.deepcopy(current.get("checkpoints") or []) if current else []
        if current and previous:
            if previous != meta:
                # Manual checkpoints need their own build when carried across canvases.
                for point in points:
                    if not point.get("exportCheckpoint"):
                        point["exportCheckpoint"] = save_checkpoint(session, point["state"], previous)
            return_id = save_checkpoint(session, current, previous)
            if not any(point.get("exportCheckpoint") == return_id for point in points):
                points.append({"id": return_id, "exportCheckpoint": return_id,
                               "name": "Before opening exported version",
                               "created": datetime.now(timezone.utc).isoformat(),
                               "state": checkpoint(session, return_id)["state"]})
        state = discard_star_state(saved["state"])
        state["checkpoints"] = points
        state["sources"] = [layer["key"] for layer in meta.get("layers", [])]
        if build is not None:
            builds.restore(session, build, state=state)
        else:
            write_json(session / "edits.json", state)
            revision = _read(session / "edits.rev.json", {}).get("revision", 0)
            write_json(session / "edits.rev.json", {
                "revision": revision + 1, "source": "restore", "note": "Restored exported version",
            })
