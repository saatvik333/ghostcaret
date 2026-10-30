<script lang="ts">
  import { onMount } from 'svelte';
  import { api, asError, events } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import ConfirmDialog from '../lib/ConfirmDialog.svelte';
  import { bytes } from '../lib/format';
  import { app } from '../lib/store.svelte';
  import type { LocalModel, ModelChoice, ModelsView, ProviderConfig, SentEntry } from '../lib/types';
  import ProviderForm from './ProviderForm.svelte';

  const s = $derived(app.settings!);
  const localAvailable = $derived(app.overview?.local_available ?? false);
  let view = $state<ModelsView | null>(null);
  let rowErrors = $state<Record<string, string>>({});
  let adding = $state(false);
  let editing = $state<string | null>(null);
  let sent = $state<SentEntry[]>([]);
  let confirm = $state<{ title: string; message: string; label: string; run: () => void } | null>(null);
  let confirmOpen = $state(false);
  let filePath = $state('');
  let fileError = $state<string | null>(null);

  async function refresh() {
    if (localAvailable) view = await api.localModels();
    sent = await api.sentLog();
  }

  onMount(() => {
    void refresh();
    const done = events.modelDone((d) => {
      if (d.error) rowErrors[d.id] = d.error;
      else delete rowErrors[d.id];
      announce(d.cancelled ? 'Download stopped' : d.error ? `Download failed: ${d.error}` : 'Download finished');
      void refresh();
    });
    return () => void done.then((f) => f());
  });

  function choiceKey(c: ModelChoice): string {
    return c.kind === 'none' ? 'none' : c.kind === 'local' ? `local:${c.model}` : `provider:${c.provider}`;
  }

  function parseChoice(v: string): ModelChoice {
    if (v.startsWith('local:')) return { kind: 'local', model: v.slice(6) };
    if (v.startsWith('provider:')) return { kind: 'provider', provider: v.slice(9) };
    return { kind: 'none' };
  }

  // Installed curated models, the current custom file and every provider.
  const options = $derived.by(() => {
    const list: { value: string; label: string }[] = [{ value: 'none', label: 'None' }];
    for (const m of view?.models ?? []) {
      if (m.installed) list.push({ value: `local:${m.id}`, label: `${m.name} (on this device)` });
    }
    for (const c of [s.autocomplete_model, s.draft_model]) {
      const k = choiceKey(c);
      if (c.kind === 'local' && !list.some((o) => o.value === k)) {
        const known = view?.models.find((m) => m.id === c.model);
        list.push({ value: k, label: known ? `${known.name} (not downloaded)` : c.model });
      }
    }
    for (const p of s.providers) list.push({ value: `provider:${p.id}`, label: `${p.label}: ${p.model}` });
    return list;
  });

  function setChoice(feature: 'autocomplete_model' | 'draft_model', value: string) {
    s[feature] = parseChoice(value);
    void app.save();
  }

  function progressFor(m: LocalModel) {
    return app.download && app.download.id === m.id ? app.download : null;
  }

  async function download(m: LocalModel) {
    delete rowErrors[m.id];
    try {
      await api.downloadModel(m.id);
      announce(`Downloading ${m.name}`);
      if (view) view.downloading = m.id;
    } catch (e) {
      rowErrors[m.id] = asError(e).message;
    }
  }

  function askDelete(m: LocalModel) {
    confirm = {
      title: `Delete ${m.name}?`,
      message: m.in_use
        ? `This frees ${bytes(m.size)}. Features that use it will have no model until you choose another.`
        : `This frees ${bytes(m.size)}. You can download it again later.`,
      label: 'Delete model',
      run: async () => {
        try {
          await api.deleteModel(m.id);
          announce(`${m.name} deleted`);
          const o = await api.overview();
          app.adopt(o.settings);
          await refresh();
        } catch (e) {
          rowErrors[m.id] = asError(e).message;
        }
      },
    };
    confirmOpen = true;
  }

  function askRemove(p: ProviderConfig) {
    confirm = {
      title: `Remove ${p.label}?`,
      message: 'Its key is deleted from the keychain, and features that use it stop until you choose another model.',
      label: 'Remove provider',
      run: async () => {
        try {
          app.adopt(await api.removeProvider(p.id));
          app.keys = app.keys.filter((k) => k !== p.id);
          announce(`${p.label} removed`);
        } catch (e) {
          announce(`Could not remove: ${asError(e).message}`);
        }
      },
    };
    confirmOpen = true;
  }

  async function useFile(e: SubmitEvent) {
    e.preventDefault();
    const path = filePath.trim();
    try {
      await api.checkModelFile(path);
      fileError = null;
      s.autocomplete_model = { kind: 'local', model: path };
      if (await app.save('Model file in use for autocomplete')) filePath = '';
    } catch (err) {
      fileError = asError(err).message;
    }
  }

  function onCapInput(e: Event & { currentTarget: HTMLInputElement }) {
    const v = e.currentTarget.value.trim();
    s.daily_request_cap = v === '' ? null : Math.max(0, Math.floor(Number(v)));
    void app.save();
  }
</script>

<h1 id="page-title" tabindex="-1">Models</h1>

