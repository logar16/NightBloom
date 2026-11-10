import json
import os
import hashlib
import tempfile
import unittest
import socket
import threading
import time
import struct
import zlib
from pathlib import Path
from unittest.mock import patch

import numpy as np
from astropy.io import fits
from astropy.wcs import WCS
from PIL import Image
import requests
import uvicorn

from astroimp.contactsheet import view_transform
from astroimp.fullres import (area_preview, estimate_calibration, estimate_parts, export_full,
                             fit_tone,
                             plan, reproject_image, subgrid)
from astroimp.render import Project, output, render
from astroimp.server import create_app
from astroimp.session import Jobs
from test_assistant_edits import layer, state


def synthetic(session):
    folder = session / "project"
    folder.mkdir(parents=True)
    h, w = 24, 32
    yy, xx = np.mgrid[:h*2, :w*2]
    normalized = .12 + .4*xx/(w*2) + .2*yy/(h*2) + .03*np.sin(xx/6)
    raw = 13 + 7*normalized
    native = WCS(naxis=2)
    native.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    native.wcs.crval = [12, -25]
    native.wcs.crpix = [w+.5, h+.5]
    native.wcs.cd = np.array([[-1/3600, 0], [0, 1/3600]])
    fits.writeto(session / "source.fits", raw.astype(np.float32), native.to_header(), overwrite=True)
    build = WCS(naxis=2)
    build.wcs.ctype = native.wcs.ctype
    build.wcs.crval = native.wcs.crval
    build.wcs.crpix = [w/2+.5, h/2+.5]
    build.wcs.cdelt = [-2/3600, 2/3600]
    preview = normalized.reshape(h,2,w,2).mean((1,3))[::-1].astype("<f2")
    for name in ("a.f16", "a.starless.f16"):
        preview.tofile(folder / name)
    np.ones((h,w), "u1").tofile(folder / "a.coverage.u8")
    np.full((h,w), 1024, "<f2").tofile(folder / "a.edge.f16")
    np.zeros((h,w), "<u2").tofile(folder / "stars.u16")
    (folder / "stars.json").write_text("[]")
    meta = {"width": w, "height": h, "wcs": dict(build.to_header()),
            "stars": {"file": "stars.u16", "list": "stars.json"},
            "layers": [{"key": "a", "filter": "F500W", "wavelength_nm": 500,
                        "source": str(session / "source.fits"), "file": "a.f16",
                        "starless": "a.starless.f16", "coverage_file": "a.coverage.u8",
                        "edge_file": "a.edge.f16"}]}
    (folder / "meta.json").write_text(json.dumps(meta))
    edits = state()
    edits["layers"] = [layer("A", "a", color="#ffeedd")]
    (session / "edits.json").write_text(json.dumps(edits))
    return edits, meta


