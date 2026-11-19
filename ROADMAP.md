# Zarncode: next-phase plan

Status: branding, terminal design, local usage, headless execution, durable steering/follow-up queues, per-agent step/admission limits, and named primary/subagent profiles are implemented. Branch/rewind, richer recovery/billing budgets, Zarnkina integration, and the gateway remain planned.

## Product direction

Build **Zarncode**, a Rust coding-agent CLI powered first by **Zarnkina inference**. Use OpenCode as a product reference and borrow Pi's steering, follow-up queues, and session-tree ideas without tying the implementation to either project's internals.

Product boundaries:

- **Zarnkina**: inference service/model execution. Its actual wire protocol, authentication, model discovery, and tool-calling capabilities must be confirmed with service documentation or fixtures before implementing its adapter.
- **Zarncode**: terminal UX, conversation runtime, tools, approvals, session history, agents, and subagents. All local filesystem and shell execution stays here.
- **Zarnlib**: reusable Rust inference contracts, provider adapters, stream normalization, model capabilities, errors, and credential interfaces. It is not another agent runtime.
- **Zarngate**: optional OAuth/authentication and provider-routing service, using Zarnlib. It is not required to use Zarncode directly with Zarnkina.

Implementation choice: cancellation keeps pending messages paused and editable in the queue instead of merging them back into the editor. Only named user-owned definitions load automatically; workspace trust and ad-hoc file overrides remain separate.

Interpretation: “borrow” means adopting useful Pi concepts. Branch/fork is included explicitly; no separate feature named `borrow` is assumed.

## Foundation audit (before the branding change)

Confirmed from source, not the README feature checklist:

- The package is still named `ironcode` (`Cargo.toml:2`), and `README.md` documents IronCode configuration and commands.
- `src/llm/provider.rs:42` defines `LLMProvider` and normalized streaming events, but `src/llm/session/actor.rs:2655` constructs only the Kimi implementation. A configurable base URL does not yet constitute generic provider support.
- `src/llm/session/mod.rs:52` defines session commands, including send, cancel, approval, and questions; there are no explicit steering, follow-up, or branching commands.
- `src/session/store.rs:15` stores metadata and linear message history in JSONL. It is not yet a parent-linked session tree.
- `src/subagents/mod.rs:50` already defines agent types with model overrides and tool policies. `LaborMarket::load_builtin` at line 218 loads bundled coder, explore, and plan definitions.
- `src/tools/handlers/agent/mod.rs:205` and `:330` already implement foreground and background subagent paths. Extend and test these rather than building a second delegation system.
- `AGENTS.md:5-17` currently mandates strict Kimi CLI parity. The next implementation phase should explicitly replace that product-direction constraint while retaining its Rust quality and safety rules.

No runtime tests were run to certify these existing features; these are structural findings.

## Architecture

```text
Zarncode -- direct mode --> Zarnlib --> Zarnkina inference
                                  --> other configured providers

Zarncode -- gateway mode --> Zarngate --> Zarnlib --> Zarnkina / other providers
Other clients ------------> Zarngate

Zarncode owns tools, approvals, agents, and local sessions.
Zarngate owns gateway identity, upstream credentials, routing, and quotas.
Zarnlib owns inference contracts and provider transport, not application state.
```

Default to direct mode during development. Create shared contracts inside the existing code first; extract a real library when there is a second consumer. Do not start with a distributed gateway or duplicate provider implementations.

## Harness checkpoint

Implemented: `--print`/`-p`, prompt + UTF-8 stdin, text/JSONL output with terminal results, configured `--model` selection, session resume, fail-closed unattended approvals/hooks, portable collision-resistant session IDs, owned session shutdown, and terminal restoration on errors. Interactive and automation modes reuse the same actor/tool/provider path. Local mock HTTP fixtures exercise real inference parsing and tool loops; no live Zarnkina integration is implied.

Implemented next-priority controls: durable steer/follow-up queues and queue management; editable user-owned primary/subagent YAML profiles and strict tool intersections; logical step caps and shared worker admission. Recovered queues pause for review. Next: branch/rewind, interrupted-tool recovery, broader billing budgets, and provider contract work. Gateway work remains separate.

## Delivery sequence

### 0. Establish the Zarncode baseline

- Update project direction in `AGENTS.md` and refresh the stale README against actual source behavior.
- Rename the executable and user-facing identity to `zarncode`; audit `Cargo.toml`, `src/main.rs`, `src/cli/args.rs`, configuration paths, environment variables, prompt substitutions, and detached-worker launching.
- Introduce `~/.zarncode` with an explicit, non-destructive import path for existing `~/.ironcode` configuration and sessions. Never silently overwrite either directory.
- Record a baseline of formatting, lint, and test results before behavioral changes.

Exit gate: `zarncode --help`, new-session/resume flows, and background workers use the new identity; legacy data remains readable through a documented import path.

