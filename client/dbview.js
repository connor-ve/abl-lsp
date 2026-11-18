// Database browser: a sidebar tree of the databases in each abl.toml (tables
// and sequences, from the .df files) and a page per table with its fields,
// indexes, rows and the code that uses it. Everything comes from the `abl` CLI.
const path = require('path');
const crypto = require('crypto');
const { execFile } = require('child_process');
const vscode = require('vscode');

let exe;
let output;

function runAbl(cwd, args) {
  return new Promise((resolve) => {
    execFile(exe, args, { cwd, maxBuffer: 64 * 1024 * 1024, windowsHide: true }, (err, stdout, stderr) => {
      resolve({ ok: !err, stdout: stdout || '', stderr: (stderr || '').trim() || (err ? err.message : '') });
    });
  });
}

function cleanError(text) {
  return text.replace(/^abl:\s*/gm, '').trim();
}

// ---- model ----

class Model {
  constructor() {
    this.projects = []; // { dir, name, databases: [...], error }
  }

  async load() {
    const files = await vscode.workspace.findFiles('**/abl.toml', '{**/node_modules/**,**/vendor/**,**/.abl/**}', 50);
    const dirs = [...new Set(files.map((f) => path.dirname(f.fsPath)))].sort();
    this.projects = await Promise.all(
      dirs.map(async (dir) => {
        const r = await runAbl(dir, ['db', 'schema', '--json']);
        const name = path.basename(dir);
        if (!r.ok) return { dir, name, databases: [], error: cleanError(r.stderr) };
        try {
          return { dir, name, databases: JSON.parse(r.stdout).databases };
        } catch (e) {
          return { dir, name, databases: [], error: String(e) };
        }
      }),
    );
  }

  allTables() {
    const out = [];
    for (const p of this.projects) {
      for (const db of p.databases) {
        for (const t of db.tables) out.push({ project: p, db, table: t });
      }
    }
    return out;
  }
}

// ---- tree ----

class Tree {
  constructor(model) {
    this.model = model;
    this.emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.emitter.event;
  }

  refresh() {
    this.emitter.fire();
  }

  getTreeItem(n) {
    return n.item;
  }

  getChildren(n) {
    const projects = this.model.projects.filter((p) => p.databases.length || p.error);
    if (!n) {
      if (projects.length === 1) return this.projectChildren(projects[0]);
      return projects.map((p) => {
        const item = new vscode.TreeItem(p.name, vscode.TreeItemCollapsibleState.Expanded);
        item.iconPath = new vscode.ThemeIcon('folder');
        item.tooltip = p.dir;
        return { kind: 'project', project: p, item };
      });
    }
    if (n.kind === 'project') return this.projectChildren(n.project);
    if (n.kind === 'db') {
      const kids = n.db.tables
        .slice()
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((t) => {
          const item = new vscode.TreeItem(t.name, vscode.TreeItemCollapsibleState.None);
          item.iconPath = new vscode.ThemeIcon('table');
          item.description = t.label && t.label !== t.name ? t.label : '';
          item.tooltip = new vscode.MarkdownString(
            `**${n.db.name}.${t.name}**${t.description ? `\n\n${t.description}` : ''}\n\n${t.fields.length} fields, ${t.indexes.length} indexes`,
          );
          item.contextValue = 'table';
          item.command = { command: 'abl.db.openTable', title: 'Open', arguments: [n.project.dir, n.db.name, t.name] };
          return { kind: 'table', project: n.project, db: n.db, table: t, item };
        });
      if (n.db.sequences.length) {
        const item = new vscode.TreeItem('Sequences', vscode.TreeItemCollapsibleState.Collapsed);
        item.iconPath = new vscode.ThemeIcon('symbol-number');
        item.description = String(n.db.sequences.length);
        kids.push({ kind: 'sequences', db: n.db, project: n.project, item });
      }
      return kids;
    }
    if (n.kind === 'sequences') {
      return n.db.sequences.map((s) => {
        const item = new vscode.TreeItem(s.name, vscode.TreeItemCollapsibleState.None);
        item.iconPath = new vscode.ThemeIcon('symbol-number');
        const bits = [];
        if (s.initial) bits.push(`from ${s.initial}`);
        if (s.increment) bits.push(`by ${s.increment}`);
        if (s.cycle) bits.push('cycles');
        item.description = bits.join(', ');
        return { kind: 'sequence', item };
      });
    }
    return [];
  }

