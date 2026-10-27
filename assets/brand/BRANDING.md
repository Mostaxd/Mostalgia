# Mostalgia — brand guide

> **Nostalgia, but for meetings you can actually recall in full.**
> *Nostalgie – aber für Meetings, an die du dich wirklich vollständig erinnerst.*

## The name

**Mosta** + **nostalgia**. Nostalgia is remembering the good parts and
forgetting the rest; Mostalgia remembers *everything* — every word, who said
it and when — and still keeps the warm feeling. The wordmark splits it
"Most|algia": *most* of your meeting, finally recalled.

The motif is a **cassette tape whose window shows a waveform**: the
old-fashioned way of keeping a voice forever, now with live transcription
playing inside.

## Palette

| Name        | Hex       | Use |
|-------------|-----------|-----|
| Sunset Amber | `#F2A541` | Highlights, the "algia" in dark contexts, dark-theme accent |
| Amber Light | `#F7C26B` | Top of the icon gradient |
| Terracotta  | `#D9653B` | Icon gradient, label stripe, "algia" on light backgrounds |
| Ember       | `#B8472A` | Bottom of the icon gradient, light-theme accent (buttons, focus) |
| Cream       | `#FBF3E4` | Cassette, text on dark backgrounds |
| Cream Deep  | `#F0DFC2` | Cassette foot, light borders |
| Plum        | `#5B2A4E` | "Most" in the wordmark, headings on light backgrounds |
| Night Plum  | `#2B1730` | Cassette window, dark backgrounds, body text on light |
| Soft Sage   | `#9DB8A0` | Calm secondary accent (success, small print on dark) |

Themes built from it live in [`theme-mostalgia.json`](theme-mostalgia.json):
**Mostalgia** (light) and **Mostalgia Nacht / Night** (dark). Contrast (WCAG):

| Pair | Mostalgia | Mostalgia Nacht |
|------|-----------|-----------------|
| text / background | 15.3 : 1 | 16.4 : 1 |
| muted text / background | 6.6 : 1 | 9.2 : 1 |
| muted text / sidebar | 5.8 : 1 | 9.6 : 1 |
| button text / accent | 5.0 : 1 | 8.1 : 1 |
| accent / background | 4.9 : 1 | 8.8 : 1 |
| weakest speaker colour / background | 4.7 : 1 | 7.0 : 1 |

All pass AA for normal text (≥ 4.5 : 1).

## Typography

- **Inter** — UI and wordmark. The wordmark is Inter Bold, tracking −2 %,
  converted to outlines (renders identically everywhere).
- **JetBrains Mono** — timestamps, timers, numbers that should line up.
- Both are SIL OFL and bundled with the app.

## Logo and icon

| File | What |
|------|------|
| `icon.svg` | App icon, 512 × 512 (detailed: waveform in the window) |
| `icon-small.svg` | Bolder variant for 16–32 px (bigger cassette, no waveform) |
| `icon-symbolic.svg` | One-colour 16 × 16 silhouette for the GNOME top bar / tray |
| `icons/hicolor/…` | Ready for `~/.local/share/icons` (16–512 px PNG, scalable, symbolic) as `io.github.mostaxd.Mostalgia` |
| `wordmark.svg` / `.png` | Icon + "Mostalgia" for light backgrounds |
| `wordmark-dark.svg` / `.png` | The same for dark backgrounds |
| `banner.svg` / `.png` | 1280 × 640 social preview |
| `favicon.svg`, `favicon.ico` | 16 / 32 / 48 |

- **Clear space:** keep at least ¼ of the icon's width free around it.
- Don't recolour the icon, rotate it, or put the light wordmark on busy
  photos; use the dark wordmark on Night Plum or darker.
- Below 32 px always use `icon-small.svg` (the hicolor PNGs already do).

Regenerate everything with `python3 generate.py` (needs fontTools), then
export PNGs with [`resvg`](https://github.com/linebender/resvg):
`resvg -w 512 icon.svg icon-512.png`.

## Voice & tone

Warm, witty, never cheesy. A good colleague with a great memory — not a
mascot.

- **Short.** One idea per line. The joke is a seasoning, not the meal.
- **Helpful first.** Errors say what happened and what to do; wit only where
  nothing is at stake.
- **Memory, not surveillance.** Talk about remembering and recalling, never
  "monitoring" or "capturing everyone".
- **German is not a translation.** Write each language so it sounds native.

## Microcopy

| Moment | Deutsch | English |
|--------|---------|---------|
| Empty library | Noch keine Erinnerungen. Zeit, welche zu machen. | No memories yet. Let's make some. |
| Waiting for speech | Bereit, wenn ihr es seid. Der erste Satz landet hier. | Ready when you are. The first word lands here. |
| Recording saved | Gespeichert. Diese Besprechung vergisst du nicht mehr. | Saved. This meeting is now unforgettable. |
| Summary ready | Das Wichtigste, frisch aufgebrüht. | The highlights, freshly brewed. |
| Summary failed | Die Zusammenfassung hat sich verhaspelt. Noch mal versuchen? | The summary lost its train of thought. Try again? |
| Ask — empty | Frag deine Besprechungen. Sie erinnern sich an alles. | Ask your meetings. They remember everything. |
| Ask — no answer | Dazu wurde nichts gesagt – zumindest nicht, als Mostalgia zugehört hat. | Nobody said that — at least not while Mostalgia was listening. |
| Delete confirm | Diese Besprechung endgültig vergessen? | Forget this meeting for good? |
| Mic silent | Dein Mikrofon ist ganz still. Stummgeschaltet – oder nur nachdenklich? | Your mic has gone quiet. Muted — or just thinking? |
| Loading model | Erinnerungen werden geladen … | Loading memories … |
| About | Mostalgia hört zu, schreibt mit und vergisst nichts – alles auf deinem Rechner. | Mostalgia listens, takes notes and forgets nothing — all on your own machine. |
