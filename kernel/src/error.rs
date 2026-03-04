// Unified error type for pulp-os
//
// Replaces the flat `StorageError` enum and ad-hoc `&'static str`
// errors with a single `Copy` type that carries:
//
//   ErrorKind  — *what* went wrong (storage, parse, resource …)
//   source     — *where* it happened (`&'static str`, usually
//                module_path!() or a short caller-supplied tag)
//
// Every `Result` in the kernel and app layers should use this type.
// The smol-epub trait boundary (`Result<T, &'static str>`) converts
// at the edge via the `From` impls.

use core::fmt;

// ---------------------------------------------------------------------------
// ErrorKind — the category of failure
// ---------------------------------------------------------------------------

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    // -- storage / SD card --
    /// SD card not inserted or not responding.
    NoCard,
    /// Could not open the FAT volume.
    OpenVolume,
    /// Could not open a directory.
    OpenDir,
    /// Could not open a file.
    OpenFile,
    /// Read I/O failed.
    ReadFailed,
    /// Write I/O failed.
    WriteFailed,
    /// Seek within a file failed.
    SeekFailed,
    /// Delete operation failed.
    DeleteFailed,
    /// Directory is full (cannot create entry).
    DirFull,
    /// File or directory not found.
    NotFound,

    // -- data / parsing --
    /// EPUB, ZIP, or similar structure is invalid.
    ParseFailed,
    /// Data is malformed or unexpected.
    InvalidData,
    /// UTF-8 or other text-encoding error.
    BadEncoding,

    // -- resources --
    /// Heap allocation failed.
    OutOfMemory,
    /// Supplied buffer is too small for the operation.
    BufferTooSmall,

    // -- network (upload) --
    /// Network read/write failed.
    NetworkIo,
    /// Protocol-level error (HTTP, multipart, etc.).
    Protocol,

    // -- catch-all --
    /// Unclassified error (carries context in `source`).
    Other,
}

