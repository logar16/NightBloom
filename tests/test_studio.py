from __future__ import annotations

import io
import shutil
import threading
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

import numpy as np
from fastapi.testclient import TestClient
from astropy.io import fits
from astropy.wcs import WCS
from PIL import Image

from astroimp.build import (block_mean, build, low_weight_cut,
                           normalize, prepare_science, repair_weight_streaks)
from astroimp.mast import write_json
from astroimp.session import BusyError, Jobs, Sessions


class StudioTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-studio-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def test_low_exposure_drizzle_fringe_is_removed_along_its_stepped_outline(self):
        pixels = np.ones((60, 60), np.float32)
        weight = np.zeros((60, 60), np.float32)
        weight[10:50, 10:50] = 3.0           # full-depth interior
        weight[10:30, 50:58] = 1.0           # one-exposure step on the right
        weight[50:56, 10:40] = 1.0           # another step along the bottom
        weight[30, 15:45] = 0.0              # thin interior cosmic-ray / bad-column streak
        weight[38:41, 38:41] = 0.0           # small saturated star core
        result = low_weight_cut(pixels, weight)
        self.assertTrue(np.isfinite(result[30, 25]))
        self.assertTrue(np.isfinite(result[39, 39]))
        self.assertTrue(np.isnan(result[20, 54]))
        self.assertTrue(np.isnan(result[53, 20]))
        self.assertTrue(np.isnan(result[5, 5]))
        self.assertEqual(result[30, 30], 1)
        # The interior is trimmed by only a couple of pixels at its border.
        self.assertTrue(np.isfinite(result[13, 13]))
        self.assertIs(low_weight_cut(pixels, None), pixels)

    def test_block_means_preserve_partial_edges_and_ignore_invalid_pixels(self):
        image = np.arange(35, dtype=np.float32).reshape(5, 7)
        image[:2, :2] = np.nan
        expected = np.array([[np.nan, 6, 8, 9.5], [18, 20, 22, 23.5],
                             [28.5, 30.5, 32.5, 34]], dtype=np.float32)
        np.testing.assert_allclose(block_mean(image, 2), expected)

    def test_thin_bent_weight_gap_is_repaired_before_block_means_without_erasing_stars(self):
        pixels = np.full((100, 100), 10, np.float32)
        weight = np.ones_like(pixels)
        weight[:10] = .1
        weight[25:70, 30:33] = 0
        weight[67:70, 30:80] = np.nan
        pixels[25:70, 30:33] = 1000
        pixels[67:70, 30:80] = -1000
        weight[64:73, 27:36] = 0  # a wider, contaminated chip-gap junction is not a star
        pixels[64:73, 27:36] = 1000
        pixels[40, 34] = 1000  # noisy high-weight gap edge
        weight[48:51, 36:39] = 0  # compact saturated core close to the seam
        pixels[48:51, 36:39] = 5000
        original = pixels.copy()
        repaired = repair_weight_streaks(pixels, weight)
        np.testing.assert_allclose(repaired[25:70, 30:33], 10, atol=.01)
        np.testing.assert_allclose(repaired[67:70, 30:80], 10, atol=.01)
        np.testing.assert_allclose(repaired[64:73, 27:36], 10, atol=.01)
        self.assertAlmostEqual(repaired[40, 34], 10, places=2)
        np.testing.assert_array_equal(repaired[48:51, 36:39], 5000)
        np.testing.assert_array_equal(pixels, original)
        before, after = low_weight_cut(pixels, weight), prepare_science(pixels, weight)
        np.testing.assert_array_equal(np.isnan(before), np.isnan(after))
        reduced = prepare_science(pixels, weight, 3)
        np.testing.assert_allclose(reduced[12:15, 10], 10, atol=.01)

    def test_weight_repair_keeps_compact_blobs_and_missing_weight_unchanged(self):
        pixels = np.ones((80, 80), np.float32)
        weight = np.ones_like(pixels)
        weight[20:28, 20:28] = 0
        pixels[20:28, 20:28] = 900
        np.testing.assert_array_equal(repair_weight_streaks(pixels, weight), pixels)
        self.assertIs(repair_weight_streaks(pixels, None), pixels)
        self.assertIs(repair_weight_streaks(pixels, np.zeros_like(weight)), pixels)

    def test_detector_bleed_connected_to_a_saturated_star_preserves_its_peak(self):
        yy, xx = np.mgrid[:300, :300]
        pixels = (10 + 5000 * np.exp(-((xx - 250)**2 + (yy - 150)**2) / 18)).astype(np.float32)
        weight = np.ones_like(pixels)
        weight[30:270, 249:252] = 0
        pixels[30:120, 249:252] = 1000
        weight[(xx - 250)**2 + (yy - 150)**2 < 8**2] = 0
        repaired = repair_weight_streaks(pixels, weight)
        self.assertEqual(repaired[150, 250], pixels[150, 250])
        np.testing.assert_allclose(repaired[40:100, 249:252], 10, atol=.01)
        self.assertTrue(np.isfinite(repaired).all())

    def test_repair_preserves_white_calibration_and_unaffected_star_peaks(self):
        yy, xx = np.mgrid[:100, :100]
        raw = (10 + xx * .1 + yy * .05).astype(np.float32)
        raw[30:70, 35:38] = 1000
        raw[80, 80] = 5000
        weight = np.ones_like(raw)
        weight[30:70, 35:38] = 0
        repaired, reference = prepare_science(raw, weight, return_reference=True)
        original, _, _ = normalize(low_weight_cut(raw, weight))
        result, _, _ = normalize(repaired, reference)
        self.assertEqual(result[80, 80], original[80, 80])
        np.testing.assert_array_equal(result[75:90, 75:90], original[75:90, 75:90])
        self.assertLess(result[50, 36], original[50, 36] / 10)

    def test_build_aligns_offset_images_and_writes_top_down_half_float_layers(self):
        downloads = self.root / "downloads" / "existing-target"
        downloads.mkdir(parents=True)
        session = self.root / "sessions" / "test"
        session.mkdir(parents=True)
        obs = []
        for index, shift in enumerate((0, 9)):
            wcs = WCS(naxis=2)
            wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
            wcs.wcs.crval = [20, 30]
            wcs.wcs.crpix = [30 + shift, 30]
            wcs.wcs.cd = [[-1 / 3600, 0], [0, 1 / 3600]]
            y, x = np.mgrid[:64, :64]
            image = (np.exp(-((x - 29 - shift)**2 + (y - 44)**2) / 6) * 50
                     + y * .001).astype(np.float32)
            name = f"image{index}.fits"
            header = wcs.to_header()
            for card in list(header):
                if card.startswith(("PC", "CDELT")):
                    del header[card]
            for card, value in {"CD1_1": -1 / 3600, "CD1_2": 0,
                                "CD2_1": 0, "CD2_2": 1 / 3600}.items():
                header[card] = value
            fits.HDUList([fits.PrimaryHDU(), fits.ImageHDU(
                image, header=header, name="SCI")]).writeto(downloads / name)
            obs.append({"obs_id": f"offset:{index}", "data_uri": f"mast:JWST/product/{name}",
                        "filter": "F200W", "collection": "JWST", "instrument": "NIRCAM",
                        "wavelength_nm": 2000, "info": {}})
        write_json(session / "catalog.json", {"groups": [{"observations": obs}]})
        write_json(session / "selection.json", {"obs_ids": [o["obs_id"] for o in obs]})
        updates = []
        def progress(*args):
            pass
        progress.update = lambda **values: updates.append(values)
        meta = build(session, downloads=downloads.parent, max_side=32, progress=progress)
        # Design decision: builds retain science, coverage and luminance, not star products.
        self.assertNotIn("stars", meta)
        self.assertTrue(all("starless" not in layer for layer in meta["layers"]))
        steps = next(update["steps"] for update in reversed(updates) if "steps" in update)
        self.assertTrue(all(step["state"] == "done" and step["progress"] == 1 for step in steps))
        self.assertTrue(any(update.get("phase") == "align" and update.get("eta_s") is not None
                            for update in updates))
        shape = meta["height"], meta["width"]
        self.assertLessEqual(max(shape), 32)
        images = [np.fromfile(session / "project" / o["file"], "<f2").reshape(shape)
                  for o in meta["layers"]]
        peaks = [np.unravel_index(np.argmax(im), shape) for im in images]
        np.testing.assert_allclose(peaks[0], peaks[1], atol=1)
        target = WCS(fits.Header(meta["wcs"]))
        north = target.pixel_to_world_values(peaks[0][1], shape[0] - 1 - peaks[0][0])[1]
        self.assertGreater(north, 30)
        self.assertAlmostEqual(north, 30 + 15 / 3600, delta=2 / 3600)
        self.assertLess(peaks[0][0], shape[0] / 2)
        self.assertFalse(list((session / "project").glob("*star*")))
        cached_time = (session / "project" / meta["layers"][0]["file"]).stat().st_mtime_ns
        build(session, downloads=downloads.parent, max_side=32)
        self.assertEqual((session / "project" / meta["layers"][0]["file"]).stat().st_mtime_ns,
                         cached_time)
        write_json(session / "session.json", {"stars": {"detect": True}})
        rebuilt = build(session, downloads=downloads.parent, max_side=32)
        self.assertNotIn("stars", rebuilt)
        self.assertFalse(list((session / "project").glob("*star*")))

    def test_luminance_averages_only_finite_coverage_and_preserves_cached_coverage(self):
        session = self.root / "session"
        session.mkdir()
        downloads = self.root / "downloads"
        downloads.mkdir()
        wcs = WCS(naxis=2)
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        wcs.wcs.crval = [20, 30]
        wcs.wcs.crpix = [32, 32]
        wcs.wcs.cdelt = [-1 / 3600, 1 / 3600]
        obs = []
        for index in range(2):
            image = np.repeat(np.arange(64, dtype=np.float32)[:, None], 64, axis=1)
            if index:
                image[:, 32:] = np.nan
            name = f"coverage{index}.fits"
            fits.PrimaryHDU(image, header=wcs.to_header()).writeto(downloads / name)
            obs.append({"obs_id": str(index), "data_uri": "mast:JWST/" + name,
                        "collection": "JWST", "filter": "F200W"})
        write_json(session / "catalog.json", {"groups": [{"observations": obs}]})
        write_json(session / "selection.json", {"obs_ids": ["0", "1"]})
        meta = build(session, downloads=downloads)
        shape = meta["height"], meta["width"]
        first = np.fromfile(session / "project" / meta["layers"][0]["file"], "<f2").reshape(shape)
        second_valid = np.fromfile(session / "project" / meta["layers"][1]["coverage_file"],
                                   "u1").reshape(shape).astype(bool)
        luminance = np.fromfile(session / "project" / meta["luminance"]["file"],
                               "<f2").reshape(shape)
        np.testing.assert_allclose(luminance[~second_valid], first[~second_valid], atol=.001)
        self.assertGreater(float(luminance[4, -4]), .5)
        build(session, downloads=downloads)
        cached = np.fromfile(session / "project" / meta["luminance"]["file"], "<f2")
        np.testing.assert_array_equal(cached, luminance.ravel())

    def test_job_rejects_concurrent_work_and_surfaces_failure(self):
        jobs = Jobs()
        release = threading.Event()
        self.addCleanup(release.set)

        def work(progress):
            progress("search", .5, "Searching")
            release.wait(5)
            raise ValueError("Archive unavailable")
        jobs.start("capture", work)
        with self.assertRaises(BusyError):
            jobs.start("build", work)
        release.set()
        jobs.thread.join(5)
        self.assertEqual(jobs.payload()["state"], "error")
        self.assertEqual(jobs.payload()["error"], "Archive unavailable")

    def test_session_reopens_selection_and_opaque_edits(self):
        path = self.root / "sessions" / "test"
        write_json(path / "catalog.json", {"groups": []})
        write_json(path / "selection.json", {"obs_ids": []})
        write_json(path / "edits.json", {"layers": [{"blend": "screen"}]})
        store = Sessions(path.parent)
        store.open("test")
        reopened = Sessions(path.parent).payload()
        self.assertEqual(reopened["edits"]["layers"][0]["blend"], "screen")
        self.assertEqual(reopened["id"], "test")
        with self.assertRaises(ValueError):
            store.open("..")

    def test_http_session_selection_project_edits_export_and_cached_preview(self):
        from astroimp.server import create_app

        session = self.root / "sessions" / "test"
        write_json(session / "catalog.json", {"groups": [{"observations": [
            {"obs_id": "one", "preview_uri": "mast:HST/one.jpg"}]}]})
        write_json(session / "selection.json", {"obs_ids": []})
        write_json(session / "project" / "meta.json", {
            "layers": [{"file": "one.f16", "starless": "one.starless.f16",
                        "coverage_file": "one.coverage.u8"}],
            "stars": {"file": "stars.u16", "list": "stars.json"},
            "luminance": {"file": "luminance.f16", "starless": "luminance.starless.f16"}})
        (session / "project" / "one.f16").write_bytes(b"\0\0" * 4)
        (session / "project" / "one.coverage.u8").write_bytes(b"\1\1\0\0")
        (session / "project" / "luminance.f16").write_bytes(b"\0\0" * 4)
        (session / "project" / "private.txt").write_text("not a project layer")
        app = create_app(self.root)
        client = self.enterContext(TestClient(app))
        self.assertEqual(client.get("/api/session").status_code, 404)
        self.assertEqual(client.post("/api/sessions/test/open").status_code, 200)
        self.assertNotIn("stars", client.get("/api/session").json())
        # Missing API paths can be rejected by the static GET-only mount with 405.
        self.assertIn(client.put("/api/session/stars", json={"detect": False}).status_code, (404, 405))
        self.assertIn(client.post("/api/stars/detect", json={"detect": True}).status_code, (404, 405))
        self.assertEqual(client.put("/api/selection", json={"obs_ids": ["unknown"]}).status_code, 400)
        result = client.put("/api/selection", json={"obs_ids": ["one", "one"]})
        self.assertEqual(result.json(), {"obs_ids": ["one"]})
        self.assertEqual(client.get("/api/project/one.f16").content, b"\0\0" * 4)
        self.assertEqual(client.get("/api/project/one.coverage.u8").content,
                         b"\1\1\0\0")
        self.assertEqual(client.get("/api/project/luminance.f16").content,
                         b"\0\0" * 4)
        self.assertEqual(client.get("/api/project/private.txt").status_code, 404)
        edits = {"layers": [{"intensity": 1.4}], "global": {"flipX": True}}
        self.assertEqual(client.put("/api/edits", json=edits).json(), {"revision": 1})
        self.assertEqual(client.get("/api/edits").json(), edits)
        self.assertEqual(client.put("/api/edits", content="null",
                                     headers={"Content-Type": "application/json"}).json(),
                         {"revision": 2})
        png = io.BytesIO()
        Image.new("RGB", (20, 10), "red").save(png, format="PNG")
        exported = client.post("/api/export", content=png.getvalue(),
                                 headers={"X-Name": "../../outside"})
        self.assertEqual(exported.status_code, 200)
        self.assertEqual(Path(exported.json()["path"]).parent, (session / "exports").resolve())
        self.assertEqual(client.post("/api/export", content=b"invalid").status_code,
                         400)
        response = unittest.mock.MagicMock()
        response.__enter__.return_value = response
        source = io.BytesIO()
        Image.new("RGB", (1800, 900), "blue").save(source, format="PNG")
        response.iter_content.return_value = [source.getvalue()]
        with patch("astroimp.server.requests.get", return_value=response) as download:
            for size in ("thumb", "large", "thumb"):
                preview = client.get(f"/api/preview/one?size={size}")
                self.assertEqual(preview.status_code, 200)
                with Image.open(io.BytesIO(preview.content)) as image:
                    self.assertEqual(max(image.size), 320 if size == "thumb" else 1400)
            self.assertEqual(download.call_count, 1)


if __name__ == "__main__":
    unittest.main()
