import shutil
import threading
import unittest
import uuid
from pathlib import Path

from astroimp.mast import write_json
from astroimp.server import StorageDelete, create_app
from astroimp.storage import Storage
from fastapi import HTTPException


class StorageTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-storage-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))
        self.storage = Storage(self.root)

    def download(self, name, content=b"12345"):
        path = self.root / "downloads" / "old-target" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return f"old-target/{name}"

    def session(self, name, observations=(), selected=(), layers=(), archive=False):
        path = self.root / name
        write_json(path / "session.json", {"name": name.split("/")[-1]})
        write_json(path / "catalog.json", {"groups": [{"observations": [
            {"obs_id": obs, "data_uri": f"mast:TEST/{obs}.fits", "filter": "F555W"}
            for obs in observations]}]})
        write_json(path / "selection.json", {"obs_ids": list(selected)})
        project = path / "builds" / "before" / "project" if archive else path / "project"
        write_json(project / "meta.json", {"layers": list(layers)})
        return path

    def test_classification_selection_meta_archives_fixtures_and_duplicates(self):
        for name in ("selection", "meta", "archive", "fixture", "deleted", "backup", "orphan", "shared"):
            self.download(name + ".fits")
        duplicate = self.root / "downloads" / "duplicate" / "selection.fits"
        duplicate.parent.mkdir()
        duplicate.write_bytes(b"x")
        self.session("sessions/live", ["selection", "shared"], ["selection", "shared"], [
            {"obs_id": "old", "source": r"C:\old-cache\meta.fits"}])
        self.session("sessions/archived", layers=[{
            "source_signature": ["/old/cache/archive.fits", 5, 123]}], archive=True)
        self.session("test-fixtures/sessions/frozen", ["fixture"], ["fixture"])
        self.session("sessions/.trash/gone-20261024T200000Z", ["deleted", "shared"], ["deleted", "shared"])
        self.session("sessions/_backup_saved", ["backup"], ["backup"])
        inventory = self.storage.inventory()
        entries = {item["name"]: item for item in inventory["downloads"]}
        self.assertEqual({name: entry["status"] for name, entry in entries.items()}, {
            "selection.fits": "in_use", "meta.fits": "in_use", "archive.fits": "in_use",
            "fixture.fits": "in_use", "deleted.fits": "trash_only",
            "backup.fits": "trash_only", "orphan.fits": "orphan", "shared.fits": "in_use"})
        self.assertEqual(len(entries["shared.fits"]["references"]["live"]), 1)
        self.assertEqual(len(entries["shared.fits"]["references"]["trash"]), 1)
        self.assertEqual(entries["selection.fits"]["filter"], "F555W")
        self.assertEqual(inventory["trash"][0]["date"], "2026-10-24T20:00:00+00:00")
        self.assertEqual(inventory["totals"]["downloads"], 41)

    def test_delete_revalidates_cached_inventory_and_refuses_protected_batch(self):
        protected = self.download("protected.fits")
        orphan = self.download("orphan.fits")
        self.storage.inventory()
        self.session("sessions/new", ["protected"], ["protected"])
        with self.assertRaisesRegex(ValueError, "live project"):
            self.storage.delete(downloads=[orphan, protected])
        self.assertTrue((self.root / "downloads" / orphan).exists())
        fixture = self.download("fixture.fits")
        self.session("test-fixtures/sessions/frozen", layers=[{"source": "fixture.fits"}])
        with self.assertRaisesRegex(ValueError, "test fixture"):
            self.storage.delete(downloads=[fixture])

    def test_delete_frees_exact_bytes_and_trash_deletion_reclassifies_downloads(self):
        deleted = self.download("deleted.fits")
        orphan = self.download("orphan.fits", b"123")
        folder = self.session("sessions/.trash/gone", ["deleted"], ["deleted"])
        before = self.storage.inventory()
        result = self.storage.delete(trash=["gone"])
        self.assertEqual(result["freed_bytes"], before["trash"][0]["size"])
        self.assertFalse(folder.exists())
        self.assertEqual(self.storage.inventory()["downloads"][0]["status"], "orphan")
        self.assertEqual(self.storage.delete(downloads=[deleted, orphan])["freed_bytes"], 8)

    def test_delete_trash_only_download_and_explicit_backup_and_caches(self):
        deleted = self.download("deleted.fits")
        self.session("sessions/.trash/gone", ["deleted"], ["deleted"])
        backup = self.session("sessions/_backup_saved")
        for cache in ("aligned_cache", "cache"):
            (self.root / cache).mkdir()
            (self.root / cache / "entry").write_bytes(b"12")
        expected = self.storage.inventory()["totals"]["backups"] + 9
        result = self.storage.delete(downloads=[deleted], backups=[backup.name], caches=["aligned", "previews"])
        self.assertEqual(result["freed_bytes"], expected)
        self.assertTrue((self.root / "sessions" / ".trash" / "gone").exists())

    def test_path_traversal_and_live_session_targets_are_refused(self):
        live = self.session("sessions/live")
        self.download("orphan.fits")
        for name in ("../sessions/live/catalog.json", r"..\sessions\live\catalog.json",
                     r"C:\outside.fits", "/outside.fits", "old-target/orphan.fits:stream"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.storage.delete(downloads=[name])
        with self.assertRaises(ValueError):
            self.storage.delete(backups=["live"])
        with self.assertRaises(ValueError):
            self.storage.delete(trash=["../live"])
        self.assertTrue(live.exists())

    def test_unreadable_manifest_fails_closed(self):
        self.download("orphan.fits")
        folder = self.session("sessions/live")
        (folder / "selection.json").write_text("{broken")
        with self.assertRaises(ValueError):
            self.storage.delete(downloads=["old-target/orphan.fits"])

    def test_api_refuses_cleanup_during_job(self):
        app = create_app(self.root)
        finished = threading.Event()
        self.addCleanup(finished.set)
        app.state.jobs.start("test", lambda progress: finished.wait(10))
        endpoint = next(route.endpoint for route in app.routes if route.path == "/api/storage/delete")
        with self.assertRaises(HTTPException) as error:
            endpoint(StorageDelete())
        self.assertEqual(error.exception.status_code, 409)
        finished.set()
        app.state.jobs.thread.join(5)


if __name__ == "__main__":
    unittest.main()
