const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(path.resolve(__dirname, '..', '..', 'frontend', 'node_modules', 'playwright'));
const ts = require(path.resolve(__dirname, '..', '..', 'frontend', 'node_modules', 'typescript'));

function autoToneCode() {
  const root = path.resolve(__dirname, '..', '..');
  const source = fs.readFileSync(path.join(root, 'frontend', 'src', 'screens', 'sky', 'Snapshot.tsx'), 'utf8');
  const start = source.indexOf('  const autoTone = () => {');
  const end = source.indexOf('  const neutral = ', start);
  if (start < 0 || end < 0) throw new Error('Cannot locate the actual Sky Auto implementation');
  return ts.transpileModule(source.slice(start, end), {
    compilerOptions: { target: ts.ScriptTarget.ES2022 },
  }).outputText;
}

function toneHTML(imageURL, width, height) {
  return `
      <style>html,body{margin:0;background:black}img{display:block;width:${width}px;height:${height}px}</style>
      <img alt="Rosette Sky display" src="${imageURL}">
      <svg width="0" height="0" style="position:absolute">
        <filter id="tone" color-interpolation-filters="sRGB">
          <feComponentTransfer id="range"></feComponentTransfer>
          <feComponentTransfer id="gamma"></feComponentTransfer>
          <feColorMatrix type="saturate" values="1"/>
        </filter>
      </svg>`;
}

async function applyTone(page, code) {
  return page.evaluate(code => {
      const image = { current: document.querySelector('img') };
      const tone = { black: 0, white: 1, midtones: 1, saturation: 1 };
      let result;
      const changeTone = next => { result = next; };
      eval(code + '\nautoTone();');
      if (!result) throw new Error('Sky Auto did not produce tone controls');
      const range = Math.max(.01, result.white - result.black);
      document.querySelector('#range').innerHTML = ['R', 'G', 'B'].map(c =>
        `<feFunc${c} type="linear" slope="${1 / range}" intercept="${-result.black / range}"/>`).join('');
      document.querySelector('#gamma').innerHTML = ['R', 'G', 'B'].map(c =>
        `<feFunc${c} type="gamma" amplitude="1" exponent="${result.midtones}" offset="0"/>`).join('');
      image.current.style.filter = 'url(#tone)';
      return result;
    }, code);
}

async function main() {
  const assets = path.join(__dirname, 'assets');
  const imageURL = `data:image/png;base64,${fs.readFileSync(path.join(assets, 'sky-before.png')).toString('base64')}`;
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1400, height: 812 }, deviceScaleFactor: 1 });
    await page.setContent(toneHTML(imageURL, 1400, 812));
    await page.getByRole('img', { name: 'Rosette Sky display' }).evaluate(img => img.decode());
    const result = await applyTone(page, autoToneCode());
    await page.getByRole('img', { name: 'Rosette Sky display' }).screenshot({
      path: path.join(assets, 'sky-after.png'),
    });
    fs.writeFileSync(path.join(assets, 'sky-metrics.json'), JSON.stringify(result, null, 2) + '\n');
    console.log(JSON.stringify(result));
  } finally {
    await browser.close();
  }
}

module.exports = { autoToneCode, toneHTML, applyTone };
if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
