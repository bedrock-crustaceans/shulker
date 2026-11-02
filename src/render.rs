//! Diagnostics as rustc-style reports, on stderr.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use annotate_snippets::{AnnotationKind, Group, Level, Origin, Snippet};
use bedrock::addon::diagnostic::{Diagnostic, DiagnosticCode, JsonPath, Severity};
use bedrock::addon::pack::{InvalidPack, OpenError, Pack};
use bedrock::addon::storage::PackPath;

use crate::help;
use crate::load::{Loaded, display_path, host_file};
use crate::locate::{Located, locate};
use crate::names;
use crate::status::plural;

/// How many findings of each severity were printed.
#[derive(Debug, Default)]
pub struct Tally {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

/// A file a finding points into: how to show its name, and its text when it can be read.
#[derive(Clone)]
struct SourceFile {
    name: String,
    text: Option<String>,
}

impl SourceFile {
    /// The file `path` of a pack that did not open, read from its host folder `root`.
    fn on_host(root: Option<&Path>, path: &PackPath) -> Self {
        let host = root.map(|root| host_file(root, path));
        Self {
            name: host
                .as_deref()
                .map_or_else(|| path.to_string(), display_path),
            text: host.and_then(|host| std::fs::read_to_string(host).ok()),
        }
    }
}

/// The part of a file a finding marks.
struct Mark {
    span: Range<usize>,
    /// The member the finding's JSON path names that the file does not have.
    missing: Option<String>,
}

pub struct Renderer<'a> {
    loaded: &'a Loaded,
    min_severity: Severity,
    output: annotate_snippets::Renderer,
    /// Files already read, by host root and pack path.
    files: HashMap<(PathBuf, PackPath), SourceFile>,
    /// Reports already printed.
    seen: HashSet<String>,
    tally: Tally,
}

impl<'a> Renderer<'a> {
    pub fn new(loaded: &'a Loaded, min_severity: Severity) -> Self {
        Self {
            loaded,
            min_severity,
            output: annotate_snippets::Renderer::styled(),
            files: HashMap::new(),
            seen: HashSet::new(),
            tally: Tally::default(),
        }
    }

    /// Prints one finding. `pack` is the pack it is about when the finding does not say.
    pub fn diagnostic(&mut self, finding: &Diagnostic, pack: Option<&Pack>) {
        if finding.severity < self.min_severity {
            return;
        }
        // Vanilla's own files are not the author's to fix.
        if finding
            .location
            .pack
            .as_ref()
            .is_some_and(|pack| pack.is_app_data() || self.loaded.is_vanilla(pack.id()))
        {
            return;
        }
        if let Some((located, dependent)) = self.at_dependencies(finding) {
            return self.diagnostic(&located, Some(dependent));
        }
        let pack = pack.or_else(|| self.loaded.pack(finding.location.pack.as_ref()?.id()));
        let file = finding
            .location
            .path
            .as_ref()
            .map(|path| self.source_file(pack, path));
        let pack_name = match (&finding.location.pack, pack) {
            (Some(pack), _) => Some(self.loaded.name(pack.id(), pack.name())),
            (None, Some(pack)) => Some(self.loaded.pack_name(pack)),
            (None, None) => None,
        };
        let origin = match (&file, pack) {
            (None, Some(pack)) => Some(self.pack_dir(pack)),
            _ => None,
        };
        self.emit(finding, file, origin, pack_name);
    }

    /// A missing dependency names its pack but no place: the finding moved to the dependent's
    /// manifest list, and the dependent.
    fn at_dependencies(&self, finding: &Diagnostic) -> Option<(Diagnostic, &'a Pack)> {
        let DiagnosticCode::MissingDependency { pack, .. } = &finding.code else {
            return None;
        };
        if finding.location.path.is_some() {
            return None;
        }
        let dependent = self.loaded.pack(pack)?;
        let mut located = finding.clone();
        located.location.path = Some(PackPath::from_static("manifest.json"));
        located.location.json_path = JsonPath::of_keys(["dependencies"]);
        Some((located, dependent))
    }

