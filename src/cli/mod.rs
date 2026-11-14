//! The non-interactive command surface: argument parsing, dispatch and the
//! themed `--help` output.
//!
//! Lives at `src/cli` (app level, not TUI level) and is declared by
//! `src/tui/main.rs` with an explicit `#[path]`, because the help screen is
//! painted with the user's active theme — a TUI asset — while everything else
//! (session lookup, update, paths) is plain command-line work.
//!
//! The grammar is deliberately small and hand-parsed: `cosh` is an interactive
//! TUI first, and its CLI is a handful of flags plus three commands. A parser
//! crate would be more surface than the surface it parses.
//!
//! ```text
//! cosh                          start the TUI
//! cosh -c | --continue          start in the most recently active session here
//! cosh -s <id> | --session <id> start in a specific session
//! cosh update [version] [-y]    install the latest (or a pinned) release
//! cosh paths                    print the storage directories
//! cosh -h | --help [command]    show help
//! cosh -v | --version           print the version
//! ```

use std::ffi::OsString;
use std::fmt;

pub mod help;
pub mod paths;
pub mod resume;
pub mod style;
pub mod update;

/// Program name used in usage lines and error hints.
pub const NAME: &str = "cosh";

/// The version `--version` prints, the help header shows and `update` reports
/// against. Compile-time so it can never drift from the release tag.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every command `cosh --help` documents.
///
/// This list is also what `cosh help <command>` validates against, so a
/// command cannot ship without a help page (the guard test in [`help`] keeps
/// the two in lockstep).
pub const COMMANDS: &[&str] = &["update", "paths", "help"];

/// Options for the plain `cosh` (TUI) start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    /// `-c` / `--continue`: resume the most recently active session of the
    /// current directory.
    pub continue_last: bool,
    /// `-s <id>` / `--session <id>`: resume one specific session.
    pub session_id: Option<String>,
}

/// What this process should do, resolved from `argv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Start the interactive TUI.
    Launch(LaunchOptions),
    /// `cosh update [version]`
    Update(update::UpdateOptions),
    /// `cosh paths`
    Paths,
    /// `cosh --check-keyring` — the keyring diagnostic, unchanged.
    CheckKeyring,
    /// Print help and exit 0. `None` = the top-level page.
    Help(Option<String>),
    /// Print the version and exit 0.
    Version,
}

/// A malformed command line. Carries the message only: the caller prints it
/// and exits with the usage-error status, so no caller has to classify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn unknown_flag(flag: &str) -> Self {
        Self::new(format!("unexpected argument '{flag}'"))
    }

    /// The same error plus the command it belongs to, for its help hint.
    fn in_command(mut self, command: &str) -> Self {
        self.message = format!("{} {}", self.message, format!("(in '{command}')"));
        self
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Parse `argv` (without the program name).
pub fn parse<I>(args: I) -> Result<Invocation, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    // Lossy on purpose: the only values that cross this boundary are an
    // optional session id and a release version, both ASCII by construction.
    // A non-UTF-8 argument elsewhere is an unknown argument, not a crash.
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let mut launch = LaunchOptions::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => return Ok(Invocation::Help(help_subject(&args[index + 1..])?)),
            "-v" | "--version" => return Ok(Invocation::Version),
            "-c" | "--continue" => launch.continue_last = true,
            "-s" | "--session" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::new("a session id is required: -s <id> / --session <id>")
                })?;
                if launch.session_id.replace(value.clone()).is_some() {
                    return Err(CliError::new("--session was given more than once"));
                }
                index += 1;
            }
            "--check-keyring" => return Ok(Invocation::CheckKeyring),
            _ => {
                if let Some(value) = arg.strip_prefix("--session=") {
                    if value.is_empty() {
                        return Err(CliError::new("a session id is required: --session=<id>"));
                    }
                    if launch.session_id.replace(value.to_string()).is_some() {
                        return Err(CliError::new("--session was given more than once"));
                    }
                } else if arg.starts_with('-') {
                    return Err(CliError::unknown_flag(arg));
                } else {
                    return parse_command(arg, &args[index + 1..]);
                }
            }
        }
        index += 1;
    }

    if launch.continue_last && launch.session_id.is_some() {
        return Err(CliError::new(
            "-c/--continue and -s/--session are mutually exclusive",
        ));
    }
    Ok(Invocation::Launch(launch))
}

/// Parse the first positional token as a command name plus that command's own
/// arguments.
fn parse_command(command: &str, rest: &[String]) -> Result<Invocation, CliError> {
    match command {
        "update" => parse_update(rest),
        "paths" => {
            for arg in rest {
                match arg.as_str() {
                    "-h" | "--help" => return Ok(Invocation::Help(Some("paths".to_string()))),
                    other => return Err(CliError::unknown_flag(other).in_command("paths")),
                }
            }
            Ok(Invocation::Paths)
        }
        "help" => Ok(Invocation::Help(help_subject(rest)?)),
        other => Err(CliError::new(format!(
            "unknown command '{other}' — available commands: {}",
            COMMANDS.join(", ")
        ))),
    }
}

