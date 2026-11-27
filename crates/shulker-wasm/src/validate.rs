//! Core validation pipeline for in-memory archives, directories, worlds, and optional vanilla data.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use annotate_snippets::{AnnotationKind, Group, Level, Origin, Snippet};
use bedrock::addon::diagnostic::{Diagnostic, DiagnosticCode, JsonPath, Severity};
use bedrock::addon::load::{
    AppData, LoadOptions, PackLoader, Place, RepositoryLoader, RootId, Source, StackLoader,
    StackOptions,
};
use bedrock::addon::molang::MolangCheck;
use bedrock::addon::pack::{
    InvalidPack, LinkOptions, OpenError, OpenOptions, Pack, PackIdVersion, PackOrigin,
    PackRepository, PackSelection, PackType, StackSet, WorldPacks,
};
use bedrock::addon::storage::PackPath;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use shulker::diag::{
    is_base_game_absent, line_col, mark, open_error_help, open_error_text, title,
};
use shulker::help;
use shulker::names;

use crate::memfs::MemFs;

#[derive(Debug, Clone)]
pub struct ArchiveInput {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DirInput {
    pub name: String,
    pub files: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ValidateOptions {
    pub min_severity: String,
    pub no_link: bool,
    pub no_lints: bool,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self {
            min_severity: "warning".to_owned(),
            no_link: false,
            no_lints: false,
        }
    }
}

impl ValidateOptions {
    fn severity(&self) -> Severity {
        match self.min_severity.to_ascii_lowercase().as_str() {
            "debug" => Severity::Debug,
            "info" => Severity::Info,
            "error" => Severity::Error,
            _ => Severity::Warning,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Tally {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackSummary {
    pub name: String,
    pub version: String,
    pub uuid: String,
    pub pack_type: &'static str,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticItem {
    pub severity: &'static str,
    pub code: String,
    pub title: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub json_path: Option<String>,
    pub pack_name: Option<String>,
    pub where_shown: Option<String>,
    pub hints: Vec<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationResult {
    pub ok: bool,
    pub fatal_error: Option<String>,
    pub packs: Vec<PackSummary>,
    pub vanilla_pack_count: usize,
    pub tally: Tally,
    pub diagnostics: Vec<DiagnosticItem>,
    pub plain_report: String,
}

impl ValidationResult {
    fn fatal(message: impl Into<String>) -> Self {
        let message = message.into();
        let group = Group::with_title(Level::ERROR.primary_title(message.as_str()));
        let plain_report = annotate_snippets::Renderer::plain().render(&[group]);
        Self {
            ok: false,
            fatal_error: Some(message),
            packs: Vec::new(),
            vanilla_pack_count: 0,
            tally: Tally::default(),
            diagnostics: Vec::new(),
            plain_report,
        }
    }
}

/// Normalizes a JS-provided relative file path into a [`PackPath`].
pub fn normalize_pack_path(raw: &str) -> Option<PackPath> {
    let mut normalized = raw.replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_owned();
    }
    let trimmed = normalized.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    PackPath::new(trimmed).ok()
}

pub fn run_validation(
    archives: &[ArchiveInput],
    dirs: &[DirInput],
    vanilla_files: &[(String, Vec<u8>)],
    options: &ValidateOptions,
) -> ValidationResult {
    match run_inner(archives, dirs, vanilla_files, options) {
        Ok(result) => result,
        Err(message) => ValidationResult::fatal(message),
    }
}

struct WorldSelection {
    packs: WorldPacks,
    roots: Vec<RootId>,
}

fn run_inner(
    archives: &[ArchiveInput],
    dirs: &[DirInput],
    vanilla_files: &[(String, Vec<u8>)],
    options: &ValidateOptions,
) -> Result<ValidationResult, String> {
    let mut memfs = MemFs::new();
    let mut sources: Vec<Source> = Vec::new();
    let mut pack_dirs: Vec<RootId> = Vec::new();
    let mut world: Option<WorldSelection> = None;

    for archive in archives {
        let root = memfs.add_archive(archive.name.clone(), archive.bytes.clone());
        sources.push(Source::Archive {
            root,
            origin: PackOrigin::World,
        });
    }

    for dir in dirs {
        let files = collect_dir_files(&dir.files);
        if is_single_pack(&files) {
            let root = memfs.add_dir(dir.name.clone(), files);
            pack_dirs.push(root);
        } else if is_world_dir(&files) {
            if world.is_some() {
                return Err("Only one world folder can be checked at a time.".to_owned());
            }
            let parsed_world = parse_world_packs(&files)?;
            let mut roots = Vec::new();
            for sub in ["behavior_packs", "resource_packs"] {
                let sub_files = extract_subtree(&files, sub);
                if !sub_files.is_empty() {
                    let root = memfs.add_dir(format!("{}/{sub}", dir.name), sub_files);
                    roots.push(root);
                    sources.push(Source::Directory {
                        root,
                        origin: PackOrigin::World,
                    });
                }
            }
            world = Some(WorldSelection {
                packs: parsed_world,
                roots,
            });
        } else {
            let root = memfs.add_dir(dir.name.clone(), files);
            sources.push(Source::Directory {
                root,
                origin: PackOrigin::World,
            });
        }
    }

    let mut repository: PackRepository =
        memfs.drive(RepositoryLoader::new(sources, OpenOptions::default()))?;

    let mut unopened: Vec<(Place, OpenError)> = Vec::new();
    for root in pack_dirs {
        match memfs.drive(PackLoader::directory(root, OpenOptions::default()))? {
            Ok(pack) => repository.add_pack(pack, PackOrigin::World),
            Err(error) => unopened.push((Place::root(root), error)),
        }
    }

    if repository.packs().next().is_none()
        && repository.invalid().is_empty()
        && unopened.is_empty()
    {
        return Err(
            "No Minecraft Bedrock pack found in the selected files or folders.".to_owned(),
        );
    }

    let mut selection = match world {
        None => every_pack(repository.packs()),
        Some(world) => world_selection(&world, &repository),
    };

    let names = resolve_pack_names(&memfs, &repository);

    let mut vanilla_ids: HashSet<PackIdVersion> = HashSet::new();
    let mut app_data: Option<AppData> = None;
    if !vanilla_files.is_empty() {
        let vanilla = open_vanilla(vanilla_files);
        selection
            .resource
            .extend(vanilla.resource_layer.iter().map(select));
        for pack in vanilla.packs {
            vanilla_ids.insert(pack.id().clone());
            repository.add_pack(pack, PackOrigin::Package);
        }
        app_data = Some(vanilla.app_data);
    }

    let load_options = LoadOptions {
        molang: Some(Arc::new(MolangCheck::new())),
        app_data,
        ..LoadOptions::default()
    };
    let stacks = repository.stacks(&selection, load_options);
    let stacks: StackSet = memfs.drive(StackLoader::new(stacks, StackOptions::default()))?;

    let links = (!options.no_link).then(|| {
        let link_options = LinkOptions {
            lints: !options.no_lints,
            ..Default::default()
        };
        stacks.link_with(&link_options)
    });

    let pack_summaries = repository
        .packs()
        .filter(|pack| !vanilla_ids.contains(pack.id()))
        .map(|pack| {
            let name = names
                .get(pack.id())
                .cloned()
                .unwrap_or_else(|| names::display_name(&pack.manifest().header.name, None));
            let source = memfs
                .display_place(pack.location())
                .unwrap_or_else(|| "in memory".to_owned());
            PackSummary {
                name,
                version: pack.id().version.to_string(),
                uuid: pack.id().uuid.to_string(),
                pack_type: match pack.pack_type() {
                    PackType::Behavior => "behavior",
                    PackType::Resource => "resource",
                    _ => "other",
                },
                source,
            }
        })
        .collect::<Vec<_>>();

    let mut collector = Collector::new(
        &memfs,
        &repository,
        &vanilla_ids,
        &names,
        options.severity(),
    );

    for invalid in repository.invalid() {
        collector.invalid_pack(invalid);
    }
    for (place, error) in &unopened {
        collector.unopened_pack(Some(place), error);
    }
    for pack in repository
        .packs()
        .filter(|pack| !vanilla_ids.contains(pack.id()))
    {
        for finding in pack.report().diagnostics.iter() {
            collector.diagnostic(finding, Some(pack));
        }
    }

    let loaded_findings = stacks.load_all();
    let findings = loaded_findings
        .iter()
        .filter(|finding| !is_base_game_absent(finding))
        .chain(links.iter().flat_map(|links| links.iter()));
    for finding in findings {
        collector.diagnostic(finding, None);
    }

    let (tally, diagnostics, plain_report) = collector.finish(pack_summaries.len());
    Ok(ValidationResult {
        ok: tally.errors == 0,
        fatal_error: None,
        packs: pack_summaries,
        vanilla_pack_count: vanilla_ids.len(),
        tally,
        diagnostics,
        plain_report,
    })
}

fn collect_dir_files(raw_files: &[(String, Vec<u8>)]) -> BTreeMap<PackPath, Bytes> {
    let mut map = BTreeMap::new();
    for (path, bytes) in raw_files {
        if let Some(pack_path) = normalize_pack_path(path) {
            map.insert(pack_path, Bytes::from(bytes.clone()));
        }
    }
    map
}

fn is_single_pack(files: &BTreeMap<PackPath, Bytes>) -> bool {
    files.contains_key(&PackPath::from_static("manifest.json"))
}

fn is_world_dir(files: &BTreeMap<PackPath, Bytes>) -> bool {
    files.contains_key(&PackPath::from_static("level.dat"))
        || files.contains_key(&PackPath::from_static(WorldPacks::BEHAVIOR_FILE))
        || files.contains_key(&PackPath::from_static(WorldPacks::RESOURCE_FILE))
}

fn parse_world_packs(files: &BTreeMap<PackPath, Bytes>) -> Result<WorldPacks, String> {
    let bp = files
        .get(&PackPath::from_static(WorldPacks::BEHAVIOR_FILE))
        .map(Bytes::as_ref);
    let rp = files
        .get(&PackPath::from_static(WorldPacks::RESOURCE_FILE))
        .map(Bytes::as_ref);
    WorldPacks::parse(bp, rp).map_err(|err| format!("Invalid world pack list: {err}"))
}

fn extract_subtree(files: &BTreeMap<PackPath, Bytes>, folder: &str) -> BTreeMap<PackPath, Bytes> {
    let prefix = format!("{folder}/");
    let mut out = BTreeMap::new();
    for (path, bytes) in files {
        if let Some(rest) = path.as_str().strip_prefix(&prefix)
            && let Ok(rel) = PackPath::new(rest)
        {
            out.insert(rel, bytes.clone());
        }
    }
    out
}

fn world_selection(world: &WorldSelection, repository: &PackRepository) -> WorldPacks {
    let in_world: HashSet<&PackIdVersion> = repository
        .source_reports()
        .iter()
        .filter(|report| world.roots.contains(&report.location.root))
        .flat_map(|report| report.packs.keys())
        .collect();
    let extra = every_pack(
        repository
            .packs()
            .filter(|pack| !in_world.contains(pack.id())),
    );
    let mut selection = world.packs.clone();
    selection.behavior.splice(0..0, extra.behavior);
    selection.resource.splice(0..0, extra.resource);
    selection
}

fn every_pack<'a>(packs: impl Iterator<Item = &'a Pack>) -> WorldPacks {
    let packs: Vec<&Pack> = packs.collect();
    let of_type = |kind: PackType| {
        packs
            .iter()
            .filter(|pack| pack.pack_type() == kind)
            .map(|pack| select(pack.id()))
            .collect()
    };
    WorldPacks {
        behavior: of_type(PackType::Behavior),
        resource: of_type(PackType::Resource),
    }
}

fn select(id: &PackIdVersion) -> PackSelection {
    PackSelection {
        pack_id: id.uuid,
        version: id.version.clone(),
        subpack: None,
    }
}

fn resolve_pack_names(memfs: &MemFs, repository: &PackRepository) -> HashMap<PackIdVersion, String> {
    let mut names = HashMap::new();
    for pack in repository.packs() {
        let lang = read_pack_text(memfs, pack, &PackPath::from_static("texts/en_US.lang"));
        let name = names::display_name(&pack.manifest().header.name, lang.as_deref());
        names.insert(pack.id().clone(), name);
    }
    names
}

fn read_pack_text(memfs: &MemFs, pack: &Pack, path: &PackPath) -> Option<String> {
    pack.read(path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .or_else(|| {
            let place = pack.location().join(path);
            let bytes = memfs.read_dir_file(&place)?;
            String::from_utf8(bytes.to_vec()).ok()
        })
}

struct Vanilla {
    packs: Vec<Pack>,
    resource_layer: Vec<PackIdVersion>,
    app_data: AppData,
}

fn open_vanilla(files: &[(String, Vec<u8>)]) -> Vanilla {
    let mut packs: BTreeMap<String, Vec<(PackPath, Vec<u8>)>> = BTreeMap::new();
    let mut app_data = Vec::new();
    for (raw_path, bytes) in files {
        let normalized = raw_path.replace('\\', "/");
        let path = normalized.trim_start_matches('/');
        let mut parts = path.splitn(3, '/');
        let (Some(folder), Some(name), Some(rest)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if folder.ends_with("_packs") {
            if let Ok(rest) = PackPath::new(rest) {
                packs
                    .entry(format!("{folder}/{name}"))
                    .or_default()
                    .push((rest, bytes.clone()));
            }
        } else if let Ok(pack_path) = PackPath::new(path) {
            app_data.push((pack_path, bytes.clone()));
        }
    }

    let options = OpenOptions {
        origin: PackOrigin::Package,
        ..OpenOptions::default()
    };
    let packs: Vec<(String, Pack)> = packs
        .into_iter()
        .filter_map(|(folder, files)| Some((folder, Pack::from_files(files, &options).ok()?)))
        .collect();
    let resource_layer = vanilla_resource_layer(&packs);
    Vanilla {
        packs: packs.into_iter().map(|(_, pack)| pack).collect(),
        resource_layer,
        app_data: AppData::from_files(app_data),
    }
}

fn vanilla_resource_layer(packs: &[(String, Pack)]) -> Vec<PackIdVersion> {
    let named = |name: &'static str| {
        packs
            .iter()
            .filter(move |(folder, _)| folder.strip_prefix("resource_packs/") == Some(name))
            .map(|(_, pack)| pack)
    };
    let mut slices: Vec<&Pack> = packs
        .iter()
        .filter(|(folder, _)| {
            folder
                .strip_prefix("resource_packs/vanilla_")
                .is_some_and(|version| version.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(|(_, pack)| pack)
        .collect();
    slices.sort_by(|a, b| b.id().version.cmp(&a.id().version));

    named("platform_gdk_pc")
        .chain(named("vanilla_music"))
        .chain(slices)
        .chain(named("vanilla"))
        .chain(named("vanilla_base"))
        .map(|pack| pack.id().clone())
        .collect()
}

#[derive(Clone)]
struct SourceFile {
    name: String,
    text: Option<String>,
}

struct Collector<'a> {
    memfs: &'a MemFs,
    repository: &'a PackRepository,
    vanilla: &'a HashSet<PackIdVersion>,
    names: &'a HashMap<PackIdVersion, String>,
    min_severity: Severity,
    renderer: annotate_snippets::Renderer,
    files: HashMap<(Place, PackPath), SourceFile>,
    seen: HashSet<String>,
    items: Vec<DiagnosticItem>,
    snippets: Vec<String>,
    tally: Tally,
}

impl<'a> Collector<'a> {
    fn new(
        memfs: &'a MemFs,
        repository: &'a PackRepository,
        vanilla: &'a HashSet<PackIdVersion>,
        names: &'a HashMap<PackIdVersion, String>,
        min_severity: Severity,
    ) -> Self {
        Self {
            memfs,
            repository,
            vanilla,
            names,
            min_severity,
            renderer: annotate_snippets::Renderer::plain(),
            files: HashMap::new(),
            seen: HashSet::new(),
            items: Vec::new(),
            snippets: Vec::new(),
            tally: Tally::default(),
        }
    }

    fn pack_name(&self, id: &PackIdVersion, raw: &str) -> String {
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| names::display_name(raw, None))
    }

    fn diagnostic(&mut self, finding: &Diagnostic, pack: Option<&Pack>) {
        if finding.severity < self.min_severity {
            return;
        }
        if finding
            .location
            .pack
            .as_ref()
            .is_some_and(|pack| pack.is_app_data() || self.vanilla.contains(pack.id()))
        {
            return;
        }
        if let Some((located, dependent)) = self.at_dependencies(finding) {
            return self.diagnostic(&located, Some(dependent));
        }
        let pack = pack.or_else(|| self.repository.get(finding.location.pack.as_ref()?.id()));
        let file = finding
            .location
            .path
            .as_ref()
            .map(|path| self.source_file(pack, path));
        let pack_name = match (&finding.location.pack, pack) {
            (Some(pack), _) => Some(self.pack_name(pack.id(), pack.name())),
            (None, Some(pack)) => Some(self.pack_name(pack.id(), &pack.manifest().header.name)),
            (None, None) => None,
        };
        let origin = match (&file, pack) {
            (None, Some(pack)) => Some(self.pack_dir(pack)),
            _ => None,
        };
        self.emit(finding, file, origin, pack_name);
    }

    fn at_dependencies(&self, finding: &Diagnostic) -> Option<(Diagnostic, &'a Pack)> {
        let DiagnosticCode::MissingDependency { pack, .. } = &finding.code else {
            return None;
        };
        if finding.location.path.is_some() {
            return None;
        }
        let dependent = self.repository.get(pack)?;
        let mut located = finding.clone();
        located.location.path = Some(PackPath::from_static("manifest.json"));
        located.location.json_path = JsonPath::of_keys(["dependencies"]);
        Some((located, dependent))
    }

    fn invalid_pack(&mut self, invalid: &InvalidPack) {
        self.unopened_pack(Some(&invalid.location), &invalid.error);
    }

    fn unopened_pack(&mut self, root: Option<&Place>, error: &OpenError) {
        let place = root
            .and_then(|place| self.memfs.display_place(place))
            .unwrap_or_else(|| "<unknown>".to_owned());

        if let OpenError::InvalidManifest { report, .. } = error {
            let lang = root.and_then(|root| {
                let bytes = self
                    .memfs
                    .read_dir_file(&root.join(&PackPath::from_static("texts/en_US.lang")))?;
                String::from_utf8(bytes.to_vec()).ok()
            });
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
                    .map(|path| self.source_file_at_root(root, path));
                let origin = file.is_none().then(|| place.clone());
                self.emit(finding, file, origin, name.clone());
            }
            if report.diagnostics.has_errors() {
                return;
            }
        }

        let err_title = format!("cannot open pack: {}", open_error_text(error));
        let help_text = open_error_help(error);
        let group = Group::with_title(Level::ERROR.primary_title(err_title.as_str()))
            .element(Origin::path(place.as_str()))
            .element(Level::HELP.message(help_text.as_str()));
        let rendered = self.renderer.render(&[group]);
        if !self.seen.insert(rendered.clone()) {
            return;
        }
        self.tally.errors += 1;
        self.snippets.push(rendered.clone());
        self.items.push(DiagnosticItem {
            severity: "error",
            code: "OpenPackError".to_owned(),
            title: err_title,
            file: Some(place),
            line: None,
            column: None,
            json_path: None,
            pack_name: None,
            where_shown: None,
            hints: vec![help_text],
            snippet: rendered,
        });
    }

    fn emit(
        &mut self,
        finding: &Diagnostic,
        file: Option<SourceFile>,
        origin: Option<String>,
        pack_name: Option<String>,
    ) {
        let diag_title = title(finding);
        let mut group = Group::with_title(
            level(finding.severity)
                .primary_title(diag_title.as_str())
                .id(finding.code.name()),
        );

        let text = file.as_ref().and_then(|file| file.text.as_deref());
        let marked = text.and_then(|text| mark(finding, text));
        let missing = marked.as_ref().and_then(|mark| mark.missing.as_deref());
        let (line, column) = match (text, &marked) {
            (Some(text), Some(marked)) => {
                let (l, c) = line_col(text, marked.span.start);
                (Some(l), Some(c))
            }
            _ => (None, None),
        };

        match (&file, text, &marked) {
            (Some(file), Some(text), Some(marked)) => {
                let mut annotation = AnnotationKind::Primary.span(marked.span.clone());
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
        let where_shown = help::where_shown(finding).map(str::to_owned);
        let hints = help::hints(finding, missing);
        let notes = [
            (!at.is_empty()).then(|| format!("at `{at}`")),
            pack_name.as_ref().map(|pack| format!("in pack `{pack}`")),
            where_shown.clone(),
        ];
        group = group
            .elements(
                notes
                    .into_iter()
                    .flatten()
                    .map(|note| Level::NOTE.message(note)),
            )
            .elements(hints.iter().map(|hint| Level::HELP.message(hint)));

        let rendered = self.renderer.render(&[group]);
        if !self.seen.insert(rendered.clone()) {
            return;
        }
        match finding.severity {
            Severity::Error => self.tally.errors += 1,
            Severity::Warning => self.tally.warnings += 1,
            _ => self.tally.infos += 1,
        }
        self.snippets.push(rendered.clone());
        self.items.push(DiagnosticItem {
            severity: severity_str(finding.severity),
            code: finding.code.name().to_owned(),
            title: diag_title,
            file: file.map(|f| f.name).or(origin),
            line,
            column,
            json_path: (!at.is_empty()).then(|| at.to_string()),
            pack_name,
            where_shown,
            hints,
            snippet: rendered,
        });
    }

    fn finish(mut self, pack_count: usize) -> (Tally, Vec<DiagnosticItem>, String) {
        let (summary_level, summary_text) = summary(&self.tally, pack_count);
        let report = Group::with_title(summary_level.primary_title(summary_text));
        self.snippets.push(self.renderer.render(&[report]));
        let plain_report = self.snippets.join("\n\n");
        (self.tally, self.items, plain_report)
    }

    fn source_file(&mut self, pack: Option<&Pack>, path: &PackPath) -> SourceFile {
        let Some(pack) = pack else {
            return SourceFile {
                name: path.to_string(),
                text: None,
            };
        };
        let key = (pack.location().clone(), path.clone());
        if let Some(file) = self.files.get(&key) {
            return file.clone();
        }
        let name = match self.memfs.display_place(pack.location()) {
            Some(root) => format!("{root}/{path}"),
            None => format!(
                "{}/{path}",
                self.pack_name(pack.id(), &pack.manifest().header.name)
            ),
        };
        let text = read_pack_text(self.memfs, pack, path);
        let file = SourceFile { name, text };
        self.files.insert(key, file.clone());
        file
    }

    fn source_file_at_root(&self, root: Option<&Place>, path: &PackPath) -> SourceFile {
        let name = match root.and_then(|r| self.memfs.display_place(r)) {
            Some(root_name) => format!("{root_name}/{path}"),
            None => path.to_string(),
        };
        let text = root.and_then(|r| {
            let bytes = self.memfs.read_dir_file(&r.join(path))?;
            String::from_utf8(bytes.to_vec()).ok()
        });
        SourceFile { name, text }
    }

    fn pack_dir(&self, pack: &Pack) -> String {
        self.memfs
            .display_place(pack.location())
            .unwrap_or_else(|| self.pack_name(pack.id(), &pack.manifest().header.name))
    }
}

fn level(severity: Severity) -> Level<'static> {
    match severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
        Severity::Info => Level::INFO,
        _ => Level::NOTE,
    }
}

fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        _ => "debug",
    }
}

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

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BP_MANIFEST: &str = r#"{
  "format_version": 2,
  "header": {
    "name": "Ruby BP",
    "description": "Test BP",
    "uuid": "11111111-1111-4111-8111-111111111111",
    "version": [1, 0, 0],
    "min_engine_version": [1, 21, 0]
  },
  "modules": [
    { "type": "data", "uuid": "22222222-2222-4222-8222-222222222222", "version": [1, 0, 0] }
  ]
}"#;

