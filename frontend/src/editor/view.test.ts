import { describe, expect, it } from 'vitest';
import cases from '../../../tests/view_cases.json';
import { applyViewCrop, cropRotation, imageToScreen, insideCrop, outputToImage, screenToOutput, viewFrame } from './view';

const viewport = { width: 800, height: 500, zoom: 1.7, x: 17, y: -23 };
describe('anchored crop geometry (shared CPU parity vectors)', () => {
  for (const test of cases) it.each([false, true])(`round-trips delta=${test.g.rotation - 23}, flip=%s`, flipX => {
    const { size, probes } = test, g = { ...test.g, flipX }, b = viewFrame(size, g);
    for (const key of ['x', 'y', 'w', 'h', 'width', 'height'] as const)
      expect(b[key]).toBeCloseTo(test.frame[key], 10);
    for (const probe of probes) {
      const source = outputToImage(size, g, probe.output);
      expect(source.x).toBeCloseTo(flipX ? 1 - probe.image.x : probe.image.x, 10);
      expect(source.y).toBeCloseTo(probe.image.y, 10);
      expect(insideCrop(size, g, probe.output)).toBe(probe.inside);
      const screen = imageToScreen(size, g, viewport, source);
      const output = screenToOutput(size, g, viewport, screen);
      expect(output.x).toBeCloseTo(probe.output.x, 10);
      expect(output.y).toBeCloseTo(probe.output.y, 10);
    }
    // Rotation about the crop's centre must preserve the centre's source sample.
    const original = { ...g, rotation: 23 };
    const before = outputToImage(size, original, { x: .4, y: .45 });
    const after = outputToImage(size, g, { x: b.x + b.w / 2, y: b.y + b.h / 2 });
    expect(after.x).toBeCloseTo(before.x, 10); expect(after.y).toBeCloseTo(before.y, 10);
  });

  it('swaps pixel dimensions exactly at quarter turns, with no empty corners', () => {
    const { size, g } = cases[0];
    for (const delta of [90, -90, 270, -270]) {
      const current = { ...g, rotation: 23 + delta }, b = viewFrame(size, current);
      expect([b.width, b.height]).toEqual([240, 500]);
      for (const x of [b.x, b.x + b.w]) for (const y of [b.y, b.y + b.h])
        expect(insideCrop(size, current, { x, y })).toBe(true);
    }
  });

  it('keeps missing anchors and full-frame crops exactly on the legacy formulas', () => {
    const { size, g } = cases[4], crop = { x: .15, y: .25, w: .5, h: .4 };
    const legacy = { ...g, crop }, b = viewFrame(size, legacy);
    expect(b).toEqual({ ...crop, width: 500, height: 240 });
    const p = screenToOutput(size, legacy, viewport, { x: 123, y: 321 });
    const fit = Math.min(800 / 500, 500 / 240) * viewport.zoom;
    expect(p).toEqual({ x: crop.x + ((123 - 400 - 17) / (500 * fit) + .5) * crop.w,
      y: crop.y + ((321 - 250 + 23) / (240 * fit) + .5) * crop.h });
    const full = { ...g, crop: { x: 0, y: 0, w: 1, h: 1, rotation: 12 } };
    expect(cropRotation(full)).toBe(full.rotation);
    expect(viewFrame(size, full)).toMatchObject({ width: 1000, height: 600 });
    expect(applyViewCrop(full.crop, 37)).not.toHaveProperty('rotation');
  });

  it('recrops in current theta-space without moving content, even outside the original 0–1 frame', () => {
    const { size, g } = cases[1], b = viewFrame(size, g);
    const rectangle = { x: b.x + b.w * .1, y: b.y + b.h * .05, w: b.w * .5, h: b.h * .7 };
    expect(rectangle.y).toBeLessThan(0);
    const next = { ...g, crop: applyViewCrop(rectangle, g.rotation) };
    expect(cropRotation(next)).toBe(next.rotation);
    expect(viewFrame(size, next)).toMatchObject(rectangle);
    for (const u of [0, .5, 1]) for (const v of [0, .5, 1]) {
      const p = { x: rectangle.x + u * rectangle.w, y: rectangle.y + v * rectangle.h };
      expect(outputToImage(size, next, p)).toEqual(outputToImage(size, g, p));
    }
  });

  it('recrops the interior at an arbitrary delta without changing sampled content', () => {
    const { size, g } = cases[4], b = viewFrame(size, g);
    const rectangle = { x: b.x + b.w * .4, y: b.y + b.h * .4, w: b.w * .2, h: b.h * .2 };
    const next = { ...g, crop: applyViewCrop(rectangle, g.rotation) };
    expect(cropRotation(next)).toBe(g.rotation);
    for (const u of [0, .5, 1]) for (const v of [0, .5, 1]) {
      const p = { x: rectangle.x + u * rectangle.w, y: rectangle.y + v * rectangle.h };
      expect(insideCrop(size, g, p)).toBe(true);
      expect(outputToImage(size, next, p)).toEqual(outputToImage(size, g, p));
    }
  });

  it('treats a wrapped full turn as the same crop instead of growing the bounds', () => {
    const { size, g } = cases[0], wrapped = { ...g, rotation: g.rotation - 360 };
    const b = viewFrame(size, wrapped);
    expect([b.width, b.height]).toEqual([500, 240]);
    expect(b.x).toBeCloseTo(g.crop.x, 10); expect(b.y).toBeCloseTo(g.crop.y, 10);
  });
});