<h2>Which model does what</h2>
<div class="field">
  <label for="auto-model">Autocomplete</label>
  <select
    id="auto-model"
    value={choiceKey(s.autocomplete_model)}
    onchange={(e) => setChoice('autocomplete_model', e.currentTarget.value)}
    aria-invalid={app.errors.autocomplete_model ? true : undefined}
    aria-describedby={app.errors.autocomplete_model ? 'auto-model-e' : undefined}
  >
    {#each options as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
  </select>
  {#if app.errors.autocomplete_model}<p class="error" id="auto-model-e">{app.errors.autocomplete_model}</p>{/if}
</div>
<div class="field">
  <label for="draft-model">Reply & rewrite</label>
  <select
    id="draft-model"
    value={choiceKey(s.draft_model)}
    onchange={(e) => setChoice('draft_model', e.currentTarget.value)}
    aria-invalid={app.errors.draft_model ? true : undefined}
    aria-describedby={app.errors.draft_model ? 'draft-model-e' : 'draft-model-h'}
  >
    {#each options as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
  </select>
  <p class="hint" id="draft-model-h">A common setup is a local model for autocomplete and a cloud key for drafts.</p>
  {#if app.errors.draft_model}<p class="error" id="draft-model-e">{app.errors.draft_model}</p>{/if}
</div>

{#if localAvailable}
  <h2>Models on this device</h2>
  {#if view}
    <p class="hint">
      {view.ram_bytes ? `This device has ${bytes(view.ram_bytes)} of memory. ` : ''}Files are checked against their published hash before
      use, and downloads resume after interruption.
    </p>
    <table>
      <thead>
        <tr>
          <th scope="col">Model</th>
          <th scope="col" class="num">Size</th>
          <th scope="col">License</th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each view.models as m (m.id)}
          {@const p = progressFor(m)}
          <tr>
            <th scope="row">
              {m.name}
              {#if m.recommended}<span class="badge">Recommended here</span>{/if}
              {#if m.in_use}<span class="badge">In use</span>{/if}
              <span class="muted block">{m.tier === 'small' ? 'Small' : m.tier === 'standard' ? 'Standard' : 'For comparison'}, needs {m.min_ram_gb} GB memory</span>
              {#if rowErrors[m.id]}<span class="error block" id="err-{m.id}">{rowErrors[m.id]}</span>{/if}
            </th>
            <td class="num">{bytes(m.size)}</td>
            <td>{m.license}</td>
            <td>
              {#if p}
                <div class="stack">
                  <progress max={p.total} value={p.downloaded} aria-label="Downloading {m.name}"></progress>
                  <span class="muted">{bytes(p.downloaded)} of {bytes(p.total)}</span>
                  <button type="button" onclick={() => api.cancelDownload()}>Stop</button>
                </div>
              {:else if m.installed}
                <button type="button" onclick={() => askDelete(m)}>Delete</button>
              {:else}
                <button
                  type="button"
                  onclick={() => download(m)}
                  disabled={app.download !== null}
                  aria-describedby={rowErrors[m.id] ? `err-${m.id}` : undefined}>Download</button
                >
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="hint">Stored in {view.dir}</p>
  {:else}
    <p class="muted">Loading…</p>
  {/if}

  <h3>Another GGUF file</h3>
  <form class="row" onsubmit={useFile}>
    <label for="gguf" class="visually-hidden">Path to a GGUF file</label>
    <input
      id="gguf"
      type="text"
      bind:value={filePath}
      placeholder="/path/to/model.gguf"
      spellcheck="false"
      size="40"
      aria-invalid={fileError ? true : undefined}
      aria-describedby={fileError ? 'gguf-e' : undefined}
    />
    <button type="submit">Use for autocomplete</button>
  </form>
  {#if fileError}<p class="error" id="gguf-e">{fileError}</p>{/if}
{/if}

<h2>Your own key</h2>
{#if s.providers.length > 0}
  <table>
    <thead>
      <tr>
        <th scope="col">Provider</th>
        <th scope="col">Model</th>
        <th scope="col">Key</th>
        <th scope="col"><span class="visually-hidden">Actions</span></th>
      </tr>
    </thead>
    <tbody>
      {#each s.providers as p (p.id)}
        <tr>
          <th scope="row">{p.label}<span class="muted block">{p.kind.type === 'open_ai_compatible' ? p.kind.base_url : p.kind.type === 'anthropic' ? 'Anthropic' : 'Gemini'}</span></th>
          <td>{p.model}</td>
          <td>{app.keys.includes(p.id) ? 'Stored' : 'None'}</td>
          <td class="row">
            <button type="button" onclick={() => (editing = editing === p.id ? null : p.id)} aria-expanded={editing === p.id}>Edit</button>
            <button type="button" onclick={() => askRemove(p)}>Remove</button>
          </td>
        </tr>
        {#if editing === p.id}
          <tr>
            <td colspan="4">
              <ProviderForm existing={p} onsaved={() => (editing = null)} oncancel={() => (editing = null)} />
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
{/if}
{#if adding}
  <ProviderForm onsaved={() => (adding = false)} oncancel={() => (adding = false)} />
{:else}
  <button type="button" onclick={() => (adding = true)}>Add a provider</button>
{/if}

<h2>Cloud use</h2>
<p>{sent.length} {sent.length === 1 ? 'request' : 'requests'} sent this session. The Privacy page shows exactly what was sent.</p>
<div class="field">
  <label for="cap">Daily request cap</label>
  <input
    id="cap"
    type="number"
    min="0"
    value={s.daily_request_cap ?? ''}
    onchange={onCapInput}
    placeholder="No cap"
    aria-describedby="cap-h"
  />
  <p class="hint" id="cap-h">When reached, cloud requests stop until tomorrow and an installed local model takes over autocomplete.</p>
</div>

{#if confirm}
  <ConfirmDialog bind:open={confirmOpen} title={confirm.title} message={confirm.message} confirm={confirm.label} onconfirm={confirm.run} />
{/if}

<style>
  .block {
    display: block;
    font-weight: 400;
  }
  .badge {
    margin-left: 0.35rem;
  }
</style>
