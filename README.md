# mostalgia

Local meeting recorder for Linux: records your microphone and system audio
(both sides of a call), prints a live German/English transcript with
Whisper large-v3-turbo, then makes a clean final transcript, a Claude
summary (key points, decisions, action items) and keeps everything in a
searchable library.

Pure Rust, fully local transcription (only the optional summary calls the
Claude API). Built for PipeWire (via its PulseAudio compatibility layer),
so it works on any modern distro.

## Status

Done: dual capture with live per-source transcription, final pass,
summaries, SQLite library with full-text search, and the desktop app.
Next: speaker diarization, noise suppression, tray + shortcuts, export.

![Recording](docs/screenshots/2-recording-hell.png)

## Layout

A Cargo workspace:

- `crates/mostalgia-core` — capture, VAD, live chunking, Whisper, final
  pass, storage, summaries. Front ends drive a `Recorder` and consume its
  `Event` channel (transcript lines, levels, timings).
- `crates/mostalgia-cli` — the `mostalgia` command.
- `crates/mostalgia-app` — the desktop app (GPUI + gpui-component).

## Build

System packages (Ubuntu/Debian):

```bash
sudo apt install build-essential cmake clang libpulse-dev pkg-config
```

Pick a backend:

```bash
# CPU only (works everywhere, slow for large models)
cargo build --release

# Vulkan — recommended: works today on NVIDIA/AMD/Intel without extra toolkits.
# Needs the Vulkan SDK headers: sudo apt install libvulkan-dev glslc
cargo build --release --features vulkan

# CUDA (NVIDIA, fastest; needs the CUDA toolkit installed)
cargo build --release --features cuda
```

The binary lands in `target/release/mostalgia`. On hybrid laptops mostalgia
picks the discrete GPU automatically.

## Model

```bash
./scripts/download-model.sh   # fetches ggml-large-v3-turbo.bin (~1.6 GB) and the Silero
                              # VAD model ggml-silero-v5.1.2.bin (~1 MB)
```

Models go to `~/.local/share/mostalgia/models/` (a `models/` folder in the
current directory is used first, if present).

Any ggml Whisper model works via `--model`; `ggml-base.bin` is handy for
quick tests.

## Desktop app

```bash
# Extra packages for GPUI (windowing, fonts, keyboard):
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev \
  libxcb1-dev libfontconfig-dev libfreetype-dev libasound2-dev libzstd-dev

cargo build --release -p mostalgia-app --features vulkan
./target/release/mostalgia-app
```

To get it in the GNOME app grid: copy `target/release/mostalgia-app` to
`~/.local/bin/` and `packaging/mostalgia.desktop` to
`~/.local/share/applications/`.

- **Library** (sidebar): meetings grouped by day, full-text search across
  all transcripts.
- **Recording**: mic / desktop toggles, language, live level meters,
  streaming transcript with speaker chips (auto-scroll pauses when you
  scroll up), bookmarks ("Markieren"), pause, stop → final pass → saved.
- **Meeting**: summary (generated automatically if a key is set),
  transcript (click a timestamp to play from there, click a speaker chip to
  rename, pencil to correct a line, title editable), audio waveform per
  track (click to play from that point).
- **Settings**: grouped into categories (Aufnahme, Sprecher, KI &
  Zusammenfassung, Darstellung, Speicher & Export, Tastenkürzel & System) in
  a list on the left, each shown as a card grid that uses two columns when
  there's room; a search box finds settings across all categories, and the
  last category is remembered. Includes devices with a 3-second level test,
  language, VAD sensitivity, speaker separation strength, provider/model/
  thinking level and API keys, themes and fonts, the data folder, and a
  one-click GNOME shortcut setup. Stored in `~/.config/mostalgia/config.json`, which the CLI also
  reads for the VAD sensitivity.

### Managing meetings

Right-click a meeting in the sidebar: **Rename**, **Export** (Markdown,
text, SRT, Word) or **Delete…**. Ctrl-click / Shift-click selects several,
and the Delete key deletes the selection. Deleting asks first and removes,
in one transaction, the meeting, its transcript and keyword index rows, the
summary, bookmarks, speaker names and the "Ask" passages and vectors — and
by default its audio files (untick "Also delete the audio files" to keep
them). `mostalgia delete <id> [--keep-audio]` does the same.

![Delete](docs/screenshots/delete-dialog-en.png)

### Language

Einstellungen → Darstellung → "Sprache der App": System, Deutsch or
English, switched live. Summaries and transcripts stay in the meeting's own
language.

### Tray, shortcut, export

- **Tray icon**: start/stop recording, show the window, quit. GNOME needs
  the AppIndicator extension enabled:
  `gnome-extensions enable ubuntu-appindicators@ubuntu.com`.
