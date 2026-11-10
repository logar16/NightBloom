import json
import shutil
import uuid
from pathlib import Path
from unittest.mock import patch

import numpy as np
import pytest
from astropy.io import fits
from astropy.wcs import WCS
from fastapi.testclient import TestClient

from astroimp import mosaic
from astroimp.build import build
from astroimp.fullres import export_full, reproject_mosaic, subgrid
from astroimp.mast import write_json
from astroimp.render import Project, output, render
from astroimp.server import create_app
from astroimp.storage import Storage
from PIL import Image
from test_assistant_edits import layer as artistic_layer, state


@pytest.fixture
def folder():
    root = Path.cwd() / f".test-mosaic-{uuid.uuid4().hex}"
    root.mkdir()
    try:
        yield root
    finally:
        shutil.rmtree(root)


def test_overlap_graph_recovers_calibration_without_stacking_light():
    rng = np.random.default_rng(42)
    y, x = np.mgrid[:180, :340]
    sky = (10 + x*.025 + y*.013 + 2*np.sin(x/22)*np.cos(y/29)).astype(np.float32)
    images = []
    gains, offsets = [1, 1.8, .65, 1.2], [0, 4, -2, 1]
    for i, (start, end) in enumerate([(0, 145), (85, 235), (185, 310), (325, 340)]):
        tile = ((sky-offsets[i])/gains[i] + rng.normal(0, .003, sky.shape)).astype(np.float32)
        # Compact stars and mismatched saturated cores do not drive the fit.
        tile[45:48, 110:113] += 500*(i+1)
        tile[:, :start] = np.nan
        tile[:, end:] = np.nan
        images.append(tile)
    fits_ = mosaic.calibrate(images)
    reference = next(i for i, fit in enumerate(fits_) if fit["reference"])
    for i in range(3):
        assert fits_[i]["gain"] == pytest.approx(gains[i]/gains[reference], rel=.003)
        assert fits_[i]["offset"] == pytest.approx((offsets[i]-offsets[reference])/gains[reference], abs=.03)
    assert fits_[3]["no_overlap"]
    assert fits_[3]["gain"] == 1
    assert fits_[3]["offset"] == 0
    merged = mosaic.combine(images, fits_)
    expected = (sky-offsets[reference])/gains[reference]
    assert np.nanmedian(np.abs(merged[:, :310]-expected[:, :310])) < .01
    assert np.isnan(merged[:, 310:325]).all()
    # No extra light from two overlapping tiles.
    assert np.nanmedian(merged[60:100, 100:140]-expected[60:100, 100:140]) < .01


def observations():
    return [dict(obs_id=f"tile{i}", collection="HST", instrument="WFC3/UVIS",
                 proposal="123", filter=filter_, wavelength_nm=500,
                 data_uri=f"mast:HST/product/tile{i}.fits")
            for i, filter_ in enumerate(["F502N", "F502N", "F673N"])]


def test_grouping_keeps_filters_programs_and_unknown_provenance_separate():
    obs = observations()
    assert [len(g) for g in mosaic.groups(obs)] == [2, 1]
    assert len(mosaic.groups(obs, False)) == 3
    obs[1]["proposal"] = "456"
    assert len(mosaic.groups(obs)) == 3
    for o in obs:
        o.pop("proposal")
    assert len(mosaic.groups(obs)) == 3


