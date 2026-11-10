import os
import shutil
import socket
import threading
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

import numpy as np
import requests
import uvicorn
from PIL import Image

from astroimp.aladin import AladinView, Snapshot
from astroimp.build import build, download, write_array
from astroimp.mast import write_json
from astroimp.server import create_app
from astroimp.session import BusyError, Jobs, Sessions, capture, read_json
from astroimp.thumbnails import thumbnail


class HomeJobTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-home-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def session(self, identifier="test"):
        path = self.root / "sessions" / identifier
        write_json(path / "catalog.json", {"groups": []})
        write_json(path / "selection.json", {"obs_ids": []})
        write_json(path / "session.json", {"name": identifier, "created": "2020-01-01T00:00:00Z"})
        return path

    def test_session_activity_includes_edits_selection_name_and_catalog(self):
        for index, filename in enumerate(("edits.json", "selection.json", "session.json", "catalog.json")):
            path = self.session(str(index))
            write_json(path / "edits.json", {})
            for source in path.glob("*.json"):
                os.utime(source, (100, 100))
            os.utime(path / filename, (200 + index, 200 + index))
        entries = Sessions(self.root / "sessions").list()
        self.assertEqual([item["id"] for item in entries], ["3", "2", "1", "0"])
        self.assertTrue(entries[0]["updated"].startswith("1970-01-01T00:03:23"))

    def test_session_summary_fallback_persists_without_reparsing_or_changing_activity(self):
        path = self.session()
        content = {"groups": [{"targets": ["M16"], "observations": [
            {"obs_id": "a", "target": "Eagle"}, {"obs_id": "b", "target": "Eagle"}]}]}
        write_json(path / "catalog.json", content)
        before = (path / "session.json").stat().st_mtime_ns
        sessions = Sessions(self.root / "sessions")
        first = sessions.list()[0]
        self.assertEqual((first["observations"], first["groups"], first["target_hint"]), (2, 1, "M16"))
        self.assertEqual((path / "session.json").stat().st_mtime_ns, before)
        self.assertIn("catalog_summary", read_json(path / "session.json"))
        from astroimp.session import read_json as original
        def no_catalog(file, default=None):
            self.assertNotEqual(file.name, "catalog.json")
            return original(file, default)
        with patch("astroimp.session.read_json", side_effect=no_catalog):
            self.assertEqual(sessions.list()[0], first)
        content["groups"].append({"targets": [], "observations": [{"obs_id": "c"}]})
        write_json(path / "catalog.json", content)
        refreshed = sessions.list()[0]
        self.assertEqual((refreshed["observations"], refreshed["groups"]), (3, 2))
        self.assertEqual(refreshed["name"], "test")

    def test_summary_backfill_without_session_metadata_does_not_create_recent_activity(self):
        path = self.session()
        (path / "session.json").unlink()
        for file in path.glob("*.json"):
            os.utime(file, (100, 100))
        listing = Sessions(self.root / "sessions").list()[0]
        self.assertEqual(listing["updated"], "1970-01-01T00:01:40+00:00")
        self.assertEqual((path / "session.json").stat().st_mtime_ns,
                         (path / "catalog.json").stat().st_mtime_ns)

    def test_thumbnail_prefers_newest_readable_export_ignores_work_and_caches(self):
        path = self.session()
        Image.new("RGB", (800, 400), "red").save(path / "snapshot.jpg")
        exports = path / "exports"
        exports.mkdir()
        Image.new("RGB", (800, 400), "blue").save(exports / "old.png")
        Image.new("RGB", (800, 400), "green").save(exports / "new.png")
        os.utime(exports / "old.png", (100, 100))
        os.utime(exports / "new.png", (200, 200))
        work = exports / ".fullres-partial"
        work.mkdir()
        Image.new("RGB", (800, 400), "yellow").save(work / "partial.png")
        (exports / "broken.png").write_bytes(b"incomplete")
        thumb = thumbnail(path)
        with Image.open(thumb) as image:
            self.assertEqual(image.size, (480, 240))
            self.assertGreater(image.getpixel((0, 0))[1], 100)
        modified = thumb.stat().st_mtime_ns
        thumbnail(path)
        self.assertEqual(thumb.stat().st_mtime_ns, modified)
        Image.new("RGB", (800, 400), "blue").save(exports / "new.png")
        with Image.open(thumbnail(path)) as image:
            self.assertGreater(image.getpixel((0, 0))[2], 240)
        shutil.rmtree(exports)
        with Image.open(thumbnail(path)) as image:
            self.assertGreater(image.getpixel((0, 0))[0], 240)
        (path / "snapshot.jpg").unlink()
        with self.assertRaises(FileNotFoundError):
            thumbnail(path)

    def test_thumbnail_scales_16_bit_grayscale_without_clipping(self):
        path = self.session()
        exports = path / "exports"
        exports.mkdir()
        Image.fromarray(np.full((700, 1000), 32768, np.uint16)).save(exports / "gray.png")
        with Image.open(thumbnail(path)) as image:
            self.assertEqual(max(image.size), 480)
            self.assertAlmostEqual(image.getpixel((0, 0))[0], 127, delta=2)

    def test_thumbnail_reads_16_bit_rgb_exports(self):
        import tifffile
        from astroimp.fullres import write_png16
        pixels = np.full((600, 800, 3), [16384, 32768, 49152], np.uint16)
        for format in ("png", "tiff"):
            with self.subTest(format=format):
                path = self.session(format)
                exports = path / "exports"
                exports.mkdir()
                if format == "png":
                    write_png16(exports / "rgb.png", pixels)
                else:
                    tifffile.imwrite(exports / "rgb.tiff", pixels, photometric="rgb")
                with Image.open(thumbnail(path)) as image:
                    np.testing.assert_allclose(image.getpixel((0, 0)), [64, 128, 192], atol=2)

    def test_cancel_keeps_slot_busy_until_worker_cleans_up(self):
        jobs = Jobs()
        entered, release, cleaning, finish = [threading.Event() for _ in range(4)]
        self.addCleanup(release.set)
        self.addCleanup(finish.set)
        def work(progress):
            entered.set()
            try:
                release.wait(5)
                progress("download", .5, "Halfway")
            finally:
                cleaning.set()
                finish.wait(5)
        jobs.start("build", work)
        self.assertTrue(entered.wait(5))
        self.assertTrue(jobs.cancel()["cancel_requested"])
        release.set()
        self.assertTrue(cleaning.wait(5))
        self.assertTrue(jobs.busy())
        with self.assertRaises(BusyError):
            jobs.start("capture", lambda _: None)
        finish.set()
        jobs.thread.join(5)
        self.assertEqual(jobs.payload()["state"], "cancelled")
        self.assertIsNone(jobs.payload()["error"])
        self.assertFalse(jobs.busy())
        jobs.start("capture", lambda _: None)
        jobs.thread.join(5)
        self.assertEqual(jobs.payload()["state"], "done")

    def test_cancelled_rebuild_leaves_active_project_unchanged(self):
        path = self.session()
        write_json(path / "project" / "meta.json", {"layers": [{"obs_id": "a"}]})
        write_json(path / "selection.json", {"obs_ids": ["a"]})
        write_array(path / "project" / "a.f16", np.ones((2, 2)), "<f2")
        original = (path / "project" / "a.f16").read_bytes()
        jobs = Jobs()
        def fake_build(session, staging, **kwargs):
            write_array(staging / "a.f16", np.zeros((2, 2)), "<f2")
            jobs.cancel()
            kwargs["progress"]("stars", .5, "Separating")
        with patch("astroimp.build._build", side_effect=fake_build):
            jobs.start("build", lambda progress: build(path, progress=progress))
            jobs.thread.join(5)
        self.assertEqual(jobs.payload()["state"], "cancelled")
        self.assertEqual((path / "project" / "a.f16").read_bytes(), original)
        self.assertFalse(list(path.glob(".build-*")))

    def test_cancelled_capture_removes_new_session(self):
        jobs = Jobs()
        view = AladinView((0, 0), (1, 1), (), (10, 10), (10, 10), ("RA", "DEC"), 4)
        snapshot = Snapshot(view, b"jpeg", {})
        def name(*args):
            jobs.cancel()
            return "Cancelled capture"
        root = self.root / "captures"
        with patch("astroimp.session.aladin.read_snapshot", return_value=snapshot), \
                patch("astroimp.surveys.available", return_value=[]), \
                patch("astroimp.session.catalog", return_value={"groups": []}), \
                patch("astroimp.session.auto_name", side_effect=name):
            jobs.start("capture", lambda progress: capture(root, progress))
            jobs.thread.join(5)
        self.assertEqual(jobs.payload()["state"], "cancelled")
        self.assertEqual(list(root.iterdir()), [])

    def test_cancelled_download_cleans_partial_but_keeps_cached_files(self):
        jobs = Jobs()
        cache = self.root / "downloads"
        cache.mkdir()
        (cache / "already.fits").write_bytes(b"cached")
        def chunks(*args):
            yield b"first"
            jobs.cancel()
            yield b"second"
        with patch("astroimp.build.requests.get") as get:
            response = get.return_value.__enter__.return_value
            response.headers = {}
            response.iter_content.side_effect = chunks
            jobs.start("build", lambda progress: download(
                {"data_uri": "mast:HST/new.fits", "collection": "HST"}, cache, progress))
            jobs.thread.join(5)
        self.assertEqual(jobs.payload()["state"], "cancelled")
        self.assertEqual((cache / "already.fits").read_bytes(), b"cached")
        self.assertFalse(list(cache.rglob("*.part")))
        self.assertFalse((cache / "HST" / "new.fits").exists())

    def test_thumbnail_http_does_not_wait_for_job_lock_and_cancel_endpoint(self):
        path = self.session()
        Image.new("RGB", (80, 40), "blue").save(path / "snapshot.jpg")
        app = create_app(self.root)
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        server = uvicorn.Server(uvicorn.Config(app, log_level="error"))
        thread = threading.Thread(target=lambda: server.run(sockets=[sock]), daemon=True)
        thread.start()
        def stop():
            server.should_exit = True
            thread.join(5)
            sock.close()
        self.addCleanup(stop)
        deadline = time.monotonic() + 10
        while not server.started and time.monotonic() < deadline:
            time.sleep(.02)
        self.assertTrue(server.started)
        base = f"http://127.0.0.1:{sock.getsockname()[1]}"
        with app.state.jobs.lock:
            self.assertEqual(requests.get(base + "/api/sessions/test/thumbnail.jpg", timeout=3).status_code, 200)
        release = threading.Event()
        self.addCleanup(release.set)
        def work(progress):
            release.wait(5)
            progress("search", .5, "Searching")
        app.state.jobs.start("capture", work)
        response = requests.post(base + "/api/job/cancel", timeout=3)
        self.assertEqual(response.status_code, 200)
        self.assertTrue(response.json()["cancel_requested"])
        release.set()
        app.state.jobs.thread.join(5)
        self.assertEqual(requests.get(base + "/api/job", timeout=3).json()["state"], "cancelled")


