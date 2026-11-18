//! End-to-end test: runs the server binary over stdio against the sample project.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use lsp_types::Url;
use serde_json::{json, Value};

struct Lsp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: i64,
    pending: Vec<Value>,
}

impl Lsp {
    fn start(root: &Path) -> Self {
        // No global config: these tests must not depend on the machine's OE installs.
        let home = std::env::temp_dir().join(format!("abl-lsp-nohome-{}", std::process::id()));
        Self::start_with_home(root, &home)
    }

    fn start_with_home(root: &Path, home: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_abl"))
            // As vscode-languageclient starts it: the stdio transport adds --stdio.
            .args(["lsp", "--stdio"])
            .env("ABL_HOME", home)
            .env_remove("ABL_OE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start server");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut lsp = Lsp {
            child,
            stdin,
            out,
            next_id: 1,
            pending: Vec::new(),
        };
        let root_uri = Url::from_file_path(root).unwrap();
        lsp.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
                "initializationOptions": { "propath": ["src"] }
            }),
        );
        lsp.notify("initialized", json!({}));
        lsp
    }

    fn send(&mut self, v: &Value) {
        let body = v.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            self.out.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                len = v.parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        self.out.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let m = self.request_raw(method, params);
        assert!(m.get("error").is_none(), "{method} failed: {m}");
        m["result"].clone()
    }

    /// The whole response message, including any error.
    fn request_raw(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let m = self.read();
            if m.get("method").is_none() && m["id"] == json!(id) {
                return m;
            }
            self.pending.push(m);
        }
    }

    fn at_with(
        &mut self,
        method: &str,
        path: &Path,
        (line, character): (u32, u32),
        extra: Value,
    ) -> Value {
        let uri = Url::from_file_path(path).unwrap();
        let mut params = json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        self.request(method, params)
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn open(&mut self, path: &Path) -> Vec<Value> {
        let text = std::fs::read_to_string(path).unwrap();
        let uri = Url::from_file_path(path).unwrap();
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "abl", "version": 1, "text": text } }),
        );
        self.wait_diagnostics(&uri)
    }

    /// The next diagnostics published for `uri`.
    fn wait_diagnostics(&mut self, uri: &Url) -> Vec<Value> {
        loop {
            if let Some(i) = self.pending.iter().position(|m| {
                m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == json!(uri)
            }) {
                let m = self.pending.remove(i);
                return m["params"]["diagnostics"].as_array().unwrap().clone();
            }
            let m = self.read();
            self.pending.push(m);
        }
    }

    fn at(&mut self, method: &str, path: &Path, line: u32, character: u32) -> Value {
        let uri = Url::from_file_path(path).unwrap();
        self.request(method, json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }))
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// (line, column) of the `nth` occurrence of `needle` in `text`, plus `delta` characters.
fn pos(text: &str, needle: &str, nth: usize, delta: usize) -> (u32, u32) {
    let off = text
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle} #{nth} not found"))
        .0
        + delta;
    let line = text[..off].matches('\n').count();
    let col = off - text[..off].rfind('\n').map_or(0, |i| i + 1);
    (line as u32, col as u32)
}

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("samples")
        .canonicalize()
        .unwrap()
}

fn def_target(result: &Value) -> (String, u32) {
    let uri = result["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("no definition: {result}"));
    let file = uri.rsplit('/').next().unwrap().to_string();
    (
        file,
        result["range"]["start"]["line"].as_u64().unwrap() as u32,
    )
}

#[test]
fn procedure_features() {
    let root = samples();
    let file = root.join("src").join("customer-report.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);

    let diags = lsp.open(&file);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");

    // Outline
    let symbols = lsp.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": Url::from_file_path(&file).unwrap() } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    for expected in [
        "iCount",
        "oService",
        "bCustomer",
        "LoadCustomers",
        "FormatBalance",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    // RUN internal procedure -> PROCEDURE LoadCustomers
    let (l, c) = pos(&text, "RUN LoadCustomers", 0, 5);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(
        def_target(&d),
        (
            "customer-report.p".into(),
            pos(&text, "PROCEDURE LoadCustomers", 0, 0).0
        )
    );

    // Function call -> implementation, not the FORWARD prototype
    let (l, c) = pos(&text, "FormatBalance(ttCustomer", 0, 2);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(
        def_target(&d).1,
        pos(
            &text,
            "FUNCTION FormatBalance RETURNS CHARACTER (INPUT pdBalance AS DECIMAL):",
            0,
            0
        )
        .0
    );

    // Include reference -> include file
    let (l, c) = pos(&text, "{inc/ttCustomer.i}", 0, 3);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d).0, "ttCustomer.i");

    // Temp-table field defined in the include
    let (l, c) = pos(&text, "ttCustomer.Name:", 0, 12);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d), ("ttCustomer.i".into(), 5));

    // Field through a buffer
    let (l, c) = pos(&text, "bCustomer.Balance", 0, 11);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d), ("ttCustomer.i".into(), 6));

    // Preprocessor name from the include
    let (l, c) = pos(&text, "{&MAX-CUSTOMERS}", 0, 4);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d), ("ttCustomer.i".into(), 1));

    // Method on a class-typed variable (class found through USING app.*)
    let (l, c) = pos(&text, "oService:ProcessOrders", 0, 10);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d).0, "OrderService.cls");

    // Class name -> class file
    let (l, c) = pos(&text, "NEW OrderService", 0, 6);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d), ("OrderService.cls".into(), 3));

    // RUN of an external procedure
    let (l, c) = pos(&text, "util/format.p", 0, 3);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d).0, "format.p");

    // Hover
    let (l, c) = pos(&text, "iCount = iCount", 0, 1);
    let h = lsp.at("textDocument/hover", &file, l, c);
    let value = h["contents"]["value"].as_str().unwrap();
    assert!(
        value.contains("DEFINE VARIABLE iCount AS INTEGER NO-UNDO"),
        "{value}"
    );

    // Keyword abbreviation hover
    let (l, c) = pos(&text, "DISPLAY", 0, 1);
    let h = lsp.at("textDocument/hover", &file, l, c);
    assert!(h["contents"]["value"].as_str().unwrap().contains("DISPLAY"));

    // Field completion after `bCustomer.`
    let (l, c) = pos(&text, "bCustomer.CustNum", 0, 10);
    let items = lsp.at("textDocument/completion", &file, l, c);
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["CustNum", "Name", "Balance", "Region"]);

    // Member completion after `oService:` includes inherited and interface members
    let (l, c) = pos(&text, "oService:ProcessOrders", 0, 9);
    let items = lsp.at("textDocument/completion", &file, l, c);
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    for expected in ["ProcessOrders", "LastStatus", "Log", "ServiceName"] {
        assert!(
            labels.contains(&expected),
            "{expected} missing from {labels:?}"
        );
    }

    // General completion includes locals, include symbols and keywords
    let (l, c) = pos(&text, "    iCount = iCount", 0, 4);
    let items = lsp.at("textDocument/completion", &file, l, c);
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    for expected in ["iCount", "ttCustomer", "LoadCustomers", "DEFINE"] {
        assert!(labels.contains(&expected), "{expected} missing");
    }

    // Semantic tokens
    let t = lsp.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": Url::from_file_path(&file).unwrap() } }),
    );
    let data = t["data"].as_array().unwrap();
    assert!(data.len() > 100 && data.len().is_multiple_of(5));

    // Workspace symbols (index is built in the background)
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = lsp.request("workspace/symbol", json!({ "query": "orderserv" }));
        if r.as_array()
            .is_some_and(|a| a.iter().any(|s| s["name"] == "app.OrderService"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "index never contained app.OrderService: {r}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn class_features() {
    let root = samples();
    let file = root.join("src").join("app").join("OrderService.cls");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    assert!(lsp.open(&file).is_empty());

    // THIS-OBJECT:Log -> inherited method in BaseService
    let (l, c) = pos(&text, "THIS-OBJECT:Log", 0, 13);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d), ("BaseService.cls".into(), 12));

    // Unqualified inherited call
    let (l, c) = pos(&text, "Log(e:", 0, 1);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d).0, "BaseService.cls");

    // Enum member via the enum type
    let (l, c) = pos(&text, "OrderStatus:Pending", 0, 13);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d), ("OrderStatus.cls".into(), 1));

    // Superclass and interface names
    let (l, c) = pos(&text, "INHERITS BaseService", 0, 10);
    assert_eq!(
        def_target(&lsp.at("textDocument/definition", &file, l, c)).0,
        "BaseService.cls"
    );
    let (l, c) = pos(&text, "IMPLEMENTS IOrderProcessor", 0, 12);
    assert_eq!(
        def_target(&lsp.at("textDocument/definition", &file, l, c)).0,
        "IOrderProcessor.cls"
    );

    // Method parameter
    let (l, c) = pos(&text, "TO piMax", 0, 4);
    let d = lsp.at("textDocument/definition", &file, l, c);
    assert_eq!(def_target(&d).1, pos(&text, "INPUT piMax", 0, 0).0);

    // CATCH variable hover
    let (l, c) = pos(&text, "e:GetMessage", 0, 0);
    let h = lsp.at("textDocument/hover", &file, l, c);
    assert!(h["contents"]["value"]
        .as_str()
        .unwrap()
        .contains("CATCH e AS Progress.Lang.Error"));
}

