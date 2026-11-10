import json
from pathlib import Path
from types import SimpleNamespace
import unittest

import numpy as np

from astroimp.edits import apply
from astroimp.render import brush_data, brush_maps, brush_mask, render


class BrushTests(unittest.TestCase):
    def test_shared_mask_samples(self):
        fixtures = json.loads((Path(__file__).parent / "fixtures" / "brush.json").read_text())
        for fixture in fixtures:
            with self.subTest(fixture=fixture["name"]):
                mask = brush_mask(fixture["width"], fixture["height"], fixture["strokes"])
                for x, y, expected in fixture["samples"]:
                    self.assertAlmostEqual(float(mask[y, x]), expected, places=6)
                maps = brush_maps(fixture["width"], fixture["height"], fixture["strokes"])
                for x, y, gain, bias in fixture.get("signal_samples", []):
                    self.assertAlmostEqual(float(maps[1, y, x]), gain, places=6)
                    self.assertAlmostEqual(float(maps[2, y, x]), bias, places=6)

    def test_clear_legacy_and_painted_layers(self):
        state = {"layers": [{"id": "a", "name": "A", "source": "a"},
                            {"id": "b", "name": "B", "source": "b", "brush": {"strokes": [{"points": []}]}}]}
        meta = {"layers": [{"key": "a", "filter": "A"}, {"key": "b", "filter": "B"}]}
        apply(state, meta, "*", ["brush=clear"])
        self.assertTrue(all(layer["brush"] == {"strokes": []} for layer in state["layers"]))
        with self.assertRaises(ValueError):
            apply(state, meta, "A", ["brush=paint"])

    def test_empty_and_outside_strokes(self):
        strokes = [{"mode": "hide", "size": 8, "strength": 1, "hardness": .5, "points": []},
                   {"mode": "hide", "size": 8, "strength": 1, "hardness": .5, "points": [[-20, -20]]}]
        np.testing.assert_array_equal(brush_mask(12, 10, strokes), np.ones((10, 12)))

    def test_source_paint_fills_missing_coverage_before_tone_and_colour(self):
        raw = np.zeros((3, 3), np.float32)
        raw[0, 0] = raw[1, 1] = np.nan
        coverage = np.isfinite(raw).astype(np.float32)
        project = SimpleNamespace(meta={"width": 3, "height": 3, "layers": [{"key": "a"}]},
                                  shape=(3, 3), step=1, window=None, keys=["a"], full=[raw], lum=None,
                                  edge_fade=lambda _: coverage)
        stroke = {"mode": "white", "size": 1, "hardness": 1, "strength": 1, "points": [[1.5, 1.5]]}
        layer = {"id": "a", "source": "a", "mode": "tint", "color": "#804020", "blend": "normal",
                 "stretch": "linear", "black": 0, "white": 1, "k": 10, "intensity": .5, "opacity": .7,
                 "curve": [{"x": 0, "y": .2}, {"x": 1, "y": 1}], "brush": {"strokes": [stroke]}}
        tint = np.array([128, 64, 32]) / 255
        for mode, expected in [("white", .35), ("black", .07)]:
            stroke["mode"] = mode
            result, _ = render(project, {"layers": [layer]})
            np.testing.assert_allclose(result[1, 1], tint * expected, atol=1e-6)
            np.testing.assert_array_equal(result[0, 0], [0, 0, 0])
        self.assertTrue(np.isnan(raw[1, 1]))
        layer["transform"] = {"x": 1}
        maps = brush_data(project, layer)
        np.testing.assert_array_equal(maps[1], np.ones((3, 3)))
        np.testing.assert_array_equal(maps[2], np.zeros((3, 3)))
