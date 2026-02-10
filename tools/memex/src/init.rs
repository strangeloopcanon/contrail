use crate::detect;
use crate::types::DetectedAgents;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const COMPACT_PROMPT: &str = r#"You are compacting a conversation to preserve what is needed to continue work.

Hard requirements:
- Restate the current objective in 1-2 sentences.
- Keep decisions, constraints, and non-obvious insights.
- Drop repetition, greetings, abandoned branches, verbose logs.
- For long content (logs, diffs, stack traces, large code blocks), replace with:
  - a one-line gist
  - key identifiers (file path, command, error signature)
  - search keys for finding full detail in .context/sessions/

Output format:
# Objective
# What we know
# Decisions and constraints
# Current plan
# Open questions / risks
# Pointers to archived detail
"#;

const LEARNINGS_HEADER: &str = "# Learnings\n\nAccumulated notes from coding sessions. Append decisions, pitfalls, and patterns here.\n";

const AGENT_INSTRUCTION: &str = r#"## Context
- Past session transcripts are in `.context/sessions/` (one file per session).
- Read recent sessions or grep for keywords when you need context about previous work.
- Append decisions, pitfalls, and patterns to `.context/LEARNINGS.md`.
- Run `memex sync` if sessions look stale.
"#;

const AGENT_MARKER: &str = "Past session transcripts are in `.context/sessions/`";

const CURSOR_RULE: &str = r#"---
description: Project context from past sessions
alwaysApply: true
---
Past session transcripts are in .context/sessions/. Read recent ones
or grep when you need context about previous work. Append decisions,
pitfalls, and patterns to .context/LEARNINGS.md.
Run `memex sync` if sessions look stale.
"#;

pub fn run_init(repo_root: &Path) -> Result<()> {
    let agents = detect::detect_agents(repo_root);
    if !agents.any() {
        println!("No agent history found for this repo. Creating .context/ anyway.");
    }

    // 1. Create .context/sessions/
    let context_dir = repo_root.join(".context");
    let sessions_dir = context_dir.join("sessions");
    fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("create {}", sessions_dir.display()))?;

    // Write .gitkeep so git tracks the empty dir
    let gitkeep = sessions_dir.join(".gitkeep");
    if !gitkeep.exists() {
        fs::write(&gitkeep, "")?;
    }

    // 2. Write compact prompt
    let compact_path = context_dir.join("compact_prompt.md");
    write_if_missing(&compact_path, COMPACT_PROMPT, "compact_prompt.md")?;

    // 3. Write LEARNINGS.md
    let learnings_path = context_dir.join("LEARNINGS.md");
    write_if_missing(&learnings_path, LEARNINGS_HEADER, "LEARNINGS.md")?;

    // 4. Write agent-specific files
    write_agent_files(repo_root, &agents)?;

    // 5. Install git hook
    install_git_hook(repo_root)?;

    // 6. Summary
    print_summary(repo_root, &agents);

    Ok(())
}

fn write_agent_files(repo_root: &Path, agents: &DetectedAgents) -> Result<()> {
    // Codex: patch AGENTS.md
    if agents.codex {
        let agents_md = repo_root.join("AGENTS.md");
        append_section_if_missing(&agents_md, AGENT_INSTRUCTION, AGENT_MARKER)?;

        // Write .codex/config.toml entry for compact prompt
        let codex_dir = repo_root.join(".codex");
        fs::create_dir_all(&codex_dir)?;
        let codex_config = codex_dir.join("config.toml");
        append_codex_compact_config(&codex_config)?;
    }

    // Claude Code: CLAUDE.md
    if agents.claude {
        let claude_md = repo_root.join("CLAUDE.md");
        append_section_if_missing(&claude_md, AGENT_INSTRUCTION, AGENT_MARKER)?;
    }

    // Cursor: .cursor/rules/memex.mdc
    if agents.cursor {
        let rules_dir = repo_root.join(".cursor/rules");
        fs::create_dir_all(&rules_dir)?;
        let mdc_path = rules_dir.join("memex.mdc");
        write_if_missing(&mdc_path, CURSOR_RULE, ".cursor/rules/memex.mdc")?;
    }

    // Gemini: GEMINI.md
    if agents.gemini {
        let gemini_md = repo_root.join("GEMINI.md");
        append_section_if_missing(&gemini_md, AGENT_INSTRUCTION, AGENT_MARKER)?;
    }

    Ok(())
}

fn write_if_missing(path: &Path, content: &str, label: &str) -> Result<()> {
    if path.exists() {
        println!("  skip {} (already exists)", label);
    } else {
        fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
        println!("  wrote {}", label);
    }
    Ok(())
}

