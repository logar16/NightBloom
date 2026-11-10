from __future__ import annotations

from pathlib import Path
import shutil
import threading
import unittest
import uuid
from unittest.mock import Mock, patch

import numpy as np
import requests
from astropy.coordinates import SkyCoord
from astropy.io import fits
from astropy.wcs import WCS
from fastapi.testclient import TestClient

from astroimp import coordinates, surveys
from astroimp.build import build
from astroimp.mast import write_json
from astroimp.server import create_app
from astroimp.session import Sessions, capture_coordinates, read_json, rotate_capture, snapshot_header
from astroimp.thumbnails import thumbnail


class CoordinateTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-coordinates-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        coordinates._cache.clear()
        self.addCleanup(coordinates._cache.clear)

    def test_degree_and_sexagesimal_formats(self):
        for value in ("161.26 -59.87", "161.26, -59.87", "10:37:00 -59:52:00",
                      "10:37:00,-59:52:00", "10h37m -59d52m"):
            with self.subTest(value=value):
                result = coordinates.parse_position(value)
                self.assertIsNotNone(result)
                expected = (161.26, -59.87) if value.startswith("161") else (159.25, -59-52/60)
                np.testing.assert_allclose([result.ra.deg, result.dec.deg], expected)
        self.assertIsNone(coordinates.parse_position("NGC 3324"))
        for value in ("", "400 -20", "12 91", "10:99:00 -20:00:00"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                coordinates.parse_position(value)

    def test_names_use_mast_first_then_sesame_and_coordinates_skip_both(self):
        with patch("astroimp.coordinates.mast_request", return_value={"resolvedCoordinate": [
                {"canonicalName": "NGC  3324", "ra": 159.34, "decl": -58.615}]}) as mast, \
                patch("astroimp.coordinates._sesame") as sesame:
            self.assertEqual(coordinates.resolve("NGC 3324")["name"], "NGC 3324")
            sesame.assert_not_called()
            coordinates.resolve("161.26 -59.87")
            self.assertEqual(mast.call_count, 1)
        with patch("astroimp.coordinates.mast_request", return_value={}), \
                patch("astroimp.coordinates._sesame", return_value=SkyCoord(12, -20, unit="deg")):
            self.assertEqual(coordinates.resolve("M16")["ra"], 12)
        with patch("astroimp.coordinates.mast_request", return_value={}), \
                patch("astroimp.coordinates._sesame", side_effect=ValueError("not found")), \
                self.assertRaisesRegex(ValueError, "No match for 'xyz'"):
            coordinates.resolve("xyz")
        with patch("astroimp.coordinates.mast_request", side_effect=requests.Timeout(MAST_URL)), \
                patch("astroimp.coordinates._sesame", side_effect=ValueError("not found")), \
                self.assertRaisesRegex(RuntimeError, "MAST isn't responding"):
            coordinates.resolve("xyz")

    def rows(self, width=4, height=2):
        wcs = WCS(naxis=2)
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        wcs.wcs.crval = [359.99, 20]
        wcs.wcs.crpix = [1, 1]
        wcs.wcs.cdelt = [-1/60, 1/60]
        sky = wcs.pixel_to_world(np.array([-1, 1, 1, -1])*width/2,
                                  np.array([-1, -1, 1, 1])*height/2)
        region = "POLYGON ICRS " + " ".join(str(v) for pair in zip(sky.ra.deg, sky.dec.deg) for v in pair)
        return [{"obs_id": "one", "obs_collection": "HST", "instrument_name": "WFC3",
                 "s_region": region}]

    def test_frame_padding_aspect_clamp_wrap_and_no_covering_default(self):
        rows = self.rows()
        frame = coordinates.suggest_frame((359.99, 20), rows)
        self.assertAlmostEqual(frame["width_arcmin"], 4.4, places=3)
        self.assertAlmostEqual(frame["width_arcmin"]/frame["height_arcmin"], 16/9, places=3)
        self.assertEqual(frame["covering"], 1)
        self.assertEqual(coordinates.suggest_frame((0, -40), rows)["width_arcmin"], 10)
        for aspect in coordinates.ASPECTS:
            small = coordinates.suggest_frame((359.99, 20), self.rows(.01, .01), aspect)
            large = coordinates.suggest_frame((359.99, 20), self.rows(60, 60), aspect)
            self.assertGreaterEqual(min(small["width_arcmin"], small["height_arcmin"]), 1)
            self.assertLessEqual(max(large["width_arcmin"], large["height_arcmin"]), 30)

    def test_coverage_counts_unique_observations_caches_position_and_handles_timeout(self):
        row = self.rows()[0]
        with patch("astroimp.coordinates.mast_request", return_value={"data": [row, row]}) as request:
            first = coordinates.coverage(359.99, 20)
            second = coordinates.coverage(359.99, 20, "1:1")
            self.assertEqual(first["counts"], {"HST": 1, "JWST": 0})
            self.assertEqual(second["frame"]["aspect"], "1:1")
            self.assertEqual(request.call_count, 1)
        with patch("astroimp.coordinates.mast_request", side_effect=requests.Timeout(MAST_URL)):
            self.assertIn("MAST isn't responding", coordinates.coverage(10, -20)["warning"])
        with patch("astroimp.coordinates.mast_request", return_value={"data": []}) as request:
            self.assertIsNone(coordinates.coverage(10, -20, refresh=True)["warning"])
            request.assert_called_once()

    def test_slow_coverage_returns_pending_and_coalesces_concurrent_checks(self):
        release = threading.Event()
        def delayed(*args, **kwargs):
            release.wait(5)
            return {"data": self.rows()}
        with patch("astroimp.coordinates.mast_request", side_effect=delayed) as request, \
                patch("astroimp.coordinates.COVERAGE_WAIT", .01):
            try:
                self.assertTrue(coordinates.coverage(359.99, 20)["pending"])
                self.assertTrue(coordinates.coverage(359.99, 20, "1:1")["pending"])
                with coordinates._lock:
                    done = coordinates._inflight[(359.99, 20)]
                release.set()
                self.assertTrue(done.wait(5))
                result = coordinates.coverage(359.99, 20)
                self.assertFalse(result["pending"])
                self.assertEqual(result["counts"]["HST"], 1)
                self.assertEqual(request.call_count, 1)
            finally:
                release.set()

    def test_zero_rotation_archive_build_keeps_the_chosen_coordinate_frame(self):
        from astroimp.catalog import Observation
        from astroimp.fill import grid_view
        source = coordinates.synthetic_view(12, -20, 10, "1:1")[2]
        source["NAXIS1"], source["NAXIS2"] = 64, 64
        source_wcs = WCS(source).celestial
        source_wcs.wcs.crpix = [32.5, 32.5]
        source_wcs.wcs.cdelt = [-10/60/64, 10/60/64]
        downloads = self.root / "downloads"
        downloads.mkdir(parents=True)
        fits.writeto(downloads / "hst.fits", np.arange(64*64, dtype=np.float32).reshape(64, 64),
                     source_wcs.to_header())
        observation = Observation("1", "hst", "HST", "WFC3", "F656N", "1", "Field", "",
                                  "", 12, -20, 656, 1, "", "mast:HST/product/hst.fits", "")
        with patch("astroimp.catalog.query", return_value=[observation]), \
                patch("astroimp.surveys.available", return_value=[]):
            path = capture_coordinates(self.root / "sessions", 12, -20, 2, "16:9")
        write_json(path / "selection.json", {"obs_ids": ["hst"]})
        meta = build(path, downloads=downloads, max_side=32)
        actual = grid_view(meta)
        np.testing.assert_allclose(actual["center_icrs_deg"], [12, -20], atol=1e-8)
        self.assertAlmostEqual(actual["field_arcmin"][0], 2, delta=.15)
        self.assertGreater(meta["width"]/meta["height"], 1.6)

    def test_capture_rotation_payload_thumbnail_build_and_edit_without_snapshot(self):
        band = surveys.BY_ID["CDS/P/DSS2/red"]
        with patch("astroimp.session.catalog", return_value={"groups": []}), \
                patch("astroimp.surveys.available", return_value=[band]):
            path = capture_coordinates(self.root / "sessions", 359.99, 20, 2, "16:9", 30, "Test field")
            self.assertFalse((path / "snapshot.jpg").exists())
            header = snapshot_header(path)
            self.assertTrue(WCS(header).has_celestial)
            self.assertEqual(read_json(path / "view.json")["source"], "coordinates")
            rotate_capture(path, 90)
            rotate_capture(path, 0)
        view = read_json(path / "view.json")
        np.testing.assert_allclose(view["center_icrs_deg"], [359.99, 20])
        self.assertTrue(thumbnail(path).is_file())
        payload = Sessions(self.root / "sessions").payload(path)
        self.assertEqual(payload["name"], "Test field")
        obs = payload["catalog"]["groups"][0]["observations"][0]
        self.assertTrue(obs["pixels"])
        write_json(path / "selection.json", {"obs_ids": [obs["obs_id"]]})

        def pixels(band, wcs, progress):
            y, x = np.mgrid[:wcs.array_shape[0], :wcs.array_shape[1]]
            return (x+y+.2*np.sin(x)).astype(np.float32)
        with patch("astroimp.surveys.cutout", side_effect=pixels):
            meta = build(path, downloads=self.root / "downloads", max_side=32)
        from test_assistant_edits import layer as editor_layer, state as editor_state
        from astroimp.render import Project, render
        state = editor_state()
        state["layers"] = [editor_layer("DSS2 red", meta["layers"][0]["key"])]
        write_json(path / "edits.json", state)
        image, _ = render(Project(path / "project"), state)
        self.assertEqual(image.shape[:2], (meta["height"], meta["width"]))
        self.assertIsNotNone(Sessions(self.root / "sessions").payload(path)["edits"])

    def test_endpoint_creates_scoped_job_and_rejects_aladin_refresh_without_contact(self):
        app = create_app(self.root)
        with patch("astroimp.session.catalog", return_value={"groups": []}), \
                patch("astroimp.surveys.available", side_effect=requests.Timeout("cds.unistra.fr")), \
                patch("astroimp.server.scripts") as samp, TestClient(app) as client:
            response = client.post("/api/capture/coordinates", json={
                "ra": 161.26, "dec": -59.87, "name": "Field", "surveys_only": True})
            self.assertEqual(response.status_code, 200)
            app.state.jobs.thread.join(5)
            self.assertEqual(app.state.jobs.job.state, "done")
            payload = client.get("/api/session").json()
            self.assertEqual(payload["name"], "Field")
            self.assertFalse(payload["catalog"]["mast_searched"])
            self.assertIn("CDS isn't responding", payload["catalog"]["survey_warning"])
            self.assertEqual(client.post("/api/session/snapshot/refresh").status_code, 400)
            samp.assert_not_called()
            self.assertEqual(client.get(f"/api/sessions/{payload['id']}/thumbnail.jpg").status_code, 200)
            for body in ({"ra": 12, "dec": 91}, {"ra": 12, "dec": 0, "aspect": "invalid"}):
                self.assertIn(client.post("/api/capture/coordinates", json=body).status_code, (400, 422))

    def test_coordinate_backdrop_tries_independent_preview_while_cds_is_down(self):
        from PIL import Image
        path = self.root / "sessions" / "one"
        write_json(path / "catalog.json", {"groups": []})
        write_json(path / "view.json", {"source": "coordinates"})
        preview = self.root / "preview.jpg"
        Image.new("RGB", (4, 4), "white").save(preview)
        health = Mock()
        health.snapshot.return_value = {"cds": {"ok": False}}
        app = create_app(self.root, health=health)
        app.state.sessions.open("one")
        with patch.object(surveys, "preview", return_value=preview) as fetch, TestClient(app) as client:
            response = client.get("/api/session/backdrop.jpg")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.content, preview.read_bytes())
        fetch.assert_called_once()


MAST_URL = coordinates.MAST_URL
