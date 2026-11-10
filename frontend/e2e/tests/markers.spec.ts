import { test, expect, editScreen, element } from '../fixtures';
import type { Marker } from '../../src/editor/markers';

test('Edit - canvas markers track image coordinates through rotation and zoom', async ({ page, studio }) => {
  let stored: Marker[] = [];
  const writes: Marker[][] = [];
  await page.route('**/api/markers', async route => {
    if (route.request().method() === 'PUT') {
      stored = route.request().postDataJSON() as Marker[];
      writes.push(structuredClone(stored));
      await route.fulfill({ json: { count: stored.length } });
    } else await route.fulfill({ json: stored });
  });
  await editScreen(page);
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  const box = (await canvas.boundingBox())!;
  const point = { x: box.width * .5, y: box.height * .48 };
  await expect(page.getByRole('button', { name: 'Mark (M)', exact: true })).toBeEnabled();
  await canvas.focus(); await page.keyboard.press('m');
  await expect(element(page, 'canvas-area')).toHaveAttribute('data-tool', 'mark');
  await canvas.hover({ position: point });
  const coordinates = (await element(page, 'pixel-info').innerText()).split(',').map(Number);
  await canvas.click({ position: point });
  await expect.poll(() => writes.at(-1)?.length).toBe(1);
  expect(stored[0]).toMatchObject({ n: 1, kind: 'point', x: coordinates[0], y: coordinates[1], radius: 4 });
  await expect(page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true })).toBeDisabled();
  const pin = page.getByRole('img', { name: `Marker #1 point ${coordinates[0]},${coordinates[1]} r=4`, exact: true })
    .locator('.marker-anchor');
  const pinCenter = () => pin.evaluate(node => {
    const rect = node.getBoundingClientRect();
    return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
  });
  const fit = Math.min(box.width / studio.session.meta.width, box.height / studio.session.meta.height);
  const dx = coordinates[0]! - studio.session.meta.width / 2, dy = coordinates[1]! - studio.session.meta.height / 2;
  expect((await pinCenter()).x).toBeCloseTo(box.x + box.width / 2 + dx * fit, 1);
  expect((await pinCenter()).y).toBeCloseTo(box.y + box.height / 2 + dy * fit, 1);
  await canvas.focus(); await page.keyboard.press('r');
  const angle = page.getByRole('textbox', { name: 'Angle value', exact: true });
  await angle.fill('30'); await angle.press('Enter');
  const radians = Math.PI / 6;
  await expect.poll(async () => (await pinCenter()).x).toBeCloseTo(
    box.x + box.width / 2 + (Math.cos(radians) * dx - Math.sin(radians) * dy) * fit, 1);
  await expect.poll(async () => (await pinCenter()).y).toBeCloseTo(
    box.y + box.height / 2 + (Math.sin(radians) * dx + Math.cos(radians) * dy) * fit, 1);
  await angle.fill('0'); await angle.press('Enter');
  await canvas.hover({ position: point }); await page.mouse.wheel(0, -100);
  await expect.poll(async () => (await pinCenter()).y).toBeCloseTo(box.y + box.height / 2 + dy * fit * Math.exp(.1), 1);
  await page.getByRole('button', { name: 'Fit image (F)', exact: true }).click();
  await canvas.focus(); await page.keyboard.press('m');
  await page.mouse.move(box.x + box.width * .45, box.y + box.height * .35);
  const start = (await element(page, 'pixel-info').innerText()).split(',').map(Number);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * .65, box.y + box.height * .65, { steps: 8 });
  const end = (await element(page, 'pixel-info').innerText()).split(',').map(Number);
  await page.mouse.up();
  await expect.poll(() => writes.at(-1)?.length).toBe(2);
  expect(stored[1]).toMatchObject({
    n: 2, kind: 'rect', x1: Math.min(start[0]!, end[0]!), y1: Math.min(start[1]!, end[1]!),
    x2: Math.max(start[0]!, end[0]!), y2: Math.max(start[1]!, end[1]!),
  });
  await expect(page.getByTestId('marker-rect')).toHaveCount(1);
});
