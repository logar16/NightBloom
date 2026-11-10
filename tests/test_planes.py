from pathlib import Path
import tempfile
import unittest

import numpy as np

from astroimp.planes import load_plane


class PlaneTests(unittest.TestCase):
    def test_strip_conversion_matches_full_conversion_and_releases_mapping(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as folder:
            path = Path(folder) / "plane.f16"
            original = np.random.default_rng(19).normal(size=(259, 317)).astype("<f2")
            original.tofile(path)
            full = original.astype(np.float32)
            for step in (1, 3, 4, 9):
                h, w = np.array(full.shape)//step
                expected = full[:h*step, :w*step].reshape(h, step, w, step).mean((1, 3))
                np.testing.assert_array_equal(load_plane(path, original.shape, step=step), expected)
            np.testing.assert_array_equal(load_plane(path, original.shape, window=(7, 9, 45, 23)),
                                          full[9:23, 7:45])
            path.rename(path.with_suffix(".moved"))
