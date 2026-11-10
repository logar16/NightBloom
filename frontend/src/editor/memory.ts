export const PREVIEW_BUDGET = 1.5 * 1024 ** 3;

/** Also reserves three bounded R32F detail caches and one separable-blur scratch target. */
export function textureBytes(width: number, height: number, sources: number) {
  return width * height * (8 * (sources + 1) + 64);
}

export function previewPlan(width: number, height: number, sources: number, maxLayers: number,
  maxSize: number, budget = PREVIEW_BUDGET) {
  // Reducing spatial resolution cannot reduce array depth.
  if (!sources || sources + 1 > maxLayers) throw Error('Project exceeds this GPU’s source-layer limit. Reduce the number of source images.');
  const fits = (w: number, h: number) => w <= maxSize && h <= maxSize && textureBytes(w, h, sources) <= budget;
  if (fits(width, height)) return { width, height, scale: 1 };
  const w = Math.ceil(width / 2), h = Math.ceil(height / 2);
  if (!fits(w, h)) throw Error('Project exceeds this GPU’s memory or texture-size budget even at half resolution. Reduce the canvas or source count.');
  return { width: w, height: h, scale: 2 };
}

/** Keep raw half-float bits (and discrete labels) intact while reducing preview storage. */
export function previewPixels(data: Uint16Array, width: number, height: number, scale: number, channels = 1) {
  if (data.length !== width * height * channels) throw Error('Invalid preview asset dimensions.');
  if (scale === 1) return data;
  const w = Math.ceil(width / scale), h = Math.ceil(height / scale), output = new Uint16Array(w * h * channels);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const source = (Math.min(height - 1, y * scale + 1) * width + Math.min(width - 1, x * scale + 1)) * channels;
    for (let c = 0; c < channels; c++) output[(y * w + x) * channels + c] = data[source + c];
  }
  return output;
}