#[test]
fn diagnostics_and_appbuilder_files() {
    let root = samples();
    let mut lsp = Lsp::start(&root);

    let broken = lsp.open(&root.join("src").join("broken.p"));
    let messages: Vec<&str> = broken
        .iter()
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    let all = messages.join("\n");
    for expected in [
        "'cName' is already defined on line 2",
        "needs an AS <type> or LIKE <field> clause",
        "'(' is never closed",
        "DO block starting on line 9 has no matching END",
        "Unterminated string",
        "Include file 'inc/doesNotExist.i' was not found on the PROPATH",
    ] {
        assert!(all.contains(expected), "missing '{expected}' in:\n{all}");
    }

    let legacy = root.join("src").join("legacy.w");
    let diags = lsp.open(&legacy);
    let messages: Vec<&str> = diags
        .iter()
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        messages,
        vec![
            "Variable 'C-Win' is never used",
            "'COL' is an abbreviation of COLUMN"
        ],
        "legacy.w should only have the lint findings"
    );
    let symbols = lsp.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": Url::from_file_path(&legacy).unwrap() } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names.iter().filter(|n| **n == "InitializeWindow").count(),
        1,
        "{names:?}"
    );
    assert!(
        names.contains(&"CHOOSE OF btnOk IN FRAME DEFAULT-FRAME"),
        "{names:?}"
    );
    assert!(names.contains(&"WINDOW-NAME"), "{names:?}");
}

/// (file name, line) of every location, sorted.
fn locations(v: &Value) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = v.as_array().unwrap().iter().map(def_target).collect();
    out.sort();
    out
}

fn line_of(text: &str, needle: &str) -> u32 {
    pos(text, needle, 0, 0).0
}

#[test]
fn database_schema() {
    let root = samples();
    let file = root.join("src").join("db-report.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let df = std::fs::read_to_string(root.join("db").join("sports.df")).unwrap();
    let mut lsp = Lsp::start(&root);
    assert!(lsp.open(&file).is_empty());

    // The schema loads in the background together with the index.
    let balance = pos(&text, "Customer.Balance", 0, 10);
    let deadline = Instant::now() + Duration::from_secs(10);
    let d = loop {
        let d = lsp.at_with("textDocument/definition", &file, balance, json!({}));
        if !d.is_null() {
            break d;
        }
        assert!(Instant::now() < deadline, "schema never loaded");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        def_target(&d),
        ("sports.df".into(), line_of(&df, "ADD FIELD \"Balance\""))
    );

    // Table name, db-qualified field, buffer FOR a db table
    let d = lsp.at_with(
        "textDocument/definition",
        &file,
        pos(&text, "EACH Customer", 0, 6),
        json!({}),
    );
    assert_eq!(def_target(&d), ("sports.df".into(), 0));
    let d = lsp.at_with(
        "textDocument/definition",
        &file,
        pos(&text, "sports.Customer.City", 0, 17),
        json!({}),
    );
    assert_eq!(
        def_target(&d),
        ("sports.df".into(), line_of(&df, "ADD FIELD \"City\""))
    );
    let d = lsp.at_with(
        "textDocument/definition",
        &file,
        pos(&text, "bCust.CustNum", 0, 7),
        json!({}),
    );
    assert_eq!(
        def_target(&d),
        (
            "sports.df".into(),
            line_of(&df, "ADD FIELD \"CustNum\" OF \"Customer\"")
        )
    );
    let d = lsp.at_with(
        "textDocument/definition",
        &file,
        pos(&text, "sports.Order", 0, 8),
        json!({}),
    );
    assert_eq!(
        def_target(&d),
        ("sports.df".into(), line_of(&df, "ADD TABLE \"Order\""))
    );

    // Hover shows schema attributes
    let h = lsp.at_with("textDocument/hover", &file, balance, json!({}));
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(
        v.contains("FIELD Customer.Balance AS DECIMAL") && v.contains("Outstanding balance"),
        "{v}"
    );

    // Field completion through the buffer, table completion after `sports.`
    let items = lsp.at_with(
        "textDocument/completion",
        &file,
        pos(&text, "bCust.CustNum", 0, 6),
        json!({}),
    );
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["CustNum", "Name", "City", "Balance"]);
    let items = lsp.at_with(
        "textDocument/completion",
        &file,
        pos(&text, "sports.Order", 0, 7),
        json!({}),
    );
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["Customer", "Order"]);
}

