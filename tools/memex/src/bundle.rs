use crate::share;
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const BUNDLES_DIR: &str = ".context/bundles";

/// Encrypt a single session transcript into a portable bundle under `.context/bundles/<id>.age`.
pub fn run_share_session(
    repo_root: &Path,
    session_filename: &str,
    passphrase: Option<String>,
) -> Result<()> {
    let context_dir = repo_root.join(".context");
    let sessions_dir = context_dir.join("sessions");
    anyhow::ensure!(
        sessions_dir.is_dir(),
        ".context/sessions/ not found. Run `memex init` + `memex sync` first."
    );

    let session_path = safe_sessions_join(&sessions_dir, session_filename)?;
    if let Ok(meta) = fs::symlink_metadata(&session_path) {
        anyhow::ensure!(
            !meta.file_type().is_symlink(),
            "refusing to read symlinked session file: {}",
            session_path.display()
        );
    }
    anyhow::ensure!(
        session_path.is_file(),
        "session not found: .context/sessions/{} (run `memex sync` first)",
        session_filename
    );
    let content = fs::read_to_string(&session_path)
        .with_context(|| format!("read {}", session_path.display()))?;

    let id = generate_bundle_id();
    let bundles_dir = repo_root.join(BUNDLES_DIR);
    fs::create_dir_all(&bundles_dir)
        .with_context(|| format!("create {}", bundles_dir.display()))?;

    let mut archive: BTreeMap<String, String> = BTreeMap::new();
    archive.insert(format!("sessions/{}", session_filename), content);

    let manifest = json!({
        "format": "memex-session-bundle",
        "version": 1,
        "created_at": Utc::now().to_rfc3339(),
        "session_filename": session_filename,
        "repo_root": repo_root.to_string_lossy(),
        "git_head": git_output(repo_root, &["rev-parse", "HEAD"]).ok(),
        "git_origin": git_output(repo_root, &["config", "--get", "remote.origin.url"]).ok(),
    });
    archive.insert(
        "manifest.json".to_string(),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    );

    let plaintext = serde_json::to_vec(&archive).context("serialize bundle")?;
    let passphrase = share::require_passphrase(passphrase, "memex share-session")?;
    let encrypted = share::encrypt_bytes(&passphrase, &plaintext)?;

    let out_rel = format!("{BUNDLES_DIR}/{id}.age");
    let out_path = repo_root.join(&out_rel);
    fs::write(&out_path, &encrypted).with_context(|| format!("write {}", out_path.display()))?;

    println!("Bundle ID: {}", id);
    println!("Bundle file: {}", out_rel);
    println!("Filesystem path: {}", out_path.display());
    println!();
    println!("Import in another repo:");
    println!("  memex import {}", id);
    println!("  (use the same --passphrase you encrypted with)");
    println!();
    println!("To share via git:");
    println!("  git add {}", out_rel);
    println!(
        "  git commit -m \"chore(memex): share session bundle {}\"",
        id
    );

    Ok(())
}

/// Import a shared session bundle by ID.
///
/// Resolution order:
/// 1) working tree: `.context/bundles/<id>.age`
/// 2) git history: `git log --all -- .context/bundles/<id>.age` + `git show`
pub fn run_import(repo_root: &Path, id: &str, passphrase: Option<String>) -> Result<()> {
    let id = normalize_id(id);
    validate_id(&id)?;

    let bundles_rel = format!("{BUNDLES_DIR}/{id}.age");
    let bundles_path = repo_root.join(&bundles_rel);

    let encrypted = if bundles_path.is_file() {
        fs::read(&bundles_path).with_context(|| format!("read {}", bundles_path.display()))?
    } else {
        read_git_file(repo_root, &bundles_rel)?
    };

    let passphrase = share::require_passphrase(passphrase, "memex import")?;
    let plaintext = share::decrypt_bytes(&passphrase, &encrypted)?;

    let archive: BTreeMap<String, String> =
        serde_json::from_slice(&plaintext).context("corrupted bundle contents")?;

    let context_dir = repo_root.join(".context");
    let sessions_dir = context_dir.join("sessions");
    anyhow::ensure!(
        sessions_dir.is_dir(),
        ".context/sessions/ not found. Run `memex init` first."
    );

    let existing = list_existing_sessions(&sessions_dir)?;
    let mut existing = existing;

    let mut imported = 0usize;
    let mut skipped = 0usize;

    for (k, v) in &archive {
        if !k.starts_with("sessions/") {
            continue;
        }
        let rel = k.trim_start_matches("sessions/");
        // Keep it simple: only allow filenames, not nested paths.
        if rel.contains('/') || rel.contains('\\') {
            anyhow::bail!("refusing to import unsafe session path from bundle: {k}");
        }
        let base = rel.to_string();
        let out_name = if existing.contains(&base) {
            let out_path = sessions_dir.join(&base);
            if let Ok(existing_content) = fs::read_to_string(&out_path) {
                if existing_content == *v {
                    skipped += 1;
                    continue;
                }
            }
            allocate_unique_filename(&base, &existing)
        } else {
            base
        };

        let out_path = sessions_dir.join(&out_name);
        ensure_safe_session_write_target(&sessions_dir, &out_path)?;
        fs::write(&out_path, v).with_context(|| format!("write {}", out_path.display()))?;
        existing.insert(out_name);
        imported += 1;
    }

    println!("Imported {} session(s) ({} skipped).", imported, skipped);
    Ok(())
}

