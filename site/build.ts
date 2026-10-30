// Renders the templates in site/src with site/site.config.json into site/dist.
//   bun site/build.ts            local build: missing deploy values only warn
//   bun site/build.ts --strict   what CI runs: missing or stale values fail the build
//   --config <file> and --out <dir> override the defaults (for previews)
import { gzipSync } from "node:zlib";
import { mkdir, readdir, rm } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import scenarios from "./src/scenarios.json";
import { prepare, suggest } from "./src/engine.js";
import { ogStamp } from "./tools/og.ts";

const root = import.meta.dir;
const strict = Bun.argv.includes("--strict");
const flag = (name: string) => {
  const at = Bun.argv.indexOf(name);
  return at >= 0 ? Bun.argv[at + 1] : undefined;
};
const outDir = resolve(flag("--out") ?? join(root, "dist"));
const configPath = resolve(flag("--config") ?? join(root, "site.config.json"));

type Config = {
  name: string;
  nameIsWorkingTitle: boolean;
  headline: string;
  headlineGhost: string;
  description: string;
  replyName: string;
  aiDisclosure: string;
  xHandle: string;
  repoUrl: string;
  testerUrl: string;
  siteUrl: string;
  statusDate: string;
  license: string;
  licensePublished: boolean;
};

const problems: string[] = [];
const warnings: string[] = [];
const need = (ok: boolean, message: string, deploy = false) => {
  if (ok) return;
  (deploy && !strict ? warnings : problems).push(message);
};

const cfg = (await Bun.file(configPath).json()) as Config;
for (const key of ["name", "headline", "headlineGhost", "description", "replyName", "statusDate", "license"] as const) {
  need(typeof cfg[key] === "string" && cfg[key].trim() !== "", `site.config.json: "${key}" is required`);
}
const isHttps = (u: string) => /^https:\/\/[^\s/]+\.[^\s/]+/.test(u);
need(/^[A-Za-z0-9_]{1,15}$/.test(cfg.xHandle), `site.config.json: "xHandle" must be an X handle without the @`);
need(isHttps(cfg.siteUrl), `site.config.json: "siteUrl" must be the https URL the site is served from (needed for canonical, share card and sitemap)`, true);
need(!cfg.repoUrl || isHttps(cfg.repoUrl), `site.config.json: "repoUrl" must be an https URL or empty`);
need(!cfg.testerUrl || isHttps(cfg.testerUrl), `site.config.json: "testerUrl" must be an https URL or empty`);
need(/^\d{4}-\d{2}-\d{2}$/.test(cfg.statusDate), `site.config.json: "statusDate" must be YYYY-MM-DD`);
need(typeof cfg.aiDisclosure === "string", `site.config.json: "aiDisclosure" must be a string (empty hides it)`);
need(typeof cfg.licensePublished === "boolean", `site.config.json: "licensePublished" must be true or false (true once the repository has a LICENSE file)`);
if (cfg.nameIsWorkingTitle) warnings.push(`"${cfg.name}" is still marked as a working title ("nameIsWorkingTitle"), so the page says the name is not decided`);

const siteUrl = cfg.siteUrl ? cfg.siteUrl.replace(/\/?$/, "/") : "";
const xUrl = `https://x.com/${cfg.xHandle}`;
const testerIsX = !cfg.testerUrl;
const hostLabel = (url: string) => {
  const host = new URL(url).hostname.replace(/^www\./, "");
  return host === "github.com" ? "GitHub" : host;
};

// The committed share image must come from the current name and headline.
const stampFile = Bun.file(join(root, "assets", "og.stamp"));
const stamp = (await stampFile.exists()) ? (await stampFile.text()).trim() : "";
need(stamp === ogStamp(cfg), `assets/og.png is out of date for this config: run "bun site/tools/og.ts" and commit the result`, true);

if (problems.length) fail();

// Static examples use the same engine as the live demo.
const prepared = scenarios.map(prepare);
const email = scenarios[0];
const example = (length: string, boldness = 3) => {
  const s = suggest(prepared, email.starter, { length, boldness }) as { state: string; text: string };
  return s.state === "ready" ? s.text.replace(/\s+/g, " ").trim() : "";
};

const esc = (s: unknown) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const starterButtons = scenarios
  .map((s) => `<button type="button" class="chip" data-starter="${esc(s.id)}">${esc(s.label)}</button>`)
  .join("\n          ");