- **Keyboard shortcut**: `mostalgia toggle` starts or stops recording in the
  running app (and launches it if needed); `mostalgia open` brings it to the
  front. Wayland doesn't let apps grab global hotkeys, so bind it in GNOME:
  `./scripts/install-gnome-shortcut.sh` (Super+Shift+R by default, or pass
  your own like `'<Ctrl><Alt>m'`).
- **Export**: "Exportieren" in the meeting view, or
  `mostalgia export <id> --format md|txt|srt|docx [-o file]` — summary plus
  transcript as Markdown, text, SRT subtitles or Word.

### Themes

Einstellungen → Design switches live between 19 themes: **System**
(follows GNOME's light/dark), **Hell**, **Dunkel**, **Blockwelt** (blocky
pixel look: square corners, chunky borders, Pixelify Sans), **Synthwave**,
**Nord**, **Solarized**, **Terminal** (green CRT, VT323), **Papier**
(sepia), **Hoher Kontrast**, **Ozean**, **Wald**, **Nachtschatten**,
**Kaffee**, **Sakura**, **Comic** (bold outlines, bright colours),
**Aurora**, **Retro 90er** and **Weltraum**. The choice is saved in
`config.json`.

![All themes](docs/screenshots/themes.png)

**Animations**: a pulsing record dot, smooth level meters, new lines that
fade/slide in (a bouncier pop-in in playful themes like Comic, Blockwelt,
Retro 90er, Weltraum), breathing provisional text, and a small original
animated scene behind the empty transcript for many themes (Synthwave sun
and moving grid, Ozean waves, Wald leaves, Kaffee steam, Sakura petals,
Comic thought bubble, Aurora ribbons, Weltraum stars and planet, hopping
grass blocks in Blockwelt, plus a pixel burst when you press Beenden). They
are cheap (≤ 30 fps, only while nothing is transcribed yet), off in Hoher
Kontrast, and "Animationen reduzieren" in Darstellung turns them all off.

![Scenes](docs/screenshots/themes-scenes.png)

Themes are data: colours, corner radius, border width, fonts and the
speaker-chip palette, in [`assets/themes.json`](crates/mostalgia-app/assets/themes.json).
Add your own by dropping a JSON file (one theme or a list, same fields) into
`~/.config/mostalgia/themes/`; a theme with a built-in id replaces it.

**Fonts** (all bundled, SIL OFL): Inter, IBM Plex Sans and Atkinson
Hyperlegible to choose from under Darstellung → Schriftart; JetBrains Mono
for the timer and timestamps; Pixelify Sans and VT323 for the pixel and
terminal themes. Noto Sans Arabic is the fallback for every UI font, so
Arabic titles and transcripts render properly in every theme. Right-to-left
text is laid out word by word in a right-to-left flow (embedded English or
numbers keep their own order), since gpui lays words out left-to-right;
RTL summaries use a small built-in Markdown renderer for the same reason.

| | |
|---|---|
| ![Arabic summary](docs/screenshots/arabic-summary-hell.png) | ![Arabic transcript, dark](docs/screenshots/arabic-transcript-dunkel.png) |

| | |
|---|---|
| ![New recording](docs/screenshots/1-new-recording-hell.png) | ![Summary](docs/screenshots/3-summary-hell.png) |
| ![Transcript](docs/screenshots/4-transcript-hell.png) | ![Settings](docs/screenshots/settings-aufnahme-hell.png) |
| ![Recording, dark](docs/screenshots/2-recording-dunkel.png) | ![Audio, dark](docs/screenshots/5-audio-dunkel.png) |

Screens can be re-rendered offscreen (no display needed) with
`cargo run -p mostalgia-app --example screenshots --features screenshots -- <out-dir> <meeting-id>`
(the README's come from a demo library: `MOSTALGIA_DATA_DIR=<dir>` points
mostalgia at a separate data folder).

## Run (CLI)

```bash
# Record mic + system audio with a live German transcript. Ctrl+C stops;
# then the final pass runs, the meeting is saved and summarized.
./target/release/mostalgia start

# English, or let Whisper detect the language per chunk
./target/release/mostalgia start --language en
./target/release/mostalgia start --language auto

# Only one side
./target/release/mostalgia start --source mic      # just your microphone
./target/release/mostalgia start --source system   # just system audio (the call)

# Label live lines by who spoke: "Ich" (mic) vs "Andere" (system audio).
# The saved transcript is always labelled.
./target/release/mostalgia start --speakers

# Live transcript only, no final pass / library / summary
./target/release/mostalgia start --no-final
```

Tracks land in `~/.local/share/mostalgia/recordings/session-<unix-ts>/` as
`mic.wav` and `system.wav` (16 kHz mono, which is what Whisper consumes;
`--out` picks another folder).

### Library

