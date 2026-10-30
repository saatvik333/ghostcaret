<script lang="ts">
  import { onMount } from 'svelte';
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import ConfirmDialog from '../lib/ConfirmDialog.svelte';
  import { lengthLabels } from '../lib/format';
  import { app } from '../lib/store.svelte';
  import type { AppCategory, AppRule, FixedLength, LengthMode, SeenApp } from '../lib/types';

  const s = $derived(app.settings!);
  let seen = $state<SeenApp[]>([]);
  let loadError = $state<string | null>(null);
  let newId = $state('');
  let newIdError = $state<string | null>(null);
  let removing = $state<string | null>(null);
  let resetOpen = $state(false);

  const lengths: LengthMode[] = ['auto', 'word', 'phrase', 'sentence', 'paragraph'];
  const fixed: FixedLength[] = ['word', 'phrase', 'sentence', 'paragraph'];
  const categories: [AppCategory, string][] = [
    ['chat', 'Chat'], ['email', 'Email'], ['document', 'Documents'], ['search', 'Search'],
    ['terminal', 'Terminal'], ['code', 'Code editor'], ['other', 'Other'],
  ];

  onMount(async () => {
    try {
      seen = await api.seenApps();
    } catch (e) {
      loadError = asError(e).message;
    }
  });

  // Apps with rules first, then apps seen while typing.
  const rows = $derived.by(() => {
    const ids = new Set(Object.keys(s.apps));
    const list = Object.keys(s.apps).map((id) => ({ id, name: s.apps[id].name ?? seen.find((a) => a.id === id)?.name ?? id }));
    for (const a of seen) if (!ids.has(a.id)) list.push({ id: a.id, name: a.name });
    return list;
  });

  function rule(id: string): AppRule {
    return s.apps[id] ?? {};
  }

  function update(id: string, name: string, change: Partial<AppRule>) {
    const next: AppRule = { ...rule(id), name, ...change };
    const empty = next.enabled == null && next.length == null && next.ceiling == null && next.category == null;
    if (empty) delete s.apps[id];
    else s.apps[id] = next;
    void app.save();
  }

  const nullable = <T extends string>(v: string): T | null => (v === '' ? null : (v as T));
  const onOff = (v: string): boolean | null => (v === '' ? null : v === 'on');

  function addApp(e: SubmitEvent) {
    e.preventDefault();
    const id = newId.trim();
    if (!id) {
      newIdError = 'Enter the app id, such as org.telegram.desktop or slack.exe.';
      return;
    }
    if (s.apps[id]) {
      newIdError = 'This app already has rules.';
      return;
    }
    newIdError = null;
    s.apps[id] = { name: id, enabled: false };
    newId = '';
    void app.save(`Added ${id}, turned off`);
  }

  function removeRules() {
    if (!removing) return;
    const id = removing;
    delete s.apps[id];
    void app.save(`Rules for ${id} removed`);
  }

  async function pauseApp(id: string, name: string) {
    await api.pause({ kind: 'app', id, name }, { kind: 'until_resumed' });
    announce(`Paused in ${name}`);
  }

  async function resumeApp(id: string, name: string) {
    await api.resume(id);
    announce(`Resumed in ${name}`);
  }
</script>

<h1 id="page-title" tabindex="-1">Apps</h1>
<p class="hint">
  Apps appear here as you type in them. Empty choices use the global settings. Code editors and password managers are off unless you
  turn them on.
</p>
{#if loadError}<p class="error">Could not list apps: {loadError}</p>{/if}
{#if app.errors.apps}<p class="error" role="alert">{app.errors.apps}</p>{/if}

{#if rows.length === 0}
  <p class="muted">No apps yet. Type in any app and it shows up here.</p>
{:else}
  <table>
    <thead>
      <tr>
        <th scope="col">App</th>
        <th scope="col">Suggestions</th>
        <th scope="col">Length</th>
        <th scope="col">Auto up to</th>
        <th scope="col">Kind</th>
        <th scope="col"><span class="visually-hidden">Actions</span></th>
      </tr>
    </thead>
    <tbody>
      {#each rows as row (row.id)}
        {@const r = rule(row.id)}
        <tr>
          <th scope="row">
            {row.name}
            {#if row.name !== row.id}<span class="muted id">{row.id}</span>{/if}
          </th>
          <td>
            <select
              aria-label="Suggestions in {row.name}"
              value={r.enabled == null ? '' : r.enabled ? 'on' : 'off'}
              onchange={(e) => update(row.id, row.name, { enabled: onOff(e.currentTarget.value) })}
            >
              <option value="">Default</option>
              <option value="on">On</option>
              <option value="off">Off</option>
            </select>
          </td>
          <td>
            <select
              aria-label="Length in {row.name}"
              value={r.length ?? ''}
              onchange={(e) => update(row.id, row.name, { length: nullable<LengthMode>(e.currentTarget.value) })}
            >
              <option value="">Default</option>
              {#each lengths as m (m)}<option value={m}>{lengthLabels[m]}</option>{/each}
            </select>
          </td>
          <td>
            <select
              aria-label="Auto ceiling in {row.name}"
              value={r.ceiling ?? ''}
              onchange={(e) => update(row.id, row.name, { ceiling: nullable<LengthMode>(e.currentTarget.value) })}
            >
              <option value="">Default</option>
              {#each fixed as m (m)}<option value={m}>{lengthLabels[m]}</option>{/each}
            </select>
          </td>
          <td>
            <select
              aria-label="Kind of app for {row.name}"
              value={r.category ?? ''}
              onchange={(e) => update(row.id, row.name, { category: nullable<AppCategory>(e.currentTarget.value) })}
            >
              <option value="">Detected</option>
              {#each categories as [c, label] (c)}<option value={c}>{label}</option>{/each}
            </select>
          </td>
          <td class="row">
            {#if app.desktop}
              {#if app.status?.pauses.some((p) => p.app === row.id)}
                <button type="button" class="quiet" onclick={() => resumeApp(row.id, row.name)}>Resume</button>
              {:else}
                <button type="button" class="quiet" onclick={() => pauseApp(row.id, row.name)}>Pause</button>
              {/if}
            {/if}
            {#if s.apps[row.id]}
              <button
                type="button"
                class="quiet"
                onclick={() => {
                  removing = row.id;
                  resetOpen = true;
                }}>Reset</button
              >
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<h2>Turn off an app that is not listed</h2>
<form class="row" onsubmit={addApp}>
  <label for="new-app">App id</label>
  <input
    id="new-app"
    type="text"
    bind:value={newId}
    autocomplete="off"
    spellcheck="false"
    aria-invalid={newIdError ? true : undefined}
    aria-describedby={newIdError ? 'new-app-e' : 'new-app-h'}
  />
  <button type="submit">Turn off</button>
</form>
<p class="hint" id="new-app-h">The Wayland app id, the Windows program name (slack.exe) or the Android package name.</p>
{#if newIdError}<p class="error" id="new-app-e">{newIdError}</p>{/if}

<ConfirmDialog
  bind:open={resetOpen}
  title="Reset this app?"
  message={`The rules for ${removing ?? ''} are removed and it uses the global settings again.`}
  confirm="Reset rules"
  onconfirm={removeRules}
/>

<style>
  .id {
    display: block;
    font-weight: 400;
    font-size: 0.8rem;
  }
</style>
