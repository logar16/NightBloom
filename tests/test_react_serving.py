from __future__ import annotations

import os
import shutil
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

from fastapi.testclient import TestClient

from astroimp.frontend import build_frontend, needs_build
from astroimp.server import create_app


class ReactServingTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-react-" + uuid.uuid4().hex)
        self.dist = self.root / "dist"
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def test_react_root_serves_dist_and_assets(self):
        self.dist.mkdir()
        (self.dist / "index.html").write_text("react document")
        (self.dist / "assets").mkdir()
        (self.dist / "assets" / "studio.js").write_text("react module")
        with TestClient(create_app(self.root / "data", frontend=self.dist)) as client:
            self.assertEqual(client.get("/").text, "react document")
            self.assertEqual(client.get("/assets/studio.js").text, "react module")
            self.assertEqual(client.get("/api/sessions").json(), [])

    def test_staleness_covers_sources_and_ignores_generated_files(self):
        paths = ["src\\main.tsx", "src\\editor\\state.ts", "index.html", "package.json",
                 "vite.config.ts", "vite.config.mjs", "dist\\assets\\studio.js",
                 "node_modules\\package\\index.js"]
        for name in paths:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture")
            os.utime(path, ns=(1_000_000_000, 1_000_000_000))
        self.assertTrue(needs_build(self.root))
        index = self.dist / "index.html"
        index.write_text("react")
        os.utime(index, ns=(2_000_000_000, 2_000_000_000))
        self.assertFalse(needs_build(self.root))
        for name in paths:
            with self.subTest(source=name):
                path = self.root / name
                os.utime(path, ns=(3_000_000_000, 3_000_000_000))
                self.assertEqual(needs_build(self.root),
                                 name.startswith("src\\") or name in (
                                     "index.html", "package.json", "vite.config.ts", "vite.config.mjs"))
                os.utime(path, ns=(1_000_000_000, 1_000_000_000))

    def test_fresh_build_does_not_require_npm(self):
        self.dist.mkdir()
        (self.dist / "index.html").write_text("react")
        with patch("astroimp.frontend.shutil.which") as which, \
                patch("astroimp.frontend.subprocess.run") as npm:
            build_frontend(self.root)
            which.assert_not_called()
            npm.assert_not_called()
