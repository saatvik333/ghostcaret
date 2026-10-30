// Draws the 1200x630 share image from site.config.json and renders it to PNG.
//   bun site/tools/og.ts
// Needs rsvg-convert. The build does not run this: it only checks that the committed
// image was made from the current config (see ogStamp), so a rename cannot ship a stale image.
import { createHash } from "node:crypto";
import { join } from "node:path";

export type OgConfig = { name: string; headline: string; headlineGhost: string };

const INK = "#ecebe5";
const GHOST = "#8c8c97";
const AMBER = "#f0a63a";
const MONO = "'JetBrains Mono','JetBrainsMono Nerd Font',ui-monospace,monospace";
const SANS = "'Noto Sans','Inter','DejaVu Sans',sans-serif";
const FONT_SIZE = 88;
const MAX_CHARS = 21;

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** Greedy word wrap that remembers which characters are typed text and which are the faint suggestion. */
function lines(cfg: OgConfig): { text: string; ghost: boolean }[][] {
  const full = cfg.headline + cfg.headlineGhost;
  const out: { text: string; ghost: boolean }[][] = [];
  let current = "";
  let start = 0;
  const flush = (end: number) => {
    const runs: { text: string; ghost: boolean }[] = [];
    for (let i = start; i < end; i++) {
      const ghost = i >= cfg.headline.length;
      const last = runs[runs.length - 1];
      if (last && last.ghost === ghost) last.text += full[i];
      else runs.push({ text: full[i], ghost });
      if (i === cfg.headline.length - 1) runs.push({ text: "\u0000", ghost: false }); // caret goes here
    }
    out.push(runs);
  };
  let pos = 0;
  for (const word of full.split(" ")) {
    const next = current ? `${current} ${word}` : word;
    if (next.length > MAX_CHARS && current) {
      flush(pos - 1);
      start = pos;
      current = word;
    } else {
      current = next;
    }
    pos += word.length + 1;
  }
  flush(full.length);
  return out;
}

export function ogSvg(cfg: OgConfig): string {
  const rows = lines(cfg);
  const top = 250;
  const step = 100;
  const text = rows
    .map((runs, i) => {
      const spans = runs
        .map((r) =>
          r.text === "\u0000"
            ? `<tspan fill="${AMBER}" font-weight="400">|</tspan>`
            : `<tspan fill="${r.ghost ? GHOST : INK}">${esc(r.text).replace(/^ /, "&#160;")}</tspan>`,
        )
        .join("");
      return `<text x="72" y="${top + i * step}" xml:space="preserve">${spans}</text>`;
    })
    .join("\n    ");
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630" role="img">
  <title>${esc(cfg.name)}: ${esc(cfg.headline + cfg.headlineGhost)} In development, not released yet.</title>
  <rect width="1200" height="630" fill="#111215"/>
  <circle cx="84" cy="84" r="9" fill="${AMBER}"/>
  <text x="108" y="94" font-family="${MONO}" font-size="28" fill="${AMBER}">In development. Not released yet.</text>
  <g font-family="${SANS}" font-size="${FONT_SIZE}" font-weight="700" letter-spacing="-1.5">
    ${text}
  </g>
  <g transform="translate(72 530)">
    <rect width="48" height="48" rx="11" fill="#17171b" stroke="#2c2f37"/>
    <rect x="13" y="10" width="4.5" height="28" rx="2.2" fill="${AMBER}"/>
    <rect x="22" y="22" width="14" height="4.5" rx="2.2" fill="#b9b9c2" opacity="0.75"/>
    <rect x="39" y="22" width="4.5" height="4.5" rx="2.2" fill="#b9b9c2" opacity="0.4"/>
  </g>
  <text x="140" y="565" font-family="${MONO}" font-size="34" font-weight="700" fill="${INK}">${esc(cfg.name)}</text>
</svg>
`;
}

/** Hash of everything the image depends on. The build compares it with assets/og.stamp. */
export function ogStamp(cfg: OgConfig): string {
  return createHash("sha256").update(ogSvg(cfg)).digest("hex").slice(0, 16);
}

if (import.meta.main) {
  const root = join(import.meta.dir, "..");
  const cfg = (await Bun.file(join(root, "site.config.json")).json()) as OgConfig;
  const svgPath = join(root, "assets", "og.svg");
  const pngPath = join(root, "assets", "og.png");
  await Bun.write(svgPath, ogSvg(cfg));
  const run = Bun.spawnSync(["rsvg-convert", "-w", "1200", "-h", "630", svgPath, "-o", pngPath]);
  if (run.exitCode !== 0) {
    console.error(`rsvg-convert failed: ${run.stderr.toString()}`);
    process.exit(1);
  }
  await Bun.write(join(root, "assets", "og.stamp"), `${ogStamp(cfg)}\n`);
  console.log(`Wrote assets/og.svg, assets/og.png and assets/og.stamp (${ogStamp(cfg)})`);
}
