#![forbid(unsafe_code)]

//! Explicit, bounded operator command for acquiring local persisted cover assets.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub const COVER_BACKFILL_SUBCOMMAND: &str = "cover-backfill";
pub const COVER_BACKFILL_HELP: &str = concat!(
    "Usage:\n",
    "  gamepulse cover-backfill --database <ABSOLUTE_DATABASE_PATH> [--limit 20]\n\n",
    "Select at most 20 persisted cover records; missing or rejected descriptors are reported without a request.\n",
    "The command never deletes database records, retries requests, or starts the HTTP server.\n\n",
    "Repeat only after a report with stored > 0; stop at zero progress, no candidates, or failure.\n\n",
    "Options:\n",
    "  --database <ABSOLUTE_DATABASE_PATH>  Required existing SQLite path.\n",
    "  --limit <1..20>  Maximum covers to attempt; defaults to 20.\n",
    "  --help  Show this help and exit.\n",
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverBackfillCommand {
    database_path: PathBuf,
    limit: usize,
}

impl CoverBackfillCommand {
    fn new(database_path: PathBuf, limit: usize) -> Result<Self, CoverBackfillParseError> {
        if !database_path.is_absolute() || !(1..=20).contains(&limit) {
            return Err(CoverBackfillParseError);
        }
        Ok(Self {
            database_path,
            limit,
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }
    pub const fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoverBackfillEntry {
    Help,
    Command(CoverBackfillCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoverBackfillParseError;

/// Return `None` unless the dedicated cover-backfill subcommand was requested.
pub fn parse_cover_backfill(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Option<CoverBackfillEntry>, CoverBackfillParseError> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Ok(None);
    };
    if command.as_os_str() != OsStr::new(COVER_BACKFILL_SUBCOMMAND) {
        return Ok(None);
    }
    let first_argument = arguments.next();
    if matches!(first_argument.as_ref(), Some(argument) if argument.as_os_str() == OsStr::new("--help"))
    {
        return if arguments.next().is_none() {
            Ok(Some(CoverBackfillEntry::Help))
        } else {
            Err(CoverBackfillParseError)
        };
    }
    let mut arguments = first_argument.into_iter().chain(arguments);
    let mut database_path = None;
    let mut limit = None;
    while let Some(argument) = arguments.next() {
        let value = arguments.next().ok_or(CoverBackfillParseError)?;
        if argument.as_os_str() == OsStr::new("--database") && database_path.is_none() {
            database_path = Some(PathBuf::from(value));
        } else if argument.as_os_str() == OsStr::new("--limit") && limit.is_none() {
            limit = Some(
                value
                    .to_string_lossy()
                    .parse::<usize>()
                    .map_err(|_| CoverBackfillParseError)?,
            );
        } else {
            return Err(CoverBackfillParseError);
        }
    }
    CoverBackfillCommand::new(
        database_path.ok_or(CoverBackfillParseError)?,
        limit.unwrap_or(20),
    )
    .map(CoverBackfillEntry::Command)
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_bounded_existing_database_command() {
        let parsed = parse_cover_backfill([
            OsString::from("cover-backfill"),
            OsString::from("--database"),
            OsString::from("/tmp/gamepulse.sqlite3"),
            OsString::from("--limit"),
            OsString::from("3"),
        ])
        .expect("command must parse")
        .expect("cover command must be selected");
        assert!(matches!(parsed, CoverBackfillEntry::Command(command)
            if command.database_path() == Path::new("/tmp/gamepulse.sqlite3") && command.limit() == 3));
    }

    #[test]
    fn rejects_an_unbounded_or_relative_backfill_request() {
        assert!(
            parse_cover_backfill([
                OsString::from("cover-backfill"),
                OsString::from("--database"),
                OsString::from("gamepulse.sqlite3"),
            ])
            .is_err()
        );
        assert!(
            parse_cover_backfill([
                OsString::from("cover-backfill"),
                OsString::from("--database"),
                OsString::from("/tmp/gamepulse.sqlite3"),
                OsString::from("--limit"),
                OsString::from("21"),
            ])
            .is_err()
        );
    }
}
