import json
import math
from pathlib import Path

import numpy as np
import pytest

from astroimp.contactsheet import view_transform
from astroimp.fullres import view_points
from astroimp.view import inside_crop, view_frame

CASES = json.loads((Path(__file__).parent / "view_cases.json").read_text())


@pytest.mark.parametrize("case", CASES)
@pytest.mark.parametrize("flip", [False, True])
def test_python_geometry_matches_typescript_vectors_and_contactsheet(case, flip):
    size, g = case["size"], {**case["g"], "flipX": flip}
    actual = view_frame(size, g)
    for key in ("x", "y", "w", "h", "width", "height"):
        assert actual[key] == pytest.approx(case["frame"][key], abs=1e-12)
    for p in case["probes"]:
        qx, qy = view_points(size, {"global": g}, p["output"]["x"], p["output"]["y"])
        assert qx / size["width"] == pytest.approx(1-p["image"]["x"] if flip else p["image"]["x"], abs=1e-12)
        assert qy / size["height"] == pytest.approx(p["image"]["y"], abs=1e-12)
        assert bool(inside_crop(size, g, p["output"]["x"], p["output"]["y"])) == p["inside"]
    # Encode source coordinates directly in pixel colours to probe the sampler.
    yy, xx = np.mgrid[:60, :100]
    rgb = np.stack(((xx+.5)/100, (yy+.5)/60, np.ones_like(xx)*.3), -1).astype(np.float32)
    image = view_transform(rgb, g, image_size=(1000, 600))
    oh, ow = (max(1, math.floor(actual[k] / 10 + .5)) for k in ("height", "width"))
    assert image.shape == (oh, ow, 3)
    for y, x in [(oh//2, ow//2), (0, 0), (oh-1, ow-1), (oh//2, ow//4)]:
        ox, oy = actual["x"]+(x+.5)/ow*actual["w"], actual["y"]+(y+.5)/oh*actual["h"]
        qx, qy = view_points(size, {"global": g}, ox, oy)
        expected = ([np.clip(qx/1000, .005, .995), np.clip(qy/600, 1/120, 119/120), .3]
                    if inside_crop(size, g, ox, oy) and 0 <= qx <= 1000 and 0 <= qy <= 600
                    else [.0784, .0824, .0902])
        np.testing.assert_allclose(image[y, x], expected, atol=2e-6)


def test_full_frame_anchor_is_ignored_and_reset_restores_legacy():
    size = {"width": 1000, "height": 600}
    for crop in ({}, {"rotation": -20}):
        g = {"rotation": 37, "crop": {"x": 0, "y": 0, "w": 1, "h": 1, **crop}}
        frame = view_frame(size, g)
        assert (frame["width"], frame["height"]) == (1000, 600)