const exampleFigure = `<figure class="example">
        <figcaption>The demo's email text, after typing “${esc(email.starter.replace(/\s+/g, " "))}”, suggests this at each length:</figcaption>
        <dl class="rows">
          ${(["word", "phrase", "sentence", "paragraph"] as const)
            .map((l) => `<div><dt>${l[0].toUpperCase() + l.slice(1)}</dt><dd><span class="ex">${esc(example(l))}</span></dd></div>`)
            .join("\n          ")}
          <div><dt>Auto</dt><dd><span class="ex">${esc(example("auto"))}</span> (boldness in the middle)</dd></div>
        </dl>
      </figure>`;

const tagline = cfg.headline + cfg.headlineGhost;
const longDate = (iso: string) =>
  new Date(`${iso}T00:00:00Z`).toLocaleDateString("en-GB", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });
const dateLong = longDate(cfg.statusDate);

// The comparison facts live in src/comparison.json, each row linked to the vendor page it came from.
type Row = { name: string; url: string; price: string; platforms: string; linux: string; openSource: string; localModel: string; ownKey: string; account: string };
const comparison = (await Bun.file(join(root, "src", "comparison.json")).json()) as { readOn: string; rows: Row[] };
const ours = {
  price: "Free, with no word or reply limits",
  platforms: "Linux (Wayland) first, then Windows; macOS later",
  linux: "Linux: the first target",
  openSource: cfg.licensePublished ? `Yes, ${cfg.license}` : `${cfg.license}, planned`,
  localModel: "Yes, any GGUF model",
  ownKey: "Yes: OpenAI-compatible endpoints, Anthropic, Gemini",
  account: "No account",
};
// The table turns into stacked cards on narrow screens. display changes drop native table semantics in
// some browsers, so the roles are written out and each cell carries its column name.
const cell = (r: Pick<Row, "price" | "platforms" | "linux" | "openSource" | "localModel" | "ownKey" | "account">) =>
  `<td role="cell" data-label="Price">${esc(r.price)}</td>
            <td role="cell" data-label="Platforms"><div>${esc(r.platforms)}<span class="linux">${esc(r.linux)}</span></div></td>
            <td role="cell" data-label="Open source">${esc(r.openSource)}</td>
            <td role="cell" data-label="Local model">${esc(r.localModel)}</td>
            <td role="cell" data-label="Your own API key">${esc(r.ownKey)}</td>
            <td role="cell" data-label="Account">${esc(r.account)}</td>`;
const comparisonTable = `<div class="table-scroll" role="region" aria-labelledby="compare-caption" tabindex="0">
        <table class="compare-table" role="table">
          <caption id="compare-caption">Inline text-completion tools, as listed on each vendor's own pages on ${esc(longDate(comparison.readOn))}</caption>
          <thead role="rowgroup">
            <tr role="row">
              <th scope="col" role="columnheader">Tool</th>
              <th scope="col" role="columnheader">Price</th>
              <th scope="col" role="columnheader">Platforms</th>
              <th scope="col" role="columnheader">Open source</th>
              <th scope="col" role="columnheader">Local model</th>
              <th scope="col" role="columnheader">Your own API key</th>
              <th scope="col" role="columnheader">Account</th>
            </tr>
          </thead>
          <tbody role="rowgroup">
          <tr role="row" class="is-ours">
            <th scope="row" role="rowheader">${esc(cfg.name)}<span class="tag tag-dev">Planned · in development</span></th>
            ${cell(ours)}
          </tr>
          ${comparison.rows
            .map((r) => `<tr role="row">
            <th scope="row" role="rowheader"><a href="${esc(r.url)}">${esc(r.name)}</a></th>
            ${cell(r)}
          </tr>`)
            .join("\n          ")}
          </tbody>
        </table>
      </div>`;

const ctx: Record<string, unknown> = {
  ...cfg,
  siteUrl,
  tagline,
  title: `${cfg.name}: ${tagline.replace(/\.$/, "")}`,
  ogAlt: `${cfg.name}: ${tagline} In development, not released yet.`,
  xUrl,
  testerIsX,
  testerHref: cfg.testerUrl || xUrl,
  linkSites: testerIsX ? "X" : `${hostLabel(cfg.testerUrl)} and X`,
  statusDateLong: dateLong,
  comparedOnLong: longDate(comparison.readOn),
  starterButtons,
  exampleFigure,
  comparisonTable,
};

// --- minimal templates: {{var}} escaped, {{{var}}} raw, {{#if k}}..{{else}}..{{/if}}, {{#unless k}}..{{/unless}}

type Node = string | { v: string; raw: boolean } | { key: string; neg: boolean; yes: Node[]; no: Node[] };

