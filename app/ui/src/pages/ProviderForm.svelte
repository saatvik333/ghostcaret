<script lang="ts">
  // Add or edit a cloud provider: address, key, test and model list. The key
  // goes to the system keychain and is never read back; when editing, an
  // empty key field keeps the stored key.
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import { app } from '../lib/store.svelte';
  import type { ModelInfo, ProviderConfig, ProviderKind } from '../lib/types';

  interface Props {
    existing?: ProviderConfig;
    /** Called after the provider and its key are saved. */
    onsaved: (config: ProviderConfig) => void;
    oncancel?: () => void;
  }
  let { existing, onsaved, oncancel }: Props = $props();

  const presets = [
    { label: 'OpenAI', url: 'https://api.openai.com/v1' },
    { label: 'OpenRouter', url: 'https://openrouter.ai/api/v1' },
    { label: 'Groq', url: 'https://api.groq.com/openai/v1' },
    { label: 'Together', url: 'https://api.together.xyz/v1' },
    { label: 'Ollama', url: 'http://localhost:11434/v1' },
    { label: 'LM Studio', url: 'http://localhost:1234/v1' },
    { label: 'llama-server', url: 'http://localhost:8080/v1' },
    { label: 'vLLM', url: 'http://localhost:8000/v1' },
  ];

  const id = $props.id();
  // The form edits its own copy; `existing` only seeds it.
  const seed = (() => existing)();
  let type = $state<ProviderKind['type']>(seed?.kind.type ?? 'open_ai_compatible');
  let label = $state(seed?.label ?? '');
  let baseUrl = $state(seed && seed.kind.type === 'open_ai_compatible' ? seed.kind.base_url : '');
  let model = $state(seed?.model ?? '');
  let key = $state('');
  let models = $state<ModelInfo[]>([]);
  let errors = $state<Record<string, string>>({});
  let testResult = $state<string | null>(null);
  let busy = $state<'test' | 'models' | 'save' | null>(null);

  const hasKey = $derived(seed ? app.keys.includes(seed.id) : false);
  const dirty = $derived(
    key !== '' ||
      label !== (seed?.label ?? '') ||
      model !== (seed?.model ?? '') ||
      (type === 'open_ai_compatible' && baseUrl !== (seed?.kind.type === 'open_ai_compatible' ? seed.kind.base_url : '')),
  );
  $effect(() => {
    app.setDraft(`provider-${id}`, dirty ? 'the provider form' : null);
    return () => app.setDraft(`provider-${id}`, null);
  });

  function slug(text: string): string {
    const base = text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'provider';
    const taken = new Set(app.settings!.providers.map((p) => p.id));
    let candidate = base;
    for (let n = 2; taken.has(candidate); n++) candidate = `${base}-${n}`;
    return candidate;
  }

  function config(): ProviderConfig {
    const kind: ProviderKind =
      type === 'open_ai_compatible' ? { type, base_url: baseUrl.trim().replace(/\/+$/, '') } : { type };
    const name = label.trim() || (type === 'anthropic' ? 'Anthropic' : type === 'gemini' ? 'Gemini' : 'Provider');
    return { id: seed?.id ?? slug(name), label: name, kind, model: model.trim() };
  }

  function check(): boolean {
    const e: Record<string, string> = {};
    if (type === 'open_ai_compatible' && !/^https?:\/\/\S+$/.test(baseUrl.trim())) {
      e.base_url = 'Enter an address that starts with https:// or http://.';
    }
    if (type !== 'open_ai_compatible' && !key.trim() && !hasKey) e.key = 'Paste your API key.';
    errors = e;
    return Object.keys(e).length === 0;
  }

  function fail(e: unknown, fallback: string) {
    const err = asError(e);
    errors = { ...errors, [err.key && err.key !== 'test' ? err.key : fallback]: err.message };
    return err.message;
  }

  async function test() {
    if (!check()) return;
    if (!model.trim()) {
      errors = { ...errors, model: 'Choose a model to test.' };
      return;
    }
    busy = 'test';
    testResult = null;
    try {
      await api.testProvider(config(), key.trim() || null);
      testResult = 'The provider answered.';
      announce('Test passed: the provider answered');
    } catch (e) {
      announce(`Test failed: ${fail(e, 'test')}`);
    } finally {
      busy = null;
    }
  }

  async function loadModels() {
    if (!check()) return;
    busy = 'models';
    try {
      models = await api.providerModels(config(), key.trim() || null);
      announce(`${models.length} models available`);
    } catch (e) {
      announce(`Could not list models: ${fail(e, 'model')}`);
    } finally {
      busy = null;
    }
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!check()) return;
    if (!model.trim()) {
      errors = { ...errors, model: 'Choose a model.' };
      return;
    }
    busy = 'save';
    const c = config();
    const s = app.settings!;
    const index = s.providers.findIndex((p) => p.id === c.id);
    if (index >= 0) s.providers[index] = c;
    else s.providers.push(c);
    try {
      if (!(await app.save(`${c.label} saved`))) {
        errors = { ...errors, general: app.errors.general ?? 'Not saved.' };
        return;
      }
      if (key.trim()) {
        try {
          await api.setApiKey(c.id, key.trim());
          if (!app.keys.includes(c.id)) app.keys.push(c.id);
          key = '';
        } catch (err) {
          announce(`Key not stored: ${fail(err, 'key')}`);
          return;
        }
      }
      onsaved(c);
    } finally {
      busy = null;
    }
  }
