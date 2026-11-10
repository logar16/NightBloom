"""Exercise the compiled Rust server against isolated projects and Python exports."""
from __future__ import annotations

import argparse
import json
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import numpy as np
import requests
from PIL import Image

from prepare_rust_fixture import prepare


def verify(executable: Path, frontend: Path, fixture: Path) -> dict:
    info = json.loads((fixture / "fixture.json").read_text())
    data = Path(info["data"])
    session = info["session"]
    original = json.loads((data / "sessions" / session / "edits.json").read_text())
    sibling = "second-project"
    shutil.copytree(data / "sessions" / session, data / "sessions" / sibling)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    origin = f"http://127.0.0.1:{port}"
    log_path = fixture / "server.log"
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen([str(executable), "--data", str(data), "--port", str(port),
                                    "--frontend", str(frontend)], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 30
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"Rust server exited with {process.returncode}:\n{log_path.read_text()}")
                try:
                    ready = requests.get(f"{origin}/api/status", timeout=1)
                except requests.ConnectionError:
                    ready = None
                if ready is not None and ready.ok:
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"Rust server did not become ready:\n{log_path.read_text()}")
                time.sleep(.1)

            def api(path: str, method: str = "GET", body=None, project: str | None = session):
                response = requests.request(method, f"{origin}/api{path}", json=body,
                    headers={"X-AstroImp-Session": project} if project else {}, timeout=30)
                response.raise_for_status()
                return response.json()

            summaries = api("/sessions", project=None)
            assert {session, sibling}.issubset({item["id"] for item in summaries})
            api(f"/sessions/{session}/open", "POST", project=None)
            payload = api("/session")
            assert "stars" not in payload["meta"]
            assert "stars" not in payload["edits"]["global"]
            assert requests.get(f"{origin}/", timeout=10).ok
            for source in payload["meta"]["layers"]:
                plane = requests.get(f"{origin}/api/project/{source['file']}",
                                     params={"session": session}, timeout=10)
                assert plane.ok and len(plane.content) == info["width"] * info["height"] * 2
            rejected = requests.get(f"{origin}/api/project/%2E%2E%2Fedits.json",
                                    params={"session": session}, timeout=10)
            assert not rejected.ok, "Project API allowed a path outside the plane allowlist"
            api(f"/sessions/{sibling}/open", "POST", project=None)
            edits = json.loads(json.dumps(original))
            edits["global"]["output"]["exposure"] = .25
            revision = api("/edits", "PUT", edits)
            assert revision["revision"] > 1
            assert api("/edits")["global"]["output"]["exposure"] == .25
            assert api("/edits", project=sibling)["global"]["output"]["exposure"] == 0
            api("/edits", "PUT", original)
            plan = api("/export/full/plan")
            assert (plan["width"], plan["height"]) == (info["native_width"], info["native_height"])
            outputs = []
            render_cases = json.loads((fixture / "reference" / "render.json").read_text())
            for name, long_side in [("64", 64), ("128", 128),
                                    ("tone", 64), ("colour", 64), ("transformed", 64), ("textured", 64)]:
                api("/edits", "PUT", original if name in {"64", "128"} else render_cases[name]["state"])
                job = api("/export/full", "POST", {"format": "png8", "long_side": long_side, "quality": 92})
                deadline = time.monotonic() + 30
                while job["state"] in {"running", "queued"}:
                    assert time.monotonic() < deadline, f"Export stalled: {job}"
                    time.sleep(.05)
                    job = api("/job")
                assert job["state"] == "done", job
                result = job["result"]
                response = requests.get(f"{origin}{result['download_url']}", timeout=30)
                response.raise_for_status()
                saved = fixture / f"rust-{name}.png"
                saved.write_bytes(response.content)
                actual = np.asarray(Image.open(saved).convert("RGB"), np.int16)
                expected = np.asarray(Image.open(fixture / "reference" / f"python-{name}.png")
                                      .convert("RGB"), np.int16)
                assert actual.shape == expected.shape
                error = np.abs(actual - expected)
                outputs.append({"case": name, "long_side": long_side, "width": actual.shape[1], "height": actual.shape[0],
                                "max_channel_error": int(error.max()), "mean_channel_error": float(error.mean())})
                assert int(error.max()) <= 1, outputs[-1]
            return {"origin": origin, "scoped_autosave": True, "star_feature_removed": True,
                    "exports": outputs}
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--frontend", type=Path, default=Path("frontend") / "dist")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="astroimp-rust-check-") as temporary:
        fixture = Path(temporary) / "fixture"
        prepare(fixture)
        result = verify(args.server.resolve(), args.frontend.resolve(), fixture)
        print(json.dumps(result, indent=2))
