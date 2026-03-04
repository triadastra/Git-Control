# Git Control

Git Control is a local Rust desktop app that makes daily Git operations task-driven and safer than raw command workflows.

## What is implemented

- Repository discovery rail with branch and health chips (`staged`, `unstaged`, `conflicts`, `ahead/behind`)
- Multi-pane desktop UX:
  - `Changes` (quick actions, file filter, per-file stage/unstage, guided commit flow)
  - `History Graph` (recent commit timeline with visual commit nodes)
  - `Branch Lab` (create + checkout, switch branches)
  - `Sync` (ahead/behind, real fetch/pull/push actions, command output panel)
  - `Conflict Studio` (AI provider + strategy picker, suggestion apply/stage)
  - `Recovery Center` (reflog-backed reset flow with arm/confirm)
- AI conflict agents:
  - `Local Heuristic` provider (offline)
  - `OpenAI` provider (remote, asynchronous request in UI)
  - `Keep Ours`
  - `Keep Theirs`
  - `Smart Blend` (line-level dedupe merge)
- Command palette and shortcuts:
  - `Cmd/Ctrl+K` open command palette
  - `Cmd/Ctrl+R` refresh repositories
  - `Cmd/Ctrl+Enter` commit
- Safe UX touches:
  - explicit status footer
  - risk indicator in inspector
  - two-step reset in recovery
  - conflict marker guard before applying edited AI output

## OpenAI setup (optional)

1. Export `OPENAI_API_KEY` before launching, or paste it into the Conflict Studio settings panel.
2. Default model is `gpt-4.1-mini`; you can change model and base URL in-app.
3. Open Conflict Studio, select `OpenAI`, pick a strategy, then click `Generate AI Suggestion`.

## Run

```bash
cargo run
```

## Development checks

```bash
cargo check
cargo test
```

## Architecture

- `src/main.rs`: app boot and native window options
- `src/app.rs`: UX/state/workflow screens
- `src/git_service.rs`: libgit2-backed repository read/write operations
- `src/ai_agent.rs`: local AI conflict-resolution agent and parser tests

## Notes

- This MVP is local-first and does not require cloud APIs.
- Remote auth-heavy actions (`pull/push/fetch`) are intentionally presented as guided commands in UI rather than opaque background operations.
# Git-Control
