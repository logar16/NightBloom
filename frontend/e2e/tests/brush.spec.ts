import { test, expect, editScreen, layerRows } from '../fixtures';
import type { EditorState } from '../../src/editor/state';

test('Brush - paints selected layers and persists on release', async ({ page, studio }) => {
  await editScreen(page);
  await layerRows(page).first().getByText(studio.initial.layers[0]!.name, { exact: true }).click();
  await layerRows(page).nth(1).getByText(studio.initial.layers[1]!.name, { exact: true }).click({ modifiers: ['Control'] });
  await page.keyboard.press('b');
  await page.getByRole('textbox', { name: 'Size value', exact: true }).fill('90');
  await page.getByRole('textbox', { name: 'Size value', exact: true }).press('Enter');
  const actualCanvas = page.locator('canvas[aria-label="Astronomical composition"]');
  const bounds = (await actualCanvas.boundingBox())!;
  await page.mouse.move(bounds.x + bounds.width * .35, bounds.y + bounds.height * .45);
  await page.mouse.down();
  await page.mouse.move(bounds.x + bounds.width * .65, bounds.y + bounds.height * .6, { steps: 12 });
  expect((studio.edits.at(-1) as unknown as EditorState | undefined)?.layers[0]?.brush.strokes.length || 0).toBe(0);
  await page.mouse.up();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)?.layers.slice(0, 2).map(l => l.brush.strokes.length)).toEqual([1, 1]);
  const stroke = (studio.edits.at(-1) as unknown as EditorState).layers[0]!.brush.strokes[0]!;
  expect(stroke.mode).toBe('hide'); expect(stroke.points.length).toBeGreaterThan(1);
  expect(stroke.points.flat().every(v => v * 2 === Math.round(v * 2))).toBe(true);
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)?.layers.slice(0, 2).map(l => l.brush.strokes.length)).toEqual([0, 0]);
  await page.getByRole('button', { name: 'Redo (Ctrl+Shift+Z)', exact: true }).click();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)?.layers[0]?.brush.strokes.length).toBe(1);
  await page.getByRole('button', { name: 'Show mask', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Show mask', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await page.keyboard.down('Alt');
  await expect(page.getByRole('radio', { name: 'Reveal', exact: true })).toHaveAttribute('aria-checked', 'true');
  await page.keyboard.up('Alt');
  await expect(page.getByRole('radio', { name: 'Hide', exact: true })).toHaveAttribute('aria-checked', 'true');
  await page.getByRole('button', { name: 'Clear brush', exact: true }).click();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)?.layers.slice(0, 2).map(l => l.brush.strokes.length)).toEqual([1, 0]);
});

test('Brush - Black and White paint source intensity with Alt inversion and undo', async ({ page, studio }) => {
  await editScreen(page);
  await layerRows(page).first().getByText(studio.initial.layers[0]!.name, { exact: true }).click();
  await page.keyboard.press('b');
  await page.getByRole('radio', { name: 'White', exact: true }).click();
  const size = page.getByRole('textbox', { name: 'Size value', exact: true });
  await size.fill('60'); await size.press('Enter');
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  const bounds = (await canvas.boundingBox())!;
  await page.mouse.click(bounds.x + bounds.width * .45, bounds.y + bounds.height * .45);
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers[0]?.brush.strokes.map(stroke => stroke.mode)).toEqual(['white']);
  await page.keyboard.down('Alt');
  await expect(page.getByRole('radio', { name: 'Black', exact: true })).toHaveAttribute('aria-checked', 'true');
  await page.mouse.click(bounds.x + bounds.width * .55, bounds.y + bounds.height * .55);
  await page.keyboard.up('Alt');
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers[0]?.brush.strokes.map(stroke => stroke.mode)).toEqual(['white', 'black']);
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers[0]?.brush.strokes.map(stroke => stroke.mode)).toEqual(['white']);
  await page.getByRole('button', { name: 'Redo (Ctrl+Shift+Z)', exact: true }).click();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers[0]?.brush.strokes.map(stroke => stroke.mode)).toEqual(['white', 'black']);
  await page.getByRole('button', { name: 'Clear brush', exact: true }).click();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers[0]?.brush.strokes.length).toBe(0);
});

test('Brush - cross-layer undo keeps the active Solo layer and subsequent strokes on it', async ({ page, studio }) => {
  await editScreen(page);
  const first = studio.initial.layers[0]!, second = studio.initial.layers[1]!;
  const a = page.getByRole('row', { name: first.name, exact: true });
  const b = page.getByRole('row', { name: second.name, exact: true });
  await a.getByText(first.name, { exact: true }).click();
  await page.keyboard.press('b');
  const canvas = page.getByLabel('Astronomical composition', { exact: true });
  const bounds = (await canvas.boundingBox())!;
  const paint = () => page.mouse.click(bounds.x + bounds.width * .5, bounds.y + bounds.height * .5);
  await paint();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers.slice(0, 2).map(layer => layer.brush.strokes.length)).toEqual([1, 0]);
  await b.getByText(second.name, { exact: true }).click();
  const solo = b.getByRole('button', { name: 'Show only this layer (Q)', exact: true });
  await solo.click();
  await paint();
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers.slice(0, 2).map(layer => layer.brush.strokes.length)).toEqual([1, 1]);
  await page.getByRole('button', { name: 'Undo (Ctrl+Z)', exact: true }).click();
  await expect(b).toHaveAttribute('aria-current', 'true');
  await expect(solo).toHaveAttribute('aria-pressed', 'true');
  await expect.poll(() => (studio.edits.at(-1) as unknown as EditorState | undefined)
    ?.layers.slice(0, 2).map(layer => layer.brush.strokes.length)).toEqual([1, 0]);
  await paint();
  await expect.poll(() => {
    const state = studio.edits.at(-1) as unknown as EditorState | undefined;
    return [state?.selectedLayer, state?.layers.slice(0, 2).map(layer => layer.brush.strokes.length)];
  }).toEqual([second.id, [1, 1]]);
  await expect(solo).toHaveAttribute('aria-pressed', 'true');
});