  projectChildren(p) {
    if (p.error) {
      const item = new vscode.TreeItem(p.error.split('\n')[0], vscode.TreeItemCollapsibleState.None);
      item.iconPath = new vscode.ThemeIcon('warning');
      item.tooltip = p.error;
      return [{ kind: 'error', item }];
    }
    return p.databases.map((db) => {
      const item = new vscode.TreeItem(db.name, vscode.TreeItemCollapsibleState.Expanded);
      item.iconPath = new vscode.ThemeIcon('database');
      item.description = `${db.state} · ${db.tables.length} tables`;
      item.tooltip = db.schemaFile || db.connect || db.name;
      item.contextValue = db.managed ? `db-${db.state.replace(/\s+/g, '-')}` : 'db-external';
      return { kind: 'db', project: p, db, item };
    });
  }
}

// ---- table page ----

const panels = new Map();

function nonce() {
  return crypto.randomBytes(16).toString('base64');
}

function find(model, dir, dbName, tableName) {
  const project = model.projects.find((p) => p.dir === dir);
  const db = project && project.databases.find((d) => d.name === dbName);
  const table = db && db.tables.find((t) => t.name.toLowerCase() === tableName.toLowerCase());
  return table ? { project, db, table } : undefined;
}

/// Tables that point at this one, and that this one points at: a table whose
/// fields include all of another table's unique primary key fields.
function related(db, table) {
  const pk = (t) => {
    const i = t.indexes.find((x) => x.primary && x.unique) || t.indexes.find((x) => x.unique);
    return i ? i.fields.map((f) => f.name) : [];
  };
  const lc = (names) => names.map((x) => x.toLowerCase()).join();
  const has = (t, names) => names.length > 0 && names.every((n) => t.fields.some((f) => f.name.toLowerCase() === n.toLowerCase()));
  const mine = pk(table);
  const refersTo = [];
  const referencedBy = [];
  for (const other of db.tables) {
    if (other === table) continue;
    const theirs = pk(other);
    if (lc(theirs) === lc(mine)) continue;
    if (has(table, theirs)) refersTo.push({ name: other.name, label: other.label, via: theirs });
    if (has(other, mine)) referencedBy.push({ name: other.name, label: other.label, via: mine });
  }
  return { refersTo, referencedBy };
}

async function openTable(model, dir, dbName, tableName) {
  const hit = find(model, dir, dbName, tableName);
  if (!hit) {
    vscode.window.showWarningMessage(`No table ${tableName} in ${dbName}`);
    return;
  }
  const key = `${dir}|${dbName}|${hit.table.name}`.toLowerCase();
  const open = panels.get(key);
  if (open) {
    open.reveal();
    return;
  }
  const panel = vscode.window.createWebviewPanel('abl.table', `${hit.table.name}`, vscode.ViewColumn.Active, {
    enableScripts: true,
    retainContextWhenHidden: true,
  });
  panel.iconPath = new vscode.ThemeIcon('table');
  panels.set(key, panel);
  panel.onDidDispose(() => panels.delete(key));
  const render = () => {
    const h = find(model, dir, dbName, tableName);
    if (!h) return;
    panel.webview.html = page(panel.webview, h, related(h.db, h.table));
  };
  render();
  panel.ablRender = render;
  panel.webview.onDidReceiveMessage(async (m) => {
    const h = find(model, dir, dbName, tableName) || hit;
    switch (m.type) {
      case 'openSchema': {
        if (!h.db.schemaFile) return;
        const doc = await vscode.workspace.openTextDocument(h.db.schemaFile);
        const line = Math.max(0, (m.line || 1) - 1);
        await vscode.window.showTextDocument(doc, {
          selection: new vscode.Range(line, 0, line, 0),
          viewColumn: vscode.ViewColumn.Beside,
        });
        break;
      }
      case 'openTable':
        openTable(model, dir, dbName, m.name);
        break;
      case 'openFile': {
        const doc = await vscode.workspace.openTextDocument(path.resolve(dir, m.file));
        const line = Math.max(0, (m.line || 1) - 1);
        const col = Math.max(0, (m.column || 1) - 1);
        await vscode.window.showTextDocument(doc, {
          selection: new vscode.Range(line, col, line, col),
          viewColumn: vscode.ViewColumn.Beside,
        });
        break;
      }
      case 'query': {
        let phrase = (m.phrase || '').trim();
        if (phrase && !/^(where|by|break|use-index|no-lock|share-lock|exclusive-lock)\b/i.test(phrase)) phrase = `WHERE ${phrase}`;
        const args = ['query', `${h.db.name}.${h.table.name} ${phrase}`.trim(), '--format', 'page', '--max', String(m.max || 100)];
        const r = await runAbl(dir, args);
        let data;
        if (r.ok) {
          try {
            data = JSON.parse(r.stdout);
          } catch (e) {
            r.ok = false;
            r.stderr = String(e);
          }
        }
        panel.webview.postMessage({ type: 'queryResult', ok: r.ok, data, error: cleanError(r.stderr) });
        break;
      }
      case 'usages': {
        const target = m.field ? `${h.table.name}.${m.field}` : `${h.db.name}.${h.table.name}`;
        const r = await runAbl(dir, ['usages', target, '--json']);
        let data;
        if (r.ok) {
          try {
            data = JSON.parse(r.stdout);
          } catch (e) {
            r.ok = false;
            r.stderr = String(e);
          }
        }
        panel.webview.postMessage({ type: 'usagesResult', field: m.field || null, ok: r.ok, data, error: cleanError(r.stderr) });
        break;
      }
      case 'copy':
        await vscode.env.clipboard.writeText(m.text || '');
        vscode.window.setStatusBarMessage('Copied', 1500);
        break;
    }
  });
}

