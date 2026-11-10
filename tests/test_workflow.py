from __future__ import annotations

import base64
import hashlib
import io
import json
import math
import struct
import tempfile
import unittest
from contextlib import redirect_stdout
from dataclasses import asdict, replace
from pathlib import Path
from unittest.mock import MagicMock, patch

import numpy as np
from astropy.io import fits
from astropy.table import Table
from astropy.wcs import WCS
from pyvo.samp import SAMPHubServer, SAMPIntegratedClient

from astroimp.aladin import read_view, snapshot_geometry
from astroimp.cli import parser, run
from astroimp.datasets import (
    Framing, SQ_ARCMIN_PER_STERADIAN, SkyFootprint, footprint,
    filter_oversized_groups, group_datasets, parse_footprint, size_status, spectral_filter,
)
from astroimp.mast import Product, download, search, select_products
from astroimp.prepare import prepare
from astroimp.samp import AladinListener


class WorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def aladin_snapshot(self) -> str:
        wcs = WCS(naxis=2)
        wcs.wcs.crpix = [6.5, 3.5]
        wcs.wcs.crval = [359.99, 20]
        angle = math.radians(30)
        wcs.wcs.cd = [[-.01 * math.cos(angle), -.02 * math.sin(angle)],
                      [-.01 * math.sin(angle), .02 * math.cos(angle)]]
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        header = fits.Header({"SIMPLE": True, "BITPIX": 8, "NAXIS": 2,
                              "NAXIS1": 13, "NAXIS2": 7})
        header.update(wcs.to_header())
        header.rename_keyword("RADESYS", "RADECSYS")
        comment = header.tostring(sep="\n", endcard=False, padding=False).encode("ascii")
        dimensions = struct.pack(">BHHB", 8, 6, 12, 1)
        image = (b"\xff\xd8\xff\xfe" + struct.pack(">H", len(comment) + 2) + comment
                 + b"\xff\xc0" + struct.pack(">H", len(dimensions) + 2) + dimensions
                 + b"\xff\xda")
        encoded = base64.b64encode(image).decode("ascii")
        return "data:image/jpeg;base64," + " \n".join(
            encoded[i:i + 40] for i in range(0, len(encoded), 40)
        )

    def test_refreshing_the_aladin_image_reprojects_footprints_only_for_the_same_area(self) -> None:
        from astroimp.aladin import parse_snapshot
        from astroimp.session import refresh_snapshot

        snapshot = parse_snapshot(self.aladin_snapshot())
        session = self.root / "session"
        session.mkdir()
        view = asdict(snapshot.view)
        (session / "view.json").write_text(json.dumps(view))
        center = snapshot.view.center_icrs_deg
        region = f"POLYGON {center[0]} {center[1]} {center[0] + .01} {center[1]} {center[0]} {center[1] + .01}"
        catalog = {"groups": [{"observations": [{"obs_id": "a", "region": region, "pixels": []}]}]}
        (session / "catalog.json").write_text(json.dumps(catalog))
        with patch("astroimp.session.aladin.read_snapshot", return_value=snapshot):
            refresh_snapshot(session)
        pixels = json.loads((session / "catalog.json").read_text())["groups"][0]["observations"][0]["pixels"]
        self.assertEqual(len(pixels), 1)
        width, height = snapshot.view.pixel_dimensions
        self.assertAlmostEqual(pixels[0][0][0], (width - 1) / 2, delta=1)
        self.assertAlmostEqual(pixels[0][0][1], (height - 1) / 2, delta=1)
        self.assertEqual((session / "snapshot.jpg").read_bytes(), snapshot.jpeg)

        moved = replace(snapshot, view=replace(snapshot.view, center_icrs_deg=(center[0], center[1] + 1)))
        with patch("astroimp.session.aladin.read_snapshot", return_value=moved), \
                self.assertRaises(LookupError):
            refresh_snapshot(session)

    def test_recapture_replaces_an_unbuilt_view_and_locks_after_build(self) -> None:
        from astroimp.aladin import parse_snapshot
        from astroimp.session import recapture

        snapshot = parse_snapshot(self.aladin_snapshot())
        session = self.root / "sessions" / "s"
        session.mkdir(parents=True)
        (session / "view.json").write_text(json.dumps({"center_icrs_deg": [1, 2], "field_arcmin": [60, 30]}))
        (session / "selection.json").write_text(json.dumps({"obs_ids": ["kept", "gone"]}))
        (session / "session.json").write_text(json.dumps({"name": "Old target", "auto_named": True}))
        found = {"groups": [{"collection": "HST", "observations": [{"obs_id": "kept"}]}]}
        with patch("astroimp.session.aladin.read_snapshot", return_value=snapshot), \
                patch("astroimp.session.search_view", return_value=found), \
                patch("astroimp.session.auto_name", return_value="New target"):
            recapture(session)
        self.assertEqual(json.loads((session / "view.json").read_text())["center_icrs_deg"],
                         list(snapshot.view.center_icrs_deg))
        self.assertEqual(json.loads((session / "selection.json").read_text())["obs_ids"], ["kept"])
        self.assertEqual(json.loads((session / "session.json").read_text())["name"], "New target")

        (session / "session.json").write_text(json.dumps({"name": "My Carina", "auto_named": False}))
        with patch("astroimp.session.aladin.read_snapshot", return_value=snapshot), \
                patch("astroimp.session.search_view", return_value=found):
            recapture(session)
        self.assertEqual(json.loads((session / "session.json").read_text())["name"], "My Carina")

        (session / "project").mkdir()
        (session / "project" / "meta.json").write_text("{}")
        with self.assertRaises(LookupError):
            recapture(session)

    def test_aladin_wcs_readback_handles_rotation_wrap_and_actual_pixel_bounds(self) -> None:
        view = snapshot_geometry(self.aladin_snapshot())
        self.assertEqual(view.pixel_dimensions, (12, 6))
        self.assertEqual(view.header_pixel_dimensions, (13, 7))
        self.assertAlmostEqual(view.center_icrs_deg[0], 359.99, places=10)
        self.assertAlmostEqual(view.center_icrs_deg[1], 20, places=10)
        expected = math.degrees(2 * math.atan(math.radians(.12) / 2)) * 60
        np.testing.assert_allclose(view.field_arcmin, [expected, expected], atol=1e-9)
        self.assertTrue(any(ra < 1 for ra, _ in view.corners_icrs_deg))
        self.assertTrue(any(ra > 359 for ra, _ in view.corners_icrs_deg))
        self.assertGreater(view.corners_icrs_deg[2][1], view.corners_icrs_deg[0][1])

    def test_aladin_snapshot_rejects_missing_or_truncated_metadata(self) -> None:
        prefix = "data:image/jpeg;base64,"
        for value, message in (
            ("not a snapshot", "unexpected snapshot format"),
            (prefix + base64.b64encode(b"not JPEG").decode(), "not a JPEG"),
            (prefix + base64.b64encode(b"\xff\xd8\xff\xd9").decode(), "lacks FITS/WCS"),
            (prefix + base64.b64encode(b"\xff\xd8\xff\xfe\x00").decode(), "Truncated JPEG"),
            (prefix + base64.b64encode(b"\xff\xd8\xff\xfe\x00\x10x").decode(),
             "Invalid JPEG segment length"),
        ):
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                snapshot_geometry(value)

    def test_view_readback_surfaces_ambiguous_clients_and_failed_snapshots(self) -> None:
        for recipients, response, message in (
            ({}, {}, "Expected one SAMP-connected Aladin"),
            ({"a": {}, "b": {}}, {"samp.status": "samp.ok"}, "Expected one SAMP-connected Aladin"),
            ({"a": {}}, {"samp.status": "samp.error"}, "Aladin snapshot failed"),
            ({"a": {}}, {"samp.status": "samp.ok", "samp.result": {}}, "missing its image data"),
        ):
            client = MagicMock()
            client.get_subscribed_clients.return_value = recipients
            client.get_metadata.return_value = {"samp.name": "Aladin"}
            client.call_and_wait.return_value = response
            with self.subTest(message=message, recipients=len(recipients)), \
                    patch("astroimp.aladin.SAMPIntegratedClient", return_value=client), \
                    self.assertRaisesRegex(RuntimeError, message):
                read_view()

    def test_only_public_combined_images_with_parent_metadata_and_deduplication(self) -> None:
        observations = Table(rows=[{
            "obsid": "123", "obs_collection": "JWST", "instrument_name": "NIRCAM/IMAGE",
            "filters": "F200W", "obs_id": "field", "proposal_id": "42", "s_region": "POLYGON ICRS",
        }])
        base = {
            "productFilename": "mosaic_i2d.fits", "productType": "SCIENCE", "calib_level": 3,
            "dataRights": "PUBLIC", "parent_obsid": "123", "obsID": "999",
            "dataURI": "mast:JWST/mosaic_i2d.fits", "size": 1234,
        }
        candidates = Table(rows=[
            base, base,
            {**base, "productFilename": "exposure_i2d.fits", "calib_level": 2},
            {**base, "productFilename": "image_raw.fits"},
            {**base, "dataRights": "EXCLUSIVE_ACCESS"},
            {**base, "productType": "PREVIEW"},
        ])
        products = select_products(candidates, {"123": observations[0]})
        self.assertEqual(len(products), 1)
        self.assertEqual((products[0].filters, products[0].observation), ("F200W", "field"))

    def test_empty_search_writes_manifest_without_product_query(self) -> None:
        from astroimp.mast import sky_position

        with patch("astroimp.mast.Observations") as archive:
            archive.query_criteria.return_value = Table()
            path = self.root / "products.json"
            self.assertEqual(search(sky_position(10, -20), path, radius_arcmin=0.01), [])
            archive.get_product_list.assert_not_called()
            self.assertEqual(json.loads(path.read_text())["products"], [])
            self.assertAlmostEqual(archive.query_criteria.call_args.kwargs["radius"].value, 0.01)

    def test_related_product_uses_its_own_filter_and_footprint_not_parent_detection(self) -> None:
        observations = Table(rows=[
            {"obsid": "1", "obs_collection": "HST", "instrument_name": "WFC3/IR",
             "filters": "detection", "obs_id": "total", "proposal_id": "42", "s_region": "parent"},
            {"obsid": "2", "obs_collection": "HST", "instrument_name": "WFC3/IR",
             "filters": "F160W", "obs_id": "idnm20010", "proposal_id": "42", "s_region": "own"},
        ])
        candidates = Table(rows=[{
            "productFilename": "idnm20010_drz.fits", "productType": "SCIENCE", "calib_level": 3,
            "dataRights": "PUBLIC", "parent_obsid": "1", "obsID": "2",
            "obs_id": "idnm20010", "filters": "F160W", "dataURI": "mast:HST/image", "size": 100,
        }])
        metadata = {str(row["obsid"]): row for row in observations}
        product = select_products(candidates, metadata)[0]
        self.assertEqual((product.filters, product.footprint, product.footprint_source),
                         ("F160W", "own", "product"))
        fallback = select_products(candidates, {"1": observations[0]})[0]
        self.assertEqual(fallback.filters, "F160W")
        self.assertIsNone(footprint(fallback))

    def dataset_product(self, filename: str) -> Product:
        return Product(
            filename, "mast:HST/" + filename, "HST", "WFC3/UVIS", "F502N",
            filename.removesuffix("_drc.fits").removesuffix("_drz.fits"), "13926", 100,
            "POLYGON ICRS 359 -1 1 -1 1 1 359 1", "product",
        )

    def dataset_field(self, visit: str, region: str | None = None) -> list[Product]:
        products = [
            replace(self.dataset_product(f"ick9{visit}{number}_drc.fits"), filters=label)
            for number, label in (("030", "F502N"), ("040", "F657N"))
        ]
        return [replace(p, footprint=region) for p in products] if region is not None else products

    def test_view_size_filter_hides_only_with_two_good_fields_and_preserves_explicit_access(self) -> None:
        products = (
            self.dataset_field("05") + self.dataset_field("06")
            + self.dataset_field("07", "POLYGON 357 -3 3 -3 3 3 357 3")
            + self.dataset_field("08", "")
        )
        framing = Framing(0, 0, (60, 60))
        groups = group_datasets(products)
        oversized = next(g for g in groups if g.field == "visit-07")
        unknown = next(g for g in groups if g.field == "visit-08")
        visible, good_count, oversized_count = filter_oversized_groups(groups, framing)
        self.assertEqual((good_count, oversized_count), (2, 1))
        self.assertEqual([g.identifier for g in visible],
                         [g.identifier for g in groups if g != oversized])
        self.assertIn(unknown, visible)
        self.assertEqual(filter_oversized_groups(groups, framing, show_all=True)[0], groups)
        self.assertEqual(filter_oversized_groups(groups, Framing(0, 0))[0], groups)
        path = self.root / "products.json"
        path.write_text(json.dumps({
            "version": 1, "ra_deg": 0, "dec_deg": 0, "field_arcmin": [60, 60],
            "products": [asdict(p) for p in products],
        }))
        args = parser().parse_args([
            "fetch", str(path), "--group", oversized.identifier, "--out", str(self.root / "session"),
        ])
        with patch("astroimp.cli.download", return_value=self.root / "image.fits") as fetch, \
                patch("astroimp.cli.prepare"), redirect_stdout(io.StringIO()) as output:
            run(args)
        self.assertEqual(fetch.call_count, 2)

    def test_sparse_fallback_does_not_count_reductions_unknowns_or_inadequate_fields(self) -> None:
        reductions = [
            self.dataset_product("hst_13926_05_wfc3_uvis_f502n_ick905_drc.fits"),
            self.dataset_product("hst_13926_05_wfc3_uvis_f657n_ick905_drc.fits"),
        ]
        products = (
            self.dataset_field("05") + reductions
            + self.dataset_field("07", "POLYGON 357 -3 3 -3 3 3 357 3")
            + self.dataset_field("08", "")
            + self.dataset_field("09", "POLYGON 100 -1 102 -1 102 1 100 1")
            + self.dataset_field("10", "POLYGON 359.9 -0.1 0.1 -0.1 0.1 0.1 359.9 0.1")
            + self.dataset_field("11")[:1]
        )
        groups = group_datasets(products)
        framing = Framing(0, 0, (60, 60))
        self.assertEqual(filter_oversized_groups(groups, framing), (groups, 1, 1))

    def test_size_boundary_and_partly_unknown_sets(self) -> None:
        products = self.dataset_field("05")
        selected = list(enumerate(products, 1))
        image_area = footprint(products[0]).area * SQ_ARCMIN_PER_STERADIAN
        for ratio, expected in ((10, (False, True)), (10.000001, (True, False))):
            with self.subTest(ratio=ratio), patch.object(Framing, "area", image_area / ratio):
                self.assertEqual(size_status(selected, Framing(0, 0, (60, 60))), expected)
        partial = [selected[0], (2, replace(products[1], footprint=""))]
        framing = Framing(0, 0, (1, 1))
        self.assertEqual(size_status(partial, framing), (True, False))
        self.assertEqual(size_status([(2, partial[1][1])], framing), (False, False))
        self.assertEqual(size_status(selected, Framing(0, 0)), (False, False))

    def test_sets_keep_fields_and_mosaic_families_separate_and_preserve_alternatives(self) -> None:
        products = [
            self.dataset_product("ick905030_drz.fits"),
            self.dataset_product("ick905030_drc.fits"),
            replace(self.dataset_product("ick905040_drc.fits"), filters="F657N"),
            self.dataset_product("ick906030_drc.fits"),
            self.dataset_product("hst_13926_05_wfc3_uvis_f502n_ick905_drc.fits"),
            self.dataset_product("hst_13926_05_wfc3_uvis_total_ick905_drc.fits"),
        ]
        groups = group_datasets(products)
        self.assertEqual(len(groups), 3)
        group = next(g for g in groups if g.family == "Standard" and g.field == "visit-05")
        self.assertEqual(len(group.members), 3)
        self.assertEqual([index for index, _ in group.suggested(Framing(0, 0))], [2, 3])
        self.assertIsNone(spectral_filter(products[-1]))

    def test_spherical_areas_and_center_coverage_wrap_poles_and_concave_edges(self) -> None:
        octant = parse_footprint("POLYGON 0 0 90 0 0 90")
        self.assertIsNotNone(octant)
        self.assertAlmostEqual(octant.area, math.pi / 2, places=12)
        self.assertTrue(octant.contains_radec(45, 45))
        self.assertFalse(octant.contains_radec(180, 0))
        reversed_octant = parse_footprint("POLYGON 0 90 90 0 0 0")
        self.assertAlmostEqual(reversed_octant.area, octant.area, places=12)
        wrapped = parse_footprint("POLYGON 359 -1 1 -1 1 1 359 1 359 -1 359 -1")
        self.assertTrue(wrapped.contains_radec(0, 0))
        self.assertFalse(wrapped.contains_radec(180, 0))
        self.assertAlmostEqual(wrapped.area * SQ_ARCMIN_PER_STERADIAN, 4 * 60**2, delta=2)
        self.assertAlmostEqual(Framing(0, 0, (2, 2)).area, 4, places=5)

        def plane_polygon(points: list[tuple[float, float]]) -> SkyFootprint:
            x, y = np.asarray(points).T
            ra = np.degrees(np.arctan(x))
            dec = np.degrees(np.arctan2(y, np.sqrt(1 + x * x)))
            return SkyFootprint(np.column_stack((ra, dec)))

        concave = plane_polygon([(0, 0), (.02, 0), (.02, .01), (.01, .01), (.01, .02), (0, .02)])
        left = plane_polygon([(0, 0), (.01, 0), (.01, .02), (0, .02)])
        bottom_right = plane_polygon([(.01, 0), (.02, 0), (.02, .01), (.01, .01)])
        self.assertAlmostEqual(concave.area, left.area + bottom_right.area, places=12)
        for x, y, expected in [(.005, .015, True), (.015, .015, False)]:
            point = plane_polygon([(x, y)]).coordinates[0]
            self.assertEqual(concave.contains_radec(*point), expected)

    def test_requested_framing_prefers_room_over_a_smaller_same_filter_alternative(self) -> None:
        large = self.dataset_product("ick905030_drc.fits")
        small = replace(
            self.dataset_product("ick905031_drc.fits"), size_bytes=1,
            footprint="POLYGON -0.008333 -0.008333 0.008333 -0.008333 "
                      "0.008333 0.008333 -0.008333 0.008333",
        )
        group = group_datasets([large, small])[0]
        self.assertEqual(group.suggested(Framing(0, 0))[0][0], 2)
        self.assertEqual(group.suggested(Framing(0, 0, (2, 2)))[0][0], 1)
        suitable = replace(
            self.dataset_product("ick905032_drz.fits"), size_bytes=1000,
            footprint="POLYGON -0.025 -0.025 0.025 -0.025 0.025 0.025 -0.025 0.025",
        )
        group = group_datasets([large, small, suitable])[0]
        self.assertEqual(group.suggested(Framing(0, 0, (2, 2)))[0][0], 3)

    def test_group_fetch_downloads_only_requested_filters_and_rejects_unknown_labels(self) -> None:
        products = [
            self.dataset_product("ick905030_drc.fits"),
            replace(self.dataset_product("ick905040_drc.fits"), filters="F657N"),
        ]
        path = self.root / "products.json"
        path.write_text(json.dumps({
            "version": 1, "ra_deg": 0, "dec_deg": 0, "products": [asdict(p) for p in products],
        }))
        args = parser().parse_args([
            "fetch", str(path), "--group", "G01", "--filters", "F657N", "--out", str(self.root / "session"),
        ])
        with patch("astroimp.cli.download", return_value=self.root / "image.fits") as fetch, \
                patch("astroimp.cli.prepare"):
            run(args)
            self.assertEqual(fetch.call_count, 1)
            self.assertEqual(fetch.call_args.args[0].filters, "F657N")
            args.filters = ["F999N"]
            with self.assertRaisesRegex(ValueError, "Unknown filters"):
                run(args)
            self.assertEqual(fetch.call_count, 1)

    def science_file(self, name: str = "input_i2d.fits", *, chips: int = 1) -> Path:
        path = self.root / name
        wcs = WCS(naxis=2)
        wcs.wcs.crpix = [2, 2]
        wcs.wcs.crval = [83.8, -5.4]
        wcs.wcs.cdelt = [-0.001, 0.001]
        wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        primary = fits.PrimaryHDU(header=fits.Header({
            "FILTER": "F444W", "PUPIL": "F470N", "TELESCOP": "JWST", "INSTRUME": "NIRCAM",
        }))
        science = [fits.ImageHDU(np.arange(12, dtype=np.float32).reshape(3, 4) + i,
                                 header=wcs.to_header(), name="SCI") for i in range(chips)]
        fits.HDUList([primary, fits.ImageHDU(np.ones((3, 4)) * -99, name="ERR"),
                      *science]).writeto(path)
        return path

    def test_science_extraction_preserves_pixels_wcs_filters_and_original(self) -> None:
        source = self.science_file()
        before = hashlib.sha256(source.read_bytes()).digest()
        manifest = prepare([source], self.root / "prepared")
        layers = json.loads(manifest.read_text())["layers"]
        self.assertEqual(layers[0]["filter"], "F444W+F470N")
        with fits.open(manifest.parent / layers[0]["file"], checksum=True) as hdus:
            self.assertEqual(len(hdus), 1)
            np.testing.assert_array_equal(hdus[0].data, np.arange(12).reshape(3, 4))
            np.testing.assert_allclose(WCS(hdus[0].header).pixel_to_world_values(1, 1), (83.8, -5.4))
            self.assertEqual(hdus[0].header["TELESCOP"], "JWST")
            self.assertEqual(hdus[0].verify_checksum(), 1)
        self.assertEqual(hashlib.sha256(source.read_bytes()).digest(), before)
        prepare([source], manifest.parent)
        self.assertEqual(len(json.loads(manifest.read_text())["layers"]), 1)

    def test_all_science_chips_are_extracted_but_cubes_are_rejected(self) -> None:
        manifest = prepare([self.science_file(chips=2)], self.root / "chips")
        self.assertEqual(len(json.loads(manifest.read_text())["layers"]), 2)
        cube = self.root / "cube.fits"
        fits.PrimaryHDU(np.ones((2, 3, 4))).writeto(cube)
        with self.assertRaisesRegex(ValueError, "2D science"):
            prepare([cube], self.root / "cube-output")

    def test_duplicate_source_filenames_do_not_silently_replace_layers(self) -> None:
        source = self.science_file()
        other = self.root / "other" / source.name
        other.parent.mkdir()
        other.write_bytes(source.read_bytes())
        with self.assertRaisesRegex(ValueError, "collision"):
            prepare([source, other], self.root / "prepared")

    def test_download_is_atomic_checked_and_reuses_complete_files(self) -> None:
        content = self.science_file().read_bytes()
        product = Product("image_i2d.fits", "mast:JWST/image_i2d.fits", "JWST",
                          "NIRCAM", "F200W", "field", "42", len(content), "")
        response = MagicMock()
        response.__enter__.return_value = response
        response.iter_content.return_value = [content[:100]]
        with patch("astroimp.mast.requests.get", return_value=response):
            with self.assertRaisesRegex(ValueError, "Incomplete download"):
                download(product, self.root / "originals")
        destination = self.root / "originals" / product.filename
        self.assertFalse(destination.exists())
        self.assertFalse(destination.with_name(destination.name + ".part").exists())
        response.iter_content.return_value = [content]
        with patch("astroimp.mast.requests.get", return_value=response) as get:
            self.assertEqual(download(product, destination.parent), destination)
            self.assertEqual(download(product, destination.parent), destination)
            self.assertEqual(get.call_count, 1)
        with self.assertRaisesRegex(ValueError, "Unsafe"):
            download(replace(product, filename="..\\escape.fits"), destination.parent)

    def test_live_samp_roundtrip_calls_notifications_and_bad_coordinates(self) -> None:
        hub = SAMPHubServer(addr="127.0.0.1", web_profile=False,
                            lockfile=str(self.root / "samp-lock"), mode="multiple")
        hub.start()
        self.addCleanup(hub.stop)
        listener = AladinListener()
        listener.connect(hub)
        self.addCleanup(listener.disconnect)
        sender = SAMPIntegratedClient(name="Aladin test sender", addr="127.0.0.1")
        sender.connect(hub=hub)
        self.addCleanup(sender.disconnect)
        message = {"samp.mtype": "coord.pointAt.sky", "samp.params": {"ra": "83.8", "dec": "-5.4"}}
        response = sender.call_and_wait(listener.client.get_public_id(), message, "5")
        self.assertEqual(response["samp.status"], "samp.ok")
        coord = listener.next_position(timeout=2)
        self.assertAlmostEqual(coord.ra.deg, 83.8)
        sender.notify(listener.client.get_public_id(), message)
        self.assertAlmostEqual(listener.next_position(timeout=2).dec.deg, -5.4)
        message["samp.params"]["dec"] = "nan"
        response = sender.call_and_wait(listener.client.get_public_id(), message, "5")
        self.assertEqual(response["samp.status"], "samp.error")
        self.assertTrue(listener.positions.empty())


if __name__ == "__main__":
    unittest.main()
