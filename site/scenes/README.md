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

## Pacing (chapters, captions, controls)

`pacing.js` + `pacing.css` (loaded once from the page `<head>`) add
`MostalgiaSite.timeline(scene, chapters, opts)`. A scene that uses it no longer
runs its own clock: its `reset()` rewinds the DOM and returns its beats as
`[{ time, fn(late) }]` in scene seconds, and the timeline plays them 1.5x
slower with a hold after each chapter, dims the parts a chapter is not about,
shows a one-line caption, and adds Pause/Play plus clickable chapters (a jump
rewinds and fast-forwards through earlier beats). The `<li>`s under the scene
become the chapter buttons. Episodes 1 and 4 use it; see `ep1.js` for the
options (`bar`, `ctlBefore`, `parts`, `replay`, `end`, `reset`, `tick`).
