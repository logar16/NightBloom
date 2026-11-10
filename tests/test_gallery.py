import io
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np
import tifffile
from fastapi.testclient import TestClient
from PIL import Image

from astroimp.mast import write_json
from astroimp.server import create_app


class GalleryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = self.enterContext(tempfile.TemporaryDirectory())
        self.root = Path(self.temporary)
        for identifier in ("alpha", "beta", "empty"):
            project = self.root / "sessions" / identifier
            write_json(project / "catalog.json", {"groups": []})
            write_json(project / "session.json", {"name": identifier.title()})
            write_json(project / "edits.json", {"keep": identifier})
            (project / "exports").mkdir()
        self.exports = self.root / "sessions" / "alpha" / "exports"
        for name, size, stamp in (("large.png", (80, 60), 100), ("latest.png", (80, 60), 200),
                                  ("preview.jpg", (40, 30), 400)):
            path = self.exports / name
            Image.new("RGB", size, (0, 128, 255)).save(path)
            os.utime(path, (stamp, stamp))
        (self.exports / "old").mkdir()
        self.tiff = self.exports / "old" / "study α.tiff"
        tifffile.imwrite(self.tiff, np.tile(np.array([0, 32768, 65535], np.uint16), (48, 64, 1)),
                         photometric="rgb")
        (self.exports / "bad.png").write_bytes(b"unfinished image")
        write_json(self.exports / "sidecar.json", {"width": 999999})
        Image.new("RGB", (10, 10)).save(self.root / "sessions" / "beta" / "exports" / "other.png")
        self.app = create_app(self.root)
        self.app.state.sessions.open("beta")
        self.client = TestClient(self.app)

    def test_inventory_groups_actual_images_hires_first_without_opening_or_reading_project_assets(self):
        sessions = self.root / "sessions"
        before = {path.relative_to(sessions): path.read_bytes() for path in sessions.rglob("*") if path.is_file()}
        with patch("astroimp.session.Sessions.list", side_effect=AssertionError("No project summaries")), \
                patch("astroimp.render.Project", side_effect=AssertionError("No science render")):
            response = self.client.get("/api/gallery")
        self.assertEqual(response.status_code, 200, response.text)
        albums = {album["id"]: album for album in response.json()}
        self.assertEqual(set(albums), {"alpha", "beta"})
        images = albums["alpha"]["images"]
        self.assertEqual([image["filename"] for image in images],
                         ["latest.png", "large.png", "old/study α.tiff", "preview.jpg", "bad.png"])
        self.assertEqual((images[2]["width"], images[2]["height"]), (64, 48))
        self.assertTrue(images[-1]["error"])
        self.assertEqual(self.app.state.sessions.current.name, "beta")
        self.assertEqual(before, {path.relative_to(sessions): path.read_bytes()
                                  for path in sessions.rglob("*") if path.is_file()})

    def test_original_is_byte_exact_clipboard_is_full_size_png_and_previews_refresh_safely(self):
        original = self.tiff.read_bytes()
        base = "/api/gallery/alpha/old/study%20%CE%B1.tiff"
        saved = self.client.get(base + "?view=original")
        self.assertEqual(saved.content, original)
        self.assertIn("attachment", saved.headers["content-disposition"])
        for view in ("thumbnail", "preview", "clipboard"):
            response = self.client.get(base + f"?view={view}")
            self.assertEqual(response.status_code, 200, response.text[:100])
            with Image.open(io.BytesIO(response.content)) as image:
                self.assertEqual(image.size, (64, 48))
                self.assertEqual(image.format, "PNG" if view == "clipboard" else "JPEG")
        self.assertEqual(self.tiff.read_bytes(), original)
        url = "/api/gallery/alpha/large.png?view=preview"
        first = self.client.get(url).content
        Image.new("RGB", (80, 60), "red").save(self.exports / "large.png")
        os.utime(self.exports / "large.png", (500, 500))
        self.assertNotEqual(self.client.get(url).content, first)
        for path in ("/api/gallery/alpha/%2e%2e%2fcatalog.json",
                     "/api/gallery/alpha/sidecar.json", "/api/gallery/alpha/C%3Aother.png",
                     "/api/gallery/missing/large.png"):
            self.assertEqual(self.client.get(path).status_code, 404, path)
        self.assertEqual(self.client.get("/api/gallery/alpha/bad.png?view=preview").status_code, 400)
        self.assertEqual(self.app.state.sessions.current.name, "beta")

    @unittest.skipUnless(os.name == "nt", "Windows junction contract")
    def test_inventory_does_not_follow_export_directory_junctions(self):
        outside = self.root / "outside"
        outside.mkdir()
        Image.new("RGB", (20, 20)).save(outside / "not-an-export.png")
        junction = self.exports / "linked"
        quoted = lambda path: str(path).replace("'", "''")
        subprocess.run(
            ["powershell", "-NoProfile", "-NonInteractive", "-Command",
             f"New-Item -ItemType Junction -Path '{quoted(junction)}' -Target '{quoted(outside)}' | Out-Null"],
            check=True, capture_output=True, creationflags=subprocess.CREATE_NO_WINDOW,
        )
        response = self.client.get("/api/gallery")
        self.assertEqual(response.status_code, 200, response.text)
        alpha = next(album for album in response.json() if album["id"] == "alpha")
        self.assertNotIn("linked/not-an-export.png", [image["filename"] for image in alpha["images"]])
