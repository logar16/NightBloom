from copy import deepcopy
import json
import math
from pathlib import Path
import shutil
import unittest
import uuid
from unittest.mock import patch

import numpy as np
from astropy.io import fits
from astropy.wcs import WCS

from astroimp.canvas import load, plan, prepare
from astroimp.fullres import plan as export_plan, subgrid
from astroimp.grid import GridMap, grid_from_meta, remap_markers, remap_state
from astroimp.mast import write_json


def grid(width=2119, height=3072, scale=.1485):
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = [274.7, -13.8]
    wcs.wcs.crpix = [width/2, height/2]
    wcs.wcs.cdelt = [-scale/3600, scale/3600]
    return {"width": width, "height": height, "wcs": dict(wcs.to_header()), "pixel_scale_arcsec": scale}


class CanvasRegionTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-canvas-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def metadata(self):
        old = grid()
        preview = {"width": 1024, "height": 1024, "pixel_scale_arcsec": old["pixel_scale_arcsec"]*8,
                   "wcs": dict(subgrid(WCS(old["wcs"]), old["height"], (-128, -128), (1024, 1024), .125).to_header()),
                   "reference": old, "current_scale_arcsec": old["pixel_scale_arcsec"],
                   "footprints": [{"key": "wide", "native_scale_arcsec": .04,
                                   "points": [[0, 0], [1024, 0], [1024, 1024], [0, 1024]]}]}
        return old, preview

    def region(self, old, metadata, bounds):
        p = GridMap(old, metadata).rect(bounds)
        return dict(zip(("x", "y", "w", "h"), [p[0], p[1], p[2]-p[0], p[3]-p[1]]))

    def test_square_top_right_keeps_scale_and_exact_integer_translation(self):
        old, metadata = self.metadata()
        region = self.region(old, metadata, [0, -256, 3072, 2816])
        result = plan(metadata, region)
        new = result["grid"]
        self.assertEqual((new["width"], new["height"]), (3072, 3072))
        self.assertEqual(new["pixel_scale_arcsec"], old["pixel_scale_arcsec"])
        mapping = GridMap(old, new)
        np.testing.assert_array_equal(mapping.translation, [0, 256])
        points = [[1., 2.], [40., 90.]]
        np.testing.assert_array_equal(mapping.points(points), np.array(points)+[0, 256])
        state = {"layers": [{"transform": {"x": .1, "y": .2, "scale": 1, "rotation": 0},
                             "brush": {"strokes": [{"points": points, "size": 12.}]}}],
                 "global": {"crop": {"x": .1, "y": .2, "w": .6, "h": .5}, "rotation": -39.58},
                 "starOverrides": {"5": {"gain": 1.7}}}
        state["checkpoints"] = [{"state": deepcopy(state)}]
        remapped = remap_state(state, old, new)
        np.testing.assert_array_equal(remapped["layers"][0]["brush"]["strokes"][0]["points"], np.array(points)+[0, 256])
        self.assertEqual(remapped["layers"][0]["brush"]["strokes"][0]["size"], 12.)
        self.assertNotIn("starOverrides", remapped)
        back = remap_state(remapped, new, old)
        for key in ("x", "y", "w", "h"):
            self.assertAlmostEqual(back["global"]["crop"][key], state["global"]["crop"][key], places=12)
        markers = [{"kind": "point", "x": 10., "y": 20., "radius": 4},
                   {"kind": "rect", "x1": 1., "y1": 2., "x2": 40., "y2": 80.}]
        moved = remap_markers(markers, old, new)
        self.assertEqual(moved[0], {**markers[0], "y": 276.})
        self.assertEqual(moved[1]["y1"], 258.)
        np.testing.assert_array_equal(remapped["checkpoints"][0]["state"]["layers"][0]["brush"]["strokes"][0]["points"],
                                      np.array(points)+[0, 256])

    def test_resolution_ignores_tiny_fine_image_and_wide_unused_extent(self):
        old, metadata = self.metadata()
        metadata["footprints"][0]["native_scale_arcsec"] = .2
        region = self.region(old, metadata, [0, 0, 3072, 3072])
        metadata["footprints"].append({"key": "tiny", "native_scale_arcsec": .005,
                                      "points": [[130, 130], [131, 130], [131, 131], [130, 131]]})
        result = plan(metadata, region)
        self.assertEqual(result["grid"]["pixel_scale_arcsec"], .2)
        self.assertLessEqual(max(result["grid"]["width"], result["grid"]["height"]), 3072)
        self.assertLess(result["kept"][0]["kept_fraction"], .2)
        larger = plan(metadata, {"x": 0, "y": 0, "w": 1024, "h": 1024})
        self.assertGreater(larger["grid"]["pixel_scale_arcsec"], result["grid"]["pixel_scale_arcsec"])

    def fixture(self):
        session, downloads = self.root / "session", self.root / "downloads"
        (session / "project").mkdir(parents=True)
        downloads.mkdir()
        meta = grid(32, 40, 1.)
        source = downloads / "a.fits"
        yy, xx = np.mgrid[:40, :32]
        fits.PrimaryHDU((1+xx*.1+yy*.03).astype("f4"), header=fits.Header(meta["wcs"])).writeto(source)
        write_json(session / "catalog.json", {"groups": [{"observations": [
            {"obs_id": "a", "filter": "F502N", "data_uri": "mast:HST/a.fits", "wavelength_nm": 502}]}]})
        write_json(session / "selection.json", {"obs_ids": ["a"]})
        meta["layers"] = [{"key": "a", "source": str(source.resolve())}]
        write_json(session / "project" / "meta.json", meta)
        return session, downloads, meta

    def test_preview_is_cached_by_sources_and_does_not_change_build_or_edits(self):
        session, downloads, meta = self.fixture()
        write_json(session / "edits.json", {"keep": "untouched"})
        before = (session / "edits.json").read_bytes()
        first = prepare(session, downloads)
        with patch("astroimp.canvas.reproject_interp", side_effect=AssertionError("cache missed")):
            second = prepare(session, downloads)
        self.assertEqual(first, second)
        self.assertEqual(load(session, first["id"], downloads), first)
        self.assertEqual((session / "edits.json").read_bytes(), before)
        self.assertEqual(json.loads((session / "project" / "meta.json").read_text()), meta)
        write_json(session / "selection.json", {"obs_ids": []})
        with self.assertRaises(ValueError):
            load(session, first["id"], downloads)

    def test_region_validation_and_native_export_stay_inside_chosen_canvas(self):
        session, downloads, old = self.fixture()
        metadata = prepare(session, downloads)
        for rect in ({"x": -1, "y": 0, "w": 20, "h": 20}, {"x": 0, "y": 0, "w": 0, "h": 2}):
            with self.assertRaises(ValueError):
                plan(metadata, rect)
        grid = plan(metadata, metadata["default_region"])["grid"]
        meta = {**old, **grid}
        write_json(session / "project" / "meta.json", meta)
        state = {"layers": [{"source": "a", "mode": "tint", "visible": True}],
                 "global": {"crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "stars": {"color": "layers"}}}
        result = export_plan(session, state=state)
        self.assertEqual((result["preview_width"], result["preview_height"]), (grid["width"], grid["height"]))
        self.assertGreaterEqual(result["native_factor"], 1)
        self.assertGreaterEqual(min(result["origin"]), 0)
        self.assertLessEqual(result["origin"][0]+result["shape"][1], math.ceil(grid["width"]*result["factor"]))
        self.assertEqual(grid_from_meta(meta)["region_sky"], grid["region_sky"])

    def test_canvas_api_checks_confirmation_then_builds_with_checkpoint_and_locked_region(self):
        from fastapi.testclient import TestClient
        from astroimp.server import create_app
        session, downloads, _ = self.fixture()
        state = {"layers": [], "global": {"crop": {"x": 0, "y": 0, "w": 1, "h": 1}}, "starOverrides": {}}
        write_json(session / "edits.json", state)
        metadata = prepare(session, downloads)
        app = create_app(self.root)
        app.state.sessions.current = session
        with TestClient(app) as client:
            body = {"preview_id": metadata["id"], "region": metadata["default_region"]}
            response = client.post("/api/build/canvas/plan", json=body)
            self.assertEqual(response.status_code, 200, response.text)
            grid = response.json()["grid"]
            self.assertEqual(client.post("/api/build/canvas", json={**body, "grid": {**grid, "width": 1}}).status_code, 409)
            self.assertEqual(client.post("/api/build/canvas", json={**body, "grid": grid}).status_code, 200)
            app.state.jobs.thread.join(timeout=10)
            self.assertEqual(app.state.jobs.payload()["state"], "done")
            self.assertEqual(json.loads((session / "grid.json").read_text()), grid)
            saved = json.loads((session / "edits.json").read_text())
            self.assertEqual(saved["checkpoints"][-1]["name"], "Before canvas change")
