from __future__ import annotations

import argparse
import json
import logging
from dataclasses import asdict
from pathlib import Path

from astroimp.datasets import (
    Framing, display_groups, get_dataset, group_datasets, load_selection, spectral_filter,
    size_status,
)
from astroimp.mast import (
    display_products, download, read_products, search, sky_position, write_json,
)
from astroimp.prepare import open_siril, prepare


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description="NightBloom: Aladin, archive images, and color composition.")
    root.add_argument("--no-build", action="store_true", help="Skip the NightBloom frontend build")
    commands = root.add_subparsers(dest="command")
    studio = commands.add_parser("studio", help="Launch the local NightBloom app")
    studio.add_argument("--port", type=int, default=8765)
    studio.add_argument("--no-browser", action="store_true")
    studio.add_argument("--no-build", action="store_true", default=argparse.SUPPRESS,
                        help="Skip the NightBloom frontend build")
    studio.add_argument("--data", type=Path,
                        help="NightBloom data directory (sessions, downloads and caches)")
    studio.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    commands.add_parser("capture", help="Capture Aladin's view and search the NightBloom catalog")
    edit = commands.add_parser("edit", help="Inspect, analyse and adjust the open composition (NightBloom must be running)")
    edit.add_argument("--url", default="http://127.0.0.1:8765")
    edit.add_argument("--session", help="Project ID (defaults to the last opened project)")
    edit_commands = edit.add_subparsers(dest="edit_command", required=True)
    edit_commands.add_parser("show", help="List layers and their settings (top layer first)")
    analysis = edit_commands.add_parser("analyze", help="Render on the CPU and report clipping and per-layer contribution")
    analysis.add_argument("--max-side", type=int, default=900)
    setter = edit_commands.add_parser(
        "set", help="Change settings: TARGET is a layer name/filter/number, comma list, '*' or 'global'",
        epilog="Examples: set F187N intensity*=0.6 white=1.4 | set '*' white*=1.2 | "
               "set global output.highlights=0.5 | set 3 color=#ff8040 blend=screen")
    setter.add_argument("target")
    setter.add_argument("ops", nargs="+", metavar="KEY=VALUE|KEY*=F|KEY+=D")
    setter.add_argument("--note", default="")
    edit_commands.add_parser("markers", help="List markers placed in the editor")
    prober = edit_commands.add_parser("probe", help="Explain a region's colour layer by layer (full-res pixels)")
    prober.add_argument("point", nargs="*", type=float, metavar="X Y", help="Pixel position as shown in the editor")
    prober.add_argument("--radius", type=float, default=4)
    prober.add_argument("--rect", nargs=4, type=float, metavar=("X1", "Y1", "X2", "Y2"))
    prober.add_argument("--marker", help="Probe a marker number placed in the editor")
    build = commands.add_parser("build", help="Build GPU layers for a NightBloom session")
    build.add_argument("session_dir", type=Path)
    full = commands.add_parser("export-full", help="Render saved edits at native source resolution")
    full.add_argument("session_dir", type=Path)
    full.add_argument("--scale-factor", type=float)
    full.add_argument("--long-side", type=int)
    full.add_argument("--format", choices=["png8", "png16", "tiff16", "jpeg", "png"], default="png8")
    full.add_argument("--quality", type=int, default=92)
    query = commands.add_parser("search", help="Find public, combined Hubble/JWST filter images")
    query.add_argument("target", nargs="?", help="Object name, e.g. M42")
    query.add_argument("--ra", type=float, help="ICRS right ascension in decimal degrees")
    query.add_argument("--dec", type=float, help="ICRS declination in decimal degrees")
    query.add_argument("--out", type=Path, default=Path("data/products.json"))
    listing = commands.add_parser("list", help="Show the numbered products in a saved manifest")
    listing.add_argument("manifest", type=Path)
    groups = commands.add_parser("groups", help="Show candidate filter sets, costs and framing")
    groups.add_argument("manifest", type=Path)
    groups.add_argument("--group", help="Show a set's suggested IDs and all alternatives, e.g. G01")
    fetch = commands.add_parser("fetch", help="Download selected products and prepare their science layers")
    fetch.add_argument("manifest", type=Path)
    selection = fetch.add_mutually_exclusive_group(required=True)
    selection.add_argument("--select", type=int, nargs="+", metavar="ID")
    selection.add_argument("--group", help="Download a set's one-per-filter suggestions, e.g. G01")
    fetch.add_argument("--filters", nargs="+", help="With --group, select only these filter labels")
    fetch.add_argument("--out", type=Path, required=True, help="Session folder for originals and Siril layers")
    local = commands.add_parser("prepare", help="Prepare already-downloaded FITS without network access")
    local.add_argument("inputs", type=Path, nargs="+", help="FITS paths, or folders of i2d/drc/drz products")
    local.add_argument("--out", type=Path, required=True, help="Output folder for Siril-ready science images")
    launch = commands.add_parser("siril", help="Open the first prepared layer in Siril (Windows)")
    launch.add_argument("manifest", type=Path, help="Prepared layers.json")
    commands.add_parser("hub", help="Run a local SAMP hub if Aladin has not started one")
    view = commands.add_parser("view", help="Read Aladin's current center, angular size and corners over SAMP")
    view.add_argument("--out", type=Path, help="Also save the view metadata as JSON")
    bridge = commands.add_parser("listen", help="Search MAST when Aladin sends a sky position over SAMP")
    bridge.add_argument("--out", type=Path, default=Path("data/selections"))
    bridge.add_argument("--once", action="store_true", help="Exit after the first selected position is searched")
    bridge.add_argument("--timeout", type=float, help="Exit with an error after this many seconds without a position")
    for command in (query, bridge):
        command.add_argument("--radius", type=float, default=2, help="Search radius in arcminutes (default: 2)")
        command.add_argument("--max-observations", type=int, default=50,
                             help="Limit archive product-list queries (default: 50; reports truncation)")
    for command in (query, bridge, groups, fetch):
        command.add_argument("--field-arcmin", type=float, nargs=2, metavar=("WIDTH", "HEIGHT"),
                             help="Desired angular width and height; independent of search radius")
        command.add_argument("--max-area-ratio", type=float,
                             help="Optional LARGE warning above this sky-area ratio; never hides files")
    for command in (query, bridge, groups):
        command.add_argument("--show-all", action="store_true",
                             help="Include oversized sets even when enough good smaller fields exist")
    for command in (fetch, local):
        command.add_argument("--open-siril", action="store_true", help="Open the first prepared layer in Siril")
    for command in (fetch, local, launch):
        command.add_argument("--siril-exe", type=Path, help="Override Siril executable detection")
    return root


