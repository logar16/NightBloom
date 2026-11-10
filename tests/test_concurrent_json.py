import json
import tempfile
import threading
import unittest
from pathlib import Path

from astroimp.mast import write_json
from astroimp.session import read_json


class ConcurrentJsonTests(unittest.TestCase):
    def test_saves_survive_concurrent_writers_and_polling_readers(self):
        # Mirrors the editor saving edits while the status poll reads edits.rev.json.
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "edits.rev.json"
            write_json(path, {"revision": 0})
            errors = []

            def writer(offset):
                try:
                    for i in range(200):
                        write_json(path, {"revision": offset + i})
                except Exception as exc:  # noqa: BLE001 - collected for the assertion
                    errors.append(exc)

            def reader():
                try:
                    for _ in range(400):
                        self.assertIn("revision", read_json(path))
                except Exception as exc:  # noqa: BLE001
                    errors.append(exc)

            threads = [threading.Thread(target=writer, args=(n * 1000,)) for n in range(3)]
            threads += [threading.Thread(target=reader) for _ in range(3)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
            self.assertEqual(errors, [])
            self.assertIn("revision", json.loads(path.read_text()))
            self.assertEqual(list(Path(temp).glob("*.tmp")), [])


if __name__ == "__main__":
    unittest.main()
