# LlamaProxy

LlamaProxy is a desktop app that connects coding agents and API clients to AI
providers through a local proxy. Sign in to a provider or add an API key, choose
a model, and configure your client from one window.

The app manages [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI), the
separate process that handles API requests. LlamaProxy does not run models
itself. Requests go to the providers you configure, using your accounts and their
quotas or billing.

```text
Coding agent or API client → CLIProxyAPI on your machine → Model provider
                                      ↑
                            Managed by LlamaProxy
```

LlamaProxy is a fork of
[EasyCLIProxyAPI](https://github.com/router-for-me/EasyCLIProxyAPI).

## Install

Download a package from [GitHub Releases](https://github.com/rselbach/LlamaProxy/releases/latest)
for your operating system and processor. Release names use `amd64` for x86-64
and `aarch64` for ARM64, including Apple silicon. Releases are built for macOS
on Apple silicon and Linux on both architectures. Windows and Intel macOS
release builds are disabled.

- On macOS with Apple silicon, open the `.dmg` and drag **LlamaProxy** into
  **Applications**.
- On Linux, extract the entire `.tar.gz` into a writable folder and run
  `./LlamaProxy` from that folder. The app requires WebKitGTK 4.1 and GTK 3 runtime
  libraries.

Keep the Linux package contents together. The executable uses the
adjacent `cpa-core` directory and `core-version.txt` to install the bundled core.
A separate CLIProxyAPI installation is not required.

## Connect your first client

Install the client itself before this setup. LlamaProxy detects and configures
clients such as Claude Code, Claude Desktop, Codex, OpenCode, OpenClaw, Hermes
Agent, DeepSeek Harness, ZCode, Kimi Code, Grok Build, and Pi.

1. Open LlamaProxy. On first launch, the app installs the bundled core and starts
   it by default. Check the core status on **Home** before continuing.
2. In **Advanced Settings**, replace the default `123456` key under
   **Authentication Keys** with a strong key. This is the key your clients use
   to reach the proxy, not a provider API key.
3. Connect a provider. Use **OAuth** for an
   account login, or **API Access** for a provider URL and API key. For API
   access, fetch the models, select at least one, and save the connection.
4. Open **Agent Configuration** and select an installed client and a model. Before changing an existing client
   configuration, use **Manual Backup** in **Agent Configuration** to save the
   files on disk. Configuration operations can replace custom settings and do
   not create automatic backups.
5. Apply the configuration and wait for success before launching the client.
   For Pi, use **Install Provider** instead of **Apply Config**.

Keep LlamaProxy running while clients use its managed core. Stopping the core or
quitting the app interrupts access through the proxy.

### Connect another API client

On **Home**, copy the URL for your client's API format and use a key from
**Authentication Keys**. With the default network settings, the base URLs are:

| Client API format | Base URL |
| --- | --- |
| OpenAI-compatible | `http://127.0.0.1:11432/v1` |
| Anthropic-compatible | `http://127.0.0.1:11432` |
| Gemini-compatible | `http://127.0.0.1:11432` |

To check the models exposed by the proxy, replace the placeholder with your
proxy key and run:

```sh
export LLAMAPROXY_API_KEY='your-proxy-api-key'
curl --fail-with-body 'http://127.0.0.1:11432/v1/models' \
  -H "Authorization: Bearer ${LLAMAPROXY_API_KEY}"
```

The response lists the available model IDs. Use one of those IDs in your
client. If you change the port, listen address, or TLS settings, use the URL
shown on **Home** rather than the default above.

## Manage connections

The standard console provides controls beyond the initial setup:

- **OAuth** manages account logins, credential files, and provider quota
  queries. Login options include Codex, Claude, Antigravity, Kimi, xAI, and GitHub
  Copilot.
- **API Access** manages Codex, Claude, Gemini, DeepSeek, and other
  OpenAI-compatible providers, including model selection and health checks.
- **Agent Configuration** manages client settings, manual backups, model
  catalogs, and client launches. It also includes Codex session management.
- **Usage** shows request history, token counts, timing, and cost estimates.
- **Advanced Settings** controls authentication keys, network access, routing,
  retries, and logging.

The interface supports English, Japanese, Simplified Chinese, and Traditional
Chinese, with light, dark, and system appearance settings.

### Connect GitHub Copilot

Use a GitHub account with Copilot access. Requests consume your plan's usage
allowance and remain subject to GitHub's model and organization policies.

1. Start the core, then open **OAuth**.
2. On the **GitHub Copilot** card, select **Start Sign-In**.
3. Select **Copy code**, then **Open Link**. Enter the code on GitHub and approve
   access. Keep the LlamaProxy page open while it checks authorization.
4. After your account name and model count appear, choose a `copilot/…` model
   in **Agent Configuration** or your API client.

Use **Refresh models** after changing your Copilot plan or model access.
LlamaProxy supports one Copilot account at a time. **Replace account** keeps
using the previous account until the new sign-in succeeds. **Disconnect**
removes the local credentials and model routes; it does not revoke the GitHub
OAuth authorization.

Copilot support is built into LlamaProxy. It requires no plugin or separate
service. See [Copilot routing and limits](docs/copilot.md) for implementation
and verification details.

## Protect your credentials and data

The proxy listens on `127.0.0.1:11432` by default. Leave **Allow LAN** off unless
other machines need access. Before enabling it, replace the default key and
restrict access with your firewall. An empty authentication-key list allows
requests without a configured key.

LlamaProxy stores provider credentials and proxy settings on disk. The app also
keeps a separate, generated management secret for its connection to CLIProxyAPI.
Do not use that secret as a client API key.

The runtime data directory depends on how you run the app:

- For the macOS app bundle, it is
  `~/Library/Application Support/com.llamaproxy.app/`.
- For portable executables, it is the directory containing the executable.
  This also applies to unbundled development builds.

That directory contains `config.toml` for app settings, `cpa-core/config.yaml`
for proxy configuration, and `oauth/` for credentials by default. Usage history
is in `usage-records/usage.db`, and manual client backups are under
`backups/agents/`. Client configuration changes are written to each client's
own configuration files, outside the LlamaProxy data directory.

Copilot credentials are stored separately in `copilot-account.json` in the
runtime data directory, with owner-only file permissions on macOS and Linux.
Treat this file as a secret when making backups. Short-lived Copilot tokens
stay in memory and are not written into the core configuration.

Quit the app before copying its runtime data directory for a backup. Treat the
copy as sensitive: configuration files and client backups can contain keys or
tokens. Redact credentials before attaching logs or configuration to an issue.

## Update or troubleshoot

Use **Version Management** to check the desktop app, proxy core, and Codex model
catalog independently. Updating the app restarts it and briefly interrupts the
managed core. Schedule updates between requests.

If an in-app update fails, download a complete package from
[GitHub Releases](https://github.com/rselbach/LlamaProxy/releases/latest).
Back up the runtime data directory before a manual replacement.

- If the core is missing and GitHub is unavailable, use **Version Management** →
  **Offline Install** to install the bundled core. Offline installation does
  not make provider requests work without a network connection.
- If **OAuth**, **API Access**, or **Agent Configuration** is disabled, start
  the core from **Home**.
- If a client cannot connect, check the core status, copied URL, proxy key, and
  selected model. Provider credentials belong in LlamaProxy; the client needs
  the proxy key.
- If a client is not detected, install it and refresh **Agent Configuration**.

Report reproducible problems in
[GitHub Issues](https://github.com/rselbach/LlamaProxy/issues), including your
operating system, architecture, app version, and core version.

## Develop

The frontend uses React, TypeScript, and Vite. Tauri 2 provides the desktop
shell, with Rust code for core management, filesystem access, and client
configuration.

Install [Bun](https://bun.sh/) 1.3.14, Rust, and the
[Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/).
Then, from the repository root, install dependencies and start the desktop app:

```sh
bun install --frozen-lockfile
bun tauri dev
```

A source checkout does not include the core archive. Use **Version Management**
→ **Install Latest** if no core is available. `bun run dev` starts only the Vite
frontend; core management and other desktop operations require Tauri.

Run the TypeScript checks, frontend tests, production frontend build, and Rust
tests from the repository root:

```sh
bun run check
bun test
bun run build
cargo test --manifest-path src-tauri/Cargo.toml
```

Rust configuration tests reject paths with symlink components. On macOS, resolve
the temporary-directory path before running them:

```sh
TMPDIR="$(cd "${TMPDIR:-/tmp}" && pwd -P)" \
  cargo test --manifest-path src-tauri/Cargo.toml
```

The standalone browser checks in `tests/*-ui.cjs` are separate from `bun test`
and require Playwright and a running Vite server.

Before using `build.sh` again, back up any runtime configuration you need from
`bin-work/cpa-core/`. The script replaces that directory.

To build a portable executable with the pinned core archive, run `./build.sh`
on macOS or Linux, or `.\build.ps1` in PowerShell on Windows. The scripts
download the core and place the result in `bin-work/`. Use `./run.sh` or
`.\run.ps1` to launch it. These are unbundled builds, not the signed macOS app
produced by the release workflow.

The app version is defined in `src-tauri/Cargo.toml`. `core-version.txt` pins the
bundled CLIProxyAPI version. Release packaging is defined in
[`.github/workflows/release.yml`](.github/workflows/release.yml).

## License

[MIT](LICENSE). LlamaProxy builds on EasyCLIProxyAPI by Router-For.ME. The
[upstream license notice](LICENSE-EasyCLIProxyAPI) is retained in this repository.
