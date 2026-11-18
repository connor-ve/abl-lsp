# Changelog

## 0.8.1 (unreleased)

- Fixed: `.cls` files opened as LaTeX when a LaTeX extension was installed (it claims `.cls` too), with no highlighting, errors or lint. ABL files that open as another language are now switched to ABL (in an abl.toml project always, elsewhere when a `.cls` reads as ABL), with an offer to save that in the workspace settings. The extension also starts in any folder with an abl.toml or ABL files.
- Fixed: `abl test --changed` found no tests when the project path was spelled differently from git's (a Windows 8.3 short name such as `RUNNER~1`, or a junction).
- Fixed: the language server failed to start in VS Code (`abl lsp` rejected the `--stdio` flag the client passes).
- Instant syntax colouring from a TextMate grammar, before the language server starts. It is generated from the server's keyword table.
- File icons: ABL source files get the procedure icon in themes without their own (icons for `.p`, `.cls`, `.w`, `.i`, `.t` and `.r` are in `media/` for themes that take custom icons).
- An icon for compiled `.r` files (the `abl-rcode` language; opt in with `files.associations`, since `.r` is R's by default).
- `abl pas create`, `start`, `stop` and `status`: a local PASOE instance for the project, its ABL application set up with the project's r-code, PROPATH and databases.
- `abl doctor --fix` creates missing PROPATH folders, adds unlisted `.df` files to abl.toml and registers a matching OpenEdge install.
- A package without its own abl.toml puts its `src` folder on the PROPATH when it has one.
- `abl status` shows where the PROPATH is set (file and line) and lists its folders, absolute and in search order, with installed packages and missing folders marked.

## 0.8.0

First Marketplace release (preview).

- Language server: go-to-definition, references, rename, hover, completion, signature help, outline, workspace symbols, folding, highlights and semantic highlighting for ABL procedures, includes and classes.
- Preprocessor evaluation, database schema from `.df` files, built-in functions, handles and classes (Progress.Lang, Progress.Json and common .NET types).
- Diagnostics, configurable lint rules with quick fixes, formatting with optional keyword case.
- Compile on save with an OpenEdge install.
- Database browser: databases, tables and sequences in the activity bar; table pages with fields, indexes, data, usages and related tables.
- Extra file extensions from `abl.toml` open as ABL.
- The bundled `abl` command adds compile against several OpenEdge versions, `.pl` builds, a REPL, a profiler report, `abl test` for ABLUnit, git packages (`abl install`), startup procedures, `abl dotnet` and `abl vulncheck`.

## 0.7.0

First release on GitHub: the language server and the VS Code extension.
