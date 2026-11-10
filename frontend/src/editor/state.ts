export interface GradientStop { t: number; color: string }
export interface SpectralStop { nm: number; color: string }
export interface SourceLayer {
  key: string;
  obs_id?: string;
  filter: string;
  collection?: string;
  instrument?: string;
  name?: string;
  wavelength_nm?: number | null;
  fwhm_arcsec?: number;
  info?: { line?: string; shows?: string; kind?: string; band?: string };
  file: string;
  /** Ignored when reading legacy projects. */
  starless?: string;
  edge_file?: string;
  tiles?: { key: string; obs_id?: string; gain: number; offset: number }[];
}
export interface ProjectMeta {
  width: number;
  height: number;
  built_at?: string;
  layers: SourceLayer[];
  luminance?: { file: string; starless?: string; edge_file?: string };
  /** Optional legacy metadata, never loaded by the compositor. */
  stars?: { file?: string; list?: string; count?: number; detect?: boolean };
}
export type ColorMode = 'tint' | 'gradient' | 'spectral';
export type Stretch = 'linear' | 'asinh' | 'log' | 'gamma';
export type Blend = 'normal' | 'add' | 'screen' | 'multiply' | 'overlay' | 'softlight' | 'lighten' | 'difference' | 'subtract' | 'darken';
export const BLENDS: Blend[] = ['normal', 'add', 'screen', 'multiply', 'overlay', 'softlight', 'lighten', 'difference', 'subtract', 'darken'];
export interface Stroke {
  mode: 'hide' | 'reveal' | 'black' | 'white';
  size: number;
  hardness: number;
  strength: number;
  points: [number, number][];
}
export interface Layer {
  id: string;
  name: string;
  visible: boolean;
  source: string;
  mode: ColorMode;
  color: string;
  gradient: GradientStop[];
  spectral: { stops: SpectralStop[] };
  black: number;
  white: number;
  stretch: Stretch;
  k: number;
  invert: boolean;
  curve: { x: number; y: number }[];
  detail: { amount: number; radius: number };
  clip: LayerClip | null;
  intensity: number;
  toneMatch?: { gain: number; offset: number };
  opacity: number;
  blend: Blend;
  mask: { source: string | null; black: number; white: number; invert: boolean };
  transform: { x: number; y: number; scale: number; rotation: number };
  edge: { feather: number; irregularity: number };
  brush: { strokes: Stroke[] };
}
export interface Crop { x: number; y: number; w: number; h: number; rotation?: number }
/** Fixed canvas-space coverage; feather is a fraction of the region's short side. */
export interface LayerClip extends Crop { feather: number; shape: 'rect' | 'ellipse' }
export interface EditorSnapshot {
  version: number;
  layers: Layer[];
  selectedLayer?: string;
  global: {
    crop: Crop;
    rotation: number;
    flipX: boolean;
    output: { exposure: number; highlights: number; black?: number; white?: number; midtones?: number };
  };
  /** Built sources these edits have already seen; only newly built sources get default layers. */
  sources?: string[];
}
export interface Checkpoint {
  id: string; name: string; created: string; state: EditorSnapshot;
  exportCheckpoint?: string;
}
export interface EditorState extends EditorSnapshot { checkpoints?: Checkpoint[] }

