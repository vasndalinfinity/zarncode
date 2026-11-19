# Zarncode

AI-powered terminal code assistant built with Rust.

## Features

### Core Experience
- âœ… **TUI Interface** â€” Built with `ratatui` for an interactive terminal experience
- âœ… **Streaming Responses** â€” Real-time LLM response streaming with thinking/reasoning support
- âœ… **Input History** â€” Shell-like Up/Down navigation with persistent `history.jsonl`
- âœ… **Configuration via TOML** â€” Customizable models, providers, and settings in `~/.zarncode/config.toml`

### LLM & Providers
- âœ… **Kimi Provider** â€” Native Kimi API support with Coding Agent authentication headers
- âœ… **Thinking Mode** â€” Supports `<think>` reasoning content extraction
- âœ… **OpenAI-Compatible Framework** â€” Extensible provider trait for adding new models
- âœ… **Context Compaction** â€” Automatic context compression with rolling window strategy
- âœ… **Auto-Retry with Backoff** â€” Exponential backoff for failed requests

### Tool System
- âœ… **ReadFile** â€” Read file contents with line numbers and offset/limit
- âœ… **WriteFile** â€” Write or append content to files
- âœ… **ReplaceFile** â€” String replacement within files (single or multiple edits)
- âœ… **Glob** â€” Find files matching glob patterns
- âœ… **Grep** â€” Search file contents with ripgrep integration
- âœ… **Bash / PowerShell** â€” Execute shell commands with timeout support
- âœ… **SearchWeb** â€” Web search via DuckDuckGo
- âœ… **FetchURL** â€” Fetch and extract article content from URLs
- âœ… **SetTodoList** â€” Track task progress visually
- âœ… **AskUserQuestion** â€” Structured question definitions (TUI integration pending)
- âŒ **MCP Support** â€” Model Context Protocol server integration
- âŒ **Git Tools** â€” Diff, blame, log integration
- âŒ **LSP Integration** â€” Code completion and go-to-definition
- âŒ **AST-Aware Tools** â€” Code parsing and semantic analysis

### Sessions & Persistence
- âœ… **Session Persistence** â€” JSONL-based storage (`meta.json` + `context.jsonl`)
- âœ… **Resume by ID** â€” `zarncode --session <ID>` to resume a specific session
- âœ… **Resume Latest** â€” `zarncode --continue` to continue the most recent session
- âœ… **Auto-Save** â€” Automatic persistence on messages, tool results, and history clears
- âœ… **Session Metadata** â€” Title, timestamps, and message tracking
- âŒ **Session List UI** â€” Browse and load historical sessions from within the TUI
- âŒ **Checkpoints / D-Mail** â€” Snapshot and rollback to previous conversation states

### CLI & Modes
- âœ… **Custom Config Directory** â€” `-c / --config` to specify config location
- âœ… **Print / Non-Interactive Mode** â€” `--print`, piped input, model selection, and JSONL events
- âœ… **YOLO Mode** â€” Explicit opt-in to unattended tool execution
- âœ… **Approval System** â€” Interactive approvals and fail-closed headless handling

### Advanced Features
- âœ… **Agents / Subagents** â€” Named primary/subagent profiles, bounded delegation, and enforced tool policies
- âŒ **Skill System** â€” Reusable skills and flow-based workflows
- âŒ **Web UI** â€” FastAPI + WebSocket alternative interface
- âŒ **ACP Protocol** â€” Agent Client Protocol for IDE integration
- âŒ **OAuth Authentication** â€” Device flow token management
- âŒ **Vision / Image Input** â€” Multimodal message support

## Terminal design

A shared dark terminal shell powers live chat and `--preview`: warm accents, a branded welcome screen, quieter message panels, an always-visible composer, and a compact model/context footer. Layouts adapt to narrow terminals; approval and question panels retain their own space.

The visual direction takes inspiration from OpenCode, Freebuff, and Claude Code while retaining Zarncode branding and its existing commands. This intentionally differs from the original Kimi-parity visual guidance; inference and tool permissions are unchanged.

## Token usage

The footer shows the last reported requestâ€™s **Input, Output, Cache reads, and Total** tokens, plus persistent local totals for a **rolling 5 hours**, **rolling 7-day week**, and **all tracked usage**. Use `/usage` for exact counts and input/output/cache breakdowns for every window.

Accounting includes main-agent and subagent requests using the shared provider factory, survives restarts in `<data_dir>/usage.jsonl`, and records each streamâ€™s final reported counters once. Cache reads are already part of input; they are not added to total again. Missing provider cache details appear as `n/a`, not a fabricated zero. No prompts or credentials are written to the usage ledger.

These are **locally tracked token totals, not provider subscription limits or quota percentages**. Account-wide usage, remaining allowances, and reset times need an authoritative provider/gateway usage API; they are not guessed. Usage before tracking was enabled is not reconstructed. Offline preview makes no inference requests and therefore shows no reported tokens.

## Automation harness

