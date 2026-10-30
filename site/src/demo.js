// The landing-page demo. Canned suggestions only: no model, no network.
import scenarios from "./scenarios.json";
import { prepare, suggest } from "./engine.js";

const list = scenarios.map(prepare);
const root = document.querySelector("[data-demo]");
if (root) start(root);

function start(root) {
  const $ = (selector) => root.querySelector(selector);
  const field = $("[data-field]");
  const typed = $("[data-typed]");
  const ghost = $("[data-ghost]");
  const status = $("[data-status]");
  const live = $("[data-live]");
  const bold = $("[data-bold]");
  const boldNote = $("[data-bold-note]");
  const actions = {
    word: $('[data-action="word"]'),
    all: $('[data-action="all"]'),
    more: $('[data-action="more"]'),
    dismiss: $('[data-action="dismiss"]'),
  };
  const boldWords = ["1 of 5, most careful", "2 of 5", "3 of 5", "4 of 5", "5 of 5, boldest"];

  let shown = null; // the suggestion currently drawn, or null
  let more = 0; // how many times "show more" was pressed for this spot
  let dismissed = false;
  let composing = false;
  let touched = false; // stay silent for screen readers until the first interaction
  let announceTimer = 0;
  let lastSpoken = "";

  const length = () => root.querySelector('input[name="length"]:checked').value;
  const clip = (text, max) => {
    const words = text.trim().split(/\s+/);
    return words.length > max ? `${words.slice(0, max).join(" ")} and ${words.length - max} more words` : words.join(" ");
  };
  const plural = (n) => `${n} word${n === 1 ? "" : "s"}`;

  function say(text, now) {
    clearTimeout(announceTimer);
    const write = () => {
      live.textContent = "";
      requestAnimationFrame(() => {
        live.textContent = text;
      });
    };
    if (now) write();
    else announceTimer = setTimeout(write, 900);
  }

  function update() {
    const value = field.value;
    const atEnd = field.selectionStart === value.length && field.selectionEnd === value.length;
    const inside = root.contains(document.activeElement);
    const result = suggest(list, value, { length: length(), boldness: +bold.value, more });
    const ready = result.state === "ready";
    shown = ready && atEnd && inside && !dismissed ? result : null;

    const midWord = shown && /\S$/.test(value) && /^\S/.test(shown.text);
    typed.textContent = value + (midWord ? "\u200b" : "");
    ghost.textContent = shown ? shown.text : "";

    const isAuto = length() === "auto";
    bold.disabled = !isAuto;
    bold.setAttribute("aria-valuetext", boldWords[bold.value - 1]);
    boldNote.textContent = isAuto ? "Boldness changes how far Auto goes." : "Boldness only applies when the length is Auto.";

    actions.word.setAttribute("aria-disabled", String(!shown));
    actions.all.setAttribute("aria-disabled", String(!shown));
    actions.dismiss.setAttribute("aria-disabled", String(!shown));
    actions.more.setAttribute("aria-disabled", String(!(shown && shown.canMore)));

    let message;
    let spoken;
    if (dismissed && ready) {
      message = spoken = "Dismissed. Type another letter and it can come back.";
    } else if (ready && !atEnd) {
      message = spoken = "Suggestions show when the cursor is at the end of the text.";
    } else if (shown) {
      message = `Suggestion: ${plural(shown.words)}. Tab takes “${shown.first.trim()}”.`;
      spoken = `Suggestion: ${clip(shown.text, 14)}. Tab takes the next word, backtick takes all.`;
    } else if (result.state === "quiet") {
      message = spoken = "Quiet: the model is not sure enough at this boldness. Turn boldness up or pick a length.";
    } else if (result.state === "off-script") {
      message = spoken = "No suggestion. The demo only knows three texts, so pick a starter.";
    } else if (result.state === "end") {
      message = spoken = "That is the end of the canned text. Clear the field or pick another starter.";
    } else {
      message = spoken = "No suggestion yet. Pick a starter or type an opening line.";
    }
    status.textContent = message;
    if (touched && spoken !== lastSpoken) say(spoken, false);
    lastSpoken = spoken;
  }

  let frame = 0;
  const later = () => {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(update);
  };

  // Insert through the editing command so the browser's own undo removes the whole chunk.
  function put(text, replaceAll) {
    field.focus({ preventScroll: true });
    if (replaceAll) field.select();
    else field.setSelectionRange(field.value.length, field.value.length);
    const ok = text ? document.execCommand("insertText", false, text) : document.execCommand("delete");
    if (!ok) {
      field.value = replaceAll ? text : field.value + text;
      field.dispatchEvent(new Event("input", { bubbles: true }));
    }
    field.setSelectionRange(field.value.length, field.value.length);
  }

  function take(kind) {
    touched = true;
    if (!shown) return say("There is no suggestion to take.", true);
    const text = kind === "word" ? shown.first : shown.text;
    say(`Took “${clip(text, 10)}”.`, true);
    put(text, false);
  }

  function showMore() {
    touched = true;
    if (!shown) return say("There is no suggestion to extend.", true);
    if (!shown.canMore) return say("That is all of this paragraph.", true);
    more++;
    update();
  }

  function dismiss() {
    touched = true;
    if (!shown) return;
    dismissed = true;
    update();
    say("Dismissed.", true);
  }

  field.addEventListener("keydown", (e) => {
    if (e.isComposing || composing || !shown) return;
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey;
    if (e.key === "Tab" && plain) {
      e.preventDefault();
      take("word");
    } else if (e.key === "`" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      take("all");
    } else if (e.key === "ArrowRight" && e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
      e.preventDefault();
      showMore();
    } else if (e.key === "Escape" && plain) {
      e.preventDefault();
      dismiss();
    }
  });

  field.addEventListener("input", () => {
    touched = true;
    more = 0;
    dismissed = false;
    if (!composing) update();
  });
  field.addEventListener("compositionstart", () => {
    composing = true;
  });
  field.addEventListener("compositionend", () => {
    composing = false;
    update();
  });
  field.addEventListener("click", later);
  field.addEventListener("keyup", later);
  field.addEventListener("select", later);
  document.addEventListener("selectionchange", () => {
    if (document.activeElement === field) later();
  });
  root.addEventListener("focusin", later);
  root.addEventListener("focusout", () => setTimeout(update, 0));

  const keepFocus = (e) => e.preventDefault(); // pressing a button must not take focus out of the field
  for (const [kind, button] of Object.entries(actions)) {
    button.addEventListener("mousedown", keepFocus);
    button.addEventListener("click", () => {
      if (kind === "word" || kind === "all") take(kind);
      else if (kind === "more") showMore();
      else dismiss();
    });
  }

  for (const radio of root.querySelectorAll('input[name="length"]')) {
    radio.addEventListener("change", () => {
      touched = true;
      more = 0;
      dismissed = false;
      update();
    });
  }
  bold.addEventListener("input", () => {
    touched = true;
    more = 0;
    update();
  });

  for (const button of root.querySelectorAll("[data-starter]")) {
    button.addEventListener("click", () => {
      touched = true;
      const scenario = scenarios.find((s) => s.id === button.dataset.starter);
      more = 0;
      dismissed = false;
      put(scenario.starter, true);
    });
  }
  $("[data-clear]").addEventListener("click", () => {
    touched = true;
    more = 0;
    dismissed = false;
    put("", true);
    say("Cleared.", true);
  });

  update();
  root.hidden = false;
}
