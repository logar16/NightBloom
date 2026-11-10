import time
import unittest
from unittest.mock import Mock, patch

import requests

from astroimp import services


class HealthTest(unittest.TestCase):
    def test_reports_unreachable_services_and_rechecks_sooner(self):
        health = services.Health({"cds": "https://cds.test", "mast": "https://mast.test"})
        self.assertEqual(health.snapshot()["cds"]["ok"], None)  # unknown until the first probe

        def get(url, timeout):
            if "cds" in url:
                raise requests.ConnectTimeout("timed out")
            return Mock(status_code=200)
        with patch.object(services.requests, "get", side_effect=get):
            health.next_check = 0
            health.lock.acquire()
            health._probe()
        state = health.snapshot()
        self.assertIs(state["cds"]["ok"], False)
        self.assertIs(state["mast"]["ok"], True)
        self.assertLessEqual(health.next_check - time.monotonic(), services.DOWN_INTERVAL)

    def test_server_errors_count_as_down(self):
        health = services.Health({"mast": "https://mast.test"})
        with patch.object(services.requests, "get", return_value=Mock(status_code=503)):
            health.lock.acquire()
            health._probe()
        self.assertIs(health.state["mast"]["ok"], False)

    def test_network_failures_read_as_one_sentence(self):
        from urllib3.exceptions import ConnectTimeoutError, MaxRetryError
        url = "https://alasky.cds.unistra.fr/MocServer/query?RA=173.68&expr=ID%3DCDS"
        inner = MaxRetryError(None, url, ConnectTimeoutError("Connection to alasky.cds.unistra.fr timed out."))
        try:
            try:
                raise requests.ConnectTimeout(inner)
            except requests.ConnectTimeout as error:
                raise RuntimeError("hips2fits failed for DSS2 red") from error
        except RuntimeError as wrapped:
            self.assertEqual(services.friendly(wrapped), "CDS isn't responding. Try again in a few minutes.")
        response = Mock(status_code=503)
        http = requests.HTTPError("503 Server Error: https://mast.stsci.edu/api", response=response)
        self.assertEqual(services.friendly(http), "MAST returned an error (503). Try again later.")
        self.assertEqual(services.friendly(ValueError("Select at least one band.")), "Select at least one band.")

    def test_survey_retry_keeps_mast_groups_and_clears_warning(self):
        import json, tempfile
        from pathlib import Path
        from astroimp import session as sessions
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "sessions" / "s"
            path.mkdir(parents=True)
            (path / "view.json").write_text(json.dumps({"center_icrs_deg": [10, 20], "field_arcmin": [10, 6],
                                                        "corners_icrs_deg": [], "pixel_dimensions": [100, 60]}))
            hst = {"collection": "HST", "observations": [{"obs_id": "h1"}]}
            (path / "catalog.json").write_text(json.dumps({"groups": [hst], "survey_warning": "old"}))
            with patch.object(sessions.surveys, "available", side_effect=requests.ConnectTimeout("alasky.cds.unistra.fr timed out")):
                self.assertIn("CDS isn't responding", sessions.search_surveys(path)["warning"])
            band = {"collection": "Survey", "observations": [{"obs_id": "s1"}]}
            with patch.object(sessions.surveys, "available", return_value=[]), \
                    patch.object(sessions.surveys, "groups", return_value=[band]):
                self.assertIsNone(sessions.search_surveys(path)["warning"])
            content = json.loads((path / "catalog.json").read_text())
            self.assertEqual([g["collection"] for g in content["groups"]], ["HST", "Survey"])
            self.assertNotIn("survey_warning", content)


if __name__ == "__main__":
    unittest.main()
