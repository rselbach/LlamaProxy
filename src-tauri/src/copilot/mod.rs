//! LlamaProxy-owned Copilot authentication and a private adapter for the managed core.
//! The core owns API translation; GitHub credentials never enter its configuration.
mod api;
#[cfg(test)]
mod auth_tests;
mod config;
#[cfg(test)]
mod tests;
mod transport;

use api::{Api, DeviceCode, Token};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

static RUNTIME: LazyLock<Mutex<Option<Arc<Copilot>>>> = LazyLock::new(|| Mutex::new(None));
const BRIDGE_PATH: &str = "/llamaproxy-copilot";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Endpoint {
    Chat,
    Responses,
    Messages,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Chat => "/chat/completions",
            Self::Responses => "/responses",
            Self::Messages => "/v1/messages",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Model {
    id: String,
    endpoint: Endpoint,
}

#[derive(Clone, Deserialize, Serialize)]
struct Account {
    login: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    models: Vec<Model>,
    #[serde(default)]
    disabled_models: Vec<String>,
}

impl Account {
    fn enabled_models(&self) -> Vec<Model> {
        self.models
            .iter()
            .filter(|model| !self.disabled_models.contains(&model.id))
            .cloned()
            .collect()
    }

    fn set_model_enabled(&mut self, model: &str, enabled: bool) -> Result<(), String> {
        let id = model
            .strip_prefix("copilot/")
            .ok_or_else(|| "Invalid Copilot model identifier".to_string())?;
        if !self.models.iter().any(|candidate| candidate.id == id) {
            return Err("The Copilot model is no longer available; refresh the catalog".into());
        }
        self.disabled_models.retain(|disabled| disabled != id);
        if !enabled {
            self.disabled_models.push(id.to_string());
        }
        Ok(())
    }
}

struct Login {
    cancel: CancellationToken,
    device: DeviceCode,
    expires_at: Instant,
    next_poll: Instant,
}

struct State {
    account: Option<Account>,
    token: Option<Token>,
    login: Option<Login>,
}

struct Copilot {
    api: Api,
    path: PathBuf,
    config_path: PathBuf,
    base_url: String,
    key: String,
    state: AsyncMutex<State>,
    login_cancel: Mutex<CancellationToken>,
    // Core startup is synchronous. Publish only routing data, never credentials.
    models: RwLock<Vec<Model>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopilotStatus {
    login: Option<String>,
    models: Vec<String>,
    disabled_models: Vec<String>,
    pending: Option<PendingLogin>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingLogin {
    user_code: String,
    url: String,
    expires_in: u64,
}

fn runtime() -> Result<Arc<Copilot>, String> {
    let mut runtime = RUNTIME
        .lock()
        .map_err(|_| "Copilot runtime lock is poisoned".to_string())?;
    if let Some(service) = runtime.as_ref() {
        return Ok(service.clone());
    }
    let path = super::core_base_dir()?.join("copilot-account.json");
    let account = load_account(&path)?;
    let models = account
        .as_ref()
        .map(Account::enabled_models)
        .unwrap_or_default();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| format!("Could not start the private Copilot adapter: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Could not configure the Copilot adapter: {e}"))?;
    let address = listener
        .local_addr()
        .map_err(|e| format!("Could not locate the Copilot adapter: {e}"))?;
    let service = Arc::new(Copilot {
        api: Api::new()?,
        path,
        config_path: super::core_install_dir()?.join(super::CORE_CONFIG_FILE),
        base_url: format!("http://{address}{BRIDGE_PATH}"),
        key: format!("lp-copilot-{}", random_key()?),
        state: AsyncMutex::new(State {
            account,
            token: None,
            login: None,
        }),
        models: RwLock::new(models),
        login_cancel: Mutex::new(CancellationToken::new()),
    });
    let running = service.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = transport::serve(listener, running).await {
            eprintln!("Copilot adapter stopped: {error}");
        }
    });
    *runtime = Some(service.clone());
    Ok(service)
}

/// Rebind managed routes when starting or adopting a core after an app restart.
pub(crate) fn sync_core_config() -> Result<(), String> {
    let service = runtime()?;
    let models = service
        .models
        .read()
        .map_err(|_| "Copilot catalog lock is poisoned".to_string())?;
    service.configure(&models)
}

impl Copilot {
    fn configure(&self, models: &[Model]) -> Result<(), String> {
        config::write(&self.config_path, &self.base_url, &self.key, models)
    }

