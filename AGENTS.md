---
name: workspace-agent
description: Operating manual for autonomous coding agents across this monolith workspace. Default to AGENT_MODE=baseline; escalate per Appendix E triggers.
mode: baseline
---

# Drop‑In Rules for Autonomous Coding Agents
> Constitution + lean appendices; ship minimal, correct, succinct code.

## Quick start (read this first)
- Set `AGENT_MODE` (baseline by default; see Appendix E for production triggers).
- Run `bd init` before work; track plan/risks/tests/failures in bd (no Markdown checklists).
- Prefer `uv` over `pip`.
- Run Interface Contract commands (stop after first failure):
  - `setup` / `bootstrap`: env + deps; install/refresh hooks (pre-commit, commitlint).
  - `check`: format check, lint, types (mode-appropriate), quick Bandit, detect-secrets on staged changes.
  - `test`: unit/integration; enforce coverage by mode; no live network unless explicitly marked.
  - `llm-live` (if LLM code exists): goldens with cost + p95 latency.
  - `deps-audit`: advisory in baseline; blocking in production; SBOM in production.
  - `all`: `check` → `test` (+ `llm-live` if applicable) → (`deps-audit` in production).
  - `release` (production): semantic release + changelog + tag.
- Local vs CI: local runs fast gates; CI enforces full suite (strict types in production, LLM-live, SAST, dep audit).
Agents MUST call these targets, not raw tools.

## Stack & paths (project knowledge)
- Monolith, single process; all architecture code lives here. Modules only call via `some_module/api.py` with statically typed interfaces (Pydantic preferred; no opaque dicts). CI/pre-commit enforce this.
- Read from module code; write only through declared interfaces and allowed dirs for the task.
- `.env` is the source of secrets; never commit secrets.
- Do not touch vendor/prod configs, generated/binary assets, or `node_modules/`/`vendor/` unless asked.

## Boundaries
- **Always:** follow Interface Contract commands; prefer `uv`; update README/docstrings when behaviour changes (start with “so what”; make optional sections collapsible; avoid exhaustive technical lists); structured logs (JSONL, no f-strings in log calls; pass fields); stop and ask for help if blocked; find root cause (no band-aids).
- **Ask first:** schema changes; new dependencies; CI/hook policy changes; module renames; prod/dev deploys; changing coverage/type thresholds.
- **Never:** commit secrets/PII; edit prod configs or generated/vendor folders; add “improved/better/new” comments; push directly to `main`; skip tests because they fail; hard-reset user work.

## Quality gates by mode
| Topic | Baseline | Production |
| --- | --- | --- |
| Format | Prettier (JS/TS) | + Black (Py) |
| Lint | Ruff defaults; ESLint v9 flat | Expanded rules; exceptions documented |
| Types | Basic | `mypy --strict`; TS `strict`; no lingering ignores |
| Coverage | Global ≥80%; no regression | Global ≥90%; changed lines ≥90%; mutation tests on critical modules (scheduled) |
| LLM live | Mandatory if LLM code exists | + faithfulness/relevance, OWASP LLM Top-10 probes, SLO gates |
| SAST | Bandit advisory; Semgrep minimal advisory | Bandit + Semgrep blocking on high severity |
| Deps | `pip-audit` advisory | `pip-audit` blocking; SBOM |
| Releases | Conventional Commits | Semantic release + changelog |

## Development cycle
1) Plan and log assumptions (bd).  
2) Implement smallest viable slice with typed interfaces.  
3) Run Interface Contract (halt on first failure; triage via Appendix B).  
4) Open PR with tests (LLM live if relevant), risks, rollback.  
5) If blocked, split scope and log deferrals as issues.  
6) If asked to update GitHub: create PR, merge via CLI, sync, return to main, confirm.

## Tooling and style
- Format/Lint: Black, Ruff (Py); ESLint v9 + Prettier (JS/TS); flake8/pylint if present.
- Types: mypy; `tsc`.
- Tests: pytest + pytest-cov; property-based where stateful/numeric/parser-like; no live network in unit tests; integration uses local fakes unless marked.
- Secrets: detect-secrets (pre-commit) blocks new secrets.
- Security: Bandit; Semgrep (advisory → blocking in production).
- Deps audit: `pip-audit`.
- Repo ops: `gh`/`glab`; commitlint.
- Python env: use existing venv or create/log if needed; prefer `uv` installs.
- OS: macOS (M4).

## LLM-specific rules
- Live tests are non-negotiable; staging credentials only.  
- Output shape: choose JSON vs free-text; validate (JSON Schema/Pydantic) or enforce invariants.  
- Providers: lock provider+model+version for goldens; default GPT-5 unless specified; see https://github.com/strangeloopcanon/llm-api-hub for latest docs.  
- Determinism: `temperature=0`, `top-p=1` for goldens.  
- Retries: transient (5xx/429/timeout) up to 3 attempts with jitter; deterministic errors fail fast. Max 9 total calls/job; if exhausted on transients, exit code 3.  
- Cost ceilings: Baseline ≤ $3, Production ≤ $10; exit code 2 if exceeded (advisory in baseline).  
- Logging: redact PII/secrets; no raw model I/O when user data may appear.

## Git & PR workflow
- Branches: `feat/*`, `fix/*`, `chore/*` off `main`.  
- Commits: Conventional Commits; use `BREAKING CHANGE:` footer when needed.  
- PRs: summary, rationale, reproduction commands; link issues; focused diffs.  
- Checklist: format/lint; types by mode; tests + coverage; LLM live (or N/A reason); secrets/SAST clean; dep audit reviewed; docs updated; if rollback claimed, add `@pytest.mark.rollback` test.

