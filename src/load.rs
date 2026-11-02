//! Turning host paths into loaded stacks with the std driver of `bedrock_addon`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use bedrock::addon::load::{AppData, LoadOptions, Place, StackOptions};
use bedrock::addon::molang::MolangCheck;
use bedrock::addon::pack::{
    OpenError, Pack, PackIdVersion, PackOrigin, PackRepository, PackSelection, PackType, StackSet,
    WorldPacks,
};
use bedrock::addon::storage::PackPath;
use bedrock::addon_std::{DirSource, Driver, LoadError, Options, read_world_packs};

use crate::names;
use crate::status::{plural, status};
use crate::vanilla;

/// What one run checks: the packs found, and both stacks of every usable pack, loaded.
pub struct Loaded {
    /// Keeps the root table, which maps a pack's location back to the host.
    driver: Driver,
    pub repository: PackRepository,
    /// Pack folders given on the command line that could not be opened.
    pub unopened: Vec<(PathBuf, OpenError)>,
    /// The embedded vanilla packs added below the packs checked.
    vanilla: HashSet<PackIdVersion>,
    /// The names to show for the packs checked (`names::display_name`).
    names: HashMap<PackIdVersion, String>,
    pub stacks: StackSet,
}

impl Loaded {
    /// The packs being checked: every pack found, without the vanilla ones.
    pub fn packs(&self) -> impl Iterator<Item = &Pack> + '_ {
        self.repository
            .packs()
            .filter(|pack| !self.is_vanilla(pack.id()))
    }

    pub fn is_vanilla(&self, id: &PackIdVersion) -> bool {
        self.vanilla.contains(id)
    }

    pub fn pack(&self, id: &PackIdVersion) -> Option<&Pack> {
        self.repository.get(id)
    }

    /// The name to show for the pack `id`: localized and without formatting codes for a pack
    /// being checked, else `raw` without its codes.
    pub fn name(&self, id: &PackIdVersion, raw: &str) -> String {
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| names::display_name(raw, None))
    }

    /// The name to show for `pack`.
    pub fn pack_name(&self, pack: &Pack) -> String {
        self.name(pack.id(), &pack.manifest().header.name)
    }

    /// The host folder or archive `location` is in; `None` for a pack held in memory.
    pub fn host_path(&self, location: &Place) -> Option<PathBuf> {
        self.driver.roots().host_path(location)
    }

    /// The text of the file `path` of `pack` (`read_text`).
    pub fn read_text(&self, pack: &Pack, path: &PackPath) -> Option<String> {
        read_text(&self.driver, pack, path)
    }
}

/// Loads every path into one repository, so packs given together can depend on each other.
pub fn load(paths: &[PathBuf], with_vanilla: bool) -> anyhow::Result<Loaded> {
    let mut driver = Driver::new(Options::default())?;
    let inputs = Inputs::classify(paths)?;
    let mut repository = driver.repository(inputs.sources)?;
    let unopened = open_pack_dirs(&mut driver, &mut repository, &inputs.pack_dirs)?;

    if repository.packs().next().is_none() && repository.invalid().is_empty() && unopened.is_empty()
    {
        match paths {
            [path] => bail!("no pack found in `{}`", path.display()),
            _ => bail!("no pack found in any of the paths given"),
        }
    }

    let mut selection = match inputs.world {
        None => every_pack(repository.packs()),
        Some(world) => world.selection(&driver, &repository),
    };
    let names = announce(&driver, &repository);

    // The game's own packs go below, as the base-game layer the stacks are composed on, with
    // the definitions it reads from outside packs.
    let mut vanilla_ids = HashSet::new();
    let mut app_data = None;
    if with_vanilla && let Some(vanilla) = vanilla::load()? {
        selection
            .resource
            .extend(vanilla.resource_layer.iter().map(select));
        for pack in vanilla.packs {
            vanilla_ids.insert(pack.id().clone());
            repository.add_pack(pack, PackOrigin::Package);
        }
        app_data = Some(vanilla.app_data);
    }

    let checked = repository.packs().count() - vanilla_ids.len();
    status("Loading", loading_message(checked, vanilla_ids.len()));
    let stacks = repository.stacks(&selection, load_options(app_data));
    let stacks = driver.load_stacks(stacks, StackOptions::default())?;

    Ok(Loaded {
        driver,
        repository,
        unopened,
        vanilla: vanilla_ids,
        names,
        stacks,
    })
}

/// The command-line paths, sorted by how each is opened.
#[derive(Default)]
struct Inputs<'a> {
    /// Archives and folders of packs, opened by the repository.
    sources: Vec<DirSource>,
    /// Single pack folders, opened one by one so a failure names its folder.
    pack_dirs: Vec<&'a Path>,
    world: Option<World>,
}

impl<'a> Inputs<'a> {
    fn classify(paths: &'a [PathBuf]) -> anyhow::Result<Self> {
        let mut inputs = Self::default();
        for path in paths {
            let metadata = std::fs::metadata(path)
                .with_context(|| format!("cannot read `{}`", path.display()))?;
            if metadata.is_file() {
                // `.mcpack`, `.mcaddon`, `.mcworld`, `.zip`, …
                inputs
                    .sources
                    .push(DirSource::archive(path, PackOrigin::World));
            } else if path.join("manifest.json").is_file() {
                inputs.pack_dirs.push(path);
            } else if is_world(path) {
                if inputs.world.is_some() {
                    bail!("only one world can be checked at a time");
                }
                let world = World::read(path)?;
                inputs.sources.extend(
                    world
                        .dirs
                        .iter()
                        .map(|dir| DirSource::packs(dir, PackOrigin::World)),
                );
                inputs.world = Some(world);
            } else {
                // A folder whose children are packs (or one subfolder holding a manifest).
                inputs
                    .sources
                    .push(DirSource::packs(path, PackOrigin::World));
            }
        }
        Ok(inputs)
    }
}

