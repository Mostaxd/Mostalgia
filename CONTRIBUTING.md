# Contributing to Mostalgia

Thanks for wanting to help! Bug reports, ideas, translations, themes and code
are all welcome. Questions and early ideas are best in
[Discussions](https://github.com/Mostaxd/Mostalgia/discussions); issues are
for bugs and concrete tasks. If you're new, look for issues labelled
[`good first issue`](https://github.com/Mostaxd/Mostalgia/labels/good%20first%20issue).

Please read the [Code of Conduct](CODE_OF_CONDUCT.md) before you join in.

## Building

Mostalgia targets Linux (PipeWire / PulseAudio) and needs a recent stable Rust
toolchain (edition 2024).

```bash
# System packages (Ubuntu/Debian)
sudo apt install build-essential cmake clang pkg-config libpulse-dev pulseaudio-utils \
  libvulkan-dev glslc libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev \
  libxcb1-dev libfontconfig-dev libfreetype-dev libasound2-dev libzstd-dev

# Models: Whisper, Silero VAD, speaker and search models (the app also
# offers this on first launch, and `mostalgia download-models` does it too)
./scripts/download-model.sh

# Build: drop --features vulkan for a CPU-only build, or use --features cuda
cargo build --release --features vulkan

# Run the desktop app or the CLI
cargo run --release --features vulkan -p mostalgia-app
cargo run --release --features vulkan -p mostalgia -- --help
```

The workspace has three crates:

- `crates/mostalgia-core`: capture, transcription, storage, summaries, *Ask*.
- `crates/mostalgia-app`: the desktop app (GPUI + gpui-component).
- `crates/mostalgia-cli`: the `mostalgia` command.

`vendor/gpui-component` is a patched copy of an upstream crate; keep changes
there small and marked with a `Mostalgia:` comment. The website lives in
`site/` (plain HTML, CSS and JavaScript, see [`site/README.md`](site/README.md)).

## Before you open a pull request

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

If you change something visible in the app, add a screenshot to the pull
request.

## Pull request flow

1. Fork the repository (or create a branch, if you have access).
2. Name the branch after what it changes:
   - `feature/...` for new features, e.g. `feature/meeting-templates`
   - `fix/...` for bug fixes, e.g. `fix/speaker-matching`
   - `docs/...` for documentation only, e.g. `docs/install-fedora`
3. Keep each pull request to one change, with commit messages that say what
   changed and why.
4. Open the pull request against `main` and fill in the template.
5. A maintainer reviews it; small follow-up commits are fine, we squash or
   merge when it's ready.

For bigger changes (a new provider, a new crate, a redesign), open an issue
first so we can agree on the approach before you spend time on it.

## Licensing of contributions

Mostalgia is dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Unless you explicitly state
otherwise, any contribution you intentionally submit for inclusion in
Mostalgia, as defined in the Apache-2.0 license, is dual-licensed as above,
without any additional terms or conditions.
