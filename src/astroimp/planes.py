"""Bounded conversion of the on-disk, top-down float16 science planes."""
from pathlib import Path

import numpy as np


def load_plane(path: Path, shape: tuple[int, int], *, step: int = 1,
               window: tuple[int, int, int, int] | None = None) -> np.ndarray:
    """Read float32 pixels, converting only a strip before any block-mean reduction."""
    mapped = np.memmap(path, "<f2", mode="r", shape=shape)
    try:
        if window:
            x0, y0, x1, y1 = window
        else:
            y0, x0 = 0, 0
            y1, x1 = shape
        h, w = (y1-y0)//step, (x1-x0)//step
        result = np.empty((h, w), np.float32)
        rows = max(1, 128//step)
        for y in range(0, h, rows):
            end = min(y+rows, h)
            block = np.asarray(mapped[y0+y*step:y0+end*step, x0:x0+w*step], np.float32)
            result[y:end] = (block.reshape(end-y, step, w, step).mean((1, 3))
                             if step > 1 else block)
        return result
    finally:
        mapped._mmap.close()