function parse(tpl: string): Node[] {
  const root: Node[] = [];
  const stack: { key: string; neg: boolean; yes: Node[]; no: Node[]; inElse: boolean }[] = [];
  const target = () => (stack.length ? (stack[stack.length - 1].inElse ? stack[stack.length - 1].no : stack[stack.length - 1].yes) : root);
  const re = /\{\{\{\s*(\w+)\s*\}\}\}|\{\{\s*(#if|#unless)\s+(\w+)\s*\}\}|\{\{\s*else\s*\}\}|\{\{\s*\/(if|unless)\s*\}\}|\{\{\s*(\w+)\s*\}\}/g;
  let last = 0;
  for (let m; (m = re.exec(tpl)); ) {
    if (m.index > last) target().push(tpl.slice(last, m.index));
    last = re.lastIndex;
    if (m[1]) target().push({ v: m[1], raw: true });
    else if (m[2]) stack.push({ key: m[3], neg: m[2] === "#unless", yes: [], no: [], inElse: false });
    else if (m[4]) {
      const b = stack.pop();
      if (!b) throw new Error("template: unmatched closing tag");
      target().push({ key: b.key, neg: b.neg, yes: b.yes, no: b.no });
    } else if (m[5]) target().push({ v: m[5], raw: false });
    else {
      const b = stack[stack.length - 1];
      if (!b) throw new Error("template: {{else}} outside a block");
      b.inElse = true;
    }
  }
  if (stack.length) throw new Error(`template: unclosed {{#${stack[stack.length - 1].key}}}`);
  if (last < tpl.length) root.push(tpl.slice(last));
  return root;
}

function run(nodes: Node[]): string {
  return nodes
    .map((n) => {
      if (typeof n === "string") return n;
      if ("v" in n) {
        if (!(n.v in ctx)) throw new Error(`template: "${n.v}" is not defined`);
        return n.raw ? String(ctx[n.v]) : esc(ctx[n.v]);
      }
      if (!(n.key in ctx)) throw new Error(`template: "${n.key}" is not defined`);
      const on = Boolean(ctx[n.key]) !== n.neg;
      return run(on ? n.yes : n.no);
    })
    .join("");
}

const template = async (file: string) => run(parse(await Bun.file(join(root, "src", file)).text()));

// --- output

await rm(outDir, { recursive: true, force: true });
await mkdir(outDir, { recursive: true });

const bundle = await Bun.build({ entrypoints: [join(root, "src", "demo.js")], minify: true, format: "iife", target: "browser" });
if (!bundle.success) {
  for (const log of bundle.logs) console.error(log);
  process.exit(1);
}

const page = await template("index.template.html");
const notFound = await template("404.template.html");
await Bun.write(join(outDir, "index.html"), page);
await Bun.write(join(outDir, "404.html"), notFound);
await Bun.write(join(outDir, "styles.css"), Bun.file(join(root, "src", "styles.css")));
await Bun.write(join(outDir, "demo.js"), await bundle.outputs[0].text());
for (const name of ["favicon.svg", "favicon.ico", "apple-touch-icon.png", "og.png"]) {
  await Bun.write(join(outDir, name), Bun.file(join(root, "assets", name)));
}
await Bun.write(join(outDir, "robots.txt"), `User-agent: *\nAllow: /\n${siteUrl ? `\nSitemap: ${siteUrl}sitemap.xml\n` : ""}`);
if (siteUrl) {
  await Bun.write(
    join(outDir, "sitemap.xml"),
    `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n  <url>\n    <loc>${esc(siteUrl)}</loc>\n    <lastmod>${cfg.statusDate}</lastmod>\n  </url>\n</urlset>\n`,
  );
}

// --- report

let total = 0;
let totalGz = 0;
const files = (await readdir(outDir, { recursive: true, withFileTypes: true })).filter((e) => e.isFile());
for (const f of files.sort((a, b) => a.name.localeCompare(b.name))) {
  const path = join(f.parentPath, f.name);
  const bytes = new Uint8Array(await Bun.file(path).arrayBuffer());
  const gz = gzipSync(bytes).length;
  total += bytes.length;
  totalGz += gz;
  console.log(`${relative(root, path).padEnd(28)} ${String(bytes.length).padStart(8)} B  ${String(gz).padStart(8)} B gzip`);
}
console.log(`${"total".padEnd(28)} ${String(total).padStart(8)} B  ${String(totalGz).padStart(8)} B gzip`);
for (const w of warnings) console.warn(`warning: ${w}`);

function fail(): never {
  for (const p of problems) console.error(`error: ${p}`);
  process.exit(1);
}
