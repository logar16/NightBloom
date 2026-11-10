"""Linear overlap calibration and coverage-weighted tile mosaics."""
from __future__ import annotations

import copy
import hashlib
import json

import numpy as np
from scipy import ndimage as ndi

VERSION = 1
FEATHER = 64


def groups(observations: list[dict], enabled: bool = True) -> list[list[dict]]:
    result = {}
    for obs in observations:
        # A proposal bounds the sky/programme being combined. Unknown proposals and
        # survey cutouts stay independent rather than guessing at their provenance.
        proposal = obs.get("proposal") or obs.get("proposal_id")
        identity = (obs.get("collection"), obs.get("instrument"), obs.get("filter"), str(proposal))
        if not enabled or not proposal or obs.get("hips_id"):
            identity = (obs["obs_id"],)
        result.setdefault(identity, []).append(obs)
    return list(result.values())


def key(group: list[dict]) -> str:
    from astroimp.build import safe_key
    if len(group) == 1:
        return safe_key(group[0]["obs_id"])
    obs = group[0]
    identity = [obs.get(k) for k in ("collection", "instrument", "filter", "proposal", "proposal_id")]
    digest = hashlib.sha256(json.dumps(identity).encode()).hexdigest()[:12]
    return safe_key(f"mosaic_{obs.get('filter')}_{digest}")


def _fit(a, b):
    keep = np.isfinite(a) & np.isfinite(b)
    a, b = a[keep].astype(float), b[keep].astype(float)
    if a.size < 64:
        return None
    # Trim bright cores and the faint noise floor after spatial median filtering.
    lo, hi = np.percentile(a, [5, 95])
    low, high = np.percentile(b, [5, 95])
    keep = (a > lo) & (a < hi) & (b > low) & (b < high)
    for _ in range(5):
        x, y = b[keep], a[keep]
        if x.size < 32 or np.std(x) < max(1e-12, abs(np.mean(x))*1e-7):
            return None
        gain = np.cov(x, y, ddof=0)[0, 1] / np.var(x)
        offset = np.median(y - gain*x)
        residual = a - (gain*b + offset)
        scatter = 1.4826*np.median(np.abs(residual[keep] - np.median(residual[keep])))
        keep &= np.abs(residual) <= max(4*scatter, 1e-10)
    if not np.isfinite(gain) or gain <= 0:
        return None
    return float(gain), float(offset), int(keep.sum())


def calibrate(images: list[np.ndarray]) -> list[dict]:
    """Solve log gains then offsets over all robust pairwise overlap constraints."""
    valid = [np.isfinite(image) for image in images]
    reference = int(np.argmax([mask.sum() for mask in valid]))
    # Bounded samples preserve spatial filtering at the build scale without keeping
    # a second full-resolution stack. Erode before sampling to reject tile borders.
    stride = max(1, int(np.ceil(max(images[0].shape)/768)))
    samples = [ndi.median_filter(np.where(mask, image, 0), size=5)[::stride, ::stride]
               for image, mask in zip(images, valid)]
    masks = [ndi.binary_erosion(mask, iterations=4)[::stride, ::stride] for mask in valid]
    edges = []
    for i in range(len(images)):
        for j in range(i+1, len(images)):
            overlap = masks[i] & masks[j]
            fit = _fit(samples[i][overlap], samples[j][overlap])
            if fit:
                edges.append((i, j, *fit))
    # Independently anchor disconnected components; isolated tiles remain identity.
    remaining = set(range(len(images)))
    gains, offsets = np.ones(len(images)), np.zeros(len(images))
    connected = set()
    while remaining:
        root = reference if reference in remaining else max(remaining, key=lambda i: valid[i].sum())
        component = {root}
        while True:
            expanded = component | {j for i, j, *_ in edges if i in component} | {
                i for i, j, *_ in edges if j in component}
            if expanded == component:
                break
            component = expanded
        remaining -= component
        variables = sorted(component - {root})
        if not variables:
            continue
        connected |= component
        rows, log_values, offset_values = [], [], []
        local_edges = [e for e in edges if e[0] in component]
        for i, j, slope, intercept, count in local_edges:
            row = np.zeros(len(variables))
            if i != root:
                row[variables.index(i)] = -1
            if j != root:
                row[variables.index(j)] = 1
            weight = np.sqrt(count)
            rows.append(row*weight)
            log_values.append(np.log(slope)*weight)
        solution = np.linalg.lstsq(rows, log_values, rcond=None)[0]
        gains[variables] = np.exp(solution)
        for i, j, slope, intercept, count in local_edges:
            offset_values.append(gains[i]*intercept*np.sqrt(count))
        offsets[variables] = np.linalg.lstsq(rows, offset_values, rcond=None)[0]
    return [{"gain": float(g), "offset": float(o), "reference": i == reference,
             "no_overlap": i not in connected}
            for i, (g, o) in enumerate(zip(gains, offsets))]


def feather_weight(valid: np.ndarray, radius: float = FEATHER) -> np.ndarray:
    distance = ndi.distance_transform_edt(np.pad(valid, 1))[1:-1, 1:-1]
    return np.minimum(distance / max(1, radius), 1).astype(np.float32)


def combine(images, calibration, radius=FEATHER, *, destination=None):
    total = np.zeros(images[0].shape, np.float32)
    weight = np.zeros_like(total)
    for image, tile in zip(images, calibration):
        valid = np.isfinite(image)
        local = feather_weight(valid, radius)
        total += np.where(valid, image*tile["gain"] + tile["offset"], 0)*local
        weight += local
    result = destination if destination is not None else total
    np.divide(total, weight, out=result, where=weight > 0)
    result[weight == 0] = np.nan
    return result


def remap_edits(state: dict | None, old: dict, new: dict) -> dict | None:
    if not state:
        return state
    result = copy.deepcopy(state)
    aliases = {}
    old_members = {l["key"]: {t["key"] for t in l.get("tiles", [])} for l in old["layers"]}
    for layer in new["layers"]:
        members = {t["key"] for t in layer.get("tiles", [])} or {layer["key"]}
        for previous in old["layers"]:
            if previous["key"] in members or members & old_members[previous["key"]]:
                aliases.setdefault(previous["key"], layer["key"])
    aliases = {source: target for source, target in aliases.items() if source != target}
    if not aliases:
        return state
    for snapshot in [result, *(c["state"] for c in result.get("checkpoints", []))]:
        seen, layers = set(), []
        for layer in snapshot.get("layers", []):
            source = layer["source"]
            target = aliases.get(source, source)
            # Keep the first artistic layer from a collapsed group, preserving its ID.
            if source != target and target in seen:
                continue
            if source != target:
                layer["source"] = target
            mask = layer.get("mask", {})
            if mask.get("source") in aliases:
                mask["source"] = aliases[mask["source"]]
            layers.append(layer)
            seen.add(target)
        snapshot["layers"] = layers
        snapshot["sources"] = list(dict.fromkeys(aliases.get(k, k) for k in snapshot.get("sources", [])))
        if snapshot.get("selectedLayer") not in {l["id"] for l in layers}:
            snapshot["selectedLayer"] = layers[0]["id"] if layers else None
    return result
