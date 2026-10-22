use serde::Serialize;
use serde::ser::SerializeStruct;
use ts_rs::TS;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every failure the core can report. Messages are written for the person
/// using the app: they say what happened and, where possible, what to do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Network error: {0}")]
    Network(String),

    #[error("The server returned HTTP {status} for {url}")]
    HttpStatus { status: u16, url: String },

    #[error(
        "The download of {id} doesn't match its published checksum, so it was discarded. \
         (expected {expected}, got {actual})"
    )]
    HashMismatch {
        id: String,
        expected: String,
        actual: String,
    },

    #[error("The download of {id} is {actual} bytes, but the registry says {expected}")]
    SizeMismatch { id: String, expected: u64, actual: u64 },

    #[error(
        "{path} reads back differently from what was written, so it was removed. \
         The drive may be faulty; try another drive."
    )]
    DriveFaulty { path: String },

    #[error("Ran out of space while {context}")]
    DiskFull { context: String },

    #[error("The drive was disconnected or stopped responding while {context}")]
    DriveDisconnected { context: String },

    #[error("Not enough free space: {needed} bytes needed, {available} available")]
    NotEnoughSpace { needed: u64, available: u64 },

    #[error(
        "{name} is {size} bytes, larger than {filesystem} allows for one file. \
         Back up the drive, format it as exFAT, and try again."
    )]
    FileTooLarge {
        name: String,
        size: u64,
        filesystem: String,
    },

    #[error("{0}")]
    Profile(String),

    #[error("Unknown package: {0}")]
    UnknownPackage(String),

    #[error("Unknown profile: {0}")]
    UnknownProfile(String),

    #[error("{0}")]
    Dependency(String),

    #[error("{path} already exists on the drive and wasn't installed by this app")]
    Conflict { path: String },

    #[error("The archive contains an unsafe path: {entry}")]
    UnsafeArchive { entry: String },

    #[error("{0}")]
    InvalidData(String),

    #[error("This drive hasn't been set up yet. Run Check drive and choose Initialize.")]
    NotInitialized,

    #[error("Cancelled")]
    Cancelled,

    #[error("Another drive operation is still running. Wait for it to finish.")]
    Busy,

    #[error("{0}")]
    Eject(String),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

/// Machine-readable error category, sent to the frontend next to the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ErrorKind {
    Network,
    HttpStatus,
    HashMismatch,
    SizeMismatch,
    DriveFaulty,
    DiskFull,
    DriveDisconnected,
    NotEnoughSpace,
    FileTooLarge,
    Profile,
    UnknownPackage,
    UnknownProfile,
    Dependency,
    Conflict,
    UnsafeArchive,
    InvalidData,
    NotInitialized,
    Cancelled,
    Busy,
    Eject,
    Io,
}

/// The shape every command error takes on the wire.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AppError {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::Network(_) => ErrorKind::Network,
            Error::HttpStatus { .. } => ErrorKind::HttpStatus,
            Error::HashMismatch { .. } => ErrorKind::HashMismatch,
            Error::SizeMismatch { .. } => ErrorKind::SizeMismatch,
            Error::DriveFaulty { .. } => ErrorKind::DriveFaulty,
            Error::DiskFull { .. } => ErrorKind::DiskFull,
            Error::DriveDisconnected { .. } => ErrorKind::DriveDisconnected,
            Error::NotEnoughSpace { .. } => ErrorKind::NotEnoughSpace,
            Error::FileTooLarge { .. } => ErrorKind::FileTooLarge,
            Error::Profile(_) => ErrorKind::Profile,
            Error::UnknownPackage(_) => ErrorKind::UnknownPackage,
            Error::UnknownProfile(_) => ErrorKind::UnknownProfile,
            Error::Dependency(_) => ErrorKind::Dependency,
            Error::Conflict { .. } => ErrorKind::Conflict,
            Error::UnsafeArchive { .. } => ErrorKind::UnsafeArchive,
            Error::InvalidData(_) => ErrorKind::InvalidData,
            Error::NotInitialized => ErrorKind::NotInitialized,
            Error::Cancelled => ErrorKind::Cancelled,
            Error::Busy => ErrorKind::Busy,
            Error::Eject(_) => ErrorKind::Eject,
            Error::Io { .. } => ErrorKind::Io,
        }
    }

    /// Transient failures worth retrying (a dropped connection, a 5xx).
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Network(_) => true,
            Error::HttpStatus { status, .. } => *status >= 500 || *status == 408 || *status == 429,
            _ => false,
        }
    }

    pub fn to_app_error(&self) -> AppError {
        AppError {
            kind: self.kind(),
            message: self.to_string(),
        }
    }
}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", &self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

/// Wraps an I/O error with what was being done, recognising a full disk and a
/// drive that vanished so the user gets an actionable message.
pub fn io_error(context: impl Into<String>, err: std::io::Error) -> Error {
    let context = context.into();
    if err.kind() == std::io::ErrorKind::StorageFull || is_disk_full(&err) {
        return Error::DiskFull { context };
    }
    if is_disconnected(&err) {
        return Error::DriveDisconnected { context };
    }
    Error::Io { context, source: err }
}

/// `move |e| io_error(context, e)` for use with `map_err`.
pub fn io_ctx(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Error {
    let context = context.into();
    move |e| io_error(context, e)
}

fn is_disk_full(err: &std::io::Error) -> bool {
    match err.raw_os_error() {
        // ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL
        #[cfg(windows)]
        Some(39) | Some(112) => true,
        #[cfg(unix)]
        Some(code) if code == libc::ENOSPC || code == libc::EDQUOT => true,
        _ => false,
    }
}

fn is_disconnected(err: &std::io::Error) -> bool {
    match err.raw_os_error() {
        // ERROR_GEN_FAILURE, ERROR_NOT_READY, ERROR_DEV_NOT_EXIST,
        // ERROR_IO_DEVICE, ERROR_DEVICE_NOT_CONNECTED, ERROR_NO_SUCH_DEVICE
        #[cfg(windows)]
        Some(31) | Some(21) | Some(55) | Some(1117) | Some(1167) | Some(433) => true,
        #[cfg(unix)]
        Some(code) if code == libc::ENODEV || code == libc::ENXIO || code == libc::EIO => true,
        _ => false,
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::InvalidData(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_kind_and_message() {
        let json = serde_json::to_value(Error::NotInitialized).unwrap();
        assert_eq!(json["kind"], "notInitialized");
        assert!(json["message"].as_str().unwrap().contains("Initialize"));
    }

    #[test]
    fn full_disk_is_recognised() {
        let err = io_error("copying", std::io::Error::from(std::io::ErrorKind::StorageFull));
        assert!(matches!(err, Error::DiskFull { .. }));
    }

    #[test]
    fn retryable_errors() {
        assert!(Error::Network("reset".into()).is_retryable());
        assert!(
            Error::HttpStatus {
                status: 503,
                url: String::new()
            }
            .is_retryable()
        );
        assert!(
            !Error::HttpStatus {
                status: 404,
                url: String::new()
            }
            .is_retryable()
        );
        assert!(!Error::Cancelled.is_retryable());
    }
}