    const RP_MANIFEST: &str = r#"{
  "format_version": 2,
  "header": {
    "name": "Ruby RP",
    "description": "Test RP",
    "uuid": "33333333-3333-4333-8333-333333333333",
    "version": [1, 0, 0],
    "min_engine_version": [1, 21, 0]
  },
  "modules": [
    { "type": "resources", "uuid": "44444444-4444-4444-8444-444444444444", "version": [1, 0, 0] }
  ]
}"#;

    const VALID_BLOCK: &str = r#"{
  "format_version": "1.21.0",
  "minecraft:block": {
    "description": { "identifier": "demo:lamp" },
    "components": { "minecraft:friction": 0.6 }
  }
}"#;

    const INVALID_BLOCK: &str = r#"{
  "format_version": "1.21.0",
  "minecraft:block": {
    "description": { "identifier": "demo:lamp" },
    "components": {
      "minecraft:friction": "slippery"
    }
  }
}"#;

    #[test]
    fn valid_bp_and_rp_folders_pass_with_zero_errors() {
        let dirs = vec![
            DirInput {
                name: "Ruby_BP".into(),
                files: vec![
                    ("manifest.json".into(), BP_MANIFEST.as_bytes().to_vec()),
                    ("blocks/lamp.json".into(), VALID_BLOCK.as_bytes().to_vec()),
                ],
            },
            DirInput {
                name: "Ruby_RP".into(),
                files: vec![("manifest.json".into(), RP_MANIFEST.as_bytes().to_vec())],
            },
        ];
        let res = run_validation(&[], &dirs, &[], &ValidateOptions::default());
        assert!(res.ok, "expected ok, got: {:?}", res.diagnostics);
        assert_eq!(res.tally.errors, 0);
        assert_eq!(res.packs.len(), 2);
    }

    #[test]
    fn invalid_block_reports_line_col_snippet_and_hint() {
        let dirs = vec![DirInput {
            name: "Ruby_BP".into(),
            files: vec![
                ("manifest.json".into(), BP_MANIFEST.as_bytes().to_vec()),
                ("blocks/lamp.json".into(), INVALID_BLOCK.as_bytes().to_vec()),
            ],
        }];
        let res = run_validation(&[], &dirs, &[], &ValidateOptions::default());
        assert!(!res.ok);
        assert!(res.tally.errors >= 1);
        let item = res
            .diagnostics
            .iter()
            .find(|d| d.code == "InvalidType")
            .expect("InvalidType diagnostic");
        assert_eq!(item.line, Some(6));
        assert!(item.column.is_some());
        assert!(item.snippet.contains("slippery"));
        assert!(!item.hints.is_empty());
    }

    #[test]
    fn invalid_manifest_reports_manifest_diagnostics() {
        let bad_manifest = r#"{ "format_version": 2, "header": { "name": "Broken" } }"#;
        let dirs = vec![DirInput {
            name: "Broken_BP".into(),
            files: vec![("manifest.json".into(), bad_manifest.as_bytes().to_vec())],
        }];
        let res = run_validation(&[], &dirs, &[], &ValidateOptions::default());
        assert!(!res.ok);
        assert!(res.tally.errors >= 1);
        assert!(
            res.diagnostics
                .iter()
                .any(|d| d.file.as_deref() == Some("Broken_BP/manifest.json"))
        );
    }

    #[test]
    fn mcpack_archive_bytes_validate_like_a_folder() {
        let pack = Pack::from_files(
            [
                (
                    PackPath::from_static("manifest.json"),
                    BP_MANIFEST.as_bytes(),
                ),
                (
                    PackPath::from_static("blocks/lamp.json"),
                    INVALID_BLOCK.as_bytes(),
                ),
            ],
            &OpenOptions::default(),
        )
        .expect("pack");
        let archive_bytes = pack.to_archive().expect("zip bytes");
        let res = run_validation(
            &[ArchiveInput {
                name: "Ruby_BP.mcpack".into(),
                bytes: archive_bytes.to_vec(),
            }],
            &[],
            &[],
            &ValidateOptions::default(),
        );
        assert!(!res.ok);
        assert!(res.diagnostics.iter().any(|d| d.code == "InvalidType"));
    }

    #[test]
    fn world_folder_uses_world_pack_selection() {
        let world_bp_list = r#"[{ "pack_id": "11111111-1111-4111-8111-111111111111", "version": [1, 0, 0] }]"#;
        let unused_bp_manifest = r#"{
  "format_version": 2,
  "header": {
    "name": "Unused BP",
    "description": "",
    "uuid": "99999999-9999-4999-8999-999999999999",
    "version": [1, 0, 0],
    "min_engine_version": [1, 21, 0]
  },
  "modules": [
    { "type": "data", "uuid": "88888888-8888-4888-8888-888888888888", "version": [1, 0, 0] }
  ]
}"#;
        let dirs = vec![DirInput {
            name: "MyWorld".into(),
            files: vec![
                ("level.dat".into(), b"BDF\x00".to_vec()),
                (
                    "world_behavior_packs.json".into(),
                    world_bp_list.as_bytes().to_vec(),
                ),
                (
                    "behavior_packs/enabled/manifest.json".into(),
                    BP_MANIFEST.as_bytes().to_vec(),
                ),
                (
                    "behavior_packs/enabled/blocks/lamp.json".into(),
                    VALID_BLOCK.as_bytes().to_vec(),
                ),
                (
                    "behavior_packs/unused/manifest.json".into(),
                    unused_bp_manifest.as_bytes().to_vec(),
                ),
                (
                    "behavior_packs/unused/blocks/broken.json".into(),
                    INVALID_BLOCK.as_bytes().to_vec(),
                ),
            ],
        }];
        let res = run_validation(&[], &dirs, &[], &ValidateOptions::default());
        assert!(
            res.ok,
            "unused pack in world folder should not be loaded: {:?}",
            res.diagnostics
        );
    }
}
