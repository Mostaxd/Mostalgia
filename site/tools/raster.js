const { chromium } = require('playwright');
const fs = require('fs'), path = require('path');
(async () => {
  const src = process.argv[2], out = process.argv[3];
  const b = await chromium.launch();
  const p = await b.newPage({ deviceScaleFactor: 1.5 });
  for (const ch of fs.readdirSync(src)) {
    fs.mkdirSync(path.join(out, ch), { recursive: true });
    for (const f of fs.readdirSync(path.join(src, ch)).sort()) {
      const svg = fs.readFileSync(path.join(src, ch, f), 'utf8');
      const w = +svg.match(/width="([\d.]+)"/)[1], h = +svg.match(/height="([\d.]+)"/)[1];
      await p.setViewportSize({ width: Math.ceil(w), height: Math.ceil(h) });
      await p.setContent(`<html><body style="margin:0;background:transparent">${svg}</body></html>`);
      await p.locator('svg').screenshot({ path: path.join(out, ch, f.replace('.svg', '.png')), omitBackground: true });
    }
    console.log(ch);
  }
  await b.close();
})();
