// VS Code client for abl-lsp: starts the bundled server and forwards settings.
const fs = require('fs');
const path = require('path');
const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

let client;

function serverCommand(context) {
  const configured = vscode.workspace.getConfiguration('abl').get('serverPath');
  if (configured) return configured;
  const exe = process.platform === 'win32' ? 'abl-lsp.exe' : 'abl-lsp';
  return context.asAbsolutePath(path.join('bin', exe));
}

async function activate(context) {
  const command = serverCommand(context);
  if (!fs.existsSync(command)) {
    vscode.window.showErrorMessage(`ABL language server not found at ${command}. Set "abl.serverPath" or rebuild the extension.`);
    return;
  }
  const cfg = vscode.workspace.getConfiguration('abl');
  const server = { command, transport: TransportKind.stdio };
  client = new LanguageClient(
    'abl',
    'ABL Language Server',
    { run: server, debug: server },
    {
      documentSelector: [{ scheme: 'file', language: 'abl' }],
      synchronize: {
        configurationSection: 'abl',
        fileEvents: vscode.workspace.createFileSystemWatcher('**/*.{p,w,i,cls,t,df,P,W,I,CLS,T,DF}'),
      },
      initializationOptions: {
        propath: cfg.get('propath'),
        schema: cfg.get('schema'),
        lint: cfg.get('lint'),
        unresolvedIncludes: cfg.get('diagnostics.unresolvedIncludes'),
        preprocessor: cfg.get('preprocessor'),
        format: cfg.get('format'),
      },
    },
  );
  context.subscriptions.push(
    vscode.commands.registerCommand('abl.restartServer', () => client && client.restart()),
  );
  await client.start();
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
