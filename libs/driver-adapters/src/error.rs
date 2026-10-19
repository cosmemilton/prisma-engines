// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
#[cfg(feature = "mysql")]
use quaint::error::MysqlError;

#[cfg(feature = "postgresql")]
use quaint::error::PostgresError;

#[cfg(feature = "sqlite")]
use quaint::error::SqliteError;

#[cfg(feature = "mssql")]
use quaint::error::MssqlError;

use serde::Deserialize;

#[cfg(feature = "postgresql")]
#[derive(Deserialize)]
#[serde(remote = "PostgresError")]
pub struct PostgresErrorDef {
    code: String,
    message: String,
    severity: String,
    detail: Option<String>,
    column: Option<String>,
    hint: Option<String>,
}

#[cfg(feature = "mysql")]
#[derive(Deserialize)]
#[serde(remote = "MysqlError")]
pub struct MysqlErrorDef {
    pub code: u16,
    pub message: String,
    pub state: String,
}

#[cfg(feature = "sqlite")]
#[derive(Deserialize)]
#[serde(remote = "SqliteError", rename_all = "camelCase")]
pub struct SqliteErrorDef {
    pub extended_code: i32,
    pub message: Option<String>,
}

#[cfg(feature = "mssql")]
#[derive(Deserialize)]
#[serde(remote = "MssqlError", rename_all = "camelCase")]
pub struct MssqlErrorDef {
    pub code: u32,
    pub message: String,
}

/// Wrapper for JS-side errors
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DriverAdapterError {
    pub original_message: Option<String>,
    pub original_code: Option<String>,
    #[serde(flatten)]
    pub mapped: MappedDriverAdapterError,
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum MappedDriverAdapterError {
    /// Unexpected JS exception
    GenericJs {
        id: i32,
    },
    UnsupportedNativeDataType {
        #[serde(rename = "type")]
        native_type: String,
    },
    InvalidIsolationLevel {
        level: String,
    },
    LengthMismatch {
        column: Option<String>,
    },
    UniqueConstraintViolation {
        constraint: Option<DriverAdapterConstraint>,
    },
    NullConstraintViolation {
        constraint: Option<DriverAdapterConstraint>,
    },
    ForeignKeyConstraintViolation {
        constraint: Option<DriverAdapterConstraint>,
    },
    DatabaseNotReachable {
        host: Option<String>,
        port: Option<u16>,
    },
    DatabaseDoesNotExist {
        db: Option<String>,
    },
    DatabaseAlreadyExists {
        db: Option<String>,
    },
    DatabaseAccessDenied {
        db: Option<String>,
    },
    ConnectionClosed,
    TlsConnectionError {
        reason: String,
    },
    AuthenticationFailed {
        user: Option<String>,
    },
    TransactionWriteConflict,

    TableDoesNotExist {
        table: Option<String>,
    },
    ColumnNotFound {
        column: Option<String>,
    },
    TooManyConnections {
        cause: String,
    },
    ValueOutOfRange {
        cause: String,
    },
    InvalidInputValue {
        message: String,
    },
    SocketTimeout,
    InconsistentColumnData {
        cause: String,
    },
    MissingFullTextSearchIndex,
    TransactionAlreadyClosed {
        cause: String,
    },
    #[cfg(feature = "postgresql")]
    #[serde(rename = "postgres")]
    Postgres(#[serde(with = "PostgresErrorDef")] PostgresError),
    #[cfg(feature = "mysql")]
    #[serde(rename = "mysql")]
    Mysql(#[serde(with = "MysqlErrorDef")] MysqlError),
    #[cfg(feature = "sqlite")]
    #[serde(rename = "sqlite")]
    Sqlite(#[serde(with = "SqliteErrorDef")] SqliteError),
    #[cfg(feature = "mssql")]
    #[serde(rename = "mssql")]
    Mssql(#[serde(with = "MssqlErrorDef")] MssqlError),
    #[cfg(feature = "firebird")]
    #[serde(rename = "firebird")]
    Firebird {
        code: i32,
        message: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DriverAdapterConstraint {
    Fields(Vec<String>),
    Index(String),
    ForeignKey,
}

impl From<DriverAdapterConstraint> for quaint::error::DatabaseConstraint {
    fn from(value: DriverAdapterConstraint) -> Self {
        match value {
            DriverAdapterConstraint::Fields(fields) => quaint::error::DatabaseConstraint::Fields(fields),
            DriverAdapterConstraint::Index(index) => quaint::error::DatabaseConstraint::Index(index),
            DriverAdapterConstraint::ForeignKey => quaint::error::DatabaseConstraint::ForeignKey,
        }
    }
}

#[cfg(all(test, feature = "firebird"))]
mod firebird_error_contract_tests {
    use super::*;
    use quaint::error::{Error, ErrorKind};

    fn convert(payload: &str) -> Error {
        serde_json::from_str::<DriverAdapterError>(payload).unwrap().into()
    }

    #[test]
    fn firebird_invalid_input_guard_keeps_its_actionable_message() {
        let error = convert(
            r#"{"kind":"InvalidInputValue","message":"Firebird migration history requires an explicit, separate shadowDatabase"}"#,
        );
        assert!(
            matches!(error.kind(), ErrorKind::QueryInvalidInput(message) if message.contains("separate shadowDatabase"))
        );
        assert!(error.to_string().contains("separate shadowDatabase"));
    }

    #[test]
    fn firebird_native_error_keeps_gds_and_original_diagnostics() {
        let error = convert(
            r#"{"kind":"firebird","code":335544569,"message":"SQL error code = -104","originalCode":"335544569","originalMessage":"Dynamic SQL Error: SQL error code = -104"}"#,
        );
        assert!(
            matches!(error.kind(), ErrorKind::RawConnectorError { status, reason } if status == "335544569" && reason == "SQL error code = -104")
        );
        assert_eq!(error.original_code(), Some("335544569"));
        assert_eq!(
            error.original_message(),
            Some("Dynamic SQL Error: SQL error code = -104")
        );

        let fallback = convert(r#"{"kind":"firebird","code":335544569,"message":"retained native diagnostic"}"#);
        assert_eq!(fallback.original_code(), Some("335544569"));
        assert_eq!(fallback.original_message(), Some("retained native diagnostic"));
    }

    #[test]
    fn firebird_timeout_and_column_conversion_errors_keep_their_shapes() {
        let timeout =
            convert(r#"{"kind":"SocketTimeout","originalCode":"ETIMEDOUT","originalMessage":"socket timed out"}"#);
        assert!(matches!(timeout.kind(), ErrorKind::SocketTimeout));
        assert_eq!(timeout.original_code(), Some("ETIMEDOUT"));
        assert_eq!(timeout.original_message(), Some("socket timed out"));

        let conversion = convert(r#"{"kind":"InconsistentColumnData","cause":"Invalid UTF8 BLOB"}"#);
        assert!(matches!(conversion.kind(), ErrorKind::ConversionError(cause) if cause.as_ref() == "Invalid UTF8 BLOB"));
    }

    #[test]
    fn firebird_malformed_error_contract_is_not_silently_swallowed() {
        assert!(serde_json::from_str::<DriverAdapterError>(r#"{"kind":"firebird","message":"missing code"}"#).is_err());
        assert!(serde_json::from_str::<DriverAdapterError>(r#"{"kind":"InvalidInputValue"}"#).is_err());
    }
}