### 1. Make Zarnkina inference a first-class provider

- Capture the real Zarnkina API contract: request/response examples, streaming framing, authentication, models, context limits, tool calls, errors, usage, and cancellation behavior. Do not assume OpenAI compatibility.
- Move provider construction out of `SessionActor` into a provider factory/registry. Both root agents and children must use this same path.
- Add a Zarnkina adapter using `src/llm/provider.rs` and `src/llm/types.rs` as starting points. Reuse compatible transport only where contract tests prove compatibility; retain Kimi as a separate adapter.
- Define explicit capabilities: streaming, tool calls, reasoning, images, context/output limits, and usage reporting. Do not advertise unsupported capabilities.
- Establish a credential-resolver interface now so later OAuth support does not require changing the agent runtime.
- Keep shell/file operations in the local tool executor. Providers receive tool schemas and return tool requests; they never receive direct local execution authority.

Exit gate: a recorded/mock streaming fixture passes through the actual adapter for text, fragmented tool arguments, usage, errors, and cancellation; one opt-in live Zarnkina smoke test succeeds when credentials are available.

### 2. Add steering and follow-up queues

Primary areas: `src/llm/session/mod.rs`, `actor.rs`, `state.rs`, `src/cli/app.rs`, and `src/view/chat/input.rs`.

- Add distinct `Steer` and `FollowUp` commands plus queue inspection, edit, and removal operations. Keep `Cancel` separate.
- Steering becomes the next user instruction at a safe boundary after the current response/tool work settles, before more autonomous work begins. It does not mutate an in-flight provider request or preempt a running shell command.
- Follow-ups wait until the current task has finished. Define task completion explicitly rather than treating every streamed response as task completion.
- Preserve FIFO ordering within each queue; steering has priority at eligible boundaries. Persist enqueue/consume identifiers to avoid duplicate delivery after resume.
- Show queued versus delivered input in the TUI. Cancellation returns undelivered messages for editing and records the delivery state.
- Bind keys after checking Windows Terminal behavior; commands remain available even when a terminal intercepts a shortcut.

Exit gate: deterministic tests cover input during streaming, tool execution, pending approvals/questions, completion, cancellation, and restart. A steering message never bypasses an unresolved approval or drops a tool result.

### 3. Add session trees, fork, and safe rewind

Primary areas: `src/session/store.rs`, `src/session/mod.rs`, `src/llm/session/context.rs`, `persistence.rs`, `compaction.rs`, and the session command/UI paths.

- Introduce versioned entries with stable `entry_id`, `parent_id`, session/branch identity, entry type, and an active-leaf pointer. Retain abandoned branches rather than truncating their history.
- Convert legacy linear history to a single chain through a recoverable migration with backups. Use a single-writer policy, durable writes, and crash recovery for the active pointer.
- Proposed commands: `/tree` navigates history; `/fork` creates a separate session from a chosen point; `/rewind` changes the active conversation point without deleting descendants.
- Branch only at valid conversation boundaries. Assistant tool calls and results must remain paired; incomplete calls are explicitly resolved or marked interrupted before branching.
- Compaction writes branch-scoped summaries without erasing original entries. Rewinding to an earlier branch must not inject a later branch's summary.
- **Conversation rewind is not filesystem rollback.** Add file restoration as a separately confirmed checkpoint operation: capture before/after content for managed file edits, validate paths and current content, preview changes, and refuse conflicts with later user edits.
- Shell commands, network requests, commits, and external effects are not automatically reversible. Label this limitation in the UI; never implement rewind with `git reset --hard`.

Exit gate: restart preserves all branches and the active leaf; sibling history never leaks into inference context; legacy imports recover correctly; file restore refuses an edited/conflicting file and preserves unrelated work.

### 4. Make agents and subagents user-creatable

Primary areas: `src/subagents/mod.rs`, `store.rs`, `src/tools/handlers/agent/mod.rs`, `src/cli/runtime.rs`, and bundled definitions under `src/agents/default/`.

- Extend the existing registry with validated user and project definitions. Proposed locations: `~/.zarncode/agents/` and `.zarncode/agents/`, using a versioned YAML schema.
- Define agent name, description, prompt, primary/subagent/both mode, model alias, allowed tools, execution mode, and resource limits. Do not invent an independent runtime for custom agents.
- Add proposed `zarncode agent create`, `agent list`, and `agent validate` commands plus primary-agent selection and explicit subagent invocation in the TUI.
- Resolve trusted definitions deterministically: bundled defaults, user definitions, then explicitly trusted project definitions. Surface collisions and reject invalid or cyclic references.
- Effective child permissions are the intersection of parent permissions, child policy, and workspace policy. Project files, prompts, and model output cannot grant additional authority.
- Track parent/session IDs, ancestry, depth, concurrency, token/time budgets, cancellation, and artifact ownership. Use isolated workspaces or an explicit single-writer policy for parallel file-editing agents.
- Keep child transcripts separate; return structured summaries, output references, and status to the parent. Resume from existing subagent storage where supported.

