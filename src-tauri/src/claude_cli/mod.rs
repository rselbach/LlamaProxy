//! Serves a Claude subscription to the managed core by running the installed
//! Claude Code CLI. Claude Code keeps its own sign-in; LlamaProxy stores only
//! the account's name and never reads a credential.
mod config;
mod engine;
mod http;
#[cfg(all(test, unix))]
mod tests;
mod transport;

use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError},
};

static RUNTIME: LazyLock<Mutex<Option<Arc<ClaudeCli>>>> = LazyLock::new(|| Mutex::new(None));
const BRIDGE_PATH: &str = "/llamaproxy-claude-cli";
const MODEL_PREFIX: &str = "claude-cli/";
// The IDs `claude --model` accepts. The short aliases resolve to the latest
// model in Claude Code itself, so they stay current when the full IDs fall
// behind. The core forwards reasoning effort only for the full IDs it knows.
const MODELS: [&str; 9] = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-2",
    "claude-fable-5-1",
    "opus",
    "sonnet",
    "haiku",
    "fable",
];

// The connection as saved: who Claude Code is signed in as, and which of
// MODELS the user turned off. It holds no credential.
#[derive(Clone, Deserialize, Serialize)]
struct Account {
    label: String,
    #[serde(default)]
    disabled_models: Vec<String>,
}

impl Account {
    fn enabled_models(&self) -> Vec<&'static str> {
        MODELS
            .into_iter()
            .filter(|model| {
                !self
                    .disabled_models
                    .iter()
                    .any(|disabled| disabled == model)
            })
            .collect()
    }

    fn set_model_enabled(&mut self, model: &str, enabled: bool) -> Result<(), String> {
        let id = model
            .strip_prefix(MODEL_PREFIX)
            .filter(|id| MODELS.contains(id))
            .ok_or_else(|| "Invalid Claude CLI model identifier".to_string())?;
        self.disabled_models.retain(|disabled| disabled != id);
        if !enabled {
            self.disabled_models.push(id.to_string());
        }
        Ok(())
    }
}

struct ClaudeCli {
    engine: engine::Engine,
    path: PathBuf,
    config_path: PathBuf,
    base_url: String,
    key: String,
    // routes exist only while this is set
    account: Mutex<Option<Account>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeCliStatus {
    installed: bool,
    account: Option<String>,
    models: Vec<String>,
    disabled_models: Vec<String>,
}

fn runtime() -> Result<Arc<ClaudeCli>, String> {
    let mut runtime = RUNTIME
        .lock()
        .map_err(|_| "Claude CLI runtime lock is poisoned".to_string())?;
    if let Some(service) = runtime.as_ref() {
        return Ok(service.clone());
    }
    let base = super::core_base_dir()?;
    let (service, listener) = ClaudeCli::build(
        base.join("claude-cli-account.json"),
        super::core_install_dir()?.join(super::CORE_CONFIG_FILE),
        base.join("claude-cli"),
        None,
    )?;
    let running = service.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = transport::serve(listener, running).await {
            eprintln!("Claude CLI adapter stopped: {error}");
        }
    });
    *runtime = Some(service.clone());
    Ok(service)
}

/// Rebind managed routes when starting or adopting a core after an app restart.
pub(crate) fn sync_core_config() -> Result<(), String> {
    let service = runtime()?;
    let models = service.enabled_models();
    service.configure(&models)
}