#[test]
fn references_and_rename() {
    let root = samples();
    let file = root.join("src").join("customer-report.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    lsp.open(&file);

    // Local variable: declaration + 4 uses
    let refs = lsp.at_with(
        "textDocument/references",
        &file,
        pos(&text, "iCount = iCount", 0, 1),
        json!({ "context": { "includeDeclaration": true } }),
    );
    assert_eq!(refs.as_array().unwrap().len(), 5, "{refs}");
    let refs = lsp.at_with(
        "textDocument/references",
        &file,
        pos(&text, "iCount = iCount", 0, 1),
        json!({ "context": { "includeDeclaration": false } }),
    );
    assert_eq!(refs.as_array().unwrap().len(), 4);

    // Temp-table field defined in an include, used directly and through a buffer
    let refs = lsp.at_with(
        "textDocument/references",
        &file,
        pos(&text, "ttCustomer.Name:", 0, 12),
        json!({ "context": { "includeDeclaration": true } }),
    );
    let locs = locations(&refs);
    assert_eq!(locs.len(), 4, "{locs:?}");
    assert!(locs.contains(&("ttCustomer.i".into(), 5)));
    assert!(locs.contains(&("customer-report.p".into(), line_of(&text, "bCustomer.Name"))));

    // Document highlight = same-file occurrences
    let hl = lsp.at_with(
        "textDocument/documentHighlight",
        &file,
        pos(&text, "RUN LoadCustomers", 0, 5),
        json!({}),
    );
    assert_eq!(hl.as_array().unwrap().len(), 2);

    // Rename an internal procedure: RUN target + definition
    let edit = lsp.at_with(
        "textDocument/rename",
        &file,
        pos(&text, "RUN LoadCustomers", 0, 5),
        json!({ "newName": "FillCustomers" }),
    );
    let changes = edit["changes"].as_object().unwrap();
    let edits: Vec<&Value> = changes
        .values()
        .flat_map(|v| v.as_array().unwrap())
        .collect();
    assert_eq!(edits.len(), 2, "{edit}");
    assert!(edits.iter().all(|e| e["newText"] == "FillCustomers"));

    // prepareRename returns the exact name span; classes are refused
    let r = lsp.at_with(
        "textDocument/prepareRename",
        &file,
        pos(&text, "iCount = iCount", 0, 2),
        json!({}),
    );
    assert_eq!(r["start"]["character"], 4);
    assert_eq!(r["end"]["character"], 10);
    let (l, c) = pos(&text, "NEW OrderService", 0, 6);
    let uri = Url::from_file_path(&file).unwrap();
    let m = lsp.request_raw(
        "textDocument/prepareRename",
        json!({ "textDocument": { "uri": uri }, "position": { "line": l, "character": c } }),
    );
    assert!(
        m["error"]["message"]
            .as_str()
            .unwrap()
            .contains("rename the file"),
        "{m}"
    );
    let m = lsp.request_raw("textDocument/rename", json!({ "textDocument": { "uri": uri }, "position": { "line": 10, "character": 18 }, "newName": "bad name" }));
    assert!(m.get("error").is_some());
}

#[test]
fn cross_file_references() {
    let root = samples();
    let file = root.join("src").join("app").join("OrderService.cls");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    lsp.open(&file);
    // Wait for the index so other files are searched.
    let deadline = Instant::now() + Duration::from_secs(10);
    while lsp
        .request("workspace/symbol", json!({ "query": "BaseService" }))
        .as_array()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    // Inherited method: definition in BaseService + 2 calls in OrderService
    let refs = lsp.at_with(
        "textDocument/references",
        &file,
        pos(&text, "THIS-OBJECT:Log", 0, 13),
        json!({ "context": { "includeDeclaration": true } }),
    );
    let locs = locations(&refs);
    assert_eq!(locs.len(), 3, "{locs:?}");
    assert!(locs.contains(&("BaseService.cls".into(), 12)));
    // Method called from a .p through a class-typed variable
    let refs = lsp.at_with(
        "textDocument/references",
        &file,
        pos(&text, "INTEGER ProcessOrders", 0, 9),
        json!({ "context": { "includeDeclaration": true } }),
    );
    let files: Vec<String> = locations(&refs).into_iter().map(|l| l.0).collect();
    assert!(
        files.contains(&"customer-report.p".to_string()),
        "{files:?}"
    );
}

#[test]
fn signature_help_and_folding() {
    let root = samples();
    let file = root.join("src").join("customer-report.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    lsp.open(&file);

    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "FormatBalance(ttCustomer", 0, 14),
        json!({}),
    );
    assert_eq!(
        s["signatures"][0]["label"],
        "FormatBalance(INPUT pdBalance AS DECIMAL)"
    );
    assert_eq!(s["activeParameter"], 0);

    // RUN file.p (...): parameters come from the called file; cursor after the comma
    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "OUTPUT cTitle)", 0, 0),
        json!({}),
    );
    assert_eq!(
        s["signatures"][0]["label"],
        "src/util/format.p(INPUT pcTitle AS CHARACTER, OUTPUT pcResult AS CHARACTER)"
    );
    assert_eq!(s["activeParameter"], 1);

    // NEW Class(...) -> constructor
    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "NEW OrderService()", 0, 17),
        json!({}),
    );
    assert_eq!(s["signatures"][0]["label"], "OrderService()");

    let uri = Url::from_file_path(&file).unwrap();
    let folds = lsp.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let ranges: Vec<(u64, u64)> = folds
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["startLine"].as_u64().unwrap(),
                f["endLine"].as_u64().unwrap(),
            )
        })
        .collect();
    let proc_start = line_of(&text, "PROCEDURE LoadCustomers") as u64;
    let proc_end = line_of(&text, "END PROCEDURE") as u64;
    assert!(ranges.contains(&(proc_start, proc_end - 1)), "{ranges:?}");
    assert!(
        ranges.contains(&(0, 3)),
        "header comment should fold: {ranges:?}"
    );
}

