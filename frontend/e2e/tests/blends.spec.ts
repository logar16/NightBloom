import { mkdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { test, expect, editScreen, exportPreview, layerRows } from '../fixtures';
import { type Blend, type EditorState, type ProjectMeta } from '../../src/editor/state';

test('Edit - blend explainer applies Multiply as one undoable edit', async ({ page }) => {
  await editScreen(page);
  await layerRows(page).first().click();
  const blend = page.getByRole('combobox', { name: 'Blend', exact: true });
  await expect(blend).toHaveText('Screen');
  await page.getByRole('button', { name: 'How blend modes work', exact: true }).click();
  const help = page.getByRole('dialog', { name: 'How blend modes work' });
  await expect(help.getByRole('button', { name: /^Apply / })).toHaveCount(10);
  await expect(help.getByRole('radio', { name: 'Contrast', exact: true })).toHaveAttribute('aria-checked', 'true');
  await expect(help.getByText('Similar colours — showing contrasting pair')).toBeVisible();
  const intensity = help.getByRole('checkbox', { name: 'Use layer intensity' });
  await expect(intensity).not.toBeChecked();
  await intensity.check();
  await expect(intensity).toBeChecked();
  await intensity.uncheck();
  await help.getByRole('radio', { name: 'Layers', exact: true }).click();
  await expect(help.getByRole('radio', { name: 'Layers', exact: true })).toHaveAttribute('aria-checked', 'true');
  await help.getByRole('radio', { name: 'Contrast', exact: true }).click();
  const distinction = await help.locator('.blend-tile canvas').evaluateAll(elements => {
    const pixels = elements.map(el => {
      const canvas = el as HTMLCanvasElement;
      return canvas.getContext('2d')!.getImageData(0, 0, canvas.width, canvas.height).data;
    });
    let minimum = Infinity;
    for (let a = 0; a < pixels.length; a++) for (let b = a + 1; b < pixels.length; b++) {
      let difference = 0;
      for (let i = 0; i < pixels[a].length; i++) if (i % 4 !== 3) difference += Math.abs(pixels[a][i] - pixels[b][i]);
      minimum = Math.min(minimum, difference / (pixels[a].length * .75));
    }
    return minimum;
  });
  expect(distinction).toBeGreaterThan(.5);
  const opacity = help.getByRole('slider', { name: 'Example opacity', exact: true });
  await opacity.press('ArrowLeft');
  await expect(opacity).toHaveAttribute('aria-valuenow', '0.99');
  await expect(page.getByRole('slider', { name: 'Opacity', exact: true })).toHaveAttribute('aria-valuenow', '1');
  await expect(page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true })).toBeDisabled();
  await help.getByRole('button', { name: 'Apply Multiply', exact: true }).click();
  await expect(blend).toHaveText('Multiply');
  await expect(help.getByRole('button', { name: 'Apply Multiply', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await page.keyboard.press('Escape');
  await expect(help).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'How blend modes work', exact: true })).toBeFocused();
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(blend).toHaveText('Screen');
  await page.setViewportSize({ width: 1920, height: 1080 });
  const screenshots = process.env.ASTROIMP_BLEND_SCREENSHOTS;
  if (screenshots) await mkdir(screenshots, { recursive: true });
  for (const selection of ['single', 'multi'] as const) {
    if (selection === 'multi') await layerRows(page).nth(1).click({ modifiers: ['Control'] });
    await page.getByRole('button', { name: 'How blend modes work', exact: true }).click();
    await expect(help.getByRole('button', { name: /^Apply / })).toHaveCount(10);
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      await help.getByRole('button', { name: 'Apply Overlay', exact: true }).hover();
      if (screenshots) await page.screenshot({ path: path.join(screenshots, `${selection}-${theme}.png`) });
      expect(await help.evaluate(el => el.scrollHeight <= el.clientHeight)).toBe(true);
    }
    if (selection === 'multi') {
      await help.getByRole('button', { name: 'Apply Screen', exact: true }).focus();
      await page.keyboard.press('ArrowRight');
      await expect(help.getByRole('button', { name: 'Apply Multiply', exact: true })).toBeFocused();
      await page.keyboard.press('Enter');
      await expect(blend).toHaveText('Multiply');
    }
    await page.keyboard.press('Escape');
    if (selection === 'multi') {
      await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
      await expect(blend).toHaveText('Screen');
    }
  }
});

