//! What to tell the author about a finding: a short label under the marked span, and `help:`
//! lines saying what to change.
//!
//! `DiagnosticCode` is `#[non_exhaustive]` and has well over a hundred variants; the common ones
//! get advice here, the rest print the library's message alone.

use bedrock::addon::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticDetail, ReferenceHint, Severity, VanillaText,
};
use bedrock::addon::families::json_ui::UiPackErrorKind;

/// The JSON UI errors the game puts in the pack's report, not the content log.
const UI_PACK_ERRORS: &[UiPackErrorKind] = &[
    UiPackErrorKind::InvalidChildNames,
    UiPackErrorKind::ParseError,
    UiPackErrorKind::MissingControl,
    UiPackErrorKind::MissingControlTarget,
    UiPackErrorKind::MissingArrayName,
    UiPackErrorKind::MissingCondition,
    UiPackErrorKind::MissingValue,
    UiPackErrorKind::MissingOperation,
    UiPackErrorKind::InvalidOperationName,
];

/// The kind of `uiPackError` a finding is, matched by the game text it carries.
fn ui_pack_error(finding: &Diagnostic) -> Option<UiPackErrorKind> {
    let Some(VanillaText::Formatted { template, .. }) = finding.code.game_text() else {
        return None;
    };
    UI_PACK_ERRORS
        .iter()
        .copied()
        .find(|kind| kind.template() == *template)
}

/// Where the game shows a finding, when that is not its content log.
pub fn where_shown(finding: &Diagnostic) -> Option<&'static str> {
    if ui_pack_error(finding).is_some() {
        return Some(
            "the game shows this in the pack's warnings on the resource pack screen, \
             not in the content log",
        );
    }
    if finding.severity <= Severity::Info {
        return Some(
            "the game does not report this: a lint of this tool, or a message the game logs \
             only in some situations",
        );
    }
    None
}

/// The label under the marked span. `missing` is the member the finding's JSON path names that
/// the file does not have.
pub fn label(finding: &Diagnostic, missing: Option<&str>) -> Option<String> {
    if let Some(name) = missing {
        return Some(format!("`{name}` missing in here"));
    }
    if let Some(DiagnosticDetail::Reference { kind, id, .. }) = finding.detail.as_deref() {
        return Some(format!("no {kind} `{id}`"));
    }
    let label = match &finding.code {
        DiagnosticCode::UnrecognizedField { .. } => "unknown member",
        DiagnosticCode::UnrecognizedComponent { .. } => "unknown component",
        DiagnosticCode::MissingRequiredField { .. } => "required member missing here",
        DiagnosticCode::InvalidType { .. } => "wrong type",
        DiagnosticCode::InvalidValue { .. } | DiagnosticCode::InvalidEnumValue { .. } => {
            "invalid value"
        }
        DiagnosticCode::DeprecatedMember { .. } => "deprecated",
        _ => return None,
    };
    Some(label.into())
}

/// The `help:` lines of a finding.
pub fn hints(finding: &Diagnostic, missing: Option<&str>) -> Vec<String> {
    let mut hints = Vec::new();
    if let Some(hint) = detail_hint(finding) {
        hints.push(hint);
    }
    if let Some(name) = missing {
        hints.push(format!("add the member `{name}` to this object"));
    } else if let Some(hint) = code_hint(finding) {
        hints.push(hint);
    }
    hints
}

/// Whether a finding is about a member's value rather than its name, so the value is marked.
pub fn marks_value(code: &DiagnosticCode) -> bool {
    matches!(
        code,
        DiagnosticCode::InvalidType { .. }
            | DiagnosticCode::InvalidValue { .. }
            | DiagnosticCode::InvalidEnumValue { .. }
            | DiagnosticCode::ClampedValue { .. }
            | DiagnosticCode::ValueRewritten { .. }
            | DiagnosticCode::UnresolvedReference { .. }
            | DiagnosticCode::InvalidReference { .. }
            | DiagnosticCode::TextureNotFound { .. }
            | DiagnosticCode::InvalidMolang { .. }
            | DiagnosticCode::MolangLint { .. }
            | DiagnosticCode::UnsupportedVersion { .. }
            | DiagnosticCode::MalformedFormatVersion { .. }
            | DiagnosticCode::FormatVersionAboveEngine { .. }
            | DiagnosticCode::NamespaceRule { .. }
    )
}

fn detail_hint(finding: &Diagnostic) -> Option<String> {
    let DiagnosticDetail::Reference { kind, id, hint } = finding.detail.as_deref()? else {
        return None;
    };
    Some(match hint {
        Some(ReferenceHint::CaseMismatch(target)) => {
            format!("names are case-sensitive: a {kind} `{target}` exists, write it like that")
        }
        Some(ReferenceHint::MissingNamespace) => {
            format!("add the namespace the {kind} is defined with, e.g. `namespace:{id}`")
        }
        Some(ReferenceHint::Shadowed) => format!(
            "a higher-priority pack defines its own {kind} `{id}` that replaces this one; \
             rename yours or reorder the packs"
        ),
        _ => format!(
            "define a {kind} `{id}` in one of the packs, or correct the name if it is a typo"
        ),
    })
}

