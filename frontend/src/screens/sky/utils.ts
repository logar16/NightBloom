import type { CSSProperties } from 'react';
import type { Observation, Session } from '../../api/types';
import { projectUrl } from '../../api/client';

export function previewUrl(observation: Observation, size: 'thumb' | 'large', view: Session['view'], revision = 0) {
  const frame = encodeURIComponent(JSON.stringify([
    view.captured_at_utc, view.center_icrs_deg, view.field_arcmin, view.pixel_dimensions, view.rotation_deg ?? 0,
  ]));
  const version = observation.collection === 'Survey'
    ? `hips-fits-v3-${frame}-${revision}${observation.instrument === 'DSS2' ? '-overlap-v2' : ''}` : revision || '';
  return projectUrl(`/api/preview/${encodeURIComponent(observation.obs_id)}?size=${size}${version ? `&v=${version}` : ''}`);
}

export const logPosition = (nm: number, maxNm = 25000) => Math.min(1, Math.max(0, Math.log(nm / 200) / Math.log(maxNm / 200)));
export const wavelengthAt = (t: number, maxNm = 25000) => 200 * (maxNm / 200) ** t;
export const waveLabel = (nm: number) => nm >= 1000
  ? `${(nm / 1000).toFixed(nm >= 10000 ? 1 : 2)} µm` : `${Math.round(nm)} nm`;
export function wavelengthColor(nm: number | null | undefined) {
  const wavelength = nm || 550;
  const stops = [[200, 121, 99, 170], [400, 107, 118, 212], [490, 91, 162, 199],
    [550, 125, 178, 124], [640, 213, 171, 104], [750, 195, 111, 86],
    [2500, 142, 68, 59], [25000, 102, 67, 51]];
  const index = stops.findIndex((s) => s[0] >= wavelength);
  const b = stops[index < 0 ? stops.length - 1 : index], a = stops[Math.max(0, index - 1)];
  const f = b[0] === a[0] ? 0 : Math.min(1, Math.max(0,
    (Math.log(wavelength) - Math.log(a[0])) / (Math.log(b[0]) - Math.log(a[0]))));
  return '#' + [1, 2, 3].map((i) => Math.round(a[i] * (1 - f) + b[i] * f)
    .toString(16).padStart(2, '0')).join('');
}
export const waveStyle = (nm: number | null | undefined) => ({ '--wave': wavelengthColor(nm) }) as CSSProperties;
export function insidePolygon(x: number, y: number, polygon: number[][]) {
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const [xi, yi] = polygon[i], [xj, yj] = polygon[j];
    if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}
export function bytes(value: number | null | undefined) {
  if (value == null) return 'Size pending';
  if (value >= 1e9) return `${(value / 1e9).toFixed(2)} GB`;
  return `${(value / 1e6).toFixed(1)} MB`;
}
export const eta = (seconds: number) => seconds >= 60
  ? `~${Math.max(1, Math.round(seconds / 60))} min` : `~${Math.max(1, Math.round(seconds))} sec`;
export const progressMessage = (text: string) => text
  .replace(/([\d,]+)\s*\/\s*([\d,]+)\s*bytes/gi, (_, a: string, b: string) =>
    `${bytes(Number(a.replaceAll(',', '')))} / ${bytes(Number(b.replaceAll(',', '')))}`)
  .replace(/([\d,]+)\s*bytes/gi, (_, value: string) => bytes(Number(value.replaceAll(',', ''))));
