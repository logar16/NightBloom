"""Read-only storage inventory and explicitly scoped, reference-checked cleanup."""
from __future__ import annotations

import json
import os
import re
import shutil
import threading
import time
from datetime import datetime, timezone
from pathlib import Path, PureWindowsPath

from astroimp.build import filename


def files(root: Path):
    if root.is_dir():
        for directory, folders, names in os.walk(root, followlinks=False):
            folders[:] = [name for name in folders
                          if not linked(Path(directory) / name)]
            for name in names:
                path = Path(directory) / name
                if not linked(path) and path.is_file():
                    yield path


def linked(path: Path) -> bool:
    return path.is_symlink() or path.is_junction()


def size(root: Path) -> int:
    return sum(path.stat().st_size for path in files(root))


def read(path: Path, default):
    # An unreadable reference manifest must never turn protected sources into orphans.
    if not path.exists():
        return default
    return json.loads(path.read_text(encoding="utf-8"))


class Storage:
    def __init__(self, data: Path):
        self.data = Path(data).absolute()
        self.lock = threading.RLock()
        self._cached = None
        self._time = -float("inf")

    def inventory(self, *, fresh=False):
        with self.lock:
            if not fresh and self._cached is not None and time.monotonic() - self._time < 4:
                return self._cached
            result = self._inventory()
            self._cached, self._time = result, time.monotonic()
            return result

    def _inventory(self):
        data = self.data
        roots = []
        trash, backups = [], []
        sessions = data / "sessions"
        for folder in sorted(sessions.iterdir()) if sessions.exists() else []:
            if not folder.is_dir() or folder.name.startswith("."):
                continue
            category = "backups" if folder.name.startswith("_backup_") else "live"
            roots.append((folder, category))
        deleted = sessions / ".trash"
        for folder in sorted(deleted.iterdir()) if deleted.exists() else []:
            if folder.is_dir():
                roots.append((folder, "trash"))
        fixture_root = data / "test-fixtures"
        fixture_sessions = fixture_root / "sessions"
        if fixture_sessions.exists():
            roots.extend((folder, "fixtures") for folder in sorted(fixture_sessions.iterdir())
                         if folder.is_dir())
        elif fixture_root.exists():
            roots.append((fixture_root, "fixtures"))

        records, observations = [], {}
        for folder, category in roots:
            if linked(folder):
                raise ValueError("Cannot verify references in a linked project folder.")
            for directory, folders, names in os.walk(folder, followlinks=False):
                if any(linked(Path(directory) / name) for name in folders + names):
                    raise ValueError("Cannot verify references in linked project files.")
            manifests = list(files(folder))
            catalogs = [path for path in manifests if path.name == "catalog.json"]
            local = {}
            for path in catalogs:
                for group in read(path, {}).get("groups", []):
                    for observation in group.get("observations", []):
                        local[observation["obs_id"]] = observation
                        observations.setdefault(observation["obs_id"], []).append(observation)
            name = read(folder / "session.json", {}).get("name") or folder.name
            reference = {"id": folder.name, "name": name}
            records.append((category, reference, manifests, local))
            if category in {"trash", "backups"}:
                stamp = re.search(r"-(\d{8}T\d{6}Z)$", folder.name)
                date = (datetime.strptime(stamp[1], "%Y%m%dT%H%M%SZ").replace(
                    tzinfo=timezone.utc).isoformat() if stamp else
                    datetime.fromtimestamp(folder.stat().st_mtime, timezone.utc).isoformat())
                (trash if category == "trash" else backups).append({
                    **reference, "size": size(folder), "date": date})

        references = {}
        details = {}

        def add(name, category, reference, observation=None):
            if not name:
                return
            refs = references.setdefault(name.casefold(), {
                "live": [], "trash": [], "backups": [], "fixtures": []})
            if reference not in refs[category]:
                refs[category].append(reference)
            if observation:
                details[name.casefold()] = {
                    key: observation.get(key) for key in ("instrument", "filter")}

        for category, reference, manifests, local in records:
            def add_observation(identifier):
                candidates = [local[identifier]] if identifier in local else observations.get(identifier, [])
                for observation in candidates:
                    from astroimp import surveys
                    if surveys.is_survey(observation):
                        folder = data / "downloads" / "surveys" / surveys.slug(observation["hips_id"])
                        for source in files(folder):
                            add(source.relative_to(data / "downloads").as_posix(),
                                category, reference, observation)
                    else:
                        add(filename(observation), category, reference, observation)

            for path in manifests:
                if path.name == "selection.json":
                    for identifier in read(path, {}).get("obs_ids", []):
                        add_observation(identifier)
                elif path.name == "meta.json" and path.parent.name == "project":
                    for layer in read(path, {}).get("layers", []):
                        for tile in layer.get("tiles", []):
                            if tile.get("source"):
                                add(PureWindowsPath(tile["source"]).name, category, reference, tile)
                            if tile.get("obs_id"):
                                add_observation(tile["obs_id"])
                        source = layer.get("source") or (layer.get("source_signature") or [None])[0]
                        if source:
                            from astroimp import surveys
                            if surveys.is_survey(layer):
                                add_observation(layer["obs_id"])
                            else:
                                add(PureWindowsPath(source).name, category, reference, layer)
                        if layer.get("obs_id"):
                            add_observation(layer["obs_id"])

        downloads = []
        for path in sorted(files(data / "downloads")):
            relative = path.relative_to(data / "downloads").as_posix()
            reference_key = relative.casefold() if relative.startswith("surveys/") else path.name.casefold()
            refs = references.get(reference_key, {
                "live": [], "trash": [], "backups": [], "fixtures": []})
            status = ("in_use" if refs["live"] or refs["fixtures"] else
                      "trash_only" if refs["trash"] or refs["backups"] else "orphan")
            downloads.append({"path": path.relative_to(data / "downloads").as_posix(),
                              "name": path.name, "size": path.stat().st_size,
                              "status": status, "references": refs,
                              **details.get(reference_key, {})})
        totals = {"downloads": sum(item["size"] for item in downloads),
                  "aligned": size(data / "aligned_cache"), "previews": size(data / "cache"),
                  "sessions": sum(size(folder) for folder, category in roots if category == "live"),
                  "trash": sum(item["size"] for item in trash),
                  "backups": sum(item["size"] for item in backups),
                  "fixtures": size(fixture_root)}
        return {"totals": totals, "total": sum(totals.values()), "downloads": downloads,
                "trash": trash, "backups": backups}

    def safe(self, root: Path, name: str) -> Path:
        relative = PureWindowsPath(name)
        if (not name or relative.is_absolute() or relative.drive
                or any(part in {".", ".."} for part in relative.parts)
                or any(char in name for char in ':<>"|?*\x00')):
            raise ValueError("Invalid storage path.")
        path = root.joinpath(*relative.parts)
        if not path.resolve().is_relative_to(root.resolve()):
            raise ValueError("Storage path escapes its category.")
        # Do not traverse junctions, even when their targets happen to be inside data.
        for parent in (path, *path.parents):
            if linked(parent):
                raise ValueError("Linked storage paths cannot be deleted.")
            if parent == self.data:
                break
        if not path.resolve().is_relative_to(self.data.resolve()):
            raise ValueError("Storage path escapes data.")
        return path

    def delete(self, *, downloads=(), trash=(), backups=(), caches=()):
        with self.lock:
            snapshot = self.inventory(fresh=True)
            known = {item["path"]: item for item in snapshot["downloads"]}
            targets = []
            for name in set(downloads):
                path = self.safe(self.data / "downloads", name)
                if name not in known:
                    raise ValueError("Unknown download.")
                if known[name]["status"] == "in_use":
                    raise ValueError("A live project or test fixture uses this download.")
                targets.append(path)
            for category, names, root in (
                    ("trash", trash, self.data / "sessions" / ".trash"),
                    ("backups", backups, self.data / "sessions")):
                allowed = {item["id"] for item in snapshot[category]}
                for name in set(names):
                    path = self.safe(root, name)
                    if name not in allowed:
                        raise ValueError(f"Unknown {category} folder.")
                    targets.append(path)
            for name in set(caches):
                if name not in {"aligned", "previews"}:
                    raise ValueError("Unknown cache.")
                targets.append(self.safe(self.data, "aligned_cache" if name == "aligned" else "cache"))
            for path in targets:
                if path.is_dir():
                    # Validate the whole batch before removing any files.
                    for directory, folders, names in os.walk(path, followlinks=False):
                        if any(linked(Path(directory) / name) for name in folders + names):
                            raise ValueError("Linked storage paths cannot be deleted.")
            freed = 0
            try:
                for path in targets:
                    if path.is_dir():
                        amount = size(path)
                        shutil.rmtree(path)
                        freed += amount
                    elif path.is_file():
                        amount = path.stat().st_size
                        path.unlink()
                        freed += amount
            finally:
                self._cached = None
            return {"freed_bytes": freed}
