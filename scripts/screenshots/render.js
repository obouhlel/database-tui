// node render.js out_dir file1.html [file2.html ...] -> PNG next to out_dir
const { chromium } = require('playwright');
const path = require('path');

(async () => {
  const [outDir, ...files] = process.argv.slice(2);
  const browser = await chromium.launch();
  const page = await browser.newPage({ deviceScaleFactor: 2, viewport: { width: 1600, height: 1000 } });
  for (const f of files) {
    await page.goto('file://' + path.resolve(f));
    await page.evaluate(() => document.fonts.ready);
    const body = await page.$('body');
    const out = path.join(outDir, path.basename(f, '.html') + '.png');
    await body.screenshot({ path: out });
    console.log(out);
  }
  await browser.close();
})();
