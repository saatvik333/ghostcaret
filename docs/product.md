# Ghostcaret product spec

Status: draft. Decisions recorded here were agreed on 2026-10-29 and 2026-10-30. Nothing is built yet.

Ghostcaret suggests the rest of what you are typing, in any text field, on every desktop and mobile platform. The suggestion appears as faint text at your cursor. You can take the next word, take all of it, ask for more, or keep typing and ignore it. It can also draft a reply to the conversation on screen, or rewrite text you have selected.

It is free and open source under MPL-2.0. You choose the model: a local model that runs on your own machine, or your own API key for a cloud provider. There is no account, no telemetry and no paid tier.

The goal is the best predictive typing available on each platform: more accurate, faster, lighter on the machine and more private than the alternatives. [quality.md](quality.md) defines how each of those is measured, and no release ships without meeting it.

## Rules the product keeps

1. Typing never waits on Ghostcaret. Keystrokes reach the app at once; prediction runs beside them, and the next keystroke cancels it.
2. Silence is better than a wrong guess. A suggestion appears only when the model is sure enough for the user's boldness setting.
3. The user decides how much it suggests, which keys accept it, and which apps it runs in.
4. Text stays on the device unless the user configured a cloud provider. Even then, only the context for that one request leaves, after the secret filter has run.
5. It never sends anything for the user. Drafts and rewrites go into the text field, and the user presses send.
6. Behaviour is the same on every platform from the user's side, even where the operating system forces a different mechanism underneath.
7. It is light. It does no work while you are not typing, stays within published memory, battery and size budgets, and no release may be slower or heavier than the one before beyond the limits in [quality.md](quality.md).

## Comparable products

Prices, platforms and data practices below come from each vendor's own pricing, privacy and terms pages, read on 2026-10-30. They change; check the linked page before repeating a figure.

### Paid tools that complete text inline across apps