def fixture(root):
    session = root / "sessions" / "example"
    session.mkdir(parents=True)
    downloads = root / "downloads"
    downloads.mkdir()
    y, x = np.mgrid[:96, :160]
    sky = (10 + x*.02 + y*.01 + np.sin(x/17)).astype(np.float32)
    native = WCS(naxis=2)
    native.wcs.ctype = ["RA---TAN", "DEC--TAN"]
    native.wcs.crval = [12, -25]
    native.wcs.crpix = [80.5, 48.5]
    native.wcs.cdelt = [-1/3600, 1/3600]
    for i, obs in enumerate(observations()):
        raw = sky.copy() if i != 1 else (sky-3)/1.7
        if i == 0:
            raw[:, 110:] = np.nan
        if i == 1:
            raw[:, :55] = np.nan
        fits.writeto(downloads / f"{obs['obs_id']}.fits", raw, native.to_header())
    write_json(session / "catalog.json", {"groups": [{"proposal": "123", "observations": observations()}]})
    write_json(session / "selection.json", {"obs_ids": ["tile0", "tile1", "tile2"]})
    write_json(session / "session.json", {"name": "test", "stars": {"detect": False}, "build": {"mosaic": False}})
    write_json(session / "view.json", {})
    grid = {"width": 80, "height": 48, "pixel_scale_arcsec": 2}
    small = native.deepcopy()
    small.wcs.crpix = (small.wcs.crpix-.5)/2+.5
    small.wcs.cdelt *= 2
    grid["wcs"] = dict(small.to_header())
    write_json(session / "grid.json", grid)
    return session, downloads


def test_build_regroup_export_cache_and_option_off(folder):
    session, downloads = fixture(folder)
    first = build(session, downloads=downloads)
    assert len(first["layers"]) == 3
    write_json(session / "edits.json", {
        "layers": [{"id": "keep", "source": "tile0", "color": "#123456", "blend": "screen"},
                   {"id": "drop", "source": "tile1", "color": "#abcdef", "blend": "add"}],
        "selectedLayer": "drop", "global": {"stars": {}},
        "sources": ["tile0", "tile1", "tile2"], "starWeights": {"tile0": .4}})
    write_json(session / "session.json", {"stars": {"detect": False}, "build": {"mosaic": True}})
    meta = build(session, downloads=downloads)
    assert len(meta["layers"]) == 2
    merged = next(l for l in meta["layers"] if l.get("tiles"))
    assert len(merged["tiles"]) == 2
    assert merged["tiles"][1]["gain"] == pytest.approx(1.7, rel=.002)
    edits = json.loads((session / "edits.json").read_text())
    assert len(edits["layers"]) == 1
    assert edits["layers"][0]["id"] == "keep"
    assert edits["layers"][0]["color"] == "#123456"
    assert edits["layers"][0]["source"] == merged["key"]
    assert edits["sources"] == [merged["key"], "tile2"]
    assert "starWeights" not in edits
    with patch("astroimp.build.aligned_source", side_effect=AssertionError("cache missed")):
        cached = build(session, downloads=downloads)
    assert cached["layers"] == meta["layers"]
    # Reproject at native scale, using saved linear gains and the same normalization.
    shape = (96, 160)
    target = subgrid(WCS(meta["wcs"]), 48, (0, 0), shape, 2)
    raw = np.empty(shape, np.float32)
    reproject_mosaic(merged, target, raw, folder, scale=2)
    normalized = (raw-merged["stats"]["background"])/merged["stats"]["scale"]
    small = normalized.reshape(48, 2, 80, 2).mean((1, 3))[::-1]
    preview = np.fromfile(session / "project" / merged["file"], "<f2").reshape(48, 80)
    assert np.nanmedian(np.abs(preview-small)) < .001
    assert np.nanpercentile(np.abs(preview-small), 95) < .003
    edits = state()
    edits["layers"] = [artistic_layer("m", merged["key"], color="#77aaff")]
    edits["global"]["stars"] = {"separate": False}
    exported = export_full(session, scale_factor=1, state=edits)
    expected, _ = render(Project(session / "project"), edits)
    expected = np.round(output(expected, edits["global"])*255).astype(np.uint8)
    actual = np.asarray(Image.open(exported["path"]))
    np.testing.assert_allclose(actual, expected, atol=1)
    native_export = export_full(session, scale_factor=2, state=edits)
    native_rgb = np.asarray(Image.open(native_export["path"])).reshape(48, 2, 80, 2, 3).mean((1, 3))
    assert np.percentile(np.abs(native_rgb-expected), 95) <= 1
    write_json(session / "session.json", {"stars": {"detect": False}, "build": {"mosaic": False}})
    separate = build(session, downloads=downloads)
    assert len(separate["layers"]) == 3
    assert all("tiles" not in l for l in separate["layers"])
    write_json(folder / "sessions" / "current.json", {"id": "example"})
    with TestClient(create_app(folder)) as client:
        history = client.get("/api/builds").json()
        identifier = next(h["id"] for h in history if not h["current"] and len(h["layers"]) == 2)
        assert client.post(f"/api/builds/{identifier}/restore").status_code == 200
        restored = client.get("/api/session").json()
        assert set(restored["selection"]["obs_ids"]) == {"tile0", "tile1", "tile2"}
        assert restored["build"] == {"mosaic": True}


