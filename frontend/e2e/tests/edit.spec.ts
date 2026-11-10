import { test, expect, editScreen, element, increment, layerRows, exportPreview } from '../fixtures';

test('Edit - Backspace in a layer name does not delete the layer', async ({ page, studio }) => {
  await editScreen(page);
  const rows = layerRows(page), count = studio.initial.layers.length;
  await rows.first().click();
  await page.keyboard.press('F2');
  const input = page.getByRole('textbox', { name: 'Layer name', exact: true });
  await input.fill('Keyboard layer');
  await input.press('Backspace');
  await expect(rows).toHaveCount(count);
  await expect(input).toHaveValue('Keyboard laye');
});

test('Edit - Space pans with brush without painting, restores tool, and 0 / 1 zoom uses build pixels', async ({ page, studio }) => {
  await editScreen(page);
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  const area = page.getByTestId('canvas-area');
  await canvas.focus();
  await page.keyboard.press('b');
  const box = (await canvas.boundingBox())!;
  const center = { x: box.width / 2, y: box.height / 2 };
  await canvas.hover({ position: center });
  const before = await page.getByTestId('pixel-info').innerText();
  await page.keyboard.down('Space');
  await expect(area).toHaveAttribute('data-tool', 'pan');
  await page.mouse.down();
  await page.mouse.move(box.x + center.x + 60, box.y + center.y + 25, { steps: 5 });
  await page.mouse.up();
  await page.keyboard.up('Space');
  await expect(area).toHaveAttribute('data-tool', 'brush');
  await canvas.hover({ position: center });
  await expect(page.getByTestId('pixel-info')).not.toHaveText(before);
  await expect(page.getByRole('button', { name: 'Clear brush', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true })).toBeDisabled();
  await page.keyboard.press('1');
  await expect(page.getByLabel('Zoom', { exact: true })).toHaveText('100%');
  await canvas.hover({ position: center });
  const first = (await page.getByTestId('pixel-info').innerText()).split(',').map(Number);
  await canvas.hover({ position: { x: center.x + 10, y: center.y } });
  const second = (await page.getByTestId('pixel-info').innerText()).split(',').map(Number);
  expect(second[0]! - first[0]!).toBe(10);
  await page.keyboard.press('0');
  const fit = Math.round(Math.min(box.width / studio.session.meta.width, box.height / studio.session.meta.height) * 100);
  await expect(page.getByLabel('Zoom', { exact: true })).toHaveText(`${fit}%`);
  await page.getByRole('textbox', { name: 'Size value', exact: true }).fill('100');
  await page.getByRole('textbox', { name: 'Size value', exact: true }).press('Space');
  await expect(area).toHaveAttribute('data-tool', 'brush');
});

test('Edit - project renders nonempty pixels, visibility and layer reorder', async ({ page, studio }) => {
  await editScreen(page);
  await expect(layerRows(page)).toHaveCount(studio.initial.layers.length);
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  await expect.poll(() => canvas.evaluate((node) => {
    const source = node as HTMLCanvasElement;
    const copy = document.createElement('canvas');
    copy.width = 64;
    copy.height = 64;
    const context = copy.getContext('2d')!;
    context.drawImage(source, 0, 0, 64, 64);
    const pixels = context.getImageData(0, 0, 64, 64).data;
    return Array.from(pixels).filter((value, index) => index % 4 !== 3 && value > 8).length;
  })).toBeGreaterThan(10);
  const name = studio.initial.layers[0]!.name;
  await page.getByRole('button', { name: `Hide ${name}`, exact: true }).click();
  await expect(page.getByRole('button', { name: `Show ${name}`, exact: true })).toBeVisible();
  await expect.poll(() => studio.edits.at(-1)?.layers[0]?.visible).toBe(false);
  await page.getByRole('button', { name: `Show ${name}`, exact: true }).click();
  await layerRows(page).first().dragTo(layerRows(page).nth(2));
  await expect.poll(() => studio.edits.at(-1)?.layers[2]?.id).toBe(studio.initial.layers[0]!.id);
});

test('Edit - autosave failure warns until the next successful save', async ({ page, studio }) => {
  await editScreen(page);
  let fail = true;
  await page.route('**/api/edits', async route => {
    if (fail && route.request().method() === 'PUT')
      await route.fulfill({ status: 503, json: { detail: 'Autosave unavailable' } });
    else await route.fallback();
  });
  await increment(page, 'Exposure');
  await expect(page.getByRole('img', { name: 'Autosave failed', exact: true })).toHaveCount(1);
  await expect(page.getByRole('status')).toContainText('Autosave unavailable');
  fail = false;
  await increment(page, 'Exposure');
  await expect.poll(() => studio.edits.at(-1)?.global.output.exposure).toBeCloseTo(.1);
  await expect(page.getByRole('img', { name: 'Autosave failed', exact: true })).toHaveCount(0);
});

