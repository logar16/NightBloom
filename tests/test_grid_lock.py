from copy import deepcopy
import json
import os
from pathlib import Path
import shutil
import unittest
import uuid
from unittest.mock import patch

import numpy as np
from astropy.io import fits
from astropy.wcs import WCS

from astroimp.build import build, build_plan, expand_grid, expansion_plan
from astroimp.builds import restore
from astroimp.fullres import view_points
from astroimp.grid import GridMap, grid_from_meta, locked_grid, remap_markers, remap_state
from astroimp.mast import write_json


def grid(width=100, height=120, scale=1., ra=10., dec=20.):
    wcs = WCS(naxis=2)
    wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    wcs.wcs.crval = [ra, dec]
    wcs.wcs.crpix = [width/2+.5, height/2+.5]
    wcs.wcs.cdelt = [-scale/3600, scale/3600]
    return {"width": width, "height": height, "pixel_scale_arcsec": scale,
            "wcs": dict(wcs.to_header())}


class GridLockTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-grid-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(lambda: shutil.rmtree(self.root))

    def fixture(self):
        downloads, session = self.root / "downloads", self.root / "session"
        downloads.mkdir()
        session.mkdir()
        observations = []
        for name, ra in (("a", 10.), ("b", 10.03)):
            g = grid(32, 40, 1, ra=ra)
            yy, xx = np.mgrid[:40, :32]
            fits.PrimaryHDU((1+xx*.1+yy*.03).astype("f4"),
                            header=fits.Header(g["wcs"])).writeto(downloads / f"{name}.fits")
            observations.append({"obs_id": name, "filter": name.upper(), "collection": "HST",
                                 "data_uri": f"mast:HST/{name}.fits"})
        write_json(session / "catalog.json", {"groups": [{"observations": observations}]})
        write_json(session / "selection.json", {"obs_ids": ["a"]})
        return session, downloads, observations

    def test_rebuild_locks_edited_canvas_and_reports_clipping(self):
        session, downloads, observations = self.fixture()
        first = build(session, downloads=downloads, max_side=100)
        self.assertIsNone(locked_grid(session))
        expected = {"layers": [], "global": {"rotation": 37}}
        edits = {**expected, "starOverrides": {}}
        write_json(session / "edits.json", edits)
        write_json(session / "selection.json", {"obs_ids": ["a", "b"]})
        plan = build_plan(observations, downloads, session=session)
        self.assertTrue(plan["grid_locked"])
        self.assertGreater(plan["files"][1]["outside_fraction"], .99)
        second = build(session, downloads=downloads, max_side=100)
        self.assertEqual(grid_from_meta(first), grid_from_meta(second))
        self.assertEqual(locked_grid(session), grid_from_meta(first))
        self.assertEqual(json.loads((session / "edits.json").read_bytes()), expected)

    def test_unchanged_rebuild_reuses_every_downstream_product(self):
        session, downloads, _ = self.fixture()
        first = build(session, downloads=downloads, max_side=100)
        project = session / "project"
        products = {name: (project / name).read_bytes() for name, _, _ in first["downstream_products"]}
        with patch("astroimp.build.ensure_edge_map", side_effect=AssertionError("recomputed edges")):
            second = build(session, downloads=downloads, max_side=100)
        self.assertEqual(second["downstream_signature"], first["downstream_signature"])
        self.assertEqual(products, {name: (project / name).read_bytes() for name in products})

    def test_rebuild_cache_misses_for_changed_inputs_versions_settings_or_missing_products(self):
        from astroimp.build import ensure_edge_map, EDGE_VERSION
        session, downloads, _ = self.fixture()
        meta = build(session, downloads=downloads, max_side=100)
        project = session / "project"

        def touch(path):
            stat = path.stat()
            os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))

        for reason in ("aligned", "coverage", "source", "edges", "missing", "wavelength"):
            with self.subTest(reason=reason):
                if reason in ("aligned", "coverage"):
                    touch(project / meta["layers"][0]["file" if reason == "aligned" else "coverage_file"])
                elif reason == "source":
                    touch(downloads / "a.fits")
                elif reason == "missing":
                    (project / meta["luminance"]["edge_file"]).unlink()
                elif reason == "wavelength":
                    content = json.loads((session / "catalog.json").read_text())
                    content["groups"][0]["observations"][0]["wavelength_nm"] = 600
                    write_json(session / "catalog.json", content)
                with patch("astroimp.build.EDGE_VERSION", EDGE_VERSION + (reason == "edges")), \
                        patch("astroimp.build.ensure_edge_map", wraps=ensure_edge_map) as edges:
                    meta = build(session, downloads=downloads, max_side=100)
                self.assertTrue(edges.called)
                # Restore the unpatched edge version before the next invalidation.
                if reason == "edges":
                    meta = build(session, downloads=downloads, max_side=100)

    def test_expand_unions_sources_at_current_scale_then_caps_size(self):
        session = self.root / "session"
        (session / "project").mkdir(parents=True)
        old = grid()
        write_json(session / "project" / "meta.json", old)
        source = grid(100, 120, .25, ra=10.03)
        inputs = [((120, 100), WCS(source["wcs"]))]
        expanded = expand_grid(session, inputs, max_side=1000)
        self.assertAlmostEqual(expanded["pixel_scale_arcsec"], 1.)
        for g in (old, source):
            corners = GridMap(g, expanded).points([[0, 0], [g["width"], g["height"]]])
            self.assertTrue(np.all(corners >= -.51))
            self.assertTrue(np.all(corners <= [expanded["width"]+.51, expanded["height"]+.51]))
        capped = expand_grid(session, inputs, max_side=80)
        self.assertLessEqual(max(capped["width"], capped["height"]), 80)
        self.assertGreater(capped["pixel_scale_arcsec"], 1.)

    def test_capture_rotation_rebuilds_archive_canvas_and_remaps_edits_and_markers(self):
        from astroimp.fill import grid_view
        from astroimp.surveys import view_grid
        session, downloads, _ = self.fixture()
        view = {"center_icrs_deg": [10, 20], "field_arcmin": [.6, .3],
                "pixel_dimensions": [600, 300]}
        write_json(session / "view.json", view)
        before = build(session, downloads=downloads, max_side=100)
        state = self.state()
        write_json(session / "edits.json", state)
        write_json(session / "markers.json", [{"id": "p", "kind": "point", "x": 20, "y": 15}])
        for angle in (90, 0):
            with self.subTest(angle=angle):
                old_meta = json.loads((session / "project" / "meta.json").read_text())
                old_state = json.loads((session / "edits.json").read_text())
                old_markers = json.loads((session / "markers.json").read_text())
                write_json(session / "view.json", {**view, "rotation_deg": angle})
                self.assertIsNone(locked_grid(session))
                after = build(session, downloads=downloads, max_side=100)
                self.assertEqual(after["view_rotation_deg"], angle)
                self.assertAlmostEqual(grid_view(after)["rotation_deg"], angle, places=6)
                expected = view_grid({**view, "rotation_deg": angle}, 1, 100)
                self.assertEqual((after["width"], after["height"]),
                                 (expected["width"], expected["height"]))
                saved = json.loads((session / "edits.json").read_text())
                np.testing.assert_allclose(saved["layers"][0]["brush"]["strokes"][0]["points"],
                    GridMap(old_meta, after).points(old_state["layers"][0]["brush"]["strokes"][0]["points"]),
                    atol=1e-6)
                moved = json.loads((session / "markers.json").read_text())
                np.testing.assert_allclose([moved[0]["x"], moved[0]["y"]],
                    GridMap(old_meta, after).points([[old_markers[0]["x"], old_markers[0]["y"]]])[0],
                    atol=1e-6)
                self.assertEqual(saved["checkpoints"][-1]["name"], "Before view rotation")
                self.assertEqual(locked_grid(session), grid_from_meta(after))

    def test_rotated_expansion_keeps_canvas_orientation(self):
        from astroimp.fill import grid_view
        from astroimp.surveys import view_grid
        session = self.root / "session"
        old = view_grid({"center_icrs_deg": [10, 20], "field_arcmin": [2, 1],
                         "rotation_deg": -30}, 1, 120)
        write_json(session / "project" / "meta.json", old)
        source = grid(120, 120, .25, ra=10.03)
        result = expand_grid(session, [((120, 120), WCS(source["wcs"]))], max_side=80)
        self.assertLessEqual(max(result["width"], result["height"]), 80)
        # Moving the frame centre slightly changes its local sky position angle.
        self.assertAlmostEqual(grid_view(result)["rotation_deg"], -30, places=2)
        np.testing.assert_allclose(
            WCS(result["wcs"]).pixel_scale_matrix/result["pixel_scale_arcsec"],
            WCS(old["wcs"]).pixel_scale_matrix/old["pixel_scale_arcsec"], atol=1e-12)
        for g in (old, source):
            corners = GridMap(g, result).points([[0, 0], [g["width"], 0],
                                                 [g["width"], g["height"]], [0, g["height"]]])
            self.assertTrue(np.all(corners >= -1e-6))
            self.assertTrue(np.all(corners <= [result["width"]+1e-6, result["height"]+1e-6]))

    def state(self):
        return {
            "layers": [{"id": "one", "source": "a",
                        "transform": {"x": .02, "y": -.03, "scale": 1.1, "rotation": 12},
                        "brush": {"strokes": [{"points": [[20., 30.], [55., 70.]], "size": 12.}]}}],
            "global": {"crop": {"x": .1, "y": .2, "w": .6, "h": .5},
                       "rotation": -39.58, "flipX": True},
            "starOverrides": {},
        }

    def test_geometry_roundtrip_and_output_rotation(self):
        old, new = grid(), grid(210, 180, .8, ra=10.001, dec=20.002)
        state = self.state()
        state["checkpoints"] = [{"name": "saved", "state": deepcopy(state)}]
        moved = remap_state(state, old, new)
        back = remap_state(moved, new, old)
        np.testing.assert_allclose(back["layers"][0]["brush"]["strokes"][0]["points"],
                                   state["layers"][0]["brush"]["strokes"][0]["points"], atol=.5)
        self.assertAlmostEqual(back["layers"][0]["brush"]["strokes"][0]["size"], 12)
        for a, b in ((state, back), (state["checkpoints"][0]["state"], back["checkpoints"][0]["state"])):
            for key, extent in (("x", 100), ("y", 120), ("w", 100), ("h", 120)):
                self.assertLess(abs(a["global"]["crop"][key]-b["global"]["crop"][key])*extent, .5)
            self.assertEqual(a["global"]["rotation"], b["global"]["rotation"])
            np.testing.assert_allclose([a["layers"][0]["transform"][k] for k in ("x", "y")],
                                       [b["layers"][0]["transform"][k] for k in ("x", "y")], atol=.005)
        # The centre of the rotated/flipped output crop still samples the same sky.
        def center(meta, value):
            crop = value["global"]["crop"]
            return np.array(view_points(meta, value, crop["x"]+crop["w"]/2, crop["y"]+crop["h"]/2))
        np.testing.assert_allclose(center(new, moved),
                                   GridMap(old, new).points([center(old, state)])[0], atol=.1)
        def transformed(point, meta, layer):
            transform = layer["transform"]
            center = np.array([meta["width"], meta["height"]])/2
            a = np.deg2rad(transform["rotation"])
            matrix = np.array([[np.cos(a), -np.sin(a)], [np.sin(a), np.cos(a)]])
            return ((np.array(point)-center) @ matrix.T * transform["scale"] + center
                    + np.array([transform["x"]*meta["width"], transform["y"]*meta["height"]]))
        original_point = [20., 30.]
        mapped_point = GridMap(old, new).points([original_point])[0]
        np.testing.assert_allclose(transformed(mapped_point, new, moved["layers"][0]),
                                   GridMap(old, new).points([transformed(original_point, old, state["layers"][0])])[0],
                                   atol=.5)

    def test_remap_anchored_crop_uses_application_angle_including_checkpoints(self):
        from astroimp.view import view_frame
        old = grid()
        for new in (grid(160, 180), grid(210, 180, .8, ra=10.001, dec=20.002)):
            with self.subTest(width=new["width"]):
                state = self.state()
                state["global"]["crop"]["rotation"] = 23
                state["checkpoints"] = [{"name": "saved", "state": deepcopy(state)}]
                moved = remap_state(state, old, new)
                for before, after in ((state, moved), (state["checkpoints"][0]["state"],
                                                       moved["checkpoints"][0]["state"])):
                    self.assertEqual(after["global"]["crop"]["rotation"], 23)
                    self.assertEqual(after["global"]["rotation"], before["global"]["rotation"])
                    def center(meta, value):
                        b = view_frame(meta, value["global"])
                        return np.array(view_points(meta, value, b["x"]+b["w"]/2, b["y"]+b["h"]/2))
                    np.testing.assert_allclose(center(new, after),
                        GridMap(old, new).points([center(old, before)])[0], atol=.1)
                    back = remap_state(after, new, old)
                    np.testing.assert_allclose([back["global"]["crop"][k] for k in ("x", "y", "w", "h")],
                        [before["global"]["crop"][k] for k in ("x", "y", "w", "h")], atol=.005)

    def test_remap_does_not_activate_a_previously_ignored_full_frame_anchor(self):
        state = self.state()
        state["global"]["crop"] = {"x": 0, "y": 0, "w": 1, "h": 1, "rotation": 23}
        moved = remap_state(state, grid(), grid(160, 180))
        self.assertNotIn("rotation", moved["global"]["crop"])

    def test_marker_points_rectangles_and_radius_roundtrip(self):
        old, new = grid(), grid(160, 180, .7, ra=10.001)
        markers = [{"kind": "point", "x": 30., "y": 40., "radius": 4},
                   {"kind": "rect", "x1": 10., "y1": 20., "x2": 40., "y2": 60.}]
        back = remap_markers(remap_markers(markers, old, new), new, old)
        for original, actual in zip(markers, back):
            for key, value in original.items():
                if key != "kind":
                    self.assertAlmostEqual(value, actual[key], delta=.5)

    def test_expand_saves_before_checkpoint_and_restore_maps_markers(self):
        session, downloads, _ = self.fixture()
        old = build(session, downloads=downloads, max_side=100)
        state = self.state()
        write_json(session / "edits.json", state)
        marker = [{"kind": "point", "x": 15., "y": 20., "radius": 4}]
        write_json(session / "markers.json", marker)
        write_json(session / "selection.json", {"obs_ids": ["a", "b"]})
        expanded = expansion_plan(session, downloads, max_side=100)
        new = build(session, downloads=downloads, max_side=100, expand=expanded)
        saved = json.loads((session / "edits.json").read_text())
        self.assertEqual(saved["checkpoints"][-1]["name"], "Before expand")
        self.assertEqual(saved["global"]["rotation"], state["global"]["rotation"])
        archived = next((session / "builds").iterdir())
        self.assertEqual(json.loads((archived / "edits.json").read_text()), state)
        restore(session, archived.name)
        self.assertEqual(locked_grid(session), grid_from_meta(old))
        restored = json.loads((session / "markers.json").read_text())[0]
        self.assertAlmostEqual(restored["x"], 15., delta=.5)
        self.assertGreater(new["width"], old["width"])

    def test_cancelled_expansion_keeps_grid_edits_and_project(self):
        from astroimp.cancellation import CancelledError
        session, downloads, _ = self.fixture()
        old = build(session, downloads=downloads, max_side=100)
        write_json(session / "edits.json", self.state())
        before = (session / "edits.json").read_bytes()
        with patch("astroimp.build._build", side_effect=CancelledError("cancelled")):
            with self.assertRaises(CancelledError):
                build(session, downloads=downloads, expand=grid(200, 200))
        self.assertEqual(json.loads((session / "project" / "meta.json").read_text()), old)
        self.assertEqual((session / "edits.json").read_bytes(), before)
        self.assertFalse((session / "grid.json").exists())

    def test_expansion_api_rejects_stale_confirmation_and_builds_confirmed_grid(self):
        from fastapi.testclient import TestClient
        from astroimp.server import create_app

        session, downloads, _ = self.fixture()
        build(session, downloads=downloads, max_side=100)
        write_json(session / "edits.json", self.state())
        write_json(session / "selection.json", {"obs_ids": ["a", "b"]})
        app = create_app(self.root)
        app.state.sessions.current = session
        with TestClient(app) as client:
            response = client.get("/api/build/expand/plan")
            self.assertEqual(response.status_code, 200)
            confirmed = response.json()
            stale = {**confirmed, "width": confirmed["width"]+1}
            self.assertEqual(client.post("/api/build/expand", json={"grid": stale}).status_code, 409)
            self.assertEqual(client.post("/api/build/expand", json={"grid": confirmed}).status_code, 200)
            app.state.jobs.thread.join(timeout=10)
            self.assertEqual(app.state.jobs.payload()["state"], "done")
            self.assertEqual(grid_from_meta(json.loads((session / "project" / "meta.json").read_text())),
                             confirmed)

    def test_restore_uses_archived_edits_grid_when_it_differs_from_project(self):
        session = self.root / "session"
        target = session / "builds" / "legacy"
        (target / "project").mkdir(parents=True)
        old, newer = grid(), grid(180, 210, .8, ra=10.001)
        write_json(target / "project" / "meta.json", old)
        write_json(target / "grid.json", newer)
        write_json(target / "edits.json", remap_state(self.state(), old, newer))
        restore(session, "legacy")
        state = json.loads((session / "edits.json").read_text())
        np.testing.assert_allclose(state["layers"][0]["brush"]["strokes"][0]["points"],
                                   self.state()["layers"][0]["brush"]["strokes"][0]["points"], atol=.5)
        self.assertEqual(locked_grid(session), old)

    def test_recovery_backup_is_not_offered_as_an_editable_session(self):
        from astroimp.session import Sessions
        for name in ("original", "_backup_original_20261024T201850Z"):
            directory = self.root / "sessions" / name
            directory.mkdir(parents=True)
            write_json(directory / "catalog.json", {"groups": []})
        self.assertEqual([s["id"] for s in Sessions(self.root / "sessions").list()], ["original"])
