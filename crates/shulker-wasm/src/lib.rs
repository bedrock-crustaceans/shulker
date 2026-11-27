//! WebAssembly bindings for `shulker`.

mod memfs;
mod validate;

use wasm_bindgen::prelude::*;

use validate::{ArchiveInput, DirInput, ValidateOptions, ValidationResult, run_validation};

#[wasm_bindgen]
pub fn shulker_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Accumulates archives, directory trees, and optional vanilla files from JS before running one
/// validation pass.
#[wasm_bindgen]
pub struct Session {
    archives: Vec<ArchiveInput>,
    dirs: Vec<DirInput>,
    vanilla_files: Vec<(String, Vec<u8>)>,
}

#[wasm_bindgen]
impl Session {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            archives: Vec::new(),
            dirs: Vec::new(),
            vanilla_files: Vec::new(),
        }
    }

    pub fn add_archive(&mut self, name: String, bytes: Vec<u8>) {
        self.archives.push(ArchiveInput { name, bytes });
    }

    pub fn begin_dir(&mut self, name: String) -> u32 {
        let id = u32::try_from(self.dirs.len()).expect("too many directories");
        self.dirs.push(DirInput {
            name,
            files: Vec::new(),
        });
        id
    }

    pub fn add_dir_file(&mut self, dir_id: u32, relative_path: String, bytes: Vec<u8>) {
        if let Some(dir) = self.dirs.get_mut(dir_id as usize) {
            dir.files.push((relative_path, bytes));
        }
    }

    pub fn add_vanilla_file(&mut self, relative_path: String, bytes: Vec<u8>) {
        self.vanilla_files.push((relative_path, bytes));
    }

    pub fn validate(&self, options_json: &str) -> String {
        let options: ValidateOptions = if options_json.trim().is_empty() {
            ValidateOptions::default()
        } else {
            match serde_json::from_str(options_json) {
                Ok(parsed) => parsed,
                Err(err) => {
                    return serde_json::to_string(&ValidationResult {
                        ok: false,
                        fatal_error: Some(format!("Invalid validation options JSON: {err}")),
                        packs: Vec::new(),
                        vanilla_pack_count: 0,
                        tally: validate::Tally::default(),
                        diagnostics: Vec::new(),
                        plain_report: String::new(),
                    })
                    .unwrap_or_else(|_| {
                        r#"{"ok":false,"fatal_error":"serialization error"}"#.to_owned()
                    });
                }
            }
        };

        let result = run_validation(&self.archives, &self.dirs, &self.vanilla_files, &options);
        serde_json::to_string(&result).unwrap_or_else(|err| {
            format!(
                r#"{{"ok":false,"fatal_error":"Failed to serialize result: {}"}}"#,
                err.to_string().replace('"', "'")
            )
        })
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