def run(args: argparse.Namespace) -> None:
    if args.command in (None, "studio") and not getattr(args, "child", False):
        supervise(args)
    elif args.command in (None, "studio"):
        from astroimp.server import serve

        serve(getattr(args, "port", 8765), getattr(args, "no_browser", False),
              getattr(args, "data", None) or Path("data"), no_build=args.no_build, supervised=True)
    elif args.command == "edit":
        from astroimp import editcli

        if args.edit_command == "show":
            editcli.show(args.url, session=args.session)
        elif args.edit_command == "analyze":
            editcli.analyze(args.url, args.max_side, session=args.session)
        elif args.edit_command == "markers":
            editcli.markers(args.url, session=args.session)
        elif args.edit_command == "probe":
            if args.marker:
                region = {"marker": args.marker}
            elif args.rect:
                region = dict(zip(("x1", "y1", "x2", "y2"), args.rect))
            elif len(args.point) == 2:
                region = {"x": args.point[0], "y": args.point[1], "radius": args.radius}
            else:
                raise ValueError("Give X Y, --rect X1 Y1 X2 Y2, or --marker N.")
            editcli.probe(args.url, region, session=args.session)
        else:
            editcli.set_values(args.url, args.target, args.ops, args.note, session=args.session)
    elif args.command == "capture":
        from astroimp.session import capture

        print(capture(progress=lambda stage, fraction, message: print(
            f"{stage} {fraction:.0%}: {message}", flush=True)))
    elif args.command == "build":
        from astroimp.build import build

        build(args.session_dir, progress=lambda stage, fraction, message: print(
            f"{stage} {fraction:.0%}: {message}", flush=True))
    elif args.command == "export-full":
        from astroimp.fullres import export_full

        result = export_full(args.session_dir, scale_factor=args.scale_factor, format=args.format,
                             long_side=args.long_side, quality=args.quality,
                             progress=lambda stage, fraction, message: print(
                                 f"{stage} {fraction:.0%}: {message}", flush=True))
        print(json.dumps(result, indent=2))
    elif args.command == "search":
        if args.target and (args.ra is not None or args.dec is not None):
            raise ValueError("Supply either a target name or --ra and --dec, not both.")
        if args.target:
            from astropy.coordinates import SkyCoord

            coord = SkyCoord.from_name(args.target)
        elif args.ra is not None and args.dec is not None:
            coord = sky_position(args.ra, args.dec)
        else:
            raise ValueError("Supply a target name or both --ra and --dec.")
        field = tuple(args.field_arcmin) if args.field_arcmin is not None else None
        Framing(float(coord.ra.deg), float(coord.dec.deg), field, args.max_area_ratio)
        products = search(coord, args.out, target=args.target or "", radius_arcmin=args.radius,
                          max_observations=args.max_observations, field_arcmin=field,
                          max_area_ratio=args.max_area_ratio)
        _, framing = load_selection(args.out)
        display_groups(products, framing, show_all=args.show_all)
        print(f"Manifest: {args.out.resolve()}")
    elif args.command == "list":
        display_products(read_products(args.manifest))
    elif args.command == "groups":
        products, framing = load_selection(args.manifest, args.field_arcmin, args.max_area_ratio)
        display_groups(products, framing, args.group, show_all=args.show_all)
    elif args.command == "fetch":
        if args.group is not None:
            products, framing = load_selection(args.manifest, args.field_arcmin, args.max_area_ratio)
            group = get_dataset(group_datasets(products), args.group)
            suggestions = group.suggested(framing)
            if args.filters:
                wanted = {label.upper() for label in args.filters}
                available = {label for _, product in suggestions
                             if (label := spectral_filter(product)) is not None}
                if not wanted <= available:
                    raise ValueError(f"Unknown filters: {', '.join(sorted(wanted - available))}. "
                                     f"Available: {', '.join(sorted(available))}. Nothing downloaded.")
                suggestions = [entry for entry in suggestions if spectral_filter(entry[1]) in wanted]
            selected = [product for _, product in suggestions]
            if size_status(suggestions, framing)[0]:
                print("OVERSIZED: a selected image exceeds 10x the view area. "
                      "Proceeding because --group was explicitly selected.", flush=True)
            print(f"{group.identifier} suggested file IDs: "
                  + " ".join(str(index) for index, _ in suggestions), flush=True)
        else:
            if args.filters or args.field_arcmin or args.max_area_ratio is not None:
                raise ValueError("Framing and --filters options apply to --group, not --select.")
            products = read_products(args.manifest)
            indices = list(dict.fromkeys(args.select))
            if any(index < 1 or index > len(products) for index in indices):
                raise ValueError(f"Product IDs must be in [1, {len(products)}]. Nothing downloaded.")
            selected = [products[index - 1] for index in indices]
        if len({p.filename for p in selected}) != len(selected):
            raise ValueError("Selected products have colliding filenames. Use separate session folders.")
        total = sum(p.size_bytes for p in selected)
        print(f"Selected {len(selected)} files, {total / 1024**2:.1f} MiB "
              "(plus extracted science images; unknown sizes excluded).", flush=True)
        write_json(args.out / "selection.json", {
            "version": 1, "source_manifest": str(args.manifest.resolve()),
            "group": args.group,
            "products": [asdict(product) for product in selected],
        })
        paths = [download(product, args.out / "originals") for product in selected]
        layers = prepare(paths, args.out / "siril")
        if args.open_siril:
            open_siril(layers, args.siril_exe)
    elif args.command == "prepare":
        layers = prepare(args.inputs, args.out)
        if args.open_siril:
            open_siril(layers, args.siril_exe)
    elif args.command == "siril":
        open_siril(args.manifest, args.siril_exe)
    elif args.command == "hub":
        from astroimp.samp import run_hub

        run_hub()
    elif args.command == "view":
        from astroimp.aladin import read_view

        metadata = asdict(read_view())
        if args.out is not None:
            write_json(args.out, metadata)
        print(json.dumps(metadata, indent=2, allow_nan=False))
    elif args.command == "listen":
        from astroimp.samp import listen

        listen(args.out, radius_arcmin=args.radius, max_observations=args.max_observations,
               once=args.once, timeout=args.timeout,
               field_arcmin=tuple(args.field_arcmin) if args.field_arcmin is not None else None,
               max_area_ratio=args.max_area_ratio, show_all=args.show_all)