fn is_world(dir: &Path) -> bool {
    dir.join("level.dat").is_file()
        || dir.join(WorldPacks::BEHAVIOR_FILE).is_file()
        || dir.join(WorldPacks::RESOURCE_FILE).is_file()
}

/// A world folder: its pack lists choose the packs.
struct World {
    packs: WorldPacks,
    /// `behavior_packs/` and `resource_packs/`, the folders its packs are in.
    dirs: [PathBuf; 2],
}

impl World {
    fn read(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            packs: read_world_packs(path)?,
            dirs: [path.join("behavior_packs"), path.join("resource_packs")],
        })
    }

    /// The world's own pack lists, with the packs given beside it switched on above them.
    fn selection(self, driver: &Driver, repository: &PackRepository) -> WorldPacks {
        let in_world: HashSet<&PackIdVersion> = repository
            .source_reports()
            .iter()
            .filter(|report| {
                driver
                    .roots()
                    .host_path(&report.location)
                    .is_some_and(|root| self.dirs.contains(&root))
            })
            .flat_map(|report| report.packs.keys())
            .collect();
        let extra = every_pack(
            repository
                .packs()
                .filter(|pack| !in_world.contains(pack.id())),
        );
        let mut selection = self.packs;
        // The first entry of a pack list is the top of the stack.
        selection.behavior.splice(0..0, extra.behavior);
        selection.resource.splice(0..0, extra.resource);
        selection
    }
}

/// Opens the pack folders given on the command line into `repository`; the ones that are no
/// pack are returned with why.
fn open_pack_dirs(
    driver: &mut Driver,
    repository: &mut PackRepository,
    dirs: &[&Path],
) -> anyhow::Result<Vec<(PathBuf, OpenError)>> {
    let mut unopened = Vec::new();
    for &dir in dirs {
        match driver.pack_dir(dir) {
            Ok(pack) => repository.add_pack(pack, PackOrigin::World),
            Err(LoadError::Open { source, .. }) => unopened.push((dir.to_path_buf(), source)),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(unopened)
}

/// The name to show for every pack of `repository`, each announced with a `Checking` line.
fn announce(driver: &Driver, repository: &PackRepository) -> HashMap<PackIdVersion, String> {
    let mut names = HashMap::new();
    for pack in repository.packs() {
        let lang = read_text(driver, pack, &PackPath::from_static("texts/en_US.lang"));
        let name = names::display_name(&pack.manifest().header.name, lang.as_deref());
        let place = driver
            .roots()
            .host_path(pack.location())
            .map_or_else(|| "in memory".into(), |path| display_path(&path));
        let version = &pack.id().version;
        status("Checking", format!("{name} v{version} ({place})"));
        names.insert(pack.id().clone(), name);
    }
    names
}

/// The `Loading` status line: how many packs are checked, on how many vanilla ones.
fn loading_message(checked: usize, vanilla: usize) -> String {
    match vanilla {
        0 => plural(checked, "pack"),
        n => format!(
            "{} on {}",
            plural(checked, "pack"),
            plural(n, "vanilla pack")
        ),
    }
}

fn load_options(app_data: Option<AppData>) -> LoadOptions {
    LoadOptions {
        // With a Molang check every Molang field is compiled while the stacks load.
        molang: Some(Arc::new(MolangCheck::new())),
        app_data,
        ..LoadOptions::default()
    }
}

/// A pack selection that turns on every behaviour and resource pack of `packs`.
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

/// The pack `id` as an entry of a pack list, without a subpack.
fn select(id: &PackIdVersion) -> PackSelection {
    PackSelection {
        pack_id: id.uuid,
        version: id.version.clone(),
        subpack: None,
    }
}

/// The text of the file `path` of `pack`: from the pack when its bytes are resident (archives),
/// else from the host folder the pack was opened from.
fn read_text(driver: &Driver, pack: &Pack, path: &PackPath) -> Option<String> {
    pack.read(path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .or_else(|| {
            let root = driver.roots().host_path(pack.location())?;
            std::fs::read_to_string(host_file(&root, path)).ok()
        })
}

/// The host file of the pack file `path`, for a pack opened from the folder `root`.
pub fn host_file(root: &Path, path: &PackPath) -> PathBuf {
    path.segments()
        .fold(root.to_path_buf(), |host, segment| host.join(segment))
}

/// A host path as the user would type it: relative to the working directory when below it.
pub fn display_path(path: &Path) -> String {
    let relative = std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf));
    relative.as_deref().unwrap_or(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loading_line_names_the_vanilla_packs_only_when_there_are_some() {
        assert_eq!(loading_message(1, 0), "1 pack");
        assert_eq!(loading_message(2, 145), "2 packs on 145 vanilla packs");
    }

    #[test]
    fn a_pack_file_maps_below_its_host_root() {
        let path = PackPath::from_static("texts/en_US.lang");
        assert_eq!(
            host_file(Path::new("/packs/ruby"), &path),
            Path::new("/packs/ruby/texts/en_US.lang")
        );
    }
}
