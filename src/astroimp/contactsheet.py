"""Observatory-style plates documenting the saved composition and its visible layers."""
from __future__ import annotations

import copy
import json
import math
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont, PngImagePlugin
from scipy import ndimage as ndi

from astroimp.filters import describe
from astroimp.render import Project, gradient_lut, log_position, output, render, warp
from astroimp.view import inside_crop, view_frame

BACKGROUND = "#1b1c1e"
PANEL = "#232427"
INK = "#dcd8cf"
MUTED = "#8d8b86"
AMBER = "#e9b35a"
WIDTH, HEIGHT = 3600, 2025


def view_transform(rgb: np.ndarray, global_state: dict, *, tone_map: bool = False,
                   image_size: tuple[int, int] | None = None) -> np.ndarray:
    """Sample the GPU's bounding frame, including the anchored crop's empty corners."""
    h, w = rgb.shape[:2]
    iw, ih = image_size or (w, h)
    meta = {"width": iw, "height": ih}
    crop = view_frame(meta, global_state)
    ow, oh = max(1, math.floor(w * crop["w"] + .5)), max(1, math.floor(h * crop["h"] + .5))
    yy, xx = np.mgrid[:oh, :ow].astype(np.float32)
    px = (crop["x"] + (xx + .5) / ow * crop["w"] - .5) * iw
    py = (crop["y"] + (yy + .5) / oh * crop["h"] - .5) * ih
    angle = math.radians(global_state.get("rotation", 0))
    c, s = math.cos(angle), math.sin(angle)
    qx, qy = (c * px + s * py) / iw, (-s * px + c * py) / ih
    if global_state.get("flipX"):
        qx = -qx
    qx, qy = qx + .5, qy + .5
    sampled = np.stack([ndi.map_coordinates(rgb[..., channel],
                       [qy * h - .5, qx * w - .5], order=1, mode="nearest")
                        for channel in range(3)], -1)
    if tone_map:
        sampled = output(sampled, global_state)
    inside = ((qx >= 0) & (qx <= 1) & (qy >= 0) & (qy <= 1)
              & inside_crop(meta, global_state, px / iw + .5, py / ih + .5))
    return np.where(inside[..., None], sampled, np.array([.0784, .0824, .0902], np.float32))


def _font(size: int, role: str = "text"):
    candidates = {
        "title": ["Sitka.ttc", "SitkaVF.ttf", "georgia.ttf"],
        "text": ["SegUIVar.ttf", "segoeui.ttf"],
        "mono": ["CascadiaMono.ttf", "consola.ttf"],
    }[role]
    for name in candidates:
        try:
            return ImageFont.truetype(str(Path("C:\\Windows\\Fonts") / name), size)
        except OSError:
            pass
    return ImageFont.load_default(size=size)


def _image(rgb: np.ndarray) -> Image.Image:
    return Image.fromarray(np.round(np.clip(rgb, 0, 1) * 255).astype(np.uint8))