    fn status(&self, state: &State) -> CopilotStatus {
        CopilotStatus {
            login: state.account.as_ref().map(|a| a.login.clone()),
            models: state
                .account
                .as_ref()
                .map(|a| {
                    a.models
                        .iter()
                        .map(|m| format!("copilot/{}", m.id))
                        .collect()
                })
                .unwrap_or_default(),
            disabled_models: state
                .account
                .as_ref()
                .map(|account| {
                    account
                        .disabled_models
                        .iter()
                        .map(|id| format!("copilot/{id}"))
                        .collect()
                })
                .unwrap_or_default(),
            pending: state.login.as_ref().map(|login| PendingLogin {
                user_code: login.device.user_code.clone(),
                url: login.device.verification_uri.clone(),
                expires_in: login
                    .expires_at
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
            }),
        }
    }

    fn commit(&self, state: &mut State, account: Option<Account>) -> Result<(), String> {
        let models = account
            .as_ref()
            .map(Account::enabled_models)
            .unwrap_or_default();
        let mut published = self
            .models
            .write()
            .map_err(|_| "Copilot catalog lock is poisoned".to_string())?;
        save_account(&self.path, account.as_ref())?;
        if let Err(error) = self.configure(&models) {
            return match save_account(&self.path, state.account.as_ref()) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!(
                    "{error}; could not restore the previous Copilot account: {rollback}"
                )),
            };
        }
        *published = models;
        state.account = account;
        state.token = None;
        Ok(())
    }

    async fn token(&self, state: &mut State) -> Result<Token, String> {
        if let Some(token) = &state.token {
            if Instant::now() < token.refresh_at {
                return Ok(token.clone());
            }
        }
        let mut account = state
            .account
            .clone()
            .ok_or_else(|| "Sign in to GitHub Copilot first".to_string())?;
        let now = unix_now()?;
        if account
            .expires_at
            .is_some_and(|expiry| expiry <= now.saturating_add(60))
        {
            let refresh = account.refresh_token.as_deref().ok_or_else(|| {
                "GitHub authorization expired; sign in to Copilot again".to_string()
            })?;
            let refreshed = self.api.refresh_github_token(refresh).await?;
            if !refreshed.error.is_empty() || refreshed.access_token.is_empty() {
                return Err(
                    "GitHub authorization could not be refreshed; sign in to Copilot again".into(),
                );
            }
            account.access_token = refreshed.access_token;
            if refreshed.refresh_token.is_some() {
                account.refresh_token = refreshed.refresh_token;
            }
            account.expires_at = refreshed
                .expires_in
                .map(|seconds| unix_now().map(|now| now.saturating_add(seconds)))
                .transpose()?;
            save_account(&self.path, Some(&account))?;
            state.account = Some(account.clone());
        }
        let token = self.api.exchange(&account.access_token).await?;
        state.token = Some(token.clone());
        Ok(token)
    }

    async fn poll(&self, state: &mut State) -> Result<(), String> {
        let Some(cancel) = state.login.as_ref().map(|login| login.cancel.clone()) else {
            return Ok(());
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => { state.login = None; Ok(()) }
            result = self.poll_inner(state) => result,
        }
    }

    fn cancel_login(&self) -> Result<(), String> {
        self.login_cancel
            .lock()
            .map_err(|_| "Copilot sign-in lock is poisoned".to_string())?
            .cancel();
        Ok(())
    }

    async fn poll_inner(&self, state: &mut State) -> Result<(), String> {
        let Some(login) = state.login.as_mut() else {
            return Ok(());
        };
        let now = Instant::now();
        if now >= login.expires_at {
            state.login = None;
            return Err("GitHub device code expired; start sign-in again".into());
        }
        if now < login.next_poll {
            return Ok(());
        }
        // Set before awaiting, so failed network attempts are rate-limited too.
        login.next_poll = now + Duration::from_secs(login.device.interval);
        let token = self
            .api
            .poll_device_login(&login.device.device_code)
            .await?;
        match token.error.as_str() {
            "authorization_pending" => return Ok(()),
            "slow_down" => {
                login.device.interval = login.device.interval.saturating_add(5);
                login.next_poll = Instant::now() + Duration::from_secs(login.device.interval);
                return Ok(());
            }
            "" if !token.access_token.is_empty() => {}
            "access_denied" => {
                state.login = None;
                return Err("GitHub authorization was denied".into());
            }
            _ => {
                state.login = None;
                return Err("GitHub authorization failed or expired; start sign-in again".into());
            }
        }
        state.login = None;
        let login = self.api.login_name(&token.access_token).await?;
        let copilot_token = self.api.exchange(&token.access_token).await?;
        let models = self.api.models(&copilot_token).await?;
        let expires_at = token
            .expires_in
            .map(|seconds| unix_now().map(|now| now.saturating_add(seconds)))
            .transpose()?;
        self.commit(
            state,
            Some(Account {
                login,
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_at,
                models,
                disabled_models: Vec::new(),
            }),
        )?;
        state.token = Some(copilot_token);
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn get_copilot_status() -> Result<CopilotStatus, String> {
    let service = runtime()?;
    let state = service.state.lock().await;
    Ok(service.status(&state))
}

#[tauri::command]
pub(crate) async fn start_copilot_login() -> Result<CopilotStatus, String> {
    let service = runtime()?;
    let cancel = {
        let mut active = service
            .login_cancel
            .lock()
            .map_err(|_| "Copilot sign-in lock is poisoned".to_string())?;
        active.cancel();
        *active = CancellationToken::new();
        active.clone()
    };
    let mut state = service.state.lock().await;
    let device = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err("Copilot sign-in was canceled".into()),
        device = service.api.start_device_login() => device?,
    };
    let now = Instant::now();
    state.login = Some(Login {
        cancel,
        expires_at: now + Duration::from_secs(device.expires_in),
        next_poll: now + Duration::from_secs(device.interval),
        device,
    });
    Ok(service.status(&state))
}

