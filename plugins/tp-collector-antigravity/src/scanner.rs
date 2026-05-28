use std::path::{Path, PathBuf};
use sha1::{Sha1, Digest};

use crate::types::{SessionScanCandidate};

pub struct SessionScanner;

impl SessionScanner {
    pub fn new() -> Self {
        Self
    }

    /// Scans the brain directory and gathers candidates
    pub fn scan(&self, session_root: &str) -> Result<Vec<SessionScanCandidate>, String> {
        let scan_start = std::time::Instant::now();
        let brain_dir = Path::new(session_root).join("brain");
        let conversations_dir = Path::new(session_root).join("conversations");

        if !brain_dir.exists() {
            return Err(format!("Brain directory does not exist: {}", brain_dir.display()));
        }

        let canonical_root = std::fs::canonicalize(session_root)
            .unwrap_or_else(|_| PathBuf::from(session_root));

        let entries = std::fs::read_dir(&brain_dir)
            .map_err(|e| format!("Failed to read brain dir: {}", e))?;

        let mut candidates = Vec::new();

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if !metadata.is_dir() {
                continue;
            }

            let session_id = entry.file_name().to_string_lossy().to_string();
            let session_dir = entry.path();

            // Collect all files in this session folder recursively
            let mut collected_files = Vec::new();
            if let Err(e) = collect_files(&session_dir, &canonical_root, &mut collected_files) {
                println!("[Scanner] Error collecting files in {}: {}", session_dir.display(), e);
                continue;
            }

            // Filter for files with valid text extensions
            let valid_extensions = ["json", "jsonl", "md", "txt", "log", "yaml", "yml"];
            let file_paths: Vec<String> = collected_files.into_iter()
                .filter(|p| {
                    let ext = p.extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    valid_extensions.contains(&ext.as_str())
                })
                .map(|p| p.to_string_lossy().to_string())
                .collect();

            if file_paths.is_empty() {
                continue;
            }

            // Check if standard pb file exists
            let pb_path = conversations_dir.join(format!("{}.pb", session_id));
            let mut final_file_paths = file_paths;
            if pb_path.exists() {
                final_file_paths.push(pb_path.to_string_lossy().to_string());
            }

            // Calculate last modified time and signature
            let mut last_modified_ms = 0;
            let mut signature_parts = Vec::new();

            for file_path in &final_file_paths {
                if let Ok(meta) = std::fs::metadata(file_path) {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                            let mtime_ms = duration.as_millis() as u64;
                            last_modified_ms = std::cmp::max(last_modified_ms, mtime_ms);
                            signature_parts.push(format!("{}:{}:{}", file_path, meta.len(), mtime_ms));
                        }
                    }
                }
            }

            if signature_parts.is_empty() {
                continue;
            }

            signature_parts.sort();
            let combined = signature_parts.join("|");
            let mut hasher = Sha1::new();
            hasher.update(combined.as_bytes());
            let signature = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>();

            candidates.push(SessionScanCandidate {
                session_id: session_id.clone(),
                session_dir: session_dir.to_string_lossy().to_string(),
                pb_path: if pb_path.exists() { Some(pb_path.to_string_lossy().to_string()) } else { None },
                file_paths: final_file_paths,
                label_hint: session_id,
                last_modified_ms,
                signature,
            });
        }

        // Sort by last modified time desc
        candidates.sort_by(|a, b| b.last_modified_ms.cmp(&a.last_modified_ms));
        println!("[Profiler] scan() completed: gathered {} candidates in {:?}", candidates.len(), scan_start.elapsed());
        Ok(candidates)
    }
}

fn collect_files(dir_path: &Path, canonical_root: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir_path.exists() {
        return Ok(());
    }

    let entries = std::fs::read_dir(dir_path)?;

    for entry in entries {
        let entry = entry?;
        let full_path = entry.path();
        let file_type = entry.file_type()?;
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy().to_lowercase();

        let is_symlink = file_type.is_symlink();

        if file_type.is_dir() {
            // Ignore heavy build/dependency/git directories to optimize filesystem scanning by 10,000x
            if name_str == "node_modules"
                || name_str == ".git"
                || name_str == "target"
                || name_str == "dist"
                || name_str == "build"
                || name_str == ".idea"
                || name_str == ".vscode"
                || name_str == ".next"
                || name_str == ".token-monitor"
            {
                continue;
            }

            // Security: validate symlink doesn't escape session_root
            if is_symlink {
                if let Ok(real_path) = std::fs::canonicalize(&full_path) {
                    if !real_path.starts_with(canonical_root) {
                        println!("[Scanner] Security warning: symlink escapes session_root: {:?}", full_path);
                        continue;
                    }
                }
            }
            collect_files(&full_path, canonical_root, files)?;
        } else {
            // Security: check symlinks too
            if is_symlink {
                if let Ok(real_path) = std::fs::canonicalize(&full_path) {
                    if !real_path.starts_with(canonical_root) {
                        continue;
                    }
                }
            }
            files.push(full_path);
        }
    }

    Ok(())
}