def test_build_option_api_and_plan_are_project_scoped(folder):
    session, _ = fixture(folder)
    write_json(folder / "sessions" / "current.json", {"id": "example"})
    with TestClient(create_app(folder)) as client:
        assert client.get("/api/session").json()["build"] == {"mosaic": False}
        assert client.put("/api/session/build", json={"mosaic": "true"}).status_code == 400
        assert client.put("/api/session/build", json={"mosaic": True}).status_code == 200
        assert client.get("/api/session").json()["build"] == {"mosaic": True}
        assert client.get("/api/build/plan").json()["layer_count"] == 2
    settings = json.loads((session / "session.json").read_text())
    assert settings["stars"] == {"detect": False}


def test_science_only_luminance_products_use_merged_coverage(folder):
    session, downloads = fixture(folder)
    y, x = np.mgrid[:96, :160]
    for i in range(3):
        with fits.open(downloads / f"tile{i}.fits", mode="update", memmap=False) as hdus:
            hdus[0].data += (200*np.exp(-((x-80)**2+(y-60)**2)/8)/(1.7 if i == 1 else 1))
    write_json(session / "session.json", {"stars": {"detect": True}})
    meta = build(session, downloads=downloads)
    assert len(meta["layers"]) == 2
    assert "stars" not in meta
    assert not list((session / "project").glob("*star*"))
    shape = (meta["height"], meta["width"])
    layers = [np.fromfile(session / "project" / l["file"], "<f2").reshape(shape).astype(float)
              for l in meta["layers"]]
    coverage = [np.fromfile(session / "project" / l["coverage_file"], "u1").reshape(shape)
                for l in meta["layers"]]
    lum = np.fromfile(session / "project" / meta["luminance"]["file"], "<f2").reshape(shape)
    np.testing.assert_allclose(lum, sum(layers)/sum(coverage), rtol=.001, atol=.001)
    for layer in meta["layers"]:
        assert "starless" not in layer
        assert not (session / "project" / f"{layer['key']}.natural.f16").exists()
        assert (session / "project" / layer["edge_file"]).is_file()


def test_storage_protects_every_mosaic_source_even_without_selection_or_catalog(folder):
    session = folder / "sessions" / "art"
    write_json(session / "session.json", {"name": "Art"})
    write_json(session / "project" / "meta.json", {"layers": [{
        "key": "m", "source": r"C:\cache\first.fits", "tiles": [
            {"key": "a", "source": r"C:\cache\first.fits"},
            {"key": "b", "source": r"C:\cache\second.fits"}]}]})
    downloads = folder / "downloads"
    downloads.mkdir()
    for name in ("first.fits", "second.fits"):
        (downloads / name).write_bytes(b"science")
    inventory = Storage(folder).inventory()
    assert all(f["status"] == "in_use" for f in inventory["downloads"])
    assert all(f["references"]["live"] == [{"id": "art", "name": "Art"}] for f in inventory["downloads"])
