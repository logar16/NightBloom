from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
from pathlib import Path

import numpy as np
from astropy.io import fits

from astroimp.mast import text, write_json


def filter_label(header: fits.Header) -> str:
    labels = []
    for key in ("FILTER", "FILTER1", "FILTER2", "PUPIL"):
        value = text(header.get(key))
        if value and not value.upper().startswith(("CLEAR", "OPEN", "N/A")) and value not in labels:
            labels.append(value)
    return "+".join(labels) or "unknown-filter"


def input_files(paths: list[Path]) -> list[Path]:
    files: list[Path] = []
    for path in paths:
        if path.is_dir():
            # Do not accidentally treat the old raw exposures as ready-to-color mosaics.
            matches = sorted(p for p in path.rglob("*") if p.is_file()
                             and p.name.lower().endswith(("_i2d.fits", "_drc.fits", "_drz.fits")))
            if not matches:
                raise ValueError(f"No processed i2d/drc/drz FITS found in {path}. "
                                 "For another image format, supply explicit FITS paths.")
            files.extend(matches)
        elif path.is_file():
            files.append(path)
        else:
            raise FileNotFoundError(path)
    return list(dict.fromkeys(path.resolve() for path in files))


def prepare(paths: list[Path], output: Path) -> Path:
    sources = input_files(paths)
    if not sources:
        raise ValueError("No input FITS files selected.")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    layers = []
    destinations: set[Path] = set()
    for source in sources:
        with fits.open(source, memmap=False) as hdus:
            science = [(i, hdu) for i, hdu in enumerate(hdus) if hdu.name == "SCI"]
            if not science:
                science = [(0, hdus[0])]
            for index, hdu in science:
                if hdu.header.get("NAXIS") != 2 or hdu.data is None:
                    raise ValueError(f"{source.name}, HDU {index}: expected a 2D science image; "
                                     "cubes and tables are not supported.")
                header = hdus[0].header.copy()
                header.update(hdu.header)
                label = filter_label(header)
                prefix = re.sub(r"[^A-Za-z0-9_+-]", "_", label)
                destination = output / f"{prefix}_{source.stem}_hdu{index}.fits"
                if destination in sources or destination in destinations:
                    raise ValueError(f"Output filename collision: {destination}")
                destinations.add(destination)
                header.add_history(f"NightBloom science extraction from {source.name}, HDU {index}")
                pixels = np.asarray(hdu.data, dtype=np.float32)
                temporary = destination.with_name(destination.name + ".part")
                try:
                    fits.PrimaryHDU(pixels, header=header).writeto(temporary, overwrite=True, checksum=True)
                    temporary.replace(destination)
                finally:
                    temporary.unlink(missing_ok=True)
                layers.append({
                    "file": destination.name,
                    "source": str(source),
                    "source_size_bytes": source.stat().st_size,
                    "hdu": index,
                    "filter": label,
                    "instrument": text(header.get("INSTRUME")),
                    "telescope": text(header.get("TELESCOP")),
                    "shape": list(pixels.shape),
                })
                print(f"Prepared {destination.name} ({pixels.shape[1]} x {pixels.shape[0]})", flush=True)
    manifest = output / "layers.json"
    write_json(manifest, {"version": 1, "layers": layers})
    print(f"Siril-ready folder: {output}", flush=True)
    print("Use Siril RGB composition to select these layers, align, and assign colors.", flush=True)
    return manifest


def find_siril(executable: Path | None = None) -> Path:
    if executable is not None:
        if not executable.is_file():
            raise FileNotFoundError(executable)
        return executable.resolve()
    on_path = shutil.which("siril")
    if on_path:
        return Path(on_path)
    if os.name == "nt":
        import winreg

        for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
            for root in (r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
                         r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"):
                try:
                    key = winreg.OpenKey(hive, root)
                except FileNotFoundError:
                    continue
                with key:
                    for index in range(winreg.QueryInfoKey(key)[0]):
                        with winreg.OpenKey(key, winreg.EnumKey(key, index)) as entry:
                            try:
                                name = winreg.QueryValueEx(entry, "DisplayName")[0]
                                location = winreg.QueryValueEx(entry, "InstallLocation")[0]
                            except FileNotFoundError:
                                continue
                            candidate = Path(location) / "bin" / "siril.exe"
                            if str(name).lower().startswith("siril") and candidate.is_file():
                                return candidate
    raise FileNotFoundError("Siril executable not found. Supply --siril-exe with its full path.")


def open_siril(manifest: Path, executable: Path | None = None) -> int:
    if os.name != "nt":
        raise ValueError("Automatic Siril launch currently supports Windows only. "
                         "Open the prepared FITS files in Siril manually.")
    content = json.loads(manifest.read_text(encoding="utf-8"))
    if content.get("version") != 1 or not content.get("layers"):
        raise ValueError(f"Not a prepared layers manifest: {manifest}")
    directory = manifest.resolve().parent
    image = (directory / content["layers"][0]["file"]).resolve()
    if image.parent != directory or not image.is_file():
        raise ValueError(f"Missing or invalid prepared image: {image}")
    siril = find_siril(executable)
    # siril.exe is a console-subsystem binary; DETACHED_PROCESS stops Windows from opening
    # a terminal for it, and job breakaway keeps it alive after the calling terminal closes.
    flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
    launch = dict(cwd=directory, close_fds=True, stdin=subprocess.DEVNULL,
                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    command = [str(siril), "-d", str(directory), str(image)]
    try:
        pid = subprocess.Popen(command, creationflags=flags | subprocess.CREATE_BREAKAWAY_FROM_JOB,
                               **launch).pid
    except PermissionError:
        pid = subprocess.Popen(command, creationflags=flags, **launch).pid
    print(f"Opened Siril (PID {pid}) with the first layer. "
          "Add the other prepared layers in RGB composition.", flush=True)
    return pid
