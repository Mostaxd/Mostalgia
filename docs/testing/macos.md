# Mostalgia on Mac: test guide

Thanks for testing! This is an early Mac build, and your results decide what gets fixed before release.

## What you need

- A Mac with **Apple Silicon** (M1 or newer). Intel Macs aren't supported yet.
- **macOS 13 or later**; 14.2 or later works best.
- About **3 GB** of free space.
- Headphones.

## 1. Install

1. Download [**Mostalgia-macOS-arm64.dmg**](https://github.com/Mostaxd/Mostalgia/releases/latest/download/Mostalgia-macOS-arm64.dmg) from the latest release.
2. Open the DMG and drag **Mostalgia** into **Applications**.
3. The app isn't signed by Apple yet, so macOS blocks it at first. Open it once, then go to **System Settings → Privacy & Security** and click **Open Anyway** near the bottom.
4. If macOS says the app is "damaged", run this in Terminal, then open it again:

   ```bash
   xattr -dr com.apple.quarantine /Applications/Mostalgia.app
   ```

## 2. First launch

1. Start the app from **Terminal** so you can see its log, and keep the window open while testing:

   ```bash
   /Applications/Mostalgia.app/Contents/MacOS/mostalgia-app
   ```

2. The app offers to **download the speech models** (about 2 GB, once). Accept and wait until it finishes.
3. *Optional:* summaries, Ask and translation need an AI. Add one in **Settings → AI & transcription**, either with an API key or Ollama for fully local use.

## 3. Recording (most important)

1. Click **New recording** with both **Microphone** and **Desktop audio** switched on.
2. macOS asks for **Microphone** access, then for **audio recording** or **Screen Recording** access. Allow both.
   - On macOS 13 to 14.1 you may have to switch Mostalgia on under **System Settings → Privacy & Security → Screen Recording**, then restart the app.
3. ⏱ **Note how long it takes** from clicking record until the timer starts running. The Terminal prints a line containing `system audio: … took`; copy it.
4. Play a YouTube video and talk yourself at the same time for about 2 minutes. Lines should appear live, labelled **Me** and **Others**.
5. During the recording:
   - plug in or unplug headphones,
   - switch **Desktop audio** off and on again,
   - press **Pause**, then continue.
6. Press **Stop** and wait for the final pass. Then open the meeting and check:
   - Playback works, and seeking works.
   - Both your voice and the video are there, and they're in sync.
   - **Transcribe again** works.

## 4. Calls

1. In **Settings**, turn on starting a recording when a call starts, set to **ask**.
2. Join a call: Zoom, Teams, Google Meet (in Chrome or Safari) or FaceTime.
3. A notification should appear. Its **Start recording** button should start a recording.

## 5. Mori and live captions

1. **Settings → Companion**: put Mori **on the desktop**. Check that she:
   - stays on top of everything, including full-screen apps and other Spaces,
   - can be dragged,
   - lets clicks next to her reach the window behind.
2. During a recording, turn on **Live captions**. Check that the window:
   - floats on top,
   - doesn't take focus away from your call app,
   - can be resized.

## 6. Everything else

- **Menu bar icon:** the menu and its actions work, and it shows a dot while recording.
- **⌃⌥⇧R** (Control+Option+Shift+R) starts and stops recording from anywhere.
- **Start at login:** turn it on in Settings, log out and back in, and check that the app starts.
- **Cmd shortcuts** in the app (e.g. Cmd+, for Settings).
- *If you set up an AI:* create a summary, ask a question in **Ask**, and press **Translate** during a recording.

## What to send back

- For each step: ✅ works, or ❌ with a short description and a screenshot.
- The **Terminal output**, especially the `took` line and any errors.
- Your **macOS version**, **Mac model** and **which call apps** you tried.

The most important results are how long a recording takes to start, whether both tracks stay in sync, and whether clicks pass through next to Mori.
