"""Bounded, file-versioned metadata caches for interactive windowed rendering."""
from functools import lru_cache
import json
from pathlib import Path

import numpy as np



@lru_cache(maxsize=16)
def _json(path: Path, mtime: int, size: int):
    return json.loads(path.read_text(encoding="utf-8"))


def read_metadata(path: Path):
    stat = path.stat()
    return _json(path.resolve(), stat.st_mtime_ns, stat.st_size)


def windowed_array(path: Path, dtype: str, shape: tuple, window: tuple | None):
    if window is None:
        return np.fromfile(path, dtype).reshape(shape)
    mapped = np.memmap(path, dtype, mode="r", shape=shape)
    x0, y0, x1, y1 = window
    try:
        return np.array(mapped[y0:y1, x0:x1], copy=True)
    finally:
        mapped._mmap.close()


@lru_cache(maxsize=16)
def _brush(width: int, height: int, strokes: str, calculate):
    mask = calculate(width, height, json.loads(strokes))
    mask.setflags(write=False)
    return mask


def brush_coverage(width: int, height: int, strokes: list, calculate):
    return _brush(width, height, json.dumps(strokes, separators=(",", ":")), calculate)
