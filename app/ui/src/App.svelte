<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { onMount } from 'svelte';
  import { live } from './lib/announce.svelte';
  import { asError } from './lib/api';
  import ConfirmDialog from './lib/ConfirmDialog.svelte';
  import { app } from './lib/store.svelte';
  import About from './pages/About.svelte';
  import Apps from './pages/Apps.svelte';
  import Reply from './pages/Reply.svelte';
  import General from './pages/General.svelte';
  import Models from './pages/Models.svelte';
  import Privacy from './pages/Privacy.svelte';
  import Usage from './pages/Usage.svelte';
  import Setup from './setup/Setup.svelte';

  const pages = [
    { id: 'general', label: 'General', component: General },
    { id: 'apps', label: 'Apps', component: Apps },
    { id: 'models', label: 'Models', component: Models },
    { id: 'draft', label: 'Reply & rewrite', component: Reply },
    { id: 'privacy', label: 'Privacy & context', component: Privacy },
    { id: 'usage', label: 'Usage', component: Usage },
    { id: 'about', label: 'About', component: About },
  ] as const;
  type PageId = (typeof pages)[number]['id'];

  let page = $state<PageId>('general');
  let loadError = $state<string | null>(null);
  let confirmClose = $state(false);
  const current = $derived(pages.find((p) => p.id === page)!);
  const inSetup = $derived(app.overview !== null && app.overview.setup !== 'done');
  const draftNames = $derived(Object.values(app.drafts));

  onMount(() => {
    app.load().catch((e) => (loadError = asError(e).message));
    // Closing with unsaved text asks first instead of discarding it.
    const unlisten = getCurrentWindow().onCloseRequested((event) => {
      if (Object.keys(app.drafts).length > 0) {
        event.preventDefault();
        confirmClose = true;
      }
    });
    return () => void unlisten.then((f) => f());
  });

  function closeAnyway() {
    app.drafts = {};
    void getCurrentWindow().destroy();
  }

  function navKey(e: KeyboardEvent, index: number) {
    const delta = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0;
    if (!delta) return;
    e.preventDefault();
    const next = (index + delta + pages.length) % pages.length;
    const buttons = (e.currentTarget as HTMLElement).parentElement?.parentElement?.querySelectorAll('button');
    buttons?.[next]?.focus();
  }
</script>

<div class="live visually-hidden" role="status" aria-live="polite" aria-atomic="true">
  {#key live.seq}<span>{live.message}</span>{/key}
</div>

{#if loadError}
  <main class="content">
    <h1>Ghostcaret</h1>
    <p class="banner-error" role="alert">Could not load settings: {loadError}</p>
  </main>
{:else if !app.overview || !app.settings}
  <main class="content" aria-busy="true"><p class="muted">Loading…</p></main>
{:else if inSetup}
  <Setup />
{:else}
  <div class="shell">
    <nav aria-label="Settings">
      <p class="product">Ghostcaret</p>
      <ul>
        {#each pages as p, i (p.id)}
          <li>
            <button
              type="button"
              aria-current={page === p.id ? 'page' : undefined}
              onclick={() => (page = p.id)}
              onkeydown={(e) => navKey(e, i)}>{p.label}</button
            >
          </li>
        {/each}
      </ul>
      {#if app.status}
        <p class="status" class:attention={app.status.kind !== 'active'}>{app.status.text}</p>
      {/if}
    </nav>
    <main class="content" aria-labelledby="page-title">
      <current.component />
    </main>
  </div>
{/if}

<ConfirmDialog
  bind:open={confirmClose}
  title="Close without saving?"
  message={`Unsaved changes will be lost: ${draftNames.join(', ')}.`}
  confirm="Close and discard"
  cancel="Keep editing"
  onconfirm={closeAnyway}
/>

<style>
  .shell {
    display: grid;
    grid-template-columns: 13rem 1fr;
    min-height: 100vh;
  }
  nav {
    border-right: 1px solid var(--line);
    padding: 1rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .product {
    font-weight: 700;
    padding: 0 0.5rem;
    margin: 0 0 0.5rem;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.1rem;
  }
  nav button {
    width: 100%;
    text-align: left;
    border-color: transparent;
    background: transparent;
  }
  nav button[aria-current='page'] {
    background: var(--panel);
    border-color: var(--line);
    font-weight: 600;
  }
  .status {
    margin-top: auto;
    padding: 0 0.5rem;
    font-size: 0.85rem;
    color: var(--muted);
  }
  .status.attention {
    color: var(--text);
    font-weight: 600;
  }
  .content {
    padding: 1.25rem 1.75rem 3rem;
    max-width: 52rem;
  }
  @media (max-width: 640px) {
    .shell {
      grid-template-columns: 1fr;
    }
    nav {
      border-right: none;
      border-bottom: 1px solid var(--line);
    }
  }
</style>
