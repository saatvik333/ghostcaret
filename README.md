# Ghostcaret

Ghostcaret suggests the rest of what you're typing, in the apps you already use. The suggestion shows up as faint text at your cursor. Tab takes the next word, ` takes the whole suggestion, Alt+→ shows more, and Esc dismisses it. You choose how long suggestions get, from one word up to a paragraph.

It runs a model on your own machine (any GGUF model, through llama.cpp) or uses your own API key for an OpenAI-compatible endpoint, Anthropic or Gemini. There's no account and no telemetry, and password fields are never read.

It can also draft a reply to the conversation on screen, or rewrite text you've selected. You see the text before it goes in, and it never sends anything for you.

## Status

In development. Nothing is released yet and there's nothing to install.

Linux comes first, starting with niri. On Wayland, Ghostcaret is itself the input method (`zwp_input_method_v2`), so compositors that support that protocol should work too, but only niri has been tested. Windows is being built alongside it; macOS comes later.

Most of the code is written with AI coding agents under my direction.

## Docs

- [Product spec](docs/product.md): what it does, the controls and the defaults
- [Architecture](docs/architecture.md): how it's built
- [Quality bar](docs/quality.md): the targets every release has to meet

## License

[MPL-2.0](LICENSE)
