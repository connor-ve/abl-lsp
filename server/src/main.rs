//! abl-lsp: a language server for OpenEdge ABL.
//!
//! Usage:
//!   abl-lsp                 run the language server on stdio
//!   abl-lsp check FILE...   print diagnostics for files (exit code 1 on errors)
//!   abl-lsp format [--write] FILE...   print (or rewrite) formatted files
//!   abl-lsp --version

mod analysis;
mod builtins;
mod features;
mod format;
mod handles;
mod keywords;
mod langref;
mod lexer;
mod line_index;
mod lint;
mod oo;
mod schema;
mod workspace;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use analysis::{Analysis, Severity};
use workspace::Workspace;

struct Backend {
    client: Client,
    ws: Arc<RwLock<Workspace>>,
}

fn uri_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

fn propath_entries(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s.split([';', ',']).map(str::to_string).collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

impl Backend {
    fn doc(&self, uri: &Url) -> Option<(PathBuf, Arc<Analysis>)> {
        let path = uri_path(uri)?;
        let a = self.ws.read().unwrap().get(&path)?;
        Some((path, a))
    }

    async fn publish(&self, uri: Url, path: PathBuf, ana: Arc<Analysis>) {
        let diags = {
            let ws = self.ws.read().unwrap();
            features::diagnostics(&ws, &path, &ana)
        };
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    /// Accepts both the initializationOptions shape and the `abl` settings section.
    async fn apply_settings(&self, v: &Value) {
        let unknown = self.apply_settings_sync(v);
        if !unknown.is_empty() {
            let msg = format!(
                "abl-lsp: ignoring unknown abl.lint entries: {}",
                unknown.join(", ")
            );
            self.client.log_message(MessageType::WARNING, msg).await;
        }
    }

    fn apply_settings_sync(&self, v: &Value) -> Vec<String> {
        let mut ws = self.ws.write().unwrap();
        let unknown = v.get("lint").map(|l| ws.lint.apply(l)).unwrap_or_default();
        if let Some(p) = v.get("propath") {
            ws.set_propath(propath_entries(p));
        }
        if let Some(d) = v.get("preprocessor") {
            ws.set_predefined(d);
            ws.reanalyze_open();
        }
        if let Some(k) = v
            .get("format")
            .and_then(|f| f.get("keywordCase"))
            .and_then(Value::as_str)
        {
            ws.keyword_case = format::KeywordCase::parse(k);
        }
        if let Some(s) = v.get("schema") {
            ws.set_schema_setting(propath_entries(s));
        }
        let unresolved = v.get("unresolvedIncludes").or_else(|| {
            v.get("diagnostics")
                .and_then(|d| d.get("unresolvedIncludes"))
        });
        if let Some(b) = unresolved.and_then(Value::as_bool) {
            ws.unresolved_include_diags = b;
        }
        unknown
    }

    async fn republish_open(&self) {
        let docs: Vec<(PathBuf, Arc<Analysis>)> = {
            let ws = self.ws.read().unwrap();
            ws.index()
                .filter(|(p, _)| ws.is_open(p))
                .filter_map(|(p, _)| ws.get(p).map(|a| (p.clone(), a)))
                .collect()
        };
        for (p, a) in docs {
            if let Ok(uri) = Url::from_file_path(&p) {
                self.publish(uri, p, a).await;
            }
        }
    }

    async fn on_text(&self, uri: Url, text: String) {
        let Some(path) = uri_path(&uri) else { return };
        let ana = self.ws.write().unwrap().open_doc(&path, text);
        self.publish(uri, path, ana).await;
    }

    fn spans_to_locations(spans: Vec<features::Span>) -> Vec<Location> {
        spans
            .into_iter()
            .filter_map(|(p, a, s, e)| {
                Some(Location {
                    uri: Url::from_file_path(&p).ok()?,
                    range: a.lines.range(&a.text, s, e),
                })
            })
            .collect()
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let mut roots: Vec<PathBuf> = params
            .workspace_folders
            .iter()
            .flatten()
            .filter_map(|f| uri_path(&f.uri))
            .collect();
        #[allow(deprecated)]
        let root_uri = params.root_uri.clone();
        if roots.is_empty() {
            roots.extend(root_uri.as_ref().and_then(uri_path));
        }
        self.ws.write().unwrap().roots = roots;
        if let Some(opts) = &params.initialization_options {
            self.apply_settings(opts).await;
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "abl-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into(), ":".into()]),
                    ..Default::default()
                }),
                references_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::SOURCE_FIX_ALL,
                        ]),
                        ..Default::default()
                    },
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: features::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let ws = self.ws.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let roots = ws.read().unwrap().roots.clone();
            let started = std::time::Instant::now();
            let Ok(scan) = tokio::task::spawn_blocking(move || workspace::scan(&roots)).await
            else {
                return;
            };
            let n = scan.files.len();
            let tables = {
                let mut w = ws.write().unwrap();
                w.set_index(scan.files);
                w.set_discovered_schema(scan.schema_files);
                w.schema()
                    .iter()
                    .map(|s| format!("{} ({} symbols)", s.db, s.ana.symbols.len()))
                    .collect::<Vec<_>>()
            };
            client
                .log_message(
                    MessageType::INFO,
                    format!("abl-lsp: indexed {n} files in {:.1?}", started.elapsed()),
                )
                .await;
            if !tables.is_empty() {
                client
                    .log_message(
                        MessageType::INFO,
                        format!("abl-lsp: schema loaded: {}", tables.join(", ")),
                    )
                    .await;
            }
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.on_text(params.text_document.uri, params.text_document.text)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.on_text(params.text_document.uri, change.text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        if let Some(path) = uri_path(&params.text_document.uri) {
            self.ws.write().unwrap().close_doc(&path);
        }
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let settings = params
            .settings
            .get("abl")
            .cloned()
            .unwrap_or(params.settings);
        self.apply_settings(&settings).await;
        self.republish_open().await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        {
            let mut ws = self.ws.write().unwrap();
            for change in &params.changes {
                if let Some(p) = uri_path(&change.uri) {
                    ws.file_changed(&p, change.typ == FileChangeType::DELETED);
                }
            }
        }
        self.republish_open().await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let tdp = params.text_document_position_params;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, tdp.position);
        Ok(features::resolve_at(&ws, &path, &ana, off)
            .and_then(|d| features::location(&d))
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let tdp = params.text_document_position_params;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, tdp.position);
        Ok(features::hover(&ws, &path, &ana, off))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let tdp = params.text_document_position;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, tdp.position);
        Ok(Some(CompletionResponse::Array(features::completion(
            &ws, &path, &ana, off,
        ))))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let Some((_, ana)) = self.doc(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(DocumentSymbolResponse::Nested(
            features::document_symbols(&ana),
        )))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let ws = self.ws.read().unwrap();
        Ok(Some(features::workspace_symbols(&ws, &params.query)))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let tdp = params.text_document_position;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        let spans = {
            let ws = self.ws.read().unwrap();
            let off = ana.lines.offset(&ana.text, tdp.position);
            features::references(
                &ws,
                &path,
                &ana,
                off,
                params.context.include_declaration,
                false,
            )
        };
        Ok(Some(Self::spans_to_locations(spans)))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let tdp = params.text_document_position_params;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, tdp.position);
        let spans = features::references(&ws, &path, &ana, off, true, true);
        Ok(Some(
            spans
                .into_iter()
                .map(|(_, a, s, e)| DocumentHighlight {
                    range: a.lines.range(&a.text, s, e),
                    kind: Some(DocumentHighlightKind::TEXT),
                })
                .collect(),
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let Some((path, ana)) = self.doc(&params.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, params.position);
        if let Some(why) = features::rename_blocker(&ws, &path, &ana, off) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(why));
        }
        let here = features::references(&ws, &path, &ana, off, true, true);
        Ok(here
            .into_iter()
            .find(|(_, _, s, e)| *s <= off && off <= *e)
            .map(|(_, a, s, e)| PrepareRenameResponse::Range(a.lines.range(&a.text, s, e))))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let tdp = params.text_document_position;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        if !features::is_valid_name(&params.new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(format!(
                "'{}' is not a valid ABL name",
                params.new_name
            )));
        }
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, tdp.position);
        if let Some(why) = features::rename_blocker(&ws, &path, &ana, off) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(why));
        }
        let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
            std::collections::HashMap::new();
        for (p, a, s, e) in features::references(&ws, &path, &ana, off, true, false) {
            if let Ok(uri) = Url::from_file_path(&p) {
                changes.entry(uri).or_default().push(TextEdit {
                    range: a.lines.range(&a.text, s, e),
                    new_text: params.new_name.clone(),
                });
            }
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let tdp = params.text_document_position_params;
        let Some((path, ana)) = self.doc(&tdp.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let off = ana.lines.offset(&ana.text, tdp.position);
        Ok(features::signature_help(&ws, &path, &ana, off))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let only = params.context.only.clone().unwrap_or_default();
        let wants = |k: &CodeActionKind| {
            only.is_empty() || only.iter().any(|o| k.as_str().starts_with(o.as_str()))
        };
        let mut actions = Vec::new();

        if wants(&CodeActionKind::QUICKFIX) {
            for d in &params.context.diagnostics {
                if d.source.as_deref() != Some("abl-lint") {
                    continue;
                }
                let Some(data) = &d.data else { continue };
                let rule = data["rule"].as_str().unwrap_or_default();
                if let Some((range, text, title)) = fix_of(data) {
                    actions.push(quick_fix(&uri, d, title, range, text, true));
                }
                if let Some(pos) = data.get("suppress").and_then(position_of) {
                    let text = format!(" /* lint-ignore {rule} */");
                    actions.push(quick_fix(
                        &uri,
                        d,
                        format!("Ignore {rule} on this line"),
                        Range::new(pos, pos),
                        text,
                        false,
                    ));
                }
            }
        }

        // Every fixable finding in the file (Source Action menu, or editor.codeActionsOnSave).
        if wants(&CodeActionKind::SOURCE_FIX_ALL) {
            if let Some((path, ana)) = self.doc(&uri) {
                let diags = {
                    let ws = self.ws.read().unwrap();
                    lint::run(&ws, &path, &ana)
                };
                let edits: Vec<TextEdit> = diags
                    .iter()
                    .filter_map(|d| d.data.as_ref().and_then(fix_of))
                    .map(|(range, text, _)| TextEdit {
                        range,
                        new_text: text,
                    })
                    .collect();
                // In the lightbulb (no `only` filter) it is only worth offering for several fixes.
                let min = if only.is_empty() { 2 } else { 1 };
                if edits.len() >= min {
                    let n = edits.len();
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Fix all ABL lint issues ({n})"),
                        kind: Some(CodeActionKind::SOURCE_FIX_ALL),
                        edit: Some(WorkspaceEdit {
                            changes: Some([(uri.clone(), edits)].into_iter().collect()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }));
                }
            }
        }
        Ok(Some(actions))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let Some((path, ana)) = self.doc(&params.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        Ok(Some(features::format(
            &ws,
            &path,
            &ana,
            &params.options,
            None,
        )))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let Some((path, ana)) = self.doc(&params.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let (a, b) = (
            params.range.start.line as usize,
            params.range.end.line as usize,
        );
        Ok(Some(features::format(
            &ws,
            &path,
            &ana,
            &params.options,
            Some((a, b)),
        )))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let Some((_, ana)) = self.doc(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(features::folding_ranges(&ana)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let Some((path, ana)) = self.doc(&params.text_document.uri) else {
            return Ok(None);
        };
        let ws = self.ws.read().unwrap();
        let data = features::semantic_tokens(&ws, &path, &ana);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
}

fn position_of(v: &Value) -> Option<Position> {
    Some(Position::new(
        v.get("line")?.as_u64()? as u32,
        v.get("character")?.as_u64()? as u32,
    ))
}

/// The `fix` recorded in a lint diagnostic's data: (range to replace, new text, title).
fn fix_of(data: &Value) -> Option<(Range, String, String)> {
    let fix = data.get("fix")?;
    let start = position_of(fix)?;
    let end = fix.get("end").and_then(position_of).unwrap_or(start);
    Some((
        Range::new(start, end),
        fix.get("text")?.as_str()?.to_string(),
        fix.get("title")?.as_str()?.to_string(),
    ))
}

fn quick_fix(
    uri: &Url,
    d: &Diagnostic,
    title: String,
    range: Range,
    text: String,
    preferred: bool,
) -> CodeActionOrCommand {
    let edit = TextEdit {
        range,
        new_text: text,
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![d.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some([(uri.clone(), vec![edit])].into_iter().collect()),
            ..Default::default()
        }),
        is_preferred: Some(preferred),
        ..Default::default()
    })
}

/// `abl-lsp check FILE...`: print diagnostics in a compiler-like format.
fn check(files: &[String]) -> i32 {
    let mut errors = 0;
    let mut ws = Workspace::new();
    ws.roots.extend(std::env::current_dir().ok());
    for f in files {
        let p = PathBuf::from(f);
        let Some(text) = workspace::read_text(&p) else {
            eprintln!("{f}: cannot read file");
            errors += 1;
            continue;
        };
        let a = ws.analyze_file(&p, text, None);
        for d in &a.diags {
            let pos = a.lines.position(&a.text, d.start);
            let sev = match d.severity {
                Severity::Error => {
                    errors += 1;
                    "error"
                }
                Severity::Warning => "warning",
            };
            println!(
                "{f}:{}:{}: {sev}: {}",
                pos.line + 1,
                pos.character + 1,
                d.message
            );
        }
        let lints = lint::run(&ws, &p, &a);
        for d in &lints {
            let code = match &d.code {
                Some(NumberOrString::String(s)) => s.as_str(),
                _ => "",
            };
            let sev = match d.severity {
                Some(DiagnosticSeverity::ERROR) => {
                    errors += 1;
                    "error"
                }
                Some(DiagnosticSeverity::WARNING) => "warning",
                Some(DiagnosticSeverity::INFORMATION) => "info",
                _ => "hint",
            };
            println!(
                "{f}:{}:{}: {sev} [{code}]: {}",
                d.range.start.line + 1,
                d.range.start.character + 1,
                d.message
            );
        }
        println!(
            "{f}: {} symbols, {} diagnostics",
            a.symbols.len(),
            a.diags.len() + lints.len()
        );
    }
    i32::from(errors > 0)
}

/// `abl-lsp format [--write] FILE...`: 4-space indentation, keywords as written.
fn format_files(args: &[String]) -> i32 {
    let write = args.iter().any(|a| a == "--write");
    let mut ws = Workspace::new();
    ws.roots.extend(std::env::current_dir().ok());
    let opts = FormattingOptions {
        tab_size: 4,
        insert_spaces: true,
        ..Default::default()
    };
    for f in args.iter().filter(|a| *a != "--write") {
        let p = PathBuf::from(f);
        let Some(text) = workspace::read_text(&p) else {
            eprintln!("{f}: cannot read file");
            return 1;
        };
        let ana = Arc::new(ws.analyze_file(&p, text, None));
        let mut edits: Vec<(usize, usize, String)> = features::format(&ws, &p, &ana, &opts, None)
            .into_iter()
            .map(|e| {
                (
                    ana.lines.offset(&ana.text, e.range.start),
                    ana.lines.offset(&ana.text, e.range.end),
                    e.new_text,
                )
            })
            .collect();
        edits.sort_by_key(|a| std::cmp::Reverse(a.0));
        let mut out = ana.text.clone();
        for (s, e, t) in edits {
            out.replace_range(s..e, &t);
        }
        if write {
            if std::fs::write(&p, out).is_err() {
                eprintln!("{f}: cannot write file");
                return 1;
            }
        } else {
            print!("{out}");
        }
    }
    0
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("abl-lsp {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("check") => std::process::exit(check(&args[1..])),
        Some("format") => std::process::exit(format_files(&args[1..])),
        _ => {}
    }
    let (service, socket) = LspService::new(|client| Backend {
        client,
        ws: Arc::new(RwLock::new(Workspace::new())),
    });
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}
