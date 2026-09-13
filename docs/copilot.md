# Copilot routing and limits

LlamaProxy owns GitHub device authorization, credential storage, Copilot token
renewal, model discovery, and forwarding. CLIProxyAPI still owns translation
between client API formats, request routing, and usage recording. No Copilot
plugin or additional package dependency is required.

## Request path

```text
Client → managed CLIProxyAPI → private LlamaProxy adapter → Copilot API
```

A client selects an advertised model such as `copilot/gpt-5.4`. The managed
core resolves the alias, translates the request, and calls the adapter. The
adapter replaces its local authentication key with a short-lived Copilot token
and forwards the response, including SSE chunks, to the core.

The adapter binds an OS-assigned port on `127.0.0.1` and requires a random key
that changes on each app launch. When starting or adopting a core, LlamaProxy
rewrites its managed routes with the current port and key. Unrelated provider
entries and YAML comments are preserved.

## Model routing

Discovery uses the authenticated Copilot `/models` endpoint. Only chat models
with a recognized `supported_endpoints` entry are registered. The preference
order is Responses, Chat Completions, then Messages.

| Copilot endpoint | Core configuration section |
| --- | --- |
| `/responses` | `codex-api-key` |
| `/chat/completions` | `openai-compatibility` |
| `/v1/messages` | `claude-api-key` |

Explicit `copilot/…` aliases prevent native Claude or Codex model names from
colliding. The managed entries are hidden from **API Access** editing. Account
changes and model refreshes belong to the **GitHub Copilot** card.

The core's Codex executor inserts a default hosted image-generation tool into
Responses requests. The adapter removes that declaration because it is not a
Copilot subscription tool. Function-tool declarations pass through unchanged.
Explicit image tool choices and non-default hosted-image declarations are
rejected.

## Authentication and storage

Device authorization uses GitHub's public OAuth client identifier
`Iv1.b507a08c87ecfe98`. LlamaProxy respects the polling interval, increases it
on `slow_down`, expires pending sessions, and cancels in-flight authorization
when the user cancels or replaces a sign-in.

The GitHub access token and any refresh token are stored in
`copilot-account.json`, outside the core's OAuth directory. Writes replace the
file atomically and use mode `0600` on Unix. The managed core directory is also
restricted to its owner on Unix because its configuration contains the local
adapter key. The frontend receives only the account name, model IDs,
verification URL, and user code.

Copilot tokens are exchanged through
`https://api.github.com/copilot_internal/v2/token`. The adapter refreshes them
before expiry and retries an inference request once after HTTP 401. It accepts
only HTTPS API endpoints under `githubcopilot.com` from that exchange and does
not follow redirects. Authorization errors omit remote bodies that might echo
credentials. Inference errors preserve the upstream HTTP status and numeric
`Retry-After` header without exposing the remote error body.

## Current limits

- One GitHub account is connected at a time. There is no account pool or
  Copilot quota dashboard.
- The saved model catalog is refreshed on sign-in and with **Refresh models**.
- Streaming and non-streaming inference use the core's existing translators.
  Copilot availability, tool support, and reasoning support depend on the
  selected model and GitHub policy.
- The adapter accepts JSON requests up to 32 MiB. It does not implement file
  uploads, audio, hosted image generation, Responses compaction, WebSocket
  forwarding, or the Messages token-count endpoint.
- Copilot network requests use the process environment's proxy settings, not
  the app's configured proxy URL. The core-to-adapter connection is direct.
- GitHub authorization and subscription access require a live account check.
  Automated tests do not authenticate against GitHub or spend subscription
  usage.

## Source map and design choice

- `src-tauri/src/copilot/mod.rs` owns account state, persistence, cancellation,
  and Tauri commands.
- `api.rs` owns the GitHub and Copilot wire formats and validation.
- `config.rs` owns the managed core entries and alias policy.
- `transport.rs` owns the constrained, authenticated HTTP adapter and streaming.
- `src/components/CopilotConnection.tsx` provides the GitHub Copilot card on the OAuth page.

A core fork would couple the feature to every core update. A shared-library
plugin would require platform-specific builds and a separate plugin lifecycle.
The private adapter instead reuses the core's existing provider formats and
keeps Copilot credentials and behavior in LlamaProxy. Tests against the pinned
core check that those provider formats still work as expected.

The [reference plugin](https://github.com/arthur-sommer-etc/cliproxyapi-copilot-plugin)
was consulted for Copilot protocol behavior. It is not bundled or imported.
GitHub documents the authorization protocol in
[OAuth device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow).

## Verification

Focused backend tests use real loopback HTTP connections with fake GitHub and
Copilot responses. They cover polling, cancellation, token renewal, private
storage, configuration rollback, route ownership, all three upstream formats,
error redaction, and truncated streams.

```sh
cargo test --manifest-path src-tauri/Cargo.toml copilot
bun test tests/copilot.test.ts
```

The ignored core integration test requires a separately downloaded,
checksum-verified CLIProxyAPI executable matching `core-version.txt`. It starts
that executable with a temporary configuration and fake Copilot upstream. It
checks the public model catalog, streaming and non-streaming cross-format
requests, and rebinding a running core to a new adapter port and key. It does
not read or change an installed core or user credentials.

```sh
LLAMAPROXY_TEST_CORE=/absolute/path/to/cli-proxy-api \
  cargo test --manifest-path src-tauri/Cargo.toml copilot_core_routes \
  -- --ignored --nocapture
```

The browser check uses the existing fixture convention, Playwright, and Chrome.
With Vite running on port 1420, its command is:

```sh
node tests/copilot-ui.cjs
```

`PLAYWRIGHT_MODULE` can identify an existing Playwright installation outside
this repository. The fixture mocks Tauri IPC and never opens GitHub.