fn code_hint(finding: &Diagnostic) -> Option<String> {
    if ui_pack_error(finding) == Some(UiPackErrorKind::InvalidChildNames) {
        return Some(
            "give every child control its own name before the `@` \
             (`\"z_text@hud.z_coordinates_text\"`): children written as `\"@namespace.base\"` \
             all have the empty name"
                .into(),
        );
    }
    let at = &finding.location.json_path;
    // Advice about "this member" needs a member: a finding about the whole document (often the
    // game's own multi-line text) gets none.
    let about_a_member = matches!(
        finding.code,
        DiagnosticCode::UnrecognizedField { .. }
            | DiagnosticCode::UnrecognizedComponent { .. }
            | DiagnosticCode::InvalidType { .. }
            | DiagnosticCode::InvalidValue { .. }
            | DiagnosticCode::InvalidEnumValue { .. }
            | DiagnosticCode::ClampedValue { .. }
            | DiagnosticCode::NonCanonicalMemberCase
    );
    if about_a_member && at.is_empty() {
        return None;
    }
    let member = at.last_key().map(|key| format!("`{key}`"));
    let member = member.as_deref().unwrap_or("this member");
    Some(match &finding.code {
        DiagnosticCode::UnrecognizedField { .. } => format!(
            "remove {member}, or check its spelling; members can also be added or renamed \
             between `format_version`s"
        ),
        DiagnosticCode::UnrecognizedComponent { .. } => format!(
            "remove {member}, or check its spelling; some components need a newer \
             `format_version` or an experimental toggle"
        ),
        DiagnosticCode::MissingRequiredField { .. } => {
            "add the missing member; the message above names it".into()
        }
        DiagnosticCode::InvalidType { .. } => {
            format!("change {member} to the JSON type it expects (string, number, object, …)")
        }
        DiagnosticCode::InvalidValue { .. } | DiagnosticCode::InvalidEnumValue { .. } => {
            format!("change {member} to one of the values it accepts")
        }
        DiagnosticCode::ClampedValue { .. } => {
            format!("the game clamps {member} into its range; write a value inside it")
        }
        DiagnosticCode::DeprecatedMember { .. } => {
            "replace it with its current form for this `format_version`".into()
        }
        DiagnosticCode::NonCanonicalMemberCase => {
            format!("write {member} in the case the game documents")
        }
        DiagnosticCode::UnsupportedVersion { .. }
        | DiagnosticCode::MalformedFormatVersion { .. }
        | DiagnosticCode::VanillaRejectsVersion => {
            "set `format_version` to a released game version this document type supports, \
             like `\"1.21.0\"`"
                .into()
        }
        DiagnosticCode::MissingFormatVersion { .. } => {
            "add a `format_version` member at the top of the document, like \
             `\"format_version\": \"1.21.0\"`"
                .into()
        }
        DiagnosticCode::FormatVersionAboveEngine { .. } => {
            "lower `format_version` to a game version that has been released".into()
        }
        DiagnosticCode::BetaFeaturesNotEnabled { .. } | DiagnosticCode::MissingToggles { .. } => {
            "turn on the named experiment in the world settings, or avoid the feature".into()
        }
        DiagnosticCode::MalformedInput { .. } => {
            "fix the JSON syntax at the marked position (look for a missing comma, quote or \
             bracket, or a trailing comma)"
                .into()
        }
        DiagnosticCode::EmptyDocument { .. } => "fill in the document or delete the file".into(),
        DiagnosticCode::MissingIdentifier { .. } => {
            "add `description.identifier`, like `\"identifier\": \"namespace:name\"`".into()
        }
        DiagnosticCode::NamespaceRule { .. } => {
            "use a namespace of your own (`myaddon:name`); `minecraft:` is reserved for the game"
                .into()
        }
        DiagnosticCode::DuplicateIdentifier { .. } | DiagnosticCode::IdentifierCollision { .. } => {
            "give each definition its own identifier; the later one replaces the earlier".into()
        }
        DiagnosticCode::TextureNotFound { .. } => {
            "add the image, or fix the path: relative to the pack root, without the file \
             extension (`textures/blocks/ruby`)"
                .into()
        }
        DiagnosticCode::PackIconMissing => {
            "add a square `pack_icon.png` next to `manifest.json`".into()
        }
        DiagnosticCode::PackIconShape => {
            "make `pack_icon.png` a square PNG (256×256 is common)".into()
        }
        DiagnosticCode::MissingDependency { dependency, .. } => format!(
            "pass the pack `{}` version {} as another path to check them together, or remove \
             the dependency from `manifest.json`",
            dependency.uuid, dependency.version
        ),
        DiagnosticCode::MissingMinEngineVersion => {
            "add `\"min_engine_version\": [1, 21, 0]` (or the version you target) to the \
             manifest `header`"
                .into()
        }
        DiagnosticCode::MissingModules => {
            "add a module to `modules`: `\"type\": \"data\"` for a behaviour pack, \
             `\"resources\"` for a resource pack"
                .into()
        }
        DiagnosticCode::DuplicateUuid => {
            "every pack and module needs a UUID of its own; generate a fresh one".into()
        }
        DiagnosticCode::MalformedPropertyUuid => {
            "write a UUID like `3b2c7a52-8f0e-4b7d-9a35-1c6e5d0f4a11`".into()
        }
        DiagnosticCode::InvalidMolang { .. } => {
            "fix the Molang expression; the compiler's message is in the title".into()
        }
        DiagnosticCode::ReferenceCycle { .. } => {
            "break the cycle: a definition cannot (indirectly) refer to itself".into()
        }
        DiagnosticCode::UnresolvedLangKey => {
            "add the key to `texts/en_US.lang` (and the other languages you ship)".into()
        }
        DiagnosticCode::LangKeyTrailingSpace => {
            "remove the whitespace at the end of the key".into()
        }
        _ => return None,
    })
}
