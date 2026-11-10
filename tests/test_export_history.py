import copy
import io
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np
from fastapi.testclient import TestClient
from PIL import Image

from astroimp import export_history
from astroimp.builds import archive_current
from astroimp.fullres import export_full, write_png16
from astroimp.png_links import attach, chunk, link
from astroimp.edits import discard_star_state
from astroimp.mast import write_json
from astroimp.server import create_app
from test_fullres import synthetic


class ExportHistoryTests(unittest.TestCase):
    def test_png_link_preserves_rgb16_encoding_and_survives_file_copies_and_clipboard_conversion(self):
        from astroimp.gallery import encode
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "export.png"
            rgb = np.arange(36, dtype=np.uint16).reshape(3, 4, 3) * 1234
            write_png16(image, rgb)
            original = image.read_bytes()
            pointer = link("alpha", "a" * 64)
            attach(image, pointer)
            encoded = image.read_bytes()
            self.assertEqual(encoded, original[:33] + chunk(pointer) + original[33:])
            self.assertEqual(encoded[24], 16)
            attach(image, pointer)
            self.assertEqual(image.read_bytes(), encoded)
            with self.assertRaisesRegex(ValueError, "does not match"):
                attach(image, link("beta", "b" * 64))
            copied = Path(temporary) / "renamed.png"
            copied.write_bytes(encoded)
            with Image.open(copied) as png:
                self.assertEqual(json.loads(png.info["nightbloom"]), pointer)
                png.verify()
            with Image.open(io.BytesIO(encode(copied, "clipboard"))) as clipboard:
                self.assertEqual(clipboard.size, (4, 3))
                self.assertEqual(json.loads(clipboard.info["nightbloom"]), pointer)
            self.assertEqual(copied.read_bytes(), encoded)

    def test_restoring_an_old_build_formalizes_the_working_look_and_preserves_a_way_back(self):
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary)
            original, meta = synthetic(session)
            meta["built_at"] = "2026-10-23T10:00:00+00:00"
            write_json(session / "project" / "meta.json", meta)
            image = session / "exports" / "original.png"
            image.parent.mkdir()
            Image.new("RGB", (4, 3), "blue").save(image)
            identifier = export_history.capture(session, image, original, meta)
            archive_current(session)
            current, current_meta = synthetic(session)
            current_meta["built_at"] = "2026-10-23T11:00:00+00:00"
            write_json(session / "project" / "meta.json", current_meta)
            current["global"]["output"]["exposure"] = .8
            manual = copy.deepcopy(current)
            manual["global"]["output"]["exposure"] = .3
            current["checkpoints"] = [{
                "id": "manual", "name": "Manual look", "created": "2026-10-23",
                "state": manual,
            }]
            write_json(session / "edits.json", current)

            export_history.restore(session, identifier)
            restored = json.loads((session / "edits.json").read_text())
            self.assertEqual(restored["global"], discard_star_state(original)["global"])
            self.assertEqual(json.loads((session / "project" / "meta.json").read_text()), meta)
            manual, returning = restored["checkpoints"]
            self.assertEqual(manual["id"], "manual")
            self.assertEqual(export_history.checkpoint(session, manual["exportCheckpoint"])["meta"], current_meta)
            self.assertEqual(returning["state"]["global"], discard_star_state(current)["global"])

            export_history.restore(session, returning["exportCheckpoint"])
            returned = json.loads((session / "edits.json").read_text())
            self.assertEqual(returned["global"], discard_star_state(current)["global"])
            self.assertEqual(json.loads((session / "project" / "meta.json").read_text()), current_meta)
            self.assertIn("manual", [point["id"] for point in returned["checkpoints"]])

    def test_missing_build_and_changed_image_are_rejected_without_changing_working_edits(self):
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary)
            original, meta = synthetic(session)
            image = session / "exports" / "original.png"
            image.parent.mkdir()
            Image.new("RGB", (4, 3), "blue").save(image)
            identifier = export_history.capture(session, image, original, meta)
            content = image.read_bytes()
            image.write_bytes(content[:-1] + bytes([content[-1] ^ 1]))
            with self.assertRaisesRegex(ValueError, "changed"):
                export_history.image_checkpoint(session, image)
            before = (session / "edits.json").read_bytes()
            (session / "project" / "a.f16").unlink()
            with self.assertRaisesRegex(ValueError, "no longer available"):
                export_history.restore(session, identifier)
            self.assertEqual((session / "edits.json").read_bytes(), before)

    def test_gallery_restore_targets_the_export_project_and_preserves_its_newer_edits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("alpha", "beta"):
                session = root / "sessions" / name
                original, meta = synthetic(session)
                write_json(session / "catalog.json", {"groups": []})
            session = root / "sessions" / "alpha"
            original = json.loads((session / "edits.json").read_text())
            meta = json.loads((session / "project" / "meta.json").read_text())
            image = session / "exports" / "original.png"
            image.parent.mkdir()
            Image.new("RGB", (4, 3), "blue").save(image)
            identifier = export_history.capture(session, image, original, meta)
            newer = copy.deepcopy(original)
            newer["global"]["output"]["exposure"] = .7
            write_json(session / "edits.json", newer)
            app = create_app(root)
            app.state.sessions.open("beta")
            client = TestClient(app)
            albums = client.get("/api/gallery").json()
            self.assertEqual(albums[0]["images"][0]["checkpoint"], identifier)
            response = client.post("/api/gallery/alpha/restore", json={"filename": image.name},
                                   headers={"X-AstroImp-Session": "beta"})
            self.assertEqual(response.status_code, 200, response.text)
            self.assertEqual(response.json()["id"], "alpha")
            restored = response.json()["edits"]
            self.assertEqual(restored["global"], discard_star_state(original)["global"])
            self.assertEqual(restored["checkpoints"][-1]["state"]["global"], discard_star_state(newer)["global"])

    def test_native_size_exports_capture_the_rendered_state_not_later_edits_and_share_the_look(self):
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary)
            exported, meta = synthetic(session)
            newer = copy.deepcopy(exported)
            newer["global"]["output"]["exposure"] = .7
            write_json(session / "edits.json", newer)
            first = export_full(session, state=exported, format="png8")
            second_state = copy.deepcopy(exported)
            second_state.update(selectedLayer="another selection", checkpoints=[{"unrelated": "history"}])
            second = export_full(session, state=second_state, format="jpeg", long_side=16)
            self.assertEqual(first["checkpoint"], second["checkpoint"])
            saved = export_history.checkpoint(session, first["checkpoint"])
            self.assertEqual(saved["state"], exported)
            self.assertEqual(saved["meta"], meta)
            self.assertEqual(json.loads((session / "edits.json").read_text()), newer)
            self.assertEqual(len(list((session / "export-checkpoints").glob("*.json"))), 1)
            history = export_history.records(session)
            for result in (first, second):
                image = Path(result["path"])
                self.assertEqual(history[image.name]["checkpoint"], first["checkpoint"])
                self.assertEqual(history[image.name]["size_bytes"], image.stat().st_size)
            third = export_full(session, state=newer, format="png8")
            self.assertNotEqual(first["checkpoint"], third["checkpoint"])
            rebuilt = {**meta, "built_at": "different build"}
            self.assertNotEqual(export_history.save_checkpoint(session, exported, rebuilt), first["checkpoint"])

    def test_preview_upload_links_its_explicit_snapshot_and_does_not_invent_history_for_legacy_uploads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = root / "sessions" / "alpha"
            original, meta = synthetic(session)
            meta["layers"][0]["source_signature"] = ["source.fits", 2048, 1792732166334304900]
            write_json(session / "project" / "meta.json", meta)
            browser_meta = copy.deepcopy(meta)
            signature = browser_meta["layers"][0]["source_signature"]
            signature[2] = int(float(signature[2]))
            write_json(session / "catalog.json", {"groups": []})
            app = create_app(root)
            app.state.sessions.open("alpha")
            client = TestClient(app)
            response = client.post("/api/export/checkpoint", json={"state": original, "meta": browser_meta})
            self.assertEqual(response.status_code, 200, response.text)
            identifier = response.json()["checkpoint"]
            newer = copy.deepcopy(original)
            newer["global"]["output"]["exposure"] = 1
            write_json(session / "edits.json", newer)
            png = io.BytesIO()
            Image.new("RGB", (4, 3), "blue").save(png, format="PNG")
            saved = client.post("/api/export", content=png.getvalue(), headers={
                "Content-Type": "image/png", "X-Export-Checkpoint": identifier,
            })
            self.assertEqual(saved.status_code, 200, saved.text)
            image = Path(saved.json()["path"])
            with Image.open(image) as saved_png:
                self.assertEqual(json.loads(saved_png.info["nightbloom"]), link("alpha", identifier))
            self.assertEqual(export_history.image_checkpoint(session.resolve(), image), identifier)
            self.assertEqual(export_history.records(session)[image.name]["checkpoint"], identifier)
            self.assertEqual(export_history.checkpoint(session, identifier)["state"], original)
            self.assertEqual(export_history.checkpoint(session, identifier)["meta"], meta)
            wrong_link = png.getvalue()[:33] + chunk(link("beta", identifier)) + png.getvalue()[33:]
            rejected = client.post("/api/export", content=wrong_link, headers={
                "Content-Type": "image/png", "X-Export-Checkpoint": identifier,
            })
            self.assertEqual(rejected.status_code, 400, rejected.text)
            self.assertIn("does not match", rejected.json()["detail"])
            legacy = client.post("/api/export", content=png.getvalue())
            self.assertEqual(legacy.status_code, 200, legacy.text)
            self.assertNotIn(Path(legacy.json()["path"]).name, export_history.records(session))
            self.assertEqual(json.loads((session / "edits.json").read_text()), newer)
            changed = client.post("/api/export/checkpoint", json={
                "state": original, "meta": {**meta, "width": 999},
            })
            self.assertEqual(changed.status_code, 409)
