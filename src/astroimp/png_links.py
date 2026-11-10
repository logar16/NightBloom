"""Portable saved-look pointers without changing PNG pixels or bit depth."""
from __future__ import annotations

import json
import shutil
import struct
import tempfile
import zlib
from pathlib import Path

SIGNATURE = b"\x89PNG\r\n\x1a\n"
PREFIX = b"nightbloom\0\0\0\0\0"


def link(project: str, checkpoint: str) -> dict:
    return {"version": 1, "project": project, "checkpoint": checkpoint}


def chunk(value: dict) -> bytes:
    data = PREFIX + json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    content = b"iTXt" + data
    return struct.pack(">I", len(data)) + content + struct.pack(">I", zlib.crc32(content))


def read(image: Path) -> dict | None:
    with image.open("rb") as source:
        if source.read(8) != SIGNATURE:
            return None
        while True:
            header = source.read(8)
            if len(header) != 8:
                raise ValueError("Incomplete PNG.")
            size, kind = struct.unpack(">I4s", header)
            if kind in (b"IDAT", b"IEND"):
                return None
            if kind == b"iTXt" and size <= 4096:
                data = source.read(size)
                crc = source.read(4)
                if data.startswith(b"nightbloom\0"):
                    if (not data.startswith(PREFIX) or len(crc) != 4
                            or zlib.crc32(kind + data) != struct.unpack(">I", crc)[0]):
                        raise ValueError("Damaged PNG saved-look metadata.")
                    return json.loads(data[len(PREFIX):])
            else:
                source.seek(size + 4, 1)


def attach(image: Path, value: dict) -> None:
    temporary = None
    try:
        with image.open("rb") as source:
            header = source.read(33)
            if not header.startswith(SIGNATURE):
                return
            if len(header) != 33 or header[8:16] != b"\0\0\0\rIHDR":
                raise ValueError("Invalid PNG header.")
            existing = read(image)
            if existing is not None:
                if existing != value:
                    raise ValueError("PNG saved-look link does not match this export.")
                return
            with tempfile.NamedTemporaryFile(dir=image.parent, delete=False) as output:
                temporary = Path(output.name)
                output.write(header)
                output.write(chunk(value))
                shutil.copyfileobj(source, output)
        temporary.replace(image)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