#[test]
fn lint_rules_and_levels() {
    let root = samples();
    let file = root.join("src").join("lint.p");
    let uri = Url::from_file_path(&file).unwrap();
    let mut lsp = Lsp::start(&root);

    let summary = |diags: &[Value]| -> Vec<(String, String, u64)> {
        let mut v: Vec<(String, String, u64)> = diags
            .iter()
            .map(|d| {
                (
                    d["code"].as_str().unwrap_or("").to_string(),
                    d["message"].as_str().unwrap().to_string(),
                    d["severity"].as_u64().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };

    let diags = lsp.open(&file);
    assert_eq!(
        summary(&diags),
        vec![
            (
                "unused-parameter".into(),
                "Parameter 'pcUnused' is never used".into(),
                2
            ),
            (
                "unused-parameter".into(),
                "Parameter 'piSpare' is never used".into(),
                2
            ),
            (
                "unused-variable".into(),
                "Variable 'cUnused' is never used".into(),
                2
            ),
        ]
    );
    // Faded like Go's unused names
    assert!(diags.iter().all(|d| d["tags"] == json!([1])));

    // Turn one rule off and make the other an error, without restarting.
    lsp.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "abl": { "lint": { "unused-parameter": "off", "unused-variable": "error" } } } }),
    );
    let diags = lsp.wait_diagnostics(&uri);
    assert_eq!(
        summary(&diags),
        vec![(
            "unused-variable".into(),
            "Variable 'cUnused' is never used".into(),
            1
        )]
    );

    // Back to defaults when the setting is removed.
    lsp.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "abl": { "lint": {} } } }),
    );
    assert_eq!(lsp.wait_diagnostics(&uri).len(), 3);
}

/// Applies insert-only TextEdits to `text` (ASCII positions).
fn apply_inserts(text: &str, edits: &[Value]) -> String {
    let off = |p: &Value| {
        let (l, c) = (
            p["line"].as_u64().unwrap() as usize,
            p["character"].as_u64().unwrap() as usize,
        );
        text.split_inclusive('\n')
            .take(l)
            .map(str::len)
            .sum::<usize>()
            + c
    };
    let mut ins: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                off(&e["range"]["start"]),
                off(&e["range"]["end"]),
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    ins.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = text.to_string();
    for (start, end, s) in ins {
        out.replace_range(start..end, &s);
    }
    out
}

#[test]
fn no_undo_find_and_quick_fixes() {
    let root = samples();
    let file = root.join("src").join("style.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let uri = Url::from_file_path(&file).unwrap();
    let mut lsp = Lsp::start(&root);

    let diags = lsp.open(&file);
    let mut found: Vec<(String, u64)> = diags
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_string(),
                d["range"]["start"]["line"].as_u64().unwrap(),
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            ("find-without-no-error".into(), 9),
            ("missing-no-undo".into(), 1),
            ("missing-no-undo".into(), 3),
            ("missing-no-undo".into(), 4),
        ]
    );

    // Quick fixes for one finding: the fix, then the lint-ignore comment.
    let d = diags
        .iter()
        .find(|d| d["range"]["start"]["line"] == 1)
        .unwrap()
        .clone();
    let actions = lsp.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": uri }, "range": d["range"], "context": { "diagnostics": [d] } }),
    );
    let titles: Vec<&str> = actions
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Add NO-UNDO",
            "Ignore missing-no-undo on this line",
            "Fix all ABL lint issues (4)"
        ]
    );
    let edit = &actions[0]["edit"]["changes"][uri.as_str()][0];
    let fixed = apply_inserts(&text, std::slice::from_ref(edit));
    assert!(
        fixed.contains("DEFINE VARIABLE cUndoable AS CHARACTER NO-UNDO INITIAL \"x\"."),
        "{fixed}"
    );
    let ignore = &actions[1]["edit"]["changes"][uri.as_str()][0];
    assert!(apply_inserts(&text, std::slice::from_ref(ignore))
        .contains("INITIAL \"x\". /* lint-ignore missing-no-undo */\n"));

    // Fix all, then the file is clean.
    let actions = lsp.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": uri }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                "context": { "diagnostics": [], "only": ["source.fixAll"] } }),
    );
    assert_eq!(actions[0]["title"], "Fix all ABL lint issues (4)");
    let edits = actions[0]["edit"]["changes"][uri.as_str()]
        .as_array()
        .unwrap()
        .clone();
    let fixed = apply_inserts(&text, &edits);
    for expected in [
        "def var iShort as int NO-UNDO.",
        "DEFINE TEMP-TABLE ttWork NO-UNDO\n",
        "FIND FIRST ttWork WHERE ttWork.Id = 1 NO-ERROR.",
    ] {
        assert!(fixed.contains(expected), "missing {expected:?} in\n{fixed}");
    }
    lsp.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": fixed }] }));
    assert_eq!(lsp.wait_diagnostics(&uri), Vec::<Value>::new());
}

