# NightBloom — agent guide

NightBloom is a local tool for creative false-colour astronomy art. A FastAPI
backend (`src/astroimp`) finds and downloads public HST/JWST/survey FITS, aligns them
onto one grid, and exports full-resolution renders. A Vite/React/TypeScript WebGL2
editor (`frontend/`) composites the aligned layers live. `README.md` is the user-facing
feature reference; this file is the map for changing the code.

Workflow and screens: **Home → New project (search / Aladin capture / bookmarks) → Sky
(pick images) → Build (align onto a grid) → Edit (composite) → Export.**

## Ground rules

- **Never modify `data\`.** It holds the owner's real projects, downloads and frozen
  test fixtures. Reading is fine. Tests and e2e runs use their own temp copies.
- **Never start, stop or restart the owner's NightBloom on port 8765.** Backend changes
  reach it when the owner restarts from the in-app update banner. Use another port
  (`--data <temp dir> --port 8876 --no-browser`) for your own server.
- **Never commit `.github/`** (local, ignored). Commit only files you changed; check
  `git status` first.
- Python: always `uv run --no-sync …` (NightBloom holds the venv lock). Never install
  packages globally; the package index is set in `pyproject.toml`.
- Never launch windowed GUI apps (Aladin, Siril, browsers) from the shell.
- UI: clean, icon-first (Lucide icons, tooltips, no instructional text). Get the
  owner's approval for visible UI/UX changes before committing.
- Commit trailers: `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`.

### Delegated agents

If you were launched by another agent to do a task:
- You own the whole brief. Do not message, wake or coordinate with other agents or
  sessions; report only back to the agent that launched you.
- Others may be editing the same tree: check `git status`/`git diff` before committing,
  and stage only your own files.
- If blocked, finish what you can and report the blocker with evidence.

## Commands

```powershell
# Backend tests (~40 s)
uv run --no-sync --with pytest python -m pytest -q
uv run --no-sync --with pytest python -m pytest -q tests/test_fullres.py -k estimate
# Frontend (from frontend\)
npx tsc --noEmit -p .        # typecheck
npx vitest run               # unit tests (~2 s)
npm run build                # bundle served by FastAPI from frontend\dist
$env:UV_NO_SYNC='1'; npx playwright test   # e2e on its own server + fixture copy (minutes)
npm run gen:api              # regenerate src/api/schema.d.ts from a running server
# CLI
uv run --no-sync astroimp --help
uv run --no-sync astroimp edit --session <id> show     # inspect a running NightBloom composition
```

## Architecture map

### Backend (`src/astroimp`)

| Stage | Module | What lives there |
| --- | --- | --- |
| HTTP | `server.py` | All `/api/*` routes; thin, delegates to modules below |
| Jobs/projects | `session.py` | `capture`/`capture_coordinates`/`recapture`, `Sessions` (project folders, payloads), `JobManager` (per-project jobs + FIFO "heavy" queue for CPU-bound stages) |
| New project | `coordinates.py`, `bookmarks.py` | Name/coordinate resolution, archive reconnaissance; global bookmarks (`data\bookmarks.json`) |
| Aladin | `samp.py`, `aladin.py`, `aladin_fits.py` | SAMP hub/client, snapshot capture + WCS, view rotation |
| Catalog | `catalog.py`, `mast.py`, `previews.py`, `filters.py` | MAST queries for the view, preview cache, plain-English filter info |
| Surveys | `surveys.py` | Survey band registry (FWHM, scale), CDS hips2fits cutouts, detector-defect repair |
| Archive health | `services.py` | Background CDS/MAST probes; `friendly(exc)` error text |
| Build | `build.py` | Download, `prepare_science` (seam repair, low-weight fringe cut), reprojection, normalization, luminance, edge maps, caching (`downstream_signature`) |
| Mosaics | `mosaic.py` | Same-filter tile grouping, overlap gain/offset calibration, feathered combine |
| Grid | `grid.py`, `canvas.py`, `fill.py`, `builds.py` | Locked canvas grid + sky remapping of edits/markers; region-first canvas planning; "Fill around" survey surroundings; build history |
| Rendering (CPU) | `render.py`, `layer_tools.py`, `view.py` | CPU mirror of the WebGL compositor; layer tone tools; crop/rotate geometry |
| Export | `fullres.py`, `contactsheet.py` | Tiled native-resolution export (plan + time estimate, reprojection, render, write); contact sheets |
| Misc | `storage.py`, `thumbnails.py`, `planes.py`, `probe_cache.py`, `frontend.py`, `cli.py`, `editcli.py`, `edits.py` | Storage cleanup, Home thumbnails, plane IO, probe caches, auto-building the frontend, CLI |
| Legacy CLI | `datasets.py`, `prepare.py` | Original MAST → Siril workflow (still supported) |

### Frontend (`frontend/src`)

| Area | Path | Notes |
| --- | --- | --- |
| Shell | `App.tsx`, `JobsTray.tsx`, `theme/` | Header (pipeline nav, health pill, jobs ring), settings, storage, update banner |
| API | `api/` | `client.ts` (session-scoped fetch), `hooks.ts`, generated `schema.d.ts` |
| Editor state | `editor/state.ts`, `editor/store.ts` | Edit-state types, `reconcileState` (migrations: anchor legacy crops, sources), undo store |
| GPU compositor | `editor/compositor.ts` | WebGL2 shaders for every layer/global operation |
| Geometry | `editor/view.ts` | Crop/rotate/flip mapping; mirrors `view.py` |
| Screens | `screens/{home,new,sky,build,canvas,edit}/` | One folder per screen; `edit/properties.tsx` is the layer properties panel, `edit/canvas.tsx` the canvas + tools, `edit/fullres.tsx` the export dialog |
| Controls | `ui/` | Slider, Select, ColorPicker, ContextMenu, IconButton |

## Where to change things

| Change | Touch |
| --- | --- |
| New/changed layer or global look (tone, blend, colour, detail, edges…) | `editor/compositor.ts` **and** `render.py`/`layer_tools.py` (CPU mirror used by exports, probes, contact sheets), plus defaults/migration in `editor/state.ts`. Parity is tested (`tests/test_blend_contract.py`, e2e `blends`/`edges`/`layer-tools` specs). |
| Crop/rotate/flip geometry | `editor/view.ts` **and** `view.py` (+ `fullres.view_points`) |
| Properties panel UI | `screens/edit/properties.tsx` (one `<Section>` per group) |
| What gets built / cached | `build.py` (`_build`, `prepare_science`); bump the relevant version constant |
| Same-filter tile merging | `mosaic.py` (+ `fullres.reproject_mosaic` for exports) |
| Survey bands | `surveys.py` band registry |
| Export behaviour or time estimate | `fullres.py` (`plan`, `estimate_parts`, `estimate_calibration`, `export_full`) and `screens/edit/fullres.tsx` |
| New API route | `server.py`, then `npm run gen:api` (`api/schema.d.ts`); shared hand-written types live in `api/types.ts` |
| Background job | `JobManager.start` in `session.py`; call `progress(stage, fraction, message)` and `check_cancelled` |

## Data layout and invariants

A project is `data\sessions\<id>\`:

```text
session.json        name, colour, auto_named        view.json / snapshot-*.json   captured sky frame
catalog.json        archive + survey candidates       selection.json / triage.json  chosen / dismissed
edits.json          editor state (+ edits.rev.json)   markers.json                  numbered markers
grid.json           locked canvas once edits exist    exports\                      outputs + timing sidecars
project\meta.json   build output: grid WCS, layers (stats, coverage, mosaic tiles), luminance
project\<key>.f16   float16 layer planes; .coverage.u8, .edge.f16(+.v)
builds\<id>\        archived earlier builds
```

- **Planes are stored top-down** (`image[::-1]` of the WCS orientation). Marker/editor
  pixel `y` corresponds to WCS row `height - 1 - y`.
- **Cache versions:** bump `BUILD_VERSION` (build.py), `EDGE_VERSION` (edge maps),
  `mosaic.VERSION`, `REPAIR_VERSION` (surveys.py) or
  `PREVIEW_VERSION` (canvas.py) when you change what those products contain. Otherwise
  stale caches are silently reused.
- **Edits must keep old projects' looks.** New controls default to no-ops; migrations
  go in `reconcileState` (frontend) and are mirrored where the backend reads state.
  Exception: retired star-only edits/assets are ignored; ordinary edits still survive.
- **Builds lock the grid** once edits exist; edits and markers are remapped through sky
  coordinates when the canvas changes.
- **Recapture** is allowed only until a project is built (`project\meta.json` exists).
- Exports write `<name>.json` sidecars with `wall_s` (excluding queue wait) and
  `estimate.model_s`; `estimate_calibration` uses the last 8 to correct future estimates.
- Remote services: CDS (hips2fits, alasky, MocServer) can be unreachable. Code must
  degrade (coordinate grid backdrop, outage pill, friendly errors), not crash.

## Testing policy

- Test **contracts and non-trivial calculations** in Python (`tests/`) and Vitest
  (`*.test.ts`). Keep tests few and informative.
- **No brittle UI tests**: no Playwright assertions on element IDs, classes, layout or
  placement. For visual checks, write a throwaway screenshot spec, inspect, delete it.
- Tests never touch the network or `data\sessions`; use temp dirs and mocks.
- `frontend/e2e/BASELINE.md` explains the e2e harness.
