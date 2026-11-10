"""Natural-language-friendly edits to the saved editor state (used by `astroimp edit`)."""
from __future__ import annotations

import re
import math
from copy import deepcopy

OUTPUT_DEFAULTS = {"exposure": 0, "highlights": 0, "black": 0, "white": 1, "midtones": 1}


def discard_star_state(state: dict) -> dict:
    """Copy ordinary edits unchanged, discarding abandoned star-only state."""
    state = deepcopy(state)
    for key in ("starClasses", "starOverrides", "starWeights", "starsMigration"):
        state.pop(key, None)
    settings = state.get("global", {})
    settings.pop("stars", None)
    settings.pop("parallax", None)
    for point in state.get("checkpoints") or []:
        if isinstance(point.get("state"), dict):
            point["state"] = discard_star_state(point["state"])
    return state


def normalise(state: dict) -> dict:
    """Supply neutral defaults for ordinary layer and output tools."""
    state = discard_star_state(state)
    settings = state.setdefault("global", {})
    settings["output"] = {**OUTPUT_DEFAULTS, **(settings.get("output") or {})}
    for layer in state.get("layers", []):
        layer.setdefault("invert", False)
        layer.setdefault("curve", [])
        layer["detail"] = {"amount": 0, "radius": 8, **(layer.get("detail") or {})}
        layer.setdefault("clip", None)
    return state

# Same limits the editor's sliders enforce, so agent edits stay inside what the UI can show.
LIMITS = {
    "black": (0, 1), "white": (.01, 3), "k": (.1, 30), "intensity": (0, 2), "opacity": (0, 1),
    "toneMatch.gain": (.05, 20), "toneMatch.offset": (-1, 1),
    "detail.amount": (0, 3), "detail.radius": (1, 64),
    "mask.black": (0, 1), "mask.white": (.01, 2), "transform.scale": (.1, 3),
    "transform.rotation": (-180, 180), "transform.x": (-1, 1), "transform.y": (-1, 1),
    "output.exposure": (-3, 3), "output.highlights": (0, 1), "rotation": (-180, 180),
    "output.black": (0, 1), "output.white": (.01, 3), "output.midtones": (.3, 4),
    "crop.rotation": (-180, 180),
}
CHOICES = {
    "mode": {"tint", "gradient", "spectral"},
    "stretch": {"linear", "asinh", "log", "gamma"},
    "blend": {"normal", "add", "screen", "multiply", "overlay", "softlight", "lighten", "difference", "subtract", "darken"},
}
OPERATION = re.compile(r"^([A-Za-z_][\w.]*)\s*(\*=|\+=|-=|=)\s*(.+)$")


def parse_operations(expressions: list[str]) -> list[tuple[str, str, object]]:
    operations = []
    for expression in expressions:
        match = OPERATION.match(expression.strip())
        if not match:
            raise ValueError(f"Expected key=value, key*=factor or key+=delta: {expression!r}")
        key, op, raw = match.groups()
        value: object = raw
        if raw.lower() in {"true", "false"}:
            value = raw.lower() == "true"
        else:
            try:
                value = float(raw)
            except ValueError:
                if op != "=":
                    raise ValueError(f"{expression!r}: *= and += need a number.") from None
        operations.append((key, op, value))
    return operations


def match_layers(state: dict, meta: dict, target: str) -> list[dict]:
    filters = {layer["key"]: layer["filter"].lower() for layer in meta["layers"]}
    target = target.strip()
    if target in {"*", "all"}:
        return state["layers"]
    if target.isdigit():
        index = int(target) - 1
        if not 0 <= index < len(state["layers"]):
            raise ValueError(f"Layer number {target} is out of range 1-{len(state['layers'])}.")
        return [state["layers"][index]]
    wanted = {part.strip().lower() for part in target.split(",") if part.strip()}
    found = [layer for layer in state["layers"]
             if layer["id"].lower() in wanted or layer["name"].lower() in wanted
             or filters.get(layer["source"], "luminance") in wanted]
    if not found:
        names = ", ".join(layer["name"] for layer in state["layers"])
        raise ValueError(f"No layer matches {target!r}. Layers: {names}")
    return found


def _set(obj: dict, key: str, op: str, value, limit_key: str, *, fallback=None) -> tuple[object, object]:
    if key.split(".")[0] in {"stars", "parallax", "starOverrides", "starWeights", "starClasses"}:
        raise ValueError(f"Unknown setting {key!r}.")
    parts = key.split(".")
    for part in parts[:-1]:
        if not isinstance(obj.get(part), dict):
            raise ValueError(f"Unknown setting {key!r}.")
        obj = obj[part]
    leaf = parts[-1]
    if leaf not in obj and limit_key != "crop.rotation":
        raise ValueError(f"Unknown setting {key!r}.")
    old = obj.get(leaf, fallback)
    if limit_key in {"crop.rotation", "output.black", "output.white", "output.midtones"} and (
            not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value)):
        raise ValueError(f"{limit_key} must be a finite number.")
    if limit_key in {"invert", "mask.invert"} and (op != "=" or not isinstance(value, bool)):
        raise ValueError(f"{limit_key} must be true or false.")
    if op == "=":
        new = value
        choice = limit_key if limit_key in CHOICES else leaf
        if choice in CHOICES and new not in CHOICES[choice]:
            raise ValueError(f"{key} must be one of {sorted(CHOICES[choice])}.")
        if leaf == "color" and not re.fullmatch(r"#[0-9a-fA-F]{6}", str(new)):
            raise ValueError("color must be #rrggbb.")
    else:
        if not isinstance(old, (int, float)) or isinstance(old, bool):
            raise ValueError(f"{key} is not numeric.")
        new = old * value if op == "*=" else old + value if op == "+=" else old - value
    if limit_key in LIMITS and isinstance(new, (int, float)) and not isinstance(new, bool):
        low, high = LIMITS[limit_key]
        new = round(min(high, max(low, float(new))), 4)
    obj[leaf] = new
    return old, new


def apply(state: dict, meta: dict, target: str, expressions: list[str]) -> list[str]:
    """Apply operations to layers matching target, or to global settings when target is 'global'."""
    operations = parse_operations(expressions)
    changes = []
    if target.lower() == "global":
        settings = state["global"]
        settings["output"] = {**OUTPUT_DEFAULTS, **(settings.get("output") or {})}
        for key, op, value in operations:
            old, new = _set(state["global"], key, op, value, key,
                            fallback=state["global"].get("rotation", 0) if key == "crop.rotation" else None)
            changes.append(f"global {key}: {old} -> {new}")
        return changes
    for layer in match_layers(state, meta, target):
        layer.setdefault("invert", False)
        layer["detail"] = {"amount": 0, "radius": 8, **(layer.get("detail") or {})}
        for key, op, value in operations:
            if key == "brush":
                if op != "=" or value != "clear":
                    raise ValueError("brush only supports brush=clear.")
                count = len((layer.get("brush") or {}).get("strokes", []))
                layer["brush"] = {"strokes": []}
                changes.append(f"{layer['name']} brush: cleared {count} strokes")
                continue
            old, new = _set(layer, key, op, value, key)
            changes.append(f"{layer['name']} {key}: {old} -> {new}")
    return changes