def supervise(args: argparse.Namespace) -> None:
    """Run NightBloom in a child process in this console and relaunch it on request."""
    import subprocess
    import sys

    from astroimp.server import RESTART_EXIT_CODE

    command = [sys.executable, "-c", "from astroimp.cli import main; main()", "studio", "--child",
               "--port", str(getattr(args, "port", 8765))]
    if getattr(args, "data", None):
        command += ["--data", str(args.data)]
    if args.no_build:
        command.append("--no-build")
    no_browser = getattr(args, "no_browser", False)
    while True:
        child = subprocess.Popen(command + (["--no-browser"] if no_browser else []))
        try:
            code = child.wait()
        except KeyboardInterrupt:
            # Ctrl+C reaches the child through the shared console; let it shut down cleanly.
            try:
                child.wait(timeout=10)
            except (KeyboardInterrupt, subprocess.TimeoutExpired):
                child.kill()
            raise
        if code != RESTART_EXIT_CODE:
            raise SystemExit(code)
        logging.info("Restarting NightBloom…")
        no_browser = True  # The open tabs reconnect on their own.


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(levelname)s: %(message)s")
    args = parser().parse_args()
    try:
        run(args)
    except KeyboardInterrupt:
        raise SystemExit(130) from None
    except Exception:
        # Keep the full cause visible, including archive, FITS and SAMP failures.
        logging.exception("NightBloom failed")
        raise SystemExit(1) from None
