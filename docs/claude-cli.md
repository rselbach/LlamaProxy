# Claude CLI routing and limits

LlamaProxy answers `claude-cli/…` requests by running the Claude Code
command-line tool that is installed and signed in on the same computer. Claude
Code makes the request to Anthropic with its own sign-in. LlamaProxy never
reads, stores, or forwards that credential. CLIProxyAPI still owns translation
between client API formats, request routing, and usage recording.

## Request path

```text
Client → managed CLIProxyAPI → private LlamaProxy adapter → claude -p → Anthropic
```

A client selects an advertised model such as `claude-cli/claude-opus-5-5`. The
managed core resolves the alias, translates the request to the Anthropic
Messages format, and calls the adapter. The adapter writes the conversation to
a Claude Code process over stream-json and relays its stream events back.

The adapter binds an OS-assigned port on `127.0.0.1` and requires a random key
that changes on each app launch. When starting or adopting a core, LlamaProxy
rewrites its one managed `claude-api-key` entry with the current port and key.
Custom aliases created in **Model Aliases** are retained for enabled models.
Native Claude credentials, Copilot routes, and user entries are preserved. The
managed entry is hidden from **API Access** editing.

The entry sets `request-retry: 0`. A plan limit or a missing sign-in fails the
same way on a retry, and each attempt starts a Claude Code process.

## Conversations and processes

Clients send the whole conversation with every request. Claude Code keeps its
own record of a conversation, so the adapter keeps one process per conversation
and sends it only what is new.

After a reply, the process is parked under a hash of the conversation's words,
tool calls, and tool results. The hash ignores thinking blocks, call IDs, cache
marks, and how text is split into messages, because clients and the core
rewrite those. The next request takes the process whose hash matches its
history up to the last assistant turn.

A first turn, an edited or branched conversation, or a process parked longer
than ten minutes starts a new process. The new process is told the whole
conversation one message at a time, and only the last user message asks for a
reply. At most eight processes are alive. A new one replaces the oldest parked
process and waits only when all eight are answering.

Every process runs in the same empty `claude-cli` directory under the runtime
data directory. Claude Code writes its working directory into the system
prompt, so a shared directory keeps the prompt cache shared.

A client that disconnects mid-reply ends its process with `SIGTERM`, which
stops the generation.

## Tools

Claude Code's built-in tools are turned off. The client's tools are listed to
Claude Code by a loopback MCP server that the adapter starts for each process,
on a path with a random token.

When Claude calls a tool, the adapter returns the call to the client as a
normal tool use and ends the HTTP response. Claude Code's MCP request stays
open. The client's next request brings the result, which the adapter matches
by call ID, then by position, and returns as the MCP reply. The same Claude
Code turn then continues.

## System prompt

The client's system prompt is appended to Claude Code's own prompt with
`--append-system-prompt-file`. It never replaces it. A first Haiku turn with
built-in tools off measured about 2,900 input tokens for a short request
(Claude Code 2.1.321, November 2026). Later turns in the same conversation
read that prefix from the prompt cache.

Claude Code runs with user, project, and local settings sources disabled, and
with CLAUDE.md files, memory, compaction, telemetry, auto-updates, and its own
retries turned off. Child processes do not inherit `ANTHROPIC_API_KEY`,
`ANTHROPIC_BASE_URL`, or the other variables that would switch Claude Code to
another account or point it back at LlamaProxy.

## Models and reasoning effort

The adapter offers a fixed list: the current full model IDs and the short
aliases `opus`, `sonnet`, `haiku`, and `fable`. Claude Code resolves a short
alias to its latest model, so the aliases stay current between LlamaProxy
releases.

The core forwards reasoning effort only for model names it recognizes. With
core 7.2.157, a full ID such as `claude-cli/claude-opus-5-5` receives the
client's effort, and a short alias such as `claude-cli/opus` receives none.
The adapter passes a received effort to Claude Code as `--effort`, and
`max_tokens` as `CLAUDE_CODE_MAX_OUTPUT_TOKENS`.