```bash
./target/release/mostalgia list                 # meetings, newest first (✓ = summarized)
./target/release/mostalgia show 3               # summary + transcript
./target/release/mostalgia show 3 --transcript  # just the transcript
./target/release/mostalgia search budget frist  # full-text search (prefix, accent-insensitive)
./target/release/mostalgia summarize 3          # (re)generate the summary
./target/release/mostalgia delete 3             # remove from the library (WAVs are kept)

# Add an existing recording folder (runs the final pass)
./target/release/mostalgia import ~/.local/share/mostalgia/recordings/session-1793124144

# Transcribe any 16 kHz mono WAV
./target/release/mostalgia transcribe some.wav
```

The library is a SQLite database at `~/.local/share/mostalgia/mostalgia.db`
(meetings, transcript segments with an FTS5 index, summaries).

### Summaries

After the final pass, an LLM writes a summary in the meeting's language: a
title, a short overview, key points, decisions, action items with owners
and deadlines, and open questions. Only the transcript text is sent — never
audio. Choose the provider, model and thinking level in Einstellungen →
Zusammenfassungen, or per run:

- **Claude** (default `claude-opus-5-5`, thinking level `high`): the model
  list comes live from the Anthropic API (`mostalgia models`), with a built-in
  fallback. "Denktiefe" maps to `output_config.effort` (low … max) and only
  shows for models that support it. Responses are streamed, so high levels
  (Opus 5.5 at `max` took ~2 minutes on a short meeting) don't time out.
- **DeepSeek** (`deepseek-chat`, `deepseek-reasoner`; OpenAI-compatible API),
  no thinking-level control.

```bash
./target/release/mostalgia set-api-key                       # Anthropic key → GNOME keyring
./target/release/mostalgia set-api-key --provider deepseek   # DeepSeek key
./target/release/mostalgia summarize 3 --summary-model claude-sonnet-5-5 --effort low
./target/release/mostalgia summarize 3 --provider deepseek
./target/release/mostalgia models [--provider deepseek]      # available models + thinking levels
```

`ANTHROPIC_API_KEY` / `DEEPSEEK_API_KEY` take precedence over the keyring.
Without a key everything else still works; `mostalgia summarize <id>` adds
the summary later. `--no-summary` skips it for one run. The meeting view
shows which provider, model and thinking level wrote each summary.

### Choosing devices

By default the mic is the Pulse default source and system audio is
`@DEFAULT_MONITOR@` (whatever plays on the default output). To pick
something else, list them and pass the name to `--mic-device` /
`--system-device`:

```bash
./target/release/mostalgia devices
```

Quick no-code check that monitor capture works on your system:

```bash
parec -d @DEFAULT_MONITOR@ | aplay
```

## Ask your meetings

"Fragen" in the sidebar (Ctrl+K), or `mostalgia ask "Was haben wir über das Budget entschieden?"`:
Claude answers questions across all your meetings, with a clickable source
for every claim (opens the meeting at that line and plays from there), and
says so when the transcripts don't contain the answer.

![Ask your meetings](docs/screenshots/ask-hell.png)

- **Search (local):** transcripts are split into ~90 s passages (15 s
  overlap) plus summary sections and action items, and indexed two ways —
  SQLite FTS5 (BM25) and multilingual-e5-small embeddings (candle, CPU) —
  merged with Reciprocal Rank Fusion (k=60), top 15. Date/meeting filters run
  in SQL first. Without the embedding model it falls back to BM25 only.
- **Answering:** Claude with three tools (search_meetings, list_meetings,
  read_transcript), up to 6 rounds, streamed; thinking level `ask_effort`
  (default medium). DeepSeek via its tool-calling API (untested). Only the
  passages Claude looks up are sent — never audio or the whole library.
- **Indexing:** after each final pass and summary, plus a one-time
  background backfill; `mostalgia reindex [--all]`.
- **Measured** (Core Ultra 7 255HX): indexing ~30–150 ms per passage (a whole
  8-meeting library in 3.7 s), search 23–30 ms, answers ~5 s with Opus 5.5 at
  medium.

The e5 model (~490 MB) comes with `scripts/download-model.sh`.

## Auto-start with calls

mostalgia notices when a call app starts using the microphone and offers to
record — Einstellungen → Aufnahme → "Anrufe erkennen": **Aus**, **Fragen**
(in-app banner + desktop notification with "Aufnahme starten"; default) or
**Automatisch** (starts right away). When the call's mic stream has been gone
for 10 s, a recording that auto-start began is stopped and saved.

Recognised: Zoom, Teams, Discord, Slack, Signal, Element, Jitsi, Skype,
Webex, Telegram, WhatsApp, and any microphone capture in Chrome/Chromium/
Brave/Edge/Vivaldi/Opera/Firefox (Meet, Teams web, …). Apps can be ignored
individually; mostalgia's own streams and desktop-audio captures never count.
Detection polls PipeWire's Pulse interface once a second: a call is seen
within ~1 s.

