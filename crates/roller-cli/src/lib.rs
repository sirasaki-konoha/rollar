//! Command-line front end for Roller.

use std::ffi::OsString;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::Parser;
use roller_diagnostics::SourceError;
use roller_parser::{LexError, Lexer, ParseError, Parser as RollerParser, Program, Token};

const MAX_PARALLEL_JOBS: usize = 1024;

/// Roller command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "roller",
    version,
    about = "Run a section from a Roller build script"
)]
pub struct Cli {
    /// Optional script followed by the section to execute.
    #[arg(value_names = ["SCRIPT", "SECTION"], num_args = 1..=2)]
    pub targets: Vec<OsString>,

    /// Maximum number of parallel jobs (must be at least one).
    #[arg(long, short = 'j', value_parser = parse_jobs)]
    pub jobs: Option<NonZeroUsize>,

    /// Display external commands before execution.
    #[arg(long)]
    pub verbose: bool,

    /// Display planned work without starting external commands.
    #[arg(long)]
    pub dry_run: bool,

    /// Stop after lexical and syntax analysis.
    #[arg(long)]
    pub check: bool,

    /// Print the token stream.
    #[arg(long)]
    pub dump_tokens: bool,

    /// Print the parsed abstract syntax tree.
    #[arg(long)]
    pub dump_ast: bool,
}

fn parse_jobs(value: &str) -> Result<NonZeroUsize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "job count must be a positive integer".to_string())?;
    let jobs =
        NonZeroUsize::new(jobs).ok_or_else(|| "job count must be at least one".to_string())?;
    if jobs.get() > MAX_PARALLEL_JOBS {
        return Err(format!("job count must not exceed {MAX_PARALLEL_JOBS}"));
    }
    Ok(jobs)
}

/// Parsed information about an invocation.
#[derive(Debug, PartialEq)]
pub struct Invocation {
    /// Script path.
    pub script: PathBuf,
    /// Requested section name.
    pub section: String,
    /// Effective maximum parallelism.
    pub jobs: NonZeroUsize,
    /// Whether verbose output was requested.
    pub verbose: bool,
    /// Whether external execution is disabled.
    pub dry_run: bool,
    /// Number of bytes read from the script.
    pub source_bytes: usize,
    /// Loaded UTF-8 source.
    pub source: String,
    /// Token stream, including EOF.
    pub tokens: Vec<Token>,
    /// Parsed program.
    pub program: Program,
    /// Whether only frontend checking was requested.
    pub check: bool,
    /// Whether token debugging output was requested.
    pub dump_tokens: bool,
    /// Whether AST debugging output was requested.
    pub dump_ast: bool,
}

/// Parse arguments, read the selected script as UTF-8, and return invocation data.
pub fn prepare_from<I, T>(args: I) -> Result<Invocation, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let (script, section) = match cli.targets.as_slice() {
        [section] => (
            PathBuf::from("build.roller"),
            section.to_string_lossy().into_owned(),
        ),
        [script, section] => (
            PathBuf::from(script),
            section.to_string_lossy().into_owned(),
        ),
        _ => {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::WrongNumberOfValues,
                "expected a section, optionally preceded by a script path",
            )
            .into());
        }
    };
    let source = fs::read_to_string(&script).map_err(|source| SourceError::Read {
        path: script.clone(),
        source,
    })?;
    let jobs = match cli.jobs {
        Some(jobs) => jobs,
        None => std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
    };
    let tokens = Lexer::new(&source).tokenize()?;
    let program = RollerParser::new(tokens.clone()).parse_program()?;

    Ok(Invocation {
        script,
        section,
        jobs,
        verbose: cli.verbose,
        dry_run: cli.dry_run,
        source_bytes: source.len(),
        source,
        tokens,
        program,
        check: cli.check,
        dump_tokens: cli.dump_tokens,
        dump_ast: cli.dump_ast,
    })
}

/// Failures that can occur before script execution starts.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Invalid command-line arguments.
    #[error(transparent)]
    Arguments(#[from] clap::Error),
    /// Script loading failed.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// Lexical analysis failed.
    #[error(transparent)]
    Lex(#[from] LexError),
    /// Syntax analysis failed.
    #[error(transparent)]
    Parse(#[from] ParseError),
}

/// Failures while removing the project-local build directory.
#[derive(Debug, thiserror::Error)]
pub enum CleanError {
    /// The project root is not an existing directory.
    #[error("project root is not a directory: {0}")]
    InvalidProjectRoot(PathBuf),
    /// Resolving a path failed.
    #[error("cannot access {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Operating-system error.
        source: std::io::Error,
    },
    /// The build path resolved outside of the project.
    #[error("refusing to remove build path outside project root: {0}")]
    UnsafeTarget(PathBuf),
}

/// Safely remove only `<project_root>/.roller/build`.
pub fn clean_build_directory(project_root: &std::path::Path) -> Result<bool, CleanError> {
    if !project_root.is_dir() {
        return Err(CleanError::InvalidProjectRoot(project_root.to_path_buf()));
    }
    let root = project_root
        .canonicalize()
        .map_err(|source| CleanError::Io {
            path: project_root.to_path_buf(),
            source,
        })?;
    let target = root.join(".roller").join("build");
    if !target.exists() {
        return Ok(false);
    }
    let resolved = target.canonicalize().map_err(|source| CleanError::Io {
        path: target,
        source,
    })?;
    if resolved == root || !resolved.starts_with(&root) {
        return Err(CleanError::UnsafeTarget(resolved));
    }
    std::fs::remove_dir_all(&resolved).map_err(|source| CleanError::Io {
        path: resolved,
        source,
    })?;
    Ok(true)
}

/// Format a source-positioned frontend diagnostic.
#[must_use]
pub fn format_frontend_diagnostic(
    error: &CliError,
    path: &std::path::Path,
    source: &str,
) -> String {
    let (message, span) = match error {
        CliError::Lex(error) => (error.message.clone(), error.span),
        CliError::Parse(error) => (
            format!("expected {}, found {}", error.expected, error.actual),
            error.span,
        ),
        _ => return format!("error: {error}"),
    };
    let line = source
        .lines()
        .nth(span.start.line.saturating_sub(1))
        .unwrap_or("");
    let caret_width = span.end.column.saturating_sub(span.start.column).max(1);
    format!(
        "error: {message}\n  --> {}:{}:{}\n   |\n{:>3} | {}\n   | {}{} {message}",
        path.display(),
        span.start.line,
        span.start.column,
        span.start.line,
        line,
        " ".repeat(span.start.column.saturating_sub(1)),
        "^".repeat(caret_width),
    )
}
