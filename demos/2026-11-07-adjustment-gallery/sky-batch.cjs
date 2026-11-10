const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(path.resolve(__dirname, '..', '..', 'frontend', 'node_modules', 'playwright'));
const { autoToneCode, toneHTML, applyTone } = require('../2026-11-06-automatic-adjustments/sky-auto.cjs');

async function main() {
  const tasks = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
  const code = autoToneCode();
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 900, height: 600 }, deviceScaleFactor: 1 });
    for (const task of tasks) {
      const imageURL = `data:image/png;base64,${fs.readFileSync(task.input).toString('base64')}`;
      await page.setContent(toneHTML(imageURL, task.width, task.height));
      const image = page.getByRole('img', { name: 'Rosette Sky display' });
      await image.evaluate(img => img.decode());
      task.tone = await applyTone(page, code);
      await image.screenshot({ path: task.output });
    }
    fs.writeFileSync(process.argv[2], JSON.stringify(tasks, null, 2));
    console.log(`Applied the actual Sky Auto function to ${tasks.length} combinations.`);
  } finally {
    await browser.close();
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
