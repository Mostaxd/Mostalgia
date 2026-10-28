# Mostalgia website

The static site published to GitHub Pages by `.github/workflows/pages.yml`
(plain HTML, CSS and JavaScript, no build step). Open `index.html` through any
local server to preview, e.g. `python3 -m http.server -d site`.

- `assets/cast/` — the companions' animated sprites, rendered from the app's own
  artwork code (`crates/mostalgia-app/src/companion/`), transparent, 12 fps.
- `assets/shots/`, `assets/themes/` — screenshots from `docs/screenshots/`, as WebP.

## Regenerating the sprites

```bash
cd site/tools/castdump && cargo run --release -- /tmp/cast-svg
cd .. && node raster.js /tmp/cast-svg /tmp/cast-png   # needs Playwright + Chromium
python3 encode.py /tmp/cast-png ../assets/cast       # needs Pillow
```
