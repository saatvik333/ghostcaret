<script lang="ts">
  // Records one key or chord; with `allowTap`, a modifier pressed and
  // released on its own records a modifier tap. Recording ends after one key,
  // so focus is never trapped.
  import { triggerName } from './format';
  import type { Key, Modifier, Trigger } from './types';

  interface Props {
    label: string;
    value: Trigger | null;
    allowTap?: boolean;
    error?: string;
    onchange: (value: Trigger | null) => void;
  }
  let { label, value, allowTap = false, error, onchange }: Props = $props();

  const id = $props.id();
  let recording = $state(false);
  let tapped: Modifier | null = null;

  const modifiers: Record<string, Modifier> = {
    AltLeft: 'left_alt', AltRight: 'right_alt', ControlLeft: 'left_ctrl', ControlRight: 'right_ctrl',
    ShiftLeft: 'left_shift', ShiftRight: 'right_shift', MetaLeft: 'left_meta', MetaRight: 'right_meta',
  };
  const named: Record<string, Key> = {
    Tab: 'tab', Escape: 'escape', Enter: 'enter', Backspace: 'backspace', ' ': 'space',
    ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down',
    Home: 'home', End: 'end', PageUp: 'page_up', PageDown: 'page_down',
  };

  function keyOf(e: KeyboardEvent): Key | null {
    if (e.key in named) return named[e.key];
    const f = /^F(\d{1,2})$/.exec(e.key);
    if (f) return { f: Number(f[1]) };
    return [...e.key].length === 1 ? { char: e.key } : null;
  }

  function keydown(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.code in modifiers) {
      tapped = allowTap && !e.repeat ? modifiers[e.code] : null;
      return;
    }
    tapped = null;
    const key = keyOf(e);
    if (!key) return;
    recording = false;
    onchange({ chord: { key, mods: { ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey } } });
  }

  function keyup(e: KeyboardEvent) {
    if (!recording || !tapped) return;
    e.preventDefault();
    if (modifiers[e.code] === tapped) {
      const tap = tapped;
      tapped = null;
      recording = false;
      onchange({ modifier_tap: tap });
    }
  }
</script>

<div class="key-row">
  <span class="key-label" id="{id}-l">{label}</span>
  <button
    type="button"
    class="key"
    class:recording
    aria-labelledby="{id}-l {id}-v"
    aria-describedby={error ? `${id}-e` : `${id}-h`}
    class:invalid={!!error}
    onclick={() => (recording = !recording)}
    onkeydown={keydown}
    onkeyup={keyup}
    onblur={() => (recording = false)}
  >
    <span id="{id}-v">{recording ? 'Press the new key…' : value ? triggerName(value) : 'Not set'}</span>
  </button>
  {#if value}
    <button type="button" class="quiet" onclick={() => onchange(null)}>Unbind</button>
  {/if}
  {#if error}
    <p class="error" id="{id}-e">{error}</p>
  {:else}
    <p class="hint visually-hidden" id="{id}-h">
      Activate, then press the key{allowTap ? ', or tap a modifier key on its own' : ''}.
    </p>
  {/if}
</div>
