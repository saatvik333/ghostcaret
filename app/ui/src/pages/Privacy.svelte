<script lang="ts">
  import { onMount } from 'svelte';
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import ConfirmDialog from '../lib/ConfirmDialog.svelte';
  import { dateTime } from '../lib/format';
  import { app } from '../lib/store.svelte';
  import type { SentEntry, StyleSample } from '../lib/types';

  const s = $derived(app.settings!);
  let sent = $state<SentEntry[]>([]);
  let samples = $state<StyleSample[]>([]);
  let aboutTimer: ReturnType<typeof setTimeout> | undefined;
  let confirm = $state<{ title: string; message: string; label: string; run: () => void } | null>(null);
  let confirmOpen = $state(false);
  let loadError = $state<string | null>(null);

  async function refresh() {
    try {
      [sent, samples] = await Promise.all([api.sentLog(), api.styleSamples()]);
      loadError = null;
    } catch (e) {
      loadError = asError(e).message;
    }
  }
  onMount(() => {
    void refresh();
    return () => flushAbout();
  });

  // About me saves shortly after typing stops and when the field loses focus;
  // until then it counts as an unsaved draft.
  function aboutInput() {
    app.setDraft('about-me', 'About me');
    clearTimeout(aboutTimer);
    aboutTimer = setTimeout(flushAbout, 800);
  }
  function flushAbout() {
    if (aboutTimer === undefined) return;
    clearTimeout(aboutTimer);
    aboutTimer = undefined;
    void app.save('About me saved').then((ok) => ok && app.setDraft('about-me', null));
  }

  function ask(title: string, message: string, label: string, run: () => Promise<void>) {
    confirm = { title, message, label, run: () => void run() };
    confirmOpen = true;
  }

  const sampleApps = $derived([...new Set(samples.map((x) => x.app))]);
</script>

<h1 id="page-title" tabindex="-1">Privacy & context</h1>
<p class="hint">
  Password and secure fields are never read. Text leaves this device only inside a request to a cloud provider you configured, after
  keys, tokens, card numbers and password pairs are removed.
</p>

<h2>What suggestions may use</h2>
<label class="check">
  <input type="checkbox" bind:checked={s.context.conversation} onchange={() => app.save()} />
  <span>The visible conversation<span class="muted">Read through the accessibility interface of the app you type in.</span></span>
</label>
<label class="check">
  <input type="checkbox" bind:checked={s.context.screen_ocr} onchange={() => app.save()} />
  <span
    >Read the window with OCR when no text is available<span class="muted">Needs screen permission. The screenshot is read on this device and discarded.</span
    ></span
  >
</label>
<label class="check">
  <input type="checkbox" bind:checked={s.context.clipboard} onchange={() => app.save()} />
  <span>The clipboard<span class="muted">Copies from password managers are ignored.</span></span>
</label>
<label class="check">
  <input type="checkbox" bind:checked={s.context.style_memory} onchange={() => app.save()} />
  <span
    >Remember messages I send, to match my style<span class="muted">Kept on this device, up to 50 per chat or email app.</span></span
  >
</label>

<div class="field">
  <label for="about">About me</label>
  <textarea
    id="about"
    rows="4"
    bind:value={s.context.about_me}
    oninput={aboutInput}
    onblur={flushAbout}
    aria-describedby={app.errors['context.about_me'] ? 'about-e' : 'about-h'}
    aria-invalid={app.errors['context.about_me'] ? true : undefined}
  ></textarea>
  <p class="hint" id="about-h">Anything that helps, such as your name, role and how you write. Used by both features.</p>
  {#if app.errors['context.about_me']}<p class="error" id="about-e">{app.errors['context.about_me']}</p>{/if}
</div>

{#if loadError}<p class="error">{loadError}</p>{/if}

<h2>What was sent</h2>
<p class="hint">Every cloud request since Ghostcaret started, exactly as sent. Kept in memory only and cleared on quit.</p>
<div class="row">
  <button type="button" onclick={refresh}>Refresh</button>
  {#if sent.length > 0}
    <button
      type="button"
      onclick={() =>
        ask('Clear the list?', 'The list of requests sent this session is emptied. Nothing else changes.', 'Clear list', async () => {
          await api.clearSentLog();
          sent = [];
          announce('List cleared');
        })}>Clear</button
    >
  {/if}
</div>
{#if sent.length === 0}
  <p class="muted">Nothing has been sent.</p>
{:else}
  <ol class="sent-list" reversed>
    {#each [...sent].reverse() as entry, i (entry.at_ms + ':' + i)}
      <li>
        <details>
          <summary>{dateTime(entry.at_ms)} · {entry.provider} · {entry.model}{entry.truncated ? ' · shortened in this list' : ''}</summary>
          <pre class="sent">{entry.body}</pre>
        </details>
      </li>
    {/each}
  </ol>
{/if}

<h2>Remembered messages</h2>
{#if samples.length === 0}
  <p class="muted">{s.context.style_memory ? 'Nothing remembered yet.' : 'Style memory is off.'}</p>
{:else}
  <button
    type="button"
    onclick={() =>
      ask('Forget all messages?', `All ${samples.length} remembered messages are deleted from this device.`, 'Forget all', async () => {
        await api.deleteStyle(null, null);
        samples = [];
        announce('All remembered messages deleted');
      })}>Forget all</button
  >
  {#each sampleApps as appId (appId)}
    <h3>{appId}</h3>
    <ul class="samples">
      {#each samples.filter((x) => x.app === appId) as sample (sample.id)}
        <li class="row">
          <span class="text">{sample.text}</span>
          <button
            type="button"
            class="quiet"
            aria-label="Forget the message from {dateTime(sample.at_ms)}"
            onclick={() =>
              ask('Forget this message?', 'This remembered message is deleted from this device.', 'Forget message', async () => {
                await api.deleteStyle(sample.id, null);
                samples = samples.filter((x) => x.id !== sample.id);
                announce('Message forgotten');
              })}>Forget</button
          >
        </li>
      {/each}
    </ul>
  {/each}
{/if}

{#if confirm}
  <ConfirmDialog bind:open={confirmOpen} title={confirm.title} message={confirm.message} confirm={confirm.label} onconfirm={confirm.run} />
{/if}

<style>
  .sent-list,
  .samples {
    padding-left: 1.25rem;
  }
  .sent-list li {
    margin-bottom: 0.35rem;
  }
  .samples .text {
    flex: 1;
  }
</style>
