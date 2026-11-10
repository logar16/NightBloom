"""Create an isolated scientific fixture and Python export references for Rust tests."""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path

import numpy as np
from astropy.io import fits
from astropy.wcs import WCS
from reproject import reproject_interp
from scipy.ndimage import distance_transform_edt

from astroimp.fullres import export_full
from astroimp.render import Project, output, render


def write_json(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2), encoding="utf-8")


def layer(key: str, color: str) -> dict:
    return {
        "id": key, "name": key.capitalize(), "visible": True, "source": key,
        "mode": "tint", "color": color,
        "gradient": [{"t": 0, "color": "#080c16"}, {"t": .45, "color": "#426f9d"},
                     {"t": .8, "color": "#d2b17c"}, {"t": 1, "color": "#fff0d5"}],
        "spectral": {"stops": [{"nm": 200, "color": "#436bdb"},
                                {"nm": 550, "color": "#5bb8ba"},
                                {"nm": 2500, "color": "#dc8054"}]},
        "black": 0, "white": 1, "stretch": "asinh", "k": 6,
        "invert": False, "curve": [], "detail": {"amount": 0, "radius": 8},
        "clip": None, "intensity": .65, "opacity": .9, "blend": "screen",
        "mask": {"source": None, "black": 0, "white": 1, "invert": False},
        "transform": {"x": 0, "y": 0, "scale": 1, "rotation": 0},
        "edge": {"feather": 0, "irregularity": 0}, "brush": {"strokes": []},
    }


