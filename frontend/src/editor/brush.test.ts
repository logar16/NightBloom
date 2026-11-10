import { describe, expect, it } from 'vitest';
import fixtures from '../../../tests/fixtures/brush.json';
import { BrushMask, brushFalloff, strokeDabs } from './brush';
import type { Stroke } from './state';

describe('feather brush', () => {
  for (const fixture of fixtures) it(fixture.name, () => {
    const mask = new BrushMask(fixture.width, fixture.height);
    mask.update(fixture.strokes as Stroke[]);
    for (const [x, y, expected] of fixture.samples) expect(mask.data[(y * mask.width + x) * 4]).toBeCloseTo(expected, 6);
    for (const [x, y, gain, bias] of fixture.signal_samples ?? []) {
      const i = (y * mask.width + x) * 4;
      expect(mask.data[i + 1]).toBeCloseTo(gain, 6);
      expect(mask.data[i + 2]).toBeCloseTo(bias, 6);
    }
  });
  it('bounds the spacing of interpolated dabs, including a short final segment', () => {
    const stroke: Stroke = { mode: 'hide', size: 20, strength: .5, hardness: .2, points: [[1, 2], [11, 2], [11, 2.5]] };
    const dabs = strokeDabs(stroke);
    expect(dabs[0]).toEqual([1, 2]); expect(dabs.at(-1)).toEqual([11, 2.5]);
    for (let i = 1; i < dabs.length; i++) expect(Math.hypot(dabs[i][0] - dabs[i - 1][0], dabs[i][1] - dabs[i - 1][1])).toBeLessThanOrEqual(3);
    expect(brushFalloff(.75, .5)).toBe(.5);
    expect(brushFalloff(1, 1)).toBe(1);
    expect(brushFalloff(1.01, 1)).toBe(0);
  });
  it.each(['reveal', 'white', 'black'] as const)('%s preview matches fresh rasterization and invalidates on undo or clear', mode => {
    const mask = new BrushMask(64, 64), strokes: Stroke[] = [
      { mode: 'hide', size: 14, strength: .7, hardness: .3, points: [[4, 10], [15, 12]] },
      { mode, size: 8, strength: .5, hardness: .5, points: [[5, 10]] },
    ];
    const committed = strokes.slice(0, 1), preview = strokes[1];
    mask.update(committed, preview);
    preview.points.push([19, 14]);
    mask.update(committed, preview);
    const fresh = new BrushMask(64, 64); fresh.update(strokes);
    expect(mask.data).toEqual(fresh.data);
    expect(mask.update(committed, preview)).toBe(false);
    mask.update(committed);
    const undone = new BrushMask(64, 64); undone.update(committed);
    expect(mask.data).toEqual(undone.data);
    mask.update([]);
    expect(mask.data.every((v, i) => v === (i % 4 < 2 ? 1 : 0))).toBe(true);
  });
  it('rebuilds for immutable replacements, undo and a new preview identity', () => {
    const mask = new BrushMask(64, 64), committed: Stroke[] = [
      { mode: 'hide', size: 14, strength: .7, hardness: .3, points: [[4, 10], [15, 12]] },
    ];
    mask.update(committed);
    const replaced = [{ ...committed[0], strength: .2 }];
    mask.update(replaced);
    const fresh = new BrushMask(64, 64); fresh.update(replaced);
    expect(mask.data).toEqual(fresh.data);
    const preview: Stroke = { ...committed[0], mode: 'reveal', points: [[5, 10]] };
    mask.update(replaced, preview);
    const second = { ...preview, points: [[24, 12] as [number, number]] };
    mask.update(replaced, second);
    fresh.update([...replaced, second]);
    expect(mask.data).toEqual(fresh.data);
  });
});
