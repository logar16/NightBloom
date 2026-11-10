import { test as base, expect, type Locator, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { runDir, sessionIds } from './setup';
import { defaultState, type ProjectMeta } from '../src/editor/state';

export type Layer = {
  id: string; name: string; visible: boolean; intensity: number; white: number; opacity: number;
  edge: { feather: number; irregularity: number };
  brush: { strokes: unknown[] };
};
export type Edits = {
  layers: Layer[];
  global: { output: { exposure: number; highlights: number }; crop: { x: number; y: number; w: number; h: number }; rotation: number; flipX: boolean };
};
export type Observation = { obs_id: string; filter: string; collection: string; wavelength_nm: number; pixels: number[][][] };
export type Session = {
  id: string; name: string; build_version: string;
  meta: { width: number; height: number; build_version: string; layers: { obs_id: string; filter: string }[] };
  catalog: { groups: { collection: string; observations: Observation[] }[] };
  view: { pixel_dimensions: [number, number] };
};
type Studio = { origin: string; url: string; session: Session; initial: Edits; edits: Edits[] };

export function element(page: Page, testId: string): Locator {
  return page.getByTestId(testId);
}
export function observation(page: Page, id: string): Locator {
  return page.getByTestId('observation').and(page.locator(`[data-obs="${id}"]`));
}
export function layerRows(page: Page): Locator {
  return page.getByTestId('layer-row');
}
export async function editScreen(page: Page) {
  const edit = page.getByRole('button', { name: 'Edit', exact: true });
  const resume = page.getByRole('link', { name: 'Open Eagle Nebula parity', exact: true });
  await expect(edit.or(resume)).toBeVisible();
  if (await resume.isVisible()) await resume.click();
  await edit.click();
  await expect(layerRows(page)).not.toHaveCount(0);
  await expect(page.getByRole('slider', { name: 'Exposure', exact: true })).toHaveAttribute('aria-valuenow', /.+/);
}
export async function openHomeProject(page: Page, name = 'Eagle Nebula parity') {
  await page.getByRole('navigation', { name: 'Projects', exact: true })
    .getByRole('link', { name: `Open ${name}`, exact: true }).click();
}
export async function increment(page: Page, name: string, count = 1) {
  const slider = page.getByRole('slider', { name, exact: true });
  await slider.focus();
  for (let i = 0; i < count; i++) await slider.press('ArrowRight');
}
export async function exportPreview(page: Page) {
  await page.getByRole('button', { name: 'Export', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Export image' });
  await dialog.getByRole('radio', { name: /^Preview/ }).check();
  await dialog.getByRole('radio', { name: 'PNG (8-bit)', exact: true }).check();
  await dialog.getByRole('button', { name: 'Export', exact: true }).click();
}
export const test = base.extend<{ studio: Studio }>({
  studio: [async ({ page, request }, use, testInfo) => {
    const { origin } = JSON.parse(await readFile(path.join(runDir, 'server.json'), 'utf8')) as { origin: string };
    await request.post(`${origin}/api/sessions/${sessionIds[0]}/open`);
    const project = await (await request.get(`${origin}/api/session`)).json() as { meta: ProjectMeta };
    // Edit values belong to the test, not to the frozen reference composition.
    const initial = defaultState(project.meta);
    initial.checkpoints = [];
    // Fixed distinct values make relative-adjustment assertions meaningful and avoid clamping.
    initial.layers.forEach((layer, index) => {
      layer.intensity = .4 + index * .03; layer.visible = true;
      layer.edge = { feather: 0, irregularity: 0 };
      layer.brush = { strokes: [] };
    });
    initial.global.output = { exposure: 0, highlights: 0 };
    initial.global.crop = { x: 0, y: 0, w: 1, h: 1 };
    initial.global.rotation = 0;
    initial.global.flipX = false;
    await request.put(`${origin}/api/edits`, { data: initial });
    await request.put(`${origin}/api/triage`, { data: { favorites: [], dismissed: [] } });
    await request.patch(`${origin}/api/session`, { data: { name: 'Eagle Nebula parity' } });
    const session = await (await request.get(`${origin}/api/session`)).json() as Session;
    await request.put(`${origin}/api/selection`, { data: { obs_ids: session.meta.layers.map((layer) => layer.obs_id) } });
    const edits: Edits[] = [];
    let closing = false;
    const preview = await readFile(path.join(runDir, 'data', 'sessions', sessionIds[0], 'snapshot.jpg'));
    await page.route('**/api/**', async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname.startsWith('/api/aladin') || url.pathname === '/api/capture'
        || url.pathname === '/api/session/snapshot/refresh' || url.pathname === '/api/build'
        || url.pathname === '/api/session/search-mast' || url.pathname === '/api/build/fill-around'
        || url.pathname === '/api/export/full') {
        await route.fulfill({ status: 409, json: { detail: 'Live operations are disabled in parity tests' } });
      } else if (url.pathname === '/api/status') {
        const revision = await (await request.get(`${origin}/api/edits/revision`)).json() as object;
        await route.fulfill({ json: { aladin: true, stale: false, session: sessionIds[0], job: null, edits: revision } });
      } else if (url.pathname.startsWith('/api/preview/')) {
        await route.fulfill({ contentType: 'image/jpeg', body: preview });
      } else if (url.pathname === '/api/export/full/plan') {
        await route.fulfill({ json: {
          width: session.meta.width, height: session.meta.height,
          preview_width: session.meta.width, preview_height: session.meta.height, estimated_s: 1,
        } });
      } else if (url.pathname === '/api/build/plan') {
        const selection = await (await request.get(`${origin}/api/session`)).json() as { selection: { obs_ids: string[] } };
        if (closing) {
          await route.abort().catch(() => undefined);
          return;
        }
        const files = selection.selection.obs_ids.map((id) => ({
          obs_id: id, filename: `${id}.fits`, filter: session.meta.layers.find((layer) => layer.obs_id === id)?.filter ?? id,
          size_bytes: 100_000_000, cached: true,
        }));
        await route.fulfill({ json: { files, total_bytes: files.length * 100_000_000, cached_bytes: files.length * 100_000_000 } });
      } else {
        if (url.pathname === '/api/edits' && route.request().method() === 'PUT') edits.push(route.request().postDataJSON() as Edits);
        await route.continue();
      }
    });
    await page.goto(`${origin}/`);
    if (path.basename(testInfo.file) === 'shell-sky.spec.ts') {
      await openHomeProject(page);
      await page.getByRole('button', { name: 'Sky', exact: true }).click();
      await expect(page.getByTitle('Rename project (F2)', { exact: true })).toHaveText('Eagle Nebula parity');
    }
    try {
      await use({ origin, url: `${origin}/`, session, initial, edits });
    } finally {
      closing = true;
      await page.goto('about:blank');
      await page.unrouteAll({ behavior: 'wait' });
    }
  }, { auto: true }],
});
export { expect };
