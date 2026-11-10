import json
import tempfile
import unittest
from pathlib import Path

import numpy as np
from PIL import Image

from astroimp.contactsheet import export_contact_sheet, make_contact_sheet, view_transform
from astroimp.export_history import checkpoint, records
from astroimp.render import Project, output, render
from test_assistant_edits import layer, state


class ContactSheetTests(unittest.TestCase):
    def test_png_visible_rows_and_final_panel_match_transformed_render(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            session = Path(directory)
            folder = session / "project"
            folder.mkdir()
            h, w = 12, 12
            data = np.arange(h * w, dtype=np.float32).reshape(h, w) / (h * w)
            for filename in ("a.f16", "a.starless.f16"):
                data.astype("<f2").tofile(folder / filename)
            np.zeros((h, w), "<u2").tofile(folder / "stars.u16")
            (folder / "stars.json").write_text("[]")
            (folder / "meta.json").write_text(json.dumps({
                "width": w, "height": h, "stars": {"file": "stars.u16", "list": "stars.json"},
                "layers": [{"key": "a", "filter": "F502N", "wavelength_nm": 502,
                            "file": "a.f16", "starless": "a.starless.f16"}]}))
            edits = state()
            edits["layers"] = [layer("Top", "a", color="#ff8000"),
                               layer("Hidden", "a", visible=False),
                               layer("Bottom", "a", color="#003344")]
            edits["global"].update(rotation=90, flipX=True, crop={"x": .25, "y": 0, "w": .5, "h": .75})
            edits["global"]["output"].update(black=.04, white=.8, midtones=.7)
            project = Project(folder)
            plate = make_contact_sheet(project, edits, "Synthetic observatory plate")
            metadata = json.loads(plate.info["astroimp"])
            self.assertEqual(plate.size, (3600, 2025))
            self.assertEqual(metadata["visible_layers"], ["top", "bottom"])
            acc, _ = render(project, edits)
            expected = view_transform(acc, edits["global"], tone_map=True)
            x, y, pw, ph = metadata["final_box"]
            expected_image = Image.fromarray(np.round(expected * 255).astype("uint8")).resize(
                (pw, ph), Image.Resampling.LANCZOS)
            np.testing.assert_array_equal(np.array(plate.crop((x, y, x + pw, y + ph))), np.array(expected_image))
            (session / "edits.json").write_text(json.dumps(edits))
            (session / "session.json").write_text(json.dumps({"name": "Synthetic"}))
            path = export_contact_sheet(session)
            self.assertEqual(path.parent, session / "exports")
            saved_look = checkpoint(session, records(session)[path.name]["checkpoint"])
            self.assertEqual(saved_look["state"], edits)
            self.assertEqual(saved_look["meta"], json.loads((folder / "meta.json").read_text()))
            with Image.open(path) as saved:
                self.assertEqual(saved.format, "PNG")
                self.assertEqual(json.loads(saved.info["astroimp"])["visible_layers"], ["top", "bottom"])

    def test_view_transform_uses_post_rotation_crop_then_source_flip(self):
        # Inverse 90° rotation followed by source-X flip reflects across the anti-diagonal.
        rgb = np.arange(6 * 6 * 3, dtype=np.float32).reshape(6, 6, 3) / 108
        settings = {"rotation": 90, "flipX": True, "crop": {"x": .5, "y": 0, "w": .5, "h": 1}}
        actual = view_transform(rgb, settings)
        np.testing.assert_allclose(actual, rgb.transpose(1, 0, 2)[::-1, ::-1][:, 3:], atol=1e-6)
        identity = {"output": {"exposure": 1, "highlights": .4}}
        np.testing.assert_allclose(view_transform(rgb, identity, tone_map=True), output(rgb, identity), atol=1e-6)


if __name__ == "__main__":
    unittest.main()
