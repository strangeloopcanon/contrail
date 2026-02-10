use age::secrecy::SecretString;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const VAULT_FILE: &str = ".context/vault.age";
pub const DEFAULT_PASSPHRASE: &str = "pa$$word";

/// Encrypt .context/sessions/ + LEARNINGS.md into .context/vault.age.
pub fn run_share(repo_root: &Path, passphrase: Option<String>) -> Result<()> {
    let context_dir = repo_root.join(".context");
    let sessions_dir = context_dir.join("sessions");

    anyhow::ensure!(
        sessions_dir.is_dir(),
        ".context/sessions/ not found. Run `memex init` first."
    );

    // Collect files to encrypt: sessions/*.md + LEARNINGS.md
    let mut archive: BTreeMap<String, String> = BTreeMap::new();

    // Session transcripts
    if sessions_dir.is_dir() {
        for entry in fs::read_dir(&sessions_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".md") {
                let content = fs::read_to_string(entry.path())?;
                let key = format!("sessions/{name_str}");
                archive.insert(key, content);
            }
        }
    }

    // LEARNINGS.md
    let learnings_path = context_dir.join("LEARNINGS.md");
    if learnings_path.is_file() {
        let content = fs::read_to_string(&learnings_path)?;
        archive.insert("LEARNINGS.md".to_string(), content);
    }

    if archive.is_empty() {
        println!("Nothing to share (no sessions or learnings found).");
        return Ok(());
    }

    // Serialize to JSON
    let plaintext = serde_json::to_vec(&archive).context("serialize archive")?;

    // Get passphrase
    let (passphrase, default_used) = passphrase_or_default(passphrase);
    if default_used {
        eprintln!(
            "warning: using default passphrase (weak). Pass --passphrase for real encryption."
        );
    }

    // Encrypt
    let encrypted = encrypt_bytes(&passphrase, &plaintext)?;

    // Write vault
    let vault_path = repo_root.join(VAULT_FILE);
    fs::write(&vault_path, &encrypted)
        .with_context(|| format!("write {}", vault_path.display()))?;

    // Update .gitignore to hide raw files, keep vault committed
    update_gitignore_for_share(repo_root)?;

    println!("Encrypted {} file(s) → {}", archive.len(), VAULT_FILE);
    println!("Give the passphrase to teammates so they can run `memex unlock`.");

    Ok(())
}

/// Decrypt .context/vault.age back into sessions/ + LEARNINGS.md.
pub fn run_unlock(repo_root: &Path, passphrase: Option<String>) -> Result<()> {
    let vault_path = repo_root.join(VAULT_FILE);
    let encrypted = if vault_path.is_file() {
        fs::read(&vault_path).with_context(|| format!("read {}", vault_path.display()))?
    } else {
        // Fall back to git history so teammates can unlock by just providing the repo
        // and passphrase, even if vault.age isn't checked out on the current branch.
        read_git_file(repo_root, VAULT_FILE)?
    };

    // Get passphrase
    let (passphrase, default_used) = passphrase_or_default(passphrase);
    if default_used {
        eprintln!(
            "warning: using default passphrase (weak). Pass --passphrase if a custom one was used."
        );
    }

    // Decrypt
    let plaintext = decrypt_bytes(&passphrase, &encrypted)?;

    // Deserialize
    let archive: BTreeMap<String, String> =
        serde_json::from_slice(&plaintext).context("corrupted vault contents")?;

    // Write files
    let context_dir = repo_root.join(".context");
    let sessions_dir = context_dir.join("sessions");
    fs::create_dir_all(&sessions_dir)?;

    let mut count = 0usize;
    for (rel_path, content) in &archive {
        let out_path = safe_context_join(&context_dir, rel_path)?;
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out_path, content)?;
        count += 1;
    }

    println!("Unlocked {} file(s) from vault.", count);
    Ok(())
}

pub fn passphrase_or_default(passphrase: Option<String>) -> (String, bool) {
    match passphrase {
        Some(p) if !p.is_empty() => (p, false),
        _ => (DEFAULT_PASSPHRASE.to_string(), true),
    }
}