The **Models** page lists these models under **Claude CLI** with a switch for
each. A model that is switched off is removed from the core entry and refused
by the adapter. The choice is saved with the connection and is cleared by
**Disconnect**.

## Sign-in and storage

**Connect** runs `claude auth status` and accepts only a Claude subscription
sign-in. It rejects a signed-out Claude Code and an API-key sign-in.
LlamaProxy saves the account name in `claude-cli-account.json` so the card and
the routes survive a restart. **Disconnect** removes the routes and ends the
parked processes. It does not sign Claude Code out.

If Claude Code reports that a reply was charged to usage credits instead of
plan limits, the adapter ends that reply with a `billing_error`.

## Current limits

- One account is used: the one Claude Code is signed in to.
- Sampling options such as `temperature` and `top_p` are ignored.
- A `tool_choice` that forces a tool is rejected, including the OpenAI value
  `required`. `tool_choice: none` hides the tools. Hosted tools such as web
  search are rejected.
- An assistant prefill as the last message is rejected.
- With core 7.2.157, a Responses request whose `input` is a plain string
  reaches the adapter with no messages and is rejected. Send `input` as a list
  of items.
- The Messages token-count endpoint is not implemented.
- Each parked process is a full Claude Code process and holds memory until it
  is reused or times out.
- The adapter was verified on macOS. The process handling uses Unix signals,
  and Windows was not tested.
- Anthropic can change how `claude -p` usage is counted or allowed. This
  provider can then change or stop working.

## Source map and design choice

- `src-tauri/src/claude_cli/mod.rs` owns the connection state, persistence,
  and Tauri commands.
- `engine.rs` owns the Claude Code process, the conversation hash, the pool,
  and the MCP tool server.
- `transport.rs` owns the authenticated adapter endpoint for the core.
- `http.rs` owns the minimal HTTP/1 reader and writers both servers share.
- `config.rs` owns the managed core entry.
- `src/components/ClaudeCliConnection.tsx` provides the card on the OAuth page.

Three ways to drive Claude Code were compared. The Agent SDK runs the same
binary but needs a Node runtime beside a Rust app. Driving the interactive
terminal interface depends on Claude Code internals that change between
versions. Print mode with stream-json needs neither. Its flags are documented.
The opening handshake and the `shouldQuery` history frames follow what the
Agent SDK sends and are not documented, so they are the part most likely to
break on a Claude Code update. The live tests cover them.

The name is "Claude CLI" because Anthropic does not allow "Claude Code" as part
of another product's feature name.

## Verification

The backend tests replace `claude` with a shell script that plays recorded
stream-json. They use real loopback connections for the adapter and the MCP
server. They cover streaming and whole replies, a tool result continuing in the
same process, edited history, usage credits, Claude Code errors, the adapter
key, disconnect cleanup, and the managed core entry.

```sh
cargo test --manifest-path src-tauri/Cargo.toml claude_cli
bun test tests/claudeCli.test.ts
```

The ignored core test needs a CLIProxyAPI executable matching
`core-version.txt`. It still uses the fake `claude`. It checks that a two-turn
tool conversation stays in one process for Chat Completions, Responses, and
Messages clients. Run it after every core update, because a change in how the
core rewrites a conversation would make every turn start a new process.

```sh
LLAMAPROXY_TEST_CORE=/absolute/path/to/cli-proxy-api \
  cargo test --manifest-path src-tauri/Cargo.toml the_core_keeps \
  -- --ignored --nocapture
```

Two ignored live tests run the real, signed-in Claude Code and spend a small
amount of plan usage on Haiku. The second one also needs
`LLAMAPROXY_TEST_CORE` and exercises the whole path with nothing faked.

```sh
LLAMAPROXY_TEST_LIVE_CLAUDE=1 \
  cargo test --manifest-path src-tauri/Cargo.toml live_claude_code \
  -- --ignored --nocapture
```

The browser checks mock Tauri IPC and never run Claude Code. The first covers
the card. The second covers the **Models** page listing and switching these
models beside Copilot's. With Vite running on port 1420, their commands are:

```sh
node tests/claude-cli-ui.cjs
node tests/models-ui.cjs
```
