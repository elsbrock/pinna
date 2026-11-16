<!-- ConfirmDialog: a native modal <dialog> that says what happens and how to
     undo it, with Cancel and the action. Escape and Cancel close it. -->
<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    open = $bindable(false),
    title,
    action,
    danger = false,
    ready = true,
    confirm,
    children,
  }: {
    open?: boolean;
    title: string;
    /** The confirming button's words: the action itself ("Restart"). */
    action: string;
    danger?: boolean;
    /** False keeps the action disabled, until something in the dialog is filled in. */
    ready?: boolean;
    confirm: () => unknown;
    children: Snippet;
  } = $props();

  let dialog = $state<HTMLDialogElement>();
  const titleId = `dlg-${Math.random().toString(36).slice(2, 8)}`;

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} class="pn-dialog" aria-labelledby={titleId} onclose={() => (open = false)}>
  <form
    class="pn-dialog-body"
    method="dialog"
    onsubmit={(e) => {
      if (e.submitter?.getAttribute("value") === "confirm") void confirm();
    }}
  >
    <h2 id={titleId} class="pn-type-heading">{title}</h2>
    {@render children()}
    <div class="pn-dialog-actions">
      <!-- Not a submit button: Enter confirms, once the action is ready. -->
      <button class="pn-btn" type="button" onclick={() => dialog?.close()}>Cancel</button>
      <button class="pn-btn {danger ? 'pn-btn-danger' : 'pn-btn-primary'}" value="confirm" disabled={!ready}>
        {action}
      </button>
    </div>
  </form>
</dialog>