</script>

<form class="panel stack" onsubmit={save} aria-labelledby="{id}-t">
  <h3 id="{id}-t">{seed ? `Edit ${seed.label}` : 'Add a provider'}</h3>
  {#if errors.general}<p class="error" role="alert">{errors.general}</p>{/if}

  <div class="field">
    <label for="{id}-type">Type</label>
    <select id="{id}-type" bind:value={type} disabled={seed !== undefined}>
      <option value="open_ai_compatible">OpenAI-compatible (OpenAI, OpenRouter, Groq, Ollama, LM Studio…)</option>
      <option value="anthropic">Anthropic</option>
      <option value="gemini">Google Gemini</option>
    </select>
  </div>

  {#if type === 'open_ai_compatible'}
    <div class="field">
      <label for="{id}-url">Address</label>
      <input
        id="{id}-url"
        type="url"
        bind:value={baseUrl}
        list="{id}-presets"
        placeholder="https://api.openai.com/v1"
        spellcheck="false"
        aria-invalid={errors.base_url ? true : undefined}
        aria-describedby={errors.base_url ? `${id}-url-e` : `${id}-url-h`}
      />
      <datalist id="{id}-presets">
        {#each presets as p (p.url)}<option value={p.url}>{p.label}</option>{/each}
      </datalist>
      <p class="hint" id="{id}-url-h">The API base address, ending in /v1 for most servers.</p>
      {#if errors.base_url}<p class="error" id="{id}-url-e">{errors.base_url}</p>{/if}
    </div>
  {/if}

  <div class="field">
    <label for="{id}-label">Name</label>
    <input id="{id}-label" type="text" bind:value={label} placeholder="Shown in these settings" />
  </div>

  <div class="field">
    <label for="{id}-key">API key</label>
    <input
      id="{id}-key"
      type="password"
      bind:value={key}
      autocomplete="off"
      spellcheck="false"
      placeholder={hasKey ? 'Stored in the keychain; paste to replace' : type === 'open_ai_compatible' ? 'Optional for local servers' : ''}
      aria-invalid={errors.key ? true : undefined}
      aria-describedby={errors.key ? `${id}-key-e` : `${id}-key-h`}
    />
    <p class="hint" id="{id}-key-h">Kept in the system keychain. It is never shown again or written to settings.</p>
    {#if errors.key}<p class="error" id="{id}-key-e">{errors.key}</p>{/if}
  </div>

  <div class="field">
    <label for="{id}-model">Model</label>
    <div class="row">
      <input
        id="{id}-model"
        type="text"
        bind:value={model}
        list="{id}-models"
        spellcheck="false"
        aria-invalid={errors.model ? true : undefined}
        aria-describedby={errors.model ? `${id}-model-e` : undefined}
      />
      <button type="button" onclick={loadModels} disabled={busy !== null}>
        {busy === 'models' ? 'Loading…' : 'List models'}
      </button>
    </div>
    <datalist id="{id}-models">
      {#each models as m (m.id)}<option value={m.id}>{m.name}</option>{/each}
    </datalist>
    {#if errors.model}<p class="error" id="{id}-model-e">{errors.model}</p>{/if}
  </div>

  <div class="row">
    <button type="button" onclick={test} disabled={busy !== null} aria-describedby="{id}-test">
      {busy === 'test' ? 'Testing…' : 'Test'}
    </button>
    <span id="{id}-test">
      {#if errors.test}<span class="error">{errors.test}</span>{:else if testResult}{testResult}{/if}
    </span>
  </div>

  <div class="actions">
    {#if oncancel}<button type="button" onclick={oncancel}>Cancel</button>{/if}
    <button type="submit" class="primary" disabled={busy !== null}>{busy === 'save' ? 'Saving…' : 'Save'}</button>
  </div>
</form>
