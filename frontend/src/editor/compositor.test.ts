import { describe, expect, it } from 'vitest';
import { Compositor, gradientBytes } from './compositor';
import { defaultState, type ProjectMeta } from './state';

const meta: ProjectMeta = { width: 1000, height: 600, layers: [], stars: { file: 'stars', list: 'list' } };
function coordinates(rotation: number, flipX = false) {
  const compositor = Object.create(Compositor.prototype) as Compositor;
  Object.assign(compositor, {
    canvas: { getBoundingClientRect: () => ({ width: 800, height: 500 }) },
    meta, state: defaultState(meta), view: { zoom: 1, x: 0, y: 0 },
  });
  compositor.state.global.rotation = rotation;
  compositor.state.global.flipX = flipX;
  compositor.state.global.crop = { x: .15, y: .25, w: .5, h: .4 };
  return compositor;
}
describe('post-rotation crop coordinates', () => {
  it('keeps the crop center in output space instead of rotating the crop rectangle', () => {
    const compositor = coordinates(30);
    const center = compositor.screenToOutput(400, 250);
    expect(center.x).toBeCloseTo(.4);
    expect(center.y).toBeCloseTo(.45);
    const source = compositor.screenToImage(400, 250);
    expect(source.x).toBeCloseTo(.5 + (Math.cos(Math.PI / 6) * -100 + Math.sin(Math.PI / 6) * -30) / 1000);
    expect(source.y).toBeCloseTo(.5 + (-Math.sin(Math.PI / 6) * -100 + Math.cos(Math.PI / 6) * -30) / 600);
    expect([Math.round(meta.width * compositor.state.global.crop.w), Math.round(meta.height * compositor.state.global.crop.h)])
      .toEqual([500, 240]);
  });
  it.each([false, true])('round-trips source points with a 30° rotation and flip=%s', flip => {
    const compositor = coordinates(30, flip);
    compositor.view = { zoom: 1.7, x: 17, y: -23 };
    for (const point of [{ x: .3, y: .4 }, { x: .7, y: .6 }]) {
      const screen = compositor.imageToScreen(point.x, point.y);
      const source = compositor.screenToImage(screen.x, screen.y);
      expect(source.x).toBeCloseTo(point.x, 10);
      expect(source.y).toBeCloseTo(point.y, 10);
    }
  });
});
describe('gradient LUT', () => {
  it('sorts stops, clamps outside the stop span, and interpolates 8-bit colors', () => {
    const bytes = gradientBytes([{ t: .75, color: '#ff8040' }, { t: .25, color: '#000000' }]);
    expect([...bytes.slice(0, 4)]).toEqual([0, 0, 0, 255]);
    expect([...bytes.slice(-4)]).toEqual([255, 128, 64, 255]);
    expect([...bytes.slice(128 * 4, 128 * 4 + 4)]).toEqual([129, 65, 32, 255]);
  });
});
