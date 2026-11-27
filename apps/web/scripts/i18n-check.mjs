// 双语覆盖度检查：把「字典缺 key」「模板里还写死的中文」都变成可执行的检查。
//
//   pnpm i18n:check     # 打印进度 + 缺 key（CI 用 --strict 让缺口变失败）
//   pnpm i18n:scan      # 只印清单，按文件排行
//
// 之所以自己解析模板：文本节点可能跨行（`<div>\n  中文\n</div>`），
// 正则按行扫会漏，用 @vue/compiler-sfc 拿到的 AST 才是准的。
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { parse } from "@vue/compiler-sfc";

const ROOT = new URL("../src/", import.meta.url).pathname.replace(/^\/(\w:)/, "$1");
const CJK = /[\u4e00-\u9fa5]/;
const ATTRS = new Set([
  "title",
  "sub",
  "label",
  "hint",
  "desc",
  "empty",
  "badge",
  "placeholder",
  "aria-label",
]);

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (name === "__tests__" || name === "node_modules") continue;
      walk(p, out);
    } else if (name.endsWith(".vue")) out.push(p);
  }
  return out;
}

const found = [];

function walkAst(src, node, rel, depth = 0) {
  if (!node) return;
  if (node.type === 2) {
    const text = node.content.trim();
    if (text && CJK.test(text))
      found.push({ file: rel, line: lineOf(src, node.loc.start.offset), text });
  } else if (node.type === 1) {
    for (const p of node.props ?? []) {
      if (p.type === 6 && ATTRS.has(p.name) && p.value && CJK.test(p.value.content)) {
        found.push({ file: rel, line: lineOf(src, p.loc.start.offset), text: p.value.content });
      }
    }
    for (const c of node.children ?? []) walkAst(src, c, rel, depth + 1);
  } else if (node.children) {
    for (const c of node.children) walkAst(src, c, rel, depth + 1);
  }
}

function lineOf(src, offset) {
  return src.slice(0, offset).split("\n").length;
}

for (const file of walk(ROOT)) {
  const rel = relative(ROOT, file);
  const src = readFileSync(file, "utf8");
  const { descriptor } = parse(src, { filename: file });
  if (descriptor.template) walkAst(src, descriptor.template.ast, rel);
}

// 字典 key 对齐
const load = async (f) =>
  (await import(new URL(`../src/lib/i18n/${f}`, import.meta.url).href))[f.replace(".ts", "")];
const [zh, en] = await Promise.all([load("zh.ts"), load("en.ts")]);
const onlyZh = Object.keys(zh).filter((k) => !(k in en));
const onlyEn = Object.keys(en).filter((k) => !(k in zh));

const per = new Map();
for (const it of found) per.set(it.file, (per.get(it.file) ?? 0) + 1);
const rows = [...per.entries()].sort((a, b) => b[1] - a[1]);
const total = found.length;

const strict = process.argv.includes("--strict");

console.log(`字典 key：zh ${Object.keys(zh).length} / en ${Object.keys(en).length}`);
if (onlyZh.length)
  console.log(
    `缺英文：${onlyZh.slice(0, 20).join(", ")}${onlyZh.length > 20 ? ` …(+${onlyZh.length - 20})` : ""}`,
  );
if (onlyEn.length)
  console.log(
    `缺中文：${onlyEn.slice(0, 20).join(", ")}${onlyEn.length > 20 ? ` …(+${onlyEn.length - 20})` : ""}`,
  );
console.log(`\n模板里还写死的中文：${total} 条 / ${rows.length} 个文件`);
for (const [f, n] of rows) console.log(`${String(n).padStart(4)}  ${f}`);

const failed = onlyZh.length > 0 || onlyEn.length > 0 || (strict && total > 0);
if (failed) process.exitCode = 1;