fn parse_update(rest: &[String]) -> Result<Invocation, CliError> {
    let mut options = update::UpdateOptions::default();
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "-h" | "--help" => return Ok(Invocation::Help(Some("update".to_string()))),
            "-y" | "--yes" => options.assume_yes = true,
            // Internal: the in-app updater re-runs this binary with it, so the
            // app comes back on the new image. Documented in the code, kept
            // out of the help page — a user running `cosh update` stays in
            // their shell by design.
            "--relaunch" => options.relaunch = true,
            other if other.starts_with('-') => {
                return Err(CliError::unknown_flag(other).in_command("update"));
            }
            version => {
                if options.target.is_some() {
                    return Err(CliError::new(
                        "only one version can be installed at a time",
                    ));
                }
                options.target = Some(version.to_string());
            }
        }
        index += 1;
    }
    Ok(Invocation::Update(options))
}

/// The command named after `help` / `--help`, validated against [`COMMANDS`].
fn help_subject(rest: &[String]) -> Result<Option<String>, CliError> {
    let Some(subject) = rest.first() else {
        return Ok(None);
    };
    if subject.starts_with('-') {
        // `cosh help --foo` is a mistake, not a request for global help.
        return Err(CliError::unknown_flag(subject));
    }
    if COMMANDS.contains(&subject.as_str()) {
        Ok(Some(subject.clone()))
    } else {
        Err(CliError::new(format!(
            "unknown command '{subject}' — available commands: {}",
            COMMANDS.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, CliError> {
        parse(args.iter().map(OsString::from))
    }

    fn launch(args: &[&str]) -> LaunchOptions {
        match parse_args(args).expect("expected a successful parse") {
            Invocation::Launch(options) => options,
            other => panic!("expected a TUI launch, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_starts_the_tui() {
        assert_eq!(launch(&[]), LaunchOptions::default());
    }

    #[test]
    fn continue_flags_resume_the_last_session() {
        assert!(launch(&["-c"]).continue_last);
        assert!(launch(&["--continue"]).continue_last);
    }

    #[test]
    fn session_id_accepts_every_spelling() {
        for args in [
            vec!["-s", "123"],
            vec!["--session", "123"],
            vec!["--session=123"],
        ] {
            assert_eq!(
                launch(&args).session_id.as_deref(),
                Some("123"),
                "spelling {args:?}"
            );
        }
    }

    #[test]
    fn session_without_a_value_is_a_usage_error() {
        let error = parse_args(&["--session"]).unwrap_err();
        assert!(error.to_string().contains("session id"), "{error}");
    }

    #[test]
    fn continue_and_session_together_are_refused() {
        let error = parse_args(&["-c", "-s", "123"]).unwrap_err();
        assert!(error.to_string().contains("mutually exclusive"), "{error}");
    }

    #[test]
    fn help_takes_an_optional_documented_command() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Invocation::Help(None));
        assert_eq!(parse_args(&["-h"]).unwrap(), Invocation::Help(None));
        assert_eq!(
            parse_args(&["help", "update"]).unwrap(),
            Invocation::Help(Some("update".into()))
        );
        assert_eq!(
            parse_args(&["--help", "paths"]).unwrap(),
            Invocation::Help(Some("paths".into()))
        );
    }

    #[test]
    fn help_refuses_an_unknown_command() {
        let error = parse_args(&["help", "frobnicate"]).unwrap_err();
        assert!(error.to_string().contains("unknown command"), "{error}");
    }

    #[test]
    fn version_short_and_long_forms() {
        assert_eq!(parse_args(&["-v"]).unwrap(), Invocation::Version);
        assert_eq!(parse_args(&["--version"]).unwrap(), Invocation::Version);
    }

    #[test]
    fn paths_takes_no_arguments() {
        assert_eq!(parse_args(&["paths"]).unwrap(), Invocation::Paths);
        let error = parse_args(&["paths", "--fast"]).unwrap_err();
        assert!(error.to_string().contains("--fast"), "{error}");
    }

    #[test]
    fn update_accepts_a_pinned_version_and_yes() {
        let options = match parse_args(&["update"]).unwrap() {
            Invocation::Update(options) => options,
            other => panic!("expected update, got {other:?}"),
        };
        assert_eq!(options, update::UpdateOptions::default());

        let options = match parse_args(&["update", "v0.2.3", "-y"]).unwrap() {
            Invocation::Update(options) => options,
            other => panic!("expected update, got {other:?}"),
        };
        assert_eq!(options.target.as_deref(), Some("v0.2.3"));
        assert!(options.assume_yes);
        assert!(!options.relaunch);
    }

    #[test]
    fn update_refuses_two_versions() {
        let error = parse_args(&["update", "v0.2.3", "v0.2.4"]).unwrap_err();
        assert!(error.to_string().contains("only one version"), "{error}");
    }

    #[test]
    fn update_help_is_scoped_to_the_command() {
        assert_eq!(
            parse_args(&["update", "--help"]).unwrap(),
            Invocation::Help(Some("update".into()))
        );
    }

    #[test]
    fn unknown_flags_and_commands_are_refused() {
        assert!(parse_args(&["--nope"]).is_err());
        assert!(parse_args(&["frobnicate"]).is_err());
    }

    #[test]
    fn check_keyring_stays_available() {
        assert_eq!(
            parse_args(&["--check-keyring"]).unwrap(),
            Invocation::CheckKeyring
        );
    }
}
