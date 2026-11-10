"""Bounded MAST preview cache, independent of any project catalog."""
import hashlib
import io
from pathlib import Path
import threading

from PIL import Image
import requests

_locks = [threading.Lock() for _ in range(64)]


def archive(uri: str, cache: Path, size: str = "thumb") -> Path:
    if not uri.startswith("mast:"):
        raise ValueError("Expected a MAST preview URI.")
    digest = hashlib.sha1(uri.encode()).hexdigest()
    path = cache / f"{digest}.jpg"
    large = path.with_name(f"{digest}.large.jpg")
    requested = large if size == "large" else path
    with _locks[int(digest[:8], 16) % len(_locks)]:
        if not requested.exists():
            with requests.get("https://mast.stsci.edu/api/v0.1/Download/file",
                              params={"uri": uri}, timeout=(20, 90), stream=True) as response:
                response.raise_for_status()
                content = io.BytesIO()
                for chunk in response.iter_content(1024 * 1024):
                    content.write(chunk)
                    if content.tell() > 64 * 1024**2:
                        raise ValueError("Preview exceeds 64 MiB.")
            content.seek(0)
            with Image.open(content) as image:
                image.draft("RGB", (1400, 1400))
                image = image.convert("RGB")
                image.thumbnail((1400, 1400))
                path.parent.mkdir(parents=True, exist_ok=True)
                for destination, side in ((large, 1400), (path, 512)):
                    image.thumbnail((side, side))
                    temporary = destination.with_suffix(".part")
                    try:
                        image.save(temporary, format="JPEG", quality=88)
                        temporary.replace(destination)
                    finally:
                        temporary.unlink(missing_ok=True)
    return requested
