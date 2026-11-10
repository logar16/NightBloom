import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const readJson = file => JSON.parse(readFileSync(file, 'utf8'));
const metadata = JSON.parse(execFileSync('cargo', [
  'metadata', '--manifest-path', path.join(root, 'src-tauri', 'Cargo.toml'),
  '--features', 'desktop', '--filter-platform', 'x86_64-pc-windows-msvc',
  '--format-version', '1', '--locked', '--offline',
], { cwd: root, encoding: 'utf8', windowsHide: true, maxBuffer: 16 * 1024 * 1024 }));
const nodes = new Map(metadata.resolve.nodes.map(node => [node.id, node]));
const included = new Set();
function visit(id) {
  if (included.has(id)) return;
  included.add(id);
  for (const dep of nodes.get(id).deps) visit(dep.pkg);
}
visit(metadata.resolve.root);

function licenseFiles(folder) {
  return readdirSync(folder, { withFileTypes: true })
    .filter(entry => /^(licen[cs]es?|copying|notice|copyright)([-_.]|$)/i.test(entry.name))
    .flatMap(entry => {
      const file = path.join(folder, entry.name);
      return entry.isFile() ? [file] : entry.isDirectory() ? textFiles(file) : [];
    });
}
function textFiles(folder) {
  return readdirSync(folder, { withFileTypes: true }).flatMap(entry => {
    const file = path.join(folder, entry.name);
    return entry.isDirectory() ? textFiles(file) : entry.isFile() ? [file] : [];
  });
}

const texts = new Map();
const components = [];
function record(name, version, license, source, files) {
  if (!license || !files.length) throw new Error(`Missing license information: ${name} ${version}`);
  const ids = [...new Set(files.map(({ text }) => {
    const normalized = text.replaceAll('\r\n', '\n').replace(/[ \t]+$/gm, '').trim();
    if (!normalized || normalized.includes('\0')) throw new Error(`Invalid license text for ${name}`);
    const id = createHash('sha256').update(normalized).digest('hex').slice(0, 16);
    texts.set(id, normalized);
    return id;
  }))];
  components.push({ name, version, license, source, ids });
}

async function rustLicenses(pkg) {
  const folder = path.dirname(pkg.manifest_path);
  const files = licenseFiles(folder);
  if (pkg.license_file) files.push(path.resolve(folder, pkg.license_file));
  if (files.length) return [...new Set(files)].map(file => ({ text: readFileSync(file, 'utf8') }));

  // Some published binding crates omit the workspace's license files.
  const vcs = readJson(path.join(folder, '.cargo_vcs_info.json'));
  const repo = new URL(pkg.repository.replace(/\.git$/, ''));
  if (repo.hostname !== 'github.com' || !vcs.git?.sha1) {
    throw new Error(`No packaged license or pinned GitHub source for ${pkg.name}`);
  }
  const [owner, name] = repo.pathname.split('/').filter(Boolean);
  console.log(`Reading omitted upstream license files: ${pkg.name} ${pkg.version}`);
  const upstream = await Promise.all([
    'LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'LICENSE.md', 'LICENSE.txt', 'COPYING',
  ].map(async file => {
    const url = `https://raw.githubusercontent.com/${owner}/${name}/${vcs.git.sha1}/${file}`;
    const response = await fetch(url);
    if (response.status === 404) return null;
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    return { text: await response.text() };
  }));
  const found = upstream.filter(file => file !== null);
  if (!found.length && pkg.license === 'MPL-2.0') {
    const standard = [...texts.values()].find(text => text.includes('Mozilla Public License Version 2.0'));
    if (!standard) throw new Error(`Standard MPL-2.0 text unavailable for ${pkg.name}`);
    console.log(`Using the standard MPL-2.0 text for ${pkg.name}; exact source is linked.`);
    return [{ text: standard }];
  }
  return found;
}

for (const pkg of metadata.packages.filter(pkg =>
  included.has(pkg.id) && pkg.id !== metadata.resolve.root,
).sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))) {
  record(`Rust: ${pkg.name}`, pkg.version, pkg.license,
    `https://crates.io/api/v1/crates/${pkg.name}/${pkg.version}/download`,
    await rustLicenses(pkg));
}

const lock = readJson(path.join(root, 'frontend', 'package-lock.json'));
for (const [location, entry] of Object.entries(lock.packages).sort(([a], [b]) => a.localeCompare(b))) {
  if (!location || entry.dev) continue;
  const folder = path.join(root, 'frontend', location);
  if (!existsSync(folder) && entry.optional) {
    console.log(`Not distributed (optional package absent): ${location}`);
    continue;
  }
  const pkg = readJson(path.join(folder, 'package.json'));
  record(`JavaScript: ${pkg.name}`, pkg.version, pkg.license,
    `https://www.npmjs.com/package/${pkg.name}/v/${pkg.version}`,
    licenseFiles(folder).map(file => ({ text: readFileSync(file, 'utf8') })));
}

const output = [
  '# Third-party notices',
  '',
  'Generated by `node scripts/third-party-notices.mjs` from the locked Windows x64',
  'desktop dependency graph and installed production frontend dependencies.',
  'Build-time dependencies are included conservatively. Component licenses remain',
  'applicable independently of NightBloom\'s own license.',
  '',
  'MPL-2.0 components are unmodified. Their exact source packages are available',
  'through the source links below under their respective licenses.',
  'Microsoft Edge WebView2 Runtime and Aladin are installed separately, not bundled',
  'in the portable ZIP; their own terms apply.',
  '',
  '## Components',
  '',
  ...components.flatMap(pkg => [
    `### ${pkg.name} ${pkg.version}`,
    '',
    `License: ${pkg.license}`,
    '',
    `Source: ${pkg.source}`,
    '',
    `License texts: ${pkg.ids.map(id => `[${id}](#license-${id})`).join(', ')}`,
    '',
  ]),
  '## License texts',
  '',
  ...[...texts].sort(([a], [b]) => a.localeCompare(b)).flatMap(([id, text]) => [
    `### License ${id}`, '', '```text', text, '```', '',
  ]),
].join('\n');
writeFileSync(path.join(root, 'THIRD_PARTY_NOTICES.md'), output);
console.log(`Wrote notices for ${components.length} components (${texts.size} distinct texts).`);
