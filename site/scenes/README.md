# Episode scenes

Each episode's animated scene lives in its own pair of files, `epN.css` and
`epN.js`, loaded from inside that episode's `<section>` in `index.html`:

```html
<link rel="stylesheet" href="scenes/ep5.css">
<script src="scenes/ep5.js" defer></script>
```

Deferred scripts run after `main.js`, so a scene can use the shared helpers
from `window.MostalgiaSite`: `reduce` (prefers-reduced-motion), `$`, `$$`,
`sprite(character, pose)`, `type(el, text)` (typewriter) and `preload(src)`.

A scene starts when it scrolls into view, pauses when it leaves, has a
Replay button, shows its final state when motion is reduced, and keeps its
content in the HTML so the page reads fine without JavaScript.