pub fn encrypt_bytes(passphrase: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let secret = SecretString::from(passphrase.to_string());
    let recipient = age::scrypt::Recipient::new(secret.clone());
    age::encrypt(&recipient, plaintext).map_err(|e| anyhow::anyhow!("encryption failed: {e}"))
}

pub fn decrypt_bytes(passphrase: &str, encrypted: &[u8]) -> Result<Vec<u8>> {
    let secret = SecretString::from(passphrase.to_string());
    let identity = age::scrypt::Identity::new(secret);
    age::decrypt(&identity, encrypted)
        .map_err(|e| anyhow::anyhow!("decryption failed (wrong passphrase?): {e}"))
}

fn safe_context_join(context_dir: &Path, rel_path: &str) -> Result<PathBuf> {
    let rel = Path::new(rel_path);
    let mut out = context_dir.to_path_buf();
    let mut added = false;

    for comp in rel.components() {
        match comp {
            Component::Normal(part) => {
                out.push(part);
                added = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("refusing to write unsafe path from vault: {rel_path}");
            }
        }
    }

    anyhow::ensure!(
        added,
        "refusing to write unsafe path from vault: {rel_path}"
    );
    Ok(out)
}

fn read_git_file(repo_root: &Path, rel_path: &str) -> Result<Vec<u8>> {
    // Find the most recent commit (across all refs) that contains the file.
    let output = Command::new("git")
        .args(["log", "--all", "-n", "1", "--format=%H", "--", rel_path])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run git log --all -- {rel_path}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} not found on disk, and git log failed. Ask the repo owner to run `memex share`.",
            rel_path
        );
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    anyhow::ensure!(
        !sha.is_empty(),
        "{} not found. Ask the repo owner to run `memex share`.",
        rel_path
    );

    let spec = format!("{sha}:{rel_path}");
    let output = Command::new("git")
        .args(["show", &spec])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run git show {spec}"))?;
    anyhow::ensure!(output.status.success(), "git show failed for {spec}");
    Ok(output.stdout)
}

/// Add gitignore entries so raw sessions and LEARNINGS.md are not committed,
/// but vault.age and compact_prompt.md are.
fn update_gitignore_for_share(repo_root: &Path) -> Result<()> {
    let gitignore_path = repo_root.join(".gitignore");

    let lines_to_add = [
        "# memex: raw sessions gitignored when using share (vault.age is committed instead)",
        ".context/sessions/*.md",
        ".context/LEARNINGS.md",
    ];

    let marker = ".context/sessions/*.md";

    if gitignore_path.exists() {
        let existing = fs::read_to_string(&gitignore_path)?;
        if existing.contains(marker) {
            return Ok(());
        }
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        for line in &lines_to_add {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(&gitignore_path, content)?;
    } else {
        let content = lines_to_add.join("\n") + "\n";
        fs::write(&gitignore_path, content)?;
    }

    println!("Updated .gitignore (raw sessions excluded, vault.age committed).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::safe_context_join;
    use std::path::Path;

    #[test]
    fn safe_context_join_rejects_absolute_paths() {
        let context_dir = Path::new(".context");
        #[cfg(unix)]
        assert!(safe_context_join(context_dir, "/etc/passwd").is_err());
        #[cfg(windows)]
        assert!(
            safe_context_join(context_dir, "C:\\Windows\\System32\\drivers\\etc\\hosts").is_err()
        );
    }

    #[test]
    fn safe_context_join_rejects_parent_dir() {
        let context_dir = Path::new(".context");
        assert!(safe_context_join(context_dir, "sessions/../pwned").is_err());
    }

    #[test]
    fn safe_context_join_rejects_empty_path() {
        let context_dir = Path::new(".context");
        assert!(safe_context_join(context_dir, "").is_err());
    }

    #[test]
    fn safe_context_join_accepts_nested_paths() {
        let context_dir = Path::new(".context");
        let out = safe_context_join(context_dir, "sessions/2026-02-09.md").unwrap();
        let expected = context_dir.join("sessions").join("2026-02-09.md");
        assert_eq!(out, expected);
    }
}
