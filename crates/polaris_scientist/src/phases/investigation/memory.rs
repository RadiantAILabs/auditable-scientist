//! Validated distilled conclusions and hash-gated shared memory publication.

use crate::eval::{EvalError, file_digest};
use crate::phases::task::files::atomic_write;
use std::{fs, path::Path};

/// Reject multiple paragraphs or more than 300 whitespace-delimited words.
pub fn validate_conclusions(text: &str) -> Result<(), EvalError> {
    if text.split_whitespace().count() > 300 {
        return Err(EvalError("conclusions exceed 300 words".into()));
    }
    let mut previous_blank = false;
    for line in text.trim().lines() {
        if line.trim().is_empty() {
            previous_blank = true;
        } else if previous_blank {
            return Err(EvalError("conclusions must be one paragraph".into()));
        }
    }
    Ok(())
}
/// Publish only changed content; invalid conclusions leave the prior text intact.
pub fn publish(source: &Path, destination: &Path, conclusions: bool) -> Result<bool, EvalError> {
    if !source.exists() {
        return Ok(false);
    }
    let bytes = fs::read(source)?;
    if conclusions {
        validate_conclusions(std::str::from_utf8(&bytes).map_err(|e| EvalError(e.to_string()))?)?;
    }
    if destination.exists() && file_digest(source)? == file_digest(destination)? {
        return Ok(false);
    }
    atomic_write(destination, &bytes)?;
    Ok(true)
}

/// Append changed memory content without replacing earlier evidence.
pub fn append_new(source: &Path, destination: &Path) -> Result<bool, EvalError> {
    if !source.exists() {
        return Ok(false);
    }
    let incoming = fs::read(source)?;
    if incoming.is_empty() {
        return Ok(false);
    }
    let mut previous = if destination.exists() {
        fs::read(destination)?
    } else {
        Vec::new()
    };
    if previous.ends_with(&incoming) {
        return Ok(false);
    }
    if incoming.starts_with(&previous) {
        return atomic_write(destination, &incoming).map(|()| true);
    }
    if !previous.is_empty() && !previous.ends_with(b"\n") {
        previous.push(b'\n');
    }
    previous.extend(incoming);
    atomic_write(destination, &previous)?;
    Ok(true)
}