impl ErrorKind {
    /// Short human-readable label (suitable for UI and log lines).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoCard => "no sd card",
            Self::OpenVolume => "open volume failed",
            Self::OpenDir => "open dir failed",
            Self::OpenFile => "open file failed",
            Self::ReadFailed => "read failed",
            Self::WriteFailed => "write failed",
            Self::SeekFailed => "seek failed",
            Self::DeleteFailed => "delete failed",
            Self::DirFull => "directory full",
            Self::NotFound => "not found",
            Self::ParseFailed => "parse failed",
            Self::InvalidData => "invalid data",
            Self::BadEncoding => "bad encoding",
            Self::OutOfMemory => "out of memory",
            Self::BufferTooSmall => "buffer too small",
            Self::NetworkIo => "network error",
            Self::Protocol => "protocol error",
            Self::Other => "error",
        }
    }

    /// True for any variant that originates from SD-card storage I/O.
    pub const fn is_storage(self) -> bool {
        matches!(
            self,
            Self::NoCard
                | Self::OpenVolume
                | Self::OpenDir
                | Self::OpenFile
                | Self::ReadFailed
                | Self::WriteFailed
                | Self::SeekFailed
                | Self::DeleteFailed
                | Self::DirFull
                | Self::NotFound
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Error — the unified error value
// ---------------------------------------------------------------------------

/// Unified error for the entire pulp-os stack.
///
/// Cheap to copy (one discriminant byte + one `&'static str` pointer).
/// Carries *what* failed ([`ErrorKind`]) and a compile-time *source*
/// string that identifies the call-site or subsystem.
///
/// # Constructing
///
/// ```ignore
/// // Constant shorthand (no source tag):
/// Error::READ_FAILED
///
/// // With explicit source:
/// Error::new(ErrorKind::OpenFile, "epub_init_zip")
///
/// // Via the err!() macro (auto-stamps module_path!()):
/// err!(ReadFailed)
/// err!(OpenFile, "epub_init_zip")
/// ```
#[derive(Clone, Copy)]
pub struct Error {
    kind: ErrorKind,
    /// Where the error was created — a `module_path!()` or free-form
    /// tag.  Empty string when no source was attached.
    source: &'static str,
}

// -- construction ----------------------------------------------------------

impl Error {
    /// Create an error with explicit kind and source tag.
    #[inline]
    pub const fn new(kind: ErrorKind, source: &'static str) -> Self {
        Self { kind, source }
    }

    /// Create an error from a kind alone (no source context).
    #[inline]
    pub const fn from_kind(kind: ErrorKind) -> Self {
        Self { kind, source: "" }
    }

    // Named constants that mirror the old `StorageError` variants so
    // existing match-arms keep compiling during migration.

    pub const NO_CARD: Self = Self::from_kind(ErrorKind::NoCard);
    pub const OPEN_VOLUME: Self = Self::from_kind(ErrorKind::OpenVolume);
    pub const OPEN_DIR: Self = Self::from_kind(ErrorKind::OpenDir);
    pub const OPEN_FILE: Self = Self::from_kind(ErrorKind::OpenFile);
    pub const READ_FAILED: Self = Self::from_kind(ErrorKind::ReadFailed);
    pub const WRITE_FAILED: Self = Self::from_kind(ErrorKind::WriteFailed);
    pub const SEEK_FAILED: Self = Self::from_kind(ErrorKind::SeekFailed);
    pub const DELETE_FAILED: Self = Self::from_kind(ErrorKind::DeleteFailed);
    pub const DIR_FULL: Self = Self::from_kind(ErrorKind::DirFull);
    pub const NOT_FOUND: Self = Self::from_kind(ErrorKind::NotFound);
}

// -- accessors -------------------------------------------------------------

impl Error {
    /// The failure category.
    #[inline]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The compile-time tag identifying where this error was created.
    /// Returns `""` when no source was attached.
    #[inline]
    pub const fn source_tag(&self) -> &'static str {
        self.source
    }

    /// Attach (or replace) the source tag.  Useful when propagating
    /// an error upward and adding the caller's context.
    #[inline]
    pub const fn with_source(self, source: &'static str) -> Self {
        Self {
            kind: self.kind,
            source,
        }
    }

    /// Change the kind while keeping the source.
    #[inline]
    pub const fn with_kind(self, kind: ErrorKind) -> Self {
        Self {
            kind,
            source: self.source,
        }
    }

    /// True when a source tag has been attached.
    #[inline]
    pub const fn has_source(&self) -> bool {
        !self.source.is_empty()
    }

    /// True when the error originates from storage I/O.
    #[inline]
    pub const fn is_storage(&self) -> bool {
        self.kind.is_storage()
    }

    /// Short label for the smol-epub `Result<T, &'static str>` boundary.
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        self.kind.as_str()
    }
}

// -- formatting ------------------------------------------------------------

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.source.is_empty() {
            write!(f, "Error({:?})", self.kind)
        } else {
            write!(f, "Error({:?} @ {:?})", self.kind, self.source)
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.source.is_empty() {
            f.write_str(self.kind.as_str())
        } else {
            write!(f, "{} [{}]", self.kind.as_str(), self.source)
        }
    }
}

// -- equality (semantic: kind only, source is diagnostic) ------------------

impl PartialEq for Error {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for Error {}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// Wrap a bare `&'static str` (from smol-epub helpers, etc.) into an
/// [`Error`].  Well-known strings are mapped to the appropriate kind;
/// everything else becomes [`ErrorKind::Other`] with the original
/// string preserved as the source tag.
impl From<&'static str> for Error {
    #[inline]
    fn from(msg: &'static str) -> Self {
        let kind = match msg {
            "read failed" | "read local header failed" => ErrorKind::ReadFailed,
            "write failed" => ErrorKind::WriteFailed,
            "read error" | "read error during upload" => ErrorKind::NetworkIo,
            "no sd card" => ErrorKind::NoCard,
            "not found" | "OPF not found" | "no filename in upload" => ErrorKind::NotFound,
            "too small" | "CD truncated" | "cache file too small" => ErrorKind::InvalidData,
            "CD too large" | "OOM for cached image" => ErrorKind::OutOfMemory,
            "bad OPF path" | "bad encoding" | "filename encoding error" => ErrorKind::BadEncoding,
            "parse failed" | "no title in OPF" => ErrorKind::ParseFailed,
            "boundary too long"
            | "part headers too large"
            | "invalid filename"
            | "upload incomplete"
            | "connection closed during headers" => ErrorKind::Protocol,
            _ => ErrorKind::Other,
        };
        Self { kind, source: msg }
    }
}

