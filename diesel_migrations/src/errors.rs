//! Error types that represent migration errors.
//! These are split into multiple segments, depending on
//! where in the migration process an error occurs.

use std::error::Error;
use std::path::PathBuf;
use std::{fmt, io};

use diesel::migration::MigrationVersion;

use crate::file_based_migrations::DieselMigrationName;

/// Errors that occur while preparing to run migrations
#[derive(Debug)]
#[non_exhaustive]
pub enum MigrationError {
    /// The migration directory wasn't found
    MigrationDirectoryNotFound(PathBuf),
    /// Provided migration was in an unknown format
    UnknownMigrationFormat(PathBuf),
    /// General system IO error
    IoError(io::Error),
    /// Provided migration had an incompatible version number
    UnknownMigrationVersion(MigrationVersion<'static>),
    /// No migrations had to be/ could be run
    NoMigrationRun,
    /// `down.sql` file is missing, and you're asking for a `revert`
    /// or `redo`
    NoMigrationRevertFile,
}

impl Error for MigrationError {}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            MigrationError::MigrationDirectoryNotFound(ref p) => write!(
                f,
                "Unable to find migrations directory in {p:?} or any parent directories."
            ),
            MigrationError::UnknownMigrationFormat(_) => write!(
                f,
                "Invalid migration directory: the directory's name should be \
                 <timestamp>_<name_of_migration>, and it should contain up.sql and \
                 optionally down.sql."
            ),
            MigrationError::IoError(ref error) => write!(f, "{error}"),
            MigrationError::UnknownMigrationVersion(ref version) => write!(
                f,
                "Unable to find migration version {version} to revert in the migrations directory."
            ),
            MigrationError::NoMigrationRun => write!(
                f,
                "No migrations have been run. Did you forget `diesel migration run`?"
            ),
            MigrationError::NoMigrationRevertFile => {
                write!(f, "Missing `down.sql` file to revert migration")
            }
        }
    }
}

impl PartialEq for MigrationError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                &MigrationError::MigrationDirectoryNotFound(_),
                &MigrationError::MigrationDirectoryNotFound(_),
            ) => true,
            (
                MigrationError::UnknownMigrationFormat(p1),
                MigrationError::UnknownMigrationFormat(p2),
            ) => p1 == p2,
            _ => false,
        }
    }
}

impl From<io::Error> for MigrationError {
    fn from(e: io::Error) -> Self {
        MigrationError::IoError(e)
    }
}

/// Errors that occur while running migrations
#[derive(Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
#[non_exhaustive]
pub enum RunMigrationsError {
    /// A general migration error occurred
    MigrationError(DieselMigrationName, MigrationError),
    /// The provided migration included an invalid query
    QueryError(DieselMigrationName, diesel::result::Error),
    /// The provided migration was empty
    EmptyMigration(DieselMigrationName),
}

impl Error for RunMigrationsError {}

impl fmt::Display for RunMigrationsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match self {
            RunMigrationsError::MigrationError(v, err) => {
                write!(f, "Failed to run {v} with: {err}")
            }
            RunMigrationsError::QueryError(v, err) => {
                write!(f, "Failed to run {v} with: {err}")?;

                if let diesel::result::Error::DatabaseError(_, error_info) = err {
                    let message = error_info.message();
                    if message.ends_with("cannot run inside a transaction block") {
                        write!(
                            f,
                            " (see https://docs.diesel.rs/{}.x/diesel_migrations/struct.FileBasedMigrations.html#transactions)",
                            env!("CARGO_PKG_VERSION")
                                .rsplit_once('.')
                                .expect("We get a semver version here")
                                .0
                        )?;
                    }
                }

                Ok(())
            }
            RunMigrationsError::EmptyMigration(v) => write!(
                f,
                "Failed to run {v} with: Attempted to run an empty migration."
            ),
        }
    }
}

