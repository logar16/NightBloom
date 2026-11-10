import re
import unittest
from pathlib import Path

from astroimp.edits import CHOICES
from astroimp.render import BLENDS


class BlendContractTests(unittest.TestCase):
    def test_gpu_numpy_and_edit_cli_share_one_ordered_blend_list(self):
        # The shader selects formulas by index into the frontend list, so order matters.
        source = (Path(__file__).parents[1] / "frontend/src/editor/state.ts").read_text(encoding="utf-8")
        frontend = re.findall(r"'(\w+)'", re.search(r"export const BLENDS[^=]*=\s*\[([^\]]*)\]", source).group(1))
        self.assertEqual(frontend, BLENDS)
        self.assertEqual(set(CHOICES["blend"]), set(BLENDS))


class SurveySizeTests(unittest.TestCase):
    def test_survey_plan_size_matches_written_fits(self):
        import numpy as np
        from astropy.io import fits
        from astropy.wcs import WCS
        from astroimp.build import survey_size
        path = Path(".test-survey-size.fits")
        self.addCleanup(lambda: path.unlink(missing_ok=True))
        wcs = WCS(naxis=2); wcs.wcs.ctype = ["RA---TAN", "DEC--TAN"]
        fits.PrimaryHDU(np.zeros((1777, 2501), np.float32), header=wcs.to_header()).writeto(path)
        self.assertEqual(survey_size({"width": 2501, "height": 1777}), path.stat().st_size)
        self.assertIsNone(survey_size(None))
