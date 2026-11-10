"""Removed feature contracts, using only private project fixtures."""
import json

import numpy as np
import pytest

from astroimp.edits import apply
from astroimp.render import Project, render


def test_old_star_assets_are_not_required_or_recreated(tmp_path):
    # Design decision: abandoned star-only looks are ignored; science and ordinary
    # layer controls survive even when every legacy star asset has been removed.
    meta = {"width": 2, "height": 2,
            "layers": [{"key": "a", "file": "a.f16", "filter": "A",
                        "starless": "missing.starless.f16"}],
            "stars": {"file": "missing.u16", "list": "missing.json", "detect": True}}
    (tmp_path / "meta.json").write_text(json.dumps(meta))
    np.full((2, 2), .5, dtype="<f2").tofile(tmp_path / "a.f16")
    layer = {"id": "a", "source": "a", "visible": True, "mode": "tint", "color": "#ff0000",
             "black": 0, "white": 1, "stretch": "linear", "k": 1, "intensity": 1,
             "opacity": .8, "blend": "screen"}
    state = {"layers": [layer], "global": {"stars": {"brightness": 0, "separate": True}},
             "starOverrides": {"1": {"gain": 0}}}
    project = Project(tmp_path)
    result, _ = render(project, state)
    np.testing.assert_allclose(result, np.tile([.4, 0, 0], (2, 2, 1)))
    assert sorted(p.name for p in tmp_path.iterdir()) == ["a.f16", "meta.json"]


def test_removed_star_cli_settings_are_rejected():
    state = {"layers": [], "global": {"stars": {"brightness": 1}}}
    with pytest.raises(ValueError, match="Unknown setting"):
        apply(state, {"layers": []}, "global", ["stars.brightness=0"])


def test_star_http_operations_are_absent(tmp_path):
    from astroimp.server import create_app
    app = create_app(data=tmp_path)
    routes = {route.path for route in app.routes}
    assert "/api/stars/detect" not in routes
    assert "/api/session/stars" not in routes
