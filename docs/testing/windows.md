# Mostalgia on Windows: test guide

Thanks for testing! This is an early Windows build, and your results decide what gets fixed before release.

## What you need

- **Windows 10 or 11**, 64-bit.
- About **3 GB** of free space.
- Headphones.
- Ideally an **NVIDIA or AMD** graphics card for fast transcription. Without one it works too, just slower.

## 1. Install

1. Download [**Mostalgia-windows-x86_64.zip**](https://github.com/Mostaxd/Mostalgia/releases/latest/download/Mostalgia-windows-x86_64.zip) from the latest release.
2. Unzip it into a folder of your own, for example `C:\Users\<you>\Mostalgia`.
   - **Not** into Program Files: updates need to write to this folder.
3. Run **mostalgia-app.exe**. If **"Windows protected your PC"** appears, click **More info → Run anyway**.

## 2. First launch

1. The app offers to **download the speech models** (about 2 GB, once). Accept and wait until it finishes.
2. Check that the status says **"Nutze GPU: …"** with your graphics card's name. If a banner about the graphics driver appears, take a screenshot.
3. Check that a **Mostalgia** entry appeared in the **Start Menu**.
4. *Optional:* summaries, Ask and translation need an AI. Add one in **Settings → AI & transcription**, either with an API key or Ollama for fully local use.

## 3. Recording (most important)

1. In **Settings → Recording**, pick your microphone and your audio output, and press **Test** for each. The level bars should move.
2. Click **New recording** with both **Microphone** and **Desktop audio** switched on. If Windows asks for microphone access, allow it.
3. Play a YouTube video and talk yourself at the same time. Also stay silent for about 30 seconds with nothing playing. Lines should appear live, labelled **Me** and **Others**.
4. During the recording:
   - switch **Desktop audio** off and on again,
   - press **Pause**, then continue.
5. Press **Stop** and wait for the final pass. Then open the meeting and check:
   - Playback works, and seeking works.
   - Your voice and the video are in sync, also after the silent part.

## 4. Calls and notifications

1. In **Settings**, turn on starting a recording when a call starts, set to **ask**.
2. Join a call in Teams, Zoom or a browser (Google Meet).
3. A notification **from "Mostalgia"**, with its icon, should appear. Its **Start recording** button should start a recording.

## 5. Mori and live captions

1. **Settings → Companion**: put Mori **on the desktop**. Check that she:
   - stays on top of other windows,
   - doesn't show up in the taskbar,
   - can be dragged,
   - lets clicks next to her reach what's behind.
2. During a recording, turn on **Live captions**. Check that the window stays on top and doesn't take focus away from your call.

## 6. Everything else

- **Tray icon:** the menu and its actions work, and it shows a red dot while recording.
- **Ctrl+Alt+Shift+R** starts and stops recording from anywhere.
- **Start with Windows:** turn it on in Settings and restart the PC. The app should start minimized.
- **Command line:** open a terminal in the app folder and run this twice, once while the app is running and once while it's closed:

  ```powershell
  .\mostalgia.exe toggle
  ```

- *If you set up an AI:* create a summary, ask a question in **Ask**, and press **Translate** during a recording.

## What to send back

- For each step: ✅ works, or ❌ with a short description and a screenshot.
- Your **Windows version**, **graphics card** and **which call apps** you tried.

The most important results are whether both tracks stay in sync (especially after the silent part), whether the GPU is used, and whether clicks pass through next to Mori.
