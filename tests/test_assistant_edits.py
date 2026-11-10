import json
import tempfile
import unittest
from pathlib import Path

import numpy as np

from astroimp.edits import apply
from astroimp.render import analyze, output, probe


def layer(name, key, **settings):
    base = {"id": name.lower(), "name": name, "visible": True, "source": key, "mode": "tint",
            "color": "#ffffff", "gradient": [], "spectral": {"stops": []}, "black": 0, "white": 1,
            "stretch": "linear", "k": 10, "intensity": 1, "opacity": 1, "blend": "add",
            "mask": {"source": None, "black": 0, "white": 1, "invert": False},
            "transform": {"x": 0, "y": 0, "scale": 1, "rotation": 0}}
    return {**base, **settings}


META = {"layers": [{"key": "a", "filter": "F187N"}, {"key": "b", "filter": "F335M"}]}


def state():
    return {"layers": [layer("Hot", "a", intensity=.8, white=1.2), layer("Dim", "b", intensity=.5)],
            "global": {"crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "rotation": 0, "flipX": False,
                       "stars": {"fg": 1, "bg": 1}, "parallax": {"on": False},
                       "output": {"exposure": 0, "highlights": 0}},
            "starClasses": {}}


class AssistantEditTests(unittest.TestCase):
    def test_manual_output_levels_follow_exposure_preserve_highlights_and_bound_midtones(self):
        rgb = np.array([[[.125, .375, .75]]], np.float32)
        settings = {"output": {"exposure": 1, "black": .25, "white": 1.75, "midtones": 2}}
        np.testing.assert_allclose(output(rgb, settings), [[[0, 1 / 9, 25 / 36]]], atol=1e-7)
        legacy = {"output": {"exposure": .4, "highlights": .2}}
        np.testing.assert_array_equal(output(rgb, legacy), output(rgb, {"output": {
            **legacy["output"], "black": 0, "white": 1, "midtones": 1}}))
        settings["output"] = {"black": .5, "white": .5, "midtones": .7}
        np.testing.assert_array_equal(output(np.array([[[.49, .5, .51]]], np.float32), settings),
                                      [[[0, 0, 1]]])
        for key, value in (("white", 0), ("black", -.1), ("midtones", 0),
                           ("midtones", float("nan")), ("black", True), ("white", "bad")):
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                output(rgb, {"output": {key: value}})

    def test_manual_output_cli_edits_work_on_legacy_state_and_reject_non_numeric_values(self):
        s = state()
        apply(s, META, "global", ["output.black+=.08", "output.white*=.95", "output.midtones=.7"])
        np.testing.assert_allclose(output(np.array([[[.2, .5, .8]]], np.float32), s["global"]),
                                   [[[((.2 - .08) / .87) ** .7, ((.5 - .08) / .87) ** .7,
                                      ((.8 - .08) / .87) ** .7]]], atol=1e-7)
        for value in ("nan", "inf", "true", "wrong"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                apply(state(), META, "global", [f"output.midtones={value}"])

    def test_crop_anchor_accepts_missing_field_and_bounds_angles(self):
        s = state()
        s["global"]["rotation"] = 23
        apply(s, META, "global", ["crop.rotation+=14"])
        self.assertEqual(s["global"]["crop"]["rotation"], 37)
        apply(s, META, "global", ["crop.rotation=999"])
        self.assertEqual(s["global"]["crop"]["rotation"], 180)
        for value in ("nan", "inf", "true", "wrong"):
            with self.assertRaises(ValueError):
                apply(s, META, "global", [f"crop.rotation={value}"])

    def test_relative_and_absolute_edits_match_layers_by_filter_name_number_or_all(self):
        s = state()
        apply(s, META, "f187n", ["intensity*=0.5", "white+=0.3"])
        self.assertEqual((s["layers"][0]["intensity"], s["layers"][0]["white"]), (.4, 1.5))
        apply(s, META, "*", ["opacity*=0.5"])
        self.assertEqual([l["opacity"] for l in s["layers"]], [.5, .5])
        apply(s, META, "2", ["blend=screen", "color=#ff8040"])
        self.assertEqual((s["layers"][1]["blend"], s["layers"][1]["color"]), ("screen", "#ff8040"))
        apply(s, META, "global", ["output.highlights=5"])
        self.assertEqual(s["global"]["output"]["highlights"], 1)  # clamped to the slider range
        for target, ops in (("nope", ["intensity=1"]), ("1", ["blend=glow"]), ("1", ["missing=1"]),
                            ("1", ["mode*=2"])):
            with self.assertRaises(ValueError):
                apply(state(), META, target, ops)

    def test_normal_blend_uses_brightness_as_coverage(self):
        from astroimp.render import Project, render
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp)
            h, w = 4, 4
            top = np.zeros((h, w), "<f2")
            top[:, 2:] = 1.0                       # bright on the right, empty on the left
            base = np.full((h, w), .5, "<f2")
            for name, array in (("a.f16", top), ("b.f16", base)):
                array.tofile(folder / name)
                array.tofile(folder / name.replace(".f16", ".starless.f16"))
            np.zeros((h, w), "<u2").tofile(folder / "stars.u16")
            (folder / "stars.json").write_text("[]")
            (folder / "meta.json").write_text(json.dumps({
                "width": w, "height": h, "stars": {"file": "stars.u16", "list": "stars.json"},
                "layers": [{"key": "a", "filter": "F187N", "file": "a.f16", "starless": "a.starless.f16"},
                           {"key": "b", "filter": "F335M", "file": "b.f16", "starless": "b.starless.f16"}]}))
            s = state()
            s["layers"] = [layer("Top", "a", blend="normal", color="#ff0000"),
                           layer("Base", "b", color="#00ff00")]
            acc, _ = render(Project(folder), s)
            np.testing.assert_allclose(acc[0, 0], [0, .5, 0], atol=1e-3)   # faint top: base shows through
            np.testing.assert_allclose(acc[0, 3], [1, 0, 0], atol=1e-3)    # bright top: fully covers

    def test_analysis_attributes_clipping_to_the_layers_that_cause_it(self):
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp) / "project"
            folder.mkdir()
            h, w = 40, 60
            hot = np.zeros((h, w), "<f2")
            hot[:, :30] = 1.0
            dim = np.full((h, w), .3, "<f2")
            for name, array in (("a.f16", hot), ("b.f16", dim)):
                array.tofile(folder / name)
                array.tofile(folder / name.replace(".f16", ".starless.f16"))
            np.zeros((h, w), "<u2").tofile(folder / "stars.u16")
            (folder / "stars.json").write_text("[]")
            (folder / "meta.json").write_text(json.dumps({
                "width": w, "height": h, "stars": {"file": "stars.u16", "list": "stars.json"},
                "layers": [{"key": "a", "filter": "F187N", "file": "a.f16", "starless": "a.starless.f16"},
                           {"key": "b", "filter": "F335M", "file": "b.f16", "starless": "b.starless.f16"}]}))
            s = state()
            s["layers"][0].update(intensity=1, white=1)
            s["layers"][1].update(intensity=1, white=.4)
            report = analyze(folder, s, Path(temp) / "analysis")
            self.assertAlmostEqual(report["clipped_fraction"], .5, places=2)
            self.assertEqual(report["layers"][0]["source"], "F187N")
            self.assertGreater(report["layers"][0]["share_of_clipped_light"], .5)
            self.assertTrue(Path(report["preview"]).exists() and Path(report["clipping_map"]).exists())
            # Probing a spot on the hot half attributes it mostly to F187N; the cool half to F335M.
            hot_spot = probe(folder, s, {"x": 10, "y": 20, "radius": 2})
            cool_spot = probe(folder, s, {"x1": 40, "y1": 10, "x2": 50, "y2": 20})
            self.assertEqual(hot_spot["layers"][0]["source"], "F187N")
            self.assertEqual(cool_spot["layers"][0]["source"], "F335M")
            self.assertEqual(cool_spot["region"]["pixels"], 121)


if __name__ == "__main__":
    unittest.main()
