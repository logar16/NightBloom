import shutil
import threading
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

from fastapi.testclient import TestClient

from astroimp.mast import write_json
from astroimp.server import create_app
from astroimp.session import BusyError, Jobs, read_json


class MultiProjectTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-multi-" + uuid.uuid4().hex)
        for identifier, name in (("orion", "Orion"), ("carina", "Carina")):
            path = self.root / "sessions" / identifier
            write_json(path / "catalog.json", {"groups": []})
            write_json(path / "session.json", {"name": name})
            write_json(path / "selection.json", {"obs_ids": []})
        self.app = create_app(self.root)
        self.app.state.sessions.open("orion")
        self.client = TestClient(self.app)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        ping = patch("astroimp.server.aladin.ping", return_value=False)
        ping.start()
        self.addCleanup(ping.stop)

    def test_project_resolution_header_query_and_last_opened_fallback(self):
        headers = {"X-AstroImp-Session": "carina"}
        self.assertEqual(self.client.get("/api/session").json()["id"], "orion")
        self.assertEqual(self.client.get("/api/session", headers=headers).json()["id"], "carina")
        self.assertEqual(self.client.get("/api/session?session=carina").json()["id"], "carina")
        self.client.put("/api/edits", headers=headers, json={"project": "carina"})
        self.client.put("/api/markers?session=carina", json=[{"n": 1}])
        self.assertEqual(read_json(self.root / "sessions" / "carina" / "edits.json"), {"project": "carina"})
        self.assertFalse((self.root / "sessions" / "orion" / "edits.json").exists())
        self.assertEqual(self.app.state.sessions.current.name, "orion")
        self.client.post("/api/sessions/carina/open")
        self.assertEqual(self.client.get("/api/session").json()["id"], "carina")
        for bad in ("missing", "..", ".trash", "../orion", r"..\orion", "C:orion"):
            self.assertEqual(self.client.get("/api/session", headers={"X-AstroImp-Session": bad}).status_code, 404)

    def test_running_project_blocks_only_its_writes_and_deletion(self):
        release = threading.Event()
        self.addCleanup(release.set)
        jobs = self.app.state.jobs
        job = jobs.start("build", lambda progress: release.wait(5), session="orion", name="Orion")
        try:
            for endpoint, body in (("/api/edits", {}), ("/api/markers", []), ("/api/selection", {"obs_ids": []})):
                self.assertEqual(self.client.put(endpoint, json=body).status_code, 409)
                self.assertEqual(self.client.put(endpoint, json=body, headers={"X-AstroImp-Session": "carina"}).status_code, 200)
            self.assertEqual(self.client.post("/api/sessions/carina/open").status_code, 200)
            self.assertEqual(self.client.post("/api/sessions/orion/open").status_code, 200)
            self.assertEqual(self.client.delete("/api/sessions/orion").status_code, 409)
            self.assertEqual(self.client.delete("/api/sessions/carina").status_code, 200)
            status = self.client.get("/api/status", headers={"X-AstroImp-Session": "orion"}).json()
            self.assertEqual(status["job"]["id"], job["id"])
            self.assertEqual(status["jobs"][0]["name"], "Orion")
        finally:
            release.set()
            jobs.threads[job["id"]].join(5)

    def test_network_jobs_overlap_heavy_stages_are_fifo_and_cancel_is_scoped(self):
        jobs = Jobs()
        downloads = {name: threading.Event() for name in ("orion", "carina", "third")}
        proceed = {name: threading.Event() for name in downloads}
        finish = threading.Event()
        order = []

        def work(name):
            def run(progress):
                downloads[name].set()
                proceed[name].wait(5)
                progress.heavy("align")
                order.append(name)
                progress("reproject", .4, "Aligning")
                if name == "orion":
                    finish.wait(5)
            return run

        self.addCleanup(finish.set)
        for event in proceed.values():
            self.addCleanup(event.set)
        entries = {name: jobs.start("build", work(name), session=name, name=name.title()) for name in downloads}
        try:
            self.assertTrue(all(event.wait(3) for event in downloads.values()))
            with self.assertRaises(BusyError):
                jobs.start("build", lambda _: None, session="orion")
            proceed["orion"].set()
            self.wait_for(lambda: jobs.heavy_owner is not None)
            proceed["carina"].set()
            self.wait_for(lambda: jobs.payload("carina")["state"] == "queued")
            proceed["third"].set()
            self.wait_for(lambda: jobs.payload("third")["state"] == "queued")
            queued = jobs.payload("carina")
            self.assertEqual(queued["message"], "Waiting for Orion")
            self.assertEqual(queued["waiting_for"]["progress"], .4)
            # Cancellation must target its ID, not whichever job started most recently.
            jobs.cancel(entries["third"]["id"])
            jobs.threads[entries["third"]["id"]].join(3)
            self.assertEqual(jobs.payload("third")["state"], "cancelled")
            self.assertFalse(jobs.payload("orion")["cancel_requested"])
            self.assertEqual(order, ["orion"])
            finish.set()
            for entry in entries.values():
                jobs.threads[entry["id"]].join(3)
            self.assertEqual(order, ["orion", "carina"])
            self.assertIsNone(jobs.heavy_owner)
            self.assertEqual(jobs.payload("carina")["state"], "done")
        finally:
            finish.set()
            for event in proceed.values():
                event.set()
            for entry in entries.values():
                jobs.threads[entry["id"]].join(5)

    def test_cancel_endpoint_targets_a_job_from_another_project(self):
        release = threading.Event()
        self.addCleanup(release.set)
        jobs = self.app.state.jobs
        a = jobs.start("build", lambda progress: release.wait(5), session="orion", name="Orion")
        b = jobs.start("canvas", lambda progress: release.wait(5), session="carina", name="Carina")
        try:
            response = self.client.post("/api/job/cancel?id=" + a["id"])
            self.assertTrue(response.json()["cancel_requested"])
            self.assertFalse(jobs.payload("carina")["cancel_requested"])
        finally:
            release.set()
            jobs.threads[a["id"]].join(5)
            jobs.threads[b["id"]].join(5)
        self.assertEqual(jobs.payload("orion")["state"], "cancelled")
        self.assertEqual(jobs.payload("carina")["state"], "done")

    def test_capture_reserves_a_project_before_network_work_finishes(self):
        entered, release = threading.Event(), threading.Event()
        self.addCleanup(release.set)

        def fake_capture(root, progress, *, destination):
            entered.set()
            release.wait(5)
            write_json(destination / "session.json", {"name": "New target"})

        with patch("astroimp.server.capture", side_effect=fake_capture):
            response = self.client.post("/api/capture")
            self.assertEqual(response.status_code, 200)
            job = response.json()
            self.assertTrue(entered.wait(3))
            try:
                path = self.app.state.sessions.resolve(job["session"])
                self.assertNotEqual(path.name, "orion")
                self.assertEqual(self.app.state.sessions.current, path)
                self.assertEqual(self.client.put("/api/edits", json={}, headers={
                    "X-AstroImp-Session": "orion"}).status_code, 200)
            finally:
                release.set()
                self.app.state.jobs.threads[job["id"]].join(5)
            self.assertEqual(self.app.state.jobs.payload(path.name)["name"], "New target")

    def test_cli_session_option_becomes_an_explicit_project_header(self):
        from astroimp.editcli import _call
        with patch("astroimp.editcli.requests.request") as request:
            request.return_value.ok = True
            request.return_value.json.return_value = {}
            _call("http://127.0.0.1:8765", "GET", "/api/session", session="carina")
            self.assertEqual(request.call_args.kwargs["headers"], {"X-AstroImp-Session": "carina"})
            _call("http://127.0.0.1:8765", "GET", "/api/session")
            self.assertNotIn("headers", request.call_args.kwargs)

    def wait_for(self, condition):
        deadline = time.monotonic() + 3
        while not condition() and time.monotonic() < deadline:
            time.sleep(.005)
        self.assertTrue(condition())