export const clamp = (v: number, min = 0, max = 1) => Math.min(max, Math.max(min, v));
export const defaultOutput = { exposure: 0, highlights: 0, black: 0, white: 1, midtones: 1 };
export const defaultGradient: GradientStop[] = [
  { t: 0, color: '#080c16' }, { t: .45, color: '#426f9d' },
  { t: .8, color: '#d2b17c' }, { t: 1, color: '#fff0d5' },
];
export const defaultSpectral: SpectralStop[] = [
  { nm: 200, color: '#436bdb' }, { nm: 550, color: '#5bb8ba' },
  { nm: 2500, color: '#dc8054' }, { nm: 25000, color: '#b73335' },
];
export const rampPresets: GradientStop[][] = [
  defaultGradient,
  [{ t: 0, color: '#060b14' }, { t: .4, color: '#233a81' }, { t: .7, color: '#e36b42' }, { t: 1, color: '#fff2ca' }],
  [{ t: 0, color: '#071415' }, { t: .5, color: '#397b81' }, { t: 1, color: '#ebdfc9' }],
];
export function makeLayer(source?: SourceLayer, color = '#dcd8cf'): Layer {
  return {
    id: crypto.randomUUID(), name: source?.name || source?.filter || 'Luminance', visible: true,
    source: source?.key || 'lum', mode: 'tint', color,
    gradient: structuredClone(defaultGradient), spectral: { stops: structuredClone(defaultSpectral) },
    black: 0, white: 1, stretch: 'asinh', k: 10, invert: false, curve: [], intensity: 1, opacity: 1, blend: 'screen',
    detail: { amount: 0, radius: 8 },
    clip: null,
    mask: { source: null, black: 0, white: 1, invert: false },
    transform: { x: 0, y: 0, scale: 1, rotation: 0 },
    edge: { feather: 0, irregularity: 0 },
    brush: { strokes: [] },
  };
}
export function mixColor(a: string, b: string, t: number) {
  return '#' + [1, 3, 5].map(i =>
    Math.round(parseInt(a.slice(i, i + 2), 16) * (1 - t) + parseInt(b.slice(i, i + 2), 16) * t)
      .toString(16).padStart(2, '0')).join('');
}
export function defaultState(meta: ProjectMeta): EditorState {
  const sorted = [...meta.layers].sort((a, b) => (a.wavelength_nm || 550) - (b.wavelength_nm || 550));
  const colors = sorted.map((_, i) => {
    if (sorted.length === 1) return '#dcd8cf';
    const t = i / (sorted.length - 1);
    return t < .5 ? mixColor('#4e77ef', '#64c9aa', t * 2) : mixColor('#64c9aa', '#f18b56', (t - .5) * 2);
  });
  return {
    version: 1, layers: sorted.map((source, i) => makeLayer(source, colors[i])).reverse(),
    global: {
      crop: { x: 0, y: 0, w: 1, h: 1 }, rotation: 0, flipX: false,
      output: { ...defaultOutput },
    },
    sources: meta.layers.map(l => l.key),
  };
}
/** Discard abandoned star-only state, including saved checkpoints, without requiring star assets. */
function removeStarState(value: Partial<EditorState>) {
  const legacy = value as Record<string, unknown>;
  for (const key of ['starClasses', 'starOverrides', 'starWeights', 'starsMigration']) delete legacy[key];
  if (value.global) {
    const global = value.global as unknown as Record<string, unknown>;
    delete global.stars; delete global.parallax;
  }
  for (const point of value.checkpoints ?? []) removeStarState(point.state);
}
export function reconcileState(meta: ProjectMeta, input: unknown): EditorState {
  const defaults = defaultState(meta);
  if (!input || typeof input !== 'object') return defaults;
  const edits = structuredClone(input) as Partial<EditorState>;
  if (!Array.isArray(edits.layers) || !edits.global) return defaults;
  removeStarState(edits);
  const keys = new Set(meta.layers.map(l => l.key));
  const aliases = new Map(meta.layers.flatMap(layer => (layer.tiles ?? [])
    .map(tile => [tile.key, layer.key] as const)));
  const regrouped = new Set<string>();
  edits.layers = edits.layers.filter(layer => {
    const target = aliases.get(layer.source);
    if (!target || target === layer.source) return true;
    if (regrouped.has(target)) return false;
    regrouped.add(target);
    layer.source = target;
    return true;
  });
  for (const layer of edits.layers) {
    if (layer.mask?.source) layer.mask.source = aliases.get(layer.mask.source) ?? layer.mask.source;
  }
  if (edits.sources) edits.sources = [...new Set(edits.sources.map(key => aliases.get(key) ?? key))];
  if (edits.selectedLayer && !edits.layers.some(layer => layer.id === edits.selectedLayer))
    edits.selectedLayer = edits.layers[0]?.id;
  const existing = edits.layers.filter(l => l.source === 'lum' || keys.has(l.source));
  const used = new Set(existing.map(l => l.source)), seen = new Set(edits.sources ?? []);
  return {
    ...defaults, ...structuredClone(edits),
    sources: defaults.sources,
    layers: [
      ...defaults.layers.filter(l => !used.has(l.source) && !seen.has(l.source)),
      ...existing.map(l => {
        const base = makeLayer();
        return {
          ...base, ...structuredClone(l), mask: { ...base.mask, ...l.mask },
          transform: { ...base.transform, ...l.transform }, spectral: { ...base.spectral, ...l.spectral },
          edge: { ...base.edge, ...l.edge },
          detail: { ...base.detail, ...l.detail },
          brush: { ...base.brush, ...l.brush },
        };
      }),
    ],
    global: {
      ...defaults.global, ...edits.global,
      crop: anchorCrop({ ...defaults.global.crop, ...edits.global.crop }, edits.global.rotation ?? 0),
      output: { ...defaults.global.output, ...edits.global.output },
    },
  };
}
/** Pin crops saved before crop.rotation existed at their current angle: same picture now, whole-crop rotation afterwards. */
export function anchorCrop(crop: Crop, rotation: number): Crop {
  const full = crop.x === 0 && crop.y === 0 && crop.w === 1 && crop.h === 1;
  return full || crop.rotation !== undefined ? crop : { ...crop, rotation };
}
export function snapshot(state: EditorState): EditorSnapshot {
  const { checkpoints: _checkpoints, ...rest } = state;
  return structuredClone(rest);
}
export type RelativeKey = 'intensity' | 'white' | 'black' | 'k' | 'opacity';
export const relativeAdjustments: { key: RelativeKey; label: string; min: number; max: number; unit: string }[] = [
  { key: 'intensity', label: 'Intensity', min: -100, max: 100, unit: '%' },
  { key: 'white', label: 'White point', min: -90, max: 200, unit: '%' },
  { key: 'black', label: 'Black point', min: -.5, max: .5, unit: '' },
  { key: 'k', label: 'Strength', min: -90, max: 200, unit: '%' },
  { key: 'opacity', label: 'Opacity', min: -100, max: 100, unit: '%' },
];
export function adjustRelative(layer: Layer, key: RelativeKey, value: number): Layer {
  const next = structuredClone(layer);
  const bounds: Record<RelativeKey, [number, number]> = {
    intensity: [0, 2], white: [.01, 3], black: [0, Math.max(0, layer.white - .01)],
    k: [.1, 30], opacity: [0, 1],
  };
  next[key] = clamp(key === 'black' ? layer.black + value : layer[key] * (1 + value / 100), ...bounds[key]);
  return next;
}