| Product | Platforms | Price | Local model | Own API key | Open source | Account | What leaves the machine |
| --- | --- | --- | --- | --- | --- | --- | --- |
| [FlowIn](https://flowin.so/pricing) | macOS (Apple Silicon) | Free tier of 150 words and 10 AI replies a day after a 14-day trial; Pro $10.99 a month or $79.99 a year; one-time $129.99 with 12 months of updates | Yes, plus cloud models | Only on the one-time license, and only for replies | No | License key | Cloud features send text to the vendor's relay and an AI provider. [Privacy policy](https://flowin.so/privacy): anonymous usage analytics, including the names of apps used, tied to an install ID |
| [Cotypist](https://cotypist.app/pricing) | macOS (Apple Silicon) | Free tier of 100 accepted words a day; Plus 72 a year (one Mac); Pro 108 a year (three Macs); the page shows no currency symbol | On-device only | No | No | Not required | Text stays on the Mac. Anonymous usage stats and crash reports on by default, opt-out ([privacy](https://cotypist.app/help/privacy)) |
| [TypeSeer](https://typeseer.com/pricing.html) | macOS 14+ (Apple Silicon), Windows 10/11 | No free tier; $3.99 a month, $34.99 a year or $79.99 once, per platform, two devices | On-device only; accepts your own GGUF models | No | No | Not required | Typing stays on the device; the update check sends an install ID ([privacy](https://typeseer.com/privacy.html)) |
| [Lightkey](https://www.lightkey.io/editions) | Windows, plus Chrome and Edge extensions | No free edition; Pro $49.95 a year or $229.95 once; Business $69.95 a year or $299.95 once | Offline word prediction; the vendor says it is not generative AI | No | No | Not verified | Pro sends usage metrics by default, opt-out ([privacy](https://www.lightkey.io/privacy-policy)) |
| [HyperWrite TypeAhead](https://www.hyperwriteai.com/) | Chrome extension only | Premium $19.99 a month ($16 billed yearly); Ultra $44.99 a month ($29 billed yearly) | No | Not verified | No | Required | Cloud. [Privacy policy](https://www.hyperwriteai.com/privacy) (last updated 2022) mentions Google Analytics and interest-based ads |
| [Compose AI](https://www.compose.ai/) | Chrome extension only | Free plan plus a premium tier; prices not published in readable form | No | No | No | Required | Text goes to its servers; the [privacy policy](https://www.compose.ai/privacy-policy) names Mixpanel, Segment and Google Analytics |

### Paid tools people use instead of typing

These rewrite or dictate rather than complete inline, but they are what many people pay for to write faster.

| Product | Platforms | Price | Runs locally | Own API key | Notes |
| --- | --- | --- | --- | --- | --- |
| [Grammarly](https://www.grammarly.com/pro) | Browser extension and desktop apps | Pro $12 a month billed yearly, $30 monthly | No | No | Rewrites and generates text; [content may be used to train its models](https://superhuman.com/legal/privacy-policy) unless turned off in account settings |
| [Wispr Flow](https://wisprflow.ai/pricing) | macOS, Windows, iOS, Android | Pro $15 a month ($12 billed yearly) | No; [transcription always runs in the cloud](https://wisprflow.ai/data-controls) | No | Voice dictation |
| [Superwhisper](https://superwhisper.com/) | macOS, Windows, iOS, Android | Free tier; Pro price not verified | Yes | Yes, on Pro | Voice dictation |
| [Typeless](https://www.typeless.com/pricing) | Desktop and mobile | Pro $12 a month billed yearly, $30 monthly | No | No | Voice dictation with rewriting |
| [Willow Voice](https://willowvoice.com/pricing) | Mac, Windows, iOS, Android (some marked coming soon) | Pro $15 a month ($12 billed yearly) | No | No | Voice dictation |
| [Aqua Voice](https://aquavoice.com/pricing) | Mac, Windows, iOS, Android | Free 1,000 words; Pro $12 a month ($10 billed yearly) | No | No | Voice dictation |

### Free and open-source tools

| Product | License | Platforms | Model |
| --- | --- | --- | --- |
| [Cotabby](https://github.com/FuJacob/cotabby) | AGPL-3.0 | macOS | Local GGUF models, Apple Intelligence, or an OpenAI-compatible endpoint |
| [KeyType](https://github.com/johnbean393/KeyType), [Pretype](https://github.com/nikiomori/Pretype), [Shadowtype](https://github.com/jacobbourne/shadowtype) | MIT | macOS | Local |
| [MindFlow](https://github.com/simoabid/MindFlow) | MIT | Linux (IBus) | Gemini; early project |

Operating systems also ship simpler prediction of their own: [inline predictive text](https://support.apple.com/guide/mac-help/mchlp2299/mac) on macOS 14 and later, [text suggestions](https://support.microsoft.com/en-us/accessibility/windows/enable-text-suggestions-in-windows) on Windows (word choices above the cursor, picked with the mouse or arrow keys), and phone keyboards such as Gboard. [quality.md](quality.md#side-by-side-test) compares Ghostcaret against these as well.

### Where Ghostcaret differs

Ghostcaret is in development, so everything in this section is planned, not shipped. Whether it predicts better than these products is a question for the benchmark in [quality.md](quality.md), and no such claim is made until that report exists.

- Price: free, with no word or reply limits. The paid inline tools above start at $34.99 a year (TypeSeer); HyperWrite's top plan is $348 a year billed yearly or $44.99 a month billed monthly.
- Platforms: Linux first, then Windows and Android; macOS and iOS later. None of the paid tools checked has a Linux app. The two Chrome extensions work in Chrome on Linux, but only inside the browser. TypeSeer is the only inline tool with native apps for more than one desktop OS.
- Your choice of model: a local model on your machine, or your own key for any OpenAI-compatible endpoint, Anthropic or Gemini, for both autocomplete and replies. Among the paid tools checked, only FlowIn's one-time license (for replies) and Superwhisper Pro (for dictation) accept your own key.
- Open source: MPL-2.0, so anyone can check what the app does with their keystrokes. None of the paid tools checked is open source.
- No account and no telemetry. Several of the tools above collect usage analytics by default.
- Length: you choose how long suggestions get, per app.

## First release

Each platform's first release ships autocomplete and Reply & rewrite together. Mobile limits are listed under [Platforms](#platforms).

### Autocomplete

After a short pause in typing, the suggestion appears as faint text after the cursor, in the field's own font where the platform allows it. Typing a character that matches the suggestion shortens it instead of starting over. Typing anything else, moving the cursor, clicking, switching apps or pressing Esc removes it.

It stays quiet in these places:

- password and other secure fields;
- address bars;
- code editors, unless the user turns it on for that app;
- a terminal sitting at a plain shell prompt, so Tab keeps working for shell completion. It does work in prose inside terminal apps, such as a Claude Code session;
- an empty field, until the user has typed something. Reply & rewrite covers starting from nothing.

### Length and boldness

Length is the most a suggestion may show. Boldness is how sure the model must be before it shows anything, and where it stops when it becomes unsure.

| Length | Stops at |
| --- | --- |
| Word | the end of the next word |
| Phrase | a natural pause such as a comma or a conjunction, usually 3 to 6 words |
| Sentence | the end of the sentence |
| Paragraph | the end of the paragraph, or the user's word cap |
| Auto (default) | the point where the model's confidence drops below the boldness setting |

Auto's ceiling depends on the kind of app: up to a sentence in chat apps, up to a paragraph in email, documents and notes, and one word in search boxes. Users can change the length and the ceiling for each app.

Boldness is a slider from careful to bold. The benchmark sets its default position (see [quality.md](quality.md#benchmark)).

The engine writes a little further than it shows. Taking a word or asking for more then uses text that already exists, with no new request.

"Learn my length" is an optional setting, off by default. When it is on and the user keeps taking whole suggestions in an app, that app's suggestions get longer. When the user keeps dismissing them, they get shorter.

### Keys

| Action | Default key | Notes |
| --- | --- | --- |
| Take the next word | Tab | Only captured while a suggestion is showing; otherwise the app gets Tab as usual |
| Take the whole suggestion | ` (backtick) | Only captured while a suggestion is showing. To type a literal backtick at that moment, press Esc first |
| Show more | Alt+→ (Option+→ on macOS) | Extends the suggestion in place |
| Next or previous alternative | Alt+↓ / Alt+↑ | Proposed default, not yet confirmed |
| Dismiss | Esc | |
| Reply & rewrite | Tap the right Alt key (right Option on macOS) on its own | Holding it as a modifier keeps its normal meaning |
| Pause or resume | No default | Can be set in settings; the tray menu always offers pause |

Every key can be changed, and the settings refuse a binding that clashes with another action or a reserved system shortcut. On phones the suggestion appears in the keyboard's suggestion strip instead. The touch controls are designed in the mobile phase.

### Alternatives

While a suggestion is showing, up to three other continuations can be cycled. The local model computes them at almost no extra cost. A cloud provider needs extra requests for them, so they are only fetched when the user asks.

### Reply & rewrite

It drafts a whole reply to the conversation on screen, or rewrites text the user has selected.

Reply: in a chat or email reply box, the user taps the Reply & rewrite key. Ghostcaret reads the visible conversation and offers three short directions, such as "accept and suggest Tuesday", along with a field to type a custom one. Picking one writes a full draft into a preview card. The user can accept it into the text box, edit the direction, or close the card.

Rewrite: the user selects text and taps the same key. The card offers Fix, Rephrase, Shorten, Expand, Translate and Change tone, plus any custom actions the user has defined, such as "make it formal" or "add my sign-off". The result shows in the preview first. Accepting replaces the selection, and one undo restores the original.

Reply & rewrite follows these rules:

- It shows exactly what will be inserted before inserting it.
- It never sends a message or presses Return.
- If it cannot read the conversation, it says so and offers two options: type a direction, or allow a screenshot to be read (opt-in, see Context).
- It can use a different model from autocomplete. A common setup is a local model for autocomplete and a cloud key for drafts.
- Offering a draft automatically when the user pauses in an empty reply box is an optional setting, off by default.

### Context

| Source | Default | Used for | Notes |
| --- | --- | --- | --- |
| Text before and after the cursor in the current field | On | Both features | Capped in length |
| App name and window title | On | Both features | |
| Visible conversation, read through the accessibility API | On | Both features | Read locally. Leaves the device only inside a request to a cloud provider the user configured |
| Screenshot of the active window, read with OCR | Off | Fallback when accessibility shows no text | Needs the platform's screen permission |
| Clipboard | Off | Both features | Copies from password managers and anything matching the secret filter are ignored |
| Messages the user sent (style memory) | Off | Matching the user's voice | Stored locally, limited per app, can be viewed and deleted |
| About me | Empty | Both features | Free text the user writes |

### Models and keys

Setup asks the user to choose between a local model and their own key.

For a local model, Ghostcaret checks memory first. On 16 GB or more, it offers the standard model (about 3 GB). With less, it offers the small model or suggests using a key. Any other GGUF file also works. The download runs in the background and resumes after interruption, and the file is checked against its published hash before use.

For their own key, the user picks a provider: any OpenAI-compatible endpoint (OpenAI, OpenRouter, Groq, Together, Ollama, LM Studio, llama-server, vLLM and similar), Anthropic, or Gemini. They paste the key and test it with one request. Keys are stored in the operating system's keychain.

Autocomplete and Reply & rewrite each have their own model choice. Cloud use shows a request counter and supports an optional daily request cap. If the cloud provider fails, autocomplete goes quiet, the tray shows why, and the local model takes over if one is installed.

### Privacy and safety

- Ghostcaret never reads password or secure fields.
- The secret filter removes API keys, access tokens, card numbers and `password=`-style pairs before anything leaves the device or goes into style memory.
- Password managers are on the blocked-apps list by default. Users can add any app.
- A "What was sent" view lists every cloud request from the current session: provider, model, and the exact text sent. It is kept in memory and cleared on quit.
- There is no telemetry. Network access happens only for model downloads, the update check (which can be turned off) and the provider the user configured.
- Text read from the screen, the clipboard or a conversation is treated as data. Instructions inside it cannot change what Ghostcaret does.

### Per-app control

Each app can be on or off, and can have its own length setting and Auto ceiling. The tray menu can pause everything or just the current app, for 15 minutes, an hour, or until resumed. Apps appear in the list as the user types in them.

### Usage stats

Stats are kept on the device: words accepted, keystrokes saved, and acceptance rate per app, by day. They never contain any text.

## Defaults

| Setting | Default |
| --- | --- |
| Length | Auto |
| Auto ceiling | Sentence in chat apps, Paragraph in email, documents and notes, Word in search boxes |
| Boldness | Set by the benchmark |
| Learn my length | Off |
| Keys | Tab next word, ` whole suggestion, Alt+→ more, Esc dismiss, right Alt tap for Reply & rewrite |
| Model | Chosen at setup (local or own key) |
| Local model on 16 GB or more | Standard, about 3 GB |
| Local model below 16 GB | Small model, or own key |
| Code editors | Off |
| Screenshot and OCR, clipboard, style memory, automatic draft offers | Off |
| Telemetry | None |

## Platforms

| Platform | How suggestions appear | Permissions requested | Known limits |
| --- | --- | --- | --- |
| Linux (Wayland) | Ghostcaret is the input method; the suggestion appears in a small panel at the cursor | None beyond running it; the screenshot portal only for OCR | Needs a compositor with the Wayland input-method protocol (niri, sway, Hyprland, river, labwc and others); KDE Plasma, GNOME and X11 come later. Cannot run alongside fcitx5 or IBus. Apps must support `text-input-v3`; Chromium and Electron need `--enable-wayland-ime` ([fcitx5 Wayland guide](https://fcitx-im.org/wiki/Using_Fcitx_5_on_Wayland)) |
| macOS | Faint text drawn at the cursor | Accessibility; Screen Recording only for OCR | Paused while another app holds Secure Input (Ghostcaret names the app when macOS reveals it). Browsers that hide the cursor position get a card above the field |
| Windows | Faint text drawn at the cursor | None beyond install | Cannot reach apps running as administrator unless Ghostcaret also runs elevated |
| Android | In Ghostcaret's own keyboard | Enable the keyboard | The user must switch to this keyboard. Keyboards cannot read the rest of the screen, so replies use the field text, the selection and, optionally, the clipboard |
| iOS / iPadOS | In Ghostcaret's keyboard extension | Enable the keyboard, and turn on Full Access so it can reach the cloud provider | Cloud key only: keyboard extensions are limited to tens of megabytes of memory, too little for a local model. Same conversation limit as Android |

## First run

| Step | What happens | If it goes wrong |
| --- | --- | --- |
| 1. Welcome | One screen: what Ghostcaret does, and that it works without an account | |
| 2. Permissions | The platform's required permission, with the reason and what is not done with it | Denied: setup continues, the tray shows "Permission needed", and settings can re-open the request. macOS is checked again after updates, because it can silently revoke the grant |
| 3. Model | Choose local model or own key, as described under Models and keys | Download fails: retry, resume later, or switch to a key. Key test fails: the provider's error is shown and the pasted key is kept |
| 4. Practice | A practice field to try Tab, backtick, Alt+→ and Esc, then a sample reply | If the model is still downloading, practice waits with real progress shown, or can be skipped |
| 5. Done | Shows where the tray icon is and how to pause | |

Setup can be left and resumed at the same step.

## Status shown in the tray

| State | Meaning |
| --- | --- |
| Active | Suggestions are on |
| Paused | Paused globally or for this app, with the time it resumes |
| Off in this app | Turned off by the user or by default (code editors, password managers) |
| Blocked by secure input | Another app has taken secure keyboard input; shows that app's name when known |
| Permission needed | The platform permission is missing or was revoked |
| Model downloading | Shows real progress |
| No model | Neither a local model nor a key is configured |
| Cloud unavailable | The provider failed or the daily cap was reached; shows whether the local model took over |

## Quality bar

The full threshold, with every target and how it is checked, is in [quality.md](quality.md). In user terms, a release ships only when:

- a suggestion appears within about 100 ms of the last keystroke on the reference machines, and typing is never delayed;
- it saves more keystrokes than every alternative we can test, with an equal or lower rate of wrong guesses;
- it uses no CPU while you are not typing, and stays within its memory and battery budgets;
- the suggestion sits exactly at the cursor, typing through it never makes it flicker, and accepting never loses or doubles characters;
- everything the best existing product already handles also works here.

## Not in the first release

- Accounts, sync, telemetry
- Voice input
- Code completion tuned for IDEs (code editors are off by default)
- Training a model on the user's writing
- Shared style packs for teams
- Emoji and snippet expansion

## Open questions

- Product name and visual identity.
- Default boldness and the curated local models: both come out of the benchmark, plus a license review of each model.
- Default key for cycling alternatives.
- Whether a double backtick should type a literal backtick while a suggestion is showing.
- The word cap for Paragraph.