function page(webview, hit, rel) {
  const n = nonce();
  const data = JSON.stringify({ db: { name: hit.db.name, state: hit.db.state, managed: hit.db.managed, hasSchema: !!hit.db.schemaFile }, table: hit.table, related: rel }).replace(/</g, '\\u003c');
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'nonce-${n}'; script-src 'nonce-${n}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style nonce="${n}">${CSS}</style>
</head>
<body>
<div id="app"></div>
<script nonce="${n}">const DATA = ${data};</script>
<script nonce="${n}">${SCRIPT}</script>
</body>
</html>`;
}

const CSS = `
:root {
  --bg: var(--vscode-editor-background);
  --fg: var(--vscode-editor-foreground);
  --muted: var(--vscode-descriptionForeground);
  --border: var(--vscode-panel-border, rgba(128,128,128,.25));
  --soft: color-mix(in srgb, var(--fg) 6%, transparent);
  --softer: color-mix(in srgb, var(--fg) 3%, transparent);
  --accent: var(--vscode-textLink-foreground);
  --focus: var(--vscode-focusBorder);
  --radius: 8px;
}
* { box-sizing: border-box; }
body { margin: 0; padding: 0; background: var(--bg); color: var(--fg); font-family: var(--vscode-font-family); font-size: 13px; }
.wrap { max-width: 1200px; margin: 0 auto; padding: 24px 28px 40px; }
header { display: flex; gap: 16px; align-items: flex-start; margin-bottom: 18px; }
.logo { width: 44px; height: 44px; border-radius: 10px; flex: none; display: grid; place-items: center;
  background: color-mix(in srgb, var(--accent) 18%, transparent); color: var(--accent); }
.logo svg { width: 24px; height: 24px; }
h1 { margin: 0; font-size: 22px; font-weight: 600; letter-spacing: -.01em; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
h1 .db { font-size: 12px; font-weight: 500; color: var(--muted); padding: 2px 8px; border: 1px solid var(--border); border-radius: 999px; }
.sub { color: var(--muted); margin-top: 4px; line-height: 1.45; }
.chips { display: flex; gap: 6px; flex-wrap: wrap; margin-top: 10px; }
.chip { font-size: 11.5px; padding: 3px 9px; border-radius: 999px; background: var(--soft); color: var(--muted); }
.chip b { color: var(--fg); font-weight: 600; }
.link { color: var(--accent); cursor: pointer; text-decoration: none; }
.link:hover { text-decoration: underline; }
nav { display: flex; gap: 2px; border-bottom: 1px solid var(--border); margin-bottom: 16px; overflow-x: auto; }
nav button { background: none; border: none; color: var(--muted); font: inherit; padding: 9px 14px; cursor: pointer; border-bottom: 2px solid transparent; margin-bottom: -1px; white-space: nowrap; }
nav button:hover { color: var(--fg); }
nav button.on { color: var(--fg); border-bottom-color: var(--accent); font-weight: 600; }
nav .count { font-size: 11px; margin-left: 6px; padding: 0 6px; border-radius: 999px; background: var(--soft); }
.toolbar { display: flex; gap: 8px; align-items: center; margin-bottom: 12px; flex-wrap: wrap; }
input[type=text], select { background: var(--vscode-input-background); color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border, var(--border));
  border-radius: 6px; padding: 6px 10px; font: inherit; outline: none; }
input[type=text]:focus, select:focus { border-color: var(--focus); }
input.grow { flex: 1; min-width: 200px; font-family: var(--vscode-editor-font-family); }
button.primary { background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; border-radius: 6px; padding: 7px 14px; font: inherit; cursor: pointer; font-weight: 500; }
button.primary:hover { background: var(--vscode-button-hoverBackground); }
button.ghost { background: var(--soft); color: var(--fg); border: none; border-radius: 6px; padding: 6px 11px; font: inherit; cursor: pointer; }
button.ghost:hover { background: color-mix(in srgb, var(--fg) 12%, transparent); }
.card { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
.scroll { overflow: auto; max-height: calc(100vh - 260px); }
table { border-collapse: collapse; width: 100%; }
th { position: sticky; top: 0; background: var(--vscode-editorWidget-background, var(--bg)); text-align: left; font-weight: 600; font-size: 11.5px;
  text-transform: uppercase; letter-spacing: .04em; color: var(--muted); padding: 9px 12px; border-bottom: 1px solid var(--border); white-space: nowrap; z-index: 1; }
td { padding: 8px 12px; border-bottom: 1px solid var(--border); vertical-align: top; line-height: 20px; }
tr:last-child td { border-bottom: none; }
tbody tr:hover td { background: var(--softer); }
.mono { font-family: var(--vscode-editor-font-family); font-size: 12.5px; }
.num { text-align: right; font-variant-numeric: tabular-nums; }
.dim { color: var(--muted); }
.unknown { color: var(--muted); font-style: italic; }
.name { font-weight: 600; }
.type { font-size: 11px; padding: 2px 8px; border-radius: 999px; font-family: var(--vscode-editor-font-family); white-space: nowrap; }
.t-character, .t-clob, .t-longchar { background: color-mix(in srgb, #3fb950 18%, transparent); color: color-mix(in srgb, #3fb950 75%, var(--fg)); }
.t-integer, .t-int64, .t-decimal, .t-recid, .t-rowid { background: color-mix(in srgb, #58a6ff 18%, transparent); color: color-mix(in srgb, #58a6ff 75%, var(--fg)); }
.t-date, .t-datetime, .t-datetime-tz { background: color-mix(in srgb, #d29922 20%, transparent); color: color-mix(in srgb, #d29922 80%, var(--fg)); }
.t-logical { background: color-mix(in srgb, #bc8cff 20%, transparent); color: color-mix(in srgb, #bc8cff 80%, var(--fg)); }
.t-raw, .t-blob { background: var(--soft); color: var(--muted); }
.badge { display: inline-block; font-size: 10.5px; font-weight: 600; padding: 1px 6px; border-radius: 4px; margin-right: 4px; letter-spacing: .02em; }
.b-pk { background: color-mix(in srgb, #d29922 25%, transparent); color: color-mix(in srgb, #d29922 85%, var(--fg)); }
.b-uq { background: color-mix(in srgb, #58a6ff 22%, transparent); color: color-mix(in srgb, #58a6ff 85%, var(--fg)); }
.b-ix { background: var(--soft); color: var(--muted); }
.b-mand { background: color-mix(in srgb, #f85149 20%, transparent); color: color-mix(in srgb, #f85149 85%, var(--fg)); }
.b-word { background: color-mix(in srgb, #bc8cff 22%, transparent); color: color-mix(in srgb, #bc8cff 85%, var(--fg)); }
.b-read { background: var(--soft); color: var(--muted); }
.b-create { background: color-mix(in srgb, #3fb950 22%, transparent); color: color-mix(in srgb, #3fb950 85%, var(--fg)); }
.b-update { background: color-mix(in srgb, #d29922 25%, transparent); color: color-mix(in srgb, #d29922 85%, var(--fg)); }
.b-delete { background: color-mix(in srgb, #f85149 22%, transparent); color: color-mix(in srgb, #f85149 85%, var(--fg)); }
.desc { color: var(--muted); font-size: 12px; margin-top: 2px; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; }
.ix { border: 1px solid var(--border); border-radius: var(--radius); padding: 14px 16px; }
.ix h3 { margin: 0 0 8px; font-size: 14px; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.ix ol { margin: 0; padding-left: 20px; }
.ix li { padding: 2px 0; }
.arrow { color: var(--muted); font-size: 11px; margin-left: 4px; }
.empty { text-align: center; padding: 48px 20px; color: var(--muted); }
.empty .big { font-size: 15px; color: var(--fg); margin-bottom: 6px; }
.error { border: 1px solid color-mix(in srgb, #f85149 45%, transparent); background: color-mix(in srgb, #f85149 8%, transparent);
  border-radius: var(--radius); padding: 12px 14px; white-space: pre-wrap; font-family: var(--vscode-editor-font-family); font-size: 12.5px; }
.hint { color: var(--muted); margin-top: 8px; font-size: 12px; }
.status { color: var(--muted); font-size: 12px; margin-top: 10px; }
.files { display: flex; flex-direction: column; gap: 10px; }
.file h4 { margin: 0; padding: 9px 14px; font-size: 12.5px; background: var(--softer); border-bottom: 1px solid var(--border); font-family: var(--vscode-editor-font-family); }
.use { display: grid; grid-template-columns: 56px 70px 1fr; gap: 10px; padding: 7px 14px; cursor: pointer; border-bottom: 1px solid var(--border); align-items: baseline; }
.use:last-child { border-bottom: none; }
.use:hover { background: var(--softer); }
.use code { font-family: var(--vscode-editor-font-family); font-size: 12px; white-space: pre; overflow: hidden; text-overflow: ellipsis; }
.cols { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
@media (max-width: 700px) { .cols { grid-template-columns: 1fr; } }
.rel { border: 1px solid var(--border); border-radius: var(--radius); }
.rel h3 { margin: 0; padding: 11px 14px; font-size: 13px; border-bottom: 1px solid var(--border); }
.rel .row { padding: 9px 14px; border-bottom: 1px solid var(--border); cursor: pointer; display: flex; justify-content: space-between; gap: 10px; }
.rel .row:last-child { border-bottom: none; }
.rel .row:hover { background: var(--softer); }
.spin { display: inline-block; width: 12px; height: 12px; border: 2px solid var(--muted); border-right-color: transparent; border-radius: 50%; animation: s .7s linear infinite; vertical-align: -2px; margin-right: 6px; }
@keyframes s { to { transform: rotate(360deg); } }
.hidden { display: none; }
`;

// The page script: plain DOM, no libraries.
const SCRIPT = `
const vscode = acquireVsCodeApi();
const { db, table, related } = DATA;
const state = vscode.getState() || {};
const el = (tag, attrs = {}, ...kids) => {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) e.append(k.nodeType ? k : document.createTextNode(String(k)));
  return e;
};
const save = (patch) => { Object.assign(state, patch); vscode.setState(state); };

const primary = table.indexes.find((i) => i.primary);
const pkFields = new Set(primary ? primary.fields.map((f) => f.name.toLowerCase()) : []);
const uniqueFields = new Set(table.indexes.filter((i) => i.unique).flatMap((i) => i.fields.map((f) => f.name.toLowerCase())));

const ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M3 14h18M9 9v11"/></svg>';

function header() {
  const logo = el('div', { class: 'logo' });
  logo.innerHTML = ICON;
  const chips = el('div', { class: 'chips' },
    el('span', { class: 'chip' }, el('b', {}, table.fields.length), ' fields'),
    el('span', { class: 'chip' }, el('b', {}, table.indexes.length), ' indexes'),
    table.dumpName && el('span', { class: 'chip' }, 'dump ', el('b', {}, table.dumpName)),
    table.area && el('span', { class: 'chip' }, 'area ', el('b', {}, table.area)),
    table.triggers.length > 0 && el('span', { class: 'chip' }, 'triggers ', el('b', {}, table.triggers.join(', '))),
    table.hidden && el('span', { class: 'chip' }, 'hidden'),
    db.hasSchema && el('span', { class: 'chip link', onclick: () => vscode.postMessage({ type: 'openSchema', line: table.line }) }, 'open in .df'),
  );
  return el('header', {}, logo, el('div', {},
    el('h1', {}, table.name, el('span', { class: 'db' }, db.name + ' · ' + db.state)),
    (table.label && table.label !== table.name || table.description) &&
      el('div', { class: 'sub' }, [table.label !== table.name ? table.label : null, table.description].filter(Boolean).join(' — ')),
    chips));
}

const TABS = [
  ['fields', 'Fields', table.fields.length],
  ['indexes', 'Indexes', table.indexes.length],
  ['data', 'Data'],
  ['usages', 'Used in'],
  ['related', 'Related', related.refersTo.length + related.referencedBy.length],
];

function nav(current) {
  return el('nav', {}, TABS.map(([id, label, count]) =>
    el('button', { class: id === current ? 'on' : '', onclick: () => show(id) }, label, count != null && el('span', { class: 'count' }, count))));
}

function typeCell(f) {
  return el('span', { class: 'type t-' + f.type }, f.type + (f.extent ? '[' + f.extent + ']' : ''));
}

function fieldsTab() {
  const filter = el('input', { type: 'text', class: 'grow', placeholder: 'Filter fields by name, label, type or description', value: state.fieldFilter || '' });
  const body = el('tbody');
  const fill = () => {
    const q = filter.value.trim().toLowerCase();
    save({ fieldFilter: filter.value });
    body.replaceChildren();
    const rows = table.fields.filter((f) => !q || [f.name, f.label, f.type, f.description, f.help, f.format].some((v) => v && v.toLowerCase().includes(q)));
    for (const f of rows) {
      const lc = f.name.toLowerCase();
      body.append(el('tr', {},
        el('td', { class: 'num dim' }, f.order || ''),
        el('td', {},
          el('span', { class: 'name link', title: 'Open in .df', onclick: () => vscode.postMessage({ type: 'openSchema', line: f.line }) }, f.name),
          (f.description || f.help) && el('div', { class: 'desc' }, f.description || f.help)),
        el('td', {}, typeCell(f)),
        el('td', {},
          pkFields.has(lc) && el('span', { class: 'badge b-pk', title: 'In the primary index' }, 'PK'),
          !pkFields.has(lc) && uniqueFields.has(lc) && el('span', { class: 'badge b-uq', title: 'In a unique index' }, 'UQ'),
          f.indexes.length > 0 && !pkFields.has(lc) && !uniqueFields.has(lc) && el('span', { class: 'badge b-ix', title: 'Indexes: ' + f.indexes.join(', ') }, 'IX'),
          f.mandatory && el('span', { class: 'badge b-mand', title: 'Mandatory' }, 'REQ')),
        el('td', { class: 'mono' }, f.format || ''),
        el('td', {}, f.label || ''),
        el('td', { class: 'mono' }, f.initial == null ? '' : f.initial),
        el('td', {}, el('span', { class: 'link', onclick: () => { save({ usageField: f.name }); show('usages'); } }, 'uses'))));
    }
    if (!rows.length) body.append(el('tr', {}, el('td', { colspan: 8, class: 'empty' }, 'No field matches')));
  };
  filter.addEventListener('input', fill);
  fill();
  setTimeout(() => filter.focus(), 0);
  return [
    el('div', { class: 'toolbar' }, filter),
    el('div', { class: 'card scroll' }, el('table', {},
      el('thead', {}, el('tr', {}, ['#', 'Name', 'Type', 'Keys', 'Format', 'Label', 'Initial', ''].map((h) => el('th', {}, h)))),
      body)),
  ];
}

function indexesTab() {
  if (!table.indexes.length) return el('div', { class: 'empty' }, el('div', { class: 'big' }, 'No indexes'), 'Queries on this table read every row.');
  const sorted = table.indexes.slice().sort((a, b) => (b.primary - a.primary) || (b.unique - a.unique) || a.name.localeCompare(b.name));
  return el('div', { class: 'grid' }, sorted.map((i) => el('div', { class: 'ix' },
    el('h3', {},
      el('span', { class: 'link', onclick: () => vscode.postMessage({ type: 'openSchema', line: i.line }) }, i.name),
      i.primary && el('span', { class: 'badge b-pk' }, 'PRIMARY'),
      i.unique && el('span', { class: 'badge b-uq' }, 'UNIQUE'),
      i.word && el('span', { class: 'badge b-word' }, 'WORD'),
      i.inactive && el('span', { class: 'badge b-ix' }, 'INACTIVE')),
    el('ol', {}, i.fields.map((f) => {
      const fd = table.fields.find((x) => x.name.toLowerCase() === f.name.toLowerCase());
      return el('li', { class: 'mono' }, f.name, el('span', { class: 'arrow' }, f.descending ? '↓ desc' : '↑ asc'), fd && el('span', { class: 'arrow' }, ' · ' + fd.type));
    })),
    i.area && el('div', { class: 'desc' }, 'area ' + i.area))));
}

function dataTab() {
  const phrase = el('input', { type: 'text', class: 'grow', placeholder: 'WHERE ... BY ...   (e.g. balance > 1000 BY name)', value: state.phrase || '' });
  const max = el('select', {}, [25, 100, 500, 1000, 0].map((n) => el('option', { value: n, selected: String(state.max || 100) === String(n) }, n === 0 ? 'all rows' : n + ' rows')));
  const out = el('div');
  const go = () => {
    save({ phrase: phrase.value, max: max.value });
    out.replaceChildren(el('div', { class: 'status' }, el('span', { class: 'spin' }), 'Querying ' + db.name + '.' + table.name + '...'));
    vscode.postMessage({ type: 'query', phrase: phrase.value, max: Number(max.value) });
  };
  phrase.addEventListener('keydown', (e) => { if (e.key === 'Enter') go(); });
  window.renderQuery = (m) => {
    if (!m.ok) {
      const hint = db.managed && db.state === 'not created'
        ? 'The database isn\\'t created yet: run abl db create ' + db.name + ' in a terminal.'
        : 'Reading rows needs an OpenEdge install (abl doctor) and the database.';
      out.replaceChildren(el('div', { class: 'error' }, m.error || 'The query failed'), el('div', { class: 'hint' }, hint));
      return;
    }
    const { columns, rows, more } = m.data;
    if (!rows.length) {
      out.replaceChildren(el('div', { class: 'empty' }, el('div', { class: 'big' }, 'No rows'), phrase.value ? 'Nothing matches the query.' : 'The table is empty.'));
      return;
    }
    const isNum = (c) => ['integer', 'int64', 'decimal'].includes(c.type);
    const cell = (c, v) => {
      if (v === null) return el('td', { class: 'unknown' + (isNum(c) ? ' num' : '') }, '?');
      if (Array.isArray(v)) return el('td', { class: 'mono' }, v.map((x) => (x === null ? '?' : String(x))).join(' ; '));
      if (typeof v === 'boolean') return el('td', {}, v ? 'yes' : 'no');
      return el('td', { class: isNum(c) ? 'num mono' : '' }, String(v));
    };
    const csv = () => [columns.map((c) => c.name).join(','), ...rows.map((r) => columns.map((c) => {
      const v = r[c.name]; const s = v === null ? '?' : Array.isArray(v) ? v.join(';') : String(v);
      return /[",\\n]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
    }).join(','))].join('\\n');
    out.replaceChildren(
      el('div', { class: 'card scroll' }, el('table', {},
        el('thead', {}, el('tr', {}, columns.map((c) => el('th', { class: isNum(c) ? 'num' : '', title: c.type }, c.name)))),
        el('tbody', {}, rows.map((r) => el('tr', {}, columns.map((c) => cell(c, r[c.name]))))))),
      el('div', { class: 'toolbar', style: 'margin-top:10px' },
        el('span', { class: 'status', style: 'margin:0;flex:1' }, rows.length + ' row' + (rows.length === 1 ? '' : 's') + (more ? ' shown, there are more' : '')),
        el('button', { class: 'ghost', onclick: () => vscode.postMessage({ type: 'copy', text: csv() }) }, 'Copy as CSV'),
        el('button', { class: 'ghost', onclick: () => vscode.postMessage({ type: 'copy', text: JSON.stringify(rows, null, 2) }) }, 'Copy as JSON')),
    );
  };
  const fields = el('div', { class: 'hint' }, 'Fields: ', table.fields.slice(0, 12).map((f) => f.name).join(', '), table.fields.length > 12 ? ', ...' : '');
  if (state.ranOnce) setTimeout(go, 0);
  return [
    el('div', { class: 'toolbar' }, phrase, max, el('button', { class: 'primary', onclick: () => { save({ ranOnce: true }); go(); } }, 'Run')),
    fields,
    el('div', { style: 'margin-top:14px' }, out),
  ];
}

function usagesTab() {
  const field = el('select', {}, el('option', { value: '' }, 'the whole table'),
    table.fields.map((f) => el('option', { value: f.name, selected: state.usageField === f.name }, f.name)));
  const writes = el('input', { type: 'checkbox', id: 'writes' });
  writes.checked = !!state.writesOnly;
  const out = el('div', {}, el('div', { class: 'status' }, el('span', { class: 'spin' }), 'Searching the code...'));
  let last;
  const draw = () => {
    if (!last) return;
    if (!last.ok) { out.replaceChildren(el('div', { class: 'error' }, last.error || 'abl usages failed')); return; }
    let uses = last.data.uses;
    if (writes.checked) uses = uses.filter((u) => u.access !== 'read');
    if (!uses.length) {
      out.replaceChildren(el('div', { class: 'empty' }, el('div', { class: 'big' }, 'No uses found'), 'Nothing in the project ' + (writes.checked ? 'writes ' : 'reads or writes ') + last.data.target + '.'));
      return;
    }
    const byFile = new Map();
    for (const u of uses) { if (!byFile.has(u.file)) byFile.set(u.file, []); byFile.get(u.file).push(u); }
    out.replaceChildren(
      el('div', { class: 'status', style: 'margin:0 0 10px' }, uses.length + ' use' + (uses.length === 1 ? '' : 's') + ' in ' + byFile.size + ' file' + (byFile.size === 1 ? '' : 's')),
      el('div', { class: 'files' }, [...byFile].map(([file, list]) => el('div', { class: 'card file' },
        el('h4', {}, file),
        list.map((u) => el('div', { class: 'use', title: u.note || '', onclick: () => vscode.postMessage({ type: 'openFile', file: u.file, line: u.line, column: u.column }) },
          el('span', { class: 'dim mono num' }, u.line),
          el('span', {}, el('span', { class: 'badge b-' + u.access }, u.access.toUpperCase())),
          el('code', {}, (u.text || '').trim())))))));
  };
  const go = () => {
    save({ usageField: field.value, writesOnly: writes.checked });
    out.replaceChildren(el('div', { class: 'status' }, el('span', { class: 'spin' }), 'Searching the code...'));
    vscode.postMessage({ type: 'usages', field: field.value || null });
  };
  window.renderUsages = (m) => { last = m; draw(); };
  field.addEventListener('change', go);
  writes.addEventListener('change', () => { save({ writesOnly: writes.checked }); draw(); });
  setTimeout(go, 0);
  return [
    el('div', { class: 'toolbar' }, 'Where the code uses', field, el('label', { for: 'writes', style: 'display:flex;gap:6px;align-items:center;margin-left:8px' }, writes, 'only creates, updates and deletes')),
    out,
  ];
}

function relatedTab() {
  const list = (title, items, empty) => el('div', { class: 'rel' }, el('h3', {}, title),
    items.length ? items.map((r) => el('div', { class: 'row', onclick: () => vscode.postMessage({ type: 'openTable', name: r.name }) },
      el('span', {}, el('span', { class: 'link name' }, r.name), r.label && r.label !== r.name && el('span', { class: 'dim' }, '  ' + r.label)),
      el('span', { class: 'dim mono' }, 'by ' + r.via.join(', ')))) : el('div', { class: 'row dim', style: 'cursor:default' }, empty));
  return [
    el('div', { class: 'cols' },
      list('Refers to', related.refersTo, 'No other table\\'s key is in this table'),
      list('Referred to by', related.referencedBy, 'No other table holds this table\\'s key')),
    el('div', { class: 'hint' }, 'Matched by field name: a table that has all the fields of another table\\'s unique primary index.'),
  ];
}

const app = document.getElementById('app');
const views = { fields: fieldsTab, indexes: indexesTab, data: dataTab, usages: usagesTab, related: relatedTab };
function show(tab) {
  save({ tab });
  app.replaceChildren(el('div', { class: 'wrap' }, header(), nav(tab), el('section', {}, views[tab]())));
}
window.addEventListener('message', (e) => {
  const m = e.data;
  if (m.type === 'queryResult' && window.renderQuery) window.renderQuery(m);
  if (m.type === 'usagesResult' && window.renderUsages) window.renderUsages(m);
});
show(state.tab && views[state.tab] ? state.tab : 'fields');
`;

// ---- commands ----

function register(context, ablExe) {
  exe = ablExe;
  output = vscode.window.createOutputChannel('ABL Databases');
  const model = new Model();
  const tree = new Tree(model);
  const view = vscode.window.createTreeView('abl.databases', { treeDataProvider: tree, showCollapseAll: true });
  let loading;
  const reload = () => {
    loading = model.load().then(() => {
      tree.refresh();
      for (const p of panels.values()) if (p.ablRender) p.ablRender();
      const n = model.allTables().length;
      view.message = n ? undefined : model.projects.some((p) => p.error) ? undefined : 'No databases with a schema in abl.toml. Add one:\n[db.sports]\nschema = "db/sports.df"';
    });
    return loading;
  };
  reload();

  const watcher = vscode.workspace.createFileSystemWatcher('**/{abl.toml,*.df,*.DF}');
  let timer;
  const soon = () => {
    clearTimeout(timer);
    timer = setTimeout(reload, 400);
  };
  watcher.onDidChange(soon);
  watcher.onDidCreate(soon);
  watcher.onDidDelete(soon);

  const dbAction = (verb) => async (node) => {
    if (!node || node.kind !== 'db') return;
    output.show(true);
    output.appendLine(`> abl db ${verb} ${node.db.name}`);
    const r = await runAbl(node.project.dir, ['db', verb, node.db.name]);
    output.appendLine((r.stdout + (r.stderr ? `\n${r.stderr}` : '')).trim());
    if (!r.ok) vscode.window.showErrorMessage(`abl db ${verb} ${node.db.name}: ${cleanError(r.stderr).split('\n')[0]}`);
    reload();
  };

  context.subscriptions.push(
    view,
    watcher,
    output,
    vscode.commands.registerCommand('abl.db.refresh', reload),
    vscode.commands.registerCommand('abl.db.openTable', (dir, db, table) => openTable(model, dir, db, table)),
    vscode.commands.registerCommand('abl.db.findTable', async () => {
      await loading;
      const items = model.allTables().map((t) => ({
        label: t.table.name,
        description: `${t.db.name}${model.projects.length > 1 ? ` · ${t.project.name}` : ''}`,
        detail: [t.table.label !== t.table.name ? t.table.label : '', t.table.description].filter(Boolean).join(' — ') || undefined,
        hit: t,
      }));
      if (!items.length) {
        vscode.window.showInformationMessage('No database tables: add a [db.<name>] with a schema to abl.toml.');
        return;
      }
      const pick = await vscode.window.showQuickPick(items, { placeHolder: 'Open a database table', matchOnDescription: true, matchOnDetail: true });
      if (pick) openTable(model, pick.hit.project.dir, pick.hit.db.name, pick.hit.table.name);
    }),
    vscode.commands.registerCommand('abl.db.openSchema', async (node) => {
      if (node && node.db && node.db.schemaFile) {
        const doc = await vscode.workspace.openTextDocument(node.db.schemaFile);
        await vscode.window.showTextDocument(doc);
      }
    }),
    vscode.commands.registerCommand('abl.db.create', dbAction('create')),
    vscode.commands.registerCommand('abl.db.start', dbAction('start')),
    vscode.commands.registerCommand('abl.db.stop', dbAction('stop')),
  );
}

module.exports = { register, related };
