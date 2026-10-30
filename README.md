# Ghostcaret

Ghostcaret suggests the rest of what you're typing, in the apps you already use. The suggestion shows up as faint text at your cursor. Tab takes the next word, ` takes the whole suggestion, Alt+→ shows more, and Esc dismisses it. You choose how long suggestions get, from one word up to a paragraph.

It runs a model on your own machine (any GGUF model, through llama.cpp) or uses your own API key for an OpenAI-compatible endpoint, Anthropic or Gemini. There's no account and no telemetry, and password fields are never read.

It can also draft a reply to the conversation on screen, or rewrite text you've selected. You see the text before it goes in, and it never sends anything for you.

## Status

In development. There's no release yet, but it runs end to end on my machine: niri 26.04, with a local model on the CPU. The first measurements are in [docs/quality.md](docs/quality.md#where-the-build-stands), and some of them still miss their targets.

Linux comes first. On Wayland, Ghostcaret is itself the input method (`zwp_input_method_v2`), so other compositors with that protocol should work too, but only niri has been tested. Firefox works as it is; Chrome needs `--enable-wayland-ime --wayland-text-input-version=3`. Windows is built and waiting for its first test on a real Windows machine; macOS comes later.

## Build from source (Linux)

You need Rust, [bun](https://bun.sh), CMake and a C++ compiler, plus the GTK/WebKit libraries Tauri uses:

- Arch: `webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg libxkbcommon wayland fontconfig`
- Debian/Ubuntu: `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxkbcommon-dev libwayland-dev libfontconfig-dev libdbus-1-dev`

Then:

```sh
cd app && bun install && bun run build && cd ..
cargo run --release -p ghostcaret
```

The first run walks you through picking a local model (it downloads and checks it) or adding an API key. Stop fcitx5 or IBus first: only one input method can be active at a time. Bug reports and app notes go in [Discussions](https://github.com/saatvik333/ghostcaret/discussions).

Most of the code is written with AI coding agents under my direction.

## Docs

- [Product spec](docs/product.md): what it does, the controls and the defaults
- [Architecture](docs/architecture.md): how it's built
- [Quality bar](docs/quality.md): the targets every release has to meet

## License

[MPL-2.0](LICENSE)