#[test]
fn locks_buffers_and_abbreviations() {
    let root = samples();
    let file = root.join("src").join("locks.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let uri = Url::from_file_path(&file).unwrap();
    let mut lsp = Lsp::start(&root);

    let diags = lsp.open(&file);
    let mut found: Vec<(String, u32)> = diags
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_string(),
                d["range"]["start"]["line"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            (
                "find-without-lock".into(),
                line_of(&text, "FIND FIRST bCust WHERE")
            ),
            (
                "for-each-without-lock".into(),
                line_of(&text, "FOR EACH Customer WHERE")
            ),
            ("keyword-abbreviation".into(), line_of(&text, "DISP iTotal")),
            ("unused-buffer".into(), line_of(&text, "bUnused")),
        ],
        "{diags:#?}"
    );
    let abbrev = diags
        .iter()
        .find(|d| d["code"] == "keyword-abbreviation")
        .unwrap();
    assert_eq!(abbrev["severity"], 4, "hint by default");

    let actions = lsp.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": uri }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                "context": { "diagnostics": [], "only": ["source.fixAll"] } }),
    );
    assert_eq!(actions[0]["title"], "Fix all ABL lint issues (3)");
    let edits = actions[0]["edit"]["changes"][uri.as_str()]
        .as_array()
        .unwrap()
        .clone();
    let fixed = apply_inserts(&text, &edits);
    for expected in [
        "FOR EACH Customer NO-LOCK WHERE Customer.Balance > 0,",
        "FIND FIRST bCust NO-LOCK WHERE bCust.CustNum = 1 NO-ERROR.",
        "DISPLAY iTotal.",
        "MESSAGE INT(\"3\") LOG(10).",
    ] {
        assert!(fixed.contains(expected), "missing {expected:?} in\n{fixed}");
    }
    lsp.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": fixed }] }));
    let left: Vec<String> = lsp
        .wait_diagnostics(&uri)
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(left, vec!["unused-buffer"]);
}

#[test]
fn unqualified_fields_follow_the_record_phrase() {
    let root = samples();
    let file = root.join("src").join("where-fields.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let df = std::fs::read_to_string(root.join("db").join("sports.df")).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());

    let cust = line_of(&df, "ADD FIELD \"CustNum\" OF \"Customer\"");
    let order = line_of(&df, "ADD FIELD \"CustNum\" OF \"Order\"");
    let def = |lsp: &mut Lsp, needle: &str| {
        let d = lsp.at_with(
            "textDocument/definition",
            &file,
            pos(&text, needle, 0, 1),
            json!({}),
        );
        if d.is_null() {
            None
        } else {
            Some(def_target(&d))
        }
    };
    // The schema loads in the background.
    let deadline = Instant::now() + Duration::from_secs(10);
    while def(&mut lsp, "CustNum = 1").is_none() {
        assert!(Instant::now() < deadline, "schema never loaded");
        std::thread::sleep(Duration::from_millis(100));
    }

    // CustNum is in both tables: the record phrase decides.
    assert_eq!(
        def(&mut lsp, "CustNum = 1"),
        Some(("sports.df".into(), order)),
        "FOR EACH Order WHERE"
    );
    assert_eq!(
        def(&mut lsp, "CustNum = 2"),
        Some(("sports.df".into(), order)),
        "FIND FIRST Order WHERE"
    );
    assert_eq!(
        def(&mut lsp, "CustNum = 3"),
        Some(("sports.df".into(), cust)),
        "CAN-FIND(FIRST Customer WHERE"
    );
    // Inside a join and its block: each field finds its own table.
    assert_eq!(
        def(&mut lsp, "Balance > 100"),
        Some(("sports.df".into(), line_of(&df, "ADD FIELD \"Balance\"")))
    );
    assert_eq!(
        def(&mut lsp, "OrderDate <> ?"),
        Some(("sports.df".into(), line_of(&df, "ADD FIELD \"OrderDate\"")))
    );
    assert_eq!(
        def(&mut lsp, "Name OrderDate"),
        Some(("sports.df".into(), line_of(&df, "ADD FIELD \"Name\"")))
    );

    // Completion in the FOR EACH Order block offers Order's fields first.
    let items = lsp.at_with(
        "textDocument/completion",
        &file,
        pos(&text, "OrderNum.", 0, 5),
        json!({}),
    );
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        &labels[..3],
        &["OrderNum", "CustNum", "OrderDate"],
        "{labels:?}"
    );
}

/// Waits until the background index has the given workspace symbol.
fn wait_index(lsp: &mut Lsp, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while lsp
        .request("workspace/symbol", json!({ "query": name }))
        .as_array()
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline, "index never had {name}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn classes_chains_overloads_and_builtins() {
    let root = samples();
    let file = root.join("src").join("oo-client.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let oo = root.join("src").join("oo");
    let square = std::fs::read_to_string(oo.join("Square.cls")).unwrap();
    let shape = std::fs::read_to_string(oo.join("Shape.cls")).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());

    let def = |lsp: &mut Lsp, needle: &str, delta: usize| {
        let d = lsp.at_with(
            "textDocument/definition",
            &file,
            pos(&text, needle, 0, delta),
            json!({}),
        );
        def_target(&d)
    };
    let sq = |needle: &str| ("Square.cls".to_string(), line_of(&square, needle));
    let sh = |needle: &str| ("Shape.cls".to_string(), line_of(&shape, needle));

    // Static access, chains, overloads by argument count.
    assert_eq!(def(&mut lsp, "Square:Make", 8), sq("STATIC oo.Square Make"));
    assert_eq!(def(&mut lsp, "oSq:Grow(1.0)", 5), sq("oo.Square Grow"));
    assert_eq!(
        def(&mut lsp, "Describe(\"big", 1),
        sh("Describe(INPUT pPrefix")
    );
    assert_eq!(def(&mut lsp, "Square:Unit", 8), sq("PROPERTY Unit"));
    assert_eq!(def(&mut lsp, "Unit:ToString", 6), sq("CHARACTER ToString"));
    // CAST(x, T):Member and NEW T():Member
    assert_eq!(def(&mut lsp, "Square):Grow", 9), sq("oo.Square Grow"));
    assert_eq!(
        def(&mut lsp, "Grow(2.0):ToString", 11),
        sq("CHARACTER ToString")
    );
    assert_eq!(def(&mut lsp, "Describe().", 1), sh("CHARACTER Describe():"));
    // Built-in classes: Progress.Json and the implicit Progress.Lang.*
    assert_eq!(def(&mut lsp, "oJson:Add", 7).0, "JsonObject.cls");
    assert_eq!(def(&mut lsp, "GetJsonText", 1).0, "JsonObject.cls");
    assert_eq!(def(&mut lsp, "eErr:GetMessage", 6).0, "ProError.cls");
    assert_eq!(def(&mut lsp, "eErr:ReturnValue", 6).0, "AppError.cls");
    let hover = lsp.at_with(
        "textDocument/hover",
        &file,
        pos(&text, "AppError:", 0, 2),
        json!({}),
    );
    assert!(
        hover.to_string().contains("Progress.Lang.AppError"),
        "{hover}"
    );

    // Completion after a chain.
    let (l, c) = pos(&text, "oSq:Grow(1.0):", 0, "oSq:Grow(1.0):".len());
    let items = lsp.at_with("textDocument/completion", &file, (l, c), json!({}));
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    for expected in [
        "Grow", "Area", "Describe", "Corners", "ToString", "GetClass",
    ] {
        assert!(
            labels.contains(&expected),
            "missing {expected} in {labels:?}"
        );
    }

    // Signature help lists both Describe overloads and picks the 1-parameter one.
    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "Describe(\"big", 0, 9),
        json!({}),
    );
    let labels: Vec<&str> = s["signatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels.len(), 2, "{s}");
    assert_eq!(
        labels[s["activeSignature"].as_u64().unwrap() as usize],
        "Describe(INPUT pPrefix AS CHARACTER)"
    );
    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "NEW JsonObject(", 0, 15),
        json!({}),
    );
    assert_eq!(s["signatures"][0]["label"], "JsonObject()");
    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "Square(3.0)", 0, 7),
        json!({}),
    );
    assert_eq!(
        s["signatures"].as_array().unwrap().len(),
        2,
        "both constructors: {s}"
    );
    assert_eq!(s["activeSignature"], 1);
}

