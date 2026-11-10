"""Render controlled examples through NightBloom's native build and reference compositor."""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import math
import subprocess
import tempfile
from pathlib import Path
from types import SimpleNamespace

import numpy as np
from astropy.io import fits
from PIL import Image, ImageDraw, ImageFont
from scipy import ndimage as ndi

from astroimp.build import edge_distance
from astroimp.fill import fit_match
from astroimp.mosaic import combine
from astroimp.render import render, stretch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
ASSETS = HERE / "assets"
PANEL = (840, 487)
BACKGROUND = "#10131c"
FOREGROUND = "#edf0f6"
MUTED = "#a8b1c4"
ACCENT = "#c5b5ff"


def save_image(name: str, pixels: np.ndarray) -> None:
    Image.fromarray(np.round(np.clip(pixels, 0, 1) * 255).astype(np.uint8)).save(ASSETS / name)


def read_plane(folder: Path, meta: dict) -> np.ndarray:
    return np.fromfile(folder / meta["layers"][0]["file"], dtype="<f2").astype(np.float32).reshape(
        meta["height"], meta["width"])


def native_build(session: Path, destination: Path, grid: dict, sources: list[dict], merge: bool) -> dict:
    manifest = destination.with_suffix(".json")
    manifest.write_text(json.dumps({
        "session": str(session), "destination": str(destination), "grid": grid,
        "sources": sources, "merge": merge,
    }), encoding="utf-8")
    subprocess.run([
        "cargo", "run", "--offline", "--quiet",
        "--manifest-path", str(HERE / "native" / "Cargo.toml"),
        "--target-dir", str(ROOT / "src-tauri" / "target"),
        "--", str(manifest),
    ], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    return json.loads((destination / "meta.json").read_text(encoding="utf-8"))


def observation(identifier: str) -> dict:
    return {
        "obs_id": identifier, "collection": "HST", "instrument": "controlled-demo",
        "filter": "same-band", "proposal": "illustrative-split", "wavelength_nm": 656,
    }


def font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(
        str(Path(r"C:\Windows\Fonts") / ("seguisb.ttf" if bold else "segoeui.ttf")), size)


def wrap(draw: ImageDraw.ImageDraw, text: str, face: ImageFont.FreeTypeFont, width: int) -> list[str]:
    lines, line = [], ""
    for word in text.split():
        candidate = f"{line} {word}".strip()
        if line and draw.textlength(candidate, font=face) > width:
            lines.append(line)
            line = word
        else:
            line = candidate
    return [*lines, line] if line else lines


def pair(number: str, title: str, when: str, before: str, after: str,
         labels: tuple[str, str], explanation: str, caveat: str) -> Image.Image:
    image = Image.new("RGB", (1760, 720), BACKGROUND)
    draw = ImageDraw.Draw(image)
    draw.text((32, 22), f"{number}  {title}", font=font(32, True), fill=FOREGROUND)
    draw.text((32, 66), when, font=font(19), fill=MUTED)
    for x, name, label, color in ((32, before, labels[0], MUTED), (888, after, labels[1], ACCENT)):
        draw.text((x, 105), label, font=font(20, True), fill=color)
        with Image.open(ASSETS / name) as source:
            panel = source.convert("RGB").resize(PANEL, Image.Resampling.LANCZOS)
        image.paste(panel, (x, 137))
    y = 643
    for line in wrap(draw, explanation, font(20), 1696):
        draw.text((32, y), line, font=font(20), fill=FOREGROUND)
        y += 27
    draw.text((32, 693), caveat, font=font(16), fill=MUTED)
    return image


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True, help="Existing Rosette DSS science FITS")
    args = parser.parse_args()
    source = args.source.resolve()
    ASSETS.mkdir(parents=True, exist_ok=True)
    signature = (source.stat().st_size, source.stat().st_mtime_ns)
    source_hash = hashlib.sha256(source.read_bytes()).hexdigest()
    with fits.open(source, mode="readonly", memmap=True) as hdus:
        raw = np.array(hdus[0].data, dtype=np.float32)
        header = hdus[0].header.copy()
    if raw.shape != (812, 1400) or not np.isfinite(raw).all():
        raise ValueError("These comparisons require the finite 1400x812 Rosette fixture")
    height, width = raw.shape
    grid = {"width": width, "height": height, "wcs": dict(header)}
    metrics = {
        "source": str(source), "source_sha256": source_hash,
        "mode": "controlled examples on real Rosette DSS pixels, not live project screenshots",
        "native_build": "actual native::prepare_build",
        "fill": "actual fit_match, stretch, edge_fade and render; blurred same-band surroundings are a stand-in",
        "sky": "actual Snapshot.tsx autoTone extracted and run in Chromium; actual SVG filter order",
    }
    with tempfile.TemporaryDirectory(prefix="nightbloom-examples-", dir=HERE) as temporary:
        library = Path(temporary)
        session = library / "sessions" / "demo"
        session.mkdir(parents=True)
        full_source = library / "rosette.fits"
        fits.PrimaryHDU(raw[::-1], header).writeto(full_source)
        norm_folder = session / "normalized"
        norm_meta = native_build(session, norm_folder, grid, [{
            "observation": observation("normalization"), "path": str(full_source),
        }], False)
        normalized = read_plane(norm_folder, norm_meta)
        stats = norm_meta["layers"][0]["stats"]
        tone = {"black": 0, "white": 1.3, "k": 10, "stretch": "asinh", "intensity": 1}
        # Counts share one unit conversion; the comparison isolates background removal.
        before = stretch(raw / stats["scale"], tone)
        after = stretch(normalized, tone)
        save_image("normalization-before.png", before)
        save_image("normalization-after.png", after)
        reference = (raw - np.float32(stats["background"])) / np.float32(stats["scale"])
        if np.max(np.abs(normalized[4:-4, 4:-4] - reference[4:-4, 4:-4])) > .005:
            raise AssertionError("Native output does not reproduce the original science pixels")
        metrics["normalization"] = stats

        left, right = raw.copy(), raw.copy() * np.float32(1.55) + np.float32(2200)
        left[:, int(width * .7):] = np.nan
        right[:, :int(width * .4)] = np.nan
        split_sources = []
        for name, data in (("left", left), ("right", right)):
            path = library / f"{name}.fits"
            fits.PrimaryHDU(data[::-1], header).writeto(path)
            split_sources.append({"observation": observation(name), "path": str(path)})
        mosaic_folder = session / "mosaic"
        mosaic_meta = native_build(session, mosaic_folder, grid, split_sources, True)
        mosaic_output = read_plane(mosaic_folder, mosaic_meta)
        fits_ = mosaic_meta["layers"][0]["tiles"]
        expected = (1 / 1.55, -2200 / 1.55)
        recovered = (fits_[1]["gain"], fits_[1]["offset"])
        if not math.isclose(recovered[0], expected[0], abs_tol=.005):
            raise AssertionError(f"Native overlap calibration failed: {recovered}")
        if not math.isclose(recovered[1], expected[1], abs_tol=5):
            raise AssertionError(f"Native overlap offset failed: {recovered}")
        identity = combine([left, right], [{"gain": 1, "offset": 0}] * 2)
        mosaic_stats = mosaic_meta["layers"][0]["stats"]
        save_image("mosaic-before.png", stretch(
            (identity - mosaic_stats["background"]) / mosaic_stats["scale"], tone))
        save_image("mosaic-after.png", stretch(mosaic_output, tone))
        reference_error = float(np.mean(np.abs(
            mosaic_output[8:-8, 8:-8] - reference[8:-8, 8:-8])))
        if reference_error > .015:
            raise AssertionError(f"Matched mosaic no longer follows the original field: {reference_error}")
        metrics["mosaic"] = {
            "introduced_right_gain": 1.55, "introduced_right_offset": 2200,
            "recovered": fits_, "normalized_mean_absolute_error": reference_error,
            "note": "The before image uses the same 64-pixel feathering, with gain=1 and offset=0.",
        }

        covered = np.zeros(raw.shape, bool)
        covered[int(height * .18):int(height * .82), int(width * .2):int(width * .8)] = True
        subject = np.where(covered, normalized, 0).astype(np.float32)
        survey = (ndi.gaussian_filter(normalized, 2.8) * .72 + .18).astype(np.float32)
        subject_layer = {
            **tone, "id": "subject", "source": "subject", "name": "Subject",
            "mode": "solid", "color": "#eab891", "blend": "add", "visible": True,
            "opacity": 1, "edge": {"feather": 0, "irregularity": 0},
        }
        survey_layer = {
            **copy.deepcopy(subject_layer), "id": "survey", "source": "survey",
            "name": "Illustrative surroundings", "black": 0, "white": 1,
        }
        edges = {"subject": edge_distance(covered), "survey": edge_distance(np.ones(raw.shape, bool))}
        project = SimpleNamespace(
            shape=raw.shape, step=1, window=None, keys=["subject", "survey"],
            full=[subject, survey], lum=None,
            meta={"width": width, "height": height, "layers": [
                {"key": "subject", "wavelength_nm": 656}, {"key": "survey", "wavelength_nm": 656},
            ]}, edge=lambda key: edges[key],
        )
        state = {"layers": [subject_layer, survey_layer], "global": {}}
        initial = render(project, state, collect=False)[0]
        matched = copy.deepcopy(state)
        match = fit_match(stretch(survey[covered], survey_layer), stretch(subject[covered], subject_layer))
        matched["layers"][1]["toneMatch"] = match
        matched_output = render(project, matched, collect=False)[0]
        feathered = copy.deepcopy(matched)
        feather = min(int(width * .6), int(height * .64)) * .05
        feathered["layers"][0]["edge"]["feather"] = feather
        feathered_output = render(project, feathered, collect=False)[0]
        save_image("fill-before.png", initial)
        save_image("fill-matched.png", matched_output)
        save_image("fill-feathered.png", feathered_output)
        pre_error = float(np.mean(np.abs(
            stretch(survey[covered], survey_layer) - stretch(subject[covered], subject_layer))))
        post_error = float(np.mean(np.abs(
            stretch(survey[covered], {**survey_layer, "toneMatch": match})
            - stretch(subject[covered], subject_layer))))
        if post_error >= pre_error:
            raise AssertionError("Survey match did not reduce overlap brightness mismatch")
        interior = ndi.distance_transform_edt(covered) > feather + 2
        if not np.array_equal(matched_output[interior], feathered_output[interior]):
            raise AssertionError("Edge feather changed the fully covered interior")
        metrics["fill"] = {
            "tone_match": match, "overlap_error_before": pre_error, "overlap_error_after": post_error,
            "edge_feather_pixels": feather,
            "note": "Same-band blur deliberately stands in for a lower-resolution survey; additive layer blending is unchanged.",
        }
        save_image("sky-before.png", after)
        subprocess.run(["node", str(HERE / "sky-auto.cjs")], cwd=ROOT, check=True)
        metrics["sky"] = json.loads((ASSETS / "sky-metrics.json").read_text())

    if (source.stat().st_size, source.stat().st_mtime_ns) != signature:
        raise AssertionError("Original science source changed")
    if hashlib.sha256(source.read_bytes()).hexdigest() != source_hash:
        raise AssertionError("Original science source content changed")
    metrics["source_unchanged"] = True
    cards = [
        pair("1", "Build normalization", "Automatic during Build / Rebuild",
             "normalization-before.png", "normalization-after.png",
             ("BEFORE  Background retained", "AFTER  Background removed"),
             "The estimated sky pedestal is removed; faint structure separates from the gray background.",
             "Real DSS pixels. Both panels use the same unit scale and asinh display; raw FITS are untouched."),
        pair("2", "Same-filter mosaic matching", "Automatic when Merge tiles combines overlapping images",
             "mosaic-before.png", "mosaic-after.png",
             ("BEFORE  Feathering alone", "AFTER  Feathering + overlap calibration"),
             "Exposure/background differences are matched before combining; feathering alone cannot fix the mismatch.",
             "Controlled split of one real field: right tile deliberately made 1.55x brighter + 2200 counts."),
        pair("3a", "Fill around: survey tone matching", "Only when Fill around is explicitly requested (Python workflow)",
             "fill-before.png", "fill-matched.png",
             ("BEFORE  Surroundings too bright", "AFTER  Survey tone matched"),
             "The surrounding image's gain and offset are fitted against the subject over their shared sky.",
             "Illustrative blurred same-band surroundings, not a live Fill around run. Layer blending is unchanged."),
        pair("3b", "Fill around: subject edge feathering", "Applied with Fill around after tone matching",
             "fill-matched.png", "fill-feathered.png",
             ("BEFORE  Hard subject footprint", "AFTER  Soft contribution near edges"),
             "The subject layer tapers near its footprint boundary. The fully covered interior is unchanged.",
             "Matching and feathering do not guarantee a seamless composition; additive overlap can still be brighter."),
        pair("4", "Sky Auto", "Only when Auto is clicked in the Sky view tone controls",
             "sky-before.png", "sky-after.png",
             ("BEFORE  Neutral snapshot display", "AFTER  Histogram-based display tone"),
             "Black/white points and midtones are adjusted for this snapshot; no science data or editor edits change.",
             "Actual frontend Auto code + SVG filter in headless Chromium. Clipped details cannot be recovered."),
    ]
    for name, card in zip(
            ["01-normalization", "02-mosaic", "03a-fill-tone", "03b-fill-feather", "04-sky-auto"], cards):
        card.save(HERE / f"{name}.jpg", quality=94, subsampling=0)
    sheet = Image.new("RGB", (1760, 110 + 720 * len(cards)), BACKGROUND)
    draw = ImageDraw.Draw(sheet)
    draw.text((32, 18), "NIGHTBLOOM  /  What each automatic adjustment contributes",
              font=font(30, True), fill=FOREGROUND)
    draw.text((32, 65), "Real Rosette DSS imagery; controlled examples isolate each operation, not a single sequential edit.",
              font=font(20), fill=MUTED)
    for index, card in enumerate(cards):
        sheet.paste(card, (0, 110 + 720 * index))
    sheet.save(HERE / "before-after.jpg", quality=94, subsampling=0)
    (HERE / "metrics.json").write_text(json.dumps(metrics, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: metrics[key] for key in ("normalization", "fill", "sky", "source_unchanged")}, indent=2))


if __name__ == "__main__":
    main()