def _fit(image: Image.Image, box: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    x, y, w, h = box
    ratio = min(w / image.width, h / image.height)
    rw, rh = max(1, round(image.width * ratio)), max(1, round(image.height * ratio))
    return x + (w - rw) // 2, y + (h - rh) // 2, rw, rh


def _paste(plate: Image.Image, image: Image.Image, box: tuple[int, int, int, int]):
    x, y, w, h = _fit(image, box)
    plate.paste(image.resize((w, h), Image.Resampling.LANCZOS), (x, y))
    return x, y, w, h


def _text(draw, xy, text, font, fill=INK, width=None):
    text = str(text)
    if width:
        while len(text) > 1 and draw.textlength(text, font=font) > width:
            text = text[:-2].rstrip("…") + "…"
    draw.text(xy, text, font=font, fill=fill)


def _swatch(layer: dict) -> Image.Image:
    if layer["mode"] == "tint":
        return Image.new("RGB", (160, 16), layer["color"])
    stops = layer["gradient"] if layer["mode"] == "gradient" else [
        {"t": log_position(stop["nm"]), "color": stop["color"]}
        for stop in layer["spectral"]["stops"]]
    return _image(np.broadcast_to(gradient_lut(stops)[None, ...], (16, 256, 3)))


def make_contact_sheet(project: Project, state: dict, name: str,
                       date: datetime | None = None) -> Image.Image:
    """Render a plate; PNG metadata also records row IDs and the exact final-image bounds."""
    date = date or datetime.now(timezone.utc)
    visible = [layer for layer in state["layers"] if layer.get("visible", True)]
    row_height = max(170, min(270, 1730 // max(1, len(visible))))
    height = max(HEIGHT, 290 + row_height * len(visible))
    plate = Image.new("RGB", (WIDTH, height), BACKGROUND)
    draw = ImageDraw.Draw(plate)
    title, body, mono, small = _font(66, "title"), _font(28), _font(29, "mono"), _font(24)
    _text(draw, (62, 30), name, title, width=2750)
    _text(draw, (65, 120), "ASTROIMP   /   COMPOSITION CONTACT SHEET", small, AMBER)
    full_w, full_h = project.meta["width"], project.meta["height"]
    crop = view_frame(project.meta, state["global"])
    out_w, out_h = (max(1, math.floor(crop[k] + .5)) for k in ("width", "height"))
    size = (f"{out_w} × {out_h} px" if (out_w, out_h) == (full_w, full_h)
            else f"{out_w} × {out_h} px  (cropped from {full_w} × {full_h})")
    _text(draw, (65, 164), f"{date:%Y-%m-%d}   ·   {size}"
          f"   ·   {len(visible)} visible filters", body, MUTED)
    draw.line((62, 222, 3538, 222), fill="#42413e", width=2)
    for x, label in [(65, "SOURCE / FILTER"), (1235, "COLOUR / RECIPE"), (2405, "FINAL COMPOSITE")]:
        _text(draw, (x, 242), label, small, AMBER)
    g = state["global"]
    size = (project.meta["width"], project.meta["height"])
    metadata = {layer["key"]: layer for layer in project.meta["layers"]}
    for index, layer in enumerate(visible):
        y = 292 + index * row_height
        for x in (62, 1232):
            draw.rounded_rectangle((x, y, x + 1138, y + row_height - 12), radius=8, fill=PANEL)
        source = metadata.get(layer["source"], {})
        if layer["source"] in project.keys:
            data = project.full[project.keys.index(layer["source"])]
        elif project.lum:
            data = project.lum[0]
        else:
            data = np.mean(project.full, axis=0)
        finite = data[np.isfinite(data) & (data > 0)]
        low, high = np.percentile(finite, [.5, 99.5]) if finite.size else (0, 1)
        gray = np.arcsinh(15 * np.clip((np.nan_to_num(data) - low) / max(high - low, 1e-6), 0, 1)) / math.asinh(15)
        gray = warp(gray, layer.get("transform") or {})
        thumb = _image(view_transform(np.repeat(gray[..., None], 3, axis=-1), g, image_size=size))
        thumb_size = row_height - 36
        _paste(plate, thumb, (80, y + 12, thumb_size, thumb_size))
        tx, ty = 102 + thumb_size, y + 12
        nm = source.get("wavelength_nm")
        wave = f"{nm / 1000:g} µm" if nm and nm >= 1000 else f"{nm:g} nm" if nm else "—"
        _text(draw, (tx, ty), source.get("filter", layer["name"]), mono, width=760)
        _text(draw, (tx + 330, ty + 2), wave, body, MUTED)
        info = source.get("info") or describe(source.get("filter", ""), nm)
        caption = " · ".join(str(info[key]) for key in ("line", "shows") if info.get(key))
        _text(draw, (tx, ty + 48), caption, body, width=1170 - tx)
        _text(draw, (tx, ty + 91), " · ".join(filter(None, [source.get("collection"), source.get("instrument")])),
              small, MUTED, width=1170 - tx)
        alone = copy.deepcopy(state)
        alone["layers"] = [{**layer, "opacity": 1, "blend": "add"}]
        color, _ = render(project, alone)
        _paste(plate, _image(view_transform(np.clip(color, 0, 1), g, image_size=size)),
               (1250, y + 12, thumb_size, thumb_size))
        tx = 1272 + thumb_size
        _text(draw, (tx, ty), layer["mode"].upper(), mono)
        plate.paste(_swatch(layer).resize((152, 16)), (tx + 218, ty + 14))
        if layer["mode"] == "tint":
            _text(draw, (tx + 390, ty + 3), layer["color"], small, MUTED)
        _text(draw, (tx, ty + 47), f"{layer['blend']}   ·   opacity {layer['opacity']:.2f}"
              f"   ·   intensity {layer['intensity']:.2f}", small)
        _text(draw, (tx, ty + 84), f"{layer['stretch']}  k {layer['k']:g}"
              f"   ·   black {layer['black']:.3f} / white {layer['white']:.3f}", small, MUTED)
        feather = (layer.get("edge") or {}).get("feather", 0)
        if feather:
            _text(draw, (tx, ty + 118), f"Edge softness {feather:g} px", small, MUTED)
    right = (2402, 292, 1136, height - 350)
    draw.rounded_rectangle((right[0], right[1], 3538, height - 58), radius=8, fill=PANEL)
    acc, _ = render(project, state)
    final = _image(view_transform(acc, g, tone_map=True, image_size=size))
    legend_rows = math.ceil(len(visible) / 3)
    final_box = _paste(plate, final, (2426, 318, 1088, height - 460 - legend_rows * 58))
    legend_y = max(318 + final_box[3] + 34, height - 98 - legend_rows * 58)
    _text(draw, (2430, legend_y - 42), "Filter → colour", body)
    for i, layer in enumerate(visible):
        x, y = 2430 + i % 3 * 359, legend_y + i // 3 * 58
        plate.paste(_swatch(layer).resize((42, 20)), (x, y + 9))
        source = metadata.get(layer["source"], {})
        _text(draw, (x + 54, y), source.get("filter", layer["name"]), small, width=285)
    if not visible:
        _text(draw, (82, 320), "No visible layers", body, MUTED)
    plate.info["astroimp"] = json.dumps({"visible_layers": [layer["id"] for layer in visible],
                                        "final_box": final_box})
    return plate


def export_contact_sheet(session: Path, state: dict | None = None) -> Path:
    """Save a uniquely timestamped PNG using the session's current saved edits."""
    session = Path(session)
    state = state if state is not None else json.loads((session / "edits.json").read_text(encoding="utf-8"))
    details = json.loads((session / "session.json").read_text(encoding="utf-8"))
    now = datetime.now(timezone.utc)
    project = Project(session / "project", max_side=720)
    plate = make_contact_sheet(project, state,
                               details.get("name") or session.name, now)
    destination = session / "exports" / f"contact-sheet-{now:%Y%m%dT%H%M%S%fZ}.png"
    destination.parent.mkdir(parents=True, exist_ok=True)
    info = PngImagePlugin.PngInfo()
    info.add_text("astroimp", plate.info["astroimp"])
    plate.save(destination, pnginfo=info)
    from astroimp.export_history import capture
    try:
        capture(session, destination, state, project.meta)
    except BaseException:
        destination.unlink(missing_ok=True)
        raise
    return destination