#[tauri::command]
pub(crate) async fn poll_copilot_login() -> Result<CopilotStatus, String> {
    let service = runtime()?;
    let mut state = service.state.lock().await;
    service.poll(&mut state).await?;
    Ok(service.status(&state))
}

#[tauri::command]
pub(crate) async fn cancel_copilot_login() -> Result<CopilotStatus, String> {
    let service = runtime()?;
    service.cancel_login()?;
    let mut state = service.state.lock().await;
    state.login = None;
    Ok(service.status(&state))
}

#[tauri::command]
pub(crate) async fn refresh_copilot_models() -> Result<CopilotStatus, String> {
    let service = runtime()?;
    let mut state = service.state.lock().await;
    // An explicit refresh also recovers from tokens revoked before their expiry.
    state.token = None;
    let token = service.token(&mut state).await?;
    let models = service.api.models(&token).await?;
    let mut account = state
        .account
        .clone()
        .ok_or_else(|| "Sign in to GitHub Copilot first".to_string())?;
    account.models = models;
    service.commit(&mut state, Some(account))?;
    state.token = Some(token);
    Ok(service.status(&state))
}

#[tauri::command]
pub(crate) async fn set_copilot_model_enabled(
    model: String,
    enabled: bool,
) -> Result<CopilotStatus, String> {
    let service = runtime()?;
    let mut state = service.state.lock().await;
    let mut account = state
        .account
        .clone()
        .ok_or_else(|| "Sign in to GitHub Copilot first".to_string())?;
    account.set_model_enabled(&model, enabled)?;
    service.commit(&mut state, Some(account))?;
    Ok(service.status(&state))
}

#[tauri::command]
pub(crate) async fn disconnect_copilot() -> Result<CopilotStatus, String> {
    let service = runtime()?;
    service.cancel_login()?;
    let mut state = service.state.lock().await;
    service.commit(&mut state, None)?;
    state.login = None;
    Ok(service.status(&state))
}

fn load_account(path: &Path) -> Result<Option<Account>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read the saved Copilot account: {error}")),
    };
    let account: Option<Account> = serde_json::from_slice(&bytes).map_err(|_| {
        "The saved Copilot account is invalid; restore or remove copilot-account.json".to_string()
    })?;
    if account
        .as_ref()
        .is_some_and(|a| a.access_token.is_empty() || a.login.is_empty())
    {
        return Err("The saved Copilot account is incomplete".into());
    }
    Ok(account)
}

fn save_account(path: &Path, account: Option<&Account>) -> Result<(), String> {
    let bytes = serde_json::to_vec(&account)
        .map_err(|_| "Could not encode the Copilot account".to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", random_key()?));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|e| format!("Could not create a private Copilot account file: {e}"))?;
    let result = (|| {
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("Could not save the Copilot account: {e}"))?;
        drop(file);
        super::replace_file_atomically(&temporary, path)
            .map_err(|e| format!("Could not replace the Copilot account file: {e}"))
    })();
    if let Err(error) = &result {
        if let Err(cleanup_error) = fs::remove_file(&temporary) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!(
                    "{error}; could not clean up the private Copilot account file: {cleanup_error}"
                ));
            }
        }
    }
    result
}

fn random_key() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| "Could not generate a secure Copilot request key".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn unix_now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| "The system clock is before the Unix epoch".to_string())
}
