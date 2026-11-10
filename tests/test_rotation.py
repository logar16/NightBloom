from __future__ import annotations

from dataclasses import asdict, replace
import json
from pathlib import Path
import shutil
import unittest
import uuid
from unittest.mock import patch

import numpy as np
from astropy.wcs import WCS
from fastapi.testclient import TestClient

from astroimp.aladin import AladinView, rotated_view, rotated_wcs
from astroimp.catalog import Observation, catalog
from astroimp.mast import write_json
from astroimp.server import create_app
from astroimp.session import (Sessions, read_json, rotate_capture, save_snapshot_header,
                             search_mast, snapshot_header)


class RotationTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-rotation-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        wcs = WCS(naxis=2)
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        wcs.wcs.crval = [359.99, 20]
        # Aladin's WCS reference pixel is not necessarily the JPEG centre.
        wcs.wcs.crpix = [86, 49]
        wcs.wcs.cdelt = [-1/3600, 1/3600]
        self.header = wcs.to_header()
        width, height = 160, 90
        center = wcs.pixel_to_world((width-1)/2, (height-1)/2).icrs
        corners = wcs.pixel_to_world([-.5, width-.5, width-.5, -.5],
                                    [-.5, -.5, height-.5, height-.5]).icrs
        self.view = asdict(AladinView(
            (float(center.ra.deg), float(center.dec.deg)), (width/60, height/60),
            tuple(zip(corners.ra.deg, corners.dec.deg)), (width, height),
            (161, 91), ("RA---TAN", "DEC--TAN"), 100))

    def session(self, name="one"):
        path = self.root / "sessions" / name
        write_json(path / "view.json", self.view)
        write_json(path / "snapshot-wcs.json", dict(self.header))
        write_json(path / "catalog.json", {"groups": []})
        write_json(path / "selection.json", {"obs_ids": []})
        return path

    def test_rotation_math_corners_center_and_known_pixel_with_offcenter_crpix(self):
        base = WCS(self.header)
        dims = np.asarray(self.view["pixel_dimensions"])
        center = (dims-1)/2
        for angle in (0, 90, -30):
            with self.subTest(angle=angle):
                wcs = rotated_wcs(self.header, dims, angle)
                np.testing.assert_allclose(wcs.pixel_to_world_values(*center),
                                           base.pixel_to_world_values(*center), atol=1e-12)
                theta = np.deg2rad(angle)
                rotation = np.array([[np.cos(theta), -np.sin(theta)],
                                     [np.sin(theta), np.cos(theta)]])
                pixels = np.array([[-.5, -.5], [dims[0]-.5, -.5],
                                   [dims[0]-.5, dims[1]-.5], [-.5, dims[1]-.5]])
                expected = center+(pixels-center) @ rotation.T
                world = base.pixel_to_world(expected[:, 0], expected[:, 1])
                actual = wcs.pixel_to_world(pixels[:, 0], pixels[:, 1])
                np.testing.assert_allclose(actual.separation(world).arcsec, 0, atol=1e-8)
                # The northward pixel displacement rotates right at +90°.
                north = base.pixel_to_world(*center+[0, 10])
                np.testing.assert_allclose(wcs.world_to_pixel(north),
                                           center+rotation.T @ [0, 10], atol=1e-8)
                view = rotated_view(self.view, self.header, angle)
                if angle:
                    np.testing.assert_allclose(view["corners_icrs_deg"],
                                               np.column_stack((actual.ra.deg, actual.dec.deg)))
                else:
                    self.assertEqual(view, self.view)
                    self.assertEqual(dict(wcs.to_header()), dict(base.to_header()))

    def test_catalog_filters_and_projects_against_rotated_rectangle(self):
        base = WCS(self.header)
        center = (np.array(self.view["pixel_dimensions"])-1)/2
        north = base.pixel_to_world(*center+[0, 65]).icrs
        west = base.pixel_to_world(*center+[65, 0]).icrs
        obs = Observation("1", "north", "HST", "WFC3", "F656N", "1", "", "", "",
                          float(north.ra.deg), float(north.dec.deg), 656, 1, "", "a.fits", "")
        other = replace(obs, obs_id="west", ra=float(west.ra.deg), dec=float(west.dec.deg))
        for angle, expected in ((0, "west"), (90, "north")):
            with self.subTest(angle=angle), patch("astroimp.catalog.query", return_value=[obs, other]):
                view = rotated_view(self.view, self.header, angle)
                result = catalog(view["center_icrs_deg"], view["corners_icrs_deg"],
                                 rotated_wcs(self.header, view["pixel_dimensions"], angle).to_header(),
                                 view["pixel_dimensions"])
                self.assertEqual([o["obs_id"] for g in result["groups"] for o in g["observations"]],
                                 [expected])

    def test_rotation_preserves_non_square_pixel_scales(self):
        base = WCS(self.header)
        base.wcs.cdelt = [-1/3600, 2/3600]
        center = (np.array(self.view["pixel_dimensions"])-1)/2
        target = rotated_wcs(base.to_header(), self.view["pixel_dimensions"], -30)
        np.testing.assert_allclose(np.linalg.norm(target.pixel_scale_matrix, axis=0),
                                   np.linalg.norm(base.pixel_scale_matrix, axis=0), atol=1e-14)
        np.testing.assert_allclose(target.pixel_to_world_values(*center),
                                   base.pixel_to_world_values(*center), atol=1e-12)

    def test_rotation_is_idempotent_and_reset_restores_legacy_geometry_without_header_writes(self):
        path = self.session()
        legacy = {key: value for key, value in self.view.items() if key != "rotation_deg"}
        write_json(path / "view.json", legacy)
        header_bytes = (path / "snapshot-wcs.json").read_bytes()
        with patch("astroimp.session.catalog", return_value={"groups": []}), \
                patch("astroimp.surveys.available", return_value=[]):
            rotate_capture(path, 90)
            first = (path / "view.json").read_bytes()
            rotate_capture(path, -30)
            rotate_capture(path, 90)
            self.assertEqual(first, (path / "view.json").read_bytes())
            rotate_capture(path, 0)
        reset = read_json(path / "view.json")
        for key in ("center_icrs_deg", "field_arcmin", "corners_icrs_deg"):
            self.assertEqual(json.dumps(reset[key]), json.dumps(legacy[key]))
        self.assertEqual(header_bytes, (path / "snapshot-wcs.json").read_bytes())

    def test_extra_archive_search_projects_through_current_rotation(self):
        path = self.session()
        write_json(path / "view.json", {**self.view, "rotation_deg": -30})
        with patch("astroimp.session.catalog", return_value={"groups": []}) as lookup:
            search_mast(path)
        expected = rotated_wcs(self.header, self.view["pixel_dimensions"], -30)
        np.testing.assert_allclose(WCS(lookup.call_args.args[2]).pixel_scale_matrix,
                                   expected.pixel_scale_matrix)

    def test_original_header_roundtrips_with_duplicate_commentary_and_numeric_precision(self):
        path = self.session()
        self.header.add_comment("Original Aladin capture")
        self.header.add_comment("A second comment")
        save_snapshot_header(path, self.header)
        self.assertEqual(snapshot_header(path).tostring(), self.header.tostring())

    def test_session_scoped_endpoint_marks_only_target_project_for_rebuild(self):
        one, two = self.session(), self.session("two")
        for path in (one, two):
            write_json(path / "project" / "meta.json", {"layers": [], "width": 1})
        app = create_app(self.root)
        app.state.sessions.open("two")
        with patch("astroimp.session.catalog", return_value={"groups": []}), \
                patch("astroimp.surveys.available", return_value=[]), TestClient(app) as client:
            response = client.put("/api/session/rotation", json={"rotation_deg": 90},
                                  headers={"X-AstroImp-Session": "one"})
            self.assertEqual(response.status_code, 200)
            app.state.jobs.thread.join(5)
            self.assertEqual(app.state.jobs.job.state, "done")
            self.assertTrue(Sessions(self.root / "sessions").payload(one)["rebuild_required"])
            self.assertFalse(Sessions(self.root / "sessions").payload(two)["rebuild_required"])
            self.assertEqual(read_json(two / "view.json")["rotation_deg"], 0)
            for value in (181, -181):
                self.assertEqual(client.put("/api/session/rotation", json={"rotation_deg": value},
                    headers={"X-AstroImp-Session": "one"}).status_code, 422)


if __name__ == "__main__":
    unittest.main()
