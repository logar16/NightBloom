from __future__ import annotations

import io
import shutil
import socket
import threading
import time
import unittest
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import MagicMock, patch

import numpy as np
import requests
import uvicorn
from astropy.io import fits
from astropy.table import Table
from astropy.wcs import WCS

from astroimp.build import build_plan, download_all
from astroimp.mast import write_json
from astroimp.server import create_app
from astroimp.session import Jobs, Sessions, auto_name, read_json, simbad_name
from astroimp.aladin import clients


class RoundTwoTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-round2-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def http_app(self, app):
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
        while not server.started and thread.is_alive() and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertTrue(server.started)
        return f"http://127.0.0.1:{sock.getsockname()[1]}"

    def session_app(self):
        path = self.root / "sessions" / "test"
        write_json(path / "catalog.json", {"groups": [{"observations": [
            {"obs_id": "one", "region": "POLYGON ICRS 20 30 20.1 30 20 30.1"}]}]})
        write_json(path / "selection.json", {"obs_ids": ["one"]})
        write_json(path / "view.json", {"center_icrs_deg": [20, 30]})
        app = create_app(self.root)
        app.state.sessions.open("test")
        return path, app, self.http_app(app)

    def test_session_name_and_triage_persist_and_list_has_iso_date_and_count(self):
        path, _, base = self.session_app()
        self.assertEqual(requests.get(base + "/api/session", timeout=5).json()["triage"],
                         {"favorites": [], "dismissed": []})
        self.assertEqual(requests.patch(base + "/api/session", json={"name": "Eagle"},
                                        timeout=5).json(), {"name": "Eagle"})
        self.assertEqual(requests.patch(base + "/api/session", json={"name": "  "},
                                        timeout=5).status_code, 400)
        content = {"favorites": ["one", "one"], "dismissed": ["one"]}
        self.assertEqual(requests.put(base + "/api/triage", json=content, timeout=5).json(),
                         {"favorites": ["one"], "dismissed": ["one"]})
        reopened = Sessions(path.parent).payload()
        self.assertEqual(reopened["name"], "Eagle")
        self.assertEqual(reopened["triage"], {"favorites": ["one"], "dismissed": ["one"]})
        listed = requests.get(base + "/api/sessions", timeout=5).json()[0]
        self.assertEqual(listed["name"], "Eagle")
        self.assertEqual(listed["observations"], 1)
        self.assertIn("T", listed["created"])
        self.assertEqual(requests.put(base + "/api/triage", json={
            "favorites": ["unknown"], "dismissed": []}, timeout=5).status_code, 400)

    def test_fits_round_trips_orientation_and_wcs_and_aladin_uses_loopback_notify(self):
        path, _, base = self.session_app()
        wcs = WCS(naxis=2)
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        wcs.wcs.crval = [20, 30]
        wcs.wcs.crpix = [2, 2]
        wcs.wcs.cdelt = [-.001, .001]
        image = np.arange(12, dtype="<f2").reshape(3, 4)
        project = path / "project"
        project.mkdir()
        image.tofile(project / "one.f16")
        write_json(project / "meta.json", {
            "width": 4, "height": 3, "wcs": dict(wcs.to_header()),
            "layers": [{"key": "one", "file": "one.f16", "filter": "F502N"}]})
        response = requests.get(base + "/api/project/fits/one.fits", timeout=5)
        self.assertEqual(response.status_code, 200)
        with fits.open(io.BytesIO(response.content)) as hdus:
            np.testing.assert_array_equal(hdus[0].data, image[::-1].astype(np.float32))
            self.assertEqual(hdus[0].header["BITPIX"], -32)
            np.testing.assert_allclose(WCS(hdus[0].header).pixel_to_world_values([0, 3], [0, 2]),
                                       wcs.pixel_to_world_values([0, 3], [0, 2]))
        with patch("astroimp.aladin_fits.fits.PrimaryHDU", side_effect=AssertionError("cache miss")):
            repeated = requests.get(base + "/api/project/fits/one.fits", timeout=5)
        self.assertEqual(repeated.content, response.content)
        # Both pixel replacement and WCS-only changes invalidate the cached FITS.
        (image + 1).tofile(project / "one.f16")
        replaced = requests.get(base + "/api/project/fits/one.fits", timeout=5)
        with fits.open(io.BytesIO(replaced.content)) as hdus:
            np.testing.assert_array_equal(hdus[0].data, (image + 1)[::-1].astype(np.float32))
        meta = read_json(project / "meta.json")
        meta["wcs"]["CRVAL1"] = 21
        write_json(project / "meta.json", meta)
        moved = requests.get(base + "/api/project/fits/one.fits", timeout=5)
        with fits.open(io.BytesIO(moved.content)) as hdus:
            self.assertEqual(hdus[0].header["CRVAL1"], 21)
        self.assertEqual(requests.get(base + "/api/project/fits/private.fits",
                                     timeout=5).status_code, 404)
        client = MagicMock()
        client.get_subscribed_clients.return_value = {"aladin": {}}
        client.get_metadata.return_value = {"samp.name": "Aladin"}
        with patch("astroimp.server.SAMPIntegratedClient", return_value=client):
            result = requests.post(base + "/api/aladin/layers", json={"keys": ["one"]},
                                   timeout=5)
            self.assertEqual(result.status_code, 200)
            message = client.notify.call_args.args[1]
            self.assertEqual(message["samp.params"]["url"],
                             base + "/api/project/fits/one.fits?session=test")
            self.assertEqual(message["samp.params"]["name"], "NightBloom F502N")
            self.assertEqual(message["samp.mtype"], "image.load.fits")
            result = requests.post(base + "/api/aladin/footprints",
                                   json={"obs_ids": ["one"]}, timeout=5)
            self.assertEqual(result.json()["polygons"], 1)
            script = client.notify.call_args.args[1]["samp.params"]["script"].split(";")
            self.assertIn("rm AstroImp", script)
            self.assertIn("select NightBloom", script)

    def test_status_never_waits_for_a_slow_ping_and_caches_it(self):
        _, _, base = self.session_app()
        release = threading.Event()
        entered = threading.Event()
        self.addCleanup(release.set)

        def slow_ping():
            entered.set()
            release.wait(5)
            return True
        with patch("astroimp.server.aladin.ping", side_effect=slow_ping) as ping:
            started = time.monotonic()
            for _ in range(4):
                self.assertEqual(requests.get(base + "/api/status", timeout=1).status_code, 200)
            self.assertLess(time.monotonic() - started, 1)
            self.assertTrue(entered.wait(1))
            self.assertEqual(ping.call_count, 1)
            release.set()
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline:
                if requests.get(base + "/api/status", timeout=1).json()["aladin"]:
                    break
                time.sleep(.01)
            self.assertTrue(requests.get(base + "/api/status", timeout=1).json()["aladin"])
            self.assertEqual(ping.call_count, 1)

    def test_stale_aladin_registration_is_ignored_without_executing_scripts(self):
        client = MagicMock()
        client.get_subscribed_clients.return_value = {"active": {}, "stale": {}}
        client.get_metadata.return_value = {"samp.name": "Aladin"}
        client.call_and_wait.side_effect = [{"samp.status": "samp.ok"},
                                            RuntimeError("Connection refused")]
        self.assertEqual(clients(client, "script.aladin.send"), ["active"])
        for call in client.call_and_wait.call_args_list:
            self.assertEqual(call.args[1]["samp.mtype"], "samp.app.ping")
        client.notify.assert_not_called()

    def test_simbad_chooses_nearest_object_not_cluster_member_and_uses_common_name(self):
        table = Table({
            "main_id": ["NGC 6611 323", "M 16", "NGC 123"],
            "ra": [20.0001, 20.01, 20.02], "dec": [30, 30, 30],
            "ids": ["NGC 6611 323|2MASS one",
                    "M  16|NGC  6611|NAME Eagle Nebula|NAME Star Queen", "NGC 123"]})
        query = MagicMock()
        query.query_region.return_value = table
        view = {"center_icrs_deg": [20, 30], "field_arcmin": [100, 100]}
        with patch("astroimp.session.Simbad", return_value=query):
            self.assertEqual(simbad_name(view), "Eagle Nebula (M16)")
        self.assertEqual(query.query_region.call_args.kwargs["radius"].value, 30)
        content = {"groups": [{"observations": [
            {"obs_id": "1", "target": "M16"}, {"obs_id": "2", "target": "M16"},
            {"obs_id": "3", "target": "other"}]}]}
        with patch("astroimp.session.simbad_name", side_effect=RuntimeError("offline")):
            self.assertEqual(auto_name(view, content), "M16")
            self.assertEqual(auto_name({"center_icrs_deg": [274.7, -13.8]}, {"groups": []}),
                             "RA 274.70 Dec −13.80")

    def test_parallel_download_plan_progress_and_atomic_cache_with_fake_http_source(self):
        wcs = WCS(naxis=2)
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        wcs.wcs.crval = [20, 30]
        wcs.wcs.crpix = [1, 1]
        wcs.wcs.cdelt = [-.001, .001]
        output = io.BytesIO()
        fits.PrimaryHDU(np.ones((512, 512), np.float32), header=wcs.to_header()).writeto(output)
        content = output.getvalue()
        state = {"active": 0, "maximum": 0, "head": 0}
        lock = threading.Lock()
        barrier = threading.Barrier(4)

        class Source(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_HEAD(self):
                with lock:
                    state["head"] += 1
                self.send_response(200)
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()

            def do_GET(self):
                with lock:
                    state["active"] += 1
                    state["maximum"] = max(state["maximum"], state["active"])
                try:
                    barrier.wait(5)
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(content)))
                    self.end_headers()
                    for start in range(0, len(content), 64 * 1024):
                        self.wfile.write(content[start:start + 64 * 1024])
                        self.wfile.flush()
                        time.sleep(.005)
                finally:
                    with lock:
                        state["active"] -= 1

        source = ThreadingHTTPServer(("127.0.0.1", 0), Source)
        thread = threading.Thread(target=source.serve_forever, daemon=True)
        thread.start()

        def stop():
            source.shutdown()
            source.server_close()
            thread.join(5)
        self.addCleanup(stop)
        base = f"http://127.0.0.1:{source.server_port}/"
        cache = self.root / "downloads"
        cache.mkdir()
        (cache / "cached.fits").write_bytes(content)
        chosen = [{"obs_id": str(i), "filter": f"F{i}", "collection": "HST",
                   "data_uri": f"mast:HST/product/{name}-{i}.fits"}
                  for i, name in enumerate(["one", "two", "three", "four"])]
        chosen.insert(0, {"obs_id": "cache", "filter": "F0", "collection": "HST",
                          "data_uri": "mast:HST/product/cached.fits"})
        real_get, real_head = requests.get, requests.head
        events = []
        jobs = Jobs()

        def work(progress):
            def record(**values):
                if "files" in values:
                    events.append(values)
                progress.update_original(**values)
            progress.update_original = progress.update
            progress.update = record
            paths = download_all(chosen, cache, progress)
            self.assertEqual(len(paths), 5)

        with patch("astroimp.build.requests.get", side_effect=lambda *a, **kw:
                   real_get(base, stream=True, timeout=5)), \
                patch("astroimp.build.requests.head", side_effect=lambda *a, **kw:
                      real_head(base, timeout=5)):
            plan = build_plan(chosen, cache)
            self.assertEqual(plan["total_bytes"], len(content) * 5)
            self.assertEqual(plan["cached_bytes"], len(content))
            self.assertTrue(plan["files"][0]["cached"])
            self.assertEqual(build_plan(chosen, cache), plan)
            self.assertEqual(state["head"], 4)
            jobs.start("build", work)
            jobs.thread.join(15)
        self.assertFalse(jobs.thread.is_alive())
        self.assertEqual(jobs.payload()["state"], "done", jobs.payload()["error"])
        self.assertEqual(state["maximum"], 4)
        self.assertEqual(events[0]["files"][0]["state"], "cached")
        self.assertEqual(events[0]["files"][0]["done_bytes"], len(content))
        self.assertTrue(any(f["state"] == "queued" for f in events[0]["files"]))
        self.assertTrue(any(f["state"] == "downloading" for e in events for f in e["files"]))
        self.assertTrue(any(e["eta_s"] is not None for e in events))
        for row in jobs.payload()["files"]:
            self.assertIn(row["state"], {"done", "cached"})
            self.assertEqual(row["done_bytes"], len(content))
        self.assertFalse(list(cache.rglob("*.part")))

    def test_failed_download_has_file_error_and_removes_partial_bytes(self):
        response = MagicMock()
        response.__enter__.return_value = response
        response.headers = {"Content-Length": "100"}
        response.iter_content.return_value = [b"short"]
        chosen = [{"obs_id": "bad", "filter": "F502N", "collection": "HST",
                   "data_uri": "mast:HST/product/incomplete.fits", "size_bytes": 100}]
        jobs = Jobs()
        with patch("astroimp.build.requests.get", return_value=response):
            jobs.start("build", lambda progress: download_all(chosen, self.root, progress))
            jobs.thread.join(5)
        payload = jobs.payload()
        self.assertEqual(payload["phase"], "error")
        self.assertEqual(payload["files"][0]["state"], "error")
        self.assertEqual(payload["files"][0]["done_bytes"], 5)
        self.assertIn("Incomplete download", payload["files"][0]["error"])
        self.assertFalse(list(self.root.rglob("*.part")))
        self.assertFalse(list(self.root.rglob("*.fits")))


if __name__ == "__main__":
    unittest.main()