Interactive and headless modes share the same session actor, tool loop, persistence, hooks, and provider usage accounting.

```powershell
zarncode --print "Explain Rust ownership"
Get-Content .\src\main.rs -Raw | zarncode --print "Review this code"
zarncode --print "Continue the review" --continue --output-format jsonl
zarncode --print "Explain this design" --model "kimi/kimi-for-coding"
```

- `--print` / `-p` accepts a positional prompt, UTF-8 stdin, or both. Stdin is capped at 16 MiB. Piped or redirected interactive mode is rejected with guidance to use `--print`.
- Text output contains the final assistant response only. `--output-format jsonl` emits schema-versioned `init`, `turn_start`, `content_delta`, `tool_start`, `tool_end`, `usage`, and terminal `result` records. Usage includes input/output/cache/total; absent cache reports remain null. Provider thinking is not exported by this adapter.
- `--model` selects an existing configured alias in either mode. `--session ID` and `--continue` resume normal saved sessions. Conflicting session-selection flags are rejected.
- Exit codes: **0** success or a downstream closed output pipe; **1** configuration/provider/runtime failure; **2** invalid CLI/input; **3** interactive approval/question/hook required; **130** interrupted. Parser errors use normal stderr diagnostics; valid JSONL invocations receive a terminal result even for startup failures.
- Approval-required actions fail closed by default, even if config or resumed metadata previously enabled YOLO. An explicit `--yolo` opts into unattended tool execution. Configured shell hooks remain trusted executable configuration; this is not a read-only sandbox. Direct owned shell children are killed when cancelled execution drops, but scripts may create detached descendants outside that guarantee.
- Actors are owned and closed when the client exits; abandoned provider streams are dropped. Interactive error paths restore the terminal screen and raw mode. Task completion waits for Stop-hook continuation, with one continuation maximum.

## Steering and follow-up queues

In interactive mode, **Enter** steers active work and **Alt+Enter** queues a follow-up for after the current task. **Shift+Enter** inserts a newline. When an approval or question panel owns focus, **Ctrl+G** switches to the composer and back; composing never approves the pending operation. Use slash commands when your terminal intercepts a shortcut.

```text
/steer Focus on security and edge cases
/followup Summarize the findings when this task is done
/queue
/queue pause
/queue resume
/queue edit ID replacement message
/queue drop ID
```

IDs come from `/queue`; unique prefixes of at least eight characters work. Steering has priority, with FIFO ordering within each kind. Input is delivered only at a safe model boundary, after the current response/tools and pending approvals/questions settle. It never changes an in-flight provider request or bypasses approval.

**Ctrl+C** cancels at the actorâ€™s safe command boundary and pauses pending input. It does not undo effects or promise to preempt a running tool. Queue contents survive restart; recovered pending work starts paused and requires `/queue resume`. Prepared deliveries use a persisted user-message ordinal to avoid appending the same prompt twice after an interrupted save. This is not an exactly-once guarantee for provider requests, hooks, or external effects. A prepared delivery must be recovered before editing/removing it; clearing context resets that preparation.

The queue is bounded to 64 messages and 16 MiB total. Failed sends and locally detected full queues leave the draft in the editor. Queued input is displayed separately until the actor acknowledges delivery. `--print` remains a single-task client, not a persistent interactive command socket.

## Agent profiles and resource limits

Agent management works without inference credentials:

```powershell
zarncode agent create reviewer --prompt "Review correctness and security; explain changes rather than making them." --tool ReadFile --tool Glob --tool Grep --max-steps 3
zarncode agent list
zarncode agent validate
zarncode --agent reviewer --max-steps 2
zarncode --max-steps 6 --max-concurrent-agents 2
```

Definitions live in `<config_dir>/agents/NAME.yaml` (normally `~/.zarncode/agents`). They use `schema_version: 1`, a matching portable filename/name, description, prompt, optional configured model alias, `tools`, `max_steps`, `mode` (`primary`, `subagent`, or `both`), and `supports_background`. Creation defaults to read-only tools, mode `both`, five steps, and no background support, and never overwrites a file. A ready-to-copy example is `artifacts/agents/reviewer.yaml`. Validation checks schema; model aliases and executable handlers are checked at runtime startup.

`--agent` selects primary/both profiles; the `Agent` tool can select subagent/both profiles from the same registry. Custom types are advertised in its tool schema. An explicit `--model` overrides the profileâ€™s default. Saved sessions remember their profile; resume restores its prompt/model/tool restrictions rather than silently returning to a full-access default. Workspace/project agent files are not automatically trusted or loaded.