fn append_section_if_missing(path: &Path, section: &str, marker: &str) -> Result<()> {
    let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");

    if path.exists() {
        let existing = fs::read_to_string(path)?;
        if existing.contains(marker) {
            println!("  skip {} (memex section already present)", label);
            return Ok(());
        }
        // Append to existing file
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str(section);
        fs::write(path, content)?;
        println!("  patched {} (appended context section)", label);
    } else {
        fs::write(path, section)?;
        println!("  wrote {}", label);
    }
    Ok(())
}

fn append_codex_compact_config(config_path: &Path) -> Result<()> {
    let compact_line = "experimental_compact_prompt_file = \"../.context/compact_prompt.md\"";

    if config_path.exists() {
        let existing = fs::read_to_string(config_path)?;
        if existing.contains("experimental_compact_prompt_file") {
            println!("  skip .codex/config.toml (compact prompt already configured)");
            return Ok(());
        }
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(compact_line);
        content.push('\n');
        fs::write(config_path, content)?;
        println!("  patched .codex/config.toml (added compact prompt path)");
    } else {
        fs::write(config_path, format!("{compact_line}\n"))?;
        println!("  wrote .codex/config.toml");
    }
    Ok(())
}

const HOOK_SCRIPT: &str = r#"#!/bin/sh
# memex post-checkout hook: sync session transcripts after checkout.
# Disable with MEMEX_HOOK=0 in your environment.
# Remove this block from .git/hooks/post-checkout to uninstall the hook.

if [ "${MEMEX_HOOK:-1}" = "0" ]; then
    exit 0
fi

# Only run if memex is on PATH
if command -v memex >/dev/null 2>&1; then
    memex sync --quiet &
fi
"#;

const HOOK_MARKER: &str = "# memex post-checkout hook";

const POST_COMMIT_HOOK_SCRIPT: &str = r#"#!/bin/sh
# memex post-commit hook: link commit to active agent sessions.
# Disable with MEMEX_HOOK=0 in your environment.

if [ "${MEMEX_HOOK:-1}" = "0" ]; then
    exit 0
fi

# Only run if memex is on PATH
if command -v memex >/dev/null 2>&1; then
    memex link-commit --quiet &
fi
"#;

const POST_COMMIT_HOOK_MARKER: &str = "# memex post-commit hook";

fn install_git_hook(repo_root: &Path) -> Result<()> {
    let hooks_dir = repo_root.join(".git/hooks");
    if !hooks_dir.is_dir() {
        println!("  skip git hooks (not a git repo or .git/hooks missing)");
        return Ok(());
    }

    install_single_hook(&hooks_dir, "post-checkout", HOOK_SCRIPT, HOOK_MARKER)?;

    install_single_hook(
        &hooks_dir,
        "post-commit",
        POST_COMMIT_HOOK_SCRIPT,
        POST_COMMIT_HOOK_MARKER,
    )?;

    Ok(())
}

fn install_single_hook(
    hooks_dir: &Path,
    hook_name: &str,
    script: &str,
    marker: &str,
) -> Result<()> {
    let hook_path = hooks_dir.join(hook_name);

    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path)?;
        if existing.contains(marker) {
            println!("  skip .git/hooks/{} (already installed)", hook_name);
            return Ok(());
        }
        // Append to existing hook
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        // Skip the shebang from our script since the file already has one
        let hook_body = script.strip_prefix("#!/bin/sh\n").unwrap_or(script);
        content.push_str(hook_body);
        fs::write(&hook_path, content)?;
        set_executable(&hook_path);
        println!("  patched .git/hooks/{} (appended memex hook)", hook_name);
    } else {
        fs::write(&hook_path, script)?;
        set_executable(&hook_path);
        println!("  wrote .git/hooks/{}", hook_name);
    }

    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        let _ = fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

fn print_summary(repo_root: &Path, agents: &DetectedAgents) {
    println!();
    println!("memex initialized in {}", repo_root.display());
    println!();

    let mut detected = Vec::new();
    if agents.cursor {
        detected.push("Cursor");
    }
    if agents.codex {
        detected.push("Codex");
    }
    if agents.claude {
        detected.push("Claude Code");
    }
    if agents.gemini {
        detected.push("Gemini");
    }

    if detected.is_empty() {
        println!("  Agents detected: (none yet -- start using an agent in this repo)");
    } else {
        println!("  Agents detected: {}", detected.join(", "));
    }

    println!();
    println!("  Git hooks:");
    println!("    post-checkout  — runs `memex sync` on branch switch");
    println!("    post-commit    — links commits to active agent sessions");
    println!("    Disable both with MEMEX_HOOK=0 in your environment.");
    println!();
    println!("Next: run `memex sync` to pull in past session transcripts.");
}
