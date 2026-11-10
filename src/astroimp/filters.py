"""Plain-language meaning of common HST/JWST filters (what a layer 'shows')."""
from __future__ import annotations

import re

# Narrowband filters isolate a specific emission line, so they map to a physical species.
LINES: dict[str, tuple[str, str]] = {
    "F373N": ("[O II]", "Singly ionized oxygen"),
    "F395N": ("Ca II", "Ionized calcium"),
    "F469N": ("He II", "Ionized helium"),
    "F487N": ("H-beta", "Hydrogen (H-beta)"),
    "F501N": ("[O III]", "Doubly ionized oxygen"),
    "F502N": ("[O III]", "Doubly ionized oxygen"),
    "F631N": ("[O I]", "Neutral oxygen"),
    "F645N": ("Continuum", "Red continuum (no lines)"),
    "F656N": ("H-alpha", "Hydrogen (H-alpha)"),
    "F657N": ("H-alpha + [N II]", "Hydrogen + nitrogen"),
    "F658N": ("[N II]", "Ionized nitrogen"),
    "F660N": ("[N II]", "Ionized nitrogen"),
    "F665N": ("H-alpha (redshifted)", "Hydrogen, receding gas"),
    "F673N": ("[S II]", "Ionized sulfur"),
    "F680N": ("[S II] + continuum", "Ionized sulfur"),
    "F953N": ("[S III]", "Doubly ionized sulfur"),
    "F126N": ("[Fe II]", "Ionized iron (shocks)"),
    "F128N": ("Paschen-beta", "Hydrogen (Paschen-beta)"),
    "F130N": ("Continuum", "Near-IR continuum"),
    "F164N": ("[Fe II]", "Ionized iron (shocks)"),
    "F167N": ("Continuum", "Near-IR continuum"),
    "F187N": ("Paschen-alpha", "Hydrogen (Paschen-alpha)"),
    "F190N": ("Continuum", "Near-IR continuum"),
    "F212N": ("H2", "Molecular hydrogen (warm gas)"),
    "F215N": ("Continuum", "Near-IR continuum"),
    "F323N": ("H2", "Molecular hydrogen"),
    "F335M": ("PAH 3.3 um", "Hydrocarbon dust (PAHs)"),
    "F405N": ("Brackett-alpha", "Hydrogen (Brackett-alpha)"),
    "F466N": ("CO", "Carbon monoxide"),
    "F470N": ("H2", "Molecular hydrogen"),
    "F770W": ("PAH 7.7 um", "Hydrocarbon dust (PAHs)"),
    "F1130W": ("PAH 11.3 um", "Hydrocarbon dust (PAHs)"),
}


BANDS = ((320, "Ultraviolet"), (450, "Visible blue"), (500, "Visible blue-green"),
         (570, "Visible green"), (620, "Visible yellow-orange"), (700, "Visible red"),
         (1000, "Far red / near-IR"), (2500, "Near-infrared"), (5500, "Infrared"))


def band(nm: float | None) -> str:
    if nm is None:
        return "Unknown band"
    return next((name for limit, name in BANDS if nm < limit), "Mid-infrared")


def describe(name: str, wavelength_nm: float | None = None) -> dict:
    key = name.split("+")[0].upper()
    line = LINES.get(key)
    suffix = re.sub(r"^F\d+", "", key)
    kind = {"N": "Narrowband", "M": "Medium band", "W": "Wideband", "W2": "Wideband",
            "LP": "Longpass"}.get(suffix, "Filter")
    if line:
        return {"line": line[0], "shows": line[1], "kind": kind, "band": band(wavelength_nm)}
    return {"line": "", "shows": f"{band(wavelength_nm)} light", "kind": kind, "band": band(wavelength_nm)}