    /// Prints a pack that could not be opened, and its manifest findings when it got that far.
    pub fn invalid_pack(&mut self, invalid: &InvalidPack) {
        let root = self.loaded.host_path(&invalid.location);
        self.unopened_pack(root.as_deref(), &invalid.error);
    }

    /// Prints a pack that did not open from the folder or archive at `root`.
    pub fn unopened_pack(&mut self, root: Option<&Path>, error: &OpenError) {
        let place = root.map_or_else(|| "<unknown>".to_owned(), display_path);
        if let OpenError::InvalidManifest { report, .. } = error {
            // The pack did not open, so its name is localized from the host folder.
            let lang = root.and_then(|root| {
                std::fs::read_to_string(root.join("texts").join("en_US.lang")).ok()
            });
            // A manifest too broken to read has no name; the file path names the pack then.
            let name = Some(names::display_name(&report.original_name, lang.as_deref()))
                .filter(|name| !name.is_empty());
            for finding in report.diagnostics.iter() {
                if finding.severity < self.min_severity {
                    continue;
                }
                let file = finding
                    .location
                    .path
                    .as_ref()
                    .map(|path| SourceFile::on_host(root, path));
                let origin = file.is_none().then(|| place.clone());
                self.emit(finding, file, origin, name.clone());
            }
            if report.diagnostics.has_errors() {
                return;
            }
        }
        let group = Group::with_title(
            Level::ERROR.primary_title(format!("cannot open pack: {}", open_error_text(error))),
        )
        .element(Origin::path(place.as_str()))
        .element(Level::HELP.message(open_error_help(error)));
        self.print(group, Severity::Error);
    }

    /// Renders one finding: its title and code, the file with the span marked (or just where it
    /// is), the JSON path, the pack, and what to do about it.
    fn emit(
        &mut self,
        finding: &Diagnostic,
        file: Option<SourceFile>,
        origin: Option<String>,
        pack_name: Option<String>,
    ) {
        let mut group = Group::with_title(
            level(finding.severity)
                .primary_title(title(finding))
                .id(finding.code.name()),
        );

        let text = file.as_ref().and_then(|file| file.text.as_deref());
        let mark = text.and_then(|text| mark(finding, text));
        let missing = mark.as_ref().and_then(|mark| mark.missing.as_deref());
        match (&file, text, &mark) {
            (Some(file), Some(text), Some(mark)) => {
                let mut annotation = AnnotationKind::Primary.span(mark.span.clone());
                if let Some(label) = help::label(finding, missing) {
                    annotation = annotation.label(label);
                }
                group = group.element(
                    Snippet::source(text)
                        .path(file.name.as_str())
                        .fold(true)
                        .annotation(annotation),
                );
            }
            (Some(file), ..) => group = group.element(Origin::path(file.name.as_str())),
            (None, ..) => {
                if let Some(origin) = &origin {
                    group = group.element(Origin::path(origin.as_str()));
                }
            }
        }

        let at = &finding.location.json_path;
        let notes = [
            (!at.is_empty()).then(|| format!("at `{at}`")),
            pack_name.map(|pack| format!("in pack `{pack}`")),
            help::where_shown(finding).map(str::to_owned),
        ];
        group = group
            .elements(
                notes
                    .into_iter()
                    .flatten()
                    .map(|note| Level::NOTE.message(note)),
            )
            .elements(
                help::hints(finding, missing)
                    .into_iter()
                    .map(|hint| Level::HELP.message(hint)),
            );
        self.print(group, finding.severity);
    }

    /// Prints a report once and counts it.
    fn print(&mut self, report: Group<'_>, severity: Severity) {
        let rendered = self.output.render(&[report]);
        // A finding can reach us through more than one report.
        if !self.seen.insert(rendered.clone()) {
            return;
        }
        anstream::eprintln!("{rendered}\n");
        match severity {
            Severity::Error => self.tally.errors += 1,
            Severity::Warning => self.tally.warnings += 1,
            _ => self.tally.infos += 1,
        }
    }

