"""Precompute every combination using actual processing, never changing owner projects."""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace

import numpy as np
from astropy.io import fits
from PIL import Image
from scipy import ndimage as ndi

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE.parent / "2026-11-06-automatic-adjustments"))
from generate import native_build, observation, read_plane
from astroimp.build import edge_distance
from astroimp.fill import fit_match
from astroimp.mosaic import combine
from astroimp.render import hex_rgb, render, stretch

WIDTH, HEIGHT = 900, 522
MOSAIC, NORMALIZE, MATCH, FEATHER, AUTO = 1, 2, 4, 8, 16


def identity(path: Path) -> tuple[int, int, str]:
    stat = path.stat()
    return stat.st_size, stat.st_mtime_ns, hashlib.sha256(path.read_bytes()).hexdigest()


def fit_frame(raw: np.ndarray) -> np.ndarray:
    valid = np.isfinite(raw)
    yy, xx = np.nonzero(valid)
    if len(xx) < 100:
        raise ValueError("Not enough covered science pixels")
    raw = raw[yy.min():yy.max() + 1, xx.min():xx.max() + 1]
    factor = min(WIDTH / raw.shape[1], HEIGHT / raw.shape[0])
    size = (max(1, round(raw.shape[1] * factor)), max(1, round(raw.shape[0] * factor)))
    valid = np.isfinite(raw)
    values = Image.fromarray(np.where(valid, raw, 0)).resize(size, Image.Resampling.BILINEAR)
    weights = Image.fromarray(valid.astype(np.float32)).resize(size, Image.Resampling.BILINEAR)
    weights = np.asarray(weights)
    sampled = np.full((size[1], size[0]), np.nan, np.float32)
    np.divide(np.asarray(values), weights, out=sampled, where=weights > .99)
    output = np.full((HEIGHT, WIDTH), np.nan, np.float32)
    x, y = (WIDTH - size[0]) // 2, (HEIGHT - size[1]) // 2
    output[y:y + size[1], x:x + size[0]] = sampled
    return output


def built_plane(session: Path, band: str, signatures: dict, pedestal: float) -> tuple[np.ndarray, str]:
    folder = session / "project"
    meta_path = folder / "meta.json"
    signatures[meta_path] = identity(meta_path)
    meta = json.loads(meta_path.read_text())
    layer = next(layer for layer in meta["layers"] if layer["filter"] == band)
    shape = (meta["height"], meta["width"])
    plane_path, coverage_path = folder / layer["file"], folder / layer["coverage_file"]
    for path in (plane_path, coverage_path):
        signatures[path] = identity(path)
    plane = np.fromfile(plane_path, dtype="<f2").astype(np.float32).reshape(shape)
    valid = np.fromfile(coverage_path, dtype="u1").reshape(shape) > 0
    stats = layer["stats"]
    if "background" in stats and "scale" in stats:
        plane = plane * stats["scale"] + stats["background"]
        note = "Built science plane, with stored scale/background reversed; not original raw FITS."
    else:
        plane = plane + np.float32(pedestal)
        note = "Built science plane with a modeled sky pedestal; original raw normalization is unavailable."
    plane[~valid] = np.nan
    return fit_frame(plane), note


def save_pixels(path: Path, rgb: np.ndarray) -> None:
    Image.fromarray(np.round(np.clip(rgb, 0, 1) * 255).astype(np.uint8)).save(path)