#[test]
fn class_contracts_and_override_rename() {
    let root = samples();
    let oo = root.join("src").join("oo");
    let broken = oo.join("Broken.cls");
    let text = std::fs::read_to_string(&broken).unwrap();
    let uri = Url::from_file_path(&broken).unwrap();
    let mut lsp = Lsp::start(&root);

    let diags = lsp.open(&broken);
    let mut found: Vec<(String, u32)> = diags
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_string(),
                d["range"]["start"]["line"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            (
                "missing-implementation".into(),
                line_of(&text, "CLASS oo.Broken")
            ),
            ("missing-override".into(), line_of(&text, "DECIMAL Area")),
            ("override-without-base".into(), line_of(&text, "Explode")),
        ],
        "{diags:#?}"
    );
    let msg = |code: &str| {
        diags.iter().find(|d| d["code"] == code).unwrap()["message"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        msg("missing-implementation"),
        "oo.Broken does not implement method Corners() from oo.IHasCorners"
    );
    assert_eq!(diags[0]["severity"], 1, "contract rules are errors");

    // Quick fix: Add OVERRIDE after the access modifier.
    let d = diags
        .iter()
        .find(|d| d["code"] == "missing-override")
        .unwrap()
        .clone();
    let actions = lsp.request("textDocument/codeAction", json!({ "textDocument": { "uri": uri }, "range": d["range"], "context": { "diagnostics": [d] } }));
    assert_eq!(actions[0]["title"], "Add OVERRIDE");
    let fixed = apply_inserts(
        &text,
        std::slice::from_ref(&actions[0]["edit"]["changes"][uri.as_str()][0]),
    );
    assert!(
        fixed.contains("METHOD PUBLIC OVERRIDE DECIMAL Area():"),
        "{fixed}"
    );

    // The well-formed classes are clean.
    for f in ["Square.cls", "Shape.cls", "IHasCorners.cls"] {
        assert_eq!(lsp.open(&oo.join(f)), Vec::<Value>::new(), "{f}");
    }

    // Renaming the abstract method renames every override with the same signature.
    // Wait on a file this test never opens: workspace/symbol also answers from
    // open files, so "Broken" shows up before the background scan is done.
    wait_index(&mut lsp, "LoadCustomers");
    let shape = oo.join("Shape.cls");
    let shape_text = std::fs::read_to_string(&shape).unwrap();
    let edit = lsp.at_with(
        "textDocument/rename",
        &shape,
        pos(&shape_text, "DECIMAL Area", 0, 9),
        json!({ "newName": "Surface" }),
    );
    let mut per_file: Vec<(String, usize)> = edit["changes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(u, e)| {
            (
                u.rsplit('/').next().unwrap().to_string(),
                e.as_array().unwrap().len(),
            )
        })
        .collect();
    per_file.sort();
    assert_eq!(
        per_file,
        vec![
            ("Broken.cls".into(), 1),
            ("Shape.cls".into(), 2),
            ("Square.cls".into(), 1),
            ("oo-client.p".into(), 1)
        ],
        "{edit}"
    );

    // Built-in members cannot be renamed.
    let client = root.join("src").join("oo-client.p");
    let ctext = std::fs::read_to_string(&client).unwrap();
    lsp.open(&client);
    let (l, c) = pos(&ctext, "oJson:Add", 0, 7);
    let m = lsp.request_raw("textDocument/prepareRename", json!({ "textDocument": { "uri": Url::from_file_path(&client).unwrap() }, "position": { "line": l, "character": c } }));
    assert!(
        m["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Built-in"),
        "{m}"
    );
}