test('Edit - rotate then crop exports the post-rotation dimensions and pixels', async ({ page, studio }) => {
  await editScreen(page);
  await page.getByRole('button', { name: 'Rotate (R)', exact: true }).click();
  const angle = page.getByRole('slider', { name: 'Angle', exact: true });
  await page.getByRole('textbox', { name: 'Angle value', exact: true }).fill('30');
  await page.getByRole('textbox', { name: 'Angle value', exact: true }).press('Enter');
  await expect(angle).toHaveAttribute('aria-valuenow', '30');
  await page.route('**/api/export', (route) => route.fulfill({ json: { path: 'parity.png' } }));
  const exportPng = async () => {
    const posted = page.waitForRequest('**/api/export');
    const download = page.waitForEvent('download');
    await exportPreview(page);
    const request = await posted;
    await download;
    return request.postDataBuffer()!;
  };
  const rotated = await exportPng();
  await page.getByRole('button', { name: 'Crop (C)', exact: true }).click();
  const area = element(page, 'canvas-area');
  const box = (await area.boundingBox())!;
  await page.mouse.move(box.x + box.width * .3, box.y + box.height * .3);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * .7, box.y + box.height * .7, { steps: 10 });
  await page.mouse.up();
  await page.getByRole('button', { name: 'Apply crop (Enter)', exact: true }).click();
  await expect.poll(() => studio.edits.at(-1)?.global.crop.w).toBeLessThan(1);
  const edits = studio.edits.at(-1)!;
  const png = await exportPng();
  expect(png.length).toBeGreaterThan(100);
  expect(png.readUInt32BE(16)).toBe(Math.max(1, Math.round(studio.session.meta.width * edits.global.crop.w)));
  expect(png.readUInt32BE(20)).toBe(Math.max(1, Math.round(studio.session.meta.height * edits.global.crop.h)));
  expect(edits.global.rotation).toBe(30);
  const difference = await page.evaluate(async ({ full, cropped, crop }) => {
    const decode = async (encoded: string) => createImageBitmap(await (await fetch(`data:image/png;base64,${encoded}`)).blob());
    const fullImage = await decode(full);
    const cropImage = await decode(cropped);
    const sample = (image: ImageBitmap, rect: [number, number, number, number]) => {
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = 16;
      const context = canvas.getContext('2d')!;
      context.imageSmoothingQuality = 'high';
      context.drawImage(image, ...rect, 0, 0, 16, 16);
      return context.getImageData(0, 0, 16, 16).data;
    };
    const expected = sample(fullImage, [crop.x * fullImage.width, crop.y * fullImage.height,
      crop.w * fullImage.width, crop.h * fullImage.height]);
    const actual = sample(cropImage, [0, 0, cropImage.width, cropImage.height]);
    let error = 0;
    for (let index = 0; index < expected.length; index++) {
      if (index % 4 !== 3) error += Math.abs(expected[index]! - actual[index]!);
    }
    fullImage.close();
    cropImage.close();
    return error / (16 * 16 * 3);
  }, { full: rotated.toString('base64'), cropped: png.toString('base64'), crop: edits.global.crop });
  // Export dimensions round to integer pixels; the crop edges can differ by half a source pixel, and
  // star-dense fixtures add resampling noise. A wrong crop region errs by tens of levels.
  expect(difference).toBeLessThan(8);
});

test('Edit - assistant revision toast, value synchronization and undo', async ({ page, studio, request }) => {
  await editScreen(page);
  const slider = page.getByRole('slider', { name: 'Exposure', exact: true });
  await expect(slider).toHaveAttribute('aria-valuenow', '0');
  const response = await request.post(`${studio.origin}/api/edits/apply`, { data: {
    note: 'Parity assistant change', steps: [{ target: 'global', ops: ['output.exposure=0.7'] }],
  } });
  expect(response.ok()).toBeTruthy();
  await expect(page.getByRole('status')).toContainText(/assistant|Parity assistant/i);
  await expect(slider).toHaveAttribute('aria-valuenow', '0.7');
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(slider).toHaveAttribute('aria-valuenow', '0');
});
