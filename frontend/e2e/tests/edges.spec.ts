import { readFile } from 'node:fs/promises';
import { test, expect, editScreen, exportPreview } from '../fixtures';
import type { EditorState, ProjectMeta } from '../../src/editor/state';

test('Edit - feathered irregular edges match NumPy exports, including transformed sampling', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { meta: ProjectMeta; edits: EditorState };
  const source = session.meta.layers.find(l => l.collection === 'HST')!;
  const state = structuredClone(session.edits);
  const target = state.layers.find(l => l.source === source.key)!;
  for (const layer of state.layers) {
    layer.visible = layer.id === target.id;
    layer.transform = { x: 0, y: 0, scale: 1, rotation: 0 };
    layer.edge = { feather: 0, irregularity: 0 };
  }
  Object.assign(target, {
    mode: 'tint', color: '#e08040', stretch: 'linear', black: 0, white: 1, intensity: .8,
    opacity: 1, blend: 'normal', mask: { source: null, black: 0, white: 1, invert: false },
    edge: { feather: 45, irregularity: .85 },
    brush: { strokes: [
      { mode: 'hide', size: 120, hardness: .35, strength: .85, points: [[80, 110], [220, 180]] },
      { mode: 'reveal', size: 55, hardness: .6, strength: .5, points: [[145, 142], [170, 152]] },
    ] },
  });
  state.global.flipX = false; state.global.output = { exposure: 0, highlights: 0 };
  state.selectedLayer = target.id;
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'edge-parity.png' } }));
  const results = [];
  for (const transformed of [false, true]) {
    if (transformed) target.transform = { x: .04, y: -.03, scale: 1.08, rotation: 17 };
    expect((await request.put(`${studio.origin}/api/edits`, { data: state })).ok()).toBeTruthy();
    await page.reload(); await editScreen(page);
    await expect(page.getByRole('slider', { name: 'Edge softness', exact: true })).toHaveAttribute('aria-valuenow', '45');
    const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
    await exportPreview(page);
    const png = (await posted).postDataBuffer()!;
    await download;
    const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
    const reference = await readFile(report.preview);
    await test.info().attach(`edge-gpu-${transformed ? 'transformed' : 'plain'}`, { body: png, contentType: 'image/png' });
    await test.info().attach(`edge-cpu-${transformed ? 'transformed' : 'plain'}`, { body: reference, contentType: 'image/png' });
    const difference = await page.evaluate(async ({ gpu, cpu }) => {
      const decode = async (encoded: string) => {
        const image = await createImageBitmap(await (await fetch(`data:image/png;base64,${encoded}`)).blob());
        const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;
        const context = canvas.getContext('2d')!; context.drawImage(image, 0, 0); image.close();
        return { width: canvas.width, height: canvas.height, data: context.getImageData(0, 0, canvas.width, canvas.height).data };
      };
      const a = await decode(gpu), b = await decode(cpu);
      let sum = 0, max = 0, count = 0;
      for (let i = 0; i < a.data.length; i++) if (i % 4 !== 3) {
        const error = Math.abs(a.data[i]! - b.data[i]!); sum += error; max = Math.max(max, error); count++;
      }
      return { gpu: [a.width, a.height], cpu: [b.width, b.height], mean: sum / count, max };
    }, { gpu: png.toString('base64'), cpu: reference.toString('base64') });
    expect(difference.gpu).toEqual(difference.cpu);
    results.push({ transformed, ...difference });
    await test.info().attach('edge-parity', { body: JSON.stringify(results, null, 2), contentType: 'application/json' });
    expect(difference.mean).toBeLessThan(.5);
    expect(difference.max).toBeLessThan(6);
  }
  await test.info().attach('edge-parity', { body: JSON.stringify(results, null, 2), contentType: 'application/json' });
});
