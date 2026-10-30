# ghostcaret-bench

The replay benchmark of [docs/quality.md](../../docs/quality.md#benchmark): it types public writing into a simulated field, lets the product ask a local model for suggestions at the typist's pauses, and counts what an ideal but honest user would have saved. Its results set the defaults (boldness, the curated models, the Auto ceilings), and any claim that Ghostcaret is better points to its report.

## Running

```sh
# CUDA build (set the architecture of your GPU; 86 is an RTX A500)
PATH=/opt/cuda/bin:$PATH CUDA_PATH=/opt/cuda CMAKE_CUDA_ARCHITECTURES=86 \
RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR=target/bench \
cargo run -p ghostcaret-bench --release --features cuda -- run \
    --models qwen3.5-0.8b-base-q4_k_m,gemma-4-e2b-q4_k_m:20 --out target/bench-report
```

Commands:

- `run`: replays every corpus under every configuration for each model in turn and writes `results.json` and `report.md` to `--out` after each model. Runs of other models already in `results.json` are kept when the engine settings, prompt template and method are unchanged, so models can be run one at a time.
- `corpora [--show]`: downloads and verifies the corpora and prints their sizes (and every sample).
- `report`: renders `report.md` again from `results.json`.

Options of `run`:

| Option | Default | Meaning |
| --- | --- | --- |
| `--models ID[:LAYERS],…` | every curated model | Manifest ids or GGUF paths; `:LAYERS` places that many layers on the GPU |
| `--corpora ID,…` | all | `chat-en`, `email-en`, `document-en`, `document-de` |
| `--samples N` | all | At most N samples per corpus; the report marks the run as a subset |
| `--modes …` | `word,phrase,sentence,auto` | Length modes swept |
| `--ceilings …` | `phrase,sentence,paragraph` | Auto ceilings swept, in the corpus's app category |
| `--boldness …` | `0,0.25,0.5,0.75,1` | Boldness values swept |
| `--seed N` | 1 | Seed of the pause draws |
| `--max-load F` | 2.0 | Latency is marked invalid when the 1-minute load average exceeds this at any point of a model's run |
| `--ctx N`, `--threads N` | 4096, auto | Model context and CPU threads |
| `--data-dir DIR` | `~/.local/share/ghostcaret/bench` | Where corpora are downloaded |
| `--out DIR` | `target/bench-report` | Where results go |

Corpora are downloaded on first use from pinned URLs and checked against their SHA-256 on every load; a changed source stops the run instead of silently replaying other text. Nothing downloaded is committed, and nothing is sent anywhere else. To benchmark your own writing, add a corpus in `src/corpus.rs` that reads a local file; it stays on your machine.

Cost: one model call per distinct caret position the sweep visits, about one per character typed (all 30 configurations together cost about twice one configuration). On an RTX A500 shared with a desktop that is 0.25–0.4 s per call for Qwen3.5 0.8B/2B, Qwen3 1.7B and Gemma 4 E2B (all layers on the GPU) and about 1.7 s for Qwen3.5 4B with 16 of its layers on the GPU; the full corpora (about 30,000 characters) take two to three hours per small model, so `--samples 3` (about 2,650 calls) is the quick subset.

## Method

**The product decides.** Each sample is typed character by character into a simulated field around the real `ghostcaret_engine::controller::Controller`. The bench sends the field snapshots, key-gate events and pause timer a backend would send and runs each `Output::Complete` on the model with a `stop::Stopper`, as the app runtime does. So the prompt (`prompt::build` with the conversation, title and text before the caret), the stop rule (settings and the app's category), the cut into a suggestion (`stop::cut`), the silence rules and keeping a suggestion while it is typed through are the product's own code. Take Word, Take All and typing-through use the engine's `SuggestionView` methods, as the key gate does.

**Pause points** (`src/pause.rs`). A suggestion helps only when it is on screen before the next keystroke, so it is requested and seen where the next inter-key interval is at least the product's pause (30 ms) plus the latency target (100 ms): at every word boundary, right after an accept, and inside words where a log-normal inter-key interval drawn for that position is long enough. The distribution's mean, 238.66 ms, is the mean inter-key interval of 168,000 typists in Dhakal et al., [Observations on Typing from 136 Million Keystrokes](https://aaltodoc.aalto.fi/bitstreams/c175f9bb-8c83-40bc-8969-e179f4a3a6e6/download) (CHI 2018); its spread, σ = 0.5 for ln(interval), is an assumption. That makes about 83 % of positions inside words pause points. Draws come from a hash of the seed, the sample id and the position, so every configuration pauses at the same places and every run is identical.

**The user** (`src/user.rs`) is ideal but honest. With a suggestion showing, they take all of it with one backtick when all of it is what comes next; otherwise take Take Word units with Tab while each is what comes next; otherwise type the next character. "What comes next" means the remaining text starts with the suggested text and the suggested text ends at a Take Word unit end or boundary of the real text, so `see` is not accepted before `seeing`. An accept of a single character is typed instead (it costs the same key).

**Metrics** (`src/metrics.rs`), pooled over a corpus's samples:

- keystroke savings: 1 − (typed characters + Tab + backtick) / characters of the text;
- wrong-suggestion rate: shown suggestions whose first Take Word unit was not what came next, over shown suggestions;
- acceptance rate: shown suggestions that were accepted from at least once;
- accepted characters per accepted suggestion and per shown suggestion;
- suggestions shown and wrong suggestions per 100 typed words, and words visible per suggestion;
- time to suggestion (p50, p95), marked invalid when the 1-minute load average went above `--max-load`.

**Configurations.** Word, Phrase and Sentence at every boldness, and Auto at every ceiling and boldness, with the ceiling set for the corpus's app category. Everything else is the default settings, so conversation context is on and clipboard, style memory and about-me are off.

**One generation for all configurations, in typing order** (`src/shared.rs`). Configurations differ only in their stop rule and decoding is greedy, so the text each configuration's own request would generate is a prefix of what the one that stops last generates. Each distinct prompt is generated once with every configuration's stopper attached; each configuration receives the candidate cut back to where its own stopper stopped, with the time at which it stopped. All configurations of a sample replay at once, and the model serves the waiting prompt nearest the start of the text only when every replay waits, so prompts reach the model in typing order and its cache only appends, as in the product (recurrent models such as Qwen3.5 cannot cut their state back cheaply). A unit test replays a sweep both ways and checks that every tally equals the one from separate requests, and that prompts arrive in order.

**Not measured:** Show More and cycling alternatives (an alternative is shown only when the main candidate is silent, and counted), prefill and its effect on latency, cloud providers, Paragraph mode, input-method composition and dead keys, and real typing rhythm beyond the pause model. Latency taken under load says nothing about the product.

## Corpora

| Id | Text | License | Why |
| --- | --- | --- | --- |
| `chat-en` | [MultiWOZ 2.2](https://github.com/budzianowski/multiwoz), test file 001: the first 4 turns of the first 15 dialogues, both speakers (60 samples) | MIT | Written chat between two people with every earlier turn as context. Task-oriented and public since 2018, so formulaic and likely in training data |
| `email-en` | Apache Software Foundation lists dev@community, users@kafka, users@tomcat, dev@airflow, September 2026: the first 8 human messages of 12–180 words per list without code or links, the author's own text only (32 samples) | [ASF Public Forum Archive Policy](https://www.apache.org/foundation/public-archives.html): published "without conditions"; no license grant, read locally only | Real replies written after the models' training, with the message answered as context. No openly licensed email corpus exists: Enron has no license and is memorized, AESLC is non-commercial, BC3 requires registration |
| `document-en` | [English Wikinews](https://en.wikinews.org/), articles of January–April 2026: one paragraph per article after the title and up to two earlier paragraphs (20 samples) | CC BY 4.0 | Edited prose written after the models' training; Wikinews closed in May 2026, so the set is static |
| `document-de` | German Wikipedia articles created in October 2026, same paragraph rule (16 samples) | CC BY-SA 4.0 | A second language, written after the models' training. Articles flagged for deletion or quality review (one for suspected AI writing) were left out |

Text written in 2026 may itself be partly machine-assisted; that is a known limit of the post-training sets. The chat set is likely memorized, so its absolute numbers are optimistic.