## Observability & runtime safety
- Structured JSONL logs with timestamp, level, logger, message, context; pass fields (no log f-strings).  
- Feature flags for risky changes, default off.  
- No PII in logs; redact model I/O when user data may appear.  
- Fixed seeds/timezones; stub clocks/network for determinism.  
- Use ephemeral resources; clean up on failure; do not re-run passing gates in the same job.

## Security & data classes
- Data classes: secrets (credentials/tokens/keys), PII (names/emails/phones/addresses/IDs), sensitive business context. Keep secrets in `.env`; never commit.  
- detect-secrets baseline blocks new secrets; least-privilege for tokens/CI; rotate on leaks.  
- Logging: ensure tests/fixtures assert no emails/SSNs/API keys appear in captured logs.  
- Dependency audits: `pip-audit`; Semgrep/Bandit per mode; SBOM in production.  

## Agent templates (spawn per task)
Use this skeleton for task agents:
```
---
name: <agent-name>
description: <one-line>
---
## Persona
- You are <role>, focused on <scope>.
## Project knowledge
- Stack: <tech + versions>
- Paths: read <dirs>; write <dirs>; never touch <dirs>.
## Commands
- setup/check/test/all [...]
- Task-specific: <commands with flags>
## Standards
- Follow root AGENTS.md rules; naming/style per examples.
## Boundaries
- Always: ...
- Ask first: ...
- Never: ...
```

- **Docs agent:** reads `src/`, writes `docs/`; commands: `npm run docs:build`, `npx markdownlint docs/`; never modify `src/`.
- **Test agent:** writes to `tests/`; commands: `pytest -v` (or `npm test`/`cargo test --coverage`); never delete failing tests; ask before schema changes.
- **Lint agent:** commands: `npm run lint --fix` / `prettier --write` / `ruff --fix`; only style changes, never logic.

## Style example (Python)
```python
# Good: descriptive, typed, structured logging fields
from typing import List
from my_module.api import Widget
logger.info("listing_widgets", count=len(widgets), source="cache")
def list_widgets(limit: int) -> List[Widget]:
    if limit <= 0:
        raise ValueError("limit must be positive")
    return widgets[:limit]

# Bad: untyped, vague logging, f-string in log call
logger.info(f"We have {len(widgets)} widgets")
def run(widgets):
    return widgets
```

## .agents.yml
Keep thresholds/knobs in repo root; defaults: baseline mode, coverage 0.80, LLM ceilings per Quality gates. Missing keys inherit section defaults.

## Cold start (empty repo)
- Makefile/justfile with Interface Contract targets.  
- Python: `pyproject.toml` (Ruff, pytest, coverage), `requirements*.txt`, `.gitignore`, pre-commit + commitlint.  
- LLM: `tests_llm_live/` with schema validation.  

## Additional notes
- Large model downloads happen on first run; document cache locations/quotas.  
- Default backend should include MLX via mlx-genkit and PyTorch; guard behind flags.  
- No giant config dumps; synthesize only what’s needed.  
- Prefer small models for smoke tests when applicable (e.g., `Qwen/Qwen3-0.6B`) with staging keys.
- Pin dependencies; record toolchain versions when relevant.

## Appendix E — When to switch to production mode
Switch to `AGENT_MODE=production` when any apply: PII/reg data handled; >50 external users or any paying users; uptime/correctness SLOs in place; monthly LLM/infra spend > $100; privileged third-party integration (write access to user data). Record decision (date, trigger, owner) and open tracking issue tagged `AGENT_MODE=production`.

## Appendix B — Triage on failure (LLM + infra)
- Classify failure: transient infra (5xx/429/timeout → retry with jitter up to 3) vs deterministic (schema/safety/eval fail → fix).  
- If 429 persists after retries, treat as pacing/config bug.  
- Exit codes: `0` pass; `1` test/gate failure; `2` cost ceiling exceeded; `3` infrastructure failure; `4` threshold/config missing.

## Work plan / issues
All new work must be tracked in bd (not Markdown). Run `bd quickstart` / `bd init`; bd is the source of truth for steps.

# Agents

This is a Rust project.

- Rust toolchain is already installed and working.
- Do NOT run `rustup`, `rustup-init`, or any installer scripts.
- Do NOT run long-lived commands like `cargo watch` or dev servers.
- Only use:
  - `cargo check`
  - `cargo test`
  - `cargo fmt`
  - `cargo clippy`

## memex (tools/memex)

Session context layer for coding agents. Lives at `tools/memex/` and builds a `memex` binary.

- `memex init` -- sets up `.context/` in a repo, detects active agents, wires agent instructions, installs a post-checkout git hook.
- `memex sync` -- pulls recent transcripts from Cursor/Codex/Claude/Gemini native storage into `.context/sessions/`.
- Shares session parsers and DLP/redaction from the `scrapers` crate.
- Independent of the contrail daemon; they can run separately or together.
- Contrail daemon/workspace context service: [github.com/strangeloopcanon/contrail](https://github.com/strangeloopcanon/contrail)
- Usage rule: run `memex sync` at the start of context-sensitive work (handoffs, multi-session tasks, agent continuity); skip it for isolated one-off edits where prior session context is irrelevant.
- Usage rule: use contrail for repo-wide context retrieval or long-running cross-task workflows; skip it for small isolated edits.

## Context
- Past session transcripts are in `.context/sessions/` (one file per session).
- Read recent sessions or grep for keywords when you need context about previous work.
- Append decisions, pitfalls, and patterns to `.context/LEARNINGS.md`.
- Run `memex sync` if sessions look stale.
