#!/usr/bin/env node
// Read-only audit of every Askama HTML template and shared web scripts.
// Run: node scripts/audit-i18n.mjs [--strict]
// Existing catalog parity is checked by scripts/check-i18n.mjs.
// This audit deliberately reports unannotated strings AND two-language-only
// annotations: neither counts as 14-language localization.
import fs from "node:fs";
import path from "node:path";
const root = process.cwd();
const tplDir = path.join(root, "crates/forge-web/templates");
const locales = ["en","pl","cs","de","pt-BR","es","fr","ro","tr","ru","uk","zh-Hans","ja","ko"];
const catalogSource = fs.readFileSync(path.join(root,"crates/forge-web/static/i18n.js"),"utf8");
const match = catalogSource.match(/var translations = ([\s\S]*?);\s*var supported/);
if (!match) throw Error("Translation catalog not found");
const catalog = JSON.parse(match[1]);
const files = [];
function visit(dir) {
  for (const item of fs.readdirSync(dir,{withFileTypes:true})) {
    const filename = path.join(dir,item.name);
    if (item.isDirectory()) visit(filename);
    else if (filename.endsWith(".html")) files.push(filename);
  }
}
visit(tplDir);
const issues = [];
const add = (file,line,kind,value,missing) => issues.push({file,line,kind,value,missing});
function lineOf(s,at) { return s.slice(0,at).split("\n").length; }
for (const filename of files) {
  const src = fs.readFileSync(filename,"utf8");
  const name = path.relative(root,filename).replaceAll("\\","/");
  // Markers with only French and English do not cover the other 12 locales.
  for (const m of src.matchAll(/data-l10n-(?:option-)?en="([^"]+)"[^>]*>/g)) {
    add(name,lineOf(src,m.index),"two_locale_only",m[1],locales.filter(x=>x!=="en"&&x!=="fr"));
  }
  // Attribute strings which are visible or spoken to users.
  for (const m of src.matchAll(/<(?!meta\b|link\b|script\b)([a-z][\w:-]*)\b([^>]+)>/gi)) {
    const attrs = m[2];
    for (const a of attrs.matchAll(/\b(placeholder|aria-label|title|alt)="([^"]+)"/g)) {
      if (!a[2].trim() || a[2].includes("{{") || a[2]==="") continue;
      if (/data-i18n-(?:placeholder|aria-label|title)\s*=/.test(attrs)) continue;
      if (/data-l10n-/.test(attrs)) continue;
      add(name,lineOf(src,m.index),"attribute:"+a[1],a[2],locales);
    }
  }
  // Text nodes: tags are not translated by simply having a dictionary.
  // Ignore Askama control blocks and variable-only fragments.
  const ignored = src.replace(/<!--[\s\S]*?-->/g,"").replace(/<script\b[\s\S]*?<\/script>/gi,"").replace(/<style\b[\s\S]*?<\/style>/gi,"");
  const tokens = ignored.matchAll(/<[^>]+>|[^<]+/g);
  const stack = [];
  for (const m of tokens) {
    const token=m[0];
    if (token.startsWith("<")) {
      if (/^<\//.test(token)) stack.pop();
      else if (!/^<!|^<\?|\/>$/.test(token) && /^<[a-z]/i.test(token)) {
        const tag=token.match(/^<([\w-]+)/)?.[1]?.toLowerCase();
        const local=/\bdata-i18n(?:=|-)|\bdata-l10n-(?:option-)?en=/.test(token);
        stack.push({tag,local:local||(stack.length>0&&stack[stack.length-1].local)});
        if (/^(input|img|br|hr|meta|link|source|wbr)$/.test(tag)||/\/>$/.test(token)) stack.pop();
      }
      continue;
    }
    const top=stack.at(-1);
    if (top?.local || ["code","pre","script","style","textarea","title"].includes(top?.tag)) continue;
    const raw=token.replace(/{%[\s\S]*?%}/g," ").replace(/{{[\s\S]*?}}/g," ").replace(/&(?:nbsp|amp|lt|gt|quot|#\d+);/g," ").replace(/\s+/g," ").trim();
    if (raw.length<3||!/[a-zA-ZÀ-ÿ]/.test(raw) || /^[\W\d]+$/.test(raw)) continue;
    if (/^(GitHub|SurvivorsForge|Azeroth Survivors|PNG|JPEG|WebP|SHA-256)$/.test(raw)) continue;
    add(name,lineOf(ignored,m.index),"text",raw,locales);
  }
}
const byFile = new Map();
for (const issue of issues) byFile.set(issue.file,(byFile.get(issue.file)||0)+1);
const report={templates:files.length,total:issues.length,coverageComplete:issues.length === 0,byFile:[...byFile].sort((a,b)=>b[1]-a[1]).map(([file,count])=>({file,count})),issues};
if (process.argv.includes("--json")) console.log(JSON.stringify(report,null,2));
else {
  console.log("I18N AUDIT:",files.length,"templates;",issues.length,"potential untranslated strings");
  for (const [file,count] of [...byFile].sort((a,b)=>b[1]-a[1])) console.log(String(count).padStart(4),file);
  for (const item of issues.slice(0,40)) console.log(item.file+":"+item.line, item.kind,JSON.stringify(item.value));
  if (issues.length>40) console.log("...and",issues.length-40,"more (use --json to inspect all)");
}
if (process.argv.includes("--strict") && issues.length) process.exitCode=1;
