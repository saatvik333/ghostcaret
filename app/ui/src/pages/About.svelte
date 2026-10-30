<script lang="ts">
  import { onMount } from 'svelte';
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import { clock, millis } from '../lib/format';
  import { app } from '../lib/store.svelte';
  import type { Timing } from '../lib/types';

  let timings = $state<Timing[]>([]);
  let error = $state<string | null>(null);

  async function refresh() {
    if (!app.desktop) return;
    try {
      timings = (await api.timings()).reverse();
    } catch (e) {
      error = asError(e).message;
    }
  }
  onMount(refresh);

  function quantile(values: number[], q: number): number {
    if (values.length === 0) return 0;
    const sorted = [...values].sort((a, b) => a - b);
    return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
  }
  const shown = $derived(timings.filter((t) => t.shown));

  async function rerunSetup() {
    try {
      await api.setSetupStep('welcome');
      if (app.overview) app.overview.setup = 'welcome';
    } catch (e) {
      announce(`Could not start setup: ${asError(e).message}`);
    }
  }
</script>

<h1 id="page-title" tabindex="-1">About</h1>
<p>Ghostcaret {app.overview?.version} · free software under the Mozilla Public License 2.0.</p>
<p>No account and no telemetry. Network access happens only for model downloads and the provider you configured.</p>
<p>Source code, issues and discussions: <span class="url">github.com/saatvik333/ghostcaret</span></p>
<p><button type="button" onclick={rerunSetup}>Run setup again</button></p>

{#if app.desktop}
  <h2>Recent suggestion timings</h2>
  <p class="hint">
    From the last keystroke to the suggestion being handed to the display, for recent requests. Kept in memory on this device.
  </p>
  <button type="button" onclick={refresh}>Refresh</button>
  {#if error}<p class="error">{error}</p>{/if}
  {#if timings.length === 0}
    <p class="muted">No requests yet.</p>
  {:else}
    <p>
      Shown suggestions: {shown.length}. Total p50 {millis(quantile(shown.map((t) => t.total_us), 0.5))}, p95
      {millis(quantile(shown.map((t) => t.total_us), 0.95))}; model p50 {millis(quantile(shown.map((t) => t.decode_us), 0.5))}.
    </p>
    <table>
      <thead>
        <tr>
          <th scope="col">Time</th>
          <th scope="col" class="num">Pause</th>
          <th scope="col" class="num">Build</th>
          <th scope="col" class="num">Queue</th>
          <th scope="col" class="num">Model</th>
          <th scope="col" class="num">Display</th>
          <th scope="col" class="num">Total</th>
          <th scope="col">Result</th>
        </tr>
      </thead>
      <tbody>
        {#each timings as t (t.id)}
          <tr>
            <td>{clock(t.at_ms)}</td>
            <td class="num">{millis(t.wait_us)}</td>
            <td class="num">{millis(t.build_us)}</td>
            <td class="num">{millis(t.queue_us)}</td>
            <td class="num">{millis(t.decode_us)}</td>
            <td class="num">{t.shown ? millis(t.render_us) : '–'}</td>
            <td class="num">{millis(t.total_us)}</td>
            <td>{t.cancelled ? 'Cancelled' : t.shown ? 'Shown' : 'Not shown'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
{/if}
