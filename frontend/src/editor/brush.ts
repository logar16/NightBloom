import { clamp, type Stroke } from './state';
import { perfStart } from './perf';

export type Point = [number, number];
export const invertBrushMode = (mode: Stroke['mode']): Stroke['mode'] =>
  ({ hide: 'reveal', reveal: 'hide', black: 'white', white: 'black' } as const)[mode];
export function strokeDabs(stroke: Stroke): Point[] {
  if (!stroke.points.length) return [];
  const dabs: Point[] = [stroke.points[0]], spacing = Math.max(1, stroke.size * .15);
  for (let i = 1; i < stroke.points.length; i++) {
    const a = stroke.points[i - 1], b = stroke.points[i];
    const steps = Math.max(1, Math.ceil(Math.hypot(b[0] - a[0], b[1] - a[1]) / spacing));
    for (let j = 1; j <= steps; j++) dabs.push([a[0] + (b[0] - a[0]) * j / steps, a[1] + (b[1] - a[1]) * j / steps]);
  }
  return dabs;
}
export function brushFalloff(r: number, hardness: number) {
  if (r <= hardness) return 1;
  if (r >= 1) return 0;
  const t = (r - hardness) / (1 - hardness);
  return 1 - t * t * (3 - 2 * t);
}

/** RGBA stores visibility, signal multiplier, signal addition and an unused channel. */
export class BrushMask {
  readonly width: number;
  readonly height: number;
  readonly data: Float32Array;
  private base: Float32Array;
  private footprint: Float32Array;
  private previous: readonly Stroke[] | null = null;
  private preview: Stroke | undefined;
  private previewPoints = 0;
  constructor(readonly imageWidth: number, readonly imageHeight: number) {
    const scale = Math.max(1, Math.ceil(Math.max(imageWidth, imageHeight) / 1024));
    this.width = Math.ceil(imageWidth / scale); this.height = Math.ceil(imageHeight / scale);
    this.data = new Float32Array(this.width * this.height * 4);
    this.reset();
    this.base = this.data.slice(); this.footprint = new Float32Array(this.width * this.height);
  }
  private reset() {
    this.data.fill(0);
    for (let i = 0; i < this.data.length; i += 4) {
      this.data[i] = 1; this.data[i + 1] = 1;
    }
  }
  /** Committed strokes are immutable; the live preview only appends points. */
  update(strokes: readonly Stroke[], preview?: Stroke) {
    const end = perfStart('brush');
    try {
      const count = preview?.points.length ?? 0;
      if (strokes === this.previous && preview === this.preview && count === this.previewPoints) return false;
      const extending = !!preview && strokes === this.previous && preview === this.preview && count >= this.previewPoints;
      if (extending) {
        const tail = { ...preview, points: preview.points.slice(Math.max(0, this.previewPoints - 1)) };
        this.dab(tail);
        this.apply(preview);
      } else {
        this.reset();
        for (const stroke of preview ? [...strokes, preview] : strokes) {
          this.base.set(this.data); this.footprint.fill(0);
          this.dab(stroke); this.apply(stroke);
        }
      }
      this.previous = strokes; this.preview = preview; this.previewPoints = count;
      return true;
    } finally { end(); }
  }
  private dab(stroke: Stroke) {
    const radius = Math.max(.5, stroke.size / 2), sx = this.imageWidth / this.width, sy = this.imageHeight / this.height;
    for (const [x, y] of strokeDabs(stroke)) {
      const x0 = Math.max(0, Math.ceil((x - radius) / sx - .5)), x1 = Math.min(this.width - 1, Math.floor((x + radius) / sx - .5));
      const y0 = Math.max(0, Math.ceil((y - radius) / sy - .5)), y1 = Math.min(this.height - 1, Math.floor((y + radius) / sy - .5));
      for (let j = y0; j <= y1; j++) for (let i = x0; i <= x1; i++) {
        const f = clamp(stroke.strength) * brushFalloff(Math.hypot((i + .5) * sx - x, (j + .5) * sy - y) / radius, clamp(stroke.hardness));
        const index = j * this.width + i;
        this.footprint[index] = Math.max(this.footprint[index], f);
      }
    }
  }
  private apply(stroke: Stroke) {
    for (let pixel = 0; pixel < this.footprint.length; pixel++) {
      const i = pixel * 4, s = this.footprint[pixel];
      if (stroke.mode === 'hide' || stroke.mode === 'reveal') {
        const m = this.base[i];
        this.data[i] = clamp(stroke.mode === 'hide' ? m * (1 - s) : m + (1 - m) * s);
      } else if (stroke.mode === 'black' || stroke.mode === 'white') {
        this.data[i + 1] = this.base[i + 1] * (1 - s);
        this.data[i + 2] = this.base[i + 2] * (1 - s) + (stroke.mode === 'white' ? s : 0);
      } else {
        throw Error(`Unsupported brush stroke mode: ${stroke.mode}`);
      }
    }
  }
}
