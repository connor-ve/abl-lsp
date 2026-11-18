// VS Code client for abl-lsp: starts the bundled server (`abl lsp`) and forwards settings.
const fs = require('fs');
const path = require('path');
const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');
const dbview = require('./dbview');

let client;

function serverCommand(context) {
  const configured = vscode.workspace.getConfiguration('abl').get('serverPath');
  if (configured) return configured;
  const exe = process.platform === 'win32' ? 'abl.exe' : 'abl';
  return context.asAbsolutePath(path.join('bin', exe));
}

// `[extensions]` in each abl.toml: the extensions besides .p .w .i .cls .t.
async function extraExtensions() {
  const files = await vscode.workspace.findFiles('**/abl.toml', '{**/node_modules/**,**/vendor/**}', 50);
  const found = new Set();
  for (const f of files) {
    let text;
    try {
      text = fs.readFileSync(f.fsPath, 'utf8');
    } catch {
      continue;
    }
    const section = /^\[extensions\][^\S\n]*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(text);
    if (!section) continue;
    for (const m of section[1].matchAll(/^\s*(main|include)\s*=\s*\[([^\]]*)\]/gm)) {
      for (const q of m[2].matchAll(/["']([^"']+)["']/g)) {
        const e = q[1].trim().replace(/^\./, '').toLowerCase();
        if (e && !['p', 'w', 'i', 'cls', 't'].includes(e)) found.add(e);
      }
    }
  }
  return [...found].sort();
}

// Offers to open the extra extensions as ABL, through files.associations.
async function associate(exts) {
  if (!exts.length || !vscode.workspace.workspaceFolders) return;
  const assoc = vscode.workspace.getConfiguration('files').get('associations') || {};
  const missing = exts.filter((e) => assoc[`*.${e}`] !== 'abl');
  if (!missing.length) return;
  const list = missing.map((e) => `.${e}`).join(', ');
  const pick = await vscode.window.showInformationMessage(
    `abl.toml lists ${list} as ABL source. Open those files as ABL in this workspace?`,
    'Yes',
    'Not now',
  );
  if (pick !== 'Yes') return;
  const cfg = vscode.workspace.getConfiguration('files');
  const inspect = cfg.inspect('associations');
  const next = Object.assign({}, (inspect && inspect.workspaceValue) || {});
  for (const e of missing) next[`*.${e}`] = 'abl';
  await cfg.update('associations', next, vscode.ConfigurationTarget.Workspace);
}

async function activate(context) {
  const command = serverCommand(context);
  if (!fs.existsSync(command)) {
    vscode.window.showErrorMessage(`ABL language server not found at ${command}. Set "abl.serverPath" or rebuild the extension.`);
    return;
  }
  const cfg = vscode.workspace.getConfiguration('abl');
  const exts = await extraExtensions();
  associate(exts);
  const glob = ['p', 'w', 'i', 'cls', 't', 'df', ...exts].flatMap((e) => [e, e.toUpperCase()]).join(',');
  // Always name the subcommand: a bare `abl` prints project status instead.
  const server = { command, args: ['lsp'], transport: TransportKind.stdio };
  client = new LanguageClient(
    'abl',
    'ABL Language Server',
    { run: server, debug: server },
    {
      documentSelector: [{ scheme: 'file', language: 'abl' }],
      synchronize: {
        configurationSection: 'abl',
        fileEvents: vscode.workspace.createFileSystemWatcher(`**/*.{${glob}}`),
      },
      initializationOptions: {
        propath: cfg.get('propath'),
        schema: cfg.get('schema'),
        lint: cfg.get('lint'),
        unresolvedIncludes: cfg.get('diagnostics.unresolvedIncludes'),
        preprocessor: cfg.get('preprocessor'),
        format: cfg.get('format'),
        compileOnSave: cfg.get('compileOnSave'),
      },
    },
  );
  context.subscriptions.push(
    vscode.commands.registerCommand('abl.restartServer', () => client && client.restart()),
  );
  dbview.register(context, command);
  await client.start();
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