def combinations(raw: np.ndarray, library: Path, identifier: str, color: str) -> tuple[dict, np.ndarray, dict]:
    session = library / "sessions" / identifier
    session.mkdir(parents=True)
    header = fits.Header({
        "CTYPE1": "RA---TAN", "CTYPE2": "DEC--TAN", "CRVAL1": 180., "CRVAL2": 0.,
        "CRPIX1": (WIDTH + 1) / 2, "CRPIX2": (HEIGHT + 1) / 2,
        "CDELT1": -1 / 3600, "CDELT2": 1 / 3600, "RADESYS": "ICRS",
    })
    grid = {"width": WIDTH, "height": HEIGHT, "wcs": dict(header)}
    left, right = raw.copy(), raw.copy()
    finite = raw[np.isfinite(raw)]
    contrast = float(np.percentile(finite, 99.5) - np.median(finite))
    offset = contrast * .22
    right = right * np.float32(1.55) + np.float32(offset)
    left[:, int(WIDTH * .7):] = np.nan
    right[:, :int(WIDTH * .4)] = np.nan
    inputs = []
    for name, data in (("left", left), ("right", right)):
        source = library / f"{identifier}-{name}.fits"
        fits.PrimaryHDU(data[::-1], header).writeto(source)
        inputs.append({"observation": observation(name), "path": str(source)})
    matched_folder = session / "matched"
    matched_meta = native_build(session, matched_folder, grid, inputs, True)
    unmatched = combine([left, right], [{"gain": 1, "offset": 0}] * 2)
    unmatched_source = library / f"{identifier}-unmatched.fits"
    fits.PrimaryHDU(unmatched[::-1], header).writeto(unmatched_source)
    unmatched_folder = session / "unmatched"
    unmatched_meta = native_build(session, unmatched_folder, grid, [{
        "observation": observation("unmatched"), "path": str(unmatched_source),
    }], False)
    common_scale = matched_meta["layers"][0]["stats"]["scale"]
    inputs_ = {}
    for calibrated, folder, meta in ((False, unmatched_folder, unmatched_meta), (True, matched_folder, matched_meta)):
        normalized = read_plane(folder, meta)
        stats = meta["layers"][0]["stats"]
        inputs_[(calibrated, True)] = normalized
        inputs_[(calibrated, False)] = (normalized * stats["scale"] + stats["background"]) / common_scale
    covered = np.isfinite(raw)
    subject_coverage = covered.copy()
    subject_coverage[:int(HEIGHT * .18)] = False
    subject_coverage[int(HEIGHT * .82):] = False
    subject_coverage[:, :int(WIDTH * .2)] = False
    subject_coverage[:, int(WIDTH * .8):] = False
    edges = {"subject": edge_distance(subject_coverage), "survey": edge_distance(covered)}
    tone = {"black": 0, "white": 1.3, "k": 10, "stretch": "asinh", "intensity": 1}
    subject_layer = {
        **tone, "id": "subject", "source": "subject", "name": "Subject",
        "mode": "solid", "color": color, "blend": "add", "visible": True,
        "opacity": 1, "edge": {"feather": 0, "irregularity": 0},
    }
    survey_layer = {**copy.deepcopy(subject_layer), "id": "survey", "source": "survey", "white": 1}
    frames, measured = {}, {}
    for calibrated in (False, True):
        for normalized in (False, True):
            values = inputs_[(calibrated, normalized)]
            subject = np.where(subject_coverage, values, 0).astype(np.float32)
            smooth = ndi.gaussian_filter(np.where(covered, values, 0), 2.8)
            weights = ndi.gaussian_filter(covered.astype(np.float32), 2.8)
            survey = (smooth / np.maximum(weights, 1e-5) * .72 + .18).astype(np.float32)
            survey[~covered] = 0
            project = SimpleNamespace(
                shape=raw.shape, step=1, window=None, keys=["subject", "survey"],
                full=[subject, survey], lum=None,
                meta={"width": WIDTH, "height": HEIGHT, "layers": [
                    {"key": "subject", "wavelength_nm": 656},
                    {"key": "survey", "wavelength_nm": 656},
                ]}, edge=lambda key: edges[key],
            )
            match = fit_match(
                stretch(survey[subject_coverage], survey_layer),
                stretch(subject[subject_coverage], subject_layer))
            for matched in (False, True):
                for feathered in (False, True):
                    bits = (MOSAIC if calibrated else 0) | (NORMALIZE if normalized else 0)
                    bits |= (MATCH if matched else 0) | (FEATHER if feathered else 0)
                    state = {"layers": [copy.deepcopy(subject_layer), copy.deepcopy(survey_layer)], "global": {}}
                    if matched:
                        state["layers"][1]["toneMatch"] = match
                    if feathered:
                        state["layers"][0]["edge"]["feather"] = min(WIDTH * .6, HEIGHT * .64) * .05
                    output = render(project, state, collect=False)[0]
                    output[~covered] = 0
                    frames[bits] = output
                    measured[bits] = {"tone_match": match if matched else None}
    expected = 1 / 1.55
    tiles = matched_meta["layers"][0]["tiles"]
    reference = next(index for index, tile in enumerate(tiles) if tile["reference"])
    fitted = tiles[1]["gain"] if reference == 0 else 1 / tiles[0]["gain"]
    if abs(fitted - expected) > .01:
        raise AssertionError(f"Native overlap calibration failed for {identifier}: {tiles}")
    baseline = stretch(inputs_[(True, True)], tone)[..., None] * hex_rgb(color)
    baseline[~covered] = 0
    return frames, baseline, {"native_calibration": tiles, "combinations": measured}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rosette", type=Path, required=True)
    args = parser.parse_args()
    signatures = {args.rosette: identity(args.rosette)}
    with fits.open(args.rosette, mode="readonly", memmap=True) as hdus:
        rosette = fit_frame(np.array(hdus[0].data, dtype=np.float32))
    pedestal = 4148.419 / 9670.163
    eagle, eagle_note = built_plane(ROOT / "data" / "sessions" / "20261023T050727Z",
                                    "F335M", signatures, pedestal)
    antennae, antennae_note = built_plane(ROOT / "data" / "sessions" / "20261025T050226Z",
                                          "F435W", signatures, pedestal)
    bases = [
        ("rosette", "Rosette Nebula", "DSS2 red", rosette, "#eab891", "Sampled raw science FITS."),
        ("eagle", "Eagle Nebula", "JWST F335M", eagle, "#c3b6ea", eagle_note),
        ("antennae", "Antennae Galaxies", "HST F435W", antennae, "#94cbd9", antennae_note),
    ]
    manifest = {"version": 2, "width": WIDTH, "height": HEIGHT, "bases": []}
    metrics, tasks = {}, []
    with tempfile.TemporaryDirectory(prefix="gallery-", dir=HERE) as temporary:
        library = Path(temporary)
        for identifier, name, source, raw, color, note in bases:
            frames, baseline, measured = combinations(raw, library, identifier, color)
            folder = HERE / "assets" / identifier
            folder.mkdir(parents=True, exist_ok=True)
            urls = {}
            for bits, rgb in frames.items():
                temporary_image = library / f"{identifier}-{bits}.png"
                save_pixels(temporary_image, rgb)
                output = folder / f"{bits:02}.jpg"
                with Image.open(temporary_image) as image:
                    image.save(output, quality=92, subsampling=0)
                urls[str(bits)] = f"assets/{identifier}/{bits:02}.jpg"
            baseline_image = library / f"{identifier}-baseline.png"
            save_pixels(baseline_image, baseline)
            with Image.open(baseline_image) as image:
                image.save(folder / "baseline.jpg", quality=92, subsampling=0)
            tasks.append({
                "input": str(baseline_image), "output": str(library / f"{identifier}-sky-auto.png"),
                "width": WIDTH, "height": HEIGHT, "id": identifier,
            })
            manifest["bases"].append({
                "id": identifier, "name": name, "source": source, "provenance": note,
                "frames": urls, "thumbnail": f"assets/{identifier}/baseline.jpg",
                "baseline": f"assets/{identifier}/baseline.jpg",
                "sky_auto": f"assets/{identifier}/sky-auto.jpg",
            })
            metrics[identifier] = measured
            print(f"Rendered 16 science/layer combinations for {name}.", flush=True)
        task_file = library / "sky-batch.json"
        task_file.write_text(json.dumps(tasks), encoding="utf-8")
        subprocess.run(["node", str(HERE / "sky-batch.cjs"), str(task_file)], cwd=ROOT, check=True)
        for task in json.loads(task_file.read_text()):
            output = HERE / "assets" / task["id"] / "sky-auto.jpg"
            with Image.open(task["output"]) as image:
                image.convert("RGB").save(output, quality=92, subsampling=0)
            metrics[task["id"]]["sky_auto"] = task["tone"]
            for bits in range(16, 32):
                obsolete = HERE / "assets" / task["id"] / f"{bits:02}.jpg"
                if obsolete.is_file():
                    obsolete.unlink()
    for path, original in signatures.items():
        if identity(path) != original:
            raise AssertionError(f"Owner source changed: {path}")
    for base in manifest["bases"]:
        if len(base["frames"]) != 16:
            raise AssertionError("Missing effect combination")
        unique = set()
        for url in base["frames"].values():
            path = HERE / url
            with Image.open(path) as image:
                image.verify()
            unique.add(hashlib.sha256(path.read_bytes()).hexdigest())
        if len(unique) < 8:
            raise AssertionError(f"Too few visibly distinct combinations for {base['name']}")
    (HERE / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    (HERE / "metrics.json").write_text(json.dumps({
        "sources_unchanged": True, "bases": metrics,
        "note": "All tile exposure differences and blurred survey surroundings are constructed examples. No live projects changed.",
    }, indent=2) + "\n", encoding="utf-8")
    print("Verified 48 build combinations, 3 separate Sky examples and unchanged owner inputs.", flush=True)


if __name__ == "__main__":
    main()
