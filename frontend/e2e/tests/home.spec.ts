import { test, expect, increment, openHomeProject } from '../fixtures';
import type { SessionSummary } from '../../src/api/types';
import { sessionIds } from '../setup';

test('Home - browses without loading a project and opens recent projects directly in Edit', async ({ page, studio, request }) => {
  const loads: string[] = [];
  page.on('request', request => {
    const url = new URL(request.url());
    if (/^\/api\/session$|^\/api\/project\/|^\/api\/sessions\/[^/]+\/open$/.test(url.pathname))
      loads.push(url.pathname);
  });
  await page.reload();
  await expect(page.getByRole('region', { name: 'Home', exact: true })).toBeVisible();
  const projects = await (await request.get(`${studio.origin}/api/sessions`)).json() as SessionSummary[];
  const recent = page.getByRole('navigation', { name: 'Projects', exact: true }).getByRole('link');
  await expect(recent).toHaveCount(projects.length);
  await expect(recent.first()).toHaveAccessibleName(`Open ${projects[0].name}`);
  const library = page.getByRole('complementary', { name: 'Project library', exact: true });
  const heading = library.getByRole('heading', { name: 'Projects', exact: true });
  await expect(heading).toBeHidden();
  await recent.first().hover();
  await expect(heading).toBeVisible();
  await page.getByRole('link', { name: 'NightBloom Home', exact: true }).hover();
  await expect(heading).toBeHidden();
  await page.getByRole('button', { name: 'New project', exact: true }).focus();
  await page.keyboard.press('Tab');
  await expect(heading).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(heading).toBeHidden();
  await expect(library.getByRole('button', { name: 'Show projects', exact: true })).toBeFocused();
  expect(loads).toEqual([]);
  await openHomeProject(page);
  await expect(page.getByRole('button', { name: 'Export', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Session history', exact: true }).click();
  await page.getByRole('button', { name: /Eagle Nebula parity.*images/ }).click();
  await expect(page.getByRole('button', { name: 'Export', exact: true })).toBeEnabled();
  await increment(page, 'Exposure');
  await expect.poll(async () => (await (await request.get(`${studio.origin}/api/edits`)).json())
    .global.output.exposure).toBeCloseTo(.05);
  await increment(page, 'Highlight roll-off');
  await expect.poll(async () => (await (await request.get(`${studio.origin}/api/edits`)).json())
    .global.output.highlights).toBeCloseTo(.01);
  await page.getByRole('link', { name: 'NightBloom Home', exact: true }).click();
  await page.setViewportSize({ width: 420, height: 860 });
  await page.getByRole('button', { name: 'Show projects', exact: true }).click();
  const other = page.getByRole('link', { name: `Open ${projects.find(project => project.id === sessionIds[1])!.name}`, exact: true });
  await other.click();
  await expect(page.getByRole('button', { name: 'Export', exact: true })).toBeEnabled();
  await expect.poll(async () => (await (await request.get(`${studio.origin}/api/session`)).json()).id).toBe(sessionIds[1]);
});
