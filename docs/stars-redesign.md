# Stars redesign — implementation spec

> Historical, retired spec. Star detection, photometry, separation and their UI
> have been removed. This document is not a current implementation contract.

Status: **implemented (historical design record).** Current behaviour is described in `README.md`
(Stars sections); code lives in `stars.py`, `build.py`, `fullres.py` and `editor/compositor.ts`.
Since this spec, star detection became opt-in per project and parallax moved into the Stars tool.

## User goals (priority order)

1. **Star colour** — natural white / blue / yellow / red stars instead of layer-tinted stars (e.g. all stars hot pink).
2. **Star reduction** — shrink bloated stars, overall and individually.
3. **Brightness** — stars brighter/dimmer than the nebula.
4. **Diffraction spikes** — tame or emphasize.
5. **Parallax** — keep, out of the way, and make depth *continuous*: today every star sits on one of two flat
   planes; some should feel very close, most far (start from relative size/brightness).
6. **Pick individual stars** — nice to have, mostly for big ones.
7. Native export speed — star separation dominates full-res exports.

## Rendering model

`global.stars.color` selects one of three modes:

| Mode | Layers sample | Star pass |
|---|---|---|
| `natural` (default for new edits) | `mix(full, repaired starless, alpha)` | yes — per-star colour |
| `neutral` | `mix(full, repaired starless, alpha)` | yes — white |
| `layers` (migrated edits) | full data (today's behaviour), stars scaled by brightness/reduction/spikes | no |

`alpha` is soft stellar support, not the binary ownership mask: feather residuals from one to three
local noise sigmas, rather than a fraction of the star's peak. This removes real PSF wings and diffraction
spikes without leaving hollow outlines. Unsupported nebular pixels stay exactly full data.
Repair inpainted stellar values to at least the tenth percentile of an annulus outside the mask,
excluding neighboring stars and uncovered pixels. Within validated stellar support, this fill may raise
a dark core to its background; outside support it cannot change the data. The star image uses the same alpha.
Layers mode is exactly full data at unity, and blends toward the repaired fill when stars are dimmed.
Natural/neutral intentionally recolour supported stellar light.

### Star luminance texture (computed once per weights change, not per frame)
`starLum(p) = Σ_i w_i · max(0, full_i(p) − starless_i(p)) / Σ_i w_i` over all filters, `w_i` = per-layer-source
**star contribution** (`edits.starWeights[sourceKey]`, default 1; 0 removes that filter's stars, e.g. the one filter
where a star is huge). **Respect the layer stack**: a filter contributes only if at least one *visible* layer uses
it (as source, mask source, or via spectral/luminance/mean sources, which use all filters), and its residual is
sampled through the transform of the top-most visible layer that uses it (layer transforms are usually identity).

### Star texture pre-pass (only when star controls, overrides, weights, or layer visibility change)
Render one RGBA16F **star image** at build resolution (RGB = star colour × L, premultiplied), so the per-frame
cost is tiny:
- `id = label(p)`; 0 → transparent.
- `v = starLum(p) · brightness · override.gain · (spike(p) ? spikes : 1)`.
- **Reduction = radial halo trim** (no resampling, so no aliasing and no ownership artefacts):
  `r = clamp(global.stars.reduce + override.reduce, 0, .9)`; `R' = radius(id)·(1 − r)`;
  `v *= 1 − smoothstep(.55·R', R', |p − c(id)|)` (cores stay; wings/halos/spikes are trimmed; r = 0 → no-op,
  i.e. skip the term entirely when r = 0 so default output is exact).
- `L = starStretch(v)` — asinh with a fixed `k` and a white point = the 99.5th percentile of star peaks (computed in
  star metadata), so default brightness 1 roughly matches today's look. Tune on M16 so default stars look as bright
  as now.
- Colour: `natural` → `starColor(id)` mixed toward white by `1 − saturation`, warm/cool shifted by `shift`;
  `neutral` → white.

### Star pass (every frame, after all layers, before output exposure/shoulder)
- Parallax off: one fetch, `acc = 1 − (1 − acc)(1 − star(p))` (screen).
- Parallax on: for each depth bin `k ∈ 0..7`: `q = p − parallax·shift(k)`; `id = label(q)`; if `id > 0` and
  `bin(id) == k` screen-add `star(q)`. (3 fetches per bin.)

In `layers` mode there is no star pass: apply brightness/override gain/spikes/halo-trim to the star residual inside
`sampleAt` (`full + positiveResidual·(gain − 1) + max(starless − full, 0)·max(1 − gain, 0)`) —
the per-layer path, with per-star depth bins for parallax.

### Per-star metadata (`project/stars.meta.json` + textures)
Computed lazily on the server (like `ensure_edge_map`), cached, keyed by build_version + file mtimes; computed at the
end of new builds too. Older builds must work without a rebuild. Do **not** bump BUILD_VERSION.
- `id, x, y` centroid (flux-weighted over the core), `radius`, `flux[]` per filter (Σ residual over the star mask).
- **Colour index** (a heuristic temperature, not calibrated photometry — filters are independently normalised at
  build): per filter, divide each star's flux by that filter's median flux over stars that are well detected and
  unsaturated in *all* filters (robust per-filter zeropoint, so a typical star is neutral), then fit slope `β` of
  `log(flux)` vs `log(wavelength)` with **equal weights** in log space. Exclude a filter's measurement when the star
  is saturated there (core plateau / peak ≥ 0.98 of the layer max) or SNR < 5. Need ≥ 3 usable filters (2 if both
  have SNR > 20), else `β = 0` (white). Map `β` through a blackbody-like ramp: negative = blue-white `#9bb0ff`, 0 = white `#fff4ea`,
  positive = yellow `#ffd2a1` → orange-red `#ff8c5a`. Store `colorIndex` clamped to [−1, 1] (robust: divide by the
  95th percentile of |β|).
- **Depth** `d ∈ [0,1]` (0 = far, 1 = near): percentile rank of `log(flux)·0.7 + log(area)·0.3`, curved
  `d = rank^2.2` (most stars far, few near), plus deterministic ±0.06 jitter from a hash of the id; quantised to
  8 bins for the GPU. Parallax shift for depth `d`: `mix(parallax.bg, parallax.fg, d)`. Nebula stays at
  `parallax.neb` (with its brightness shape).
- **Spike mask**: `stars.spikes.u8` per pixel (1 = spike pixel, not core). `detect_stars` already computes
  `spike_mask`; make it also return the spike pixels. For old builds rerun the current quality detector on stored
  luminance and regenerate labels/metadata consistently, preserving IDs by centroid rather than detection order.
- **Detection hygiene (quality v4)**: validate smoothed cores against a local annulus and its structure,
  require radial falloff, and bound FWHM by the compact-star PSF distribution. Limit round masks to twice
  measured FWHM for ordinary stars; confirmed bright stars follow radial PSF wings to local noise,
  bounded by eight FWHM. Only high-contrast bright stars grow diffraction spikes. Do not own negligible
  morphological residuals. Annular floors and noise-weighted alpha are shared with native rendering.
  The lazy quality migration re-detects from stored luminance and regenerates
  separation/appearance files from the unchanged science layers, without a FITS rebuild.
  Match centroids within 1.5 pixels to retain old IDs and their overrides; never reuse rejected IDs.
  Overrides on rejected nebular detections become inert. The quality version invalidates all stellar caches.
- **Saturated cores** (nice to have): JWST saturated cores show as hollow rings at native scale. In the star
  residual, fill dips inside each bright star's core (pixel < max of its core ring) by grey hole-filling limited to
  the core disk. Apply in build (new) and full-res; skip if it costs > 10 % build time.
- GPU textures: per-star lookup textures indexed `(id % 256, id / 256)` like `starClasses` — RGBA16F
  `(centroid.x, centroid.y, colorIndex, depth)` normalised, plus a per-star override texture `(gain, reduce, depthOverride or −1)`.

## Edit state

```ts
global.stars = {
  separate: true /* false uses original full data; all other star settings become inert */,
  color: 'natural' | 'neutral' | 'layers', saturation: 1 /*0..1*/, shift: 0 /*-1 warm..1 cool*/,
  brightness: 1 /*0..2*/, reduce: 0 /*0..0.9*/, spikes: 1 /*0..2*/,
}
global.parallax = { on, fg: 1, neb: .2, bg: .06, shape: 0 }   // fg/bg now = nearest/farthest star depth
starOverrides: Record<id, { gain?: number /*0..3*/, reduce?: number /*-0.5..0.9*/, depth?: number /*0..1*/ }>
starWeights: Record<sourceKey, number /*0..1*/>
```

Build's **Detect stars** option persists as `session.json` → `stars.detect` (default true)
and is recorded in `meta.json` → `stars.detect`. Switching it requires a rebuild.
With detection off, labels and the star list are empty, starless layers equal full layers,
no spike/appearance maps are generated, and lazy metadata generation is bypassed.
The editor forces separation off and shows “Rebuild with Detect stars on to enable”.

The extended-structure auto-suggestion is deferred: the two frozen fixtures have
24,962 and 46,319 detections; respectively 1.98% and 1.81% sit above the positive
starless-background 90th percentile. Neither is a labelled galaxy failure case, so
these measurements do not support a reliable warning threshold. No speculative
warning is shown and separation is never changed automatically.
- **Migration** (state.ts load + edits.py normalise, idempotent, round-trip tested): edits with legacy
  `global.stars.{fg,bg}` → `color: 'layers'`, `brightness: 1`, and for every star its effective legacy class
  (`starClasses[id] ?? star.fg`) gives `starOverrides[id] = { gain: fg|bg (omit when 1), depth: 1|0 }` so the
  migrated look is **identical** (verify pixel-identical NumPy render before vs after migration on M16). Only then
  drop `starClasses` and the fg/bg fields. New sessions (no edits yet) → `natural`.
- Everything undoable; `astroimp edit set global stars.brightness=1.3` etc. must work (edits.py ranges/choices,
  editcli docs in README).

## UI

### Stars tab (replaces STAR CLASSES)
- **Colour**: Segmented Natural / Neutral / From layers; Saturation; Warm ↔ Cool (hidden in From layers/Neutral where meaningless).
- **Stars**: Brightness, Reduction, Spikes.
- **Per filter** (collapsed): one compact slider per filter (filter name + wavelength) for star contribution.
- **Parallax** (only when parallax on): Nearest, Nebula, Farthest depth, Brightness shape.
- Reset button (RotateCcw icon) restores defaults incl. clearing overrides (with count shown, e.g. "3 edited").

### Stars tool (canvas)
- Hover shows the star's ring (existing overlay); click opens a small popover anchored at the star: Brightness
  (0 = remove), Size (`−override.reduce`, range −0.9…0.5, default 0; higher cancels more global reduction),
  Depth (only when parallax on), reset icon. Stars with overrides get a subtle ring.
  The Stars tab's Reduction slider is deliberately the opposite direction: higher trims more.
- Tool options bar: Brightness / Reduction / Spikes sliders + Colour segmented (compact, icon buttons, tooltips).
- Remove "Toggle all", fg/bg classes and the "Depth" text button.

## Export / parity / performance
- Coordinate conventions: centroids/radii in build-grid pixels, pixel centres at +0.5, y down. NumPy `Project` at
  `step > 1` block-averages images but decimates labels — compute the star image at full build resolution, then
  block-average it (exact parity is required only at step 1; analysis previews may differ slightly).
- Mirror the star pass exactly in `render.py` (also used by analyze, probe, contact sheet, thumbnails and
  full-res). E2E parity GPU vs NumPy for: natural + reduction + spikes + an override (mean < 0.5/255), and layers mode.
- Full-res (`fullres.py`):
  - Star metadata/textures at native scale: map centroids/radii from the build grid (scale factor), labels/spike mask via
    upsampling the build mask (nearest, without dilation) — do not re-detect at native scale. Sample the same
    soft residual support used by the preview; unity uses full data without cached inpainting correction.
  - **Local star-box inpainting**: run `starless()` only on padded bounding boxes of star masks (merge overlapping
    boxes), not whole tiles — target ≥ 5× faster star stage. Skip separation entirely when it's a no-op (`layers`
    mode, brightness 1, reduce 0, spikes 1, no overrides, all weights 1).
- Parallax is preview-only (not exported), as today.

## Tests
- Python: colour-index sign (synthetic blue vs red star across 3 wavelengths), depth distribution (most far),
  migration of legacy edits, reduction keeps centroid and shrinks area, spike gain only affects spike pixels,
  local inpainting equals full inpainting within tolerance on a synthetic field.
- E2E: parity cases above; Stars tool click → popover → brightness 0 removes the star (pixel check); tab controls
  undoable.
