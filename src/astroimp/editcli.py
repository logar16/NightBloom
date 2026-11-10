"""`astroimp edit`: inspect, analyse and adjust the open composition through NightBloom."""
from __future__ import annotations

import json

import requests


def _call(url: str, method: str, path: str, *, session: str | None = None, **kwargs):
    if session:
        kwargs["headers"] = {**kwargs.get("headers", {}), "X-AstroImp-Session": session}
    try:
        response = requests.request(method, url.rstrip("/") + path, timeout=300, **kwargs)
    except requests.ConnectionError as exc:
        raise RuntimeError(f"NightBloom is not running at {url}. Start it with 'uv run astroimp'.") from exc
    if not response.ok:
        try:
            detail = response.json().get("detail", response.text)
        except ValueError:
            detail = response.text
        raise RuntimeError(f"{response.status_code}: {detail}")
    return response.json()


def show(url: str, *, session: str | None = None) -> None:
    project = _call(url, "GET", "/api/session", session=session)
    state, meta = project.get("edits"), project.get("meta")
    if not state or not meta:
        raise RuntimeError("No saved edits yet. Open the build in the editor once.")
    filters = {layer["key"]: layer["filter"] for layer in meta["layers"]}
    print(f"{project.get('name')}  ·  {meta['width']}×{meta['height']}  ·  top layer first")
    print(" #  vis  name            source      mode      blend     color    stretch   k      black  white  int   opac")
    for index, layer in enumerate(state["layers"], 1):
        print(f"{index:2}  {'on ' if layer['visible'] else 'off'}  {layer['name'][:14]:14}  "
              f"{filters.get(layer['source'], 'luminance')[:10]:10}  {layer['mode']:8}  {layer['blend']:8}  "
              f"{layer['color'] if layer['mode'] == 'tint' else '-':7}  {layer['stretch']:8}  {layer['k']:<5.3g}  "
              f"{layer['black']:<5.3g}  {layer['white']:<5.3g}  {layer['intensity']:<4.3g}  {layer['opacity']:<4.3g}")
    g = state["global"]
    print(f"global: output={g.get('output')}  rotation={g['rotation']}  crop={g['crop']}")


def analyze(url: str, max_side: int, *, session: str | None = None) -> None:
    report = _call(url, "GET", "/api/analyze", params={"max_side": max_side}, session=session)
    print(json.dumps(report, indent=2))


def set_values(url: str, target: str, ops: list[str], note: str, *, session: str | None = None) -> None:
    result = _call(url, "POST", "/api/edits/apply",
                   json={"steps": [{"target": target, "ops": ops}], "note": note}, session=session)
    print("\n".join(result["changes"]))
    print(f"revision {result['revision']} (the open editor picks this up as an undoable step)")


def markers(url: str, *, session: str | None = None) -> None:
    items = _call(url, "GET", "/api/markers", session=session)
    if not items:
        print("No markers. Use the editor's marker tool (M) to place some.")
    for m in items:
        where = (f"rect {m['x1']},{m['y1']} - {m['x2']},{m['y2']}" if m.get("kind") == "rect"
                 else f"point {m['x']},{m['y']} r={m.get('radius', 4)}")
        print(f"#{m['n']}  {where}  {m.get('note', '')}")


def probe(url: str, region: dict, *, session: str | None = None) -> None:
    print(json.dumps(_call(url, "POST", "/api/probe", json=region, session=session), indent=2))