class FullResolutionTests(unittest.TestCase):
    def test_survey_export_pins_build_provider_including_legacy_cds_layers(self):
        from astroimp import surveys
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, meta = synthetic(session)
            meta["pixel_scale_arcsec"] = 2
            meta["layers"][0]["hips_id"] = "CDS/P/DSS2/red"
            edits["global"]["stars"] = {"color": "layers", "separate": False}
            for provider in ("stsci-dss-v1", None):
                with self.subTest(provider=provider):
                    meta["layers"][0]["survey_source"] = provider
                    (session / "project" / "meta.json").write_text(json.dumps(meta))
                    with patch.object(surveys, "fetch", return_value=session / "source.fits") as fetch:
                        result = export_full(session, state=edits, format="png8")
                    self.assertTrue(Path(result["path"]).is_file())
                    self.assertEqual(fetch.call_args.args[0]["survey_source"], provider or "cds")

    def test_legacy_crop_exports_are_byte_identical_to_pre_anchor_implementation(self):
        # SHA256 of synthetic PNGs captured before the geometry change (same fixture/encoder).
        cases = [(0, False, "85f7f0d1dfb8867afe25a216eb186a0cb8e74751368c9eac01008daec651ecff"),
                 (37, False, "7f9641fd47de537d2204a1f3f94d4ba0cda5101f4a537b6bb17829b058899235"),
                 (90, True, "8b75801f11f57e7b9810ee07c0725bea6c927ff49bd974261a23d3dbf4351541"),
                 (-90, False, "92719d10bf495f1376d4e2a57ac0e7f0a117db5b9959dfaeaf0bfa0e67bee36d")]
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            edits["global"]["stars"] = {"color": "layers", "separate": False}
            for angle, flip, expected in cases:
                edits["global"].update(rotation=angle, flipX=flip, crop={"x": .15, "y": .25, "w": .5, "h": .4})
                result = export_full(session, state=edits, format="png8", scale_factor=1)
                encoded = Path(result["path"]).read_bytes()
                length = struct.unpack(">I", encoded[33:37])[0]
                self.assertEqual(encoded[37:56], b"iTXtnightbloom\0\0\0\0\0")
                # Exclude only the new pointer; every original encoded byte still has to match.
                original = encoded[:33] + encoded[45 + length:]
                self.assertEqual(hashlib.sha256(original).hexdigest(), expected)

    def test_anchored_crop_export_quarter_turn_and_empty_corner_probes(self):
        from astroimp.view import view_frame
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, meta = synthetic(session)
            edits["global"].update(stars={"color": "layers", "separate": False},
                                   crop={"x": .125, "y": .25, "w": .5, "h": .5, "rotation": 0})
            for flip in (False, True):
                edits["global"].update(rotation=0, flipX=flip)
                first = export_full(session, state=edits, format="png8", scale_factor=1)
                original = np.asarray(Image.open(first["path"])).copy()
                for angle in (90, 180, -90, 30):
                    with self.subTest(angle=angle, flip=flip):
                        edits["global"]["rotation"] = angle
                        result = export_full(session, state=edits, format="png8", scale_factor=1)
                        actual = np.asarray(Image.open(result["path"]))
                        b = view_frame(meta, edits["global"])
                        self.assertEqual(actual.shape[:2], tuple(max(1, int(b[k]+.5)) for k in ("height", "width")))
                        if angle != 30:
                            np.testing.assert_array_equal(actual, np.rot90(original, -angle//90))
                        else:
                            for y, x in [(0, 0), (0, actual.shape[1]-1), (actual.shape[0]-1, 0),
                                         (actual.shape[0]-1, actual.shape[1]-1)]:
                                np.testing.assert_array_equal(actual[y, x], [20, 21, 23])
                            # Same source crop centre, with only resampling/rounding differences.
                            np.testing.assert_allclose(actual[actual.shape[0]//2, actual.shape[1]//2],
                                                       original[original.shape[0]//2, original.shape[1]//2], atol=5)

    def test_detail_native_radius_is_scaled_and_tiles_have_no_seams(self):
        from astroimp.fullres import _tiles
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            edits["layers"][0]["detail"] = {"amount": 1.5, "radius": 3}
            edits["global"]["stars"] = {"color": "layers", "separate": False}
            untiled = export_full(session, state=edits, format="png8")
            expected = np.asarray(Image.open(untiled["path"])).copy()
            with patch("astroimp.fullres._tiles", side_effect=lambda shape, size=None: _tiles(shape, 13)):
                tiled = export_full(session, state=edits, format="png8")
            actual = np.asarray(Image.open(tiled["path"]))
            self.assertEqual(expected.shape[:2], (48, 64))
            self.assertLessEqual(np.abs(actual.astype(int)-expected.astype(int)).max(), 1)

    def test_native_layer_clip_remains_in_canvas_coordinates(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            target = edits["layers"][0]
            target.update(invert=True, curve=[{"x": .4, "y": .7}],
                          detail={"amount": 1.5, "radius": 2},
                          transform={"x": .05, "y": -.03, "scale": 1.1, "rotation": 8})
            target["clip"] = {"x": .25, "y": .25, "w": .5, "h": .5, "shape": "ellipse", "feather": .2}
            edits["global"]["stars"] = {"color": "layers", "separate": False}
            result = export_full(session, state=edits, format="png8", name="Pismis 24")
            self.assertEqual(Path(result["path"]).name, "Pismis-24-64x48.png")
            image = np.asarray(Image.open(result["path"]))
            self.assertEqual(image.shape[:2], (48, 64))
            self.assertEqual(int(image[:12].max()), 0)
            self.assertEqual(int(image[:, :16].max()), 0)
            self.assertGreater(int(image[24, 32].max()), 0)

    def test_old_star_state_and_missing_star_assets_do_not_affect_native_exports(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, meta = synthetic(session)
            edits["global"]["stars"] = {"separate": True, "brightness": 0, "reduce": .9, "spikes": 0}
            edits["starOverrides"] = {"1": {"gain": 0}}
            edits["starWeights"] = {"a": 0}
            folder = session / "project"
            (folder / meta["stars"]["file"]).unlink()
            (folder / meta["stars"]["list"]).unlink()
            (folder / "a.starless.f16").unlink()
            for detected in (True, False):
                with self.subTest(detected=detected):
                    meta["stars"]["detect"] = detected
                    (folder / "meta.json").write_text(json.dumps(meta))
                    result = export_full(session, state=edits, format="png8")
                    image = np.asarray(Image.open(result["path"]))
                    self.assertGreater(image.max(), 0)
                    self.assertFalse(list(folder.glob("*star*")))

    def test_native_export_repairs_source_weight_gaps_and_preserves_compact_cores(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            source = session / "source.fits"
            with fits.open(source) as hdus:
                clean = hdus[0].data.copy()
                header = hdus[0].header.copy()
            pixels = clean.copy()
            weight = np.ones_like(pixels)
            gap = np.zeros_like(pixels, dtype=bool)
            gap[8:30, 20] = True
            gap[29, 20:44] = True
            weight[gap] = 0
            pixels[gap] = 1000
            weight[34:37, 45:48] = 0
            pixels[34:37, 45:48] = 5000
            fits.HDUList([fits.PrimaryHDU(pixels, header),
                          fits.ImageHDU(weight, name="WHT")]).writeto(source, overwrite=True)
            with patch("astroimp.fullres.reproject_image", wraps=reproject_image) as reproject:
                export_full(session, state=edits, format="png8")
            repaired = reproject.call_args.args[0]
            np.testing.assert_allclose(repaired[gap], clean[gap], atol=.08)
            np.testing.assert_array_equal(repaired[34:37, 45:48], 5000)

    def test_threaded_reprojection_matches_serial_blocks(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            _, meta = synthetic(session)
            with fits.open(session/"source.fits") as hdus:
                source = np.asarray(hdus[0].data, np.float32)
                source_wcs = WCS(hdus[0].header)
                target = subgrid(WCS(meta["wcs"]),meta["height"],(0,0),(48,64),2)
                from reproject import reproject_interp
                expected = reproject_interp((source,source_wcs),target,shape_out=(48,64),
                                            return_footprint=False,block_size=(17,19))
                actual = np.empty((48,64),np.float32)
                reproject_image(source,source_wcs,target,actual,block_size=17,workers=3)
                np.testing.assert_allclose(actual,expected,atol=1e-6,equal_nan=True)

    def test_cancel_removes_work_maps_and_partial_image(self):
        for phase in ("render", "write", "history"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
                session = Path(directory)
                synthetic(session)
                jobs = Jobs()
                if phase == "render":
                    def cancel_render(*args, **kwargs):
                        result = render(*args, **kwargs)
                        jobs.cancel()
                        return result
                    target, effect = "astroimp.fullres.render", cancel_render
                elif phase == "write":
                    save = Image.Image.save
                    def cancel_save(image, *args, **kwargs):
                        result = save(image, *args, **kwargs)
                        jobs.cancel()
                        return result
                    target, effect = "PIL.Image.Image.save", cancel_save
                else:
                    from astroimp.export_history import capture
                    def cancel_history(*args, **kwargs):
                        result = capture(*args, **kwargs)
                        jobs.cancel()
                        return result
                    target, effect = "astroimp.export_history.capture", cancel_history
                with patch(target, new=effect):
                    jobs.start("export-full", lambda progress: export_full(session, progress=progress))
                    jobs.thread.join(10)
                self.assertFalse(jobs.thread.is_alive())
                self.assertEqual(jobs.payload()["state"], "cancelled")
                self.assertFalse(list(session.glob(".fullres-*")))
                self.assertFalse(list((session / "exports").glob("*")))

    def test_robust_tone_fit_excludes_bright_outliers(self):
        x = np.linspace(0, 1, 1000)
        y = 13 + 7*x
        y[::11] += 20
        a, b = fit_tone(y, x)
        self.assertAlmostEqual(a, 13, places=6)
        self.assertAlmostEqual(b, 7, places=6)

    def test_fractional_area_mean(self):
        raw = np.arange(12*16, dtype=np.float32).reshape(12,16)
        actual = area_preview(raw, [0,0], 2, (6,8))
        np.testing.assert_allclose(actual, raw.reshape(6,2,8,2).mean((1,3)))
        constant = area_preview(np.ones((17,20)), [2,3], 2.5, (9,10))
        np.testing.assert_allclose(constant[np.isfinite(constant)], 1)

    def test_subgrid_preserves_world_coordinates_with_cd_and_pc(self):
        for cd in (True, False):
            wcs = WCS(naxis=2)
            wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
            wcs.wcs.crval = [12,-25]
            wcs.wcs.crpix = [21,31]
            matrix = np.array([[-.001,.0002],[.0001,.001]])
            if cd:
                wcs.wcs.cd = matrix
            else:
                wcs.wcs.pc = matrix
                wcs.wcs.cdelt = [1,1]
            target = subgrid(wcs, 100, (15,21), (40,50), 2.5)
            # Top-left native pixel has build edge coordinates (15.5,21.5)/2.5.
            expected = wcs.pixel_to_world_values(15.5/2.5-.5, 100-21.5/2.5-.5)
            actual = target.pixel_to_world_values(0,39)
            np.testing.assert_allclose(actual, expected, atol=1e-9)

    def test_native_export_tone_parity_and_crop_rotation_dimensions(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, meta = synthetic(session)
            result = export_full(session)
            self.assertEqual((result["width"],result["height"]), (64,48))
            self.assertAlmostEqual(result["tone_fits"][0]["offset"], 13, places=2)
            self.assertAlmostEqual(result["tone_fits"][0]["scale"], 7, places=2)
            native = np.asarray(Image.open(result["path"]), np.float32)/255
            expected, _ = render(Project(session / "project"), edits)
            small = native.reshape(24,2,32,2,3).mean((1,3))
            np.testing.assert_allclose(small, output(expected, edits["global"]), atol=.005)
            edits["global"].update(rotation=31, flipX=True,
                                   crop={"x":.2,"y":.15,"w":.47,"h":.65})
            (session / "edits.json").write_text(json.dumps(edits))
            result = export_full(session, scale_factor=1.5, format="tiff16")
            self.assertEqual((result["width"],result["height"]), (23,24))
            import tifffile
            image = tifffile.imread(result["path"])
            self.assertEqual(image.dtype, np.dtype("uint16"))
            self.assertEqual(image.shape, (24,23,3))
            self.assertFalse(list(session.glob(".fullres-*")))

    def test_scale_one_tiled_transforms_brush_mask_and_rotation_match_preview(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            edits["layers"][0].update(
                transform={"x":.07,"y":-.03,"rotation":12,"scale":1.1},
                brush={"strokes":[{"points":[[12,9],[18,13]],"size":8,
                                  "hardness":.4,"strength":.5,"mode":"hide"},
                                  {"points":[[12,9],[18,13]],"size":8,
                                   "hardness":.4,"strength":.75,"mode":"white"},
                                  {"points":[[14,11]],"size":4,
                                   "hardness":.5,"strength":.25,"mode":"black"}]},
                edge={"feather":3,"irregularity":.5})
            edits["global"].update(rotation=-19, flipX=True,
                                   crop={"x":.2,"y":.25,"w":.6,"h":.5})
            (session / "edits.json").write_text(json.dumps(edits))
            expected, _ = render(Project(session / "project"), edits)
            expected = view_transform(expected, edits["global"], tone_map=True)
            with patch("astroimp.fullres.TILE", 9):
                result = export_full(session, scale_factor=1)
            actual = np.asarray(Image.open(result["path"]), np.float32)/255
            np.testing.assert_allclose(actual, expected, atol=.005)

    def test_hidden_finer_source_does_not_set_native_scale_but_is_included_as_mask(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, meta = synthetic(session)
            with fits.open(session / "source.fits") as hdus:
                wcs = WCS(hdus[0].header)
                wcs.wcs.cdelt /= 2
                fits.writeto(session / "fine.fits", hdus[0].data, wcs.to_header())
            meta["layers"].append({**meta["layers"][0],"key":"b","source":str(session / "fine.fits")})
            (session / "project" / "meta.json").write_text(json.dumps(meta))
            edits["layers"].append(layer("Hidden", "b", visible=False))
            info = plan(session, state=edits)
            self.assertAlmostEqual(info["factor"],2)
            self.assertEqual(info["layers"],["a"])
            edits["layers"][0]["mask"]["source"] = "b"
            self.assertEqual(plan(session,state=edits)["layers"],["a","b"])
            self.assertAlmostEqual(plan(session,state=edits)["factor"],2)
            edits["layers"][0]["mode"] = "spectral"
            self.assertAlmostEqual(plan(session,state=edits)["factor"],4)

    def test_failed_tone_fit_cleans_all_mapped_intermediates(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            synthetic(session)
            with patch("astroimp.fullres.fit_tone", side_effect=ValueError("no overlap")):
                with self.assertRaisesRegex(ValueError,"no overlap"):
                    export_full(session)
            self.assertFalse(list(session.glob(".fullres-*")))

    def test_png16_preserves_composite_precision_and_jpeg_accepts_small_long_side(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            result = export_full(session, format="png16", scale_factor=1)
            data = Path(result["path"]).read_bytes()
            offset, compressed = 8, bytearray()
            while offset < len(data):
                length = struct.unpack(">I",data[offset:offset+4])[0]
                kind = data[offset+4:offset+8]
                payload = data[offset+8:offset+8+length]
                if kind == b"IHDR":
                    self.assertEqual(payload[8],16)
                if kind == b"IDAT":
                    compressed.extend(payload)
                offset += 12+length
            rows = np.frombuffer(zlib.decompress(compressed),np.uint8).reshape(24,1+32*6)
            self.assertTrue(np.all(rows[:,0] == 0))
            decoded = np.frombuffer(rows[:,1:].copy().tobytes(),">u2").reshape(24,32,3)
            self.assertTrue(np.any(decoded % 257 != 0))
            rgb, _ = render(Project(session / "project"),edits)
            np.testing.assert_allclose(decoded.astype(np.float32)/65535,
                                       output(rgb,edits["global"]),atol=.001)
            jpeg = export_full(session,format="jpeg",long_side=16,quality=92)
            with Image.open(jpeg["path"]) as image:
                self.assertEqual(image.format,"JPEG")
                self.assertEqual(image.size,(16,12))
            with self.assertRaises(ValueError):
                export_full(session,long_side=65)
            with self.assertRaises(ValueError):
                export_full(session,long_side=16,scale_factor=1)

    def test_native_export_ignores_legacy_inpainting_overshoot(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            edits, _ = synthetic(session)
            folder = session / "project"
            full = np.fromfile(folder / "a.f16","<f2").reshape(24,32)
            (full.astype(np.float32)+.03).astype("<f2").tofile(folder / "a.starless.f16")
            result = export_full(session)
            rgb, _ = render(Project(folder),edits)
            native = np.asarray(Image.open(result["path"]),np.float32)/255
            np.testing.assert_allclose(native.reshape(24,2,32,2,3).mean((1,3)),
                                       output(rgb,edits["global"]),atol=.005)

    def test_endpoint_job_result_and_conflict_download(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            data = Path(directory)
            session = data / "sessions" / "test"
            synthetic(session)
            (session / "catalog.json").write_text('{"groups":[]}')
            (session.parent / "current.json").write_text('{"id":"test"}')
            app = create_app(data)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1",0))
                port = sock.getsockname()[1]
            server = uvicorn.Server(uvicorn.Config(app,host="127.0.0.1",port=port,log_level="error"))
            thread = threading.Thread(target=server.run,daemon=True)
            thread.start()
            client = requests.Session()
            url = f"http://127.0.0.1:{port}"
            try:
                for _ in range(100):
                    if server.started:
                        break
                    time.sleep(.05)
                self.assertEqual(client.get(url+"/api/export/full/plan").json()["width"],64)
                self.assertEqual(client.post(url+"/api/export/full",json={"scale_factor":0}).status_code,400)
                response = client.post(url+"/api/export/full",json={"format":"png"})
                self.assertEqual(response.status_code,200)
                app.state.jobs.thread.join(timeout=30)
                job = client.get(url+"/api/job").json()
                self.assertEqual(job["state"],"done",job.get("error"))
                filename = Path(job["result"]["path"]).name
                self.assertEqual(client.get(url+"/api/export/full/download/"+filename).status_code,200)
                self.assertEqual(client.get(url+"/api/export/full/download/other.png").status_code,404)
                self.assertEqual(client.post(url+"/api/export/full",json={"quality":101}).status_code,400)
                response = client.post(url+"/api/export/full",json={"format":"jpeg","long_side":16,"quality":85})
                self.assertEqual(response.status_code,200)
                app.state.jobs.thread.join(timeout=30)
                result = client.get(url+"/api/job").json()["result"]
                download = client.get(url+result["download_url"])
                self.assertTrue(download.headers["content-type"].startswith("image/jpeg"))
                app.state.jobs.job.state = "running"
                self.assertEqual(client.post(url+"/api/export/full",json={}).status_code,409)
            finally:
                client.close()
                server.should_exit = True
                thread.join(timeout=10)
            self.assertFalse(list(session.glob(".fullres-*")))


class ExportNameTest(unittest.TestCase):
    def test_project_name_and_resolution_without_overwriting(self):
        from astroimp.fullres import export_path
        with tempfile.TemporaryDirectory() as folder:
            first = export_path(Path(folder), "IC 2177: Seagull", 4096, 2410, "png")
            self.assertEqual(first.name, "IC-2177-Seagull-4096x2410.png")
            first.touch()
            self.assertEqual(export_path(Path(folder), "IC 2177: Seagull", 4096, 2410, "png").name,
                             "IC-2177-Seagull-4096x2410-2.png")


if __name__ == "__main__":
    unittest.main()


class ExportEstimateTest(unittest.TestCase):
    def test_source_work_is_fixed_and_area_work_scales(self):
        layer = {"collection": "HST", "tiles": [{"source": "missing-a.fits"}, {"source": "missing-b.fits"}]}
        small = estimate_parts([layer], 1_000_000)
        large = estimate_parts([layer], 4_000_000)
        self.assertEqual(small[0], large[0])
        self.assertGreater(small[0], 0)
        self.assertAlmostEqual(large[1] / small[1], 4)

    def test_calibration_uses_recent_median_and_ignores_legacy_sidecars(self):
        with tempfile.TemporaryDirectory() as directory:
            exports = Path(directory) / "s" / "exports"
            exports.mkdir(parents=True)
            (exports / "legacy.json").write_text(json.dumps({"wall_s": 999}))
            for index, ratio in enumerate([10, 10, 1, 2, 2, 2, 3, 3, 3, 3]):
                path = exports / f"{index}.json"
                path.write_text(json.dumps({"wall_s": 50 * ratio, "estimate": {"model_s": 50}}))
                os.utime(path, (index, index))
            # The two oldest outliers fall outside the eight-export window.
            self.assertEqual(estimate_calibration(Path(directory)), 2.5)
            self.assertEqual(estimate_calibration(Path(directory) / "empty"), 1.)
