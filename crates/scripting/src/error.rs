//! Error type for the scripting bridge.

use std::fmt;

use rhai::{EvalAltResult, ParseError};

/// Errors from loading, compiling, or reloading a script.
#[derive(Debug)]
pub enum ScriptError {
    /// The script failed to compile — syntax, a disabled symbol (e.g. `eval`),
    /// or the parse-time expression-depth limit.
    Parse(ParseError),
    /// A script evaluation error surfaced on a load/reload path.
    Eval(Box<EvalAltResult>),
    /// Reading the script file failed.
    Io(std::io::Error),
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::Parse(e) => write!(f, "script parse error: {e}"),
            ScriptError::Eval(e) => write!(f, "script evaluation error: {e}"),
            ScriptError::Io(e) => write!(f, "script io error: {e}"),
        }
    }
}

impl std::error::Error for ScriptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScriptError::Parse(e) => Some(e),
            ScriptError::Eval(e) => Some(e.as_ref()),
            ScriptError::Io(e) => Some(e),
        }
    }
}

impl From<ParseError> for ScriptError {
    fn from(e: ParseError) -> Self {
        ScriptError::Parse(e)
    }
}

impl From<Box<EvalAltResult>> for ScriptError {
    fn from(e: Box<EvalAltResult>) -> Self {
        ScriptError::Eval(e)
    }
}

impl From<std::io::Error> for ScriptError {
    fn from(e: std::io::Error) -> Self {
        ScriptError::Io(e)
    }
}
