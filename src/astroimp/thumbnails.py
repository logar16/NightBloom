"""Small, independently cached session artwork previews."""
from __future__ import annotations

import os
import threading
import uuid
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

from astroimp.mast import write_json
from astroimp.session import read_json

_locks = [threading.Lock() for _ in range(32)]
_extensions = {".png", ".jpg", ".jpeg", ".tif", ".tiff"}


def thumbnail(session: Path) -> Path:
    # Never share the job lock: reading an existing export must work during a new one.
    with _locks[hash(str(session)) % len(_locks)]:
        exports = []
        for folder, directories, filenames in os.walk(session / "exports"):
            directories[:] = [name for name in directories if not name.startswith(".")]
            for name in filenames:
                source = Path(folder) / name
                if not name.startswith(".") and source.suffix.lower() in _extensions:
                    try:
                        exports.append((source.stat().st_mtime_ns, source))
                    except FileNotFoundError:
                        pass
        sources = [path for _, path in sorted(exports, reverse=True)]
        sources.append(session / "snapshot.jpg")
        destination = session / "thumb.jpg"
        marker = session / "thumb.source.json"
        cached = read_json(marker, {})
        for source in sources:
            temporary = session / f".thumb-{uuid.uuid4().hex}.part"
            try:
                stat = source.stat()
                signature = {"source": str(source.relative_to(session)),
                             "mtime_ns": stat.st_mtime_ns, "size": stat.st_size}
                if destination.is_file() and cached == signature:
                    return destination
                with Image.open(source) as image:
                    image.draft("RGB", (480, 480))
                    image.thumbnail((480, 480))
                    if image.mode.startswith("I"):
                        # Pillow clamps 16-bit grayscale on RGB conversion instead of scaling it.
                        pixels = np.asarray(image, np.float32)
                        image = Image.fromarray(np.clip(pixels / 257, 0, 255).astype("uint8"))
                    image.convert("RGB").save(temporary, format="JPEG", quality=85)
                temporary.replace(destination)
                write_json(marker, signature)
                return destination
            except (OSError, ValueError, Image.DecompressionBombError):
                # An export may still be being written, or an older file may not be an image.
                continue
            finally:
                temporary.unlink(missing_ok=True)
        view = read_json(session / "view.json", {})
        if view.get("source") == "coordinates":
            signature = {"coordinates": view.get("center_icrs_deg"),
                         "field": view.get("field_arcmin"), "rotation": view.get("rotation_deg", 0)}
            if not destination.is_file() or cached != signature:
                ratio = view["pixel_dimensions"][0] / view["pixel_dimensions"][1]
                width, height = (480, round(480/ratio)) if ratio >= 1 else (round(480*ratio), 480)
                image = Image.new("RGB", (width, height), "#171b20")
                draw = ImageDraw.Draw(image)
                for i in range(1, 8):
                    x, y = round(width*i/8), round(height*i/8)
                    draw.line((x, 0, x, height), fill="#2a343e")
                    draw.line((0, y, width, y), fill="#2a343e")
                draw.line((width/2-9, height/2, width/2+9, height/2), fill="#5fc9d6")
                draw.line((width/2, height/2-9, width/2, height/2+9), fill="#5fc9d6")
                temporary = session / f".thumb-{uuid.uuid4().hex}.part"
                try:
                    image.save(temporary, format="JPEG", quality=85)
                    temporary.replace(destination)
                    write_json(marker, signature)
                finally:
                    temporary.unlink(missing_ok=True)
            return destination
        raise FileNotFoundError("No session thumbnail.")
