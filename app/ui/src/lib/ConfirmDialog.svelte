<script lang="ts">
  // A modal alert dialog for destructive actions. It opens with focus on the
  // safe choice and returns focus to the control that opened it.
  interface Props {
    open: boolean;
    title: string;
    /** States exactly what confirming will do. */
    message: string;
    confirm: string;
    cancel?: string;
    onconfirm: () => void;
  }
  let { open = $bindable(), title, message, confirm, cancel = 'Cancel', onconfirm }: Props = $props();

  const id = $props.id();
  let dialog: HTMLDialogElement;
  let safe: HTMLButtonElement;
  let returnTo: HTMLElement | null = null;

  $effect(() => {
    if (open && !dialog.open) {
      returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      dialog.showModal();
      safe.focus();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  function closed() {
    open = false;
    returnTo?.focus();
  }

  function accept() {
    dialog.close();
    onconfirm();
  }
</script>

<dialog bind:this={dialog} role="alertdialog" aria-labelledby="{id}-t" aria-describedby="{id}-m" onclose={closed}>
  <h2 id="{id}-t">{title}</h2>
  <p id="{id}-m">{message}</p>
  <div class="actions">
    <button type="button" bind:this={safe} onclick={() => dialog.close()}>{cancel}</button>
    <button type="button" class="danger" onclick={accept}>{confirm}</button>
  </div>
</dialog>
