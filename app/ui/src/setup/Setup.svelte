<script lang="ts">
  // First run: welcome → permissions → model → practice → done. Each step is
  // saved, so leaving and reopening resumes where the user was.
  import { onMount, tick } from 'svelte';
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import { bytes, chordName, triggerName } from '../lib/format';
  import { app } from '../lib/store.svelte';
  import type { LocalModel, SetupStep } from '../lib/types';
  import ProviderForm from '../pages/ProviderForm.svelte';

  const order: SetupStep[] = ['welcome', 'permissions', 'model', 'practice', 'done'];
  const titles: Record<SetupStep, string> = {
    welcome: 'Welcome',
    permissions: 'Permissions',
    model: 'Choose a model',
    practice: 'Try it',
    done: 'All set',
  };

  let step = $state<SetupStep>(app.overview!.setup);
  let heading: HTMLHeadingElement | undefined = $state();
  let stepError = $state<string | null>(null);
  const index = $derived(order.indexOf(step));
  const s = $derived(app.settings!);
  const os = app.overview!.os;
  const ram = app.overview!.ram_bytes;
  const bigRam = ram !== null && ram >= 16 * 2 ** 30;

  let route = $state<'local' | 'key'>(app.overview!.local_available ? 'local' : 'key');
  let recommended = $state<LocalModel | null>(null);
  let modelError = $state<string | null>(null);

  async function go(next: SetupStep) {
    try {
      await api.setSetupStep(next);
      stepError = null;
    } catch (e) {
      stepError = asError(e).message;
    }
    step = next;
    if (next === 'model') void loadModels();
    await tick();
    heading?.focus();
  }

  /** Leaves setup for the settings pages; progress is already saved as done. */
  function openSettings() {
    app.overview!.setup = 'done';
  }

  async function loadModels() {
    if (!app.overview!.local_available) return;
    try {
      const view = await api.localModels();
      recommended = view.models.find((m) => m.recommended) ?? null;
    } catch (e) {
      modelError = asError(e).message;
    }
  }
  onMount(() => {
    if (step === 'model') void loadModels();
  });

  const modelReady = $derived(s.autocomplete_model.kind !== 'none');
  const downloading = $derived(app.download && recommended && app.download.id === recommended.id ? app.download : null);

  async function useLocal() {
    if (!recommended) return;
    modelError = null;
    s.autocomplete_model = { kind: 'local', model: recommended.id };
    if (s.draft_model.kind === 'none') s.draft_model = { kind: 'local', model: recommended.id };
    if (!(await app.save(`${recommended.name} chosen`))) return;
    if (!recommended.installed) {
      try {
        await api.downloadModel(recommended.id);
        announce(`Downloading ${recommended.name}. You can continue while it downloads.`);
      } catch (e) {
        modelError = asError(e).message;
      }
    }
  }

  async function useProvider(id: string) {
    s.autocomplete_model = { kind: 'provider', provider: id };
    s.draft_model = { kind: 'provider', provider: id };
    await app.save('Provider chosen for autocomplete and reply & rewrite');
  }

  const keys = $derived({
    word: s.keys.take_word ? chordName(s.keys.take_word) : null,
    all: s.keys.take_all ? chordName(s.keys.take_all) : null,
    more: s.keys.show_more ? chordName(s.keys.show_more) : null,
    dismiss: s.keys.dismiss ? chordName(s.keys.dismiss) : null,
    draft: s.keys.flow_draft ? triggerName(s.keys.flow_draft) : null,
  });
</script>

