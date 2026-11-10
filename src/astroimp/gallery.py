"""Browse completed exports without opening projects or reading science planes."""
from __future__ import annotations

import hashlib
import io
import logging
import tempfile
import threading
from pathlib import Path, PurePosixPath

from PIL import Image, UnidentifiedImageError
from PIL.PngImagePlugin import PngInfo

from astroimp.session import Sessions, read_json
from astroimp.storage import files, linked
from astroimp.export_history import records

EXTENSIONS = {".png", ".jpg", ".jpeg", ".tif", ".tiff"}
_decode_lock = threading.Lock()
_log = logging.getLogger(__name__)


def project(sessions: Sessions, identifier: str) -> Path:
    path = sessions.resolve(identifier).resolve()
    if path.parent != sessions.root.resolve():
        raise ValueError("Unknown project.")
    return path


def export_file(session: Path, filename: str) -> Path:
    parts = PurePosixPath(filename).parts
    if (not parts or any(part.startswith(".") for part in parts)
            or any(char in filename for char in "\\:\0")
            or PurePosixPath(filename).is_absolute()):
        raise ValueError("Invalid export filename.")
    directory = (session / "exports").resolve(strict=True)
    if directory.parent != session.resolve():
        raise ValueError("Unsafe exports folder.")
    path = directory.joinpath(*parts).resolve(strict=True)
    if not path.is_relative_to(directory) or not path.is_file() or path.suffix.lower() not in EXTENSIONS:
        raise ValueError("Unknown exported image.")
    return path


def inventory(sessions: Sessions) -> list[dict]:
    albums = []
    if not sessions.root.exists():
        return albums
    for candidate in sessions.root.iterdir():
        if (linked(candidate) or not candidate.is_dir() or candidate.name.startswith((".", "_"))
                or not (candidate / "catalog.json").is_file()):
            continue
        folder = project(sessions, candidate.name)
        directory = folder / "exports"
        if linked(directory) or not directory.exists():
            continue
        history_error = None
        try:
            history = records(folder)
        except (ValueError, OSError) as exc:
            history = {}
            history_error = "Saved edit history is unavailable."
            _log.warning("Gallery %s: %s %s", folder, history_error, exc)
        images = []
        for source in files(directory):
            if source.suffix.lower() not in EXTENSIONS:
                continue
            filename = source.relative_to(directory).as_posix()
            if any(part.startswith(".") for part in PurePosixPath(filename).parts):
                continue
            source = export_file(folder, filename)
            stat = source.stat()
            width = height = None
            error = None
            try:
                with Image.open(source) as image:
                    width, height = image.size
            except (OSError, ValueError, UnidentifiedImageError, Image.DecompressionBombError) as exc:
                error = f"Cannot read image: {exc}"
                _log.warning("Gallery %s: %s", source, error)
            entry = history.get(filename, {})
            identifier = entry.get("checkpoint") if isinstance(entry, dict) else None
            saved = folder / "export-checkpoints"
            available = (isinstance(identifier, str) and len(identifier) == 64
                         and all(char in "0123456789abcdef" for char in identifier)
                         and not linked(saved) and not linked(saved / f"{identifier}.json")
                         and (saved / f"{identifier}.json").is_file())
            images.append({"filename": filename, "width": width, "height": height,
                           "size_bytes": stat.st_size, "modified": stat.st_mtime,
                           "version": f"{stat.st_mtime_ns}-{stat.st_size}", "error": error,
                           "checkpoint": identifier if available else None,
                           "checkpoint_error": history_error})
        if not images:
            continue
        images.sort(key=lambda item: (-((item["width"] or 0) * (item["height"] or 0)),
                                      -item["modified"], item["filename"]))
        saved = read_json(folder / "session.json", {})
        albums.append({"id": folder.name, "name": saved.get("name") or folder.name,
                       "updated": max(image["modified"] for image in images), "images": images})
    return sorted(albums, key=lambda album: (-album["updated"], album["id"]))


def encode(source: Path, kind: str) -> bytes:
    with Image.open(source) as original:
        if kind != "clipboard":
            side = 480 if kind == "thumbnail" else 2560
            original.draft("RGB", (side, side))
            original.thumbnail((side, side), Image.Resampling.LANCZOS)
        image = original.convert("RGB")
        buffer = io.BytesIO()
        if kind == "clipboard":
            metadata = PngInfo()
            if "nightbloom" in original.info:
                metadata.add_itxt("nightbloom", original.info["nightbloom"])
            image.save(buffer, format="PNG", pnginfo=metadata)
        else:
            image.save(buffer, format="JPEG", quality=88)
        return buffer.getvalue()


def preview(source: Path, cache: Path, kind: str) -> bytes:
    if kind not in {"thumbnail", "preview", "clipboard"}:
        raise ValueError("Unknown gallery image view.")
    # One large export is decoded at a time; scrolling must not multiply its memory footprint.
    with _decode_lock:
        if kind == "clipboard":
            return encode(source, kind)
        stat = source.stat()
        key = hashlib.sha256(f"v1:{source}:{stat.st_mtime_ns}:{stat.st_size}:{kind}".encode()).hexdigest()
        destination = cache / f"{key}.jpg"
        if destination.is_file():
            return destination.read_bytes()
        content = encode(source, kind)
        cache.mkdir(parents=True, exist_ok=True)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=cache, delete=False) as stream:
                temporary = Path(stream.name)
                stream.write(content)
            temporary.replace(destination)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
        return content
