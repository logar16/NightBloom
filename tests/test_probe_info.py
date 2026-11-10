import asyncio
import copy
import json
import shutil
import unittest
import uuid
from pathlib import Path
from unittest.mock import AsyncMock

import numpy as np
from fastapi import HTTPException

from astroimp.mast import write_json
from astroimp.render import probe
from astroimp.server import create_app


class PixelInfoTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(".test-info-" + uuid.uuid4().hex)
        self.project = self.root / "session" / "project"
        self.project.mkdir(parents=True)
        self.addCleanup(lambda: shutil.rmtree(self.root))
        self.meta = {"width": 5, "height": 5, "layers": [
            {"key": "a", "filter": "F502N", "file": "a.f16", "starless": "a.f16"},
        ], "stars": {"file": "stars.u16", "list": "stars.json"}}
        np.full((5, 5), .5, "<f2").tofile(self.project / "a.f16")
        np.zeros((5, 5), "<u2").tofile(self.project / "stars.u16")
        write_json(self.project / "meta.json", self.meta)
        write_json(self.project / "stars.json", [])
        self.state = {"layers": [{"id": "unique-layer", "name": "Oxygen", "source": "a", "visible": True,
            "mode": "tint", "color": "#ff0000", "black": 0, "white": 1, "stretch": "linear", "k": 10,
            "intensity": 1, "opacity": 1, "blend": "add"}],
            "global": {"stars": {"fg": 1, "bg": 1}, "output": {"exposure": 0, "highlights": 0}}}
        write_json(self.project.parent / "edits.json", self.state)
        app = create_app(self.root)
        app.state.sessions.current = self.project.parent
        self.endpoint = next(route.endpoint for route in app.routes if route.path == "/api/probe")

    def call(self, body):
        return asyncio.run(self.endpoint(AsyncMock(json=AsyncMock(return_value=body))))

    def test_live_state_overrides_saved_edits_without_writing_and_returns_layer_ids(self):
        saved = (self.project.parent / "edits.json").read_bytes()
        live = copy.deepcopy(self.state)
        live["layers"][0]["color"] = "#00ff00"
        result = self.call({"x": 2, "y": 2, "radius": 2.5, "state": live})
        self.assertEqual(result["final_hex"], "#008000")
        self.assertEqual(result["layers"][0]["id"], "unique-layer")
        self.assertEqual((self.project.parent / "edits.json").read_bytes(), saved)
        legacy = self.call({"x": 2, "y": 2, "radius": .5})
        self.assertEqual(legacy["final_hex"], "#800000")
        self.assertEqual(legacy["region"]["pixels"], 1)

    def test_invalid_live_state_or_region_is_a_client_error(self):
        for body in ([], {"x": 2, "y": 2, "state": []}, {"x": 2, "y": 2, "state": {"layers": [], "global": None}},
                     {"x": 2, "y": 2, "state": {"layers": [{}], "global": {}}}):
            with self.subTest(body=body), self.assertRaises(HTTPException) as error:
                self.call(body)
            self.assertEqual(error.exception.status_code, 400)

    def test_windowed_probe_observes_replaced_build_files_and_metadata(self):
        first = probe(self.project, self.state, {"x": 2, "y": 2, "radius": .5})
        np.full((5, 5), .25, "<f2").tofile(self.project / "a.f16")
        self.meta["layers"][0]["filter"] = "New filter"
        write_json(self.project / "meta.json", self.meta)
        second = probe(self.project, self.state, {"x": 2, "y": 2, "radius": .5})
        self.assertNotEqual(first["final_hex"], second["final_hex"])
        self.assertEqual(second["layers"][0]["source"], "New filter")
        self.state["layers"][0]["brush"] = {"strokes": [
            {"mode": "hide", "size": 20, "hardness": 1, "strength": 1, "points": [[2.5, 2.5]]}]}
        hidden = probe(self.project, self.state, {"x": 2, "y": 2, "radius": .5})
        self.assertEqual(hidden["final_hex"], "#000000")
        self.state["layers"][0]["brush"]["strokes"][0]["strength"] = .5
        changed = probe(self.project, self.state, {"x": 2, "y": 2, "radius": .5})
        self.assertNotEqual(changed["final_hex"], hidden["final_hex"])
