//! Custom error types and `From` impls — exercises trait-impl edges for
//! the standard-library `From` trait, the `?` operator, and `Result`
//! propagation.
//!
//! Symbol contract for `errors.rs` (asserted by `MANIFEST.md`):
//!   Structs: `IoErrorWrapper` (1)
//!   Enums:   `AppError` (1)
//!   Methods (in `impl` blocks):
//!       `AppError::message`,                 (impl AppError)
//!       `AppError::from` (1) [impl From<std::io::Error> for AppError]
//!       `AppError::from` (2) [impl From<IoErrorWrapper> for AppError]
//!                                            (3 method symbols)
//!   Functions:
//!       `read_or_fail`,
//!       `q_propagation`                      (2)
//!   Inheritance edges:
//!       AppError -> From<std::io::Error>,
//!       AppError -> From<IoErrorWrapper>     (2)

use std::fmt;

#[derive(Debug)]
pub enum AppError {
    Io(String),
    Parse(String),
}

impl AppError {
    pub fn message(&self) -> &str {
        match self {
            AppError::Io(m) => m,
            AppError::Parse(m) => m,
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

#[derive(Debug)]
pub struct IoErrorWrapper(pub String);

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> AppError {
        AppError::Io(format!("{err}"))
    }
}

impl From<IoErrorWrapper> for AppError {
    fn from(w: IoErrorWrapper) -> AppError {
        AppError::Io(w.0)
    }
}

pub fn read_or_fail(ok: bool) -> Result<(), AppError> {
    if ok {
        Ok(())
    } else {
        Err(AppError::Parse(String::from("nope")))
    }
}

/// Exercises the `?` operator — propagates an `AppError` from `read_or_fail`.
pub fn q_propagation() -> Result<(), AppError> {
    read_or_fail(true)?;
    Ok(())
}
