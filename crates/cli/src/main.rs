//! `abl`: command-line tooling for OpenEdge ABL projects.
//!
//! Run with no command inside a project to see its status.

use abl_oe::config;
mod build;
mod compile;
mod data;
mod db;
mod dbschema;
mod deps;
mod dotnet;
mod explain;
mod fmt;
mod install;
mod lint;
mod logs;
mod oe;
mod profile;
mod project;
mod repl;
mod run;
mod schema;
mod source;
mod test;
mod usages;
mod vulncheck;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "abl",
    version,
    about = "Tooling for OpenEdge ABL projects",
    after_help = "With no command, shows the status of the project in the current folder."
)]
struct Cli {
    /// OpenEdge version to use for this command. Overrides ABL_OE, abl.toml and the global default.
    #[arg(long, global = true, value_name = "VERSION")]
    oe: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show the project's configuration (the default command)
    Status,
    /// Run the language server on stdin/stdout (used by the VS Code extension)
    Lsp,
    /// Create an abl.toml in the current folder
    Init {
        /// Overwrite an existing abl.toml
        #[arg(long)]
        force: bool,
    },
    /// Print the environment abl gives to OpenEdge (DLC, PROPATH, ...)
    Env,
    /// Check the configuration and installs, and say what to fix
    Doctor,
    /// Manage the OpenEdge installs this machine knows about
    Oe {
        #[command(subcommand)]
        command: OeCommand,
    },
    /// Compile with OpenEdge: only what changed, unless --force
    Compile {
        /// Files or folders (default: the whole project)
        paths: Vec<PathBuf>,
        /// Compile without saving r-code (and without recording anything)
        #[arg(long)]
        check: bool,
        /// Recompile everything, even files that are up to date
        #[arg(long)]
        force: bool,
        /// Parallel OpenEdge sessions (default: number of CPUs, at most 4)
        #[arg(long, short)]
        jobs: Option<usize>,
        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: lint::Output,
        /// Compile with each of these OpenEdge versions, into <output>/<version>
        #[arg(
            long,
            value_delimiter = ',',
            value_name = "VERSIONS",
            conflicts_with = "check"
        )]
        versions: Vec<String>,
    },
    /// Compile, then package the r-code into a procedure library (.pl)
    Build {
        /// The library to write (default: `library` in abl.toml, else build/<project>.pl)
        #[arg(long, short)]
        output: Option<PathBuf>,
        #[arg(long, short)]
        jobs: Option<usize>,
    },
    /// Run a procedure under the profiler and show where the time goes
    Profile {
        file: Option<PathBuf>,
        /// Lines to show
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Keep the profiler's output file here
        #[arg(long, value_name = "FILE")]
        save: Option<PathBuf>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Show database log files (.lg)
    Logs {
        /// Which database (default: all of them)
        db: Option<String>,
        /// Lines from the end of each log
        #[arg(long, short = 'n', default_value_t = 20)]
        lines: usize,
        /// Only lines containing this text (any case)
        #[arg(long)]
        grep: Option<String>,
        /// Keep showing new lines as they are written
        #[arg(long, short)]
        follow: bool,
    },
    /// An interactive ABL session: type statements, see what they do
    Repl,
    /// Check the project against your advisories: OE release, packages, code patterns
    Vulncheck {
        #[command(subcommand)]
        command: Option<VulncheckCommand>,
        /// An advisory file or folder (default: [vulncheck] db in abl.toml, else ~/.abl/advisories)
        #[arg(long)]
        db: Option<PathBuf>,
        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: vulncheck::Format,
        /// Exit code 1 only for findings at least this severe
        #[arg(long, value_enum, default_value = "low")]
        fail_on: vulncheck::Severity,
    },
    /// .NET: the assemblies in assemblies.xml and the namespaces the code uses
    Dotnet {
        #[command(subcommand)]
        command: Option<DotnetCommand>,
    },
    /// Install packages (git repositories) into vendor/ and put them on the PROPATH
    #[command(visible_alias = "i")]
    Install {
        /// A repository (URL, github.com/owner/repo, or a folder), optionally
        /// with @ref; `@code` installs the VS Code extension. Nothing: install
        /// what abl.toml and abl.lock list
        spec: Option<String>,
        /// The package name (default: the repository's name)
        #[arg(long)]
        name: Option<String>,
        /// A tag, branch or commit (default: the default branch)
        #[arg(long = "ref", value_name = "REF")]
        reference: Option<String>,
        /// For `@code`: the .vsix to install (default: the one next to abl)
        #[arg(long)]
        vsix: Option<PathBuf>,
    },
    /// Move packages to the newest commit of their ref, and rewrite abl.lock
    Update {
        /// One package (default: all)
        name: Option<String>,
    },
    /// Remove a package from abl.toml, abl.lock and vendor/
    Remove { name: String },
    /// Run unit tests (@Test methods and procedures, ABLUnit style)
    Test {
        /// Test files or folders (default: the whole project)
        paths: Vec<PathBuf>,
        /// Only tests whose `Suite:name` contains this (case-insensitive)
        #[arg(long)]
        filter: Option<String>,
        /// Only tests affected by files changed in git (modified, staged, untracked)
        #[arg(long)]
        changed: bool,
        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: test::Format,
        /// Show the session's output even when every test passes
        #[arg(long, short)]
        verbose: bool,
    },
    /// Run a procedure (default: `entry` in abl.toml); arguments after -- become -param
    Run {
        file: Option<PathBuf>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run a few ABL statements: abl exec "MESSAGE TODAY."
    Exec {
        #[arg(required = true)]
        code: Vec<String>,
    },
    /// Compile what changed every time a file is saved
    Watch {
        #[arg(long, short)]
        jobs: Option<usize>,
        /// Stop after this many compiles (for tests)
        #[arg(long, hide = true)]
        cycles: Option<usize>,
    },
    /// Development helpers
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
    /// Report syntax errors and lint findings (exit code 1 on errors)
    Lint {
        /// Files or folders (default: the whole project)
        paths: Vec<PathBuf>,
        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: lint::Output,
        /// Also fail when there are more warnings than this
        #[arg(long, value_name = "N")]
        max_warnings: Option<usize>,
        /// Apply the quick fixes the editor offers, then report what's left
        #[arg(long)]
        fix: bool,
        /// Also show hints (such as keyword abbreviations)
        #[arg(long)]
        hints: bool,
    },
    /// Format files in place (default: the whole project)
    Fmt {
        /// Files or folders
        paths: Vec<PathBuf>,
        /// Change nothing; exit code 1 if any file isn't formatted
        #[arg(long, conflicts_with = "stdout")]
        check: bool,
        /// Print the formatted text instead of writing it
        #[arg(long)]
        stdout: bool,
        /// Upper-case keywords (overrides abl.toml and ~/.abl/config.toml)
        #[arg(long, conflicts_with_all = ["lower", "preserve"])]
        upper: bool,
        /// Lower-case keywords
        #[arg(long, conflicts_with = "preserve")]
        lower: bool,
        /// Leave keywords as written
        #[arg(long)]
        preserve: bool,
    },
    /// Show what a file depends on, or with --reverse what depends on it
    Deps {
        file: PathBuf,
        /// What depends on the file, and what to recompile after changing it
        #[arg(long, short)]
        reverse: bool,
        /// Expand RUN targets and classes too (includes are always expanded)
        #[arg(long, conflicts_with = "reverse")]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Where a database table or field is read, created, updated or deleted
    Usages {
        /// `customer`, `customer.balance`, or with the database: `sports.customer`
        name: String,
        /// Only creates, updates and deletes
        #[arg(long)]
        writes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Explain an OpenEdge error: `abl explain 138`, or paste the whole message
    Explain {
        /// A message number, a pasted error line, or words to search for
        query: Vec<String>,
        /// List the messages abl knows
        #[arg(long)]
        list: bool,
    },
    /// Work with .df schema files
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    /// The project's databases: create, start, stop, snapshot, restore
    Db {
        #[command(subcommand)]
        command: Option<DbCommand>,
    },
    /// Show rows: abl query "customer WHERE balance > 1000 BY name"
    Query {
        /// A table and optional WHERE / BY (`FOR EACH` in front is fine)
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: data::Format,
        /// Most rows to show (0 for all)
        #[arg(long, default_value_t = 100)]
        max: u64,
        /// Only these fields: --fields custnum,name
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
    },
    /// Write a table's rows as JSON or CSV
    Export {
        table: String,
        /// Only matching rows: --where "balance > 0"
        #[arg(long = "where", value_name = "CONDITION")]
        filter: Option<String>,
        /// The file to write (default: standard output); .json or .csv sets the format
        #[arg(long, short)]
        output: Option<PathBuf>,
        #[arg(long, value_enum)]
        format: Option<data::Format>,
    },
    /// Create rows from a JSON or CSV file, all or nothing
    Import {
        table: String,
        file: PathBuf,
        /// Delete the table's rows first
        #[arg(long)]
        replace: bool,
        #[arg(long, value_enum)]
        format: Option<data::Format>,
    },
}

#[derive(Subcommand)]
enum DbCommand {
    /// The databases in abl.toml and whether they run (the default)
    List,
    /// Create the database and load its schema
    Create {
        name: Option<String>,
        /// Leave it empty: don't load the .df
        #[arg(long)]
        no_schema: bool,
    },
    /// Start a server for the database
    Start { name: Option<String> },
    /// Stop the database's server
    Stop { name: Option<String> },
    /// Delete the database
    Delete {
        name: Option<String>,
        /// Don't ask
        #[arg(long)]
        yes: bool,
    },
    /// Save the database as it is now, to restore later
    Snapshot {
        /// A name for the snapshot (default: the date and time)
        label: Option<String>,
        /// Which database, if abl.toml has several
        #[arg(long)]
        db: Option<String>,
        /// List the snapshots instead
        #[arg(long)]
        list: bool,
    },
    /// Put a snapshot back (the database must be stopped)
    Restore {
        label: String,
        #[arg(long)]
        db: Option<String>,
    },
    /// Tables, fields, indexes and sequences, from the .df files
    Schema {
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DevCommand {
    /// Make a fake OpenEdge install (uses the fake-progres binary built next to abl)
    FakeDlc {
        dir: PathBuf,
        #[arg(long, default_value = "12.8.0")]
        release: String,
    },
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
}

#[derive(Subcommand)]
enum SchemaCommand {
    /// Compare two .df files: what changed from OLD to NEW
    Diff {
        old: PathBuf,
        new: PathBuf,
        /// Write the delta .df that turns OLD into NEW
        #[arg(long, value_name = "FILE")]
        delta: Option<PathBuf>,
        /// Exit with code 1 when the schemas differ (for CI drift checks)
        #[arg(long)]
        exit_code: bool,
    },
}

#[derive(Subcommand)]
enum VulncheckCommand {
    /// Write example advisories to the advisory folder
    Init,
    /// Clone an advisory repository into ~/.abl/advisories, or pull it
    Fetch { url: Option<String> },
}

#[derive(Subcommand)]
enum DotnetCommand {
    /// The assemblies and the namespaces the code uses (the default)
    List,
    /// Add an assembly to assemblies.xml: "Name, Version=..., Culture=..., PublicKeyToken=..."
    /// for one in the GAC, or just the name for a DLL next to assemblies.xml
    Add { name: String },
    /// Remove an assembly from assemblies.xml
    Remove { name: String },
}

#[derive(Subcommand)]
enum OeCommand {
    /// List known installs; * marks the version in use here
    List,
    /// Register an install: `abl oe add 12.8 C:/Progress/OpenEdge128`, or `--detect`
    Add {
        #[arg(required_unless_present = "detect")]
        version: Option<String>,
        #[arg(required_unless_present = "detect")]
        path: Option<PathBuf>,
        /// Search the usual locations (and $DLC) for installs
        #[arg(long, conflicts_with_all = ["version", "path"])]
        detect: bool,
        /// Register the install even if its version file names another version
        #[arg(long, conflicts_with = "detect")]
        force: bool,
    },
    /// Forget an install (files are not touched)
    Remove { version: String },
    /// Set the global default version, or pin the project's with --project
    Use {
        version: String,
        /// Write the version into this project's abl.toml instead
        #[arg(long)]
        project: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let flag = cli.oe.as_deref();
    match run(cli.command, &cwd, flag) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("abl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Option<Command>, cwd: &Path, flag: Option<&str>) -> Result<u8, String> {
    let code = |c: i32| u8::try_from(c).unwrap_or(1);
    match command {
        None => {
            if !project::status(cwd, flag)? {
                Cli::command().print_help().map_err(|e| e.to_string())?;
            }
        }
        Some(Command::Status) => {
            if !project::status(cwd, flag)? {
                return Err(format!(
                    "no {} here or above (run `abl init`)",
                    config::PROJECT_FILE
                ));
            }
        }
        Some(Command::Lsp) => {
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(abl_lsp::serve());
        }
        Some(Command::Init { force }) => project::init(cwd, flag, force)?,
        Some(Command::Env) => project::env(cwd, flag)?,
        Some(Command::Doctor) => return Ok(code(project::doctor(cwd, flag))),
        Some(Command::Oe { command }) => match command {
            OeCommand::List => oe::list(cwd, flag)?,
            OeCommand::Add { detect: true, .. } => oe::detect()?,
            OeCommand::Add {
                version: Some(v),
                path: Some(p),
                force,
                ..
            } => oe::add(&v, &p, force)?,
            OeCommand::Add { .. } => unreachable!("clap requires version and path"),
            OeCommand::Remove { version } => oe::remove(&version)?,
            OeCommand::Use { version, project } => oe::use_version(cwd, &version, project)?,
        },
        Some(Command::Compile {
            paths,
            check,
            force,
            jobs,
            format,
            versions,
        }) => {
            let opts = compile::Options {
                check,
                force,
                jobs: jobs.unwrap_or_else(default_jobs),
                output: format,
                output_dir: None,
            };
            if versions.is_empty() {
                return compile::run(cwd, &paths, flag, &opts);
            }
            return compile::run_versions(cwd, &paths, &versions, &opts);
        }
        Some(Command::Build { output, jobs }) => {
            return build::run(
                cwd,
                output.as_deref(),
                jobs.unwrap_or_else(default_jobs),
                flag,
            )
        }
        Some(Command::Profile {
            file,
            top,
            save,
            args,
        }) => return profile::run(cwd, file.as_deref(), &args, top, save.as_deref(), flag),
        Some(Command::Logs {
            db,
            lines,
            grep,
            follow,
        }) => return logs::run(cwd, db.as_deref(), lines, grep.as_deref(), follow),
        Some(Command::Repl) => return repl::run(cwd, flag),
        Some(Command::Vulncheck {
            command,
            db,
            format,
            fail_on,
        }) => {
            return match command {
                Some(VulncheckCommand::Init) => vulncheck::init(cwd, db.as_deref()),
                Some(VulncheckCommand::Fetch { url }) => vulncheck::fetch(cwd, url.as_deref()),
                None => vulncheck::run(
                    cwd,
                    flag,
                    &vulncheck::Options {
                        db,
                        format,
                        fail_on,
                    },
                ),
            }
        }
        Some(Command::Dotnet { command }) => {
            return match command {
                None | Some(DotnetCommand::List) => dotnet::list(cwd),
                Some(DotnetCommand::Add { name }) => dotnet::add(cwd, &name),
                Some(DotnetCommand::Remove { name }) => dotnet::remove(cwd, &name),
            }
        }
        Some(Command::Install {
            spec,
            name,
            reference,
            vsix,
        }) => {
            return install::install(
                cwd,
                spec.as_deref(),
                name.as_deref(),
                reference.as_deref(),
                vsix.as_deref(),
            )
        }
        Some(Command::Update { name }) => return install::update(cwd, name.as_deref()),
        Some(Command::Remove { name }) => return install::remove(cwd, &name),
        Some(Command::Test {
            paths,
            filter,
            changed,
            format,
            verbose,
        }) => {
            let opts = test::Options {
                format,
                filter,
                changed,
                verbose,
            };
            return test::run(cwd, &paths, flag, &opts);
        }
        Some(Command::Run { file, args }) => return run::run(cwd, file.as_deref(), &args, flag),
        Some(Command::Exec { code }) => return run::exec(cwd, &code, flag),
        Some(Command::Watch { jobs, cycles }) => {
            return run::watch(cwd, flag, jobs.unwrap_or_else(default_jobs), cycles)
        }
        Some(Command::Dev {
            command: DevCommand::FakeDlc { dir, release },
        }) => return run::fake_dlc(&dir, &release),
        Some(Command::Lint {
            paths,
            format,
            max_warnings,
            fix,
            hints,
        }) => {
            let opts = lint::Options {
                output: format,
                max_warnings,
                fix,
                hints,
            };
            return lint::run(cwd, &paths, &opts);
        }
        Some(Command::Fmt {
            paths,
            check,
            stdout,
            upper,
            lower,
            preserve,
        }) => {
            use abl_core::format::KeywordCase;
            let case = match (upper, lower, preserve) {
                (true, _, _) => Some(KeywordCase::Upper),
                (_, true, _) => Some(KeywordCase::Lower),
                (_, _, true) => Some(KeywordCase::Preserve),
                _ => None,
            };
            let mode = match (check, stdout) {
                (true, _) => fmt::Mode::Check,
                (_, true) => fmt::Mode::Stdout,
                _ => fmt::Mode::Write,
            };
            return fmt::run(cwd, &paths, mode, case);
        }
        Some(Command::Deps {
            file,
            reverse,
            all,
            json,
        }) => return deps::run(cwd, &file, &deps::Options { reverse, all, json }),
        Some(Command::Usages { name, writes, json }) => {
            return usages::run(cwd, &name, &usages::Options { writes, json })
        }
        Some(Command::Explain { query, list }) => return explain::run(&query, list),
        Some(Command::Schema {
            command:
                SchemaCommand::Diff {
                    old,
                    new,
                    delta,
                    exit_code,
                },
        }) => return schema::diff(&old, &new, delta.as_deref(), exit_code),
        Some(Command::Db { command }) => {
            use db::Action;
            if let Some(DbCommand::Schema { name, json }) = &command {
                return dbschema::run(cwd, name.as_deref(), *json);
            }
            let (name, action) = match command.unwrap_or(DbCommand::List) {
                DbCommand::List => (None, Action::List),
                DbCommand::Create { name, no_schema } => (name, Action::Create { no_schema }),
                DbCommand::Start { name } => (name, Action::Start),
                DbCommand::Stop { name } => (name, Action::Stop),
                DbCommand::Delete { name, yes } => (name, Action::Delete { yes }),
                DbCommand::Snapshot { label, db, list } => (db, Action::Snapshot { label, list }),
                DbCommand::Restore { label, db } => (db, Action::Restore { label }),
                DbCommand::Schema { .. } => unreachable!(),
            };
            return db::run(cwd, name.as_deref(), action, flag);
        }
        Some(Command::Query {
            query,
            format,
            max,
            fields,
        }) => {
            let opts = data::QueryOptions {
                format,
                max,
                fields,
            };
            return data::query(cwd, &query.join(" "), &opts, flag);
        }
        Some(Command::Export {
            table,
            filter,
            output,
            format,
        }) => {
            return data::export(
                cwd,
                &table,
                filter.as_deref(),
                output.as_deref(),
                format,
                flag,
            )
        }
        Some(Command::Import {
            table,
            file,
            replace,
            format,
        }) => return data::import(cwd, &table, &file, replace, format, flag),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }
}
