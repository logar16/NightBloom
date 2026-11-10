from concurrent.futures import ThreadPoolExecutor
import io
from pathlib import Path
import shutil
import unittest
import uuid
from unittest.mock import patch

import numpy as np
from astropy.coordinates import SkyCoord
from astropy.wcs import WCS
from fastapi.testclient import TestClient
from PIL import Image

from astroimp import bookmarks
from astroimp.mast import write_json
from astroimp.server import create_app
from astroimp.session import read_json


def observation(key="one", ra=12, dec=20, width=4, height=2, collection="HST"):
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = [ra, dec]
    wcs.wcs.crpix = [1, 1]
    wcs.wcs.cdelt = [-1/60, 1/60]
    sky = wcs.pixel_to_world(np.array([-1, 1, 1, -1])*width/2,
                             np.array([-1, -1, 1, 1])*height/2)
    return {"obs_id": key, "collection": collection, "instrument": "WFC3", "filter": "F656N",
            "target": "Field", "wavelength_nm": 656, "ra": ra, "dec": dec,
            "region": "POLYGON ICRS " + " ".join(
                str(v) for pair in zip(sky.ra.deg, sky.dec.deg) for v in pair),
            "preview_uri": "mast:HST/product/preview.jpg"}


class BookmarkTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-bookmarks-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        self.store = bookmarks.Bookmarks(self.root)

    def test_store_dedupe_persistence_notes_and_remove(self):
        row = observation()
        item = self.store.add([row, row], "source", "Original")[0]
        self.assertEqual(len(self.store.add([row], "other", "Other")), 1)
        reloaded = bookmarks.Bookmarks(self.root)
        self.assertEqual(reloaded.get(item["id"])["source_name"], "Original")
        reloaded.note(item["id"], "Hydrogen edge")
        self.assertEqual(self.store.get(item["id"])["note"], "Hydrogen edge")
        with self.assertRaises(ValueError):
            self.store.note(item["id"], "x"*201)
        self.store.remove(item["id"])
        self.assertEqual(reloaded.list(), [])
        with self.assertRaises(KeyError):
            self.store.remove(item["id"])
        self.assertFalse(list(self.root.glob("*.tmp")))

    def test_concurrent_add_and_failed_batch_are_atomic(self):
        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(lambda index: bookmarks.Bookmarks(self.root).add(
                [observation(str(index))], "source", "Field"), range(12)))
        self.assertEqual(len(self.store.list()), 12)
        with self.assertRaises(ValueError):
            self.store.add([observation("new"), observation("survey", collection="Survey")], "s", "s")
        self.assertEqual(len(self.store.list()), 12)

    def test_single_frame_padding_and_aspects(self):
        row = observation()
        result = bookmarks.frame([row])
        np.testing.assert_allclose([result["ra"], result["dec"]], [12, 20], atol=1e-5)
        self.assertAlmostEqual(result["width_arcmin"], 4.4, places=3)
        self.assertFalse(result["exceeds_limit"])
        for aspect in ("16:9", "1:1", "9:16"):
            result = bookmarks.frame([observation(width=.01, height=.01)], aspect)
            self.assertGreaterEqual(min(result["width_arcmin"], result["height_arcmin"]), 1)

    def test_union_covers_separate_footprints_and_ra_wrap(self):
        for left, right in ((12, 12.1), (359.95, .05)):
            rows = [observation("a", left, width=2), observation("b", right, width=2)]
            result = bookmarks.frame(rows)
            self.assertFalse(result["exceeds_limit"])
            from astroimp.coordinates import synthetic_view
            wcs = WCS(synthetic_view(result["ra"], result["dec"],
                                     result["width_arcmin"], result["aspect"])[2])
            points = bookmarks._points(rows)
            x, y = wcs.world_to_pixel(SkyCoord(points[:, 0], points[:, 1], unit="deg"))
            self.assertTrue(((x >= -.5) & (x <= 1599.5)).all())
            self.assertTrue(((y >= -.5) & (y <= 899.5)).all())
            expected_ra = 12.05 if left == 12 else 0
            separation = SkyCoord(result["ra"], result["dec"], unit="deg").separation(
                SkyCoord(expected_ra, 20, unit="deg")).arcmin
            self.assertLess(separation, .001)

    def test_large_union_warns_and_clamps(self):
        result = bookmarks.frame([observation("a", 12), observation("b", 13)])
        self.assertTrue(result["exceeds_limit"])
        self.assertEqual(result["width_arcmin"], 30)
        self.assertGreater(result["required_width_arcmin"], 60)
        self.assertIn("cropped", result["warning"])
        with self.assertRaises(ValueError):
            bookmarks.frame([])

    def seed(self):
        source = self.root / "sessions" / "source"
        write_json(source / "catalog.json", {"groups": [{"observations": [
            observation(), observation("two", collection="JWST"),
            observation("survey", collection="Survey")]}]})
        write_json(source / "session.json", {"name": "Original field"})
        return source

    def test_global_api_validation_dedupe_and_origin_guard(self):
        self.seed()
        with TestClient(create_app(self.root)) as client:
            headers = {"X-AstroImp-Session": "nonexistent"}
            self.assertEqual(client.get("/api/bookmarks", headers=headers).json(), [])
            result = client.post("/api/bookmarks", headers=headers, json={
                "session": "source", "obs_ids": ["one", "two", "one"]})
            self.assertEqual(result.status_code, 200, result.text)
            self.assertEqual(len(result.json()), 2)
            identifier = result.json()[0]["id"]
            self.assertEqual(client.post("/api/bookmarks", json={
                "session": "source", "obs_ids": ["one"]}).json(), result.json())
            for ids in (["missing"], ["survey"]):
                self.assertEqual(client.post("/api/bookmarks", json={
                    "session": "source", "obs_ids": ids}).status_code, 400)
            self.assertEqual(client.patch(f"/api/bookmarks/{identifier}", json={
                "note": "x"*201}).status_code, 422)
            self.assertEqual(client.patch(f"/api/bookmarks/{identifier}", json={
                "note": "Keep"}).json()["note"], "Keep")
            for method, route, body in (
                ("post", "/api/bookmarks", {"session": "source", "obs_ids": ["one"]}),
                ("patch", f"/api/bookmarks/{identifier}", {"note": "bad"}),
                ("delete", f"/api/bookmarks/{identifier}", None),
            ):
                response = client.request(method, route, json=body, headers={"Origin": "https://evil.example"})
                self.assertEqual(response.status_code, 403)
            self.assertEqual(client.delete(f"/api/bookmarks/{identifier}", headers=headers).status_code, 200)
            self.assertEqual(client.delete(f"/api/bookmarks/{identifier}").status_code, 404)

    def test_preview_remains_available_after_source_deleted_and_uses_cache(self):
        source = self.seed()
        item = self.store.add([observation()], "source", "Field")[0]
        shutil.rmtree(source)
        image = io.BytesIO()
        Image.new("RGB", (20, 20), "orange").save(image, format="JPEG")
        with patch("astroimp.previews.requests.get") as request, TestClient(create_app(self.root)) as client:
            response = request.return_value.__enter__.return_value
            response.iter_content.return_value = [image.getvalue()]
            route = f"/api/bookmarks/{item['id']}/preview.jpg"
            self.assertEqual(client.get(route).status_code, 200)
            self.assertEqual(client.get(route).headers["content-type"], "image/jpeg")
            request.assert_called_once()

    def test_frame_api_payload_captures_correct_frame_without_removing_bookmarks(self):
        self.seed()
        with patch("astroimp.session.catalog", return_value={"groups": []}), \
                patch("astroimp.surveys.available", return_value=[]), \
                TestClient(create_app(self.root)) as client:
            items = client.post("/api/bookmarks", json={"session": "source", "obs_ids": ["one", "two"]}).json()
            result = client.post("/api/bookmarks/frame", json={"ids": [item["id"] for item in items]})
            self.assertEqual(result.status_code, 200, result.text)
            frame = result.json()
            response = client.post("/api/capture/coordinates", json={
                **{key: frame[key] for key in ("ra", "dec", "aspect", "width_arcmin")},
                "name": "Bookmarks"})
            self.assertEqual(response.status_code, 200, response.text)
            client.app.state.jobs.thread.join(10)
            self.assertEqual(client.app.state.jobs.job.state, "done")
            view = read_json(client.app.state.sessions.current / "view.json")
            np.testing.assert_allclose(view["center_icrs_deg"], [frame["ra"], frame["dec"]])
            self.assertAlmostEqual(view["field_arcmin"][0], frame["width_arcmin"])
            self.assertEqual(len(client.get("/api/bookmarks").json()), 2)