class SessionDeleteAndCaptureTests(unittest.TestCase):
    def setUp(self):
        availability = patch("astroimp.surveys.available", return_value=[])
        availability.start()
        self.addCleanup(availability.stop)
        self.root = Path(".test-delete-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        for identifier in ("a", "b"):
            write_json(self.root / identifier / "catalog.json", {"groups": []})
            write_json(self.root / identifier / "session.json", {"name": identifier, "created": "2020-01-01T00:00:00Z"})

    def test_delete_moves_to_trash_and_switches_current(self):
        sessions = Sessions(self.root)
        sessions.open("b")
        trashed = sessions.delete("b")
        self.assertTrue((trashed / "catalog.json").is_file())
        self.assertFalse((self.root / "b").exists())
        self.assertEqual(sessions.current.name, "a")
        self.assertEqual([s["id"] for s in sessions.list()], ["a"])
        for bad in ("..", ".trash", "missing"):
            with self.assertRaises((ValueError, FileNotFoundError)):
                sessions.delete(bad)

    def test_capture_writes_nan_catalog_values_as_null(self):
        view = AladinView(center_icrs_deg=(10., 20.), corners_icrs_deg=[], field_arcmin=(1., 1.),
                          pixel_dimensions=(10, 10), captured_at_utc="2020-01-01T00:00:00Z",
                          header_pixel_dimensions=(10, 10), projection="TAN", snapshot_bytes=1)
        with patch("astroimp.session.aladin.read_snapshot", return_value=Snapshot(view=view, header={}, jpeg=b"x")), \
                patch("astroimp.session.catalog", return_value={"groups": [{"observations": [
                    {"obs_id": "a", "t_exptime": float("nan")}]}]}), \
                patch("astroimp.surveys.available", return_value=[]), \
                patch("astroimp.session.auto_name", return_value="x"):
            path = capture(self.root)
        self.assertIn('"t_exptime": null', (path / "catalog.json").read_text())
        self.assertNotIn("stars", read_json(path / "session.json"))
        self.assertNotIn("stars", Sessions(self.root).payload(path))

    def test_large_view_captures_surveys_without_searching_mast(self):
        corners = [[9., 19.], [11., 19.], [11., 21.], [9., 21.]]
        view = AladinView(center_icrs_deg=(10., 20.), corners_icrs_deg=corners, field_arcmin=(120., 120.),
                          pixel_dimensions=(10, 10), captured_at_utc="2020-01-01T00:00:00Z",
                          header_pixel_dimensions=(10, 10), projection="TAN", snapshot_bytes=1)
        with patch("astroimp.session.aladin.read_snapshot", return_value=Snapshot(view=view, header={}, jpeg=b"x")), \
                patch("astroimp.session.catalog", return_value={"groups": []}) as search, \
                patch("astroimp.surveys.available", return_value=[]), \
                patch("astroimp.session.auto_name", return_value="x"):
            path = capture(self.root)
            search.assert_not_called()
            self.assertFalse(read_json(path / "catalog.json")["mast_searched"])


class RequestGuardTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-guard-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        for identifier in ("a", "b"):
            write_json(self.root / "sessions" / identifier / "catalog.json", {"groups": []})
        from fastapi.testclient import TestClient
        self.client = TestClient(create_app(self.root))
        self.client.post("/api/sessions/a/open")

    def test_rejects_foreign_host_and_cross_site_writes(self):
        self.assertEqual(self.client.get("/api/status", headers={"Host": "evil.example"}).status_code, 403)
        self.assertEqual(self.client.put("/api/edits", json={}, headers={"Origin": "https://evil.example"}).status_code, 403)
        self.assertEqual(self.client.put("/api/edits", json={}, headers={"Origin": "http://localhost:5173"}).status_code, 200)

    def test_edits_are_routed_to_the_named_session(self):
        self.assertEqual(self.client.put("/api/edits", json={"x": 1}, headers={"X-AstroImp-Session": "b"}).status_code, 200)
        self.assertFalse((self.root / "sessions" / "a" / "edits.json").exists())
        self.assertEqual(read_json(self.root / "sessions" / "b" / "edits.json"), {"x": 1})
        self.assertEqual(self.client.put("/api/edits", json={"x": 1}, headers={"X-AstroImp-Session": "a"}).status_code, 200)
        self.assertEqual(self.client.put("/api/markers", json=[], headers={"X-AstroImp-Session": "b"}).status_code, 200)


class RestartTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-restart-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        (self.root / "sessions").mkdir(parents=True)

    def test_restart_requires_supervisor_and_idle_jobs(self):
        from fastapi.testclient import TestClient
        self.assertEqual(TestClient(create_app(self.root)).post("/api/restart").status_code, 409)
        calls = []
        app = create_app(self.root, restart=lambda: calls.append(1))
        client = TestClient(app)
        self.assertTrue(client.get("/api/status").json()["restartable"])
        self.assertEqual(client.post("/api/restart").status_code, 200)
        self.assertEqual(calls, [1])