impl ClaudeCli {
    fn build(
        path: PathBuf,
        config_path: PathBuf,
        work_dir: PathBuf,
        command: Option<PathBuf>,
    ) -> Result<(Arc<Self>, TcpListener), String> {
        let account = load_account(&path)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("Could not start the private Claude CLI adapter: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Could not configure the Claude CLI adapter: {e}"))?;
        let address = listener
            .local_addr()
            .map_err(|e| format!("Could not locate the Claude CLI adapter: {e}"))?;
        let service = Arc::new(Self {
            engine: engine::Engine {
                pool: Arc::default(),
                command,
                work_dir,
            },
            path,
            config_path,
            base_url: format!("http://{address}{BRIDGE_PATH}"),
            key: format!("lp-claude-cli-{}", random_key()?),
            account: Mutex::new(account),
        });
        Ok((service, listener))
    }

    fn account(&self) -> MutexGuard<'_, Option<Account>> {
        self.account.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // the models the core may route here; none while disconnected
    fn enabled_models(&self) -> Vec<&'static str> {
        self.account()
            .as_ref()
            .map(Account::enabled_models)
            .unwrap_or_default()
    }

    fn configure(&self, models: &[&str]) -> Result<(), String> {
        config::write(&self.config_path, &self.base_url, &self.key, models)
    }

    fn status(&self) -> ClaudeCliStatus {
        let account = self.account().clone();
        let prefixed = |model: &str| format!("{MODEL_PREFIX}{model}");
        ClaudeCliStatus {
            installed: self.engine.command.is_some() || engine::find_command().is_some(),
            models: match account {
                Some(_) => MODELS.into_iter().map(prefixed).collect(),
                None => Vec::new(),
            },
            disabled_models: account
                .iter()
                .flat_map(|account| account.disabled_models.iter().map(|model| prefixed(model)))
                .collect(),
            account: account.map(|account| account.label),
        }
    }

    fn commit(&self, account: Option<Account>) -> Result<(), String> {
        let mut current = self.account();
        let models = account
            .as_ref()
            .map(Account::enabled_models)
            .unwrap_or_default();
        save_account(&self.path, account.as_ref())?;
        if let Err(error) = self.configure(&models) {
            return match save_account(&self.path, current.as_ref()) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!(
                    "{error}; could not restore the previous Claude CLI connection: {rollback}"
                )),
            };
        }
        *current = account;
        Ok(())
    }

    async fn connect(&self) -> Result<(), String> {
        let command = self
            .engine
            .command
            .clone()
            .or_else(engine::find_command)
            .ok_or(engine::INSTALL_HINT)?;
        let label = engine::subscription_account(&command).await?;
        self.commit(Some(Account {
            label,
            disabled_models: Vec::new(),
        }))
    }

    fn set_model_enabled(&self, model: &str, enabled: bool) -> Result<(), String> {
        let mut account = self
            .account()
            .clone()
            .ok_or_else(|| "Connect Claude CLI first".to_string())?;
        account.set_model_enabled(model, enabled)?;
        self.commit(Some(account))
    }

    fn disconnect(&self) -> Result<(), String> {
        self.commit(None)?;
        self.engine.pool.clear();
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn get_claude_cli_status() -> Result<ClaudeCliStatus, String> {
    Ok(runtime()?.status())
}

#[tauri::command]
pub(crate) async fn connect_claude_cli() -> Result<ClaudeCliStatus, String> {
    let service = runtime()?;
    service.connect().await?;
    Ok(service.status())
}

#[tauri::command]
pub(crate) async fn set_claude_cli_model_enabled(
    model: String,
    enabled: bool,
) -> Result<ClaudeCliStatus, String> {
    let service = runtime()?;
    service.set_model_enabled(&model, enabled)?;
    Ok(service.status())
}

#[tauri::command]
pub(crate) async fn disconnect_claude_cli() -> Result<ClaudeCliStatus, String> {
    let service = runtime()?;
    service.disconnect()?;
    Ok(service.status())
}

fn load_account(path: &Path) -> Result<Option<Account>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not read the saved Claude CLI connection: {error}"
            ))
        }
    };
    serde_json::from_slice(&bytes).map_err(|_| {
        "The saved Claude CLI connection is invalid; restore or remove claude-cli-account.json"
            .to_string()
    })
}

fn save_account(path: &Path, account: Option<&Account>) -> Result<(), String> {
    let bytes = serde_json::to_vec(&account)
        .map_err(|_| "Could not encode the Claude CLI connection".to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", random_key()?));
    fs::write(&temporary, bytes)
        .map_err(|e| format!("Could not save the Claude CLI connection: {e}"))?;
    let replaced = super::replace_file_atomically(&temporary, path)
        .map_err(|e| format!("Could not replace the Claude CLI connection file: {e}"));
    if replaced.is_err() {
        if let Err(cleanup) = fs::remove_file(&temporary) {
            if cleanup.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Could not clean up a Claude CLI connection file: {cleanup}");
            }
        }
    }
    replaced
}

fn random_key() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| "Could not generate a secure Claude CLI key".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
