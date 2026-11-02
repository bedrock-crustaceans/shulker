//! The game's own packs and app data, baked by `bake-vanilla` and embedded in the binary, so a
//! pack is checked on top of vanilla as the game loads it.

use std::collections::BTreeMap;

use bedrock::addon::load::AppData;
use bedrock::addon::pack::{OpenOptions, Pack, PackIdVersion, PackOrigin};
use bedrock::addon::storage::PackPath;

#[cfg(vanilla)]
use crate::bundle;

#[cfg(vanilla)]
static BUNDLE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/vanilla/vanilla.zst"));

/// The install the embedded files came from.
#[cfg(vanilla)]
const LABEL: &str = env!("SHULKER_VANILLA_LABEL");

/// The vanilla layer of one run.
pub struct Vanilla {
    pub packs: Vec<Pack>,
    /// The client's base resource layer, top first. The stacks are composed as a server
    /// composes a world's, and a world's resource list holds no vanilla: the client keeps it in
    /// its global stack, below the world's packs.
    pub resource_layer: Vec<PackIdVersion>,
    /// `definitions/` and `credits/`: the definitions the game reads from outside packs.
    pub app_data: AppData,
}

/// Unpacks the embedded files; `None` in a build without them.
#[cfg(vanilla)]
pub fn load() -> anyhow::Result<Option<Vanilla>> {
    crate::status::status("Unpacking", format!("vanilla files of {LABEL}"));
    let bundle = bundle::read(BUNDLE)?;
    Ok(Some(open(bundle.files)))
}

#[cfg(not(vanilla))]
pub fn load() -> anyhow::Result<Option<Vanilla>> {
    Ok(None)
}

/// Groups the files into packs (`behavior_packs/<name>/…`, `resource_packs/<name>/…`) and app
/// data (everything else) and opens them.
#[cfg_attr(not(vanilla), allow(dead_code))]
fn open(files: Vec<(String, Vec<u8>)>) -> Vanilla {
    let mut packs: BTreeMap<String, Vec<(PackPath, Vec<u8>)>> = BTreeMap::new();
    let mut app_data = Vec::new();
    for (path, bytes) in files {
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
                    .push((rest, bytes));
            }
        } else if let Ok(path) = PackPath::new(&path) {
            app_data.push((path, bytes));
        }
    }

    let options = OpenOptions {
        origin: PackOrigin::Package,
        ..OpenOptions::default()
    };
    // A folder that is no pack (no manifest) is left out, as the game does.
    let packs: Vec<(String, Pack)> = packs
        .into_iter()
        .filter_map(|(folder, files)| Some((folder, Pack::from_files(files, &options).ok()?)))
        .collect();
    let resource_layer = resource_layer(&packs);
    Vanilla {
        packs: packs.into_iter().map(|(_, pack)| pack).collect(),
        resource_layer,
        app_data: AppData::from_files(app_data),
    }
}

/// The client's base resource layer, top first: the platform pack, the music, the `vanilla_<v>`
/// slices newest first, `vanilla`, `vanilla_base`.
fn resource_layer(packs: &[(String, Pack)]) -> Vec<PackIdVersion> {
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
