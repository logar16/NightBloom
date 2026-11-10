import { test, expect } from '../fixtures';

test('Canvas - draw a square and apply only its reviewed grid', async ({ page }) => {
  const preview = { id: 'canvas-fixture', width: 1000, height: 800, current_scale_arcsec: .15,
    image_url: 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aASsAAAAASUVORK5CYII=',
    default_region: { x: 100, y: 100, w: 300, h: 400 }, current_rect: null, footprints: [] };
  let confirmed: { region: { x: number; y: number; w: number; h: number }; grid: { width: number; height: number } } | undefined;
  await page.route('**/api/build/canvas/prepare', route => route.fulfill({ json: {
    id: 'canvas-smoke', kind: 'canvas', state: 'done', stage: 'canvas', progress: 1, log: [],
  } }));
  await page.route('**/api/build/canvas/preview', route => route.fulfill({ json: preview }));
  await page.route('**/api/build/canvas/plan', route => {
    const r = route.request().postDataJSON().region as { w: number; h: number };
    return route.fulfill({ json: { grid: { width: Math.round(r.w), height: Math.round(r.h),
      pixel_scale_arcsec: .15, wcs: {} }, kept: [], detail_ratio: 1 } });
  });
  await page.route('**/api/build/canvas', async route => {
    confirmed = route.request().postDataJSON();
    await route.fulfill({ json: { id: 'canvas-build-smoke', kind: 'build', state: 'error',
      stage: 'prepare', progress: 0, error: 'Smoke test: no science files changed', log: [] } });
  });
  await page.getByRole('button', { name: 'Build', exact: true }).click();
  await page.getByRole('button', { name: 'Canvas', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Canvas', exact: true });
  await dialog.getByRole('radio', { name: 'Square', exact: true }).click();
  const svg = dialog.getByRole('img', { name: 'Canvas mosaic' });
  const points = await svg.evaluate(element => {
    const matrix = (element as SVGSVGElement).getScreenCTM()!;
    return [new DOMPoint(200, 50).matrixTransform(matrix), new DOMPoint(700, 550).matrixTransform(matrix)]
      .map(p => ({ x: p.x, y: p.y }));
  });
  await page.mouse.move(points[0].x, points[0].y);
  await page.mouse.down();
  await page.mouse.move(points[1].x, points[1].y, { steps: 5 });
  await page.mouse.up();
  const apply = dialog.getByRole('button', { name: 'Apply & build', exact: true });
  await expect(apply).toBeEnabled();
  expect(confirmed).toBeUndefined();
  await apply.click();
  await expect.poll(() => confirmed?.grid).toMatchObject({ width: 500, height: 500 });
  expect(confirmed!.region.x).toBeCloseTo(200, 4);
  expect(confirmed!.region.y).toBeCloseTo(50, 4);
  expect(confirmed!.region.w).toBeCloseTo(500, 4);
  expect(confirmed!.region.h).toBeCloseTo(500, 4);
});
