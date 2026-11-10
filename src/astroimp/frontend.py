"""Build the NightBloom frontend when its sources are newer than the bundle."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path


FRONTEND = Path(__file__).resolve().parents[2] / "frontend"


def needs_build(frontend: Path) -> bool:
    index = frontend / "dist" / "index.html"
    if not index.is_file():
        return True
    built = index.stat().st_mtime_ns
    sources = [frontend / "index.html", frontend / "package.json"]
    sources.extend(frontend.glob("vite.config.*"))
    sources.extend((frontend / "src").rglob("*"))
    return any(source.is_file() and source.stat().st_mtime_ns > built for source in sources)


def build_frontend(frontend: Path = FRONTEND) -> None:
    if not needs_build(frontend):
        return
    npm = shutil.which("npm.cmd" if os.name == "nt" else "npm")
    if npm is None:
        raise RuntimeError(
            "Node.js/npm is required to build NightBloom. Install Node.js and ensure npm is on PATH, "
            "or use --no-build with an existing frontend build.")
    options = {
        "cwd": frontend,
        "check": True,
        "shell": os.name == "nt" and Path(npm).suffix.lower() in (".cmd", ".bat"),
        "creationflags": subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        "stdout": sys.stdout,
        "stderr": sys.stderr,
    }
    try:
        if not (frontend / "node_modules").is_dir():
            subprocess.run([npm, "ci"], **options)
        subprocess.run([npm, "run", "build"], **options)
    except (OSError, subprocess.CalledProcessError) as exc:
        raise RuntimeError(
            f"NightBloom frontend build failed. Run npm run build in {frontend} to diagnose it."
        ) from exc
