import { test, expect } from '@playwright/test';
import { mkdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { runDir, sessionIds } from '../setup';
import { defaultState, type ProjectMeta } from '../../src/editor/state';
import type { Job } from '../../src/api/types';

test('Projects - tabs stay isolated, restore routes, and show the shared queue', async ({ context, request }) => {
  const { origin } = JSON.parse(await readFile(path.join(runDir, 'server.json'), 'utf8')) as { origin: string };
  const [orion, carina] = sessionIds;
  const projects = [{ id: orion, name: 'Orion' }, { id: carina, name: 'Carina' }];
  for (const project of projects) {
    const headers = { 'X-AstroImp-Session': project.id };
    const session = await (await request.get(`${origin}/api/session`, { headers })).json() as { meta: ProjectMeta };
    const state = defaultState(session.meta);
    state.global.output.exposure = 0;
    await request.put(`${origin}/api/edits`, { data: state, headers });
    await request.patch(`${origin}/api/session`, { data: { name: project.name }, headers });
  }
  const jobA: Job = {
    id: 'mock-orion-build', session: orion, name: 'Orion', kind: 'build',
    state: 'running', stage: 'align', phase: 'align', progress: .4,
    message: 'Aligning Orion', eta_s: 124, log: [],
  };
  const jobB: Job = {
    id: 'mock-carina-build', session: carina, name: 'Carina', kind: 'build',
    state: 'queued', stage: 'align', phase: 'align', progress: 0,
    message: 'Waiting for Orion', eta_s: null, log: [],
    waiting_for: { session: orion, name: 'Orion', stage: 'align', progress: .4 },
  };
  let queueCarina = false;
  await context.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    const id = route.request().headers()['x-astroimp-session'] || url.searchParams.get('session') || orion;
    const jobs = queueCarina ? [jobA, jobB] : [jobA];
    if (url.pathname === '/api/status') {
      const edits = await (await request.get(`${origin}/api/edits/revision`, {
        headers: { 'X-AstroImp-Session': id },
      })).json();
      await route.fulfill({ json: { aladin: true, stale: false, session: id, edits,
        job: jobs.find(job => job.session === id) ?? null, jobs } });
    } else if (url.pathname === '/api/job') {
      await route.fulfill({ json: jobs.find(job => job.session === id) ?? null });
    } else if (url.pathname === '/api/build/plan') {
      await route.fulfill({ json: { files: [], total_bytes: 0, cached_bytes: 0 } });
    } else if (url.pathname.startsWith('/api/aladin') || url.pathname.startsWith('/api/preview')
      || url.pathname === '/api/capture' || url.pathname === '/api/build'
      || url.pathname.includes('search-mast') || url.pathname.includes('fill-around')) {
      await route.fulfill({ status: 409, json: { detail: 'Live network operations disabled' } });
    } else await route.continue();
  });
  const a = await context.newPage(), b = await context.newPage();
  await test.step('Open two projects in separate tabs and save only to Carina', async () => {
    await a.goto(`${origin}/#/p/${orion}/build`);
    await b.goto(`${origin}/#/p/${carina}/edit`);
    await expect(a.getByText('Aligning Orion', { exact: true })).toHaveCount(1);
    await expect(b.getByRole('button', { name: 'Export', exact: true })).toBeEnabled();
    const exposure = b.getByRole('slider', { name: 'Exposure', exact: true });
    await exposure.focus();
    await exposure.press('ArrowRight');
    await expect.poll(async () => (await (await request.get(`${origin}/api/edits`, {
      headers: { 'X-AstroImp-Session': carina },
    })).json()).global.output.exposure).toBeCloseTo(.05);
    const stateA = await (await request.get(`${origin}/api/edits`, {
      headers: { 'X-AstroImp-Session': orion },
    })).json();
    expect(stateA.global.output.exposure).toBe(0);
    await a.reload();
    await expect(a).toHaveURL(`${origin}/#/p/${orion}/build`);
    await expect(a).toHaveTitle('Orion · Build — NightBloom');
    await expect(b).toHaveTitle('Carina · Edit — NightBloom');
  });
  await test.step('Flush a pending same-tab edit before switching projects', async () => {
    const exposure = b.getByRole('slider', { name: 'Exposure', exact: true });
    await exposure.focus();
    await exposure.press('ArrowRight');
    await b.getByRole('button', { name: 'Session history', exact: true }).click();
    await b.getByRole('button', { name: /^Orion.*images/ }).click();
    await expect(b).toHaveURL(`${origin}/#/p/${orion}/edit`);
    await expect.poll(async () => (await (await request.get(`${origin}/api/edits`, {
      headers: { 'X-AstroImp-Session': carina },
    })).json()).global.output.exposure).toBeCloseTo(.1);
  });
  await test.step('Inspect both named jobs and progress on Home', async () => {
    queueCarina = true;
    await a.getByRole('button', { name: /Project jobs/ }).click();
    const tray = a.locator('.jobs-tray');
    await expect(tray.getByRole('link', { name: /Carina/ })).toContainText('Waiting for Orion (align 40%)');
    await expect(tray.getByRole('link', { name: /^Orion.*40%/ })).toContainText('align');
    const screenshots = process.env.ASTROIMP_SCREENSHOTS;
    if (screenshots) await mkdir(screenshots, { recursive: true });
    for (const theme of ['dark', 'light'] as const) {
      await a.evaluate(theme => {
        document.documentElement.dataset.theme = theme;
      }, theme);
      if (screenshots) await a.screenshot({ path: path.join(screenshots, `header-jobs-${theme}.png`) });
    }
    await a.keyboard.press('Escape');
    await a.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
    await expect(a.locator('.home-job')).toHaveCount(2);
    await expect(a.getByRole('link', { name: /^Open / }).first()).toHaveAttribute('href', /#\/p\//);
    for (const theme of ['dark', 'light'] as const) {
      await a.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      if (screenshots) await a.screenshot({ path: path.join(screenshots, `home-progress-${theme}.png`) });
    }
  });
});