def prepare(destination: Path) -> None:
    destination = destination.resolve()
    protected = Path(__file__).resolve().parents[1] / "data"
    if destination == protected or protected in destination.parents:
        raise ValueError("Rust fixtures must never be written under the owner's data directory")
    if destination.exists() and any(destination.iterdir()):
        raise ValueError(f"Fixture destination must be empty: {destination}")
    project = destination / "data" / "sessions" / "rust-validation" / "project"
    project.mkdir(parents=True)
    session = project.parent
    sources = destination / "sources"
    sources.mkdir()
    reference = destination / "reference"
    reference.mkdir()
    width, height = 64, 48
    target = WCS(naxis=2)
    target.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    target.wcs.crval = [359.999, 30]
    target.wcs.crpix = [(width + 1) / 2, (height + 1) / 2]
    angle = np.deg2rad(19)
    target.wcs.cd = np.array([[-np.cos(angle), np.sin(angle)],
                              [np.sin(angle), np.cos(angle)]]) * (2 / 3600)
    source_wcs = target.deepcopy()
    source_wcs.wcs.crpix = (source_wcs.wcs.crpix - .5) * 2 + .5
    source_wcs.wcs.cd /= 2
    yy, xx = np.mgrid[:height * 2, :width * 2].astype(np.float32)
    x, y = (xx + .5) / (width * 2), (yy + .5) / (height * 2)
    raw = {
        "red": .03 + .7 * np.exp(-((x - .35) ** 2 + (y - .55) ** 2) / .035) + .15 * x,
        "blue": .02 + .6 * np.exp(-((x - .7) ** 2 + (y - .4) ** 2) / .045) + .12 * y,
    }
    planes = {}
    layers = []
    edge = distance_transform_edt(np.pad(np.ones((height, width), bool), 1))[1:-1, 1:-1]
    for key, values in raw.items():
        source = sources / f"{key}.fits"
        fits.PrimaryHDU(values.astype(np.float32), header=source_wcs.to_header()).writeto(source)
        pixels, _ = reproject_interp((values, source_wcs), target, shape_out=(height, width))
        pixels = pixels[::-1].astype(np.float16)
        planes[key] = pixels.astype(np.float32)
        pixels.tofile(project / f"{key}.f16")
        edge.astype(np.float16).tofile(project / f"{key}.edge.f16")
        np.ones((height, width), np.uint8).tofile(project / f"{key}.coverage.u8")
        layers.append({
            "key": key, "obs_id": key, "filter": key.capitalize(), "collection": "HST",
            "instrument": "synthetic", "wavelength_nm": 700 if key == "red" else 500,
            "file": f"{key}.f16", "edge_file": f"{key}.edge.f16",
            "coverage_file": f"{key}.coverage.u8", "source": str(source),
            "stats": {"background": 0, "scale": 1, "p01": 0, "p50": .2,
                      "p99": 1, "p999": 1, "max": 1},
            "coverage": 1, "background": 0, "scale": 1,
        })
    np.mean(list(planes.values()), axis=0).astype(np.float16).tofile(project / "luminance.f16")
    edge.astype(np.float16).tofile(project / "luminance.edge.f16")
    meta = {
        "width": width, "height": height, "pixel_scale_arcsec": 2,
        "built_at": "2026-11-03T00:00:00Z", "wcs": dict(target.to_header()),
        "layers": layers,
        "luminance": {"file": "luminance.f16",
                      "edge_file": "luminance.edge.f16"},
    }
    state = {
        "version": 1, "layers": [layer("red", "#ed7246"), layer("blue", "#4a90e2")],
        "global": {"crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "rotation": 0,
                   "flipX": False,
                   "output": {"exposure": 0, "highlights": 0}},
        "sources": ["red", "blue"], "checkpoints": [],
    }
    write_json(project / "meta.json", meta)
    write_json(session / "session.json", {"name": "Rust validation"})
    write_json(session / "view.json", {"source": "coordinates", "center_icrs_deg": [359.999, 30],
               "field_arcmin": [width * 2 / 60, height * 2 / 60],
               "pixel_dimensions": [width, height], "rotation_deg": 19,
               "captured_at_utc": "2026-11-03T00:00:00Z"})
    write_json(session / "catalog.json", {"groups": [], "observations_in_view": 0})
    write_json(session / "selection.json", {"obs_ids": ["red", "blue"]})
    write_json(session / "triage.json", {"favorites": [], "dismissed": []})
    write_json(session / "markers.json", [])
    write_json(session / "edits.json", state)
    write_json(session / "edits.rev.json", {"revision": 1, "source": "browser", "note": ""})
    write_json(session.parent / "current.json", {"id": session.name})
    reference_project = Project(project, max_side=width)
    cases = {}
    for blend in ("normal", "add", "screen", "multiply", "overlay", "softlight",
                  "lighten", "difference", "subtract", "darken"):
        edits = copy.deepcopy(state)
        edits["layers"][0]["blend"] = blend
        rgb, _ = render(reference_project, edits, collect=False)
        cases[blend] = {"state": edits, "width": width, "height": height,
                        "pixels": output(rgb, edits["global"]).ravel().tolist()}
    scenarios = {}
    tone = copy.deepcopy(state)
    tone["layers"][0].update(black=.08, white=.85, stretch="gamma", k=2.2, invert=True,
        curve=[{"x": 0, "y": .1}, {"x": .4, "y": .65}, {"x": 1, "y": .9}])
    tone["global"]["output"] = {"exposure": .2, "highlights": .3, "black": .03,
                                "white": .9, "midtones": .65}
    scenarios["tone"] = tone
    colour = copy.deepcopy(state)
    colour["layers"][0]["mode"] = "gradient"
    colour["layers"][1]["mode"] = "spectral"
    scenarios["colour"] = colour
    transformed = copy.deepcopy(state)
    transformed["layers"][0]["transform"] = {"x": .1, "y": -.07, "scale": .85, "rotation": 23}
    transformed["layers"][0]["mask"] = {"source": "blue", "black": .05, "white": .6, "invert": True}
    transformed["layers"][1]["clip"] = {"x": .1, "y": .15, "w": .7, "h": .65,
        "rotation": 0, "feather": .15, "shape": "ellipse"}
    scenarios["transformed"] = transformed
    textured = copy.deepcopy(state)
    textured["layers"][0]["detail"] = {"amount": .35, "radius": 2}
    textured["layers"][0]["edge"] = {"feather": 5, "irregularity": .6}
    textured["layers"][0]["brush"] = {"strokes": [{"mode": "hide", "size": 15,
        "hardness": .4, "strength": .7, "points": [[20, 20], [35, 28]]}]}
    scenarios["textured"] = textured
    for name, edits in scenarios.items():
        rgb, _ = render(reference_project, edits, collect=False)
        cases[name] = {"state": edits, "width": width, "height": height,
                       "pixels": output(rgb, edits["global"]).ravel().tolist()}
        result = export_full(session, state=edits, long_side=64, format="png8", name=f"python-{name}")
        (reference / f"python-{name}.png").write_bytes(Path(result["path"]).read_bytes())
    write_json(reference / "render.json", cases)
    for long_side in (64, 128):
        for format, suffix in (("png8", ""), ("png16", "-16")):
            name = f"python-{long_side}{suffix}"
            result = export_full(session, state=state, long_side=long_side, format=format,
                                 name=name)
            (reference / f"{name}.png").write_bytes(Path(result["path"]).read_bytes())
    write_json(destination / "fixture.json", {"data": str(session.parent.parent),
               "session": session.name, "width": width, "height": height,
               "native_width": width * 2, "native_height": height * 2})
    print(json.dumps({"fixture": str(destination), "data": str(session.parent.parent),
                      "session": session.name}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    prepare(parser.parse_args().output)