/// Attach line/column information from the migration SQL when the backend reports a
/// statement position (PostgreSQL). See diesel-rs/diesel#2378.
pub(crate) fn enrich_sql_error(error: diesel::result::Error, sql: &str) -> diesel::result::Error {
    use diesel::result::Error;

    match error {
        Error::DatabaseError(kind, info) => {
            let Some(pos) = info.statement_position() else {
                return Error::DatabaseError(kind, info);
            };
            // PostgreSQL statement positions are 1-based *character* offsets into the query
            // (not bytes). See PG_DIAG_STATEMENT_POSITION / PQresultErrorField.
            let pos = pos.max(1) as usize;
            let (line, column) = char_offset_to_line_column(sql, pos - 1);
            let message = format!("{} at line {}, column {}", info.message(), line, column);
            Error::DatabaseError(
                kind,
                Box::new(EnrichedDatabaseError {
                    message,
                    details: info.details().map(str::to_owned),
                    hint: info.hint().map(str::to_owned),
                    table_name: info.table_name().map(str::to_owned),
                    column_name: info.column_name().map(str::to_owned),
                    constraint_name: info.constraint_name().map(str::to_owned),
                    // Position is already reflected as line/column in `message` so
                    // `Error`'s Display does not also append "at character position N".
                    statement_position: None,
                }),
            )
        }
        other => other,
    }
}

/// Map a 0-based Unicode scalar offset into `sql` to 1-based line and column.
fn char_offset_to_line_column(sql: &str, char_offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for (i, ch) in sql.chars().enumerate() {
        if i == char_offset {
            return (line, column);
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    // Past the end of the string: report the position after the last character.
    (line, column)
}

struct EnrichedDatabaseError {
    message: String,
    details: Option<String>,
    hint: Option<String>,
    table_name: Option<String>,
    column_name: Option<String>,
    constraint_name: Option<String>,
    statement_position: Option<i32>,
}

impl diesel::result::DatabaseErrorInformation for EnrichedDatabaseError {
    fn message(&self) -> &str {
        &self.message
    }
    fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }
    fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
    fn table_name(&self) -> Option<&str> {
        self.table_name.as_deref()
    }
    fn column_name(&self) -> Option<&str> {
        self.column_name.as_deref()
    }
    fn constraint_name(&self) -> Option<&str> {
        self.constraint_name.as_deref()
    }
    fn statement_position(&self) -> Option<i32> {
        self.statement_position
    }
}

#[cfg(test)]
mod tests {
    use super::char_offset_to_line_column;
    use diesel::result::{DatabaseErrorInformation, DatabaseErrorKind, Error};

    #[test]
    fn maps_char_offset_to_line_and_column() {
        let sql = "SELECT 1;\nCREATE TABLE t (id INT);\n";
        // 0-based char offset of 'C' in CREATE (after "SELECT 1;\n")
        assert_eq!(char_offset_to_line_column(sql, 10), (2, 1));
        assert_eq!(char_offset_to_line_column(sql, 0), (1, 1));
        assert_eq!(char_offset_to_line_column("abc", 1), (1, 2));
    }

    #[test]
    fn maps_char_offset_with_multibyte_utf8() {
        // 'é' is one Unicode scalar but two UTF-8 bytes; positions must use characters.
        let sql = "SELECT 'café';\nCREATE";
        // "SELECT '" = 8, "café" = 4, "';\n" = 3 → 'C' at 0-based char index 15
        assert_eq!(char_offset_to_line_column(sql, 15), (2, 1));
        // Byte index of 'C' would be 16; using that would wrongly report line 2, column 2.
        assert_ne!(char_offset_to_line_column(sql, 16), (2, 1));
    }

    #[test]
    fn enrich_clears_statement_position_after_line_column() {
        struct FakeInfo;
        impl DatabaseErrorInformation for FakeInfo {
            fn message(&self) -> &str {
                "syntax error at or near \"CREATE\""
            }
            fn details(&self) -> Option<&str> {
                None
            }
            fn hint(&self) -> Option<&str> {
                None
            }
            fn table_name(&self) -> Option<&str> {
                None
            }
            fn column_name(&self) -> Option<&str> {
                None
            }
            fn constraint_name(&self) -> Option<&str> {
                None
            }
            fn statement_position(&self) -> Option<i32> {
                Some(11) // 1-based char position of 'C' in "SELECT 1;\nCREATE"
            }
        }

        let sql = "SELECT 1;\nCREATE TABLE t (id INT);\n";
        let err = Error::DatabaseError(DatabaseErrorKind::Unknown, Box::new(FakeInfo));
        let enriched = super::enrich_sql_error(err, sql);
        match &enriched {
            Error::DatabaseError(_, info) => {
                assert_eq!(
                    info.message(),
                    "syntax error at or near \"CREATE\" at line 2, column 1"
                );
                assert_eq!(info.statement_position(), None);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // Display must not append a second position line.
        assert_eq!(
            enriched.to_string(),
            "syntax error at or near \"CREATE\" at line 2, column 1"
        );
    }
}
