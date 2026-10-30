// Renders assets/brand/banner.html to assets/brand/banner.png (1280 × 640,
// drawn at 2× so it stays sharp in the README). Needs Playwright with
// Chromium, and pngquant to compress it: `node scripts/render-banner.js`.
const path = require('path');
const { chromium } = require('playwright');

(async () => {
  const root = path.resolve(__dirname, '..');
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 640 }, deviceScaleFactor: 2 });
  await page.goto('file://' + path.join(root, 'assets/brand/banner.html'), { waitUntil: 'networkidle' });
  await page.evaluate(() => document.fonts.ready);
  const out = path.join(root, 'assets/brand/banner.png');
  await page.screenshot({ path: out, clip: { x: 0, y: 0, width: 1280, height: 640 } });
  await browser.close();
  // GitHub's social preview takes at most 1 MB; pngquant keeps it well under.
  try {
    require('child_process').execFileSync('pngquant', ['--quality', '82-96', '--speed', '1', '--strip', '--force', '--output', out, out]);
  } catch (e) {
    console.warn('pngquant not found or failed; banner.png is uncompressed (over 1 MB)');
  }
})();
