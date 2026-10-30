<script lang="ts">
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import ConfirmDialog from '../lib/ConfirmDialog.svelte';
  import { count, percent } from '../lib/format';
  import type { Counts, Usage } from '../lib/types';

  let days = $state(7);
  let usage = $state<Usage | null>(null);
  let error = $state<string | null>(null);
  let clearOpen = $state(false);

  $effect(() => {
    const d = days;
    api.usage(d).then(
      (u) => {
        usage = u;
        error = null;
      },
      (e) => (error = asError(e).message),
    );
  });

  const total = $derived.by((): Counts => {
    const t: Counts = { shown: 0, accepted: 0, accepted_whole: 0, dismissed: 0, chars: 0, keystrokes_saved: 0, drafts: 0 };
    for (const d of usage?.days ?? []) for (const k of Object.keys(t) as (keyof Counts)[]) t[k] += d[k];
    return t;
  });
  const maxSaved = $derived(Math.max(1, ...(usage?.days ?? []).map((d) => d.keystrokes_saved)));

  async function clear() {
    try {
      await api.clearUsage();
      usage = { apps: [], days: [] };
      announce('Usage stats deleted');
    } catch (e) {
      error = asError(e).message;
    }
  }
</script>

<h1 id="page-title" tabindex="-1">Usage</h1>
<p class="hint">Counted on this device, by day and app. No text is ever stored here.</p>

<div class="row">
  <label for="range">Period</label>
  <select id="range" bind:value={days}>
    <option value={1}>Today</option>
    <option value={7}>Last 7 days</option>
    <option value={30}>Last 30 days</option>
    <option value={90}>Last 90 days</option>
  </select>
</div>
{#if error}<p class="error" role="alert">{error}</p>{/if}

{#if usage}
  <dl class="totals">
    <div><dt>Keystrokes saved</dt><dd>{count(total.keystrokes_saved)}</dd></div>
    <div><dt>Characters accepted</dt><dd>{count(total.chars)}</dd></div>
    <div><dt>Suggestions accepted</dt><dd>{count(total.accepted)} of {count(total.shown)} ({percent(total.accepted, total.shown)})</dd></div>
    <div><dt>Drafts inserted</dt><dd>{count(total.drafts)}</dd></div>
  </dl>

  <h2>By app</h2>
  {#if usage.apps.length === 0}
    <p class="muted">Nothing counted in this period.</p>
  {:else}
    <table>
      <thead>
        <tr>
          <th scope="col">App</th>
          <th scope="col" class="num">Shown</th>
          <th scope="col" class="num">Accepted</th>
          <th scope="col" class="num">Rate</th>
          <th scope="col" class="num">Keystrokes saved</th>
          <th scope="col" class="num">Drafts</th>
        </tr>
      </thead>
      <tbody>
        {#each usage.apps as a (a.app)}
          <tr>
            <th scope="row">{a.name}</th>
            <td class="num">{count(a.shown)}</td>
            <td class="num">{count(a.accepted)}</td>
            <td class="num">{percent(a.accepted, a.shown)}</td>
            <td class="num">{count(a.keystrokes_saved)}</td>
            <td class="num">{count(a.drafts)}</td>
          </tr>
        {/each}
      </tbody>
    </table>

    <h2>By day</h2>
    <table>
      <thead>
        <tr><th scope="col">Day</th><th scope="col" class="num">Keystrokes saved</th><th scope="col"><span class="visually-hidden">Chart</span></th></tr>
      </thead>
      <tbody>
        {#each usage.days as d (d.day)}
          <tr>
            <th scope="row">{d.day}</th>
            <td class="num">{count(d.keystrokes_saved)}</td>
            <td aria-hidden="true"><span class="bar" style:width="{(d.keystrokes_saved / maxSaved) * 12}rem"></span></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  <p><button type="button" onclick={() => (clearOpen = true)}>Delete usage stats</button></p>
{/if}

<ConfirmDialog
  bind:open={clearOpen}
  title="Delete usage stats?"
  message="All daily counts and the list of apps seen are deleted from this device. Settings and app rules stay."
  confirm="Delete stats"
  onconfirm={clear}
/>

<style>
  .totals {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: 0.5rem;
    margin: 1rem 0;
  }
  .totals div {
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 0.5rem 0.75rem;
    background: var(--panel);
  }
  dt {
    color: var(--muted);
    font-size: 0.85rem;
  }
  dd {
    margin: 0;
    font-size: 1.15rem;
    font-variant-numeric: tabular-nums;
  }
</style>
