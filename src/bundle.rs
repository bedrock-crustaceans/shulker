//! The format of the baked vanilla files, shared by `bake-vanilla` (which writes it) and
//! `shulker` (which embeds and reads it).
//!
//! One zstd stream holding:
//! - the magic `SHULKER-VANILLA-1`,
//! - the label of the install it came from (u16 length, UTF-8),
//! - the file count (u32), then per file its path below the game's `data/` folder (u16
//!   length, UTF-8, `/`-separated) and its bytes (u32 length, bytes).
//!
//! All integers are little-endian.

#![allow(dead_code, reason = "each binary uses its half")]

use std::io::{self, Read, Write};

pub const MAGIC: &[u8] = b"SHULKER-VANILLA-1";

/// The longest window the encoder uses; the decoder must allow as much.
pub const WINDOW_LOG: u32 = 30;

/// The folders of the game's `data/` folder that are baked.
pub const FOLDERS: &[&str] = &["behavior_packs", "resource_packs", "definitions", "credits"];

pub struct Bundle {
    pub label: String,
    pub files: Vec<(String, Vec<u8>)>,
}

pub fn write(out: impl Write, label: &str, files: &[(String, Vec<u8>)]) -> io::Result<()> {
    let mut encoder = zstd::Encoder::new(out, 22)?;
    encoder.long_distance_matching(true)?;
    encoder.window_log(WINDOW_LOG)?;
    encoder.include_checksum(true)?;
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    encoder.multithread(u32::try_from(workers).unwrap_or(1))?;

    encoder.write_all(MAGIC)?;
    write_str(&mut encoder, label)?;
    encoder.write_all(&u32::try_from(files.len()).map_err(invalid)?.to_le_bytes())?;
    for (path, bytes) in files {
        write_str(&mut encoder, path)?;
        encoder.write_all(&u32::try_from(bytes.len()).map_err(invalid)?.to_le_bytes())?;
        encoder.write_all(bytes)?;
    }
    encoder.finish()?;
    Ok(())
}

pub fn read(compressed: &[u8]) -> io::Result<Bundle> {
    let mut decoder = zstd::Decoder::new(compressed)?;
    decoder.window_log_max(WINDOW_LOG)?;
    let mut input = io::BufReader::new(decoder);

    let mut magic = [0; MAGIC.len()];
    input.read_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(invalid("not a baked vanilla bundle"));
    }
    let label = read_str(&mut input)?;
    let count = read_u32(&mut input)?;
    let mut files = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let path = read_str(&mut input)?;
        let mut bytes = vec![0; read_u32(&mut input)? as usize];
        input.read_exact(&mut bytes)?;
        files.push((path, bytes));
    }
    Ok(Bundle { label, files })
}

fn write_str(out: &mut impl Write, text: &str) -> io::Result<()> {
    out.write_all(&u16::try_from(text.len()).map_err(invalid)?.to_le_bytes())?;
    out.write_all(text.as_bytes())
}

fn read_str(input: &mut impl Read) -> io::Result<String> {
    let mut len = [0; 2];
    input.read_exact(&mut len)?;
    let mut bytes = vec![0; usize::from(u16::from_le_bytes(len))];
    input.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(invalid)
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