    /// Prints the cargo-style summary line and returns the counts.
    pub fn finish(self) -> Tally {
        let (level, summary) = summary(&self.tally, self.loaded.packs().count());
        let report = Group::with_title(level.primary_title(summary));
        anstream::eprintln!("{}", self.output.render(&[report]));
        self.tally
    }

    /// The file `path` of `pack`: its text from the pack, or from the host when only indexed.
    fn source_file(&mut self, pack: Option<&Pack>, path: &PackPath) -> SourceFile {
        let Some(pack) = pack else {
            return SourceFile {
                name: path.to_string(),
                text: None,
            };
        };
        let root = self.loaded.host_path(pack.location());
        let key = (root.clone().unwrap_or_default(), path.clone());
        if let Some(file) = self.files.get(&key) {
            return file.clone();
        }
        let name = match &root {
            Some(root) => display_path(&host_file(root, path)),
            None => format!("{}/{path}", self.loaded.pack_name(pack)),
        };
        let text = self.loaded.read_text(pack, path);
        let file = SourceFile { name, text };
        self.files.insert(key, file.clone());
        file
    }

    /// Where `pack` is on the host, or its name when it has no host folder.
    fn pack_dir(&self, pack: &Pack) -> String {
        match self.loaded.host_path(pack.location()) {
            Some(root) => display_path(&root),
            None => self.loaded.pack_name(pack),
        }
    }
}

/// Prints an error that stopped the run.
pub fn fatal(error: &anyhow::Error) {
    // `{:#}` joins the error with its causes.
    let group = Group::with_title(Level::ERROR.primary_title(format!("{error:#}")));
    anstream::eprintln!("{}", annotate_snippets::Renderer::styled().render(&[group]));
}

fn level(severity: Severity) -> Level<'static> {
    match severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
        Severity::Info => Level::INFO,
        _ => Level::NOTE,
    }
}

/// The library's message for a finding, with its detail.
fn title(finding: &Diagnostic) -> String {
    let title = match finding.detail.as_deref() {
        Some(detail) => format!("{}{detail}", finding.code),
        None => finding.code.to_string(),
    };
    title.trim().to_owned()
}

/// What to mark in `text`: the finding's span when the library has one, else where its JSON
/// path is in the text.
fn mark(finding: &Diagnostic, text: &str) -> Option<Mark> {
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

/// The summary line after all findings: its level and text.
fn summary(tally: &Tally, packs: usize) -> (Level<'static>, String) {
    let Tally {
        errors,
        warnings,
        infos,
    } = *tally;
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(plural(errors, "error"));
    }
    if warnings > 0 {
        parts.push(plural(warnings, "warning"));
    }
    if infos > 0 {
        parts.push(format!("{infos} info"));
    }
    let checked = format!("checked {}", plural(packs, "pack"));
    let summary = if parts.is_empty() {
        format!("{checked}: no problems found")
    } else {
        format!("{checked}: {}", parts.join(", "))
    };
    let level = if errors > 0 {
        Level::ERROR
    } else if warnings > 0 {
        Level::WARNING
    } else {
        Level::NOTE
    };
    (level, summary)
}

/// What went wrong, without the error's own location (we print the host path instead).
fn open_error_text(error: &OpenError) -> String {
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

fn open_error_help(error: &OpenError) -> String {
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

/// `offset..offset + len` in `text`, at least one character wide and on character boundaries.
fn byte_range(text: &str, offset: u32, len: u32) -> Range<usize> {
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
    fn the_summary_counts_each_severity() {
        let tally = Tally {
            errors: 1,
            warnings: 2,
            infos: 3,
        };
        assert_eq!(
            summary(&tally, 2).1,
            "checked 2 packs: 1 error, 2 warnings, 3 info"
        );
        assert_eq!(
            summary(&Tally::default(), 1).1,
            "checked 1 pack: no problems found"
        );
    }
}
