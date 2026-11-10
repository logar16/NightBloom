"""Decimate only the copied project for bounded software-renderer integration tests."""

import json
import math
import os
import sys
from pathlib import Path

import numpy as np

project = Path(sys.argv[1]).resolve()
if ".run" not in project.parts:
    raise ValueError("Only copied .run projects may be prepared")

meta_path = project / "meta.json"
meta = json.loads(meta_path.read_text())
height, width = meta["height"], meta["width"]
long_side = 3072 if os.environ.get("ASTROIMP_PERF_STAGE") else 384
stride = max(1, math.ceil(max(height, width) / long_side))
for file in project.iterdir():
    # Design decision: copied legacy projects must load without retired assets,
    # rather than quietly relying on empty labels or duplicated starless planes.
    if file.name in {"stars.u16", "stars.json"} or file.name.endswith((".starless.f16", ".natural.f16")):
        file.unlink()
        continue
    if file.name.endswith(".edge.f16"):
        # Recompute distances from decimated coverage, in the copied grid's pixel units.
        file.unlink()
        continue
    if file.suffix not in {".f16", ".u16", ".u8"}:
        continue
    dtype = np.dtype("u1" if file.suffix == ".u8" else "<u2")
    if file.stat().st_size != height * width * dtype.itemsize:
        continue
    data = np.fromfile(file, dtype=dtype).reshape(height, width)
    data[::stride, ::stride].copy().tofile(file)
meta.pop("stars", None)
for layer in [*meta["layers"], meta.get("luminance", {})]:
    layer.pop("starless", None)
meta["width"], meta["height"] = math.ceil(width / stride), math.ceil(height / stride)
meta["pixel_scale_arcsec"] *= stride
meta_path.write_text(json.dumps(meta))
