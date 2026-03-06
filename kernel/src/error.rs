// unified error type for pulp-os
//
// single Copy type carrying ErrorKind (what) and a &'static str
// source tag (where); smol-epub boundary converts via From impls

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    // storage / sd card
    NoCard,
    OpenVolume,
    OpenDir,
    OpenFile,
    ReadFailed,
    WriteFailed,
    SeekFailed,
    DeleteFailed,
    DirFull,
    NotFound,

    // data / parsing
    ParseFailed,
    InvalidData,
    BadEncoding,

    // resources
    OutOfMemory,
    BufferTooSmall,

    // network (upload)
    NetworkIo,
    Protocol,

    // catch-all
    Other,
}

impl ErrorKind {
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

// one discriminant byte + one &'static str pointer; cheap to copy
// source is module_path!() or a short caller-supplied tag
#[derive(Clone, Copy)]
pub struct Error {
    kind: ErrorKind,
    source: &'static str,
}

impl Error {
    #[inline]
    pub const fn new(kind: ErrorKind, source: &'static str) -> Self {
        Self { kind, source }
    }

    #[inline]
    pub const fn from_kind(kind: ErrorKind) -> Self {
        Self { kind, source: "" }
    }

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

impl Error {
    #[inline]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[inline]
    pub const fn source_tag(&self) -> &'static str {
        self.source
    }

    #[inline]
    pub const fn with_source(self, source: &'static str) -> Self {
        Self {
            kind: self.kind,
            source,
        }
    }

    #[inline]
    pub const fn with_kind(self, kind: ErrorKind) -> Self {
        Self {
            kind,
            source: self.source,
        }
    }

    #[inline]
    pub const fn has_source(&self) -> bool {
        !self.source.is_empty()
    }

    #[inline]
    pub const fn is_storage(&self) -> bool {
        self.kind.is_storage()
    }

    #[inline]
    pub const fn as_str(&self) -> &'static str {
        self.kind.as_str()
    }
}

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

// equality is semantic: kind only, source is diagnostic
impl PartialEq for Error {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for Error {}

// Map &'static str error strings from smol-epub into typed ErrorKind.
//
// smol-epub (and the upload module) return &'static str errors across
// the crate boundary.  Each known string is matched to an ErrorKind;
// unrecognised strings fall through to ErrorKind::Other silently.
//
// IMPORTANT: if smol-epub changes or adds error messages, update this
// match to keep the mapping correct.  Run `rg 'Err\("' ../smol-epub`
// to audit the current set.
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

// project back to &'static str for the smol-epub trait boundary
impl From<Error> for &'static str {
    #[inline]
    fn from(e: Error) -> &'static str {
        if e.source.is_empty() {
            e.kind.as_str()
        } else {
            e.source
        }
    }
}

// ergonomic source tagging on Result<T, Error> and
// Result<T, &'static str> (smol-epub returns)
pub trait ResultExt<T> {
    fn source(self, src: &'static str) -> Result<T>;
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

// create an Error with module_path!() as source
//   err!(ReadFailed)
//   err!(OpenFile, "epub_init_zip")
#[macro_export]
macro_rules! err {
    ($kind:ident) => {
        $crate::error::Error::new($crate::error::ErrorKind::$kind, module_path!())
    };
    ($kind:ident, $src:expr) => {
        $crate::error::Error::new($crate::error::ErrorKind::$kind, $src)
    };
}

// map any Result<T, _> into an Error of the given kind
//   or_err!(mgr.read(file, buf).await, ReadFailed)
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

pub type Result<T> = core::result::Result<T, Error>;
