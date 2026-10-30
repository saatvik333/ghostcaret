<script lang="ts">
  import ConfirmDialog from '../lib/ConfirmDialog.svelte';
  import { triggerName } from '../lib/format';
  import { app } from '../lib/store.svelte';

  const s = $derived(app.settings!);
  let name = $state('');
  let instruction = $state('');
  let formError = $state<string | null>(null);
  let removing = $state<number | null>(null);
  let removeOpen = $state(false);

  $effect(() => {
    app.setDraft('custom-action', name.trim() || instruction.trim() ? 'the new rewrite action' : null);
    return () => app.setDraft('custom-action', null);
  });

  async function add(e: SubmitEvent) {
    e.preventDefault();
    if (!name.trim() || !instruction.trim()) {
      formError = 'Give the action a name and an instruction.';
      return;
    }
    formError = null;
    s.reply.custom_actions.push({ name: name.trim(), instruction: instruction.trim() });
    if (await app.save(`Action “${name.trim()}” added`)) {
      name = '';
      instruction = '';
    }
  }

  function remove() {
    if (removing === null) return;
    const [gone] = s.reply.custom_actions.splice(removing, 1);
    void app.save(`Action “${gone.name}” removed`);
  }
</script>

<h1 id="page-title" tabindex="-1">Reply & rewrite</h1>
<p class="hint">
  {s.keys.reply ? triggerName(s.keys.reply) : 'The reply & rewrite key'} drafts a reply to the conversation on screen, or rewrites the
  text you selected. Nothing is inserted until you accept it, and nothing is ever sent for you.
</p>

<label class="check">
  <input type="checkbox" bind:checked={s.reply.enabled} onchange={() => app.save()} />
  <span>Use reply & rewrite</span>
</label>
<label class="check">
  <input type="checkbox" bind:checked={s.reply.auto_offer} onchange={() => app.save()} disabled={!s.reply.enabled} />
  <span>Offer a draft when I pause in an empty reply box</span>
</label>

<h2>Rewrite actions</h2>
<p class="hint">Built in: Fix, Rephrase, Shorten, Expand, Translate and Change tone. Your own actions follow them.</p>
{#if s.reply.custom_actions.length > 0}
  <table>
    <thead>
      <tr><th scope="col">Name</th><th scope="col">Instruction</th><th scope="col"><span class="visually-hidden">Actions</span></th></tr>
    </thead>
    <tbody>
      {#each s.reply.custom_actions as action, i (i)}
        <tr>
          <th scope="row">
            <label class="visually-hidden" for="an-{i}">Name of action {i + 1}</label>
            <input id="an-{i}" type="text" bind:value={action.name} onchange={() => app.save()} />
          </th>
          <td>
            <label class="visually-hidden" for="ai-{i}">Instruction for {action.name}</label>
            <input id="ai-{i}" type="text" class="wide" bind:value={action.instruction} onchange={() => app.save()} />
          </td>
          <td>
            <button
              type="button"
              onclick={() => {
                removing = i;
                removeOpen = true;
              }}>Remove</button
            >
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<form class="panel" onsubmit={add} aria-labelledby="add-action-h">
  <h3 id="add-action-h">Add an action</h3>
  <div class="field">
    <label for="act-name">Name</label>
    <input id="act-name" type="text" bind:value={name} placeholder="Make it formal" />
  </div>
  <div class="field">
    <label for="act-instr">Instruction</label>
    <textarea id="act-instr" rows="2" bind:value={instruction} placeholder="Rewrite in a formal, polite register."></textarea>
  </div>
  {#if formError}<p class="error" role="alert">{formError}</p>{/if}
  <button type="submit">Add action</button>
</form>

<ConfirmDialog
  bind:open={removeOpen}
  title="Remove this action?"
  message={`“${removing !== null ? (s.reply.custom_actions[removing]?.name ?? '') : ''}” is removed from the rewrite menu.`}
  confirm="Remove action"
  onconfirm={remove}
/>

<style>
  .wide {
    width: 100%;
  }
</style>
