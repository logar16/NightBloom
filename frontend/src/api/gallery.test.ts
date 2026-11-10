import { expect, it } from 'vitest';
import type { GalleryImage } from './types';
import { galleryCover, sortExports } from './gallery';

function image(filename: string, side: number, modified: number): GalleryImage {
  return { filename, width: side, height: side, modified, size_bytes: 1, version: filename, error: null };
}

it('keeps recent export batches ahead of older, larger images without chaining batches', () => {
  const images = Object.freeze([
    image('older-16k', 16384, 750),
    image('oldest-32k', 32768, 0),
    image('latest-2k', 2048, 1200),
    image('boundary-8k', 8192, 900),
    image('nearby-4k', 4096, 1050),
    image('newer-8k', 8192, 920),
  ]);
  expect(sortExports(images).map(item => item.filename)).toEqual([
    'newer-8k', 'boundary-8k', 'nearby-4k', 'latest-2k', 'older-16k', 'oldest-32k',
  ]);
});

it('uses pixel count rather than file bytes, keeping unavailable dimensions visible in their batch', () => {
  const large = { ...image('large', 4096, 100), height: 2048 };
  const wide = { ...image('wide', 8192, 120), height: 512, size_bytes: 100_000 };
  const unavailable = { ...image('unavailable', 0, 130), width: null, height: null, error: 'Cannot read image' };
  expect(sortExports([unavailable, wide, large]).map(item => item.filename)).toEqual(['large', 'wide', 'unavailable']);
});

it('keeps a high-resolution photograph as cover, choosing the newest among similar export sizes', () => {
  const latest = { ...image('latest-near-4k', 3840, 200), height: 2160 };
  const images = [
    image('newer-2k', 2048, 300),
    image('old-4k', 4096, 100),
    latest,
    image('earlier/contact-sheet-test.png', 8192, 400),
    { ...image('unreadable', 16384, 500), error: 'Cannot read image' },
  ];
  expect(galleryCover(sortExports(images))).toBe(latest);
});

it('keeps contact-sheet-only and unreadable albums browsable', () => {
  const sheet = image('contact-sheet-test.png', 4096, 100);
  const broken = { ...image('broken', 0, 200), width: null, height: null, error: 'Cannot read image' };
  expect(galleryCover([broken, sheet])).toBe(sheet);
  expect(galleryCover([broken])).toBe(broken);
});
