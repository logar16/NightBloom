from __future__ import annotations

import json
import io
import math
import shutil
import unittest
import uuid
from dataclasses import replace
from pathlib import Path
from unittest.mock import Mock, patch

import numpy as np
import requests
from astropy import units as u
from astropy.coordinates import SkyCoord
from astropy.io import fits
from astropy.wcs import WCS
from astropy_healpix import HEALPix

from astroimp import surveys, survey_sources
from astroimp.build import build, build_plan, cached_source, download_all, survey_size
from astroimp.cancellation import CancelledError
from astroimp.fill import fill_around, fit_match, grid_view, margin_grid, sample_layer
from astroimp.grid import GridMap
from astroimp.mast import write_json
from astroimp.session import Sessions, read_json, search_mast, search_view
from astroimp.storage import Storage


class SurveyTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-surveys-" + uuid.uuid4().hex)
        self.addCleanup(lambda: shutil.rmtree(self.root, ignore_errors=True))
        self.view = {"center_icrs_deg": [38.2, 61.45], "field_arcmin": [90, 75],
                     "pixel_dimensions": [500, 400]}
        grid = surveys.view_grid(self.view, 1, 64)
        self.view["corners_icrs_deg"] = WCS(grid["wcs"]).calc_footprint(
            axes=(grid["width"], grid["height"])).tolist()
        self.band = surveys.BY_ID["CDS/P/IPHAS/DR2/halpha"]
        self.obs = surveys.groups(self.view, [self.band])[0]["observations"][0]

    def test_registry_covers_requested_fits_bands_and_known_native_orders(self):
        self.assertEqual(len(surveys.BANDS), 29)
        self.assertEqual(len(surveys.BY_ID), 29)
        self.assertAlmostEqual(self.band.native_scale_arcsec, .2013, places=3)
        self.assertAlmostEqual(surveys.BY_ID["CDS/P/VTSS/HaCC"].native_scale_arcsec, 51.53, places=1)
        self.assertEqual(self.obs["name"], "IPHAS Hα")
        self.assertEqual(self.obs["wavelength_nm"], 656.3)
        for band in surveys.BANDS:
            self.assertTrue(band.hips_id.startswith(("CDS/P/", "ESAVO/P/")))
            self.assertGreater(band.wavelength_nm, 0)
            self.assertTrue(0 < band.sky_fraction <= 1)

    def test_threshold_is_longest_side_inclusive_not_radius(self):
        with patch("astroimp.session.catalog", return_value={"groups": []}) as mast, \
                patch("astroimp.surveys.available", return_value=[self.band]) as availability:
            for sides, expected in (([30, 30], True), ([30.01, 2], False), ([2, 31], False)):
                with self.subTest(sides=sides):
                    mast.reset_mock()
                    result = search_view({**self.view, "field_arcmin": sides}, self.root / "cache")
                    self.assertEqual(result["mast_searched"], expected)
                    self.assertEqual(mast.call_count, int(expected))
                    self.assertEqual(result["groups"][0]["collection"], "Survey")
            self.assertEqual(availability.call_count, 3)

    def test_detail_matches_four_k_view_not_native_pixel_scale(self):
        w3 = surveys.BY_ID["CDS/P/allWISE/W3"]
        w4 = surveys.BY_ID["CDS/P/allWISE/W4"]
        dss = surveys.BY_ID["CDS/P/DSS2/red"]
        for degrees, w3_soft, w4_soft in ((1, True, True), (4, False, True), (8, False, False)):
            for band, soft in ((w3, w3_soft), (w4, w4_soft), (dss, degrees == 1), (self.band, False)):
                with self.subTest(degrees=degrees, band=band.hips_id):
                    # Portrait orientation deliberately makes the second side longest.
                    view = {**self.view, "field_arcmin": [1, degrees * 60]}
                    self.assertEqual(surveys.detail_for_view(band, view)["soft"], soft)
        detail = surveys.detail_for_view(w3, {**self.view, "field_arcmin": [60, 60]})
        self.assertEqual(detail["soft_reason"], "~6.5″ detail; crisp only for views ≥ 3.7°")
        threshold = w3.fwhm_arcsec * 4096 / 2 / 60
        self.assertFalse(surveys.detail_for_view(w3, {"field_arcmin": [1, threshold]})["soft"])
        self.assertTrue(surveys.detail_for_view(w3, {"field_arcmin": [1, threshold - .01]})["soft"])
        for band in (*[b for b in surveys.BANDS if b.fwhm_arcsec <= 4],
                     replace(self.band, fwhm_arcsec=4)):
            self.assertFalse(surveys.detail_for_view(band, {"field_arcmin": [.001, .001]})["soft"])

    def test_legacy_session_backfills_catalog_and_layer_detail_without_writing(self):
        path = self.root / "sessions" / "legacy"
        w3 = surveys.BY_ID["CDS/P/allWISE/W3"]
        groups = surveys.groups(self.view, [self.band, w3])
        for group in groups:
            for observation in group["observations"]:
                for key in ("fwhm_arcsec", "soft", "soft_reason"):
                    observation.pop(key)
        write_json(path / "view.json", self.view)
        write_json(path / "catalog.json", {"groups": groups})
        write_json(path / "selection.json", {"obs_ids": [w3.obs_id]})
        write_json(path / "project" / "meta.json", {"layers": [{"hips_id": w3.hips_id}]})
        before = (path / "catalog.json").read_bytes()
        payload = Sessions(self.root / "sessions").payload(path)
        optical, wise = [g["observations"][0] for g in payload["catalog"]["groups"]]
        self.assertFalse(optical["soft"])
        self.assertTrue(wise["soft"])
        self.assertEqual(payload["meta"]["layers"][0]["fwhm_arcsec"], 6.5)
        self.assertEqual(payload["selection"]["obs_ids"], [w3.obs_id])
        self.assertEqual((path / "catalog.json").read_bytes(), before)
        self.assertNotIn("fwhm_arcsec", read_json(path / "project" / "meta.json")["layers"][0])

    def test_availability_queries_exact_ids_and_caches_result(self):
        response = Mock()
        response.json.return_value = [{"ID": self.band.hips_id}, {"ID": "CDS/P/PanSTARRS/DR1/g"}]
        with patch("astroimp.surveys.requests.get", return_value=response) as get:
            first = surveys.available(self.view, self.root)
            self.assertEqual(surveys.available(self.view, self.root), first)
            get.assert_called_once()
            self.assertIn("ID=CDS/P/IPHAS/DR2/halpha", get.call_args.kwargs["params"]["expr"])
            self.assertNotIn("PanSTARRS", get.call_args.kwargs["params"]["expr"])
            self.assertEqual(first, [self.band])

    def test_subwindows_share_world_coordinates_including_partial_last_tile(self):
        grid = surveys.view_grid(self.view, 1, 91)
        target = WCS(grid["wcs"])
        total = 0
        for x, y, wcs in surveys.windows(grid, side=32):
            width, height = wcs.pixel_shape
            total += width*height
            px, py = np.array([0, width-.5]), np.array([0, height-.5])
            np.testing.assert_allclose(wcs.pixel_to_world_values(px, py),
                                       target.pixel_to_world_values(px+x, py+y), atol=1e-10)
            np.testing.assert_allclose(wcs.wcs.crpix, target.wcs.crpix-[x, y])
        self.assertEqual(total, grid["width"]*grid["height"])

    def test_rotated_survey_grid_keeps_frame_dimensions_scale_and_sky_orientation(self):
        from astropy import units as u
        from astropy.coordinates import SkyCoord
        legacy = surveys.view_grid(self.view, 1, 64)
        self.assertEqual(legacy, surveys.view_grid({**self.view, "rotation_deg": 0}, 1, 64))
        for angle in (90, -30):
            with self.subTest(angle=angle):
                grid = surveys.view_grid({**self.view, "rotation_deg": angle}, 1, 64)
                self.assertEqual([grid[k] for k in ("width", "height", "pixel_scale_arcsec")],
                                 [legacy[k] for k in ("width", "height", "pixel_scale_arcsec")])
                wcs = WCS(grid["wcs"])
                center = SkyCoord(*self.view["center_icrs_deg"], unit="deg")
                up = center.directional_offset_by(angle*u.deg, grid["pixel_scale_arcsec"]*u.arcsec)
                np.testing.assert_allclose(wcs.world_to_pixel(up),
                    [(grid["width"]-1)/2, (grid["height"]-1)/2+1], atol=1e-6)
                self.assertNotEqual(surveys.grid_hash(grid), surveys.grid_hash(legacy))

    def test_rotated_fetch_uses_full_wcs_and_retains_exact_fits_size_estimate(self):
        grid = surveys.view_grid({**self.view, "rotation_deg": -30}, 1, 24)
        def pixels(band, wcs, progress):
            np.testing.assert_allclose(wcs.pixel_scale_matrix, WCS(grid["wcs"]).pixel_scale_matrix)
            return np.ones(wcs.array_shape, np.float32)
        with patch("astroimp.surveys.cutout", side_effect=pixels):
            path = surveys.fetch(self.obs, self.root / "downloads", grid)
        self.assertEqual(path.stat().st_size, survey_size(grid))
        with fits.open(path) as hdus:
            np.testing.assert_allclose(WCS(hdus[0].header).pixel_scale_matrix,
                                       WCS(grid["wcs"]).pixel_scale_matrix)
            self.assertEqual(hdus[0].data.shape, (grid["height"], grid["width"]))

    def test_fits_cache_is_grid_specific_and_storage_tracks_band_not_shared_hash(self):
        downloads = self.root / "downloads"
        grid = surveys.view_grid(self.view, 1, 24)
        other = surveys.groups(self.view, [surveys.BY_ID["CDS/P/DSS2/red"]])[0]["observations"][0]
        def pixels(band, wcs, progress):
            return np.arange(np.prod(wcs.array_shape), dtype=np.float32).reshape(wcs.array_shape)
        with patch("astroimp.surveys.cutout", side_effect=pixels) as network:
            first = surveys.fetch(self.obs, downloads, grid)
            second = surveys.fetch(other, downloads, grid)
            surveys.fetch(self.obs, downloads, grid)
            self.assertEqual(network.call_count, 2)
        self.assertNotEqual(first.parent, second.parent)
        self.assertEqual(first.name, second.name)
        self.assertEqual(cached_source(self.obs, downloads, grid), first)
        changed = {**grid, "width": grid["width"]+1}
        self.assertIsNone(cached_source(self.obs, downloads, changed))
        session = self.root / "sessions" / "one"
        write_json(session / "catalog.json", {"groups": [{"observations": [self.obs, other]}]})
        write_json(session / "selection.json", {"obs_ids": [self.obs["obs_id"]]})
        rows = Storage(self.root).inventory()["downloads"]
        self.assertEqual({r["status"] for r in rows}, {"in_use", "orphan"})
        live = next(r for r in rows if r["status"] == "in_use")
        self.assertEqual(live["instrument"], "IPHAS")
        with self.assertRaises(ValueError):
            Storage(self.root).delete(downloads=[live["path"]])
        from astroimp.session import Sessions
        Sessions(self.root / "sessions").delete("one")
        self.assertEqual({r["status"] for r in Storage(self.root).inventory()["downloads"]},
                         {"trash_only", "orphan"})

    def test_survey_build_skips_reprojection_keeps_nan_coverage_and_native_metadata(self):
        session = self.root / "sessions" / "one"
        write_json(session / "view.json", self.view)
        write_json(session / "catalog.json", {"groups": surveys.groups(self.view, [self.band])})
        write_json(session / "selection.json", {"obs_ids": [self.obs["obs_id"]]})
        def pixels(band, wcs, progress):
            y, x = np.mgrid[:wcs.array_shape[0], :wcs.array_shape[1]]
            image = (x+y).astype(np.float32)
            image[:5] = np.nan
            return image
        with patch("astroimp.surveys.cutout", side_effect=pixels), \
                patch("astroimp.build.reproject_interp", side_effect=AssertionError("already on grid")):
            meta = build(session, downloads=self.root / "downloads", max_side=32)
        self.assertEqual(max(meta["width"], meta["height"]), 32)
        layer = meta["layers"][0]
        self.assertEqual(layer["hips_id"], self.band.hips_id)
        self.assertEqual(layer["fwhm_arcsec"], self.band.fwhm_arcsec)
        covered = np.fromfile(session / "project" / layer["coverage_file"], "u1").reshape(meta["height"], meta["width"])
        self.assertFalse(covered[-5:].any())
        self.assertTrue(covered[:-5].all())
        self.assertTrue((session / "project" / layer["edge_file"]).exists())
        from astroimp.canvas import load, prepare
        with patch("astroimp.surveys.cutout", side_effect=pixels):
            preview = prepare(session, self.root / "downloads")
            self.assertEqual(load(session, preview["id"], self.root / "downloads")["id"], preview["id"])
        self.assertAlmostEqual(preview["footprints"][0]["native_scale_arcsec"], self.band.native_scale_arcsec)

    def test_extra_mast_search_merges_without_resetting_selection(self):
        session = self.root / "sessions" / "one"
        write_json(session / "view.json", self.view)
        write_json(session / "catalog.json", {"groups": surveys.groups(self.view, [self.band])})
        write_json(session / "selection.json", {"obs_ids": [self.obs["obs_id"]]})
        mast = {"groups": [{"collection": "HST", "observations": [{"obs_id": "hst-one"}]}]}
        with patch("astroimp.session.catalog", return_value=mast):
            search_mast(session)
            search_mast(session)
        content = read_json(session / "catalog.json")
        self.assertEqual(len(content["groups"]), 2)
        self.assertTrue(content["mast_searched"])
        self.assertEqual(read_json(session / "selection.json")["obs_ids"], [self.obs["obs_id"]])

    def test_fill_margin_preserves_scale_and_integer_remap_and_tone_fit(self):
        old = surveys.view_grid(self.view, 1, 64)
        new = margin_grid(old, .5)
        self.assertEqual(new["width"], old["width"]*2)
        self.assertEqual(new["pixel_scale_arcsec"], old["pixel_scale_arcsec"])
        mapping = GridMap(old, new)
        np.testing.assert_allclose(mapping.points([[0, 0]]), [[old["width"]/2, math.ceil(old["height"]/2)]])
        self.assertAlmostEqual(grid_view(new)["field_arcmin"][0], old["width"]*2*old["pixel_scale_arcsec"]/60)
        x = np.linspace(.1, .7, 500)
        match = fit_match(x, x*.6+.12)
        self.assertAlmostEqual(match["gain"], .6)
        self.assertAlmostEqual(match["offset"], .12)

    def test_cancellation_does_not_leave_partial_cache(self):
        def cancel(*args):
            raise CancelledError("cancelled")
        grid = surveys.view_grid(self.view, 1, 24)
        with patch("astroimp.surveys.cutout", side_effect=cancel):
            with self.assertRaises(CancelledError):
                surveys.fetch(self.obs, self.root / "downloads", grid)
        self.assertFalse(list(self.root.rglob("*.fits")))
        self.assertFalse(list(self.root.rglob("*.raw")))

    def test_cutout_retries_with_the_mirror_and_full_hips_id(self):
        primary, mirror = Mock(), Mock()
        for client in (primary, mirror):
            client._session.headers = {}
        primary.query_with_wcs.side_effect = TimeoutError("primary timed out")
        mirror.query_with_wcs.return_value = fits.HDUList([
            fits.PrimaryHDU(np.ones((24, 24), np.float32))])
        wcs = WCS(surveys.view_grid(self.view, 1, 24)["wcs"])
        wcs.pixel_shape = (24, 24)
        with patch("astroimp.surveys.hips2fitsClass", side_effect=[primary, mirror]), \
                patch("astroimp.surveys.time.sleep"):
            pixels = surveys.cutout(self.band, wcs)
        self.assertEqual(pixels.shape, (24, 24))
        self.assertEqual(primary.server, surveys.SERVERS[0])
        self.assertEqual(mirror.server, surveys.SERVERS[1])
        self.assertEqual(mirror.timeout, (6, 120))
        self.assertEqual(mirror.query_with_wcs.call_args.args[0], self.band.hips_id)

    def test_fill_adds_matched_bottom_layer_and_checkpoint_without_changing_pixel_scale(self):
        from test_assistant_edits import layer as editor_layer, state as editor_state
        session = self.root / "sessions" / "one"
        downloads = self.root / "downloads"
        downloads.mkdir(parents=True)
        grid = surveys.view_grid(self.view, 1, 24)
        y, x = np.mgrid[:grid["height"], :grid["width"]]
        fits.writeto(downloads / "hst.fits", (x+y).astype(np.float32), WCS(grid["wcs"]).to_header())
        obs = {"obs_id": "hst", "filter": "F656N", "data_uri": "mast:HST/product/hst.fits",
               "collection": "HST", "wavelength_nm": 656.3, "instrument": "WFC3"}
        omitted = {**obs, "obs_id": "hst-omitted", "filter": "F502N"}
        write_json(session / "catalog.json", {"groups": [{"observations": [obs, omitted]}]})
        write_json(session / "selection.json", {"obs_ids": ["hst", "hst-omitted"]})
        before = build(session, downloads=downloads, max_side=24)
        state = editor_state()
        state["layers"] = [editor_layer("subject", "hst")]
        write_json(session / "edits.json", state)
        def pixels(band, wcs, progress):
            y, x = np.mgrid[:wcs.array_shape[0], :wcs.array_shape[1]]
            return (x+y).astype(np.float32)
        with patch("astroimp.surveys.available", return_value=[self.band]), \
                patch("astroimp.surveys.cutout", side_effect=pixels):
            after = fill_around(session, downloads, margin=.5)
        edits = read_json(session / "edits.json")
        self.assertEqual(after["pixel_scale_arcsec"], before["pixel_scale_arcsec"])
        self.assertEqual(edits["layers"][-1]["name"], "IPHAS Hα")
        self.assertIn("toneMatch", edits["layers"][-1])
        self.assertGreater(next(layer for layer in edits["layers"] if layer["source"] == "hst")["edge"]["feather"], 0)
        self.assertFalse(next(layer for layer in edits["layers"] if layer["source"] == "hst-omitted")["visible"])
        self.assertEqual(edits["checkpoints"][-1]["name"], "Before fill")
        self.assertEqual(sum(layer["visible"] for layer in edits["checkpoints"][-1]["state"]["layers"]), 1)
        self.assertEqual({layer["source"] for layer in edits["checkpoints"][-1]["state"]["layers"]},
                         {layer["key"] for layer in after["layers"]})
        self.assertEqual(edits["global"]["crop"], {"x": 0, "y": 0, "w": 1, "h": 1})

    def test_native_export_uses_registry_scale_and_never_requests_finer_samples(self):
        from astroimp.fullres import export_full, plan
        from test_assistant_edits import layer as editor_layer, state as editor_state
        band = surveys.BY_ID["CDS/P/DSS2/red"]
        view = {**self.view, "field_arcmin": [.64, .4], "rotation_deg": 90}
        obs = surveys.groups(view, [band])[0]["observations"][0]
        session = self.root / "sessions" / "one"
        write_json(session / "view.json", view)
        write_json(session / "catalog.json", {"groups": surveys.groups(view, [band])})
        write_json(session / "selection.json", {"obs_ids": [obs["obs_id"]]})
        scales = []
        orientations = []
        def pixels(band, wcs, progress):
            from astroimp.build import pixel_scale
            scales.append(pixel_scale(wcs)*3600)
            orientations.append(wcs.pixel_scale_matrix/pixel_scale(wcs))
            y, x = np.mgrid[:wcs.array_shape[0], :wcs.array_shape[1]]
            return (x+y+.2*np.sin(x)).astype(np.float32)
        with patch("astroimp.surveys.cutout", side_effect=pixels):
            meta = build(session, downloads=self.root / "downloads", max_side=24)
            self.assertGreater(meta["width"], meta["height"])
            self.assertAlmostEqual(grid_view(meta)["rotation_deg"], 90, places=6)
            state = editor_state()
            state["layers"] = [editor_layer("DSS2 red", meta["layers"][0]["key"])]
            write_json(session / "edits.json", state)
            info = plan(session)
            self.assertAlmostEqual(info["native_factor"],
                                   meta["pixel_scale_arcsec"]/band.native_scale_arcsec)
            export_full(session, format="png8", state=state)
            from astroimp.render import Project, render
            preview, _ = render(Project(session / "project"), state)
            self.assertEqual(preview.shape[:2], (meta["height"], meta["width"]))
            from astroimp.contactsheet import make_contact_sheet
            plate = make_contact_sheet(Project(session / "project"), state, "Rotated synthetic field")
            box = json.loads(plate.info["astroimp"])["final_box"]
            self.assertAlmostEqual(box[2]/box[3], meta["width"]/meta["height"], places=2)
        self.assertEqual(len(scales), 2)
        for orientation in orientations:
            np.testing.assert_allclose(orientation, [[0, 1], [1, 0]], atol=1e-10)
        self.assertGreaterEqual(scales[-1], band.native_scale_arcsec*(1-1e-8))
        self.assertLess(scales[-1], scales[0])

    def test_fill_caps_preview_size_and_keeps_the_expanded_sky_region(self):
        old = surveys.view_grid({**self.view, "rotation_deg": -30}, 1, 3072)
        capped = margin_grid(old, 1)
        self.assertEqual(max(capped["width"], capped["height"]), 3072)
        self.assertAlmostEqual(capped["pixel_scale_arcsec"], old["pixel_scale_arcsec"]*3)
        self.assertAlmostEqual(grid_view(capped)["rotation_deg"], -30, places=6)
        mapping = GridMap(old, capped)
        expected = np.array([[0, 0], [capped["width"], capped["height"]]], float)
        corners = [[-old["width"], -old["height"]], [old["width"]*2, old["height"]*2]]
        np.testing.assert_allclose(mapping.points(corners), expected, atol=1e-6)

    def test_tone_sampling_closes_windows_file_mappings_even_on_failure(self):
        self.root.mkdir()
        path = self.root / "sample.f16"
        values = np.arange(192, dtype="<f2").reshape(12, 16)
        values.tofile(path)
        mapped = np.memmap(path, dtype="<f2", mode="r", shape=values.shape)
        with patch("astroimp.fill.np.memmap", return_value=mapped):
            sample = sample_layer(path, "<f2", values.shape, 4)
        self.assertTrue(mapped._mmap.closed)
        np.testing.assert_array_equal(sample, values[::4, ::4])
        mapped = np.memmap(path, dtype="<f2", mode="r", shape=values.shape)
        with patch("astroimp.fill.np.memmap", return_value=mapped), \
                patch("astroimp.fill.np.array", side_effect=RuntimeError("sample failed")):
            with self.assertRaises(RuntimeError):
                sample_layer(path, "<f2", values.shape, 4)
        self.assertTrue(mapped._mmap.closed)

    def test_survey_download_plan_does_not_reuse_another_targets_grid(self):
        downloads = self.root / "downloads"
        first = surveys.view_grid(self.view, 1, 24)
        second = surveys.view_grid({**self.view, "center_icrs_deg": [85.25, -2.46]}, 1, 24)
        image = np.ones((first["height"], first["width"]), np.float32)
        with patch("astroimp.surveys.cutout", return_value=image):
            surveys.fetch(self.obs, downloads, first)
        self.assertIsNone(cached_source(self.obs, downloads))
        self.assertFalse(build_plan([self.obs], downloads)["files"][0]["cached"])
        self.assertFalse(build_plan([self.obs], downloads, grid=second)["files"][0]["cached"])
        updates = []
        def progress(*args):
            pass
        progress.update = lambda **values: updates.append(values)
        with patch("astroimp.surveys.cutout", return_value=image):
            paths = download_all([self.obs], downloads, progress, grid=second)
        self.assertEqual(paths[0], surveys.cache_path(self.obs, downloads, second).resolve())
        states = [item["files"][0]["state"] for item in updates if item.get("files")]
        self.assertEqual(states[0], "queued")
        self.assertIn("downloading", states)
        self.assertTrue(build_plan([self.obs], downloads, grid=first)["files"][0]["cached"])

    def test_assistant_can_adjust_and_clamp_the_visible_tone_match(self):
        from astroimp.edits import apply
        state = {"layers": [{"id": "matched", "name": "IPHAS Hα", "source": "survey",
                              "toneMatch": {"gain": .6, "offset": .12}}]}
        meta = {"layers": [{"key": "survey", "filter": "Hα"}]}
        apply(state, meta, "matched", ["toneMatch.gain=100", "toneMatch.offset=-100"])
        self.assertEqual(state["layers"][0]["toneMatch"], {"gain": 20, "offset": -1})

    def test_state_changing_jobs_block_edit_writes(self):
        import threading
        from fastapi.testclient import TestClient
        from astroimp.server import create_app
        write_json(self.root / "sessions" / "one" / "catalog.json", {"groups": []})
        app = create_app(self.root)
        app.state.sessions.open("one")
        with TestClient(app) as client:
            for kind in ("build", "capture", "canvas", "restore", "search-mast"):
                with self.subTest(kind=kind):
                    release = threading.Event()
                    app.state.jobs.start(kind, lambda progress: release.wait(5), session="one")
                    try:
                        self.assertEqual(client.put("/api/edits", json={}).status_code, 409)
                    finally:
                        release.set()
                        app.state.jobs.thread.join(5)