#[test]
fn preprocessor_and_include_arguments() {
    let root = samples();
    let file = root.join("src").join("preproc.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());

    // &IF branches follow the defined names; the &ELSE variable does not exist.
    let symbols = lsp.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": Url::from_file_path(&file).unwrap() } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    for expected in ["iRow", "cLoaded", "cOs"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    assert!(!names.contains(&"cMissing"), "{names:?}");

    // Temp-tables and fields named by include arguments.
    let def = |lsp: &mut Lsp, needle: &str, delta: usize| {
        def_target(&lsp.at_with(
            "textDocument/definition",
            &file,
            pos(&text, needle, 0, delta),
            json!({}),
        ))
    };
    assert_eq!(def(&mut lsp, "ttOrder.OrderNum", 10), ("ttdef.i".into(), 3));
    assert_eq!(def(&mut lsp, "EACH ttLine", 6), ("ttdef.i".into(), 2));
    assert_eq!(def(&mut lsp, "ttLine.LineNum", 9), ("ttdef.i".into(), 3));
    let (l, c) = pos(&text, "ttOrder.Descr", 0, "ttOrder.".len());
    let items = lsp.at_with("textDocument/completion", &file, (l, c), json!({}));
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert!(
        labels.contains(&"OrderNum") && labels.contains(&"Descr"),
        "{labels:?}"
    );

    // Hover on an expanded reference shows its value; the variable it names resolves.
    let h = lsp.at_with(
        "textDocument/hover",
        &file,
        pos(&text, "DO {&LOOP-VAR}", 0, 5),
        json!({}),
    );
    assert!(h.to_string().contains("iRow"), "{h}");
    let h = lsp.at_with(
        "textDocument/hover",
        &file,
        pos(&text, "ttOrder.OrderNum = {&LOOP-VAR}", 0, 21),
        json!({}),
    );
    assert!(h.to_string().contains("iRow"), "{h}");
}

#[test]
fn builtin_functions_and_handles() {
    let root = samples();
    let file = root.join("src").join("handles.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());

    let hover = |lsp: &mut Lsp, needle: &str, delta: usize| {
        lsp.at_with(
            "textDocument/hover",
            &file,
            pos(&text, needle, 0, delta),
            json!({}),
        )
        .to_string()
    };
    assert!(hover(&mut lsp, "QUERY-PREPARE", 2).contains("Compiles a query string"));
    assert!(hover(&mut lsp, "SUBSTRING(", 2).contains("Part of a string"));
    assert!(hover(&mut lsp, "DATE-FORMAT", 2).contains("Date order"));
    assert!(hover(&mut lsp, "TODAY", 2).contains("Current date"));
    assert!(hover(&mut lsp, "SESSION:", 2).contains("system handle"));
    // hFld = hBuf:BUFFER-FIELD(...), and hBuf = BUFFER ttCustomer:HANDLE
    assert!(hover(&mut lsp, "BUFFER-VALUE,", 2).contains("BUFFER-FIELD:BUFFER-VALUE"));

    let labels = |lsp: &mut Lsp, needle: &str| -> Vec<String> {
        let (l, c) = pos(&text, needle, 0, needle.len());
        let items = lsp.at_with("textDocument/completion", &file, (l, c), json!({}));
        items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect()
    };
    let has = |v: &[String], s: &str| v.iter().any(|x| x == s);
    let l = labels(&mut lsp, "SUBSTRING(hFld:");
    assert!(
        has(&l, "BUFFER-VALUE") && has(&l, "STRING-VALUE") && !has(&l, "QUERY-PREPARE"),
        "{l:?}"
    );
    let l = labels(&mut lsp, "SESSION:");
    assert!(has(&l, "DATE-FORMAT") && has(&l, "SET-WAIT-STATE"), "{l:?}");
    let l = labels(&mut lsp, "GET-BUFFER-HANDLE(1):");
    assert!(has(&l, "BUFFER-FIELD") && has(&l, "FIND-FIRST"), "{l:?}");
    let l = labels(&mut lsp, "BUFFER-FIELD(1):");
    assert!(has(&l, "NAME") && has(&l, "BUFFER-VALUE"), "{l:?}");
    let l = labels(&mut lsp, "hQuery:SET-BUFFERS(hBuf).\nhQuery:");
    assert!(has(&l, "QUERY-OPEN") && !has(&l, "BUFFER-VALUE"), "{l:?}");

    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "1, 10)", 0, 3),
        json!({}),
    );
    assert_eq!(s["signatures"][0]["label"], "SUBSTRING(source AS CHARACTER, position AS INTEGER, [length AS INTEGER], [type AS CHARACTER])");
    assert_eq!(s["activeParameter"], 2);
}

#[test]
fn overloads_by_argument_type() {
    let root = samples();
    let file = root.join("src").join("oo-client.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());
    let json = std::fs::read_to_string(
        root.join("..")
            .join("crates")
            .join("core")
            .join("builtin")
            .join("Progress")
            .join("Json")
            .join("ObjectModel")
            .join("JsonObject.cls"),
    )
    .unwrap();
    let add_line = |ty: &str| {
        line_of(
            &json,
            &format!("Add(INPUT propertyName AS CHARACTER, INPUT value AS {ty})"),
        )
    };
    let def = |lsp: &mut Lsp, needle: &str| {
        def_target(&lsp.at_with(
            "textDocument/definition",
            &file,
            pos(&text, needle, 0, 7),
            json!({}),
        ))
    };
    assert_eq!(
        def(&mut lsp, "oJson:Add(\"side\""),
        ("JsonObject.cls".into(), add_line("DECIMAL"))
    );
    assert_eq!(
        def(&mut lsp, "oJson:Add(\"n\""),
        ("JsonObject.cls".into(), add_line("INTEGER"))
    );
    assert_eq!(
        def(&mut lsp, "oJson:Add(\"when\""),
        ("JsonObject.cls".into(), add_line("DATE"))
    );
    assert_eq!(
        def(&mut lsp, "oJson:Add(\"child\""),
        (
            "JsonObject.cls".into(),
            add_line("Progress.Json.ObjectModel.JsonObject")
        )
    );
    assert_eq!(
        def(&mut lsp, "oJson:Add(\"big\""),
        ("JsonObject.cls".into(), add_line("LOGICAL"))
    );
    assert_eq!(
        def(&mut lsp, "oJson:Add(\"label\""),
        ("JsonObject.cls".into(), add_line("CHARACTER"))
    );

    // Signature help highlights the overload the arguments fit.
    let s = lsp.at_with(
        "textDocument/signatureHelp",
        &file,
        pos(&text, "oJson:Add(\"n\", 5", 0, 15),
        json!({}),
    );
    let active = s["activeSignature"].as_u64().unwrap() as usize;
    assert!(
        s["signatures"][active]["label"]
            .as_str()
            .unwrap()
            .ends_with("value AS INTEGER)"),
        "{s}"
    );
}

#[test]
fn datasets_like_and_before_tables() {
    let root = samples();
    let file = root.join("src").join("datasets.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let df = std::fs::read_to_string(root.join("db").join("sports.df")).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());
    wait_index(&mut lsp, "Customer"); // the schema loads in the background

    let symbols = lsp.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": Url::from_file_path(&file).unwrap() } }),
    );
    let ds = symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "dsCust")
        .unwrap();
    assert_eq!(ds["detail"], "FOR ttCust, ttOrd");
    assert_eq!(ds["children"][0]["name"], "drOrders");
    assert_eq!(
        ds["children"][0]["detail"],
        "ttCust -> ttOrd (CustNum = CustNum)"
    );

    let def = |lsp: &mut Lsp, needle: &str, delta: usize| {
        def_target(&lsp.at_with(
            "textDocument/definition",
            &file,
            pos(&text, needle, 0, delta),
            json!({}),
        ))
    };
    assert_eq!(
        def(&mut lsp, "ttCust.City", 8),
        ("sports.df".into(), line_of(&df, "ADD FIELD \"City\""))
    );
    assert_eq!(
        def(&mut lsp, "bttOrd.OrderNum", 8),
        ("datasets.p".into(), line_of(&text, "FIELD OrderNum"))
    );
    assert_eq!(
        def(&mut lsp, "(CustNum", 2),
        (
            "sports.df".into(),
            line_of(&df, "ADD FIELD \"CustNum\" OF \"Customer\"")
        )
    );

    let hover = |lsp: &mut Lsp, needle: &str, delta: usize| {
        lsp.at_with(
            "textDocument/hover",
            &file,
            pos(&text, needle, 0, delta),
            json!({}),
        )
        .to_string()
    };
    assert!(hover(&mut lsp, "cCity = ", 2).contains("CHARACTER"));
    assert!(hover(&mut lsp, "ttOrd.OrderDate", 8).contains("DATE"));
    assert!(hover(&mut lsp, "WRITE-JSON", 2).contains("Writes the dataset as JSON"));
}

