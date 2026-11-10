import { expect, it } from 'vitest';
import { PREVIEW_BUDGET, previewPixels, previewPlan, textureBytes } from './memory';

it('retains the fixture resolution and quarters oversized texture storage', () => {
  expect(previewPlan(3072, 3072, 12, 256, 16384).scale).toBe(1);
  const plan = previewPlan(4096, 4096, 12, 256, 16384);
  expect(plan).toEqual({ width: 2048, height: 2048, scale: 2 });
  expect(textureBytes(plan.width, plan.height, 12)).toBeLessThan(PREVIEW_BUDGET);
  expect(textureBytes(4096, 4096, 12) / textureBytes(plan.width, plan.height, 12)).toBe(4);
});
it('checks array depth independently of downsampling and rejects unsafe previews', () => {
  expect(() => previewPlan(3072, 3072, 12, 12, 4096)).toThrow('source-layer limit');
  expect(previewPlan(3072, 3072, 12, 256, 2048).scale).toBe(2);
  expect(() => previewPlan(8192, 8192, 12, 256, 1024)).toThrow('even at half resolution');
});
it('downsamples odd-sized multichannel assets without mixing label or float bits', () => {
  const pixels = Uint16Array.from({ length: 3 * 3 * 3 }, (_, i) => i);
  expect([...previewPixels(pixels, 3, 3, 2, 3)]).toEqual([12, 13, 14, 15, 16, 17, 21, 22, 23, 24, 25, 26]);
  expect(previewPixels(pixels, 3, 3, 1, 3)).toBe(pixels);
});
