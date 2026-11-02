//! Bakes the vanilla packs and app data of a game install into `vanilla/vanilla.zst` (and its
//! label into `vanilla/vanilla.label`), which `shulker` embeds at build time.
//!
//! ```sh
//! cargo run --release --bin bake-vanilla -- \
//!     <game install>/data
//! cargo build --release
//! ```
//!
//! Every file is kept byte for byte (MCB files included: the library reads them) except large
//! audio and images, which are cut to their first bytes: the checks read only the header of an
//! image (its format and size) and of an audio file (its format), and those files are most of an
//! install's size. A `.brarchive` stays a brarchive with the same entries; only its large images
//! are cut the same way (the font sheets alone are 160 MiB of PNG).

#[path = "../bundle.rs"]
mod bundle;

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Parser;

/// Bake a game install's vanilla packs into `vanilla/vanilla.zst` for `shulker` to embed.
#[derive(Debug, Parser)]
struct Cli {
    /// The install's `data` folder: the one holding `behavior_packs/` and `resource_packs/`.
    data: PathBuf,

    /// What to call the install in `shulker --version`; the build folder's name by default.
    #[arg(long)]
    label: Option<String>,

    /// Where to write the bundle.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/vanilla/vanilla.zst"))]
    out: PathBuf,
}

/// Media files are cut down to their first `HEADER_LEN` bytes: enough for every header the
/// checks read (PNG, TGA, JPEG and the audio formats).
const HEADER_LEN: usize = 4 * 1024;
const MEDIA: &[&str] = &[
    "ogg", "fsb", "wav", "mp3", "png", "tga", "jpg", "jpeg", "hdr", "gif", "ktx",
];

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if !cli.data.join("behavior_packs").is_dir() || !cli.data.join("resource_packs").is_dir() {
        bail!(
            "`{}` has no `behavior_packs/` and `resource_packs/`; pass the install's `data` folder",
            cli.data.display()
        );
    }
    let label = match cli.label {
        Some(label) => label,
        None => cli
            .data
            .canonicalize()?
            .parent()
            .and_then(Path::file_name)
            .map_or_else(
                || "unknown".into(),
                |name| name.to_string_lossy().into_owned(),
            ),
    };

    let mut files = Vec::new();
    let (mut raw, mut cut) = (0u64, 0usize);
    for folder in bundle::FOLDERS {
        let root = cli.data.join(folder);
        if root.is_dir() {
            walk(&cli.data, &root, &mut files)?;
        }
    }
    for (path, bytes) in &mut files {
        raw += bytes.len() as u64;
        if extension(path) == "brarchive" {
            cut += trim_brarchive(path, bytes)?;
        } else if trim(path, bytes) {
            cut += 1;
        }
    }
    // Files of one kind side by side compress better.
    files.sort_by(|(a, _), (b, _)| extension(a).cmp(extension(b)).then(a.cmp(b)));
    let kept: u64 = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    eprintln!(
        "{} files from `{label}`: {} MiB read, {cut} media files cut to their header, {} MiB to compress",
        files.len(),
        raw >> 20,
        kept >> 20
    );

    if let Some(dir) = cli.out.parent() {
        fs::create_dir_all(dir)?;
    }
    let out =
        File::create(&cli.out).with_context(|| format!("cannot write `{}`", cli.out.display()))?;
    bundle::write(BufWriter::new(out), &label, &files)?;
    fs::write(cli.out.with_extension("label"), &label)?;
    eprintln!(
        "wrote `{}`: {} MiB",
        cli.out.display(),
        fs::metadata(&cli.out)?.len() >> 20
    );
    Ok(())
}

/// Cuts a large media file down to its header; whether it was cut.
fn trim(path: &str, bytes: &mut Vec<u8>) -> bool {
    let media = MEDIA.contains(&extension(path).to_ascii_lowercase().as_str());
    if media && bytes.len() > HEADER_LEN {
        bytes.truncate(HEADER_LEN);
        return true;
    }
    false
}

/// Cuts the large media entries of a brarchive, which stays a brarchive with the same entries in
/// the same order; how many were cut.
fn trim_brarchive(path: &str, bytes: &mut Vec<u8>) -> anyhow::Result<usize> {
    let mut entries: Vec<(String, Vec<u8>)> =
        brarchive::deserialize(bytes).with_context(|| format!("cannot read `{path}`"))?;
    let mut cut = 0;
    for (name, entry) in &mut entries {
        if trim(name, entry) {
            cut += 1;
        }
    }
    if cut > 0 {
        *bytes =
            brarchive::serialize(entries).with_context(|| format!("cannot rewrite `{path}`"))?;
    }
    Ok(cut)
}

fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, ext)| ext)
}

/// Every file below `dir`, with its path relative to `data`.
fn walk(data: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("cannot list `{}`", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            walk(data, &path, files)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(data)?
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let bytes =
                fs::read(&path).with_context(|| format!("cannot read `{}`", path.display()))?;
            files.push((relative, bytes));
        }
    }
    Ok(())
}
