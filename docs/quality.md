# Ghostcaret quality bar

Status: draft, 2026-10-30. This is the threshold every release must meet. [product.md](product.md) says what the product does and [architecture.md](architecture.md) says how it is built. This document says how good each part has to be, and how that is proven.

Ghostcaret aims to be the best predictive-typing tool on every platform it ships on. That means faster, more accurate, lighter on the machine and more private than the alternatives. "Best" is something we measure, never just claim:

- Every target below is a number with a way to check it.
- Every comparison with another product comes from a published, repeatable test.
- A release that misses a gate does not ship. Fixing the cause, or changing a target with the reason written into this file, comes first.

The numbers are first targets. The engine and overlay spikes either confirm them or replace them with measured ones, and the reason for any change is recorded here.

## Reference machines

Targets are measured on fixed reference machines so that results are comparable between releases:

| Platform | Reference class |
| --- | --- |
| macOS | Apple M1 with 16 GB |
| Linux and Windows | A 16 GB laptop with an integrated GPU (to be chosen; see open questions) |
| Android | A mid-range phone from the last two years (to be chosen) |
| iOS | A recent iPhone with cloud provider only |

## Responsiveness

| Measure | Target | Why | How it is checked |
| --- | --- | --- | --- |
| Time spent in the key handler | 50 µs or less at the 99th percentile | The handler sits in front of every keystroke. macOS disables event taps whose callbacks are slow | Timestamps around the handler in a release build, under load with the model busy |
| Last keystroke to visible suggestion, local model, warm | 100 ms or less at the median, 250 ms or less at the 95th percentile | About 100 ms is where a response feels instant ([Nielsen](https://www.nngroup.com/articles/response-times-3-important-limits/)). The average gap between keystrokes is about 240 ms ([Dhakal et al., CHI 2018](https://aaltodoc.aalto.fi/bitstreams/c175f9bb-8c83-40bc-8969-e179f4a3a6e6/download)), so a suggestion should land within a normal pause | Benchmark replay plus on-device traces |
| Ghostcaret's own overhead per request (read field, filter, build prompt, render) | 20 ms or less at the 95th percentile, local or cloud | Provider speed is outside our control; our own share is not | Traces split by stage |
| Take word, take all, show more (from the buffer) | Next frame, 16 ms or less at 60 Hz | These must feel like typing | Frame timing on the overlay |
| Reading a field | 5 ms or less at the 95th percentile in native apps. Slow apps get a timeout and the last good reading, never a blocked keystroke | Electron and web apps can answer accessibility queries slowly | Per-app traces in the app checklist |
| Reply & rewrite | First streamed text in the card as soon as the model produces it; our overhead 50 ms or less | Drafts are longer; visible progress matters more than total time | Traces |

## Resource use

| Measure | Target | How it is checked |
| --- | --- | --- |
| CPU while not typing | Effectively zero: no polling timers, only OS events. 1 wakeup per second or fewer on average | macOS Activity Monitor idle wake-ups, `powertop` on Linux, Windows Performance Recorder |
| Energy while typing | Reported per 1,000 suggestions on each reference machine. No release may be more than 10% worse than the previous one | Energy counters on the reference machines (powermetrics, RAPL, Windows energy estimation) |
| Memory, model not loaded, settings closed | 100 MB or less across all of Ghostcaret's processes | RSS of all processes |
| Memory, standard model loaded | Model file size plus 500 MB or less (KV cache at the default context, buffers) | RSS after warm-up and 1,000 suggestions; no growth across a 24-hour soak test |
| Memory, small model loaded | 1.5 GB or less in total | Same |
| Installer size, models excluded | 40 MB or less. Large GPU runtimes such as CUDA are optional downloads | Release artifact sizes |
| Start to tray ready | 500 ms or less | Launch trace |
| Model ready after first use | 3 s or less from an SSD (memory-mapped) | Launch trace |
| Network with no cloud provider configured | Zero bytes, apart from the update check (which can be turned off) and model downloads the user started | Automated test under a network monitor |
| Size of a cloud autocomplete request | Bounded by the documented segment caps, and never more than those caps allow | Unit tests on the request builder; the "What was sent" view |

## Suggestion quality

The [benchmark](#benchmark) measures:

- Keystroke savings: the share of keystrokes the user no longer has to type, counting accept keys as keystrokes.
- Wrong-suggestion rate: the share of shown suggestions whose first word is not what the user typed next.
- Acceptance rate and average accepted characters per suggestion.
- Time to suggestion, as defined above.

Default configuration thresholds:

1. Keystroke savings must beat every comparator that can run in the harness, at an equal or lower wrong-suggestion rate. The harness comparators are the local models published by the open-source alternatives (for example the models [Cotabby](https://github.com/FuJacob/cotabby) lists) and the previous Ghostcaret release.
2. Keystroke savings must also beat the manual comparators in the side-by-side test below.
3. No regression against the previous release on any language in the corpus.

Any public claim that Ghostcaret is better than another product must point to the benchmark report or the side-by-side results that support it.

### Side-by-side test

Closed products cannot run inside the harness, so they are compared by hand, using each product's best publicly available plan. The comparators are FlowIn, Cotypist, TypeSeer and macOS inline predictive text on macOS; TypeSeer, Lightkey and Windows text suggestions on Windows; and Gboard on Android. The list follows [product.md](product.md#comparable-products) as products appear or disappear.

1. A fixed set of typing tasks (reply to an email, a chat exchange, a document paragraph, a message in a terminal chat tool) is typed by a script, or by a person following a script.
2. Keystrokes, accepted suggestions, wrong suggestions and time are counted.
3. The method, the task set and the results are published with the release.

## Parity floor

The best existing product has already solved a set of everyday problems. Each one becomes a required item on our app checklist. This list comes from FlowIn's public [release notes](https://github.com/Xin-Jing/flowin-releases/releases):

- Chinese, Japanese and Korean input: suggestions pause while an input method is composing, and accepting works character by character in those scripts.
- Secure input held by another app (often a password manager) is detected, and that app is named.
- Accessibility permission silently revoked after an OS update is detected within a minute, and the user is shown how to fix it.
- Chromium- and Electron-based apps that keep their accessibility tree asleep are woken, and re-woken if they fall asleep again.
- Terminals: suggestions line up with the character grid, stay quiet at a plain shell prompt, and work inside chat tools such as Claude Code.
- A rewrite in Markdown editors keeps headings, bullets and blank lines, and replaces only the block the cursor is in.
- Inserting into replies that quote earlier messages (Gmail) lands in the right place.
- Suggestions are placed correctly on every display, including displays to the left of or above the main one.
- Suggestion windows never take clicks or change the mouse cursor.
- Other shortcuts (app switching, Shift+Return, Force Quit) are never swallowed while a suggestion or card is showing.
- Taking a word treats compounds like "well-known" and paths like `src/main.rs` as several units, and stops before punctuation.
- Pasting, deleting or inserting text keeps the suggestion aligned, or removes it.
- Suggestions stay visible over launchers such as Spotlight and Raycast, and stay on the active desktop or Space.
- A model download resumes after the app quits.

## Engineering standards

Code:

- Rust stable, with `unsafe` limited to FFI and OS-binding modules. Every `unsafe` block carries a comment stating the invariant it relies on.
- Clippy runs in CI with warnings as errors. Lints that make code harder to read are switched off in the shared configuration rather than worked around.
- The keystroke path does not allocate, lock, or make a system call beyond the OS callback itself. A test enforces this with an allocation-counting allocator.
- All work has an owner and a cancellation path, with no orphaned tasks or unbounded queues.

Tests:

- Every bug fix ships with a regression test that fails when the fix is reverted.
- Text offsets, edits, insertion and stop rules have property tests, because UTF-8 versus UTF-16 offsets and grapheme clusters are where text tools break.
- Everything that parses outside input is fuzzed: provider responses, the Linux plugin socket protocol, config files, model manifests.
- Tests wait on events, never on fixed sleeps, and never depend on paths from one developer's machine.
- Microbenchmarks cover the key gate, prompt building and the stop rules. An engine latency benchmark runs on a reference runner, and CI fails if the 95th percentile regresses by more than 10%.
- Each release gets a 24-hour soak test for memory growth. The C and C++ code underneath (llama.cpp and the OS shims) is also run under sanitizers in CI.

Supply chain:

- Dependencies are pinned through the lockfile and checked with `cargo-deny` (licenses, advisories, duplicate versions) in CI.
- Release artifacts are signed and ship with an SBOM. Reproducible builds are the goal on every platform whose toolchain allows them.
- Model files are verified against SHA-256 hashes from the manifest inside the signed app, and every model's license is recorded.

Accessibility:

- The settings and setup windows meet WCAG 2.2 AA, work from the keyboard alone, follow the system text size, contrast and reduced-motion settings, and expose names and roles to screen readers.
- Suggestions can be announced through each platform's announcement API, as an opt-in setting.
- The suggestion color has a high-contrast option, and suggestions never rely on color alone to be noticed.

Documentation:

- Every claim in the README and the docs is checked against the product before release, including the claims in this file.

## Benchmark

`crates/bench` replays writing samples as typing. It uses keystroke timing drawn from real typing data, asks the engine for a suggestion at each pause, and accepts the longest stretch of the suggestion that matches the real continuation, using the same word-by-word and whole-suggestion rules as the keys. Each run records:

- the model, quantization, prompt template version, length mode and boldness;
- the reference machine;
- every metric above.

Corpora are public text under suitable licenses, covering email, chat, documents and every language we claim to support. A user can also run the benchmark on their own writing; that writing stays on their machine. The results set the defaults (boldness, the curated models, the Auto ceilings), and the benchmark runs again before any change to a model, prompt template or stop rule ships.

## App checklist

Each platform keeps a list of about 20 apps across browser text boxes, chat, notes and documents, terminals, and email, plus every item in the [parity floor](#parity-floor). Every app is checked for:

- reading the text;
- placement of the suggestion;
- taking a word, taking all, and showing more;
- typing through a suggestion;
- clean insertion;
- one-step undo;
- Reply & rewrite insertion;
- the latency targets above.

An app that fails placement gets the card fallback, and the failure is listed in the release notes.

## Release gates

A release ships only when all of these pass:

1. Benchmark: the suggestion-quality thresholds above, with the report attached to the release.
2. Responsiveness and resource targets on every reference machine for the platforms in the release, with no regression of more than 10% on any of them.
3. App checklist and parity floor on every platform in the release.
4. Tests: unit, property, fuzz smoke runs, sanitizer runs, and the soak test.
5. Supply chain: `cargo-deny` clean, artifacts signed, SBOM published.
6. Accessibility checks on the settings and setup windows.
7. Documentation claims checked.

## Open questions

- The exact reference machines for Linux, Windows and Android.
- The benchmark corpora and their licenses.
- The side-by-side task set.