## Telling speakers apart (optional)

`--diarize` (or "Mehrere Sprecher unterscheiden" in the app's settings)
splits the system-audio side — everyone else on the call — into
"Sprecher 1", "Sprecher 2", …:

- **Final pass** (accurate): sherpa-onnx runs pyannote segmentation, a
  WeSpeaker voice embedding per turn, and clustering over the whole
  `system.wav`; each transcript line gets the speaker who talks most during
  it. Adds roughly 5 minutes of CPU per hour of audio.
- **Live** (approximate): each chunk's voice print is matched against the
  voices heard so far, so labels appear as you go and are corrected by the
  final pass.
- Rename speakers once in the app ("Sprecher 2" → "Anna") and every line
  updates.

The models (~33 MB) come with `scripts/download-model.sh`. The clustering
threshold (default 0.5, higher = fewer speakers) can be overridden with
`MOSTALGIA_DIARIZE_THRESHOLD` — very similar voices (e.g. two TTS voices)
need ~0.2. sherpa-onnx's shared libraries are copied next to the binaries
by the build; keep them together when copying the binaries elsewhere.

## Noise suppression (optional)

`--denoise` (or "Rauschunterdrückung (Mikrofon)" in the settings) runs
RNNoise (pure-Rust `nnnoiseless`) on the mic before speech detection and
Whisper — live and in the final pass. The WAV files always stay raw.

Measured on a German test dialog mixed with real fan/room noise from the
Yeti mic (word error rate, lower is better):

| SNR   | raw | denoised |
|-------|-----|----------|
| 20 dB | 9 % | 6 %      |
| 10 dB | 9 % | 7 %      |
| 5 dB  | 9 % | 9 %      |
| 0 dB  | 15 % | 33 %    |

So it helps a little with typical background noise but hurts in very heavy
noise, where RNNoise starts eating speech — Whisper is already robust to
fans. That's why it's off by default.

## How the live transcript works

Whisper is not a streaming model, so the live path chunks audio at
pauses. Mic and system audio are handled as separate streams — each with
its own voice-activity detector and chunker, sharing one Whisper model —
so room noise on the mic never bleeds into the other side's transcript,
and `--speakers` can label every line by its source. The detector marks
each 32 ms frame as speech or not, and utterances are cut after ~0.5 s
without speech (1.5–8 s chunk length; a cut forced by the 8 s limit lands
on the quietest moment of the last 3 s, i.e. between words). Lines print as each chunk
finishes, so the two sides can interleave slightly out of order.
Each chunk is transcribed independently, so text appears a moment after
each pause — about 0.2–0.5 s on a discrete GPU. Stretches without speech
are never sent to Whisper. On Ctrl+C the last partial chunk is flushed
and transcribed.

The detector is Silero VAD, run through whisper.cpp's built-in VAD
(`--vad-model`, default `models/ggml-silero-v5.1.2.bin`). If that model is
missing, mostalgia falls back to an energy gate that tracks the room's noise
floor and only counts frames ~10 dB above it as speech.

Whisper invents text for near-silent audio ("Vielen Dank.", "Thank you.",
"Untertitel …"). Two filters catch what slips past the VAD: segments
Whisper itself rates as likely non-speech (no-speech probability > 0.6)
are dropped, and a quiet chunk whose only output is a known filler phrase
is discarded (logged as "dropped filler hallucination").

The saved WAVs are untouched by any of this.

**Provisional text** ("Vorläufigen Text anzeigen", on by default): while
someone is still talking, the open chunk is re-transcribed about once a
second (only if it grew and the transcriber isn't behind) and shown greyed
out — on one overwritten line in the CLI — then replaced by the final line
when the chunk closes. Measured with 22 s of continuous German speech on
the RTX PRO 3000: first visible text after **~1 s** (before: ~15 s, since
text only appeared after a pause or the old 15 s limit), final lines word
for word in three runs, GPU busy ~15 % on average. Chunks quieter than
−42 dBFS (room noise, earphone bleed) never reach Whisper.
`MOSTALGIA_TIMING=1 mostalgia start` logs when each line becomes visible.

## The final pass

When recording stops, each full track is transcribed again for the saved
transcript: Silero VAD finds the speech regions, each region goes to
Whisper on its own (short ones are merged with their neighbour), and the
filler filter runs once more. Regions are kept short on purpose — given a
long span with pauses, Whisper tends to skip ahead or stop early. Mic and
system lines are then merged by time and labelled "Ich" / "Andere". On a
discrete GPU this takes a second or two per minute of audio.

whisper.cpp's own logging is reduced to warnings and errors; set
`MOSTALGIA_DEBUG=1` to see all of it (model load, GPU devices, VAD timings).
