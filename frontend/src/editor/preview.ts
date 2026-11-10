import { clamp } from './state';

export function previewExposure(rgba: Float32Array | Uint8Array) {
  const maxima = new Float32Array(rgba.length / 4), scale = rgba instanceof Uint8Array ? 1 / 255 : 1;
  for (let i = 0; i < maxima.length; i++) maxima[i] = Math.max(rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2]) * scale;
  maxima.sort();
  const percentile = maxima[Math.max(0, Math.ceil(maxima.length * .995) - 1)] || 0;
  return percentile > 0 ? Math.min(25, .95 / percentile) : 1;
}

export function previewPlacement(imageWidth: number, imageHeight: number,
  viewport: { width: number; height: number }, anchor: { left: number; right: number; top: number }) {
  const aspect = clamp(imageWidth / Math.max(1, imageHeight), 1 / 3, 3);
  const side = Math.max(1, Math.min(500, viewport.width - 38, viewport.height - 152));
  const width = Math.round(side * Math.min(1, aspect)), height = Math.round(side / Math.max(1, aspect));
  const panelWidth = width + 22, panelHeight = height + 136;
  const beside = anchor.right + 12 + panelWidth <= viewport.width - 8
    ? anchor.right + 12 : anchor.left - panelWidth - 12;
  return { width, height, panelWidth, panelHeight,
    left: clamp(beside, 8, Math.max(8, viewport.width - panelWidth - 8)),
    top: clamp(anchor.top, 8, Math.max(8, viewport.height - panelHeight - 8)) };
}
