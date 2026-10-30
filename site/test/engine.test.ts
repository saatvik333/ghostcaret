import { describe, expect, test } from "bun:test";
import scenarios from "../src/scenarios.json";
import { prepare, suggest } from "../src/engine.js";

const list = scenarios.map(prepare);
const email = scenarios[0].starter;
type Suggestion = { state: string; text: string; first: string; words: number; canMore: boolean };
// engine.js is untyped JavaScript; this is its documented result shape.
const at = (value: string, opts = {}) => suggest(list, value, opts) as unknown as Suggestion;

describe("matching", () => {
  test("an empty field stays quiet", () => {
    expect(at("").state).toBe("empty");
    expect(at("  \n").state).toBe("empty");
  });

  test("text that is not a canned opening gets nothing", () => {
    expect(at("Dear Sir or Madam").state).toBe("off-script");
  });

  test("matching ignores case and differences in whitespace", () => {
    expect(at("hi sam, thanks", { length: "word" }).first).toBe(" for");
    expect(at("Hi Sam,\nThanks", { length: "word" }).first).toBe(" for");
  });

  test("a finished text has nothing left to suggest", () => {
    expect(at(scenarios[1].text).state).toBe("end");
  });
});

describe("length", () => {
  test("each length stops where the product says it does", () => {
    expect(at(email, { length: "word" }).text).toBe(" and");
    expect(at(email, { length: "phrase" }).text).toBe(" and the structure works well,");
    expect(at(email, { length: "sentence" }).text).toBe(" and the structure works well, though the second section could be shorter.");
    expect(at(email, { length: "paragraph" }).text).toBe(
      " and the structure works well, though the second section could be shorter. Could we move our call to Thursday afternoon? I'm free after two.",
    );
  });

  test("paragraph stops at the end of the paragraph", () => {
    expect(at(email, { length: "paragraph" }).text.includes("\n")).toBe(false);
  });

  test("a phrase stops before a conjunction once it has two words", () => {
    expect(at(scenarios[1].starter, { length: "phrase" }).text).toBe(" after lunch");
  });

  test("show more adds one phrase at a time and never leaves the paragraph", () => {
    const base = at(email, { length: "word" });
    const once = at(email, { length: "word", more: 1 });
    expect(once.text).toBe(" and the structure works well,");
    expect(once.text.length).toBeGreaterThan(base.text.length);
    const all = at(email, { length: "word", more: 20 });
    expect(all.text).toBe(at(email, { length: "paragraph" }).text);
    expect(all.canMore).toBe(false);
    expect(base.canMore).toBe(true);
  });

  test("auto is cut by the sentence ceiling in chat and by confidence", () => {
    const chat = at(scenarios[1].starter, { length: "auto", boldness: 5 });
    expect(chat.text.endsWith("doc.")).toBe(true);
    const careful = at(email, { length: "auto", boldness: 1 });
    const bold = at(email, { length: "auto", boldness: 5 });
    expect(bold.words).toBeGreaterThan(careful.words);
  });

  test("auto never ends on a dangling function word once it shows more than one word", () => {
    for (let b = 1; b <= 5; b++) {
      const s = at(email, { length: "auto", boldness: b });
      if (s.state === "ready" && s.words > 1) expect(/\b(the|and|a|of|to|though)$/i.test(s.text)).toBe(false);
    }
  });

  test("auto can be quiet when it is not sure", () => {
    expect(at("Hi Sam,", { length: "auto", boldness: 1 }).state).toBe("quiet");
  });
});

describe("taking text", () => {
  test("typing the rest of a word shortens the suggestion without a leading space", () => {
    const s = at("Hi Sam,\n\nThanks for sending the draft ov", { length: "word" });
    expect(s.first).toBe("er.");
  });

  test("whitespace the user typed replaces the canned whitespace", () => {
    const s = at("Hi Sam, ", { length: "word" });
    expect(s.first).toBe("Thanks");
  });

  test("taking every word one by one reaches the end of the text", () => {
    let value = scenarios[1].starter;
    for (let i = 0; i < 200; i++) {
      const s = at(value, { length: "word" });
      if (s.state !== "ready") break;
      value += s.first;
    }
    expect(value).toBe(scenarios[1].text);
  });
});
