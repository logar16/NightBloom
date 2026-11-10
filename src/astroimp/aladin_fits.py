"""File-backed, input-versioned FITS responses for Aladin."""
import hashlib
import json
from pathlib import Path
import threading
import uuid

from astropy.io import fits
from fastapi.responses import FileResponse
import numpy as np

_locks = [threading.Lock() for _ in range(16)]


class FitsResponse(FileResponse):
    # Measured on the 3072-pixel fixture: avoid hundreds of thread-pool hops per plane.
    chunk_size = 4 * 1024 * 1024


def cached_fits(source: Path, meta: dict) -> Path:
    stat = source.stat()
    signature = [source.name, stat.st_mtime_ns, stat.st_size,
                 meta["width"], meta["height"], meta["wcs"]]
    key = hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()
    destination = source.parent.parent / ".aladin-fits" / f"{key}.fits"
    with _locks[hash(str(destination.resolve())) % len(_locks)]:
        if destination.is_file():
            return destination
        destination.parent.mkdir(exist_ok=True)
        temporary = destination.with_suffix(f".{uuid.uuid4().hex}.part")
        mapped = np.memmap(source, "<f2", mode="r", shape=(meta["height"], meta["width"]))
        try:
            image = mapped[::-1].astype(np.float32)
            fits.PrimaryHDU(image, header=fits.Header(meta["wcs"])).writeto(temporary)
            temporary.replace(destination)
        finally:
            mapped._mmap.close()
            temporary.unlink(missing_ok=True)
    return destination
