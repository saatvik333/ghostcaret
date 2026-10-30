<script lang="ts">
  import { api, asError } from '../lib/api';
  import { announce } from '../lib/announce.svelte';
  import { clock, lengthLabels } from '../lib/format';
  import KeyRecorder from '../lib/KeyRecorder.svelte';
  import { app } from '../lib/store.svelte';
  import type { Bindings, FixedLength, KeyProblem, LengthMode, PauseFor, Slot, Trigger } from '../lib/types';

  const s = $derived(app.settings!);
  const errors = $derived(app.errors);
  let keyProblems = $state<KeyProblem[]>([]);

  const lengths: LengthMode[] = ['auto', 'word', 'phrase', 'sentence', 'paragraph'];
  const fixed: FixedLength[] = ['word', 'phrase', 'sentence', 'paragraph'];
  const ceilings = [
    ['chat', 'Chat'],
    ['email', 'Email'],
    ['document', 'Documents and notes'],
    ['search', 'Search boxes'],
    ['terminal', 'Terminals'],
    ['other', 'Other apps'],
  ] as const;
  const slots: { slot: Slot; label: string }[] = [
    { slot: 'take_word', label: 'Take next word' },
    { slot: 'take_all', label: 'Take whole suggestion' },
    { slot: 'show_more', label: 'Show more' },
    { slot: 'next_alternative', label: 'Next alternative' },
    { slot: 'prev_alternative', label: 'Previous alternative' },
    { slot: 'dismiss', label: 'Dismiss' },
    { slot: 'flow_draft', label: 'Reply & rewrite' },
  ];

  const boldnessText = $derived(
    s.boldness < 0.34 ? 'careful' : s.boldness < 0.67 ? 'balanced' : 'bold',
  );
  const pausedAll = $derived(app.status?.pauses.find((p) => p.app === null) ?? null);

  async function setKey(slot: Slot, value: Trigger | null) {
    const keys: Bindings = { ...$state.snapshot(s.keys) };
    if (slot === 'flow_draft') keys.flow_draft = value;
    else keys[slot] = value && 'chord' in value ? value.chord : null;
    keyProblems = await api.checkKeys(keys);
    if (keyProblems.length > 0) {
      announce(`Not changed: ${keyProblems[0].message}`);
      return;
    }
    s.keys = keys;
    await app.save('Key saved');
  }

  function keyValue(slot: Slot): Trigger | null {
    if (slot === 'flow_draft') return s.keys.flow_draft;
    const chord = s.keys[slot];
    return chord ? { chord } : null;
  }

  async function pause(length: PauseFor) {
    try {
      await api.pause({ kind: 'all' }, length);
      announce('Paused');
    } catch (e) {
      announce(`Could not pause: ${asError(e).message}`);
    }
  }

  async function resume() {
    await api.resume(null);
    announce('Resumed');
  }
</script>

<h1 id="page-title" tabindex="-1">General</h1>