class SurveyDefectRepairTest(unittest.TestCase):
    def test_fills_chip_gaps_and_bad_columns_but_keeps_sky_structure(self):
        rng = np.random.default_rng(3)
        sky = rng.normal(10, .5, (400, 300)).astype(np.float32)
        yy, xx = np.mgrid[:400, :300]
        sky += 40 * np.exp(-((xx - 200) ** 2 + (yy - 120) ** 2) / 4)           # star
        sky += 3 * np.exp(-((xx - 60) ** 2) / 18) * (yy > 50) * (yy < 350)     # vertical filament
        damaged = sky.copy()
        damaged[180:215, 120:130] = 0                                           # interior chip gap
        damaged[40:360, 250] -= 4                                               # bad column
        damaged[:, :8] = np.nan                                                 # no coverage at border
        repaired = surveys.repair_defects(damaged)
        self.assertLess(np.abs(repaired[180:215, 120:130] - 10).mean(), 1)
        self.assertLess(np.abs(repaired[60:340, 250] - 10).mean(), 1)
        self.assertTrue(np.isnan(repaired[:, :8]).all())
        untouched = np.ones_like(sky, bool)
        untouched[175:220, 115:135] = untouched[:, 248:253] = untouched[:, :8] = False
        np.testing.assert_array_equal(repaired[untouched], damaged[untouched])

    def test_preview_shows_repaired_pixels_north_up(self):
        import tempfile
        from PIL import Image
        view = {"center_icrs_deg": [168.09, -61.24], "field_arcmin": [20, 12]}
        observation = {"hips_id": "CDS/P/VPHAS/DR4/u"}
        def fake(band, wcs, *args, **kwargs):
            h, w = wcs.array_shape
            sky = np.random.default_rng(1).normal(100, 2, (h, w)).astype(np.float32)
            sky[: h // 4] += 80                              # bright south edge (FITS row 0)
            sky[h // 2:h // 2 + 12, w // 2:w // 2 + 4] = 0   # chip gap
            return sky
        with tempfile.TemporaryDirectory() as folder, patch.object(surveys, "cutout", side_effect=fake):
            image = np.asarray(Image.open(surveys.preview(observation, view, Path(folder))).convert("L"), float)
        h, w = image.shape
        gap = image[h - h // 2 - 12:h - h // 2, w // 2:w // 2 + 4]
        sky = image[h // 3:2 * h // 3, : w // 4]
        self.assertGreater(gap.mean(), sky.mean() - 20)      # gap filled, not black
        self.assertGreater(image[-h // 5:].mean(), image[: h // 5].mean() + 40)  # south at bottom


class IndependentSurveyTests(unittest.TestCase):
    def setUp(self):
        import tempfile
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.view = {"center_icrs_deg": [359.99, 20], "field_arcmin": [2, 2],
                     "pixel_dimensions": [64, 64], "rotation_deg": 30}
        self.band = surveys.BY_ID["CDS/P/DSS2/red"]
        self.obs = surveys.groups(self.view, [self.band])[0]["observations"][0]
        health = patch.object(surveys, "_cds_down_until", 0)
        health.start()
        self.addCleanup(health.stop)

    def test_independent_requests_retry_resets_and_remain_cancellable(self):
        response = Mock()
        with patch.object(survey_sources.requests, "get", side_effect=[
                requests.ConnectionError("reset"), response]) as get, \
                patch.object(survey_sources.time, "sleep"):
            self.assertIs(survey_sources.get("https://archive.test"), response)
            self.assertEqual(get.call_count, 2)
        progress = Mock()
        progress.check_cancelled.side_effect = [None, CancelledError("cancelled")]
        with patch.object(survey_sources.requests, "get", side_effect=requests.ConnectionError("reset")) as get:
            with self.assertRaises(CancelledError):
                survey_sources.get("https://archive.test", progress=progress)
            get.assert_called_once()

    def test_outage_discovery_is_cached_separately_and_reports_partial_coverage(self):
        warning = []
        with patch.object(surveys.requests, "get", side_effect=requests.ConnectTimeout("CDS")), \
                patch.object(survey_sources, "available", return_value=([self.band.hips_id], [])) as independent:
            for _ in range(2):
                result = surveys.available(self.view, self.root, warn=warning.append)
                self.assertEqual(result, [self.band])
            independent.assert_called_once()
        self.assertIn("other surveys could not be checked", warning[0])
        self.assertEqual(len(list((self.root / "surveys").glob("*-independent-v1.json"))), 1)
        surveys.note_cds_health(True)
        response = Mock()
        response.json.return_value = [{"ID": "CDS/P/IPHAS/DR2/halpha"}]
        with patch.object(surveys.requests, "get", return_value=response):
            recovered = surveys.available(self.view, self.root)
        self.assertEqual(recovered[0].survey, "IPHAS")
        # A partial archive outage must remain retryable, not freeze missing bands in the cache.
        with patch.object(surveys.requests, "get", side_effect=requests.ConnectTimeout("CDS")), \
                patch.object(survey_sources, "available",
                             return_value=([self.band.hips_id], ["IRSA reset"])) as independent:
            for _ in range(2):
                surveys.available(self.view, self.root / "partial", warn=warning.append)
            self.assertEqual(independent.call_count, 2)

    def test_fallback_restarts_all_windows_and_never_overwrites_legacy_pixels(self):
        grid = surveys.view_grid(self.view, 1, 8)
        cds_calls = 0
        def image(band, wcs, progress, **kwargs):
            nonlocal cds_calls
            if kwargs.get("source", "cds") == "cds":
                cds_calls += 1
                if cds_calls == 2:
                    raise RuntimeError("CDS failed midway")
                return np.full(wcs.array_shape, 10, np.float32)
            return np.full(wcs.array_shape, 20, np.float32)
        original_windows = surveys.windows
        with patch.object(surveys, "windows", side_effect=lambda g: original_windows(g, 4)), \
                patch.object(surveys, "cutout", side_effect=image):
            path = surveys.fetch(self.obs, self.root, grid)
        self.assertEqual(surveys.provenance(path), "stsci-dss-v1")
        with fits.open(path) as hdus:
            np.testing.assert_array_equal(hdus[0].data, 20)
        legacy = surveys.cache_path(self.obs, self.root, grid, source="cds")
        fits.writeto(legacy, np.full((grid["height"], grid["width"]), 30, np.float32))
        self.assertEqual(surveys.fetch(self.obs, self.root, grid), legacy.resolve())
        pinned = {**self.obs, "survey_source": "stsci-dss-v1"}
        self.assertEqual(surveys.fetch(pinned, self.root, grid), path)
        self.assertFalse(list(self.root.rglob("*.raw")))
        self.assertFalse(list(self.root.rglob("*.part")))

    def test_pinned_source_failure_and_cancellation_never_switch_providers(self):
        grid = surveys.view_grid(self.view, 1, 8)
        pinned = {**self.obs, "survey_source": "cds"}
        with patch.object(surveys, "cutout", side_effect=RuntimeError("CDS unreachable")) as cutout:
            with self.assertRaisesRegex(RuntimeError, "CDS unreachable"):
                surveys.fetch(pinned, self.root, grid)
            cutout.assert_called_once()
        with patch.object(surveys, "cutout", side_effect=CancelledError("cancelled")):
            with self.assertRaises(CancelledError):
                surveys.fetch(self.obs, self.root, grid)
        self.assertFalse(list(self.root.rglob("*.fits")))
        self.assertFalse(list(self.root.rglob("*.raw")))

    def test_blank_fallbacks_report_no_coverage_without_caching_a_success(self):
        grid = surveys.view_grid(self.view, 1, 8)
        surveys.note_cds_health(False)
        with patch.object(surveys, "cutout", return_value=np.full((8, 8), np.nan, np.float32)):
            with self.assertRaisesRegex(RuntimeError, "no usable pixels"):
                surveys.fetch(self.obs, self.root, grid)
        self.assertFalse(list(self.root.rglob("*.fits")))
        self.assertFalse(list(self.root.rglob("*.raw")))

    def test_fallback_build_records_and_preserves_provider_on_rebuild(self):
        session = self.root / "sessions" / "one"
        write_json(session / "view.json", self.view)
        write_json(session / "catalog.json", {"groups": surveys.groups(self.view, [self.band])})
        write_json(session / "selection.json", {"obs_ids": [self.obs["obs_id"]]})
        write_json(session / "session.json", {"stars": {"detect": False}})
        def image(band, wcs, progress, **kwargs):
            if kwargs.get("source", "cds") == "cds":
                raise RuntimeError("CDS unreachable")
            return np.arange(np.prod(wcs.array_shape), dtype=np.float32).reshape(wcs.array_shape)
        with patch.object(surveys, "cutout", side_effect=image):
            first = build(session, downloads=self.root / "downloads", max_side=24)
        layer = first["layers"][0]
        self.assertEqual(layer["survey_source"], "stsci-dss-v1")
        grid = {**first, "width": first["width"], "height": first["height"]}
        legacy = surveys.cache_path(self.obs, self.root / "downloads", grid, source="cds")
        fits.writeto(legacy, np.ones((grid["height"], grid["width"]), np.float32))
        with patch.object(surveys, "cutout", side_effect=AssertionError("must reuse the same provider")):
            second = build(session, downloads=self.root / "downloads", max_side=24)
        self.assertEqual(second["layers"][0]["source"], layer["source"])
        self.assertEqual(second["downstream_signature"], first["downstream_signature"])

    def test_native_mosaics_reproject_rotated_ra_wrap_and_keep_real_gaps(self):
        target_grid = surveys.view_grid(self.view, 1, 32)
        target = WCS(target_grid["wcs"])
        target.pixel_shape = (target_grid["width"], target_grid["height"])
        source_grid = surveys.view_grid({**self.view, "rotation_deg": 0, "field_arcmin": [4, 4]}, 1, 128)
        source = WCS(source_grid["wcs"])
        yy, xx = np.mgrid[:128, :128]
        sky = source.pixel_to_world(xx, yy).icrs
        image = np.sin(sky.ra.rad) + np.cos(sky.dec.rad)
        left, right = image.copy(), image.copy()
        left[:, 62:] = np.nan
        right[:, :66] = np.nan
        with patch.object(survey_sources, "native_images",
                          return_value=iter([(left, source), (right, source)])):
            result = survey_sources.native_cutout(self.band, target, lambda *args: None, 6)
        yy, xx = np.mgrid[:result.shape[0], :result.shape[1]]
        sky = target.pixel_to_world(xx, yy).icrs
        expected = np.sin(sky.ra.rad) + np.cos(sky.dec.rad)
        valid = np.isfinite(result)
        self.assertTrue(valid.any())
        self.assertTrue((~valid).any())
        np.testing.assert_allclose(result[valid], expected[valid], atol=1e-6)

    def test_fits_hips_projection_matches_smooth_sky_across_ra_wrap(self):
        band = replace(surveys.BY_ID["CDS/P/allWISE/W1"], hips_order=0)
        hp = HEALPix(nside=512, order="nested", frame="icrs")
        grid = surveys.view_grid({**self.view, "field_arcmin": [180, 180]}, 1, 32)
        target = WCS(grid["wcs"])
        target.pixel_shape = (grid["width"], grid["height"])
        nested = np.array([[0]], np.int64)
        for _ in range(9):
            offset = nested.size
            nested = np.block([[nested, nested+offset], [nested+2*offset, nested+3*offset]])
        tiles = {}
        def tile(url, *args, **kwargs):
            index = int(url.rsplit("Npix", 1)[1].removesuffix(".fits"))
            if index not in tiles:
                lon, lat = hp.healpix_to_lonlat(index*512*512+nested)
                image = (np.cos(lat.rad)*np.sin(lon.rad)+.2*np.sin(lat.rad)).astype(np.float32)
                stream = io.BytesIO()
                fits.PrimaryHDU(image.T[::-1]).writeto(stream)
                tiles[index] = stream.getvalue()
            return tiles[index]
        with patch.object(survey_sources, "cached_fits", side_effect=tile):
            result = survey_sources.hips_cutout(band, target, self.root, lambda *args: None, 6)
        yy, xx = np.mgrid[:result.shape[0], :result.shape[1]]
        sky = target.pixel_to_world(xx, yy).icrs
        expected = np.cos(sky.dec.rad)*np.sin(sky.ra.rad)+.2*np.sin(sky.dec.rad)
        self.assertTrue(np.isfinite(result).all())
        np.testing.assert_allclose(result, expected, atol=1e-5)

    def test_irsa_fits_orientation_agrees_with_published_hpx_astrometry(self):
        # Published HiPS HPX WCS, Norder=3/Npix=448: column=31, row=129.
        # https://hipspy.readthedocs.io/en/latest/drawing_algo.html#wcs-for-fits-tiles
        band = surveys.BY_ID["CDS/P/allWISE/W1"]
        target = WCS(naxis=2)
        target.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        target.wcs.crpix = [1, 1]
        target.wcs.crval = [273.856201171875, -36.818624678015205]
        target.wcs.cdelt = [-.025, .025]
        target.pixel_shape = (1, 1)
        yy, xx = np.mgrid[:512, :512]
        stream = io.BytesIO()
        fits.PrimaryHDU((1000*yy+xx).astype(np.float32)).writeto(stream)
        with patch.object(survey_sources, "cached_fits", return_value=stream.getvalue()) as request:
            result = survey_sources.hips_cutout(band, target, self.root, lambda *args: None, 6)
        self.assertAlmostEqual(float(result[0, 0]), 129031, delta=.25)
        self.assertTrue(all("/Norder3/Dir0/Npix448.fits" in call.args[0]
                            for call in request.call_args_list))

    def test_moc_intersection_decodes_nuniq_and_rejects_bad_metadata(self):
        hp = HEALPix(nside=16, order="nested", frame="icrs")
        center = SkyCoord(359.99, 20, unit="deg")
        pixel = int(hp.skycoord_to_healpix(center))
        moc = fits.BinTableHDU.from_columns([
            fits.Column(name="UNIQ", format="K", array=[4*4**4+pixel])])
        moc.header.update(ORDERING="NUNIQ", COORDSYS="C")
        stream = io.BytesIO()
        fits.HDUList([fits.PrimaryHDU(), moc]).writeto(stream)
        self.assertTrue(survey_sources.moc_intersects(stream.getvalue(), center, .001*u.deg))
        self.assertFalse(survey_sources.moc_intersects(
            stream.getvalue(), SkyCoord(180, -20, unit="deg"), .001*u.deg))
        moc.header["COORDSYS"] = "G"
        stream = io.BytesIO()
        fits.HDUList([fits.PrimaryHDU(), moc]).writeto(stream)
        with self.assertRaisesRegex(ValueError, "unsupported coverage"):
            survey_sources.moc_intersects(stream.getvalue(), center, .001*u.deg)
