import { spawn, execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { cp, mkdir, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
export const runDir = path.join(root, 'frontend', 'e2e', '.run', process.env.ASTROIMP_E2E_RUN_ID ?? 'manual');
export const sourceData = process.env.ASTROIMP_E2E_SOURCE_DATA ?? path.join(root, 'data');
// Frozen copies, kept outside data/sessions so they never show on Home or change with live edits.
export const fixtureSessions = path.join(sourceData, 'test-fixtures', 'sessions');
// Home opens the other built session from its recent-project rail.
export const sessionIds = ['20261023T050727Z', '20261023T050000Z-jwst-validation'];

async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No free TCP port');
  await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return address.port;
}

export default async function setup() {
  await rm(runDir, { recursive: true, force: true });
  await mkdir(path.join(runDir, 'data', 'sessions'), { recursive: true });
  for (const id of sessionIds) {
    const source = path.join(fixtureSessions, id);
    if (!existsSync(source)) throw new Error(
      `E2E fixture session missing: ${source}. Frozen science fixtures are not distributed. `
      + 'Set ASTROIMP_E2E_SOURCE_DATA to a fixture data folder; see frontend/e2e/BASELINE.md.',
    );
    await cp(source, path.join(runDir, 'data', 'sessions', id), {
      recursive: true,
      filter: (entry) => !['analysis', 'exports', 'builds'].includes(path.basename(entry)) && !path.basename(entry).startsWith('.fullres-'),
    });
    execFileSync('uv', ['run', '--no-sync', 'python', path.join(root, 'frontend', 'e2e', 'prepare-project.py'),
      path.join(runDir, 'data', 'sessions', id, 'project')], { cwd: root, windowsHide: true });
  }
  await mkdir(path.join(runDir, 'data', 'downloads'), { recursive: true });
  const port = await freePort();
  const origin = `http://127.0.0.1:${port}`;
  const child = spawn('uv', [
    'run', '--no-sync', 'python', path.join(root, 'frontend', 'e2e', 'serve.py'),
    'studio', '--child', '--no-browser', '--data', path.join(runDir, 'data'),
    '--port', String(port),
  ], { cwd: root, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = '';
  child.stdout.on('data', (chunk: Buffer) => { log += chunk.toString(); });
  child.stderr.on('data', (chunk: Buffer) => { log += chunk.toString(); });
  const stop = () => {
    if (child.pid && child.exitCode === null) {
      try { execFileSync('uv', ['run', '--no-sync', 'python', '-c',
        `import subprocess; subprocess.run(['taskkill','/F','/T','/PID','${child.pid}'], capture_output=True)`],
      { cwd: root, windowsHide: true, stdio: 'ignore' }); }
      catch { /* The server may already have exited. */ }
    }
  };
  try {
    const deadline = Date.now() + 120_000;
    while (true) {
      if (child.exitCode !== null) throw new Error(`Studio exited:\n${log}`);
      try {
        const response = await fetch(`${origin}/api/sessions`);
        if (response.ok) break;
      } catch { /* Await process readiness, not an arbitrary browser delay. */ }
      if (Date.now() > deadline) throw new Error(`Studio did not become ready:\n${log}`);
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    await writeFile(path.join(runDir, 'server.json'), JSON.stringify({ origin }));
    await writeFile(path.join(runDir, 'server.log'), log);
  } catch (error) {
    stop();
    throw error;
  }
  return async () => {
    stop();
    await rm(path.join(runDir, 'data'), { recursive: true, force: true });
  };
}
