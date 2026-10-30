// Canned suggestion engine for the landing-page demo. There is no model and no
// network: it looks up what the typed text would continue into in a few fixed
// texts, then cuts that continuation to the chosen length. The build uses it
// for the static examples and the browser bundles it for the live demo, so
// both always agree.

const FUNCTION_WORDS = new Set(
  "a an the and or but so if of to in on at for with from by as is it its be i we you he she they this that these those which while though because then than my our your their could would should can will may might must are was were has have had do does".split(" "),
);
const CONJUNCTIONS = new Set("and but so because though while which then or if".split(" "));
const bare = (w) => w.toLowerCase().replace(/^[^a-z0-9']+|[^a-z0-9']+$/g, "");
const endsSentence = (w) => /[.!?]["')\]]*$/.test(w);
const endsClause = (w) => /[,;:]$/.test(w) || endsSentence(w);

/** Index a scenario once: word spans, a made-up per-word confidence, and a whitespace-free copy for matching. */
export function prepare(scenario) {
  const text = scenario.text;
  const tokens = [];
  const re = /(\s*)(\S+)/g;
  let m;
  let pos = 0; // words since the start of the current sentence
  let prevEnd = 0;
  while ((m = re.exec(text))) {
    const word = m[2];
    const start = m.index + m[1].length;
    const para = m[1].includes("\n");
    if (para) pos = 0;
    let conf = 10 - Math.floor(pos * 0.4);
    if (pos === 0) conf -= 4;
    if (FUNCTION_WORDS.has(bare(word))) conf += 2;
    if (pos > 0 && ((/^[A-Z]/.test(word) && bare(word) !== "i") || /\d/.test(word))) conf -= 2;
    tokens.push({ word, start, end: start + word.length, prevEnd, para, conf: Math.min(10, Math.max(1, conf)) });
    prevEnd = start + word.length;
    pos = endsSentence(word) ? 0 : pos + 1;
  }
  const idx = [];
  for (let i = 0; i < text.length; i++) if (!/\s/.test(text[i])) idx.push(i);
  const compact = idx.map((i) => text[i]).join("").toLowerCase();
  return { ...scenario, tokens, idx, compact };
}

/** Position in the canned text just after the typed text, or -1 when the typed text is not a prefix of it. */
function locate(p, value) {
  const typed = value.replace(/\s+/g, "").toLowerCase();
  if (!typed || !p.compact.startsWith(typed)) return -1;
  return p.idx[typed.length - 1] + 1;
}

function phraseEnds(units, paraEnd) {
  const ends = [];
  let start = 0;
  while (start < paraEnd) {
    let end = start;
    while (end < paraEnd) {
      const w = units[end].tok.word;
      if (end - start >= 2 && CONJUNCTIONS.has(bare(w))) break;
      end++;
      if (endsClause(w) || end - start === 6) {
        // Do not strand the last word or two of a clause after the 6-word cap.
        if (!endsClause(w)) {
          for (let j = 0; j < 2 && end + j < paraEnd; j++) {
            if (endsClause(units[end + j].tok.word)) {
              end += j + 1;
              break;
            }
          }
        }
        break;
      }
    }
    ends.push(end);
    start = end;
  }
  return ends;
}

/**
 * What the demo shows after `value`.
 * state: "empty" | "off-script" | "end" | "quiet" | "ready"
 * A "ready" result has `text` (all of it), `first` (the next word, with its
 * leading whitespace), `words`, and `canMore`.
 */
export function suggest(list, value, { length = "auto", boldness = 3, more = 0 } = {}) {
  if (!value.trim()) return { state: "empty" };
  let p = null;
  let pos = -1;
  for (const candidate of list) {
    pos = locate(candidate, value);
    if (pos >= 0) {
      p = candidate;
      break;
    }
  }
  if (!p) return { state: "off-script" };

  // Whitespace the user already typed stands in for the canned whitespace.
  let from = pos;
  if (/\s$/.test(value)) while (from < p.text.length && /\s/.test(p.text[from])) from++;
  const units = [];
  for (const tok of p.tokens) {
    if (tok.end <= from) continue;
    const first = units.length === 0;
    units.push({ tok, from: first ? from : tok.prevEnd, to: tok.end, brk: !first && tok.para });
  }
  if (!units.length) return { state: "end" };

  let paraEnd = units.length;
  for (let i = 1; i < units.length; i++) {
    if (units[i].brk) {
      paraEnd = i;
      break;
    }
  }
  let sentenceEnd = paraEnd;
  for (let i = 0; i < paraEnd; i++) {
    if (endsSentence(units[i].tok.word)) {
      sentenceEnd = i + 1;
      break;
    }
  }
  const ends = phraseEnds(units, paraEnd);

  let k;
  if (length === "word") k = 1;
  else if (length === "phrase") k = ends[0];
  else if (length === "sentence") k = sentenceEnd;
  else if (length === "paragraph") k = paraEnd;
  else {
    const ceiling = p.ceiling === "sentence" ? sentenceEnd : p.ceiling === "word" ? 1 : paraEnd;
    const threshold = 8 - boldness;
    k = 0;
    while (k < ceiling && units[k].tok.conf >= threshold) k++;
    // Do not stop on a dangling "the" or "and".
    while (k > 1 && k < ceiling && !endsClause(units[k - 1].tok.word) && FUNCTION_WORDS.has(bare(units[k - 1].tok.word))) k--;
    if (k === 0) return { state: "quiet" };
  }
  for (let i = 0; i < more; i++) {
    const next = ends.find((e) => e > k);
    k = next === undefined ? paraEnd : next;
  }

  return {
    state: "ready",
    text: p.text.slice(from, units[k - 1].to),
    first: p.text.slice(from, units[0].to),
    words: k,
    canMore: k < paraEnd,
  };
}
