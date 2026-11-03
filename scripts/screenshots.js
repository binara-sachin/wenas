// Turns the HTML files written by `examples/screenshots.rs` into PNGs.
const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');

(async () => {
  const [src, dst] = process.argv.slice(2);
  const browser = await chromium.launch();
  const page = await browser.newPage({ deviceScaleFactor: 2 });
  for (const f of fs.readdirSync(src).filter((f) => f.endsWith('.html'))) {
    await page.goto('file://' + path.resolve(src, f));
    const term = await page.$('#term');
    await term.screenshot({ path: path.join(dst, f.replace(/\.html$/, '.png')) });
    console.log('wrote', f.replace(/\.html$/, '.png'));
  }
  await browser.close();
})();
