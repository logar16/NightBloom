import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const origin = process.argv[2] ?? process.env.ASTROIMP_API_URL ?? 'http://127.0.0.1:8765';
const schema = new URL('/openapi.json', origin).href;
const cli = fileURLToPath(new URL('../../node_modules/openapi-typescript/bin/cli.js', import.meta.url));
const output = fileURLToPath(new URL('./schema.d.ts', import.meta.url));
const result = spawnSync(process.execPath, [cli, schema, '-o', output], { stdio: 'inherit' });
if (result.error) throw result.error;
process.exit(result.status ?? 1);
