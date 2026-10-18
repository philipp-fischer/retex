//! Source-aware typed failures (`core/errors.py`).
use super::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    Value,
    Type,
    Runtime,
    Io,
    Eof,
    Interrupted,
    Processing,
}

impl ErrorKind {
    pub fn exception_type(&self) -> &'static str {
        match self {
            Self::Value => "ValueError",
            Self::Type => "TypeError",
            Self::Runtime => "RuntimeError",
            Self::Io => "OSError",
            Self::Eof => "EOFError",
            Self::Interrupted => "KeyboardInterrupt",
            Self::Processing => "ProcessingError",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    #[serde(default = "error_severity")]
    pub severity: String,
    pub message: String,
    pub phase: String,
    pub exception_type: String,
    pub source: Option<Arc<SourceSpan>>,
    pub cause_type: Option<String>,
    pub cause_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Arc<super::recovery::DiagnosticDetails>>,
}
fn error_severity() -> String {
    "error".into()
}

/// A required physical input was absent after the normal ordered search.
/// Optional probes and policy-denied paths do not create requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissingResource {
    pub kind: String,
    pub name: Vec<u8>,
    /// Source directory only when the actual lookup includes it.
    pub directory: Option<Vec<u8>>,
}
#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub phase: Option<String>,
    pub source: Option<Arc<SourceSpan>>,
    pub cause: Option<Box<Error>>,
    pub missing_resource: Option<MissingResource>,
    pub details: Option<Arc<super::recovery::DiagnosticDetails>>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            phase: None,
            source: None,
            cause: None,
            missing_resource: None,
            details: None,
        }
    }
    pub fn value(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Value, message)
    }
    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message)
    }
    pub fn with_issue(self, issue: super::recovery::Issue) -> Self {
        self.with_details(super::recovery::DiagnosticDetails::new(issue, None))
    }
    pub fn with_details(mut self, details: super::recovery::DiagnosticDetails) -> Self {
        self.details = Some(Arc::new(details));
        self
    }
    pub fn with_missing_resource(mut self, resource: MissingResource) -> Self {
        self.missing_resource = Some(resource);
        self
    }
    pub fn capture(self, phase: impl Into<String>, source: Option<Arc<SourceSpan>>) -> Self {
        if self.kind == ErrorKind::Processing {
            return self;
        }
        let message = if self.message.is_empty() {
            if self.kind == ErrorKind::Interrupted {
                "Interrupted".into()
            } else {
                self.kind.exception_type().into()
            }
        } else {
            self.message.clone()
        };
        Self {
            kind: ErrorKind::Processing,
            message,
            phase: Some(phase.into()),
            source,
            missing_resource: self.missing_resource.clone(),
            details: self.details.clone(),
            cause: Some(Box::new(self)),
        }
    }
    pub fn diagnostic(&self) -> Diagnostic {
        Diagnostic {
            severity: error_severity(),
            message: self.message.clone(),
            phase: self.phase.clone().unwrap_or_default(),
            exception_type: self.kind.exception_type().into(),
            source: self.source.clone(),
            cause_type: self.cause.as_ref().map(|c| c.kind.exception_type().into()),
            cause_message: self.cause.as_ref().map(|c| c.message.clone()),
            details: self.details.clone(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref().map(|e| e as &dyn std::error::Error)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, error.to_string())
    }
}
