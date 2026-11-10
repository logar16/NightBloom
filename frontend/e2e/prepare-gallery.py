"""Generate representative exported images only inside a private E2E run."""
import os
import sys
from pathlib import Path

import numpy as np
import tifffile
from PIL import Image

from astroimp.render import Project, output, render
from astroimp.session import read_json

run = Path(sys.argv[1]).resolve()
repository = Path(__file__).resolve().parents[2]
if not run.is_relative_to(repository / "frontend" / "e2e" / ".run"):
    raise ValueError("Gallery fixtures require a private E2E run directory.")
for session in (run / "data" / "sessions").iterdir():
    if not (session / "project" / "meta.json").is_file():
        continue
    edits = read_json(session / "edits.json")
    rgb, _ = render(Project(session / "project", max_side=384), edits, collect=False)
    image = Image.fromarray(np.round(output(rgb, edits["global"]) * 255).astype("uint8"))
    directory = session / "exports"
    directory.mkdir(exist_ok=True)
    # Resized fixtures exercise gallery ordering, not native-detail claims.
    ratio = image.height / image.width
    image.resize((768, round(768 * ratio))).save(directory / "native-768.png")
    image.save(directory / "preview.png")
    nested = directory / "earlier"
    nested.mkdir(exist_ok=True)
    large = np.asarray(image.resize((512, round(512 * ratio)))).astype(np.uint16) * 257
    tifffile.imwrite(nested / "colour-16.tiff", large, photometric="rgb")
    for filename, stamp in ((directory / "native-768.png", 1_700_000_000),
                            (directory / "preview.png", 1_700_000_600),
                            (nested / "colour-16.tiff", 1_700_000_010)):
        os.utime(filename, (stamp, stamp))
