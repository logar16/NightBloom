import { expect, it } from 'vitest';
import { previewExposure, previewPlacement } from './preview';

it('exposes the 99.5th percentile without letting isolated stars set the gain', () => {
  const rgba = new Float32Array(1000 * 4);
  for (let i = 0; i < 1000; i++) rgba.set([.1, .2, .05, 1], i * 4);
  for (let i = 0; i < 4; i++) rgba[i * 4] = 5;
  expect(previewExposure(rgba)).toBeCloseTo(.95 / .2);
  expect(previewExposure(new Uint8Array([10, 20, 5, 255]))).toBeCloseTo(.95 / (20 / 255));
});
it('caps amplification of noise, leaves empty layers dark, and can reduce overbright previews', () => {
  expect(previewExposure(new Float32Array([.0001, .0002, 0, 1]))).toBe(25);
  expect(previewExposure(new Float32Array([0, 0, 0, 1]))).toBe(1);
  expect(previewExposure(new Float32Array([1, 2, .5, 1]))).toBeCloseTo(.475);
});

it('clamps extreme aspects with padding and preserves ordinary aspects', () => {
  const viewport = { width: 1440, height: 860 }, anchor = { left: 70, right: 220, top: 150 };
  const portrait = previewPlacement(100, 1000, viewport, anchor);
  const landscape = previewPlacement(1000, 100, viewport, anchor);
  for (const preview of [portrait, landscape]) {
    expect(Math.min(preview.width, preview.height)).toBeGreaterThanOrEqual(160);
    expect(Math.max(preview.width, preview.height)).toBeLessThanOrEqual(500);
  }
  expect(portrait.height).toBeGreaterThan(portrait.width);
  expect(landscape.width).toBeGreaterThan(landscape.height);
  const regular = previewPlacement(400, 500, viewport, anchor);
  expect(regular.width / regular.height).toBeCloseTo(400 / 500);
  expect(Math.max(regular.width, regular.height)).toBeLessThanOrEqual(500);
});
it('places beside the panel, flips near the right edge, and stays onscreen in small viewports', () => {
  const normal = previewPlacement(1, 1, { width: 1440, height: 860 }, { left: 70, right: 220, top: 700 });
  expect(normal.left).toBeGreaterThanOrEqual(220);
  expect(normal.top).toBeGreaterThanOrEqual(8);
  expect(normal.left + normal.panelWidth).toBeLessThanOrEqual(1440 - 8);
  expect(normal.top + normal.panelHeight).toBeLessThanOrEqual(860 - 8);
  for (const viewport of [{ width: 720, height: 600 }, { width: 320, height: 480 }]) {
    const p = previewPlacement(3, 1, viewport, { left: viewport.width - 100, right: viewport.width - 20, top: viewport.height - 10 });
    expect(p.left).toBeGreaterThanOrEqual(8); expect(p.top).toBeGreaterThanOrEqual(8);
    expect(p.left + p.panelWidth).toBeLessThanOrEqual(viewport.width - 8);
    expect(p.top + p.panelHeight).toBeLessThanOrEqual(viewport.height - 8);
  }
});