- `--max-steps` caps logical model steps per task and per delegated agent (default 10, range 1..10000). Definition caps can only reduce the inherited parent cap. Steering and Stop-hook continuation do not reset the current taskâ€™s allowance; a follow-up is a new task. Transport retries and internal compaction are separate, so this is not a token/spend or raw HTTP-request quota.
- `--max-concurrent-agents` limits admitted foreground and background workers in this invocation (default 3, range 1..32). Background jobs retain their slot until completion/cancellation. Capacity exhaustion produces a tool error instead of launching extra workers.
- Child tool access is the intersection of parent, definition, and launch restrictions, checked before virtual or native tool dispatch. `--yolo` cannot widen these allowlists. Recursive child delegation is deliberately disabled. Background task payloads retain their effective step/tool limits.
- Foreground collection uses an absolute deadline and waits for settled completion, including Stop-hook continuation; failures are not labeled completed. Owned child actors/provider streams are cancelled with their jobs. None of this is a filesystem/process sandbox or an account-wide billing quota.

Core tool schemas and the default system prompt are embedded in the binary. Missing user assets no longer leave installed CLIs without tools; an explicit `<data_dir>/prompts/tools` folder remains an override (including an intentionally empty one). Tool availability is platform-filtered and always enforced at dispatch.

Remaining harness work includes branch/rewind, broader interrupted-tool recovery, token/spend budgets, additional providers, and gateway integration.

## Zarnkina account and quota

Zarncode ships a first-class provider for the Zarnkina relay.

```bash
zarncode auth login              # Google sign-in through the browser
zarncode auth login --api-key zk_live_...   # API key instead
zarncode auth status             # session, tier, catalogue size, quota
zarncode auth logout
zarncode models                  # relay catalogue with tier and status
zarncode quota                   # 5h and weekly remaining
zarncode quota --reset           # rebuild a corrupt ledger
```

Every turn passes three gates before any network call: the account must be
allowed, the model must be active (and Pro when it is marked `pro`), and the
local quota ledger must have room. The ledger lives in `quota.json` under the
data directory, is updated under an exclusive file lock, and fails closed: a
corrupt file refuses turns until `zarncode quota --reset` rebuilds it.
A reservation is refunded when a stream delivers nothing, so a failed request
never costs a prompt.

Weights mirror the desktop client (5h window, weekly window): Low models cost
`0.05%`/`0.0035%`, Standard `0.10%`/`0.007%`, High `0.25%`/`0.0175%` per prompt.

Use `--no-gate` (or `[quota] enforce = false`) to bypass the account and quota
checks for local testing, for example against a loopback relay.

## Build toolchain

## Build toolchain

The project is pinned to Rust **1.97.1**. In this Windows environment the old 1.91 toolchain crashed during code generation even for a minimal program; the installed 1.97.1 toolchain successfully builds and runs the harness tests. The pin is project-local; no global default toolchain was changed.

## Quick Start

Build and try the real terminal renderer without credentials or network access:

```bash
cargo run -- --preview
```

Type a message and press Enter; Escape or Ctrl+C exits. Replies in this mode are explicitly local demo text, not inference. When output is redirected, the same renderer prints a static terminal snapshot.

For a live session, configure a provider as described below and run `cargo run --` (or install locally with `cargo install --path .`, then run `zarncode`). Zarnkina integration is planned; the currently implemented provider is Kimi.

```bash
# Run with a new session (default)
zarncode

# Continue the most recent session
zarncode --continue

# Resume a specific session
zarncode --session <SESSION_ID>

# Use a custom config directory
zarncode -c /path/to/config
```

## Existing IronCode data

The new defaults are `~/.zarncode/` and `logs/zarncode.log`. Existing `~/.ironcode/` files are never moved, deleted, or overwritten automatically. To keep using them, run `zarncode --config <home>/.ironcode` and set `dir = "~/.ironcode"` in that configuration so sessions and logs keep using the legacy data directory. Legacy `${IRONCODE_*}` prompt placeholders remain accepted alongside `${ZARNCODE_*}`.

## Configuration

Place your `config.toml` in `~/.zarncode/`:

```toml
default_model = "kimi/kimi-for-coding"

[providers.kimi]
type = "kimi"
base_url = "https://zarnkina-relay.hostless.app/v1"
api_key = "${ZARNKINA_API_KEY}"

[models."kimi/kimi-for-coding"]
provider = "kimi"
model = "kimi-for-coding"
max_context_size = 128000
supports_streaming = true

# Context compaction settings (auto-compression when approaching token limits)
[compaction]
enabled = true
trigger_ratio = 0.85
reserved_context_size = 50000

# Retry settings (exponential backoff for failed LLM requests)
[retry]
max_attempts = 3
initial_delay_ms = 1000
max_delay_ms = 30000
```

### Compaction Configuration

The `[compaction]` section controls automatic context compression to prevent exceeding the model's context window:

- `enabled` â€” Enable auto-compaction (default: `true`)
- `trigger_ratio` â€” Token usage ratio threshold (0.5-0.99) that triggers compaction (default: `0.85`)
- `reserved_context_size` â€” Reserved tokens for LLM response generation (default: `50000`)

Compaction triggers when **either** condition is met:
- `current_tokens >= max_context_size * trigger_ratio`
- `current_tokens + reserved_context_size >= max_context_size`
