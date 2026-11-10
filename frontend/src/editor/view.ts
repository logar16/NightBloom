import type { Crop } from './state';

type Size = { width: number; height: number };
type Geometry = { crop: Crop; rotation: number; flipX?: boolean };
type Point = { x: number; y: number };
export type Viewport = Size & { zoom: number; x: number; y: number };
export const fullCrop = (c: Crop) => c.x === 0 && c.y === 0 && c.w === 1 && c.h === 1;
export const cropRotation = (g: Geometry) => fullCrop(g.crop) ? g.rotation : g.crop.rotation ?? g.rotation;

function rotate(x: number, y: number, angle: number): Point {
  const r = angle * Math.PI / 180;
  const c = Math.cos(r), s = Math.sin(r);
  return { x: c * x - s * y, y: s * x + c * y };
}

/** Axis-aligned bounds in current-rotation output space (not source/image space).
 * Source -> image-relative flip -> anchor rotation -> crop -> delta rotation.
 * The last rotation is about the crop centre; the bounds' translation cancels it.
 */
export function viewFrame(size: Size, g: Geometry): Crop & Size {
  const c = g.crop, delta = g.rotation - cropRotation(g);
  if (delta === 0) return { ...c, width: size.width * c.w, height: size.height * c.h };
  const r = delta * Math.PI / 180;
  // Snap cardinal angles so an exact quarter-turn cannot grow by a rounding pixel.
  const cos = Math.abs(Math.cos(r)) < 1e-12 ? 0 : Math.abs(Math.cos(r));
  const sin = Math.abs(Math.sin(r)) < 1e-12 ? 0 : Math.abs(Math.sin(r));
  const width = size.width * c.w * cos + size.height * c.h * sin;
  const height = size.width * c.w * sin + size.height * c.h * cos;
  const center = rotate((c.x + c.w / 2 - .5) * size.width, (c.y + c.h / 2 - .5) * size.height, delta);
  return { x: .5 + (center.x - width / 2) / size.width, y: .5 + (center.y - height / 2) / size.height,
    w: width / size.width, h: height / size.height, width, height };
}

export function insideCrop(size: Size, g: Geometry, p: Point): boolean {
  const delta = g.rotation - cropRotation(g);
  if (!delta) return true;
  const q = rotate((p.x - .5) * size.width, (p.y - .5) * size.height, -delta), c = g.crop;
  const x = q.x / size.width + .5, y = q.y / size.height + .5;
  return x >= c.x - 1e-12 && x <= c.x + c.w + 1e-12 && y >= c.y - 1e-12 && y <= c.y + c.h + 1e-12;
}

export function outputToImage(size: Size, g: Geometry, p: Point): Point {
  const q = rotate((p.x - .5) * size.width, (p.y - .5) * size.height, -g.rotation);
  return { x: (g.flipX ? -q.x : q.x) / size.width + .5, y: q.y / size.height + .5 };
}

export function screenToOutput(size: Size, g: Geometry, v: Viewport, p: Point): Point {
  const b = viewFrame(size, g), fit = Math.min(v.width / b.width, v.height / b.height) * v.zoom;
  return { x: b.x + ((p.x - v.width / 2 - v.x) / (b.width * fit) + .5) * b.w,
    y: b.y + ((p.y - v.height / 2 - v.y) / (b.height * fit) + .5) * b.h };
}

export function imageToScreen(size: Size, g: Geometry, v: Viewport, p: Point): Point {
  const b = viewFrame(size, g), fit = Math.min(v.width / b.width, v.height / b.height) * v.zoom;
  const q = rotate((p.x - .5) * size.width * (g.flipX ? -1 : 1), (p.y - .5) * size.height, g.rotation);
  return { x: (q.x + (.5 - b.x - b.w / 2) * size.width) * fit + v.width / 2 + v.x,
    y: (q.y + (.5 - b.y - b.h / 2) * size.height) * fit + v.height / 2 + v.y };
}

/** A new rectangle is already in current theta-space; never rotate it again. */
export function applyViewCrop(crop: Crop, rotation: number): Crop {
  return fullCrop(crop) ? { x: 0, y: 0, w: 1, h: 1 } : { ...crop, rotation };
}
