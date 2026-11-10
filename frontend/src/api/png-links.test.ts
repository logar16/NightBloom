import { describe, expect, it } from 'vitest';
import { attachExportLink, readExportLink } from './png-links';

const original = new Blob([Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+afoUAAAAASUVORK5CYII=', 'base64')],
{ type: 'image/png' });
const link = { version: 1 as const, project: 'alpha', checkpoint: 'a'.repeat(64) };

describe('exported PNG saved-look links', () => {
  it('round-trips the portable pointer while preserving all original PNG encoding', async () => {
    const tagged = await attachExportLink(original, link);
    expect(await readExportLink(tagged)).toEqual(link);
    const before = new Uint8Array(await original.arrayBuffer());
    const after = new Uint8Array(await tagged.arrayBuffer());
    const size = new DataView(after.buffer).getUint32(33) + 12;
    expect(after.slice(0, 33)).toEqual(before.slice(0, 33));
    expect(after.slice(33 + size)).toEqual(before.slice(33));
  });

  it('rejects stripped metadata, damaged links and unsafe identifiers without guessing a project', async () => {
    await expect(readExportLink(original)).rejects.toThrow('no saved-look link');
    const bytes = new Uint8Array(await (await attachExportLink(original, link)).arrayBuffer());
    bytes[70] ^= 1;
    await expect(readExportLink(new Blob([bytes]))).rejects.toThrow('damaged');
    await expect(attachExportLink(original, { ...link, project: '../alpha' })).rejects.toThrow('Invalid');
    await expect(readExportLink(new Blob(['not a PNG']))).rejects.toThrow('Choose an exported PNG');
  });
});
