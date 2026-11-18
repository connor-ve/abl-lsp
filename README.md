# abl-lsp

[![CI](https://github.com/connor-ve/abl-lsp/actions/workflows/ci.yml/badge.svg)](https://github.com/connor-ve/abl-lsp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A language server for Progress OpenEdge ABL, written in Rust, with a VS Code extension.

ABL has very little modern editor tooling. abl-lsp gives it what other languages take for granted: go-to-definition across files, PROPATH and classes, hover docs, completion, rename, a formatter, and lint rules with quick fixes. It works from source alone and needs no OpenEdge install.

![Syntax errors in a broken ABL file, flagged as you type](docs/demo-broken.gif)

## Why this matters with coding agents

LLM coding agents are good at Java, Python and TypeScript because they have seen a huge amount of it. ABL is the opposite: a niche language with little public code, mostly in private enterprise codebases. Agents writing ABL often invent built-in functions, get handle attributes wrong, leave out `NO-UNDO`, and write `FIND` and `FOR EACH` without a lock, which takes SHARE-LOCK by default in production.

A language server gives an agent the ground truth it can't get from training data:

- **Real answers instead of guesses.** Definitions, references, hover and signature help come from the actual workspace, PROPATH, `.df` schema and class hierarchy, not from what the model remembers.
- **A fast feedback loop.** `abl lint` prints compiler-style diagnostics and lint findings without an OpenEdge install, so an agent can write code, check it, and fix it in seconds.
- **Safe refactoring.** Rename is resolved per scope and follows method overrides, so an agent can change a name across a codebase without find-and-replace mistakes.
- **House style enforced automatically.** Lint rules with quick fixes, plus `abl fmt` and `abl lint --fix`, bring agent-written code into line with what a reviewer expects.

Editors and agent tools increasingly speak LSP directly. A language that has a good server becomes one agents can work in safely. One without stays a guessing game.

## Install

**From a release:** download the `.vsix` from [Releases](https://github.com/connor-ve/abl-lsp/releases), then run:

```powershell
code --install-extension abl-lsp-0.7.0.vsix
```

**With winget** (once it is published; see Releasing below):

```powershell
winget install connor-ve.abl         # the abl command
abl i '@code'                        # the VS Code extension that ships with it (quoted: PowerShell)
```

**From source** (needs Rust and Node.js; Windows):

```powershell
.\build.ps1 -Install
```

Open any folder of `.p`, `.w`, `.i` or `.cls` files. The `samples` folder is a good place to start.

## Command line

The extension bundles `abl`, a CLI that also serves the language server (`abl lsp`).

```powershell
abl                                  # status of the project in this folder
abl init                             # write an abl.toml, guessing PROPATH and .df schemas
abl doctor                           # check the config and the OpenEdge install, say what to fix
abl env                              # DLC, PROPATH and friends, as abl hands them to OpenEdge
```

These commands work from source alone, without an OpenEdge install:

```powershell
abl lint                             # syntax errors and lint findings for the whole project
abl lint src\orders --fix            # apply the editor's quick fixes, report what's left
abl lint --format github             # annotations for GitHub Actions (also: --format json)
abl lint --max-warnings 0            # fail CI on warnings, not only errors
abl fmt                              # format the project in place
abl fmt --check                      # CI: exit code 1 if anything isn't formatted
abl fmt --upper                      # keyword case for this run: --upper, --lower or --preserve
abl deps src\order.p                 # what it includes, RUNs and uses (--all to expand everything)
abl deps -r src\inc\ttorder.i        # what depends on it, and what to recompile after changing it
abl explain 138                      # what an error means and what to do; paste the whole line if you like
abl schema diff old.df new.df --delta delta.df   # what changed, and the delta .df to load
abl usages customer                  # where the table is read, created, updated, deleted
abl usages customer.balance --writes # only where the field is assigned
```

These start OpenEdge (`_progres -b`) from the install the project's version points at:

```powershell
abl compile                          # compile what changed into `output` (default build/rcode)
abl compile --check                  # compile without saving r-code
abl compile --force -j 4             # everything, in 4 parallel sessions
abl compile --format github          # compiler errors as GitHub annotations (also: json)
abl compile --versions 11.7,12.8     # one compile per version, into <output>/11.7 and <output>/12.8
abl build                            # compile, then package the r-code into build/<project>.pl
abl build -o dist\app.pl             # somewhere else (or set `library` in abl.toml)
abl run src\report.p -- 2024 Q1      # run a procedure; arguments after -- become SESSION:PARAMETER
abl run                              # run `entry` from abl.toml
abl exec "MESSAGE TODAY."            # run a few statements
abl watch                            # compile what changed every time a file is saved
abl profile src
eport.p --top 10    # run with the profiler on: slowest lines and procedures
abl profile --save run.prof          # keep the profiler's data file too
abl repl                             # type ABL and see what it does, in one session
abl logs                             # last lines of the managed databases' .lg files
abl test                             # run the unit tests (@Test methods and procedures)
abl test test\orders --filter total  # some of them: files, folders, Suite:name text
abl test --changed                   # only tests affected by what changed in git
abl test --format junit > junit.xml  # for CI (also: json, github)
abl i github.com/someone/abl-json@v1.2   # add a package (a git repository) to vendor/ and the PROPATH
abl i                                # install what abl.toml and abl.lock list (after a fresh clone)
abl update [name]                    # move packages to the newest commit of their ref
abl remove abl-json                  # drop a package
abl i '@code'                        # install the VS Code extension shipped with abl
abl dotnet                           # assemblies.xml, and the .NET namespaces the code uses
abl dotnet add "Newtonsoft.Json"     # add an assembly (a full strong name for one in the GAC)
abl vulncheck                        # check the OE release, packages and code against your advisories
abl vulncheck --format github        # for CI (also: json); --fail-on high ignores lower findings
abl vulncheck init                   # write example advisories to ~/.abl/advisories
abl vulncheck fetch <git url>        # keep your advisories in a repository; fetch again to update
abl logs shop -f --grep error        # follow one database's log, only matching lines
```

`abl compile --versions` checks that every version is installed before it starts, and each version keeps its own record of what it compiled. `abl build` rebuilds the library from scratch with `prolib`, so r-code of deleted sources never lingers in it; it builds nothing when the compile fails.

`abl profile` turns on the `PROFILER` system handle around the RUN and reads the data file it writes. Time per line is the profiler's actual time (not counting what the line calls); line 0 of a procedure, its own overhead, counts toward the procedure but isn't listed as a line.

`abl repl` keeps one OpenEdge session up and runs each entry as a small procedure in it. An entry runs when its last statement ends with a period and every block it opens is closed. `DEFINE VARIABLE` and `DEFINE TEMP-TABLE` become `DEFINE NEW GLOBAL SHARED` and are repeated in front of later entries, so they keep their values; `= expr` shows a value. `:defs`, `:reset`, `:help` and `:quit` are the commands. The session talks to abl over a socket on 127.0.0.1.

`abl test` finds tests the way ABLUnit marks them: `@Test.` in front of a method of a class, or of an internal procedure of a `.p`. `@Ignore.` skips one, `@Before.` / `@After.` run once around a file's tests, `@Setup.` / `@TearDown.` around each, and `@Test (expected="Some.Error").` passes only when that error (or a subclass) is raised. All tests run in one session: a class is created once, a `.p` is run persistent. A raised `OpenEdge.Core.AssertionFailedError` (what `OpenEdge.Core.Assert` throws) is a failure, anything else an error, and the line shown is the test file's line in the call stack. What the session prints is shown when something fails, or always with `-v`. `--changed` takes the files git sees as modified, staged or untracked, and runs the tests that use them, directly or through RUN, includes, classes and so on.

A package is a git repository. `abl i <repo>` clones it into `vendor/<name>` (the name defaults to the repository's), adds it to `[deps]` in abl.toml, and records its commit in `abl.lock`. A repository is a URL, `github.com/owner/repo`, `git@host:path` or a local folder; `@ref` or `--ref` picks a tag, branch or commit, and without one the default branch is used. The package's own abl.toml decides what goes on the PROPATH (its `propath`, else its root), and its `[deps]` are installed next to it in `vendor/`; two packages asking for the same name differently is an error, fixed by naming one version in your own `[deps]`. `abl i` without arguments installs the locked commits, so commit `abl.lock` and put `vendor/` in `.gitignore`. `vendor/` is skipped by lint, fmt and the other source commands.

**.NET.** The editor knows the common .NET types (System.String, StringBuilder, Environment, DateTime, Math, Convert, Guid, Exception, System.IO's File, Directory and Path, the generic List and Dictionary, and the basic Windows Forms classes) from stubs written from the public .NET API, with the types mapped the way OpenEdge maps them (String is CHARACTER, Int32 is INTEGER, DateTime is DATETIME). Hover, completion, signature help and go-to-definition work on them, through `USING ... FROM ASSEMBLY` too; other .NET types are left alone, without errors. `abl dotnet` lists assemblies.xml next to the .NET namespaces the code uses and says which ones need an assembly there; `abl doctor` warns about the same. When assemblies.xml exists (in the root, or the folder `assemblies` names in abl.toml), every session gets `-assemblies` with its folder.

**Advisories.** `abl vulncheck` reads advisories you write and keep: TOML files with `[[advisory]]` entries (id, title, severity, optional description, fix and url) and one rule each. An `[advisory.oe]` rule lists affected OpenEdge releases (`11.7.0 - 11.7.15`, `< 12.2.12`, or `12.2` for every 12.2.x), checked against the install's version file. An `[advisory.package]` rule names a package by `name` or `git` with affected versions (read from the tag it is installed at) or `commits`. An `[advisory.code]` rule matches statements by their first keyword(s) and text they must or must not contain, ignoring case and spaces; code in `vendor/` is checked too. A `vulncheck-ignore <id>` comment on a statement's line or the line above silences one finding, and `[vulncheck] ignore` in abl.toml silences an advisory everywhere. `abl vulncheck init` writes a file of example code rules, which are generic patterns, not published vulnerabilities. The exit code is 1 when there are findings at or above `--fail-on` (default low).

`abl compile` remembers what it compiled in `<output>/.abl-build`. A file is recompiled when it, or anything it includes, inherits, implements or uses, changes; when its r-code is missing; or when the OpenEdge version, PROPATH, startup parameters or schema change. Files that fail are not recorded, so they are tried again next time. `abl` (status) shows how many files need compiling.

The editor also compiles a `.p`, `.w`, `.cls` or `.t` file when you save it (without saving r-code) and shows the compiler's messages next to the parser's, marked "abl compiler". Messages about an include file appear on the line that includes it. Editing the file clears them until the next save. Turn it off with `"abl.compileOnSave": false`; without a configured OpenEdge install it does nothing.

`abl usages` resolves names the way go-to-definition does, so a buffer (`DEFINE BUFFER bCust FOR Customer`) and an unqualified field inside `FOR EACH Customer` count as uses of Customer. A table's list shows the record phrases, CREATE, DELETE, field assignments (named in brackets) and the buffers and LIKE definitions on it; a field's list shows its reads too. Reads with EXCLUSIVE-LOCK are marked.

Databases:

```powershell
abl db                               # the databases in abl.toml and whether they run
abl db create                        # prodb, then load the .df from `schema`
abl db start / abl db stop           # proserve / proshut
abl db snapshot before-test          # probkup into .abl/snapshots/<db>/ (--list to see them)
abl db restore before-test           # prorest it back (the database must be stopped)
abl db delete --yes                  # prodel
abl db schema                        # tables, fields and indexes from the .df files (--json for tools)
abl query "customer WHERE balance > 1000 BY name"   # a table; also --format json|csv, --fields, --max
abl export customer --where "balance > 0" -o cust.csv   # JSON or CSV, from the extension
abl import customer cust.csv         # all or nothing; --replace deletes the rows first
```

**Database browser.** The ABL view in VS Code's activity bar lists the databases in each abl.toml with their tables and sequences, read from the `.df` files, so it works without OpenEdge. Ctrl+Alt+D (or the search button) finds a table by name, label or description. A table opens as a page with tabs:
- **Fields**: type, keys (primary, unique, indexed), mandatory, format, label and initial value, with a filter. A field name jumps to its definition in the .df.
- **Indexes**: their fields in order, ascending or descending, and whether they are primary, unique or word indexes.
- **Data**: rows from a `WHERE ... BY ...` you type, copied as CSV or JSON. This one runs `abl query`, so it needs OpenEdge and the database.
- **Used in**: every place the code reads, creates, updates or deletes the table or one field (`abl usages`), grouped by file, with each line opening the code.
- **Related**: the tables this one points at, and the ones that point at it, matched by field name. A table that has all the fields of another's unique primary index refers to it.

Managed databases also get create, start and stop buttons in the tree.

A `[db.x]` entry without `connect` is managed by abl: it lives in `.abl/db/<name>/` unless `path` says otherwise, and every session connects to it once it exists, through its server when one runs (`abl db start`), single-user otherwise. Single-user takes one session at a time, so `abl compile` then runs one session. An entry with `connect` is connected as written and never touched by `abl db`. Add `.abl/` to the project's `.gitignore`: it holds the databases and snapshots.

Startup procedures run in every session abl starts for compiling, running, testing, profiling and the REPL, but not in the data commands (`abl db`, `query`, `export`, `import`), which may run before the schema exists. When one fails or isn't found, the session stops and abl names the procedure and the error; `abl doctor` checks that each one is on the PROPATH, and `abl env` lists them. The extra `[extensions]` count everywhere: compile, lint, fmt, deps, usages and the editor. VS Code asks once whether to open them as ABL (through `files.associations` in the workspace settings).

`abl query` takes a table and an ABL WHERE / BY phrase (`FOR EACH` and a lock in front are fine) and reads NO-LOCK. JSON numbers, logicals and unknown values (`null`) keep their types; in CSV the unknown value is `?` (a quoted `"?"` is the text), and an extent field is one column per element (`Phone[1]`, `Phone[2]`). `abl import` creates every row in one transaction: one bad row (a duplicate key, a value that doesn't convert) and nothing is kept, and the error names the row.

The ABL programs abl runs inside OpenEdge are in `crates/oe/src/driver.rs` and `crates/oe/drivers/`. They are written for OpenEdge 11.7 and later, and have not yet been tried against a real install. The tests use a stand-in OpenEdge (`crates/cli/src/bin/fake-progres/`): its `_progres` "compiles" with abl's own parser, "runs" `MESSAGE` statements, and keeps databases as JSON files it can query with a subset of WHERE; copies of it play `prodb`, `proserve` and the other tools. `abl dev fake-dlc <dir>` sets one up for trying the commands without OpenEdge.

Without an `abl.toml`, `lint`, `fmt` and `deps` read `abl.propath`, `abl.lint`, `abl.preprocessor`, `abl.schema` and `abl.format.keywordCase` from `.vscode/settings.json`, so they see the project the way the editor does. An include file is checked the way the project uses it, with the arguments of the first file that includes it. Files keep their encoding when `fmt` or `--fix` rewrites them.

`abl explain` knows 23 common messages. Its table (`crates/core/builtin/messages.txt`) was written by hand from the OpenEdge documentation; the message texts are the typical form, and wording varies between releases. `abl schema diff` compares full dumps. Type and extent changes, and changed indexes, are dropped and re-added, and it warns where that loses data, where a drop and add looks like a rename, and where Progress won't drop a primary index. The delta has no PSC trailer, so it loads with the session's code page.

Configuration has two layers. `~/.abl/config.toml` (or `$ABL_HOME/config.toml`) says which OpenEdge installs this machine has. A project's `abl.toml` names the version it uses, never a path, so the same file works on every machine.

```powershell
abl oe add 12.8 C:\Progress\OpenEdge128   # register an install (abl oe add --detect finds them)
abl oe use 12.8                           # global default
abl oe use 11.7 --project                 # pin this project's version in abl.toml
abl oe list                               # * marks the version in use here, and why
```

The version is picked from `--oe`, then `ABL_OE`, then `abl.toml`, then the global default. A version that isn't installed is an error. abl never falls back to another version, because r-code compiled with the wrong one breaks at runtime.

```toml
# abl.toml
oe = "11.7"
propath = ["src", "src/include"]
output = "build/rcode"
entry = "src/main.p"
library = "dist/app.pl"       # what abl build writes (default build/<folder name>.pl)

exclude = ["legacy"]          # folders lint, fmt and deps skip (output and vendor/ are skipped too)

assemblies = "dotnet"         # the folder with assemblies.xml (default: the root)

[[procedures]]                # run before every compile, run, test, profile and REPL session, in order
path = "startup/init.p"       # as given to RUN: on the PROPATH or a full path
                              # mode = "run" (default): RUN it, and go on when it returns

[[procedures]]
path = "lib/helpers.p"
mode = "persistent"           # RUN PERSISTENT: its internal procedures stay callable

[[procedures]]
path = "lib/session-super.p"
mode = "super"                # RUN PERSISTENT, then SESSION:ADD-SUPER-PROCEDURE

[extensions]                  # source besides .p .w .cls .t (compiled) and .i (included)
main = ["x"]                  # compiled like .p
include = ["f", "v"]          # included like .i

[vulncheck]
db = "security/advisories"    # advisory file or folder (default: ~/.abl/advisories)
ignore = ["EXAMPLE-CODE-4"]   # advisories that don't apply here

[deps]                        # packages abl install keeps in vendor/
abl-json = { git = "https://github.com/someone/abl-json", ref = "v1.2" }

[db.sports]                   # managed by abl db: created from schema, served on port
schema = "db/sports.df"
port = 20000                  # optional; path = "data/sports" puts it elsewhere

[db.legacy]                   # someone else's server: connected as written
connect = "-db legacy -H dbhost -S 20001"

[lint]                        # rule levels, as in the abl.lint editor setting
keyword-abbreviation = "off"
for-each-without-lock = "error"

[format]
keyword-case = "upper"        # preserve (default), upper or lower; a [format] keyword-case in ~/.abl/config.toml is your default for projects that set none
indent = 4

[preprocessor]                # names defined before every file
MY-COMPILE-FLAG = "yes"
```

## Features

![Unused variable and parameter warnings](docs/demo-unused.gif)

| Feature | What it does |
|---|---|
| Outline | Procedures, functions, classes, methods, properties, temp-tables (with fields and indexes), buffers, variables, parameters, `ON` triggers, `&GLOBAL/SCOPED-DEFINE` |
| Go to definition | Variables and parameters by scope · internal procedures (`RUN x`) · external procedures (`RUN dir/x.p`, `.r` maps to `.p`/`.w`) · `{include.i}` · `{&NAME}` · `table.field` and `buffer.field` (also when the table is defined in an include) · classes through `USING` · `obj:Member`, `THIS-OBJECT:`, `SUPER:`, inherited members, enum members |
| Hover | Declaration text, kind, and file:line when it lives elsewhere. `LIKE` definitions show the type they resolve to. Keywords show their full name when abbreviated (`DEF` → DEFINE). `{&NAME}` shows the value it expands to |
| Completion | After `tt.` / `buffer.`: fields. After `obj:`: class members, including inherited ones. After a handle (`SESSION:`, `hBuf:`, `BUFFER x:`, `hQuery:GET-BUFFER-HANDLE(1):`): its attributes and methods. After `RUN`: procedures. Otherwise: visible symbols, built-in functions and keywords, matching your case |
| Diagnostics | Unterminated strings, comments and `{`; unbalanced `( )` / `[ ]`; missing `END`; `END PROCEDURE` closing the wrong block; stray `END`; missing final period; duplicate definitions; `DEFINE VARIABLE` without `AS`/`LIKE`; unbalanced `&IF`/`&ENDIF`; include files missing from the PROPATH |
| Lint rules | Configurable per rule (`off`/`hint`/`info`/`warning`/`error`): `unused-parameter`, `unused-variable`, `unused-buffer` (faded like Go's unused names), `missing-no-undo`, `find-without-no-error`, `for-each-without-lock` and `find-without-lock` (database tables only, not temp-tables), `keyword-abbreviation` (hint by default). Suppress with `/* lint-ignore [rule] */` on the line or the line above |
| Quick fixes | Ctrl+. on a finding: **Add NO-UNDO**, **Add NO-ERROR**, **Add NO-LOCK**, **Expand to DEFINE**, **Ignore *rule* on this line**, **Fix all ABL lint issues**. The last also runs on save with `"editor.codeActionsOnSave": { "source.fixAll": "explicit" }` |
| Highlighting | Semantic tokens: keywords (including abbreviations), types, classes, procedures, methods, properties, variables, parameters, preprocessor. `&IF` branches that are not taken are dimmed like comments |
| Preprocessor | `&GLOBAL-DEFINE` / `&SCOPED-DEFINE` / `&UNDEFINE` are tracked, including global names defined in include files. `&IF` / `&ELSEIF` conditions are evaluated (`DEFINED()`, `{&OPSYS}`, `PROVERSION`, comparisons, `AND`/`OR`/`NOT`, `BEGINS`, `MATCHES`, string functions); a condition it cannot evaluate takes the first branch. `{&NAME}` is replaced by its value, so `DEFINE VARIABLE {&VAR} AS {&TYPE}` defines a real variable. Include files are read with their arguments (`{inc/tt.i ttOrder &key=OrderNum}` fills `{1}` and `{&key}`), so tables and fields an include defines from its arguments resolve. Extra names: the `abl.preprocessor` setting |
| Built-in functions and handles | Hover docs and signature help for about 100 built-in functions (`SUBSTRING`, `ENTRY`, `LOOKUP`, `ADD-INTERVAL`...), the system handles (`SESSION`, `ERROR-STATUS`, `THIS-PROCEDURE`, `FILE-INFO`, `COMPILER`, `LOG-MANAGER`...) and the attributes and methods of buffer, buffer-field, query, temp-table, dataset, data-relation, data-source, widget, X-document and server handles. A HANDLE variable takes its kind from `CREATE QUERY h`, `h = BUFFER x:HANDLE` or `h = hBuf:BUFFER-FIELD(...)`; otherwise it offers the common kinds together. The table is `crates/core/builtin/abl-reference.txt` |
| Datasets | `DEFINE DATASET` shows its buffers and data-relations (with their relation fields) in the outline and hover. `DEFINE TEMP-TABLE x ... LIKE table` takes that table's fields wherever `LIKE` appears among the options; `LIKE-SEQUENTIAL` too. `BEFORE-TABLE bttX` is a table with the same fields. `DATASET ds:` / `TEMP-TABLE tt:` / `BUFFER b:` complete their handle attributes |
| Formatting | Format Document / Format Selection: indents one level per block (`END` lines up with its header), keeps the offset of continuation lines, lines `DO:` up with `IF ... THEN` / `ON ...` on the line above, indents property `GET`/`SET`, and trims trailing white space. Preprocessor lines, inactive branches and the inside of strings are left alone. Optional keyword case with `abl.format.keywordCase`. Also `abl fmt` on the command line |
| Workspace symbols | Ctrl+T across every `.p .w .i .cls` in the workspace, plus database tables |
| Database schema | `.df` dumps give database tables and fields go-to-definition (into the .df), hover (type, format, label, description) and completion (`customer.`, `buffer.`, `dbname.`). Works with `db.table.field` and buffers `FOR` db tables. Unqualified field names (`FOR EACH Order WHERE CustNum = 1`) resolve to the table of the record phrase they are in, then to enclosing `FOR EACH` blocks; completion there lists those tables' fields first |
| References / rename | Shift+F12 and F2 across the workspace. Each occurrence is resolved, so same-named variables in other scopes are left alone. Renaming a method renames the methods it overrides and its overrides in subclasses. Classes, schema items and built-ins are refused |
| Highlights | Other occurrences of the symbol under the cursor |
| Classes | Chains (`oSvc:GetOrder():Customer:Name`), static access (`Square:Make()`), `CAST(x, T):`, `NEW T():`, `SUPER:`, `THIS-OBJECT:`. Overloads resolve by the types of the arguments (literals, variables, fields, `LIKE`, method and built-in function results, `NEW T()`, comparisons), with INTEGER → INT64 → DECIMAL widening and subclasses accepted for a class parameter; by count when a type is unknown. Every class inherits `Progress.Lang.Object`, and `Progress.Lang.*` is always in scope |
| Built-in classes | Stubs for `Progress.Lang.*` (Object, Class, Error, ProError, AppError, SysError, Stop, StopError) and `Progress.Json.ObjectModel.*` (JsonObject, JsonArray, JsonConstruct, ObjectModelParser, JsonDataType) give completion, hover, signature help and F12 into the stub. Stubs live in `crates/core/builtin/` |
| Class contracts | Errors for a concrete class missing an interface or `ABSTRACT` member (`missing-implementation`), a method overriding a superclass method without `OVERRIDE` (`missing-override`, quick fix **Add OVERRIDE**), and `OVERRIDE` with nothing to override (`override-without-base`). Skipped when a superclass or interface cannot be found |
| Signature help | Parameter hints for functions, methods, built-in functions, handle methods, `NEW Class(`, `RUN proc (` and `RUN file.p (`. Overloaded methods and constructors list every signature and highlight the one the arguments fit |
| Folding | Blocks (END stays visible), multi-line comments, temp-table definitions |

## Settings

`build.ps1` builds the `abl` CLI in release mode, bundles `abl.exe` into the extension, and packages the `.vsix`. Add `-Install` to install it too, then reload VS Code.

Put these in your project's `.vscode/settings.json`:

```jsonc
{
  "abl.propath": ["src", "src/include", "C:/Progress/OpenEdge/src"],  // or "src;src/include"
  "abl.schema": ["db/sports2000.df"],   // default: every .df in the workspace
  "abl.diagnostics.unresolvedIncludes": true,
  "abl.preprocessor": { "MY-COMPILE-FLAG": "yes" },  // names defined before every file
  "abl.format.keywordCase": "preserve",            // or "upper" / "lower"
  "abl.compileOnSave": true,             // check saved files with the OpenEdge compiler
  "abl.lint": {                          // defaults shown
    "unused-parameter": "warning",
    "unused-variable": "warning",
    "missing-no-undo": "warning",
    "find-without-no-error": "warning",
    "unused-buffer": "warning",
    "for-each-without-lock": "warning",
    "find-without-lock": "warning",
    "keyword-abbreviation": "hint",
    "missing-implementation": "error",
    "missing-override": "error",
    "override-without-base": "error"
  }
}
```

To add a lint rule:
1. Add it to `RULES` in `crates/core/src/lint.rs` with its default level.
2. Write a `check_*` function that calls `Emitter::push`, passing a `Fix` if the rule has a quick fix, and call it from `lint::run`.
3. List the rule under `abl.lint` in `client/package.json`.

`ana.statements` gives every statement with its tokens, and `ana.symbols` every definition.

The logical database name comes from the file name: `sports2000.df` makes `sports2000.customer` work. Dump a schema with Data Administration → Admin → Dump Data and Definitions → Data Definitions (.df file).

The command palette has **ABL: Restart Language Server**.

## Development loop

```powershell
cargo test                                # unit tests, CLI tests, end-to-end LSP tests against samples/
cargo run -- check samples\src\broken.p   # print diagnostics for files, compiler style
```

To try a server change without repackaging, point `abl.serverPath` at `target/debug/abl.exe`, run `cargo build`, and then run **ABL: Restart Language Server**.

## Releasing

`release.ps1` builds everything and writes `dist\abl-<version>-windows-x64.zip` (abl.exe, the .vsix and the license) and the three winget manifests in `dist\winget\<version>\`, then runs `winget validate` on them. Nothing is uploaded.

```powershell
.\release.ps1                    # InstallerUrl = the GitHub release download
.\release.ps1 -Local             # InstallerUrl = http://localhost:8765; serves the zip and installs it with winget
.\release.ps1 -Local -Uninstall  # serves it again and uninstalls
```

`-Local` needs local manifests allowed once, from an administrator prompt: `winget settings --enable LocalManifestFiles`. To publish: run `.\release.ps1` without `-Local`, attach the zip to a GitHub release tagged `v<version>`, and open a pull request to `microsoft/winget-pkgs` adding the manifests under `manifests\c\connor-ve\abl\<version>\`.

**VS Code Marketplace.** The listing is `client/README.md`, with `client/CHANGELOG.md`, the icon in `client/media/icon.png` and the screenshots in `client/media/screenshots/` (the page loads them from GitHub, so push them first). Once: create the `connor-ve` publisher at marketplace.visualstudio.com/manage, make an Azure DevOps personal access token with the Marketplace (Manage) scope, and run `npx vsce login connor-ve` in `client`. To publish, after `.\build.ps1`, run `npm run publish:marketplace` in `client`. It publishes a Windows x64 build, since the bundled `abl.exe` only runs there. Bump `version` in `client/package.json` (and the crates) for each release.

## Code map

The repository is a Cargo workspace:

| Crate | What it is |
|---|---|
| `crates/core` (`abl-core`) | All language analysis, listed below. No LSP runtime, no I/O beyond reading source files |
| `crates/oe` (`abl-oe`) | Talking to OpenEdge: `config.rs` (global config and `abl.toml`, version precedence), `session.rs` (environment, database connections, starting `_progres`, job and result files), `driver.rs` and `drivers/` (the ABL programs run inside the session), `db.rs` (the database tools, query and import, the data file format) |
| `crates/lsp` (`abl-lsp`) | LSP wiring (tower-lsp): `serve()` maps requests to `abl-core` features; `compile.rs` is compile on save |
| `crates/cli` (`abl-cli`) | The `abl` binary: `oe.rs` (`abl oe`), `project.rs` (`status`, `env`, `doctor`, `init`), `source.rs` (project setup shared by the source commands), `lint.rs`, `fmt.rs`, `deps.rs`, `explain.rs`, `schema.rs`, `compile.rs` (incremental compile, `--versions`), `build.rs` (`.pl` via prolib), `run.rs` (`run`, `exec`, `watch`), `profile.rs`, `repl.rs`, `logs.rs`, `test.rs`, `install.rs` (packages, `@code`), `dotnet.rs`, `vulncheck.rs`, `usages.rs`, `db.rs` (`abl db`), `dbschema.rs` (`abl db schema`), `data.rs` (`query`, `export`, `import`, JSON and CSV); `bin/fake-progres/` is the test stand-in for OpenEdge and its database tools |
| `client` | The VS Code extension: `extension.js` starts the server and opens `[extensions]` files as ABL; `dbview.js` is the database browser (tree, table pages) on top of `abl db schema`, `abl query --format page` and `abl usages --json` |

`crates/core/src`:

| File | Responsibility | Change it when... |
|---|---|---|
| `lexer.rs` | Tokens: words (with `-` and `.`), strings, nested comments, `{includes}`, `&directives`, statement `.` vs member `:`. Also the preprocessor: defined names, `&IF` evaluation, `{&NAME}` / `{1}` expansion, include arguments | a construct is tokenized wrong, or a preprocessor condition evaluates wrong |
| `keywords.rs` | Keyword and type table; `DEF\|INE` marks the minimum abbreviation | a keyword is missing, or an abbreviation is wrong |
| `analysis.rs` | Splits code into statements, tracks blocks, builds symbols, scopes and diagnostics | adding a new DEFINE form or block type, or fixing a false diagnostic |
| `workspace.rs` | Open documents, disk cache, PROPATH, include/RUN/class resolution, background index | resolution rules (PROPATH, USING) need changing |
| `oo.rs` | Expression types (chains, CAST, NEW), overloads, inheritance with the implicit Progress.Lang.Object, override families | a class construct resolves wrong |
| `builtins.rs` | Writes the built-in class stubs from `crates/core/builtin/` (Progress.* and the common .NET System.* types) to a cache folder | adding a built-in or .NET class (also add it to the list) |
| `langref.rs` | Reads `crates/core/builtin/abl-reference.txt`: built-in functions, system handles, handle attributes and methods | (edit the .txt) a built-in is missing or documented wrong |
| `handles.rs` | Handle kind of an expression (`SESSION`, `BUFFER x`, `hBuf:BUFFER-FIELD()`, HANDLE variables by their CREATE / assignment) | handle completion picks the wrong kind |
| `format.rs` | Document formatting | an indentation rule is wrong |
| `lint.rs` | Lint rules, levels, `lint-ignore` comments | adding a rule or an exception |
| `schema.rs` | `.df` parser that turns tables, fields and indexes into symbols | a schema attribute is missing from hover |
| `df.rs` | Lossless .df model (every attribute line kept), the diff between two schemas, and the delta .df writer | `abl schema diff` gets a change wrong |
| `deps.rs` | Include, RUN and class edges of a file; the project graph, dependents and the recompile set | a dependency is missed or `deps -r` reports the wrong files |
| `unittest.rs` | Finds `@Test` methods and procedures and the other ABLUnit annotations, for `abl test` | a test or lifecycle routine is missed |
| `usage.rs` | Where a database table or field is read, created, updated or deleted, for `abl usages` | a use is missed or classified wrong |
| `messages.rs` | Reads `crates/core/builtin/messages.txt` for `abl explain` | (edit the .txt) a message is missing or explained wrong |
| `features.rs` | Definition, hover, completion, outline, references/rename, signature help, folding, semantic tokens, LSP diagnostics | changing how a feature behaves |

Add a new LSP capability in `crates/lsp/src/lib.rs`, and a new command in `crates/cli/src/main.rs`.

The parser works at statement level and deliberately doesn't parse expressions. It doesn't need to understand everything, so unknown syntax is skipped quietly instead of causing cascading errors.

## Known gaps / next steps

- **Packages in the editor**: the language server takes its PROPATH from the `abl.propath` setting, not abl.toml, so code in `vendor/` resolves for the CLI and OpenEdge but not yet for go-to-definition. Add the vendor folders to `abl.propath` meanwhile.
- **winget on machines without symlinks** (no Developer Mode): winget puts the package folder itself on the user PATH, and `winget uninstall` leaves that PATH entry behind. It is harmless (the folder is gone) and can be deleted by hand.
- **.NET is matched by name, not by reading assemblies**: the stubs cover common members of common types only, a generic's type parameter stays `T` (so `oList:Item` has no type), and which namespaces OpenEdge loads without assemblies.xml (mscorlib, System, System.Drawing, System.Windows.Forms and Progress' own) is from memory and needs a check on a real install. `abl dotnet` decides that an assembly covers a namespace when their names start the same way.
- **`abl vulncheck` knows only what your advisories say**: abl ships no vulnerability data. Code rules are text matches within one statement, so they can't follow a value from where it is read to where it is used.
- **Planned**: publishing the winget package (ready; see Releasing).
- **The OpenEdge drivers are untested against real OpenEdge**: `COMPILER:GET-ROW` and friends, the result file format and exit codes need a check on 11.7 and 12.x. For the data commands: how `prodict/load_df.p` reports errors, `DATETIME()` reading ISO text on import, the `proutil -C holder` exit codes, and the answers `prodel` and `prorest` expect on standard input. For stage 5: the `prolib` arguments, the profiler's output format across versions (the parser reads only the first four sections), and the REPL driver's socket reads (`READ-EXACT-NUM`, and whether `RUN` of a new file each time leaks memory in a long session). For `abl test`: `SESSION:ERROR-STACK-TRACE`, the `CallStack` format the failure line is read from, `IsA` with a type name, and `DYNAMIC-INVOKE` of VOID methods as a statement. Global shared variables also can't be redefined with another type in the same session, so `:reset` is the way out.
- **`abl usages`** finds names in the code; it can't see dynamic access (`BUFFER-FIELD("Name")`, `CREATE BUFFER ... FOR TABLE cName`) or field writes through `BUFFER-COPY` and `ASSIGN` of a whole buffer.
- **Includes are read, not inlined**: their preprocessor names and their definitions (with the include arguments applied) are known, but blocks that an include opens or closes are not seen, and code *inside* an include that uses variables of the file including it doesn't resolve, so rename doesn't update it. An include opened on its own doesn't know its arguments, so `{1}` stays as written there.
- **Preprocessor values are expanded as tokens**: a `{&NAME}` that holds half a string or joins two words into one identifier is not reproduced exactly.
- **Argument types** come from simple expressions; arithmetic other than string concatenation leaves the type unknown, and then the first overload with the right count wins.
- **Built-in reference** (`abl-reference.txt`) was written from the OpenEdge documentation, not generated from it. It has the common functions and handle members, not all of them; functions with several forms (`DATE`, `DATETIME`) show one signature.
- **Startup procedures on real OpenEdge**: whether `RUN ... PERSISTENT` procedures started from the generated `abl_startup.p` stay alive for the driver (they should: persistent procedures outlive their caller), and how a STOP in one is reported, need a check on a real install.
- **The database browser reads the schema from `.df` files**, not from a running database, so a database changed by hand (without a new .df) shows its old schema. Related tables are a guess from field names.
- **Formatting** only changes indentation, trailing white space and (optionally) keyword case. It does not wrap or align.
- **Built-in classes** cover Progress.Lang and Progress.Json only. OpenEdge.* (Net, Core, ...) resolve when their sources are on the PROPATH.

## License

MIT. See [LICENSE](LICENSE).

## Disclaimer

abl-lsp is an independent open-source project. It is not affiliated with, endorsed by, or sponsored by Progress Software Corporation. Progress, OpenEdge and ABL are trademarks or registered trademarks of Progress Software Corporation or its subsidiaries in the U.S. and other countries. All other trademarks belong to their respective owners.
