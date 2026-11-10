import { describe, expect, it } from 'vitest';
import { curveEnabled, curveLut, curvePoints } from './curve';

describe('monotone layer curves', () => {
  it('defaults to identity and supplies missing endpoints', () => {
    expect(curveEnabled([])).toBe(false);
    expect(curveEnabled([{ x: .3, y: .3 }])).toBe(false);
    const lut = curveLut();
    lut.forEach((v, i) => expect(v).toBeCloseTo(i / 255, 7));
    expect(curvePoints([{ x: .5, y: .6 }])).toEqual([{ x: 0, y: 0 }, { x: .5, y: .6 }, { x: 1, y: 1 }]);
  });
  it('preserves flat intervals and vertical endpoint edits without overshoot', () => {
    const lut = curveLut([{ x: 0, y: .1 }, { x: .3, y: .7 }, { x: .6, y: .7 }, { x: 1, y: .9 }]);
    expect(lut[0]).toBeCloseTo(.1, 7);
    expect(lut[255]).toBeCloseTo(.9, 7);
    expect([...lut].every((v, i) => !i || v >= lut[i - 1])).toBe(true);
    expect([...lut.slice(77, 153)].every(v => Math.abs(v - .7) < 1e-7)).toBe(true);
  });
  it('does not overshoot when a point reverses the curve', () => {
    const lut = curveLut([{ x: .3, y: .8 }, { x: .7, y: .2 }]);
    expect(Math.max(...lut.slice(77, 179))).toBeLessThanOrEqual(.8);
    expect(Math.min(...lut.slice(77, 179))).toBeGreaterThanOrEqual(.2);
  });
});
