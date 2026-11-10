import { test, expect, editScreen, openHomeProject } from '../fixtures';
import type { EditorState } from '../../src/editor/state';

test.beforeEach(async ({ page }) => {
  await openHomeProject(page);
  await page.getByRole('button', { name: 'Sky', exact: true }).click();
});

test('Surveys - single-band cards stay visible and participate in selection', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json();
  const survey = {
    id: 'S-IPHAS', collection: 'Survey', instrument: 'IPHAS', targets: [], field: 'IPHAS',
    title: 'IPHAS', date: '', filters: ['Hα'], observations: [{
      obs_id: 'survey-IPHAS-Ha', obsid: 'survey-IPHAS-Ha', collection: 'Survey', instrument: 'IPHAS',
      filter: 'Hα', target: '', title: 'IPHAS Hα', wavelength_nm: 656.3,
      preview_uri: 'hips:CDS/P/IPHAS/DR2/halpha', pixels: [],
      info: { line: 'Hα', shows: 'Ionised hydrogen · diffuse emission' },
    }],
  };
  await page.route('**/api/session', route => route.fulfill({
    json: { ...session, catalog: { ...session.catalog, groups: [...session.catalog.groups, survey] } },
  }));
  let selection: string[] = [];
  await page.route('**/api/selection', route => {
    selection = route.request().postDataJSON().obs_ids;
    return route.fulfill({ json: { obs_ids: selection } });
  });
  await page.reload();
  await page.getByRole('button', { name: 'Sky', exact: true }).click();
  const card = page.getByRole('article').filter({ has: page.getByRole('heading', { name: 'IPHAS', exact: true }) });
  await expect(card).toHaveCount(1);
  await card.getByRole('button', { name: 'Preview Hα', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText('Hα · IPHAS');
  await page.keyboard.press('Escape');
  await card.getByRole('button', { name: 'Select Hα survey-IPHAS-Ha', exact: true }).click();
  await expect.poll(() => selection).toContain('survey-IPHAS-Ha');
  await page.getByRole('button', { name: 'Surveys', exact: true }).click();
  await expect(card).toHaveCount(0);
});

test('Surveys - wide views offer the optional HST/JWST search with progress', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json();
  await page.route('**/api/session', route => route.fulfill({
    json: { ...session, catalog: { ...session.catalog, mast_searched: false } },
  }));
  let searching = false;
  const job = { id: 'mock-mast', kind: 'search-mast', state: 'running', stage: 'search',
    progress: 0, message: 'Searching public HST/JWST observations', log: [] };
  await page.route('**/api/session/search-mast', route => {
    searching = true;
    return route.fulfill({ json: job });
  });
  await page.route('**/api/status', route => route.fulfill({
    json: { aladin: true, stale: false, session: studio.session.id, job: searching ? job : null,
      edits: { revision: 0 } },
  }));
  await page.reload();
  await page.getByRole('button', { name: 'Sky', exact: true }).click();
  await page.getByRole('button', { name: 'Also search HST/JWST', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Searching the sky' })).toContainText('Searching public HST/JWST observations');
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('Surveys - lower-resolution-only results open automatically and remain discoverable after collapse', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json();
  const reason = '~6.5″ detail; crisp only for views ≥ 3.7°';
  const survey = {
    id: 'S-WISE', collection: 'Survey', instrument: 'WISE', targets: [], title: 'WISE',
    date: '', filters: ['W3'], observations: [{
      obs_id: 'survey-W3', obsid: 'survey-W3', collection: 'Survey', instrument: 'WISE',
      filter: 'W3', target: '', title: 'WISE W3', wavelength_nm: 12000,
      preview_uri: 'hips:CDS/P/allWISE/W3', pixels: [], info: { shows: 'Warm dust' },
      fwhm_arcsec: 6.5, soft: true, soft_reason: reason,
    }],
  };
  await page.route('**/api/session', route => route.fulfill({
    json: { ...session, catalog: { ...session.catalog, groups: [survey] } },
  }));
  let selection: string[] = [];
  await page.route('**/api/selection', route => {
    selection = route.request().postDataJSON().obs_ids;
    return route.fulfill({ json: { obs_ids: selection } });
  });
  await page.reload();
  await page.getByRole('button', { name: 'Sky', exact: true }).click();
  const select = page.getByRole('button', { name: 'Select W3 survey-W3', exact: true });
  const toggle = page.getByText('Lower-resolution surveys', { exact: false });
  await expect(select).toHaveAttribute('title', `W3 · Warm dust · ${reason}`);
  await select.click();
  await expect.poll(() => selection).toContain('survey-W3');
  await toggle.click();
  await expect(select).toBeHidden();
  await page.getByRole('button', { name: 'Find W3', exact: true }).click();
  await expect(select).toHaveAttribute('aria-pressed', 'true');
});

test('Surveys - layer info reports effective detail', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json();
  session.meta.layers.forEach((layer: { fwhm_arcsec?: number }) => { layer.fwhm_arcsec = 6.5; });
  await page.route('**/api/session', route => route.fulfill({ json: session }));
  await page.reload();
  await editScreen(page);
  await expect(page.locator('#filter-info')).toContainText('Detail ≈ 6.5″');
});

test('Fill around - chosen margin is sent without a live survey request', async ({ page }) => {
  let margin = 0;
  await page.route('**/api/build/plan', route => route.fulfill({
    json: { files: [], total_bytes: 0, cached_bytes: 0, grid_locked: true,
      grid: { width: 3072, height: 2262, pixel_scale_arcsec: .15, wcs: {} } },
  }));
  await page.route('**/api/build/fill-around/plan**', route => {
    const value = Number(new URL(route.request().url()).searchParams.get('margin'));
    const factor = 3072 / (3072 + 2 * Math.ceil(3072 * value));
    return route.fulfill({ json: {
      width: 3072, height: Math.ceil((2262 + 2 * Math.ceil(2262 * value)) * factor - 1e-8),
      pixel_scale_arcsec: .15 / factor, wcs: {},
    } });
  });
  await page.route('**/api/build/fill-around', route => {
    margin = route.request().postDataJSON().margin;
    return route.fulfill({ json: { id: 'mock-fill', kind: 'build', state: 'running',
      stage: 'download', progress: 0, message: 'Survey DSS2 red', log: [] } });
  });
  await page.getByRole('navigation', { name: 'Workspace', exact: true })
    .getByRole('button', { name: 'Build', exact: true }).click();
  await page.getByRole('combobox', { name: 'Fill margin per side', exact: true }).selectOption('0.25');
  await expect(page.getByTitle('Fill canvas; resolution is limited to 3072 pixels on the longest side'))
    .toHaveText('3072 × 2263 · 0.2250″/px');
  await page.getByRole('button', { name: 'Fill around', exact: true }).click();
  await expect.poll(() => margin).toBe(.25);
});

test('Survey tone matching - readout resets as one undoable change', async ({ page, studio, request }) => {
  const session = await (await request.get(`${studio.origin}/api/session`)).json() as { edits: EditorState };
  session.edits.layers[0]!.toneMatch = { gain: .65, offset: .12 };
  expect((await request.put(`${studio.origin}/api/edits`, { data: session.edits })).ok()).toBeTruthy();
  await page.reload();
  await editScreen(page);
  await expect(page.getByText('Survey match', { exact: true })).toHaveCount(1);
  await page.getByRole('button', { name: 'Reset survey tone match', exact: true }).click();
  await expect(page.getByText('Survey match', { exact: true })).toHaveCount(0);
  await page.keyboard.press('Control+z');
  await expect(page.getByText('Survey match', { exact: true })).toHaveCount(1);
});
