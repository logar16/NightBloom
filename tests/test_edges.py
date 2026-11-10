from __future__ import annotations

import shutil
import socket
import threading
import time
import unittest
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

import numpy as np
import requests
import uvicorn

from astroimp.build import edge_distance, ensure_edge_map
from astroimp.mast import write_json
from astroimp.render import Project, edge_fade, edge_noise, probe, render, warp
from astroimp.server import create_app


class EdgeTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-edges-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def project(self):
        folder = self.root / "sessions" / "test" / "project"
        folder.mkdir(parents=True)
        h, w = 32, 48
        covered = np.ones((h, w), bool)
        covered[:16, 24:] = False
        covered[16:, 32:] = False
        pixels = covered.astype("<f2") * .75
        pixels.tofile(folder / "a.f16")
        pixels.tofile(folder / "a.starless.f16")
        covered.astype("u1").tofile(folder / "a.coverage.u8")
        np.zeros((h, w), "<u2").tofile(folder / "stars.u16")
        write_json(folder / "stars.json", [])
        meta = {"width": w, "height": h, "layers": [{
            "key": "a", "filter": "F502N", "file": "a.f16", "starless": "a.starless.f16",
            "coverage_file": "a.coverage.u8",
        }], "stars": {"file": "stars.u16", "list": "stars.json"}}
        write_json(folder / "meta.json", meta)
        layer = {
            "id": "one", "name": "One", "source": "a", "visible": True, "mode": "tint",
            "color": "#ffffff", "black": 0, "white": 1, "stretch": "linear", "k": 10,
            "intensity": 1, "opacity": 1, "blend": "add", "edge": {"feather": 8, "irregularity": 0},
        }
        state = {"layers": [layer], "global": {"stars": {"fg": 1, "bg": 1}}, "starClasses": {}}
        return folder, meta, covered, state

    def test_distance_ignores_canvas_border_and_measures_interior_steps(self):
        np.testing.assert_array_equal(edge_distance(np.ones((7, 9), bool)), 1024)
        covered = np.ones((7, 9), bool)
        covered[:3, 6:] = False
        covered[3:, 8:] = False
        dist = edge_distance(covered)
        self.assertEqual(dist[0, 0], 6)
        self.assertEqual(dist[0, 5], 1)
        self.assertEqual(dist[4, 7], 1)
        self.assertEqual(dist[3, 6], 1)
        self.assertAlmostEqual(float(dist[4, 6]), 2, places=3)
        self.assertEqual(dist[0, 6], 0)
        self.assertEqual(dist.dtype, np.dtype("<f2"))
        long_strip = np.ones((1, 1200), bool)
        long_strip[0, -1] = False
        self.assertEqual(edge_distance(long_strip)[0, 0], 1024)

    def test_lazy_http_edge_maps_are_allowlisted_cached_and_thread_safe(self):
        folder, meta, _, _ = self.project()
        app = create_app(self.root)
        app.state.sessions.current = folder.parent
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
            time.sleep(.01)
        self.assertTrue(server.started)
        base = f"http://127.0.0.1:{sock.getsockname()[1]}"
        with patch("astroimp.build.edge_distance", wraps=edge_distance) as calculate:
            with ThreadPoolExecutor(max_workers=6) as pool:
                responses = list(pool.map(lambda _: requests.get(base + "/api/project/a.edge.f16", timeout=10), range(6)))
            self.assertTrue(all(r.status_code == 200 for r in responses))
            self.assertEqual(calculate.call_count, 1)
            self.assertEqual(len(responses[0].content), meta["width"] * meta["height"] * 2)
            self.assertTrue(all(r.content == responses[0].content for r in responses))
        self.assertEqual(requests.get(base + "/api/project/unknown.edge.f16", timeout=5).status_code, 404)
        self.assertFalse(list(folder.glob("*.part")))

    def test_fade_matches_smoothstep_and_window_probes_use_global_noise_coordinates(self):
        folder, meta, _, state = self.project()
        project = Project(folder)
        fade = edge_fade(project, state["layers"][0])
        # Pixel (23, 5) is one image pixel from the inside-grid data edge.
        t = 1 / 8
        self.assertAlmostEqual(float(fade[5, 23]), t * t * (3 - 2 * t), places=6)
        self.assertEqual(fade[0, 0], 1)
        acc, _ = render(project, state)
        self.assertAlmostEqual(float(acc[5, 23, 0]), .75 * fade[5, 23], places=6)
        state["layers"][0]["edge"]["irregularity"] = 1
        irregular = edge_fade(project, state["layers"][0])
        self.assertGreater(float(np.abs(irregular - fade).max()), .03)
        window = (12, 4, 30, 14)
        cut = Project(folder, window=window)
        np.testing.assert_allclose(edge_fade(cut, state["layers"][0]), irregular[4:14, 12:30], atol=1e-6)
        report = probe(folder, state, {"x": 23, "y": 5, "radius": 0})
        self.assertAlmostEqual(report["final_rgb"][0], .75 * irregular[5, 23], places=3)
        state["layers"][0]["edge"]["feather"] = 0
        unfaded, _ = render(project, state)
        self.assertEqual(float(unfaded[5, 23, 0]), .75)
        values = edge_noise(np.array([-1, .25, 2], np.float32), np.array([3, .5, -2], np.float32))
        self.assertTrue(np.all((values >= 0) & (values <= 1)))
        np.testing.assert_array_equal(values, edge_noise(np.array([-1, .25, 2], np.float32), np.array([3, .5, -2], np.float32)))

    def test_luminance_distance_uses_union_of_source_coverage(self):
        folder, meta, covered, _ = self.project()
        (~covered).astype("u1").tofile(folder / "b.coverage.u8")
        meta["layers"].append({"key": "b", "coverage_file": "b.coverage.u8"})
        path = ensure_edge_map(folder, meta)
        np.testing.assert_array_equal(np.fromfile(path, "<f2"), 1024)

    def test_transformed_sampling_clamps_half_pixel_border_like_the_gpu(self):
        image = np.ones((3, 4), np.float32)
        np.testing.assert_allclose(warp(image, {"x": -.1 / 4}), image)
        outside = warp(image, {"x": -.6 / 4})
        np.testing.assert_array_equal(outside[:, -1], 0)

class InteriorHoleTest(unittest.TestCase):
    def test_small_interior_holes_are_not_edges_but_outer_and_large_gaps_are(self):
        covered = np.zeros((300, 300), bool)
        covered[20:280, 20:280] = True
        covered[100:104, 100:104] = False          # bad pixels / saturated core
        covered[150:280, 200:240] = False          # large chip gap (5200 px) reaching the outer edge
        distance = edge_distance(covered).astype(float)
        self.assertGreater(distance[102, 102], 70)  # hole interior counts as deep coverage
        self.assertEqual(distance[25, 150], 6)       # outer edge still softens
        self.assertLess(distance[200, 195], 6)       # large gap still softens


if __name__ == "__main__":
    unittest.main()