Exit gate: a user-defined agent runs in foreground and background through existing paths; escalation, recursive spawning, budget exhaustion, concurrent edits, and parent cancellation have deterministic tests.

### 5. Extract Zarnlib at the second-consumer boundary

- First remove runtime/TUI dependencies from inference contracts. Provider adapters consume tool schemas as data, not `Runtime` or `ExecutableToolRegistry` handles.
- Create a Cargo workspace once the CLI and gateway need the same implementation. Proposed future layout: `crates/zarnlib`, `crates/zarncode`, and later `crates/zarngate`.
- Public surface: messages and tool definitions, request configuration, model capabilities, normalized stream events, provider registry, typed errors, usage, cancellation, and credential-resolution traits.
- Keep actual OAuth flows and credential stores outside the provider-neutral contracts. Never expose secrets through serialized requests, debug formatting, or ordinary logs.
- Keep session trees, agent orchestration, approval UX, shell/filesystem tools, and terminal dependencies outside Zarnlib. A future embedded-agent library can be designed separately if another consumer needs it.

Exit gate: a small headless Rust client and Zarncode run the same provider contract suite through Zarnlib without importing CLI/TUI internals.

### 6. Build Zarngate for OAuth and provider routing

Start only after the Zarnkina adapter, shared contract tests, and local agent behavior are reliable.

**Separate the two authentication boundaries:**

1. Zarncode authenticates to Zarngate as a user/client.
2. Zarngate authenticates to an upstream inference provider using that user's authorized connection or a separately governed service credential.

A gateway access token is never forwarded as an upstream provider token. User login is not permission to proxy arbitrary subscription credentials; only integrate flows supported by each upstream provider's terms and API.

- Prefer an existing OAuth/OIDC identity provider for gateway login. Use authorization code with PKCE for a desktop CLI and an approved loopback redirect; offer device authorization only when the issuer supports it. Do not invent an authorization server in the first release.
- Keep the CLI a public client with no embedded client secret. Validate state, issuer, audience, expiry, and scopes; use short-lived gateway tokens, revoke sessions on logout, and protect refresh credentials in the OS credential store.
- Encrypt upstream credentials at rest with managed keys, isolate them by tenant/user/provider, minimize scopes, and serialize refreshes to avoid concurrent refresh-token races. Redact secrets from logs and traces.
- Proposed initial API: health/readiness, authenticated model discovery, provider connection status, and one versioned streaming inference endpoint. Publish its schema before adding compatibility facades; an OAuth discovery endpoint belongs to the chosen issuer unless Zarngate deliberately becomes an issuer later.
- Stream normalized inference events with backpressure and disconnect cancellation. Keep session/tool orchestration local; the gateway does not execute tool calls.
- Add per-user/model quotas, token/concurrency limits, deadlines, request IDs, usage accounting, and audit metadata without storing prompt bodies by default.
- Route only to configured/allowlisted upstreams. Prevent arbitrary upstream URLs and SSRF, enforce TLS and destination policy, and separate administrative routes from user inference routes.
- Retry only when safe before output begins; never silently replay a partially consumed stream, execute a tool again, or send private prompts to a fallback provider without an explicit routing policy.

Exit gate: two users cannot access each other's connections; refresh/revocation, quota rejection, SSRF protection, secret redaction, disconnect cancellation, and partial-stream failure handling pass integration tests.

## First implementation milestone

Deliver **Zarncode identity + direct Zarnkina inference + explicit steering/follow-up queues**, while retaining and smoke-testing existing foreground/background delegation. Implement session-tree/rewind and custom-agent creation next. Do not make OAuth, a gateway deployment, or a large workspace rewrite prerequisites for a working CLI.

Verification for implementation PRs: `cargo fmt --all -- --check`, `cargo clippy --all-features -- -D warnings`, and `cargo test --all-features`, plus the feature-specific exit gates above. Record pre-existing failures separately; use local mocks for default tests and opt-in credentials for live inference tests.

## References and compatibility policy

- Pi terminal steering/follow-up semantics: https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/usage.md
- Pi session-tree, fork, and compaction behavior: https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/sessions.md
- Pi project scope: https://github.com/earendil-works/pi/tree/main/packages/coding-agent — core Pi does not ship a built-in subagent system; Zarncode's agent design extends this repository's existing machinery.
- OpenCode agents: https://opencode.ai/docs/agents — product reference for primary agents, subagents, and configurable roles, not a promise of protocol compatibility.

External references were consulted during this planning pass and can evolve. Pin a reference revision and review license/attribution requirements before copying code. The Zarnkina API and OAuth issuer were not supplied, so their exact endpoints, supported grants, and compatibility remain integration inputs rather than invented facts.
