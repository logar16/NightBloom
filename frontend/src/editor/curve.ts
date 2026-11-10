import { clamp } from './state';

export interface CurvePoint { x: number; y: number }
export const CURVE_SIZE = 256;
export const curveEnabled = (points: CurvePoint[] = []) => curvePoints(points).some(p => p.x !== p.y);

export function curvePoints(points: CurvePoint[] = []): CurvePoint[] {
  const sorted = points.filter(p => Number.isFinite(p.x) && Number.isFinite(p.y))
    .map(p => ({ x: clamp(p.x), y: clamp(p.y) })).sort((a, b) => a.x - b.x);
  const unique = sorted.filter((p, i) => !i || p.x - sorted[i - 1].x > 1e-6);
  if (unique[0]?.x !== 0) unique.unshift({ x: 0, y: 0 });
  if (unique.at(-1)?.x !== 1) unique.push({ x: 1, y: 1 });
  return unique;
}

/** Fritsch–Carlson tangents prevent overshoot, including across flat or reversing segments. */
export function curveLut(input: CurvePoint[] = []): Float32Array {
  const p = curvePoints(input), n = p.length;
  const d = p.slice(1).map((b, i) => (b.y - p[i].y) / (b.x - p[i].x));
  const m = p.map((_, i) => i === 0 ? d[0] : i === n - 1 ? d[n - 2] : (d[i - 1] + d[i]) / 2);
  for (let i = 1; i < n - 1; i++) if (d[i - 1] * d[i] <= 0) m[i] = 0;
  for (let i = 0; i < n - 1; i++) {
    if (d[i] === 0) { m[i] = 0; m[i + 1] = 0; continue; }
    const a = m[i] / d[i], b = m[i + 1] / d[i], r = Math.hypot(a, b);
    if (r > 3) { m[i] = 3 * a / r * d[i]; m[i + 1] = 3 * b / r * d[i]; }
  }
  let j = 0;
  return Float32Array.from({ length: CURVE_SIZE }, (_, i) => {
    const x = i / (CURVE_SIZE - 1);
    while (j < n - 2 && x > p[j + 1].x) j++;
    const h = p[j + 1].x - p[j].x, t = (x - p[j].x) / h, t2 = t * t, t3 = t2 * t;
    return clamp((2 * t3 - 3 * t2 + 1) * p[j].y + (t3 - 2 * t2 + t) * h * m[j]
      + (-2 * t3 + 3 * t2) * p[j + 1].y + (t3 - t2) * h * m[j + 1]);
  });
}
