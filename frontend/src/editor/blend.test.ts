import { describe, expect, it } from 'vitest';
import { blendPair, blendPixel, layerRamp, similarLayerHues } from './blend';
import { makeLayer, type Blend } from './state';

describe('blendPixel shader / render.py parity', () => {
  const a = [.25, .64, .8], b = [.8, .2, .5];
  const expected: [Blend, number[]][] = [
    ['add', [1.05, .84, 1.3]],
    ['screen', [.85, .712, .9]],
    ['multiply', [.2, .128, .4]],
    ['overlay', [.4, .424, .8]],
    ['softlight', [.4, .50176, .8]],
    ['lighten', [.8, .64, .8]],
    ['difference', [.55, .44, .3]],
    ['subtract', [0, .44, .3]],
    ['darken', [.25, .2, .5]],
  ];
  it.each(expected)('%s matches hard-coded NumPy results', (mode, values) => {
    blendPixel(mode, a, b).forEach((v, i) => expect(v).toBeCloseTo(values[i], 10));
    blendPixel(mode, a, b, .4).forEach((v, i) => expect(v).toBeCloseTo(a[i] * .6 + values[i] * .4, 10));
  });
  it('normal uses hue and brightness coverage, including black and full intensity', () => {
    const hue = [1, .5, 0];
    expect(blendPixel('normal', a, b, 1, hue, 0)).toEqual(a);
    expect(blendPixel('normal', a, b, 1, hue, 1)).toEqual(hue);
    blendPixel('normal', a, b, .5, hue, .4).forEach((v, i) => expect(v).toBeCloseTo([.4, .612, .64][i]));
  });
  it('soft light uses square root on its light branch', () => {
    blendPixel('softlight', [.25, .64, 0], [1, 1, 1]).forEach((v, i) => expect(v).toBeCloseTo([.5, .8, 0][i]));
  });
});

describe('example layer choice and ramps', () => {
  const layers = Array.from({ length: 4 }, () => makeLayer());
  it('uses the next visible lower layer, or a neutral fallback', () => {
    const stack = structuredClone(layers); stack[1].visible = false;
    expect(blendPair(stack, [stack[0].id])).toEqual([stack[0], stack[2]]);
    expect(blendPair(stack, [stack[3].id])).toEqual([stack[3], undefined]);
  });
  it('uses adjacent selections when all are selected and the sole outsider as base', () => {
    expect(blendPair(layers, layers.map(l => l.id))).toEqual([layers[0], layers[1]]);
    expect(blendPair(layers, [layers[0].id, layers[2].id, layers[3].id])).toEqual([layers[0], layers[1]]);
  });
  it('uses intensity and the real gradient/spectral map', () => {
    const layer = makeLayer(undefined, '#ff8000'); layer.intensity = .5;
    expect(layerRamp(layer)(0).color).toEqual([0, 0, 0]);
    expect(layerRamp(layer)(1).color).toEqual([.5, 128 / 510, 0]);
    layer.mode = 'gradient'; layer.gradient = [{ t: 0, color: '#ff0000' }, { t: 1, color: '#0000ff' }];
    const sample = layerRamp(layer)(1);
    expect(sample.color[0]).toBeCloseTo(.25, 2);
    expect(sample.color[2]).toBeCloseTo(.25, 2);
    layer.mode = 'spectral'; layer.spectral.stops = [{ nm: 200, color: '#00ff00' }, { nm: 25000, color: '#ff0000' }];
    expect(layerRamp(layer)(1).color).toEqual([.5, 0, 0]);
  });
  it('normalises demo colours to full brightness without changing realistic sampling', () => {
    const layer = makeLayer(undefined, '#804020'); layer.intensity = .2;
    expect(layerRamp(layer, false)(1).color).toEqual([1, .5, .25]);
    expect(layerRamp(layer, false)(0).color).toEqual([0, 0, 0]);
    expect(layerRamp(layer, true)(1).color[0]).toBeCloseTo(128 / 255 * .2);
    layer.color = '#000000';
    expect(layerRamp(layer, false)(1).color).toEqual([0, 0, 0]);
  });
  it('detects similar hues around the hue wheel and leaves distinct or neutral pairs alone', () => {
    const top = makeLayer(undefined, '#ff0040'), base = makeLayer(undefined, '#ff4000');
    expect(similarLayerHues(top, base)).toBe(false);
    base.color = '#ff0020';
    expect(similarLayerHues(top, base)).toBe(true);
    base.color = '#ff2000';
    expect(similarLayerHues(top, base)).toBe(true);
    base.color = '#00ffff';
    expect(similarLayerHues(top, base)).toBe(false);
    expect(similarLayerHues(top, undefined)).toBe(false);
  });
});