#[test]
fn formatting_reindents() {
    let root = samples();
    let file = root.join("src").join("unformatted.p");
    let uri = Url::from_file_path(&file).unwrap();
    let text = "DEFINE VARIABLE i AS INTEGER NO-UNDO.   \nDO i = 1 TO 2:\nIF i = 1 THEN\nDO:\nmessage i.\nEND.\n      END.\n";
    let mut lsp = Lsp::start(&root);
    lsp.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "abl", "version": 1, "text": text } }),
    );
    lsp.wait_diagnostics(&uri);
    let edits = lsp.request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }));
    let fixed = apply_inserts(text, edits.as_array().unwrap());
    assert_eq!(fixed, "DEFINE VARIABLE i AS INTEGER NO-UNDO.\nDO i = 1 TO 2:\n  IF i = 1 THEN\n  DO:\n    message i.\n  END.\nEND.\n");

    // Keyword case from the settings.
    lsp.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "abl": { "format": { "keywordCase": "upper" } } } }),
    );
    lsp.wait_diagnostics(&uri); // republished once the settings are applied
    let edits = lsp.request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }));
    let upper = apply_inserts(text, edits.as_array().unwrap());
    assert!(upper.contains("    MESSAGE i."), "{upper}");
}

#[test]
fn compile_on_save_with_openedge() {
    // A project with a fake OE install: saving runs the "compiler".
    let dir = std::env::temp_dir().join(format!("abl-lsp-compile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, dlc, root) = (dir.join("home"), dir.join("dlc"), dir.join("proj"));
    std::fs::create_dir_all(dlc.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let exe = if cfg!(windows) {
        "_progres.exe"
    } else {
        "_progres"
    };
    std::fs::copy(
        env!("CARGO_BIN_EXE_fake-progres"),
        dlc.join("bin").join(exe),
    )
    .unwrap();
    std::fs::write(dlc.join("version"), "OpenEdge Release 12.8.0 as of test\n").unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("[oe]\n\"12.8\" = '{}'\n", dlc.display()),
    )
    .unwrap();
    std::fs::write(
        root.join("abl.toml"),
        "oe = \"12.8\"\npropath = [\"src\"]\n",
    )
    .unwrap();
    let file = root.join("src").join("a.p");
    let text = "MESSAGE \"x\".\n/* fake-warning */\n";
    std::fs::write(&file, text).unwrap();
    let uri = Url::from_file_path(&file).unwrap();

    let mut lsp = Lsp::start_with_home(&root, &home);
    assert_eq!(lsp.open(&file), Vec::<Value>::new());
    lsp.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": uri } }),
    );
    let diags = lsp.wait_diagnostics(&uri);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0]["source"], "abl compiler");
    assert_eq!(diags[0]["message"], "fake warning");
    assert_eq!(diags[0]["severity"], 2);
    assert_eq!(diags[0]["range"]["start"]["line"], 1);

    // Editing drops the compiler's results: they describe the saved text.
    lsp.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": "MESSAGE \"y\".\n" }] }),
    );
    assert_eq!(lsp.wait_diagnostics(&uri), Vec::<Value>::new());

    // Turned off in the settings: saving compiles nothing.
    lsp.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "abl": { "compileOnSave": false } } }),
    );
    lsp.wait_diagnostics(&uri);
    lsp.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": uri } }),
    );
    // A request round trip: if a compile had started, its result would come later,
    // so check nothing is published within a short wait.
    lsp.request("workspace/symbol", json!({ "query": "zzz" }));
    std::thread::sleep(Duration::from_millis(1500));
    lsp.request("workspace/symbol", json!({ "query": "zzz" }));
    assert!(
        !lsp.pending
            .iter()
            .any(|m| m["method"] == "textDocument/publishDiagnostics"),
        "{:?}",
        lsp.pending
    );
    drop(lsp);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dotnet_types_from_stubs() {
    let root = samples();
    let file = root.join("src").join("dotnet-client.p");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut lsp = Lsp::start(&root);
    assert_eq!(lsp.open(&file), Vec::<Value>::new(), "no false diagnostics");

    let def = |lsp: &mut Lsp, needle: &str, delta: usize| {
        let d = lsp.at_with(
            "textDocument/definition",
            &file,
            pos(&text, needle, 0, delta),
            json!({}),
        );
        def_target(&d).0
    };
    // USING ... FROM ASSEMBLY, chains, statics, a USING .* namespace and a generic.
    assert_eq!(def(&mut lsp, "sb:Append", 4), "StringBuilder.cls");
    assert_eq!(def(&mut lsp, "):Append(System", 3), "StringBuilder.cls");
    assert_eq!(def(&mut lsp, "Environment:UserName", 13), "Environment.cls");
    assert_eq!(def(&mut lsp, "lst:Add", 5), "List.cls");
    assert_eq!(def(&mut lsp, "Path:Combine", 6), "Path.cls");
    assert_eq!(def(&mut lsp, "String:IsNullOrEmpty", 8), "String.cls");

    let hover = lsp.at_with(
        "textDocument/hover",
        &file,
        pos(&text, "sb:ToString", 0, 4),
        json!({}),
    );
    assert!(hover.to_string().contains("CHARACTER ToString"), "{hover}");

    let (l, c) = pos(&text, "sb:Append(\"Hello", 0, 3);
    let items = lsp.at_with("textDocument/completion", &file, (l, c), json!({}));
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    for expected in [
        "Append",
        "AppendLine",
        "Length",
        "ToString",
        "GetHashCode",
        "GetClass",
    ] {
        assert!(
            labels.contains(&expected),
            "missing {expected} in {labels:?}"
        );
    }
}