/// Project back to `&'static str` for the smol-epub trait boundary.
impl From<Error> for &'static str {
    #[inline]
    fn from(e: Error) -> &'static str {
        // Prefer the source tag if it is a meaningful human string;
        // otherwise fall back to the kind label.
        if e.source.is_empty() {
            e.kind.as_str()
        } else {
            e.source
        }
    }
}

// ---------------------------------------------------------------------------
// ResultExt — ergonomic source tagging on Results
// ---------------------------------------------------------------------------

/// Extension trait for stamping source context onto any
/// `Result<T, Error>`.
///
/// ```ignore
/// storage::read_file_chunk(sd, name, off, buf)
///     .source("epub_init_zip")?;
/// ```
pub trait ResultExt<T> {
    /// Attach a source tag to the error (if any).
    fn source(self, src: &'static str) -> Result<T>;

    /// Replace the error kind while adding a source tag.
    fn map_kind(self, kind: ErrorKind, src: &'static str) -> Result<T>;
}

impl<T> ResultExt<T> for Result<T> {
    #[inline]
    fn source(self, src: &'static str) -> Result<T> {
        self.map_err(|e| e.with_source(src))
    }

    #[inline]
    fn map_kind(self, kind: ErrorKind, src: &'static str) -> Result<T> {
        self.map_err(|_| Error::new(kind, src))
    }
}

/// Blanket impl so `Result<T, &'static str>` (smol-epub returns) can
/// be tagged and converted in one step.
impl<T> ResultExt<T> for core::result::Result<T, &'static str> {
    #[inline]
    fn source(self, src: &'static str) -> Result<T> {
        self.map_err(|msg| Error::from(msg).with_source(src))
    }

    #[inline]
    fn map_kind(self, kind: ErrorKind, src: &'static str) -> Result<T> {
        self.map_err(|_| Error::new(kind, src))
    }
}

// ---------------------------------------------------------------------------
// err! macro — stamps module_path!() automatically
// ---------------------------------------------------------------------------

/// Create an [`Error`] with the caller's module path baked in.
///
/// ```ignore
/// // Kind only — source is the calling module's path:
/// err!(ReadFailed)
///
/// // Kind + explicit context string:
/// err!(OpenFile, "epub_init_zip")
/// ```
#[macro_export]
macro_rules! err {
    ($kind:ident) => {
        $crate::error::Error::new($crate::error::ErrorKind::$kind, module_path!())
    };
    ($kind:ident, $src:expr) => {
        $crate::error::Error::new($crate::error::ErrorKind::$kind, $src)
    };
}

/// Map any `Result<T, _>` error into an [`Error`] of the given kind,
/// stamping the caller's module path.
///
/// ```ignore
/// mgr.read(file, buf).await.or_err!(ReadFailed)?;
/// ```
#[macro_export]
macro_rules! or_err {
    ($result:expr, $kind:ident) => {
        ($result)
            .map_err(|_| $crate::error::Error::new($crate::error::ErrorKind::$kind, module_path!()))
    };
    ($result:expr, $kind:ident, $src:expr) => {
        ($result).map_err(|_| $crate::error::Error::new($crate::error::ErrorKind::$kind, $src))
    };
}

// ---------------------------------------------------------------------------
// Result alias
// ---------------------------------------------------------------------------

/// Convenience alias used throughout pulp-os.
///
/// Intentionally shadows `core::result::Result` only when imported
/// unqualified — callers that need the two-param form can still write
/// `core::result::Result<T, E>`.
pub type Result<T> = core::result::Result<T, Error>;
