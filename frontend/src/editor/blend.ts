import { clamp, type Blend, type Layer } from './state';
import { gradientBytes, logPosition, rgb } from './compositor';

/** CPU equivalent of the layer shader, before framebuffer/output clipping. */
export function blendPixel(mode: Blend, a: number[], b: number[], alpha = 1, hue = b, bright = 1): number[] {
  if (mode === 'normal') { b = hue; alpha *= bright; }
  return a.map((v, i) => {
    const w = b[i];
    const result = mode === 'add' ? v + w
      : mode === 'screen' ? 1 - (1 - v) * (1 - w)
      : mode === 'multiply' ? v * w
      : mode === 'overlay' ? v >= .5 ? 1 - 2 * (1 - v) * (1 - w) : 2 * v * w
      : mode === 'softlight' ? w >= .5 ? v + (2 * w - 1) * (Math.sqrt(Math.max(v, 0)) - v) : v - (1 - 2 * w) * v * (1 - v)
      : mode === 'lighten' ? Math.max(v, w)
      : mode === 'difference' ? Math.abs(v - w)
      : mode === 'subtract' ? Math.max(v - w, 0)
      : mode === 'darken' ? Math.min(v, w) : w;
    return v + (result - v) * alpha;
  });
}

export function blendPair(layers: Layer[], selected: string[]): [Layer | undefined, Layer | undefined] {
  const picked = layers.filter(l => selected.includes(l.id));
  const other = layers.filter(l => !selected.includes(l.id));
  if (picked.length > 1 && other.length === 1) {
    const baseIndex = layers.indexOf(other[0]);
    const top = [...picked].sort((a, b) => Math.abs(layers.indexOf(a) - baseIndex) - Math.abs(layers.indexOf(b) - baseIndex))[0];
    return [top, other[0]];
  }
  const top = picked[0];
  const below = layers.slice(layers.indexOf(top) + 1);
  return [top, picked.length > 1
    ? below.find(l => l.visible && selected.includes(l.id)) ?? below.find(l => l.visible)
    : below.find(l => l.visible)];
}

export function similarLayerHues(top?: Layer, base?: Layer) {
  const angle = (layer?: Layer) => {
    const color = layerRamp(layer, false)(1).hue;
    const max = Math.max(...color), min = Math.min(...color), delta = max - min;
    if (delta < .0001) return null;
    const [r, g, b] = color;
    return ((max === r ? (g - b) / delta : max === g ? 2 + (b - r) / delta : 4 + (r - g) / delta) * 60 + 360) % 360;
  };
  const a = angle(top), b = angle(base);
  return a !== null && b !== null && Math.min(Math.abs(a - b), 360 - Math.abs(a - b)) < 30;
}

export function layerRamp(layer?: Pick<Layer, 'mode' | 'color' | 'gradient' | 'spectral' | 'intensity'>, useIntensity = true) {
  const tint = rgb(layer?.color ?? '#808080');
  const lut = layer && layer.mode !== 'tint' ? gradientBytes(layer.mode === 'gradient' ? layer.gradient
    : layer.spectral.stops.map(s => ({ t: logPosition(s.nm), color: s.color }))) : undefined;
  return (position: number) => {
    const intensity = useIntensity ? layer?.intensity ?? 1 : 1;
    const bright = clamp(position * intensity);
    // Match normalized, linearly filtered sampling of the shader's 256-pixel colour map.
    const t = layer?.mode === 'gradient' ? bright : position;
    const x = clamp(t * 256 - .5, 0, 255), lo = Math.floor(x), hi = Math.min(255, lo + 1);
    const rawHue = lut ? [0, 1, 2].map(i => (lut[lo * 4 + i] * (1 - x + lo) + lut[hi * 4 + i] * (x - lo)) / 255) : tint;
    const peak = Math.max(...rawHue);
    const hue = useIntensity || peak === 0 ? rawHue : rawHue.map(c => c / peak);
    const color = hue.map(c => c * (layer?.mode === 'gradient' ? intensity : bright));
    return { color, hue, bright };
  };
}
