import { test, expect, element, observation } from '../fixtures';

test('Sky - expanded preview keeps its thumbnail through loading and failure, then retries', async ({ page }) => {
  let release!: () => void;
  const ready = new Promise<void>(resolve => { release = resolve; });
  let attempts = 0;
  await page.route('**/api/preview/**', async route => {
    if (new URL(route.request().url()).searchParams.get('size') !== 'large') return route.fallback();
    if (++attempts > 1) return route.fallback();
    await ready;
    return route.fulfill({ status: 503, json: { detail: 'Preview service unavailable' } });
  });
  try {
    await page.getByRole('button', { name: /^Preview / }).first().click();
    const dialog = page.getByRole('dialog'), image = dialog.getByRole('img');
    await expect(dialog.getByRole('status')).toHaveText('Loading preview…');
    await expect(image).toHaveAttribute('src', /size=thumb/);
    await expect.poll(() => image.evaluate(node => (node as HTMLImageElement).naturalWidth)).toBeGreaterThan(0);
    release();
    await expect(dialog.getByRole('alert')).toContainText('Large preview unavailable');
    await expect(image).toHaveAttribute('src', /size=thumb/);
    await dialog.getByRole('button', { name: 'Retry', exact: true }).focus();
    await page.keyboard.press('Enter');
    await expect(dialog.getByRole('alert')).toHaveCount(0);
    await expect(image).toHaveAttribute('src', /size=large/);
  } finally { release(); }
});

test('Sky - group selection excludes dismissed observations', async ({ page, studio, request }) => {
  const card = page.getByRole('article').first();
  await page.getByRole('button', { name: 'Clear selection', exact: true }).click();
  const ids = await card.getByTestId('observation').evaluateAll(items => items.map(item => item.getAttribute('data-obs')!));
  const dismissed = ids.at(-1)!;
  await observation(page, dismissed).getByRole('button', { name: 'Dismiss image', exact: true }).click();
  await card.getByRole('button', { name: 'Select all in group', exact: true }).click();
  const selected = async () => (await (await request.get(`${studio.origin}/api/session`)).json()).selection.obs_ids as string[];
  await expect.poll(selected).toEqual(expect.arrayContaining(ids.filter(id => id !== dismissed)));
  expect(await selected()).not.toContain(dismissed);
  await card.getByRole('button', { name: 'Deselect group', exact: true }).click();
  await expect.poll(selected).toEqual([]);
});

test('Sky - preview keyboard triage, navigation and group restore persist', async ({ page, studio, request }) => {
  const card = page.getByRole('article').first(), items = card.getByTestId('observation');
  const first = (await items.first().getAttribute('data-obs'))!;
  const next = (await items.nth(1).getAttribute('data-obs'))!;
  const triage = async () => (await (await request.get(`${studio.origin}/api/session`)).json()).triage as {
    favorites: string[]; dismissed: string[];
  };
  await items.first().getByRole('button', { name: /^Preview / }).click();
  const dialog = page.getByRole('dialog');
  await page.keyboard.press('Space');
  const selection = dialog.getByRole('button', { name: /^(Deselect|Select) image/ });
  const selected = await selection.getAttribute('aria-pressed');
  await page.keyboard.press('Enter');
  await expect(selection).toHaveAttribute('aria-pressed', selected === 'true' ? 'false' : 'true');
  await page.keyboard.press('f');
  await expect.poll(async () => (await triage()).favorites).toContain(first);
  await page.keyboard.press('d');
  await expect.poll(async () => (await triage()).dismissed).toContain(first);
  await expect(dialog.locator('#preview-image')).toHaveAttribute('src', `/api/preview/${encodeURIComponent(next)}?size=large&session=${studio.session.id}`);
  await page.keyboard.press('ArrowLeft');
  await expect(dialog.locator('#preview-image')).toHaveAttribute('src', `/api/preview/${encodeURIComponent(first)}?size=large&session=${studio.session.id}`);
  await page.keyboard.press('ArrowRight');
  await expect(dialog.locator('#preview-image')).toHaveAttribute('src', `/api/preview/${encodeURIComponent(next)}?size=large&session=${studio.session.id}`);
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await page.locator('#dismissed-groups summary').click();
  await observation(page, first).getByRole('button', { name: 'Restore image', exact: true }).click();
  await expect.poll(async () => (await triage()).dismissed).toEqual([]);
  const groupIds = await card.getByTestId('observation').evaluateAll(nodes => nodes.map(node => node.getAttribute('data-obs')!));
  await card.getByRole('button', { name: 'Favorite group', exact: true }).click();
  await expect.poll(async () => (await triage()).favorites).toEqual(expect.arrayContaining(groupIds));
  await card.getByRole('button', { name: 'Dismiss group', exact: true }).click();
  await expect.poll(async () => (await triage()).dismissed).toEqual(expect.arrayContaining(groupIds));
  const drawer = element(page, 'dismissed-groups');
  if (await drawer.getAttribute('open') === null) await drawer.locator('summary').click();
  await drawer.getByRole('button', { name: 'Restore group', exact: true }).first().click();
  await expect.poll(async () => (await triage()).dismissed).toEqual([]);
});

test('Sky - snapshot point filter, Clear point and Escape', async ({ page, studio }) => {
  const snapshot = element(page, 'footprints'), bounds = (await snapshot.boundingBox())!;
  const [w, h] = studio.session.view.pixel_dimensions;
  const polygons = studio.session.catalog.groups.flatMap(g => g.observations.flatMap(o => o.pixels ?? []));
  const poly = polygons.find(p => p.every(([x, y]) => x! > 0 && x! < w && y! > 0 && y! < h))!;
  const x = poly.reduce((sum, p) => sum + p[0]!, 0) / poly.length;
  const y = poly.reduce((sum, p) => sum + p[1]!, 0) / poly.length;
  const click = () => snapshot.click({ position: { x: x / w * bounds.width, y: y / h * bounds.height } });
  await click();
  await expect(page.getByRole('button', { name: 'Clear point', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Select (?!all)/ }).first()).toBeVisible();
  await page.getByRole('button', { name: 'Clear point', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Clear point', exact: true })).toBeHidden();
  await click();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: 'Clear point', exact: true })).toBeHidden();
});

test('Sky - a running capture hides the previous results', async ({ page, studio, request }) => {
  const real = await (await request.get(`${studio.origin}/api/status`)).json();
  await page.route('**/api/status', route => route.fulfill({ json: { ...real, job: {
    id: 'c', kind: 'capture', state: 'running', stage: 'search', progress: .35, message: 'Querying MAST', log: [] } } }));
  await page.reload();
  await page.getByRole('button', { name: 'Sky', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Searching the sky' })).toBeVisible();
  await expect(page.getByRole('article')).toHaveCount(0);
  if (process.env.ASTROIMP_SHOTS) await page.screenshot({ path: process.env.ASTROIMP_SHOTS });
});