fn normalize_id(id: &str) -> String {
    id.trim().trim_end_matches(".age").to_string()
}

fn validate_id(id: &str) -> Result<()> {
    anyhow::ensure!(!id.is_empty(), "bundle id cannot be empty");
    anyhow::ensure!(
        !id.contains('/') && !id.contains('\\') && !id.contains(".."),
        "invalid bundle id"
    );
    Ok(())
}

fn allocate_unique_filename(base: &str, existing: &HashSet<String>) -> String {
    if !existing.contains(base) {
        return base.to_string();
    }
    let stem = base.strip_suffix(".md").unwrap_or(base);
    for i in 1u32.. {
        let candidate = format!("{stem}__import{i}.md");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("exhausted filename suffix space")
}

fn list_existing_sessions(dir: &Path) -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    if !dir.is_dir() {
        return Ok(names);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with(".md") {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names)
}

fn ensure_safe_session_write_target(sessions_dir: &Path, out_path: &Path) -> Result<()> {
    anyhow::ensure!(
        out_path.starts_with(sessions_dir),
        "refusing to write outside sessions dir: {}",
        out_path.display()
    );
    if let Ok(meta) = fs::symlink_metadata(sessions_dir) {
        anyhow::ensure!(
            !meta.file_type().is_symlink(),
            "refusing symlinked sessions dir: {}",
            sessions_dir.display()
        );
    }
    if let Ok(meta) = fs::symlink_metadata(out_path) {
        anyhow::ensure!(
            !meta.file_type().is_symlink(),
            "refusing to write to symlinked session target: {}",
            out_path.display()
        );
    }
    Ok(())
}

fn safe_sessions_join(sessions_dir: &Path, rel_path: &str) -> Result<PathBuf> {
    let rel = Path::new(rel_path);
    let mut out = sessions_dir.to_path_buf();
    let mut added = false;

    for comp in rel.components() {
        match comp {
            Component::Normal(part) => {
                out.push(part);
                added = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("refusing to read unsafe session path: {rel_path}");
            }
        }
    }

    anyhow::ensure!(added, "refusing to read unsafe session path: {rel_path}");
    Ok(out)
}

fn read_git_file(repo_root: &Path, rel_path: &str) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(["log", "--all", "-n", "1", "--format=%H", "--", rel_path])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run git log --all -- {rel_path}"))?;
    anyhow::ensure!(output.status.success(), "git log failed for {rel_path}");
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    anyhow::ensure!(!sha.is_empty(), "bundle not found: {rel_path}");

    let spec = format!("{sha}:{rel_path}");
    let output = Command::new("git")
        .args(["show", &spec])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run git show {spec}"))?;
    anyhow::ensure!(output.status.success(), "git show failed for {spec}");
    Ok(output.stdout)
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    anyhow::ensure!(output.status.success(), "git {} failed", args.join(" "));
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn generate_bundle_id() -> String {
    // 12 hex chars (6 bytes) is short but collision-resistant enough for local use.
    if let Some(bytes) = random_bytes(6) {
        return to_hex(&bytes);
    }

    // Fallback: time-based, base36.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

#[cfg(unix)]
fn random_bytes(n: usize) -> Option<Vec<u8>> {
    let mut f = fs::File::open("/dev/urandom").ok()?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

#[cfg(not(unix))]
fn random_bytes(_n: usize) -> Option<Vec<u8>> {
    None
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{normalize_id, validate_id};

    #[test]
    fn accepts_simple_id() {
        validate_id("abc123").unwrap();
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_id("../pwn").is_err());
        assert!(validate_id("a/b").is_err());
    }

    #[test]
    fn strips_extension() {
        assert_eq!(normalize_id("abc.age"), "abc");
    }
}