test('Edit - blends, survey matching and manual output tone agree with NumPy exports', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { meta: ProjectMeta; edits: EditorState };
  const state = structuredClone(session.edits);
  const [top, base] = state.layers;
  for (const layer of state.layers) layer.visible = layer === top || layer === base;
  for (const [layer, color] of [[top!, '#40a0ff'], [base!, '#ffd080']] as const) Object.assign(layer, {
    mode: 'tint', color, stretch: 'asinh', k: 20, black: 0, white: 1, intensity: 1, opacity: 1, blend: 'add',
    transform: { x: 0, y: 0, scale: 1, rotation: 0 }, edge: { feather: 0, irregularity: 0 }, brush: { strokes: [] },
    mask: { source: null, black: 0, white: 1, invert: false },
  });
  state.global.flipX = false; state.global.output = { exposure: 0, highlights: 0 };
  await page.route('**/api/export', route => route.fulfill({ json: { path: 'blend-parity.png' } }));
  for (const mode of ['subtract', 'darken', 'screen', 'manual'] as const) {
    const blend: Blend = mode === 'manual' ? 'screen' : mode;
    top!.blend = blend;
    top!.toneMatch = blend === 'screen' ? { gain: .65, offset: .12 } : undefined;
    expect((await request.put(`${studio.origin}/api/edits`, { data: state })).ok()).toBeTruthy();
    await page.reload(); await editScreen(page);
    if (mode === 'manual') {
      for (const [label, value] of [['Black point', '.08'], ['White point', '.95'], ['Midtones', '.7']]) {
        const input = page.getByRole('textbox', { name: `${label} value`, exact: true });
        await input.fill(value); await input.press('Enter');
      }
      const midpoint = page.getByRole('slider', { name: 'Midtones', exact: true });
      await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
      await expect(midpoint).toHaveAttribute('aria-valuenow', '1');
      await page.getByRole('button', { name: 'Redo (Ctrl+Shift+Z)', exact: true }).click();
      await expect(midpoint).toHaveAttribute('aria-valuenow', '0.7');
      const white = page.getByRole('slider', { name: 'White point', exact: true });
      await white.dblclick(); await expect(white).toHaveAttribute('aria-valuenow', '1');
      const whiteInput = page.getByRole('textbox', { name: 'White point value', exact: true });
      await whiteInput.fill('.95'); await whiteInput.press('Enter');
      await expect(white).toHaveAttribute('aria-valuenow', '0.95');
      await expect.poll(async () => {
        const saved = await (await request.get(`${studio.origin}/api/edits`)).json() as EditorState;
        return saved.global.output;
      }).toMatchObject({ black: .08, white: .95, midtones: .7 });
      await page.reload(); await editScreen(page);
      await expect(midpoint).toHaveAttribute('aria-valuenow', '0.7');
      await expect(white).toHaveAttribute('aria-valuenow', '0.95');
    }
    const posted = page.waitForRequest('**/api/export'), download = page.waitForEvent('download');
    await exportPreview(page);
    const png = (await posted).postDataBuffer()!;
    await download;
    const report = await (await request.get(`${studio.origin}/api/analyze?max_side=3072`)).json() as { preview: string };
    const reference = await readFile(report.preview);
    const difference = await page.evaluate(async ({ gpu, cpu }) => {
      const decode = async (encoded: string) => {
        const image = await createImageBitmap(await (await fetch(`data:image/png;base64,${encoded}`)).blob());
        const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;
        const context = canvas.getContext('2d')!; context.drawImage(image, 0, 0); image.close();
        return context.getImageData(0, 0, canvas.width, canvas.height).data;
      };
      const a = await decode(gpu), b = await decode(cpu);
      let sum = 0, count = 0, lit = 0;
      for (let i = 0; i < a.length; i++) if (i % 4 !== 3) { sum += Math.abs(a[i]! - b[i]!); count++; if (a[i]! > 8) lit++; }
      return { mean: sum / count, lit: lit / count };
    }, { gpu: png.toString('base64'), cpu: reference.toString('base64') });
    await test.info().attach(`blend-${mode}`, { body: JSON.stringify(difference), contentType: 'application/json' });
    expect(difference.mean).toBeLessThan(.5);
    expect(difference.lit).toBeGreaterThan(.01);
  }
});