<main class="setup" aria-labelledby="setup-title">
  <p class="muted" aria-hidden="true">Step {index + 1} of {order.length}</p>
  <h1 id="setup-title" tabindex="-1" bind:this={heading}>
    <span class="visually-hidden">Setup, step {index + 1} of {order.length}: </span>{titles[step]}
  </h1>
  {#if stepError}<p class="error" role="alert">Progress not saved: {stepError}</p>{/if}

  {#if step === 'welcome'}
    <p>
      Ghostcaret suggests the rest of what you are typing, in any app. The suggestion appears as faint text at the cursor; take a word,
      take it all, or keep typing.
    </p>
    <p>It needs no account. You choose the model: one that runs on this device, or your own key for a cloud provider.</p>
    <div class="actions"><button type="button" class="primary" onclick={() => go('permissions')}>Start</button></div>
  {:else if step === 'permissions'}
    {#if os === 'linux'}
      <p>
        Ghostcaret works as your Wayland input method, so it sees the text field you type in and can place suggestions at the cursor.
        Your compositor must allow input methods (niri, Sway, Hyprland and KDE do). Chromium and Electron apps need
        <code>--enable-wayland-ime</code>.
      </p>
    {:else if os === 'windows'}
      <p>
        No permission is needed. Ghostcaret reads the field you type in through UI Automation. Apps running as administrator cannot be
        reached unless Ghostcaret also runs as administrator.
      </p>
    {:else if os === 'macos'}
      <p>Ghostcaret needs the Accessibility permission to read the field you type in and to insert accepted text.</p>
    {:else}
      <p>Enable the Ghostcaret keyboard in the system settings, then switch to it in any text field.</p>
    {/if}
    <p>
      It never reads password fields, does not keep a log of your keys, and sends nothing anywhere unless you add a cloud key.
    </p>
    {#if app.status}
      <p class="panel" role="status">
        Status: {app.status.text}
        {#if app.status.kind === 'permission_needed' || app.status.kind === 'unavailable'}
          <span class="block">Setup can continue; the tray will show when this is fixed.</span>
        {/if}
      </p>
    {/if}
    <div class="actions">
      <button type="button" onclick={() => go('welcome')}>Back</button>
      <button type="button" class="primary" onclick={() => go('model')}>Continue</button>
    </div>
  {:else if step === 'model'}
    <fieldset>
      <legend>How should suggestions be made?</legend>
      {#if app.overview!.local_available}
        <label class="check">
          <input type="radio" name="route" value="local" bind:group={route} />
          <span
            >On this device<span class="muted"
              >Private and free to run. {bigRam
                ? 'This device has enough memory for the standard model.'
                : 'This device has less than 16 GB of memory, so the small model is offered; a key may give better results.'}</span
            ></span
          >
        </label>
      {/if}
      <label class="check">
        <input type="radio" name="route" value="key" bind:group={route} />
        <span>My own API key<span class="muted">OpenAI-compatible servers, Anthropic or Gemini. Text is sent to that provider.</span></span>
      </label>
    </fieldset>

    {#if route === 'local'}
      {#if recommended}
        <div class="panel">
          <p><strong>{recommended.name}</strong> · {bytes(recommended.size)} · {recommended.license}</p>
          {#if downloading}
            <progress max={downloading.total} value={downloading.downloaded} aria-label="Downloading {recommended.name}"></progress>
            <p class="muted">{bytes(downloading.downloaded)} of {bytes(downloading.total)}. You can continue while it downloads.</p>
            <button type="button" onclick={() => api.cancelDownload()}>Stop download</button>
          {:else if recommended.installed && s.autocomplete_model.kind === 'local' && s.autocomplete_model.model === recommended.id}
            <p>Downloaded and chosen.</p>
          {:else}
            <button type="button" class="primary" onclick={useLocal}>{recommended.installed ? 'Use this model' : 'Download and use'}</button>
          {/if}
          {#if modelError}
            <p class="error" role="alert">{modelError}</p>
            <p class="hint">Try again, continue and resume the download later from Models, or use a key instead.</p>
          {/if}
        </div>
      {:else if modelError}
        <p class="error" role="alert">{modelError}</p>
      {:else}
        <p class="muted">Loading the model list…</p>
      {/if}
    {:else}
      <ProviderForm onsaved={(c) => useProvider(c.id)} />
    {/if}

    <div class="actions">
      <button type="button" onclick={() => go('permissions')}>Back</button>
      <button type="button" class="primary" onclick={() => go('practice')} disabled={!modelReady} aria-describedby="model-next-h">
        Continue
      </button>
    </div>
    {#if !modelReady}<p class="hint" id="model-next-h">Choose a model to continue, or skip and choose one later in Models.</p>{/if}
    {#if !modelReady}<div class="actions"><button type="button" class="quiet" onclick={() => go('practice')}>Skip for now</button></div>{/if}
  {:else if step === 'practice'}
    <p>Type a sentence below and pause. When a faint suggestion appears:</p>
    <ul>
      {#if keys.word}<li><kbd>{keys.word}</kbd> takes the next word</li>{/if}
      {#if keys.all}<li><kbd>{keys.all}</kbd> takes the whole suggestion</li>{/if}
      {#if keys.more}<li><kbd>{keys.more}</kbd> shows more</li>{/if}
      {#if keys.dismiss}<li><kbd>{keys.dismiss}</kbd> dismisses it</li>{/if}
      {#if keys.draft}<li>Select some text and press <kbd>{keys.draft}</kbd> to rewrite it</li>{/if}
    </ul>
    {#if app.download}
      <div class="panel" role="status">
        <p>The model is still downloading: {bytes(app.download.downloaded)} of {bytes(app.download.total)}.</p>
        <progress max={app.download.total} value={app.download.downloaded} aria-label="Model download"></progress>
      </div>
    {:else if app.status && app.status.kind !== 'active'}
      <p class="panel" role="status">Status: {app.status.text}</p>
    {/if}
    <label for="practice" class="visually-hidden">Practice field</label>
    <textarea id="practice" rows="6" placeholder="Thanks for the update, I will"></textarea>
    <div class="actions">
      <button type="button" onclick={() => go('model')}>Back</button>
      <button type="button" class="primary" onclick={() => go('done')}>Finish</button>
    </div>
  {:else}
    <p>
      Ghostcaret runs in the tray. Its menu shows the status and pauses suggestions for 15 minutes, an hour, or in one app. Settings are
      in the same menu.
    </p>
    <div class="actions"><button type="button" class="primary" onclick={openSettings}>Open settings</button></div>
  {/if}
</main>

<style>
  .setup {
    max-width: 40rem;
    margin: 0 auto;
    padding: 2rem 1.5rem;
  }
  .block {
    display: block;
  }
  kbd {
    font-family: ui-monospace, 'JetBrains Mono', monospace;
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 0 0.3rem;
  }
  code {
    font-family: ui-monospace, 'JetBrains Mono', monospace;
  }
</style>
