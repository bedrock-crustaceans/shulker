//! Pure diagnostic helpers shared between the CLI renderer and the WASM frontend.

use std::ops::Range;

use bedrock::addon::diagnostic::{Diagnostic, DiagnosticCode};
use bedrock::addon::pack::OpenError;

use crate::help;
use crate::locate::{Located, locate};

/// The part of a file a finding marks.
pub struct Mark {
    pub span: Range<usize>,
    /// The member the finding's JSON path names that the file does not have.
    pub missing: Option<String>,
}

/// The stack asks for the game's own packs (the base-game layer), which are not installed here:
/// their absence is no finding about the packs being checked.
pub fn is_base_game_absent(finding: &Diagnostic) -> bool {
    matches!(finding.code, DiagnosticCode::PackNotFound { .. }) && finding.location.path.is_none()
}

/// The library's message for a finding, with its detail.
pub fn title(finding: &Diagnostic) -> String {
    let title = match finding.detail.as_deref() {
        Some(detail) => format!("{}{detail}", finding.code),
        None => finding.code.to_string(),
    };
    title.trim().to_owned()
}

/// What to mark in `text`: the finding's span when the library has one, else where its JSON
/// path is in the text.
pub fn mark(finding: &Diagnostic, text: &str) -> Option<Mark> {
    if let Some(span) = finding.location.span {
        return Some(Mark {
            span: byte_range(text, span.offset, span.len),
            missing: None,
        });
    }
    if finding.location.json_path.is_empty() {
        return None;
    }
    let located = locate(text, &finding.location.json_path)?;
    let span = located.span(text, help::marks_value(&finding.code));
    let missing = match located {
        Located::Missing { name, .. } => Some(name),
        Located::Found { .. } => None,
    };
    Some(Mark { span, missing })
}

/// `offset..offset + len` in `text`, at least one character wide and on character boundaries.
pub fn byte_range(text: &str, offset: u32, len: u32) -> Range<usize> {
    let mut start = (offset as usize).min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start.saturating_add((len as usize).max(1)).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    start..end
}

/// 1-based `(line, column)` (in Unicode scalar values) for `byte_offset` in `text`.
pub fn line_col(text: &str, byte_offset: usize) -> (usize, usize) {
    let mut offset = byte_offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |pos| pos + 1);
    let col = text[line_start..offset].chars().count() + 1;
    (line, col)
}

/// What went wrong, without the error's own location (the host path is shown instead).
pub fn open_error_text(error: &OpenError) -> String {
    match error {
        OpenError::Unreadable { kind, .. } => format!("cannot read it ({kind})"),
        OpenError::NoManifest { .. } => "no `manifest.json` found".into(),
        OpenError::MultipleManifests { count, .. } => {
            format!("{count} `manifest.json` files at the same depth")
        }
        OpenError::InvalidManifest { .. } => "invalid `manifest.json`".into(),
        _ => error.to_string(),
    }
}

/// Help message for an [`OpenError`].
pub fn open_error_help(error: &OpenError) -> String {
    match error {
        OpenError::NoManifest { .. } => {
            "every pack needs a `manifest.json` at its root (or in its single subfolder)".into()
        }
        OpenError::MultipleManifests { .. } => {
            "keep one pack per folder: move each `manifest.json` and its files into a folder of its own".into()
        }
        OpenError::InvalidManifest { .. } => "fix the errors in `manifest.json` above".into(),
        _ => "check that the file is a readable pack archive or folder".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_range_is_widened_onto_character_boundaries() {
        let text = "a§b";
        // `§` is two bytes; a span starting in its middle covers all of it.
        assert_eq!(byte_range(text, 2, 1), 1..3);
        assert_eq!(byte_range(text, 0, 0), 0..1);
        assert_eq!(byte_range(text, 9, 4), 4..4);
    }

    #[test]
    fn line_col_counts_one_based_lines_and_characters() {
        let text = "first\n§abc\nthird";
        assert_eq!(line_col(text, 0), (1, 1));
        assert_eq!(line_col(text, 6), (2, 1));
        // `§` is 2 bytes, so byte 8 is `'a'` (column 2 on line 2).
        assert_eq!(line_col(text, 8), (2, 2));
        // Mid-character byte index backs up to the character start.
        assert_eq!(line_col(text, 7), (2, 1));
        assert_eq!(line_col(text, 100), (3, 6));
    }
}