{#if app.overview?.settings_error}
  <div class="banner-error" role="alert">
    <strong>Settings file problem.</strong>
    {app.overview.settings_error} Saving any setting writes a new file.
  </div>
{/if}
{#if errors.general}
  <p class="error" role="alert">{errors.general}</p>
{/if}

{#if app.desktop}
  <section class="panel" aria-labelledby="pause-h">
    <h2 id="pause-h">Status</h2>
    <p>{app.status?.text}</p>
    {#if pausedAll}
      <p class="muted">
        {pausedAll.until_ms ? `Paused until ${clock(pausedAll.until_ms)}.` : 'Paused until you resume.'}
      </p>
      <button type="button" class="primary" onclick={resume}>Resume</button>
    {:else}
      <div class="row" role="group" aria-label="Pause everything">
        <span>Pause:</span>
        <button type="button" onclick={() => pause({ kind: 'minutes', minutes: 15 })}>15 minutes</button>
        <button type="button" onclick={() => pause({ kind: 'minutes', minutes: 60 })}>1 hour</button>
        <button type="button" onclick={() => pause({ kind: 'until_resumed' })}>Until resumed</button>
      </div>
    {/if}
  </section>
{/if}

<h2>Suggestions</h2>
<label class="check">
  <input type="checkbox" bind:checked={s.enabled} onchange={() => app.save()} />
  <span>Suggest as I type<span class="muted">Turn off to stop all suggestions until you turn this back on.</span></span>
</label>

<fieldset>
  <legend>Length</legend>
  <p class="hint" id="length-hint">The most a suggestion may show. Auto stops where the model becomes unsure.</p>
  <div class="segmented">
    {#each lengths as mode (mode)}
      <label>
        <input type="radio" name="length" value={mode} bind:group={s.length} onchange={() => app.save()} aria-describedby="length-hint" />
        {lengthLabels[mode]}
      </label>
    {/each}
  </div>
</fieldset>

<div class="field">
  <label for="boldness">Boldness</label>
  <div class="row">
    <span class="muted" aria-hidden="true">Careful</span>
    <input
      id="boldness"
      type="range"
      min="0"
      max="1"
      step="0.05"
      bind:value={s.boldness}
      onchange={() => app.save()}
      aria-valuetext="{Math.round(s.boldness * 100)}%, {boldnessText}"
      aria-describedby={errors.boldness ? 'boldness-e' : 'boldness-h'}
      aria-invalid={errors.boldness ? true : undefined}
    />
    <span class="muted" aria-hidden="true">Bold</span>
  </div>
  <p class="hint" id="boldness-h">How sure the model must be before it shows anything.</p>
  {#if errors.boldness}<p class="error" id="boldness-e">{errors.boldness}</p>{/if}
</div>

{#if s.length === 'paragraph'}
  <div class="field">
    <label for="pwords">Word cap for Paragraph</label>
    <input
      id="pwords"
      type="number"
      min="1"
      max="400"
      bind:value={s.paragraph_words}
      onchange={() => app.save()}
      aria-invalid={errors.paragraph_words ? true : undefined}
      aria-describedby={errors.paragraph_words ? 'pwords-e' : undefined}
    />
    {#if errors.paragraph_words}<p class="error" id="pwords-e">{errors.paragraph_words}</p>{/if}
  </div>
{/if}

<div class="field">
  <label for="pause-ms">Pause before suggesting (milliseconds)</label>
  <input
    id="pause-ms"
    type="number"
    min="0"
    max="2000"
    step="10"
    bind:value={s.pause_ms}
    onchange={() => app.save()}
    aria-invalid={errors.pause_ms ? true : undefined}
    aria-describedby={errors.pause_ms ? 'pause-ms-e' : 'pause-ms-h'}
  />
  <p class="hint" id="pause-ms-h">How long you stop typing before a suggestion is requested.</p>
  {#if errors.pause_ms}<p class="error" id="pause-ms-e">{errors.pause_ms}</p>{/if}
</div>

<label class="check">
  <input type="checkbox" bind:checked={s.learn_length} onchange={() => app.save()} />
  <span
    >Learn my length<span class="muted"
      >Suggestions get longer in apps where you take whole suggestions, and shorter where you dismiss them.</span
    ></span
  >
</label>

<h2>Auto length by kind of app</h2>
<p class="hint">How far Auto may go. Each app can override this on the Apps page.</p>
<table>
  <tbody>
    {#each ceilings as [key, label] (key)}
      <tr>
        <th scope="row"><label for="ceil-{key}">{label}</label></th>
        <td>
          <select id="ceil-{key}" bind:value={s.ceilings[key]} onchange={() => app.save()}>
            {#each fixed as mode (mode)}<option value={mode}>Up to a {lengthLabels[mode].toLowerCase()}</option>{/each}
          </select>
        </td>
      </tr>
    {/each}
  </tbody>
</table>
{#if errors.ceilings}<p class="error">{errors.ceilings}</p>{/if}

<h2>Keys</h2>
<p class="hint">Accept keys are only taken while a suggestion is showing; otherwise apps get them as usual.</p>
{#each slots as { slot, label } (slot)}
  <KeyRecorder
    {label}
    value={keyValue(slot)}
    allowTap={slot === 'flow_draft'}
    error={keyProblems.find((p) => p.slot === slot)?.message ?? errors[`keys.${slot}`]}
    onchange={(v) => setKey(slot, v)}
  />
{/each}

<h2>Appearance</h2>
<fieldset>
  <legend>Suggestion text</legend>
  <div class="segmented">
    <label><input type="radio" name="ghost" value="faint" bind:group={s.appearance.style} onchange={() => app.save()} /> Faint</label>
    <label
      ><input type="radio" name="ghost" value="high_contrast" bind:group={s.appearance.style} onchange={() => app.save()} /> High
      contrast</label
    >
  </div>
</fieldset>
<label class="check">
  <input type="checkbox" bind:checked={s.appearance.announce} onchange={() => app.save()} />
  <span>Announce suggestions to screen readers</span>
</label>

<h2>Resources</h2>
<div class="field">
  <label for="unload">Unload the local model after idle minutes</label>
  <input id="unload" type="number" min="0" max="1440" bind:value={s.unload_after_minutes} onchange={() => app.save()} aria-describedby="unload-h" />
  <p class="hint" id="unload-h">0 keeps it loaded. It also unloads when the system runs low on memory.</p>
</div>
