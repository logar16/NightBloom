# NightBloom reference

Detailed feature, development and legacy CLI documentation. For a quick start,
see the [README](../README.md) or [setup guide](GETTING_STARTED.md).

**NightBloom** is a local false-colour astronomy art tool: find public Hubble/JWST
and survey images, align them, compose colours live, and export your artwork.
The Windows desktop app needs no Python or Aladin. Aladin Desktop is an optional
way to explore the sky and capture a view. The existing Siril workflow remains available.
Existing `astroimp` commands, package names, API headers and library locations stay unchanged.
Contributors and coding agents: start with [`AGENTS.md`](../AGENTS.md) for the code map and rules.

## Getting started

Windows x64 preview packages are published on the
[Releases page](https://github.com/logar16/NightBloom/releases):
an installer and a portable ZIP. Read the
[getting-started guide](GETTING_STARTED.md) for installation, a first project,
optional Aladin downloads/SAMP setup, and troubleshooting. These previews are
unsigned; they include no projects or personal library data.

Licensed under [MIT](../LICENSE). Dependencies retain their
[own licenses and notices](../THIRD_PARTY_NOTICES.md). Astronomy data and imagery
remain subject to their providers' terms and credit requirements.

## Windows desktop preview (Rust/Tauri)

The desktop preview runs the existing React/WebGL editor with a Rust backend.
**Opening built projects, editing, autosaving/reopening, and image export need no
Python, `uv`, Aladin, or CDS.** New projects also run natively: capture a SAMP-connected
Aladin view, search by name/coordinates, or frame saved bookmarks; select archive/survey
images, build aligned layers, then edit and export. Capturing Aladin requires Aladin;
new archive searches and uncached downloads require their remote services.

Launch **NightBloom** from the Windows installer, or double-click
`astroimp-desktop.exe` in the portable build. Keep its `frontend` folder beside
the executable. Microsoft Edge WebView2 Runtime is required.
Choose a **library folder** on first launch, or change it in **Settings → Storage**.
Downloads, projects, exports, export scratch files, and WebView browser data stay
inside that folder. Only the small location setting lives in
`%LOCALAPPDATA%\com.astroimp.studio\settings.json`; bulk data is not automatically
created on C:.

The layout keeps the existing project format:

```text
<library>\
  downloads\                 original FITS, shared across projects
  sessions\<id>\             project state, aligned images and build history
    exports\                 rendered images and timing sidecars
  cache\                     disposable caches and temporary export work
  .runtime\webview\          browser preferences and cache
```

Importing a **data folder** copies projects, bookmarks and all available raw
downloads, verifies file contents, and makes science references library-relative.
It never changes the original, duplicates downloads per project, or imports frozen
test fixtures/disposable legacy caches. Already missing FITS cannot be recovered;
Storage reports unavailable references while cached projects remain usable.
A **single-project** import keeps external source references, so retain those inputs.
Library-relative science references are a Rust-desktop extension; continue using the
unchanged original data folder with the Python app.
Changing libraries schedules a copy on the next launch: the old library is locked,
files are verified, and the saved location switches only after success. The previous
folder is retained. An interrupted transfer can resume; conflicting files are refused.

Home's delete control moves a project into the library's Trash. **Settings → Storage**
can permanently remove explicitly selected unused downloads, deleted projects and
regenerable caches. Live projects, archived builds, recovery backups and frozen test
fixtures protect their source files; live exports and build history are not cleanup
targets. Cleanup is blocked while any job runs.

**Home** is a glass-bento gallery: one artwork cover per project, with image actions
revealed on hover or keyboard focus (always visible on touch). A compact left launcher
has **New project**, **Open image** and scrollable recent-project thumbnails, including
projects without exports. Hover or keyboard focus expands it over the artwork to show
project names; the Projects icon toggles it on touch. Click a thumbnail to resume.
Browse exports without opening the editor or loading science
layers. Covers use the newest photo within 10% of the largest long-edge resolution.
Expand a cover for a newest-first carousel: exports within five minutes of
the newest in each batch show larger pixel dimensions first. **Open current project** resumes the latest
edits; **Edit exported version** restores that particular saved look.
**Copy image** copies full-size, 8-bit PNG pixels;
**Save original image** preserves the original file and bit depth.
**Show image in folder** selects the original export in the desktop app.
New exports retain their exact edits and build metadata in small, deduplicated
checkpoints; different output sizes and formats share the same saved look.
Existing exports without saved edits cannot be reconstructed retroactively.
**Edit exported version** saves your current look as a return checkpoint before
opening the saved edits and their matching build. Return through **Checkpoints**;
if the original build is missing, your current edits stay untouched.
New PNG exports also embed a small project/checkpoint link, preserving their
pixels and 8/16-bit encoding. **Open exported PNG** on Home accepts a
file from elsewhere on your PC; pasting or dropping the file works too.
The linked project, checkpoint and original build must still be in your library.
File copies preserve the link; other image/clipboard applications may strip it.
No project files or source paths are embedded in the image.

**Export** saves directly into the project's `exports` folder. **Export as…**
(the folder icon in the export dialog) additionally offers a native Save dialog.
Its initial folder is the project's exports, not Windows Downloads.
The desktop export dialog estimates file size from the current encoded preview;
PNG/JPEG sizes vary with source detail, while RGB16 TIFF is uncompressed. Changing
resolution or JPEG quality updates the estimate; native memory limits still apply.
The folder icon beside the project name opens its files. An export-completion
toast's **Open image** uses the default Windows image app, while **Show in folder**
selects the saved image in File Explorer. Both use
the library copy even if **Export as…** also saved a copy elsewhere.

The desktop listens on loopback port **8766**, separate from the Python app on
8765. A port collision or a second process using the same data folder fails
explicitly; it does not attach to or stop the other process.
`--data`, `--import`, `--frontend`, and `--port` are optional launch arguments.
`--data` overrides the remembered library for that run; its folder control is read-only.
The repository's live `data` folder is refused by default: use import or a copy.

To copy a library without launching a window, and remember it for normal launches:

```powershell
.\src-tauri\target\debug\astroimp-desktop.exe --data D:\NightBloomLibrary --import D:\ExistingAstroImpData --import-only --set-default
```

PNG (8/16-bit), JPEG, and TIFF exports support layer/global controls.
Star detection, photometry, separation, and their controls have been removed from
both backends and the editor. Old projects still open using their original science
layers; legacy star-only looks and assets are ignored. Native enlargement supports
undistorted RA/Dec TAN FITS in ICRS or FK5 J2000. Verified compressed input is
32-bit integer RICE/GZIP1/GZIP2 with full-width, single-row tiles; other compressed
layouts and distorted WCS fail explicitly. Survey projects support source-backed
PNG (8/16-bit), TIFF16 and JPEG exports from their downloaded science FITS,
including mixed archive/survey compositions. Available resolution comes from those
files' actual sampling, not advertised survey resolution. Exports do not refetch
finer survey data or fall back to enlarged editor planes when a source is missing.
The **Preview PNG (8-bit)** quick-export path remains separate.
The full-frame renderer enforces allocation budgets rather than promising unlimited
native exports. A four-layer JWST project was exported at **2551 x 4096** in
35 seconds with about 2.3 GiB peak working memory; its true **9097 x 14608** native
size exceeds this milestone's budget. Source preparation retains float32 precision
until output encoding. Time estimates still use the Python empirical
model and are not yet calibrated for Rust. **Build alignment** has a separate
native estimator: measured preparation, reprojection, mosaic, normalization and
write timings are reused across compatible builds. Unknown costs stay
**Estimating…**; the first unfamiliar build may not have an ETA.

Native builds include same-filter mosaic calibration, cached FITS downloads,
capture rotation, locked-grid edit/marker remapping and previous-build restore.
Build marks estimated **internet-transfer payloads** with `~`, accounting for
native DSS2/Pan-STARRS cutouts, IRSA tiles and full MDW fields rather than the
smaller reprojected FITS. DSS estimates read the field's source sampling from FITS
headers (some blue plates use 1.7-arcsecond pixels rather than 1 arcsecond).
Cached sources are excluded from transfer totals; tooltips explain the estimate,
and unavailable sizes remain explicit. Received
payload bytes replace estimates when a download finishes. Compression, partial
coverage, retries and fallback sources can change the final amount.
Failed or cancelled rebuilds leave the completed project and its edits intact;
interrupted publication is recovered on the next project access. CDS outages retain
independently verified DSS2, WISE, MDW and Spitzer alternatives, with an explicit warning.
MDW Hα uses calibrated science FITS from the Orion-region DR0 and northern DR1
releases, checking real field footprints and feathering overlapping fields.
Whole MDW fields (typically about 67 MB each) are cached and reused by previews
and builds. Spitzer IRAC's four infrared bands use the independent IRSA mirror,
with Galactic-frame coverage and real-pixel checks; neither survey is assumed
to cover the whole sky. These sources support the editor, quick exports and
source-backed exports at the resolution available in the downloaded FITS.
Fields wider than 30 arcminutes skip the automatic HST/JWST search;
**Also search HST/JWST** explicitly requests it. This is a search policy, not a
limit on suitable compositions. Pan-STARRS is no longer offered for new selections;
existing selected images and built projects remain usable.
Capture rotation displays **Updating view…** through search and image refresh.
Expanded previews retain their thumbnail while loading, with an explicit retry on failure.

The optional region-first **Canvas** picker and **Fill around** still require the
Python backend. Fill around includes automatic survey tone matching and subject
edge feathering. Build normalization and same-filter mosaic matching already run
natively; there is no separate automatic layer-adjustment toolset to port.
Unsupported Rust operations return explicit errors.

Developer builds require current stable Rust/MSVC with Visual Studio C++ Build
Tools, and Node.js 22.12 or newer:

```powershell
Set-Location frontend
npm ci
npm run desktop:dev     # Vite hot reload; automatically rebuilds/restarts when Rust changes
npm run desktop:run     # debug desktop; reads the remembered library, no hot reload
npm run desktop:build   # release executable and frontend folder under src-tauri\target\release
npm run desktop:bundle  # Windows NSIS installer under src-tauri\target\release\bundle\nsis
```

For shareable packages, from the repository root in PowerShell 7:

```powershell
.\scripts\package-windows.ps1
```

This regenerates locked dependency notices, builds the frontend and Windows
installer, and creates the installer, portable ZIP, build provenance and
`SHA256SUMS.txt` under `artifacts\0.1.0`. It copies only the executable, built
frontend, setup guide and licenses, never the library or repository `data`.
`-SkipBuild` only repackages already-built artifacts; do not use it after changing
application code or resources. Publish only a package whose `BUILD-INFO.json`
reports a clean working tree at the intended commit. No signing is configured.

For development, use `desktop:dev` rather than `desktop:run`. Close any desktop
started with the one-shot command before switching; the watcher must own the
process it restarts. Tauri starts Vite on `127.0.0.1:5173`, with `/api` forwarded
to the native backend on port 8766. React changes hot-reload without a production
build; Rust changes rebuild and restart the desktop. Stop the command to stop both.
The development origin has its own appearance/export preferences; projects stay
in the remembered library. `desktop:run` still builds and serves a production
frontend bundle once, without watchers.

Headless backend and contract checks:

```powershell
cargo test --manifest-path src-tauri\Cargo.toml --lib
cargo build --manifest-path src-tauri\Cargo.toml --bin astroimp-server
uv run --no-sync python tests\verify_rust_milestone.py --server src-tauri\target\debug\astroimp-server.exe
```

## Python app

The Python project uses the package index configured in `pyproject.toml`. If that
proxy is unavailable on your network, use your approved index with
`uv sync --default-index <index-url>`. Windows desktop builds and packages do not
use Python or this index.

```powershell
uv run astroimp
# Use an existing build without checking/rebuilding:
uv run astroimp --no-build
# Or, without automatically opening a browser:
uv run astroimp studio --port 8765 --no-browser
# An isolated session/cache directory:
uv run astroimp studio --data .\studio-data --no-browser
```

The server listens only on `127.0.0.1`. **New project** opens Search or
**Capture from Aladin**. Search accepts target names, decimal RA/Dec, or
sexagesimal coordinates; MAST resolves names first, with Sesame as fallback.
Nearby public HST/JWST coverage suggests a padded frame (16:9 by default);
adjust width, aspect and rotation before Capture. No archive coverage? Choose
**Use surveys**, or open the position in connected Aladin. Coordinate projects
keep the chosen frame on their first build.
Coordinate projects need no Aladin or snapshot: Sky uses a DSS2 red preview,
falling back to a neutral RA/Dec grid with selectable footprints when CDS is unavailable.
Archive checks are bounded and cached; slow coverage checks update in place.
The header shows a small warning icon when CDS or MAST is unreachable (probed in the
background); its tooltip says which features are affected. Survey searches that fail
during an outage show a **Retry** callout instead of an empty list.

Bookmark HST/JWST images on Sky (or press **B** in Preview) to keep them across
projects, independently of per-project favourites. **New project → Bookmarks**
groups saved images by target and source, with notes and multi-selection.
**Start project here** fills Search with a padded footprint frame for review
before Capture. Frames are capped at 30′; larger selections show a cropping
warning so you can narrow them. Capturing keeps the bookmarks.
The global library lives in `data\bookmarks.json`; thumbnails use stored MAST
URIs and the shared preview cache, even after the source project is removed.

Capture the SAMP-connected Aladin view,
select filter images, then build aligned image layers. Sessions and edits persist
under `data\sessions`; science downloads are shared under `data\downloads`.
**Build → Merge tiles** (on by default) combines same-program, instrument and filter
tiles into one layer: robust linear overlap matching, feathered averaging, then one
shared normalization. Native exports reuse the recorded tile gains/offsets.
Disconnected tiles retain their own calibration; provenance and unmatched tiles appear
in `project\meta.json`. Changing this option requires rebuilding; regrouping preserves
the first tile layer's artistic edits. Turn it off to build separate tile layers.
Existing downloads with matching filenames are reused, including older target folders.
Views wider than 30 arcminutes automatically search surveys only; **Also search
HST/JWST** adds the archive results without losing selections. Smaller captures search
both.

On Sky, **Recapture** (scan icon) replaces the view and search with Aladin's current
view, e.g. after zooming; it is available until the project is built, which locks the
view. **Re-grab** (refresh icon) only refreshes the snapshot image and keeps the search.
The footprint focus toggle hides all footprints except selected and hovered ones.

Aladin's snapshot WCS does not report display rotation. On Sky, use **Rotation**
to match NightBloom's N/E compass to the compass in the captured image.
Positive angles turn north clockwise from up: **+90° means N right, E up**.
The angle input and arrow keys support 0.1° nudges; the adjacent buttons turn by
90° or reset. Each new capture starts at zero. Rotation refreshes the search and
survey footprints without contacting Aladin. Existing artwork stays unchanged
until Rebuild, which reframes the canvas and maps edits/markers through sky coordinates.
The original unrotated header and geometry persist in `snapshot-wcs.json` and
`snapshot-view.json`, so angle changes are independent and reset is lossless.

**Surveys** offers IPHAS DR2 and VPHAS+ DR4 Hα/optical, WISE W1–W4
(W3/W4 trace dust), DSS2, SHASSA/VTSS continuum-subtracted Hα,
Herschel PACS/SPIRE and 2MASS. Availability uses CDS coverage MOCs; patchy surveys
can still have gaps within the view. DSS2 is photographic, not a linear flux image.
By default, survey cards show only bands with typical detail ≤ 4″ or ≤ two pixels
in a 4096-pixel export of the captured view's longest side. The in-results
**Lower-resolution surveys** section contains the rest, dimmed but selectable,
with their approximate detail and minimum crisp view size in the tooltip.
It opens automatically when these are the only matching observations.
Older sessions receive this filtering on read;
survey layer info also reports the approximate detail.
Survey bands behave like other editable layers. Their FITS cutouts arrive directly
on the canvas grid through CDS hips2fits, with bounded tiles, retries and a mirror.
Both hips2fits servers are at CDS. When CDS is unreachable, DSS2 red/blue/NIR can
use STScI's native FITS cutouts, and WISE W1-W4 can use IRSA's FITS HiPS mirrors
with local reprojection. Legacy Pan-STARRS selections retain STScI source support. DSS2 red also
has an IRSA FITS mirror fallback. No JPEG display tiles are substituted for FITS.
STScI DSS2 cutouts overlap, match brightness from shared sky pixels, and feather their
joins before previewing or building. This mitigates photographic-plate seams without
fitting away real nebular gradients; overlaps that cannot be matched use feathering
only and are flagged in the FITS provenance. Existing built layers keep their original sources.
Discovery falls back to independent coverage MOCs and native-archive probes,
reports that coverage is partial, and omits surveys it cannot independently check.
Native-archive probes sample the view; patchy images can still contain gaps.
The coordinate sky preview tries these alternatives even when the CDS health
indicator is down, retaining the coordinate grid if no image can be loaded.

Independent sources use separate, versioned caches and record their provenance
in the downloaded FITS and built layers. Existing cached CDS files stay untouched.
Rebuilds and native exports keep the built layer's source rather than silently
changing its appearance; an existing CDS-backed layer still needs CDS if a
matching cutout is not cached. Other surveys, including IPHAS/VPHAS+ H-alpha,
Herschel and 2MASS, currently have no independent fallback in NightBloom.
Raw survey mosaics can retain frame-to-frame sky offsets and detector artefacts
(visible in the IPHAS Heart-nebula field); WISE dust bands offer smoother wide-field
backgrounds. NightBloom does not silently flatten real extended emission to hide them.
Shared files live in `data\downloads\surveys` and participate in Storage protection
and cleanup. Survey-only first builds use the captured field and the finest selected
native HiPS pixel scale, limited to 3072 pixels on the longest side. Native exports
request more detail only up to the survey's native scale.

For a built HST/JWST composition, **Build → Fill around** extends each canvas side by
25%, 50% or 100%, and adds covering IPHAS/VPHAS Hα (otherwise DSS2 red).
It keeps the pixel scale when possible; larger fields coarsen only enough to stay
within the 3072-pixel preview limit. Build shows the resulting dimensions and scale.
It places the survey underneath, matches overlapping tones and softens
the subject's edges. Change the survey through the Sky cards; the **Before fill**
checkpoint preserves the original look, and Build history restores the original grid.
Matched layers show their gain/offset under Tone, with an undoable reset.
This is an artistic blend, not photometric calibration.

This research made use of hips2fits, a service provided by CDS.
Preview thumbnails are at most 320 pixels; the separately cached lightbox images
retain up to 1400 pixels for inspecting the data before downloading science files.
Edit’s Export menu also saves a contact sheet of visible filters, colour recipes, and the final composite under the session’s `exports\` folder.
CPU analysis and contact sheets composite the same original science layers and
ordinary layer/global controls as the editor.
Edit's **Output · all layers** adds manual **Black point**, **White point** and
**Midtones** after Exposure and Highlight roll-off, before final clipping.
Midtones below 1 brighten; above 1 darken. These controls autosave, undo/reset and
apply to preview/native exports and contact sheets; neutral defaults preserve old
projects. They do not alter source FITS or layer-thumbnail metering. No Auto is applied.
Click the project name in Edit's title to rename it.
Edits autosave; Ctrl+S creates a checkpoint, colour captions live in Color properties, and Fit/zoom sit in the canvas corner.
Applying **Crop** locks its current rotation. Subsequent rotation turns the cropped
picture as a whole; quarter-turns swap its dimensions, and other angles export its
bounding box with the same opaque charcoal corners as out-of-image areas.
Rotation refits and centres the view. Flip remains image-relative (source X flip,
then rotation/crop), not a reflection of the final crop. Drawing another crop uses
the current displayed frame and locks the new angle. Crop coordinates in that
rotated frame can extend outside the original 0–1 image bounds.
**Reset crop** restores the full frame and legacy image-under-window rotation.
Saved crops without `crop.rotation` retain their original behaviour.
Layer Tone includes **Invert**, applied after stretch and before colour mapping/intensity;
uncovered pixels stay empty, including in analysis, contact sheets and native exports.
Existing projects retain their look: Invert, Curve, Detail and Layer crop default to no-ops.
**Curve** follows Invert. Click to add points, drag or use arrow keys to move them,
and double-click/right-click a point to remove it. Endpoints move vertically.
The reset icon (or double-click empty space) restores identity. Preview and exports
share a 256-entry, overshoot-free Fritsch–Carlson tonal lookup.
**Detail** adds sharpening at small radii and local contrast at larger radii.
It operates on the stretched tone before Invert/Curve, so bright flux peaks do not
overwhelm faint tendrils. Radius is Gaussian sigma in project/canvas pixels (1–64);
native export scales it with resolution and pads tiles to avoid seams. Preview
caches up to three blurred layers, with memory reserved in GPU preflight.
**Crop layer** restricts only the selected layer to a rectangle or ellipse.
Draw on the canvas, then move the region or resize its handles (also arrow-key
accessible). The cyan dashed boundary is distinct from the global canvas crop.
Coordinates are normalized project/canvas coordinates: moving/scaling a layer
does not move its crop; global crop/rotation only changes the view. Feather fades
inward over 0–0.5 of the region's short side and scales naturally in native exports.
The help button beside **Blend** compares all ten modes using the selected layers'
colours and intensity ramps. Hover or focus a tile for a larger preview with its own
opacity control; click or press Enter to apply it as one undoable edit.
Examples use full-brightness colours by default; **Use layer intensity** restores
the layers' intensity and opacity. Similar hues start with a contrasting orange/teal
pair; switch between **Layers** and **Contrast** without changing the composition.

NightBloom opens on Home. Continue or a recent-session card resumes directly in Edit
for built sessions, Build for selected images, or Sky for a fresh capture.
The NightBloom wordmark returns Home; New project also opens directly at `#/new`.
Sessions are ordered by the latest edit, selection, rename, or catalog update.
Artwork thumbnails cache the latest readable exported image (including 16-bit
PNG/TIFF), falling back to the captured Aladin snapshot or a coordinate-frame grid.

### Several projects at once

Each workspace has a project name and colour tag. Project URLs
(`#/p/<session-id>/edit`, `/sky`, or `/build`) survive reloads and can be opened
in separate tabs; Ctrl-click or middle-click a Home card to open another tab.
The activity button in the header lists progress and cancellation controls for
all projects. Home cards show their live progress too.

Captures and downloads run concurrently. Alignment, star processing, canvas
previews and native export rendering share a FIFO processing queue; a waiting
project names the project currently processing. An active job blocks changes
and deletion only for its own project. Opening another project still flushes
the previous composition's autosave first. Storage cleanup waits for all jobs.

API clients scope requests with `X-AstroImp-Session: <session-id>`; image and
download URLs use `?session=<session-id>`. Without either, requests use the last
project opened in NightBloom. `/api/status` includes `jobs` for the shared activity
tray and `job` for the requested project. Cancel a particular job with
`POST /api/job/cancel?id=<job-id>`.

The assistant CLI defaults to the last opened project. To target another:

```powershell
uv run --no-sync astroimp edit --session <session-id> show
uv run --no-sync astroimp edit --session <session-id> set global output.exposure=0.2
```

Restart NightBloom after updating to enable project-scoped requests and the queue.

Headless stages:

```powershell
uv run astroimp capture
uv run astroimp build data\sessions\<session-id>
```

Builds use FITS memory mapping, strip-wise block means for large images, and an
output grid capped at 3072 pixels per side. Each session's `project\meta.json`
describes top-down little-endian float16 science layers. Finite-coverage masks
accompany the cached layers; luminance averages only layers covering each pixel.
Unchanged sources on the same grid reuse their aligned layers; unchanged rebuilds
also reuse luminance and edge maps. Input file versions, coverage and processing
versions invalidate that cache; missing products are regenerated.
Science planes convert from memory-mapped files in bounded strips.
Once edits exist, rebuilds lock the previous canvas (WCS, dimensions and pixel scale)
in the session's `grid.json`. Added images are clipped to that field; Build shows
the locked size and approximate footprint outside the canvas for each image.
**Canvas** downloads missing images and makes a cached preview mosaic, preserving
the orientation of a rotated capture or existing canvas (otherwise north-up).
Draw the maximum field you intend to use, move its rectangle or resize its corner
handles, optionally snapping to Square, 4:5 or 3:2. Size, pixel scale, detail and
each image's retained footprint update before **Apply & build**. Escape cancels.
Resolution is chosen from that region, not the unused extent of a wide image:
the finest native scale among images covering at least 1% of the region, capped
at 3072 pixels on the longest side. First builds can still use **Build** directly
to fit the full union; rotated captures instead use their captured frame.
The chosen sky corners and scale persist in `grid.json`.
Canvas changes map edits and markers through the sky and save a **Before canvas
change** checkpoint. Same-scale, integer-aligned changes translate geometry
exactly. Output rotation and flip stay unchanged. Build history retains the original pre-expand
project and edits. Restoring a build restores its saved look and locked grid,
remapping current markers (or current edits when that archive has no saved look).
Recovery copies named `_backup_*` stay out of the recent-session library.
Thin low-exposure detector gaps and bad columns are repaired from high-weight
neighbours at source resolution, before either build or native-export resampling;
compact saturated star cores and the existing border-fringe cut are preserved.
The unrepaired source sets the original tone calibration, so removing bright
artefacts does not brighten every star or shift an existing composition's colours.
After updating from an older build, restart NightBloom and choose **Rebuild** on the
Build screen to refresh the cached layers; existing projects are not automatically rebuilt.

### Full-resolution export

Choose **Export → Image…** for Preview, 2K, 4K, Native, or a custom long side.
The dialog shows crop dimensions and an estimated render time (a fitted model of
source size, mosaic tiles and output area, corrected by this machine's
last eight exports), defaults to 4K
(or Native for smaller sources), and remembers your resolution/format choices.
PNG supports 8 or 16 bits, TIFF preserves 16-bit RGB for further editing, and JPEG
offers a quality control. Preview + PNG (8-bit) uses the instant GPU export;
other combinations use a background server job and the existing progress indicator.
Completed files download and remain in the session's `exports\` folder.
Native exports use the finest visible source's pixel scale. Preview layers are not enlarged:
science data is reprojected, tone-matched to the editor, and composited in tiles.
Temporary on-disk layers are removed when the job finishes or fails.
Reprojection uses eight bounded workers, and rendering reuses per-tile samples
of the original science layers.
The top bar shows job progress on every screen. Its Cancel button cooperatively
stops capture, build/download, and native export at the next checkpoint; a source
reprojection or network read already in progress must finish first. Cancelled
exports remove their work files and partial image. Cancelled builds keep downloaded
science files and leave the previous completed project untouched.

```powershell
uv run astroimp export-full data\sessions\<session-id>
# Optional intermediate resolution (1 = preview; maximum = native):
uv run astroimp export-full data\sessions\<session-id> --scale-factor 2 --format tiff16
uv run astroimp export-full data\sessions\<session-id> --long-side 4096 --format png16
uv run astroimp export-full data\sessions\<session-id> --long-side 2048 --format jpeg --quality 92
```

`POST /api/export/full` accepts `long_side` (or `scale_factor`), `format`
(`png8`, `png16`, `tiff16`, `jpeg`), and JPEG `quality` (1–100, default 92).
Poll `/api/job` for its `result.path` and `result.download_url`; starting another job
for the same project while one is active returns 409. Other projects can download
and queue their processing independently.

### React frontend

The Vite + React + TypeScript frontend lives in `frontend\`; FastAPI serves
`frontend\dist\` at `/`. `uv run astroimp` (or `astroimp studio`) automatically
builds missing or stale frontend output, running `npm ci` if dependencies are absent.
Node.js/npm must be on PATH. `--no-build` skips this step; without a build,
NightBloom displays a 503 page with build instructions.

```powershell
Set-Location frontend
npm ci
# Keep NightBloom running on 8765; Vite on 5173 proxies /api to it:
npm run dev
```

For an isolated backend on another port, set `ASTROIMP_DEV_API` to its loopback
URL before starting Vite (for example, `http://127.0.0.1:8876`).

`npm run typecheck`, `npm test`, and `npm run e2e` run TypeScript, unit tests, and
headless browser tests. The browser suite copies the two frozen reference sessions in
`data/test-fixtures/sessions/` (outside `data/sessions`, so they never appear on Home)
to its own run directory; live capture, Aladin operations, and downloads are
blocked. To regenerate API types, run `npm run gen:api` with NightBloom running on
port 8765, or `npm run gen:api -- http://127.0.0.1:8876` for an isolated server.

Editor assets prefetch with four bounded requests while GPU uploads retain source order;
switching projects cancels pending requests. For opt-in User Timing measurements, set
`window.__astroimpPerf = true` before opening Edit. The frozen-fixture benchmark runs
three repetitions with `UV_NO_SYNC=1`, `ASTROIMP_PERF_STAGE=<label>` and
`ASTROIMP_PERF_DIR=<output directory>` via `npx playwright test performance.spec.ts`
in `frontend`; results append to `frontend.json`. Fetch timings overlap; upload timings
measure CPU submission, not GPU execution. Normal rendering never forces a GPU finish.
Performance mode retains the frozen fixture's 3072-pixel long side and uses hardware
D3D11 on Windows; ordinary parity tests retain their bounded 384-pixel SwiftShader
grid. `ASTROIMP_PERF_SEQUENTIAL=1` selects the serial reference, and
`ASTROIMP_PERF_PREVIEW=1` simulates a 2048-pixel GPU texture-size limit.
`ASTROIMP_PERF_POINTS=1000` exercises a growing long stroke instead of the default
100 scripted pointer moves.
Before allocating project textures, Edit estimates GPU storage against a conservative
1.5 GiB budget and checks texture-size/array-depth limits. Oversized projects use a
half-resolution preview, clearly badged; exports still render on the server at the
requested full quality. Array-depth limits require fewer sources (downsampling cannot
change them). Context loss displays a recovery message and Reload editor button.
Layer thumbnails regenerate one per animation frame after the edit debounce, so a
global rotation/crop change no longer performs every synchronous readback in one task.
Brush masks track immutable committed strokes and an append-only live stroke by
reference/point count. Autosave uses state identity while editing, serializing only
when its debounce actually saves (and still suppressing equal-content saves).

Header Settings controls Dark, Light, or System appearance and the primary/secondary
accent colours. Choices apply immediately and persist in this browser; image surrounds
stay neutral dark grey in either theme. Reset to defaults restores graphite and amber/teal.
**Settings → Storage** shows disk usage and shared downloads grouped as **Only in deleted
projects**, **Unused**, or protected **In use**. Select a group or individual files,
then confirm **Delete selected** to permanently remove them. Deleted projects (trash)
can be removed separately; their downloads become Unused unless
another project needs them. Aligned cache and preview thumbnails regenerate automatically
(the next build may be slower). Live projects and frozen test fixtures protect their
sources, including build history. Cleanup is unavailable while a job runs.
The native desktop also protects recovery backups; the Python backend separately
offers explicit recovery-backup deletion.
The backend must be restarted after updating to expose the storage endpoints.
The strongly accented layer row is the active properties target, echoed above the
properties panel. Other selected layers have a softer highlight.

Ctrl/Shift-click layers to select a group. Visibility, Duplicate, and Delete act
on the selection as one undoable step. The group panel combines relative tone
adjustments with absolute Blend, Edge softness, and Edge irregularity controls.
The Color tab's EDGE section feathers a layer's coverage boundary in image pixels;
irregularity makes the boundary cloud-like. Canvas borders are not faded.
Distance maps are cached as `<key>.edge.f16`; existing builds generate them on
demand without rebuilding science layers. Small interior coverage holes (≤4096 build
pixels, e.g. masked cosmic rays) count as covered, so softening affects only real
outer edges; a `.v` version sidecar regenerates older maps. Luminance uses the union of source coverage.

The Feather brush (`B`) has Hide / Reveal visibility modes and Black / White source-intensity modes.
Black and White paint the selected layer's monochrome signal toward 0 or 1 before its
stretch and colour controls; the source FITS stays untouched.
Adjust Size in image pixels (`[` / `]`), Hardness, and Strength; hold Alt to invert
Hide/Reveal or Black/White. Show mask tints hidden or painted regions without affecting exports.
Each stroke is undoable; Clear brush resets the active layer. Brush masks follow layer
transforms and also apply to analysis, probes, and contact sheets.

Use a layer's Solo button to inspect it (or its multi-selection); hold `Q` for a
momentary solo. Release restores the prior view; `Esc` or the Solo badge exits.
Solo is view-only: eyes, edits, history, exports, analysis, and contact sheets are unchanged.
Layer thumbnails show each layer alone at full opacity in the current crop/rotation/flip,
including its brush and edge masks; the narrow strip preserves its colour identity.
Hover a thumbnail (or focus it and press Space) for a larger preview and filter details.

Hold Space or drag with the middle mouse button to pan in any tool; Shift+F toggles
a fullscreen preview; `0`/`F` fits the image and `1` shows
one build-resolution image pixel per screen pixel. Delete/Backspace removes
selected layers, Ctrl+D duplicates them, and F2 or a double-click on a layer name
renames it (Enter commits, Esc cancels). These edits are undoable. `?` or the
keyboard button opens the editor shortcut sheet.

Right-click a layer for rename, duplicate, delete, visibility, solo, stacking,
blend, and transform actions; an existing multi-selection is preserved.
Right-click the image for pixel inspection, contributor selection, markers,
zoom, solo, history, checkpoints, and export. Menus support arrow keys, Enter,
and Esc. Sky cards and group headers also offer selection and triage menus.

The Info tool (`I`) shows the final colour and the strongest contributing layers
under the cursor, including unsaved edits. Size is the averaging diameter in
build pixels (1–200, initially 5); use the Size slider or `[` / `]`. The circle
tracks that diameter at the current zoom, and the last size persists in this
browser. Click to pin the readout; click a contributor to select and reveal its
layer. Esc, another canvas click, or switching tools dismisses the pinned readout.
Probes explain the full composition, independently of view-only solo.

In Sky, hovering an image highlights its own footprint; double-click or Alt+click
a footprint to reveal its catalog card. Group headers select/deselect the group's
non-dismissed images. Preview uses Space/Enter to select, F to favorite, D to
dismiss and advance, and arrow keys to browse.

After backend updates, restart NightBloom before reloading the rebuilt frontend.

### Assistant commands and markers

With NightBloom running, `uv run astroimp edit show` lists layers, `edit analyze`
reports clipping and layer contributions, and `edit set` adjusts the composition:

```powershell
uv run astroimp edit set F187N intensity*=0.6 white=1.4
uv run astroimp edit set global output.highlights=0.5
uv run astroimp edit set F187N brush=clear
uv run astroimp edit markers
uv run astroimp edit probe --marker 1
```

Star colour accepts `natural`, `neutral`, or `layers`. Saturation is 0–1;
shift is −1 (warm) to +1 (cool); brightness and spikes are 0–2;
reduction is 0–0.9. These settings share the editor's undo history.

Use the editor's Mark tool (`M`): click for a numbered point, drag for a rectangle,
or right-click the image and choose Add marker here. Add notes and copy markers
for an assistant; markers persist separately from undo history.
`edit probe X Y` also explains colour at image-pixel
coordinates (`--rect X1 Y1 X2 Y2` for a region).

`POST /api/probe` accepts an optional `state` containing live editor edits instead
of reading saved edits; it does not save that state. Contributor rows include
stable layer `id`s. Omitting `state` preserves the existing saved-edits behaviour.

## Setup

Use Python 3.12+ and [uv](https://docs.astral.sh/uv/). Aladin Desktop and Siril
are installed separately. From this repository:

```powershell
uv sync
uv run astroimp --help
```

The project's package index is configured in `pyproject.toml`; no global Python
package installation is needed.

## Browse in Aladin and send the position

1. Open Aladin Desktop. Use its SAMP indicator/menu and **Connect with SAMP**.
   Allow Aladin to start its internal hub if prompted.
2. In **SAMP preferences**, enable **Reticle coordinates**:
   **Send reticle coordinates when its position changes**. These labels are
   present in Aladin Desktop 12.060. Broadcasting selected image/table planes
   is a different operation and does not send the position this helper needs.
3. Start the listener:

```powershell
uv run astroimp listen --once
```

4. Find an object in Aladin and recenter its reticle on the desired field.
   NightBloom receives the position, searches MAST, prints candidate filter sets
   with their costs and center coverage, and prints the saved manifest path.
   It **does not download images automatically**.

   Each received selection gets its own timestamped manifest, so subsequent
   reticle movements do not change the numbered download list you are using.

Omit `--once` to keep listening. Rapid position updates are coalesced until a
half-second pause; while a search runs, only the latest pending position is kept.
Searches default to a 2-arcminute radius and inspect at most 50 observations,
reporting any truncation. Adjust with `--radius` and `--max-observations`.
`--timeout 60` optionally stops a listener that has not received a position.

If Aladin cannot start a hub, run `uv run astroimp hub` in a separate existing
terminal, connect Aladin to it, then start the listener. This hub binds only to
localhost and does not enable SAMP's browser/web profile. Ctrl+C stops either
command. NightBloom does not launch Aladin or open terminal windows.

## Read the current Aladin view

With Aladin open and connected to SAMP, run:

```powershell
uv run astroimp view
uv run astroimp view --out data\aladin-view.json
```

This reads the view immediately, without moving the reticle or waiting for a
position broadcast. It prints JSON containing the ICRS center and four corners,
angular width/height in arcminutes, projection, pixel dimensions, and capture
time. `--out` also saves the same JSON. Exactly one Aladin must be connected.

The command reads astronomical WCS metadata embedded in Aladin's JPEG snapshot,
not positions inferred from the picture. No science FITS files are downloaded,
no images are imported, and no layers or view settings are changed. Aladin
12.060's normal `status views` reply can time out because its degree symbol is
rejected by the SAMP hub; the snapshot avoids that encoding problem. Actual
JPEG dimensions take precedence over the embedded header's off-by-one sizes.
Angular sizes are separations between opposite edge midpoints; corners are
reported separately, rather than treating a projected view as an exact sky rectangle.

`view` only reads geometry. It does not search MAST, and it does not change
`listen`'s existing center-only behavior. No Copilot session scripts are required.

## Download selected filters and hand off to Siril

Use the manifest path printed by the listener. For example:

```powershell
uv run astroimp groups "data\selections\YOUR_POSITION\products.json"
uv run astroimp groups "data\selections\YOUR_POSITION\products.json" --group G01
uv run astroimp fetch "data\selections\YOUR_POSITION\products.json" --group G01 --out "data\sessions\my-target" --open-siril
```

Replace `YOUR_POSITION` and the example group ID with actual results. Inspect
the group's suggested file IDs and alternatives before fetching it. Size is for
the downloaded products; extracted science images need additional disk space.
Some mosaics are several gigabytes. No image is downloaded by `groups`.

## Candidate sets, alternatives, and framing

Groups keep telescope, observing program, instrument, and archive visit/target
identifiers together. They also **separate product families**: standard images,
HAP visit mosaics, HAP skycells, older HLA detector products, and JWST mosaics.
Thus a larger mosaic remains an explicit alternative to a smaller reduction.
Unrecognized naming conventions are grouped conservatively by observation ID.
These are candidate sets, not a guarantee of aligned images, identical epochs,
matching pixel scales, or complete common sky coverage.

Each set suggests **one image per filter**. Suggestions prefer images whose
own footprint contains the selected center; with a requested framing, enough
sky area is preferred over insufficient area. They next prefer an
image within 10 times the requested area, then a corrected `drc` image to its
`drz` alternative and smaller files within the same family.
This does not determine image quality or signal-to-noise. All alternatives
remain inspectable with `groups --group G01`, using their original file IDs.
Detection/`total` images and unknown filters are excluded from the suggested
sets, not deleted from the raw product list.

The summary prioritizes confirmed center coverage, multiple filters, and
download size. Group IDs do not change when this display order changes. Use
`--filters` to fetch a subset of a group's filters, or select individual raw
files instead:

```powershell
uv run astroimp fetch "data\selections\YOUR_POSITION\products.json" --group G01 --filters F502N F673N --out data\sessions\narrowband
uv run astroimp list "data\selections\YOUR_POSITION\products.json"
uv run astroimp fetch "data\selections\YOUR_POSITION\products.json" --select 27 50 68 --out data\sessions\manual
```

**Zoom is not transmitted by the SAMP reticle message.** To compare images with
your intended crop, supply its angular width and height in arcminutes:

```powershell
uv run astroimp listen --once --field-arcmin 2 2
uv run astroimp groups "data\selections\YOUR_POSITION\products.json" --field-arcmin 2 2
```

Here `2 2` means a desired 2-by-2-arcminute field, **not** a radius or a factor
of two. The search radius remains an independent discovery setting. A new
search saves these framing dimensions so subsequent `groups` and `fetch
--group` commands reuse them. For an older manifest, repeat the same framing
arguments when inspecting and fetching to preserve the suggestion criteria.

Without requested dimensions, sky areas are in square arcminutes; with them,
the table shows the range of image-to-requested sky-area ratios across the
suggested layers. `SMALL` means at least one layer has less area than requested.
By default, a set is hidden if **any suggested image exceeds 10 times the
requested area** (your desired view is less than 10% of that image), provided
there are at least **two good independent fields** to choose from. A good field
has at least two filters, all with trusted footprints containing the selected
center and areas between 1 and 10 times the request. Different processing
families of the same field do not count as independent alternatives. Exactly
10 times remains eligible. With fewer than two good fields, oversized sets stay
visible as labeled fallbacks.

Use `--show-all` on `groups`, `search`, or `listen` to retain every set.
Explicit `groups --group G01` inspection and `fetch --group G01` still work for
hidden sets, with an oversize warning; `list` and `fetch --select` are unfiltered.
No products are removed from the manifest. Without requested dimensions,
size filtering is inactive; unknown footprints remain visible and do not count
as good alternatives. A known oversized layer can still flag a set whose other
layers have unknown footprints.

Optionally add `--max-area-ratio 5` for a separate `LARGE` warning above five
times the requested area. This flag only controls a warning, not the default
10-times hiding rule. These are **area** ratios, not width/height or byte-size
ratios.

Footprint areas are calculated on the sphere, including RA wrap and concave
polygons. They describe observed coverage, **not** the number of pixels,
resolution, or padded bounding image size. Center coverage and adequate area
are necessary checks, not proof that the whole rectangular crop fits.
Unsupported, invalid, or untrusted footprints display `?`, rather than
pretending a parent observation's footprint is the product's own.

Older manifests sometimes attach the parent observation's metadata to a related
product. New searches prefer the product's own observation metadata and filter.
Old manifests still work for grouping, but areas are trusted only when the
saved observation name identifies that product; otherwise they remain unknown.
Known filter tokens in filenames correct some old labels. For complete,
current metadata, repeat the archive search; this still downloads no FITS.

The session contains:

```text
selection.json    Selected archive products and source manifest path
originals\       Unchanged archive FITS files
siril\           One 32-bit, single-image FITS per science extension
siril\layers.json Source paths, filters, HDU indices and image dimensions
```

Downloads stream to temporary files, check the reported size and FITS structure,
and are renamed only after success. A repeated download reuses a complete,
validated original; a conflicting existing file raises an error rather than
being overwritten. Completed files survive cancellation. Incomplete transfers
restart from the beginning on the next attempt.

Preparation copies only `SCI` extensions (or a 2D primary image), preserving
FITS metadata/WCS and pixel values subject to 32-bit float precision. It does
not mix error/weight planes into the image, normalize/stretch the data, assign
colors, downsample, or reproject. Multiple `SCI` detector chips become separate
files, not a stitched mosaic. Cubes/tables are rejected. Generated layers can
be regenerated; originals are never overwritten. `layers.json` lists the
current set; old generated files are not automatically deleted.

**Siril handoff limitation:** `--open-siril` opens the first prepared image and
sets the working folder. It does **not** populate all of the RGB composition
dialog's color layers. In Siril, open **RGB composition**, select the prepared
images, assign any colors, align if needed, and adjust layer brightness/stretch.
The dialog supports up to eight monochrome inputs. Separate archive filters
may have different fields/pixel grids; do not assume they are already aligned.

Siril is detected from PATH or its Windows installation registry entries.
Use `--siril-exe "C:\Program Files\Siril\bin\siril.exe"` to override detection.
Launch uses a detached Windows process, not a new terminal tab. Automatic GUI
launch currently supports Windows only. To open an existing preparation later:

```powershell
uv run astroimp siril "data\sessions\my-target\siril\layers.json"
```

## Without the live bridge

Search by object name, or copy ICRS coordinates from Aladin in decimal degrees:

```powershell
uv run astroimp search M42 --out data\orion-products.json
uv run astroimp search --ra 83.822 --dec -5.391 --radius 2 --out data\orion-products.json
```

Discovery deliberately includes only public, level-3 HST/JWST imaging products
with `i2d`, `drc`, or `drz` suffixes. MAST can return level-2 exposure files even
for a level-3 observation; both observations and products are filtered.
Raw detector exposures, spectra, previews, other missions, and specialized
high-level science-product collections are outside this first workflow.
An empty result does not mean no telescope has ever imaged the target.

## Use images already on disk

No archive query or download is required:

```powershell
uv run astroimp prepare "data\downloads\NGC_6611\jw02739001001_02103_00001_nrca3_i2d.fits" --out data\sessions\eagle\siril
```

You can supply multiple FITS paths. A folder input recursively selects only
`i2d`/`drc`/`drz` products, so it will not accidentally import the old raw
exposures. Be selective: preparing a folder of large mosaics can use substantial
memory and disk space. Prefer a fresh output folder for each target/filter set.

## Retired editor

The previous Streamlit editor, custom alignment/compositing code and related
dependencies were removed. The pre-cleanup source is preserved locally in
`archive\pre-cli-source.zip` (ignored by git). Extract it to a **different**
directory to recover the old implementation; its saved lockfile restores that
environment with `uv sync`. Existing `data\downloads`, `data\uploads` and
`data\aligned_cache` were retained unchanged.

## Development

See [`AGENTS.md`](../AGENTS.md) for the architecture map, invariants and all test commands.

```powershell
uv run --no-sync --with pytest python -m pytest -q
```

Tests use synthetic FITS, analytic spherical-area cases, grouping/download
contracts, and an isolated, real local SAMP hub. Archive/download responses are
mocked, so tests do not need internet access or running GUI apps.

References: [MAST queries](https://astroquery.readthedocs.io/en/latest/mast/mast_obsquery.html),
[PyVO SAMP](https://pyvo.readthedocs.io/en/stable/samp/index.html),
[Siril RGB composition](https://siril.readthedocs.io/en/stable/processing/rgbcomp.html).
