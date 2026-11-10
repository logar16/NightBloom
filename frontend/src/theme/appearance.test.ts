import { describe, expect, it } from 'vitest';
import {
  accentText, appearanceKey, defaults, luminance, parseAppearance,
  readAppearance, readableAccent, saveAppearance,
} from './appearance';

describe('appearance preferences', () => {
  it('round trips theme and both accents through storage', () => {
    const data = new Map<string, string>();
    const storage = { getItem: (key: string) => data.get(key) ?? null,
      setItem: (key: string, value: string) => { data.set(key, value); } };
    const value = { theme: 'system' as const, primary: '#123456', secondary: '#abcdef' };
    saveAppearance(value, storage);
    expect(data.has(appearanceKey)).toBe(true);
    expect(readAppearance(storage)).toEqual(value);
  });
  it('recovers from unavailable or corrupt storage', () => {
    expect(readAppearance({ getItem: () => '{broken' })).toEqual(defaults);
    expect(readAppearance({ getItem: () => { throw Error('Blocked'); } })).toEqual(defaults);
    expect(() => saveAppearance(defaults, { setItem: () => { throw Error('Quota'); } })).not.toThrow();
  });
  it('validates individual preferences without discarding valid ones', () => {
    expect(parseAppearance({ theme: 'light', primary: 'red', secondary: '#ABCDEF' }))
      .toEqual({ theme: 'light', primary: defaults.primary, secondary: '#abcdef' });
    expect(parseAppearance({ theme: 'other', primary: 'url(evil)' })).toEqual(defaults);
    expect(parseAppearance(null)).toEqual(defaults);
  });
});
describe('accent readability', () => {
  it('uses linear sRGB luminance, not average channel brightness', () => {
    expect(luminance('#000000')).toBe(0);
    expect(luminance('#ffffff')).toBe(1);
    expect(luminance('#00ff00')).toBeCloseTo(.7152);
    expect(accentText('#00ff00')).toBe('#000000');
    expect(accentText('#0000ff')).toBe('#ffffff');
    expect(accentText(defaults.primary)).toBe('#000000');
  });
  it('chooses the higher-contrast button foreground across the colour space', () => {
    for (let r = 0; r <= 255; r += 51) for (let g = 0; g <= 255; g += 51) for (let b = 0; b <= 255; b += 51) {
      const hex = '#' + [r, g, b].map(v => v.toString(16).padStart(2, '0')).join('');
      const background = luminance(hex), foreground = luminance(accentText(hex));
      expect((Math.max(background, foreground) + .05) / (Math.min(background, foreground) + .05)).toBeGreaterThanOrEqual(4.5);
    }
  });
  it('derives AA text from even white-on-light and black-on-dark accents', () => {
    for (const [surface, mix] of [['#e5e6e7', '#000000'], ['#232427', '#ffffff']] as const) {
      for (const hex of ['#ffffff', '#000000', '#e9b35a', '#5fc9d6', '#772255']) {
        const css = readableAccent(hex, surface, mix);
        const percent = Number(css.match(/(\d+)%/)![1]);
        const blended = '#' + [1, 3, 5].map(i => Math.round(parseInt(hex.slice(i, i + 2), 16) * percent / 100
          + parseInt(mix.slice(i, i + 2), 16) * (1 - percent / 100)).toString(16).padStart(2, '0')).join('');
        const a = luminance(blended), b = luminance(surface);
        expect((Math.max(a, b) + .05) / (Math.min(a, b) + .05)).toBeGreaterThanOrEqual(4.5);
      }
    }
  });
});
