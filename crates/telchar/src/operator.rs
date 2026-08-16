//! Implements bounded, read-only operator inspection commands.

#[path = "operator/report.rs"]
mod report;

use std::io;

use report::{BackendReport, BuildReport, ConfigReport, QueueReport, RecoveryReport, StatusReport};
use telchar::service::config::ServiceConfig;

const DEFAULT_LIMIT: usize = 64;
const MAXIMUM_LIMIT: usize = 256;

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let command = parse(std::env::args().skip(2))?;
    let config = ServiceConfig::load()?;
    let value = match command {
        Command::ConfigCheck => ConfigReport::from_config(&config),
        Command::Status => {
            let database_url = config.require_database_url()?;
            StatusReport::read(database_url)?
        }
        Command::Queue { limit } => {
            let database_url = config.require_database_url()?;
            QueueReport::read(database_url, limit)?
        }
        Command::Build { derivation_path } => {
            let database_url = config.require_database_url()?;
            BuildReport::read(database_url, &derivation_path)?
        }
        Command::Backends => {
            let database_url = config.require_database_url()?;
            BackendReport::read(&config, database_url)?
        }
        Command::Recovery { limit } => {
            let database_url = config.require_database_url()?;
            RecoveryReport::read(database_url, limit)?
        }
    };
    serde_json::to_writer(std::io::stdout().lock(), &value)?;
    println!();
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Status,
    Queue { limit: usize },
    Build { derivation_path: String },
    Backends,
    Recovery { limit: usize },
    ConfigCheck,
}

fn parse(mut arguments: impl Iterator<Item = String>) -> io::Result<Command> {
    let command = arguments
        .next()
        .ok_or_else(|| invalid("operator command is required"))?;
    match command.as_str() {
        "status" => no_more(arguments, Command::Status),
        "build" => build(arguments),
        "backends" => no_more(arguments, Command::Backends),
        "config-check" => no_more(arguments, Command::ConfigCheck),
        "queue" => bounded(arguments).map(|limit| Command::Queue { limit }),
        "recovery" => bounded(arguments).map(|limit| Command::Recovery { limit }),
        _ => Err(invalid("unknown operator command")),
    }
}

fn build(mut arguments: impl Iterator<Item = String>) -> io::Result<Command> {
    let derivation_path = arguments
        .next()
        .ok_or_else(|| invalid("derivation path is required"))?;
    if arguments.next().is_some()
        || derivation_path.len() > 4096
        || !derivation_path.starts_with("/nix/store/")
        || !derivation_path.ends_with(".drv")
        || derivation_path.contains('\0')
    {
        return Err(invalid("derivation path is invalid"));
    }
    Ok(Command::Build { derivation_path })
}

fn no_more(mut arguments: impl Iterator<Item = String>, command: Command) -> io::Result<Command> {
    if arguments.next().is_some() {
        return Err(invalid("unexpected operator argument"));
    }
    Ok(command)
}

fn bounded(mut arguments: impl Iterator<Item = String>) -> io::Result<usize> {
    let Some(argument) = arguments.next() else {
        return Ok(DEFAULT_LIMIT);
    };
    if argument != "--limit" {
        return Err(invalid("expected --limit"));
    }
    let limit = arguments
        .next()
        .ok_or_else(|| invalid("operator limit is required"))?
        .parse::<usize>()
        .map_err(|_| invalid("operator limit is invalid"))?;
    if arguments.next().is_some() || !(1..=MAXIMUM_LIMIT).contains(&limit) {
        return Err(invalid("operator limit is invalid"));
    }
    Ok(limit)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_bounded_read_commands() {
        assert_eq!(
            parse(["status".into()].into_iter()).unwrap(),
            Command::Status
        );
        assert_eq!(
            parse(["queue".into(), "--limit".into(), "256".into()].into_iter()).unwrap(),
            Command::Queue { limit: 256 }
        );
        assert_eq!(
            parse(["build".into(), "/nix/store/example.drv".into()].into_iter()).unwrap(),
            Command::Build {
                derivation_path: "/nix/store/example.drv".into()
            }
        );
        assert!(parse(["cancel".into()].into_iter()).is_err());
        assert!(parse(["queue".into(), "--limit".into(), "0".into()].into_iter()).is_err());
        assert!(parse(["recovery".into(), "--limit".into(), "257".into()].into_iter()).is_err());
    }
}
