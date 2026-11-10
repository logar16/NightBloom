import json
import tempfile
import unittest
from pathlib import Path

from astroimp.builds import archive_current, list_builds, restore


def make_build(session: Path, built_at: str, obs_ids: list[str], edits: dict) -> None:
    project = session / "project"
    project.mkdir(parents=True)
    (project / "meta.json").write_text(json.dumps({
        "built_at": built_at, "layers": [{"obs_id": o, "filter": o.upper()} for o in obs_ids]}))
    (project / "layer.f16").write_bytes(built_at.encode())
    (session / "edits.json").write_text(json.dumps(edits))


class BuildHistoryTests(unittest.TestCase):
    def test_archived_build_and_its_edits_can_be_restored_without_losing_the_active_one(self):
        with tempfile.TemporaryDirectory() as temp:
            session = Path(temp)
            make_build(session, "2026-10-23T10:00:00+00:00", ["a", "b"], {"look": "first"})
            first = archive_current(session)
            self.assertFalse((session / "project").exists())
            make_build(session, "2026-10-23T11:00:00+00:00", ["c"], {"look": "second"})

            history = list_builds(session)
            self.assertEqual([(b["id"], b["current"]) for b in history],
                             [("20261023T110000Z", True), (first, False)])

            restore(session, first)
            self.assertEqual((session / "project" / "layer.f16").read_bytes(), b"2026-10-23T10:00:00+00:00")
            self.assertEqual(json.loads((session / "edits.json").read_text()), {"look": "first"})
            history = list_builds(session)
            self.assertEqual([b["current"] for b in history], [True, False])
            self.assertEqual([l["obs_id"] for l in history[1]["layers"]], ["c"])
            archived = session / "builds" / history[1]["id"]
            self.assertEqual(json.loads((archived / "edits.json").read_text()), {"look": "second"})
            with self.assertRaises(ValueError):
                restore(session, "..")


if __name__ == "__main__":
    unittest.main()
