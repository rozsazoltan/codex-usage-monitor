use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uchar, c_void, CStr, CString};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use std::os::windows::process::CommandExt;

use crate::diagnose;
use crate::localization::Strings;
use crate::models::{UsageData, UsageSection};
use crate::native_interop;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const ANTIGRAVITY_CREDENTIAL_TARGET: &str = "gemini:antigravity";
const ANTIGRAVITY_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MODEL_FALLBACK_CHAIN: &[&str] = &["claude-3-haiku-20240307", "claude-haiku-4-5-20251001"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    AuthRequired,
    NoCredentials,
    TokenExpired,
    NetworkUnavailable,
    RateLimited(Option<u32>),
    ServerError,
    RequestFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageWindowKind {
    Session,
    Weekly,
}

impl PollError {
    pub fn category(self) -> &'static str {
        match self {
            Self::AuthRequired => "auth_required",
            Self::NoCredentials => "no_credentials",
            Self::TokenExpired => "token_expired",
            Self::NetworkUnavailable => "network_unavailable",
            Self::RateLimited(_) => "rate_limited",
            Self::ServerError => "server_error",
            Self::RequestFailed => "invalid_response",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialWatchMode {
    ActiveSource,
    AllSources,
    Antigravity,
    Codex,
}

pub type CredentialWatchSnapshot = Vec<String>;

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageBucket>,
    seven_day: Option<UsageBucket>,
}

#[derive(Deserialize)]
struct UsageBucket {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexTokenData>,
}

#[derive(Clone, Deserialize)]
struct CodexTokenData {
    access_token: String,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct CodexUsageResponse {
    rate_limit: Option<Option<Box<CodexRateLimitDetails>>>,
}

#[derive(Deserialize)]
struct CodexRateLimitDetails {
    primary_window: Option<Option<Box<CodexRateLimitWindow>>>,
    secondary_window: Option<Option<Box<CodexRateLimitWindow>>>,
}

#[derive(Deserialize)]
struct CodexRateLimitWindow {
    used_percent: f64,
    reset_at: i64,
    limit_window_seconds: Option<u64>,
}

const CODEX_SESSION_WINDOW_MAX_SECONDS: u64 = 24 * 60 * 60;

#[derive(Deserialize)]
struct AntigravityAuthFile {
    token: AntigravityTokenData,
}

#[derive(Deserialize)]
struct AntigravityTokenData {
    access_token: String,
}

#[derive(Deserialize)]
struct AntigravityLoadResponse {
    #[serde(rename = "cloudaicompanionProject")]
    project: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityModelsResponse {
    models: HashMap<String, AntigravityModelInfo>,
}

#[derive(Deserialize)]
struct AntigravityModelInfo {
    #[serde(rename = "quotaInfo")]
    quota_info: Option<AntigravityQuotaInfo>,
}

#[derive(Deserialize)]
struct AntigravityQuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityQuotaSummaryResponse {
    groups: Option<Vec<AntigravityQuotaSummaryGroup>>,
}

#[derive(Deserialize)]
struct AntigravityQuotaSummaryGroup {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    description: Option<String>,
    buckets: Option<Vec<AntigravityQuotaSummaryBucket>>,
}

#[derive(Clone, Deserialize)]
struct AntigravityQuotaSummaryBucket {
    #[serde(rename = "bucketId")]
    bucket_id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    window: Option<String>,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: u64,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut c_void,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[link(name = "Advapi32")]
extern "system" {
    fn CredReadW(
        target_name: *const u16,
        type_: u32,
        reserved_flags: u32,
        credential: *mut *mut CredentialW,
    ) -> i32;
    fn CredEnumerateW(
        filter: *const u16,
        flags: u32,
        count: *mut u32,
        credentials: *mut *mut *mut CredentialW,
    ) -> i32;
    fn CredFree(buffer: *mut c_void);
}

pub fn poll_provider(id: usize) -> Result<UsageData, PollError> {
    match id {
        0 => poll_claude_code(),
        1 => poll_codex(),
        2 => poll_antigravity(),
        3 => poll_opencode(),
        4 => poll_cursor(),
        5 => poll_grok(),
        6 => poll_copilot(),
        _ => Err(PollError::RequestFailed),
    }
}

/// Whether Claude Code CLI credentials are available from a supported source.
/// Claude Desktop authentication is intentionally not treated as CLI access.
pub fn claude_code_credentials_available() -> bool {
    read_first_credentials().is_some()
}

fn poll_claude_code() -> Result<UsageData, PollError> {
    let creds = match read_first_credentials() {
        Some(c) => c,
        None => {
            diagnose::log("poll failed: no Claude credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    let creds = refresh_or_fallback(creds)?;

    fetch_usage_with_fallback(&creds.access_token)
}

fn poll_codex() -> Result<UsageData, PollError> {
    let creds = match read_codex_credentials() {
        Some(creds) => creds,
        None => {
            diagnose::log("Codex usage poll failed: no Codex credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    match fetch_codex_usage(&creds.access_token, creds.account_id.as_deref()) {
        Ok(data) => Ok(data),
        Err(PollError::AuthRequired) => {
            cli_refresh_codex_token();
            let refreshed = read_codex_credentials().ok_or(PollError::TokenExpired)?;
            fetch_codex_usage(&refreshed.access_token, refreshed.account_id.as_deref())
        }
        Err(error) => Err(error),
    }
}

fn poll_antigravity() -> Result<UsageData, PollError> {
    let creds = match read_antigravity_credentials() {
        Some(creds) => creds,
        None => {
            diagnose::log("Antigravity usage poll failed: no Antigravity credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    fetch_antigravity_usage(&creds.access_token)
}

fn refresh_or_fallback(mut creds: Credentials) -> Result<Credentials, PollError> {
    loop {
        if !is_token_expired(creds.expires_at) {
            return Ok(creds);
        }

        let source = creds.source.clone();
        cli_refresh_token(&source);

        match read_credentials_from_source(&source) {
            Some(refreshed) if !is_token_expired(refreshed.expires_at) => return Ok(refreshed),
            Some(_) => diagnose::log(format!(
                "credentials from {source:?} still expired after refresh attempt"
            )),
            None => diagnose::log(format!(
                "credentials from {source:?} unavailable after refresh attempt"
            )),
        }

        match read_next_credentials_after(&source) {
            Some(next) => creds = next,
            None => return Err(PollError::TokenExpired),
        }
    }
}

/// Invoke the Claude CLI with a minimal prompt to force its internal
/// OAuth token refresh.
fn cli_refresh_token(source: &CredentialSource) {
    match source {
        CredentialSource::Windows(_) => cli_refresh_windows_token(),
        CredentialSource::Wsl { distro } => cli_refresh_wsl_token(distro),
    }
}

fn cli_refresh_windows_token() {
    let claude_path = resolve_windows_claude_path();
    let is_cmd = claude_path.to_lowercase().ends_with(".cmd");
    diagnose::log(format!(
        "attempting Windows Claude token refresh via {claude_path}"
    ));

    let args: &[&str] = &["-p", "."];

    let mut cmd = if is_cmd {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&claude_path).args(args);
        c
    } else {
        let mut c = Command::new(&claude_path);
        c.args(args);
        c
    };
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Claude token refresh", error);
            return;
        }
    };

    // Wait up to 30 seconds — don't block the poll thread forever
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => break,
        }
    }
}

fn cli_refresh_wsl_token(distro: &str) {
    diagnose::log(format!(
        "attempting WSL Claude token refresh in distro {distro}"
    ));
    let mut cmd = Command::new("wsl.exe");
    cmd.arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lic")
        .arg("if command -v claude >/dev/null 2>&1; then claude -p .; elif [ -x \"$HOME/.local/bin/claude\" ]; then \"$HOME/.local/bin/claude\" -p .; else exit 127; fi")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn WSL Claude token refresh", error);
            return;
        }
    };

    wait_for_refresh(&mut child);
}

fn cli_refresh_codex_token() {
    let codex_path = resolve_windows_codex_path();
    let is_cmd = codex_path.to_lowercase().ends_with(".cmd");
    let is_ps1 = codex_path.to_lowercase().ends_with(".ps1");
    diagnose::log(format!(
        "attempting Windows Codex token refresh via {codex_path}"
    ));

    let args: &[&str] = &["exec", "."];

    let mut cmd = if is_cmd {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&codex_path).args(args);
        c
    } else if is_ps1 {
        let mut c = Command::new("powershell.exe");
        c.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&codex_path)
            .args(args);
        c
    } else {
        let mut c = Command::new(&codex_path);
        c.args(args);
        c
    };
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Codex token refresh", error);
            return;
        }
    };

    wait_for_refresh(&mut child);
}

/// Spawn a command and wait up to `timeout` for it to finish.
/// Returns None if the process fails to start or exceeds the deadline.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<std::process::Output> {
    let mut child = cmd.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

fn wait_for_refresh(child: &mut std::process::Child) {
    // Wait up to 30 seconds; don't block the poll thread forever.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => break,
        }
    }
}

/// Resolve the full path to the `claude` CLI executable.
fn resolve_windows_claude_path() -> String {
    for name in &["claude.cmd", "claude"] {
        if Command::new(name)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }

    for name in &["claude.cmd", "claude"] {
        if let Ok(output) = Command::new("where.exe")
            .arg(name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = first_line.trim().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
    }

    "claude.cmd".to_string()
}

fn resolve_windows_codex_path() -> String {
    for name in &["codex.cmd", "codex.ps1", "codex.exe", "codex"] {
        if Command::new(name)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }

    for name in &["codex.cmd", "codex.ps1", "codex.exe", "codex"] {
        if let Ok(output) = Command::new("where.exe")
            .arg(name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = first_line.trim().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
    }

    "codex.cmd".to_string()
}

fn build_agent() -> Result<ureq::Agent, PollError> {
    let tls = native_tls::TlsConnector::new().map_err(|_| PollError::RequestFailed)?;
    Ok(ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .tls_connector(std::sync::Arc::new(tls))
        .build())
}

fn classify_http_status(status: u16) -> PollError {
    match status {
        401 | 403 => PollError::AuthRequired,
        429 => PollError::RateLimited(None),
        500..=599 => PollError::ServerError,
        _ => PollError::RequestFailed,
    }
}

fn classify_ureq_error(error: &ureq::Error) -> PollError {
    match error {
        ureq::Error::Status(429, response) => PollError::RateLimited(
            response
                .header("Retry-After")
                .and_then(|value| crate::retry_after::delay_ms(value, SystemTime::now())),
        ),
        ureq::Error::Status(status, _) => classify_http_status(*status),
        ureq::Error::Transport(_) => PollError::NetworkUnavailable,
    }
}

pub fn credential_watch_snapshot(mode: CredentialWatchMode) -> CredentialWatchSnapshot {
    if mode == CredentialWatchMode::Codex {
        return codex_auth_path()
            .map(|p| vec![windows_credential_watch_signature(&p)])
            .unwrap_or_default();
    }
    if mode == CredentialWatchMode::Antigravity {
        return vec![antigravity_credential_watch_signature()];
    }

    let sources = match mode {
        CredentialWatchMode::ActiveSource => read_first_credentials()
            .map(|creds| vec![creds.source])
            .unwrap_or_else(all_known_credential_sources),
        CredentialWatchMode::AllSources => all_known_credential_sources(),
        CredentialWatchMode::Antigravity | CredentialWatchMode::Codex => unreachable!(),
    };

    let mut snapshot: CredentialWatchSnapshot = sources
        .into_iter()
        .filter_map(|source| credential_watch_signature(&source))
        .collect();
    snapshot.sort();
    snapshot.dedup();
    snapshot
}

fn all_known_credential_sources() -> Vec<CredentialSource> {
    let mut sources = Vec::new();
    if let Some(source) = windows_credential_source() {
        sources.push(source);
    }
    for distro in list_wsl_distros() {
        sources.push(CredentialSource::Wsl { distro });
    }
    sources
}

fn windows_credential_source() -> Option<CredentialSource> {
    Some(CredentialSource::Windows(windows_credentials_path_from(
        std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        dirs::home_dir(),
    )?))
}

fn windows_credentials_path_from(
    config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let config_dir = config_dir.filter(|path| !path.as_os_str().is_empty());
    let directory = config_dir.or_else(|| home.map(|path| path.join(".claude")))?;
    Some(directory.join(".credentials.json"))
}

fn credential_watch_signature(source: &CredentialSource) -> Option<String> {
    match source {
        CredentialSource::Windows(path) => Some(windows_credential_watch_signature(path)),
        CredentialSource::Wsl { distro } => wsl_credential_watch_signature(distro),
    }
}

fn windows_credential_watch_signature(path: &PathBuf) -> String {
    let key = format!("win:{}", path.display());
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_nanos())
                .unwrap_or(0);
            format!("{key}|present|{}|{modified}", metadata.len())
        }
        Err(_) => format!("{key}|missing"),
    }
}

fn wsl_credential_watch_signature(distro: &str) -> Option<String> {
    let output = run_with_timeout(
        Command::new("wsl.exe")
            .arg("-d")
            .arg(distro)
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg(
                "if [ -f ~/.claude/.credentials.json ]; then \
                 stat -c 'present|%s|%Y' ~/.claude/.credentials.json; \
                 else echo missing; fi",
            )
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    )?;

    let state = if output.status.success() {
        decode_wsl_text(&output.stdout).trim().to_string()
    } else {
        format!("status-{}", output.status)
    };

    Some(format!("wsl:{distro}|{state}"))
}

fn fetch_usage_with_fallback(token: &str) -> Result<UsageData, PollError> {
    // Try the dedicated usage endpoint first
    match try_usage_endpoint(token)? {
        Some(data) => {
            // If reset timers are missing, fill them in from the Messages API
            if data.session.resets_at.is_none() || data.weekly.resets_at.is_none() {
                if let Ok(fallback) = fetch_usage_via_messages(token) {
                    let mut merged = data;
                    if merged.session.resets_at.is_none() {
                        merged.session.resets_at = fallback.session.resets_at;
                    }
                    if merged.weekly.resets_at.is_none() {
                        merged.weekly.resets_at = fallback.weekly.resets_at;
                    }
                    return Ok(merged);
                }
            }
            return Ok(data);
        }
        None => {}
    }

    // Fall back to Messages API with rate limit headers
    let result = fetch_usage_via_messages(token);
    if result.is_err() {
        diagnose::log("usage endpoint and Messages API fallback both failed");
    }
    result
}

fn try_usage_endpoint(token: &str) -> Result<Option<UsageData>, PollError> {
    let agent = build_agent()?;

    let resp = match agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "usage endpoint returned auth error status {code}; re-login required"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(error @ ureq::Error::Status(429, _)) => return Err(classify_ureq_error(&error)),
        Err(_) => return Ok(None),
    };

    let response: UsageResponse = match resp.into_json() {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    let mut data = UsageData::default();

    if let Some(bucket) = &response.five_hour {
        data.session.percentage = bucket.utilization;
        data.session.resets_at = parse_iso8601(bucket.resets_at.as_deref());
    }

    if let Some(bucket) = &response.seven_day {
        data.weekly.percentage = bucket.utilization;
        data.weekly.resets_at = parse_iso8601(bucket.resets_at.as_deref());
    }

    Ok(Some(data))
}

fn fetch_usage_via_messages(token: &str) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let mut last_error = PollError::RequestFailed;

    for model in MODEL_FALLBACK_CHAIN {
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "."}]
        });

        let response = match agent
            .post(MESSAGES_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-version", "2023-06-01")
            .set("anthropic-beta", "oauth-2025-04-20")
            .send_json(&body)
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
                diagnose::log(format!(
                    "messages endpoint returned auth error status {code}; re-login required"
                ));
                return Err(PollError::AuthRequired);
            }
            Err(ureq::Error::Status(code, resp)) => {
                last_error =
                    if code == 429 {
                        PollError::RateLimited(resp.header("Retry-After").and_then(|value| {
                            crate::retry_after::delay_ms(value, SystemTime::now())
                        }))
                    } else {
                        classify_http_status(code)
                    };
                if matches!(last_error, PollError::RateLimited(_)) {
                    return Err(last_error);
                }
                resp
            }
            Err(error) => {
                last_error = classify_ureq_error(&error);
                continue;
            }
        };

        let h5 = response.header("anthropic-ratelimit-unified-5h-utilization");
        let h7 = response.header("anthropic-ratelimit-unified-7d-utilization");
        let hs = response.header("anthropic-ratelimit-unified-status");

        if h5.is_some() || h7.is_some() || hs.is_some() {
            return Ok(parse_rate_limit_headers(&response));
        }
    }

    Err(last_error)
}

fn parse_rate_limit_headers(response: &ureq::Response) -> UsageData {
    let mut data = UsageData::default();

    data.session.percentage =
        get_header_f64(response, "anthropic-ratelimit-unified-5h-utilization") * 100.0;
    data.session.resets_at = unix_to_system_time(get_header_i64(
        response,
        "anthropic-ratelimit-unified-5h-reset",
    ));

    data.weekly.percentage =
        get_header_f64(response, "anthropic-ratelimit-unified-7d-utilization") * 100.0;
    data.weekly.resets_at = unix_to_system_time(get_header_i64(
        response,
        "anthropic-ratelimit-unified-7d-reset",
    ));

    let overall_reset = get_header_i64(response, "anthropic-ratelimit-unified-reset");

    if data.session.percentage == 0.0 && data.weekly.percentage == 0.0 {
        let status = response.header("anthropic-ratelimit-unified-status");
        if status == Some("rejected") {
            let claim = response.header("anthropic-ratelimit-unified-representative-claim");
            match claim {
                Some("five_hour") => data.session.percentage = 100.0,
                Some("seven_day") => data.weekly.percentage = 100.0,
                _ => {}
            }
        }

        if data.session.resets_at.is_none() && overall_reset.is_some() {
            data.session.resets_at = unix_to_system_time(overall_reset);
        }
    }

    data
}

fn fetch_codex_usage(token: &str, account_id: Option<&str>) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let mut request = agent
        .get(CODEX_USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "codex-cli");

    if let Some(account_id) = account_id.filter(|value| !value.is_empty()) {
        request = request.set("ChatGPT-Account-Id", account_id);
    }

    let resp = match request.call() {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Codex usage endpoint request failed", error);
            return Err(classified);
        }
    };

    let response: CodexUsageResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Codex usage response", error);
            return Err(PollError::RequestFailed);
        }
    };

    codex_usage_from_response(response).ok_or(PollError::RequestFailed)
}

fn codex_usage_from_response(response: CodexUsageResponse) -> Option<UsageData> {
    let details = *response.rate_limit.flatten()?;
    let mut data = UsageData::default();
    let mut has_session = false;
    let mut has_weekly = false;

    let primary = details.primary_window.flatten();
    let secondary = details.secondary_window.flatten();

    for window in [primary.as_deref(), secondary.as_deref()]
        .into_iter()
        .flatten()
    {
        match codex_window_kind(window) {
            Some(UsageWindowKind::Session) if !has_session => {
                data.session = codex_section_from_window(window);
                has_session = true;
            }
            Some(UsageWindowKind::Weekly) if !has_weekly => {
                data.weekly = codex_section_from_window(window);
                has_weekly = true;
            }
            _ => {}
        }
    }

    // Older responses did not include a window duration. Preserve their
    // positional meaning while avoiding overwriting a duration-classified
    // window from the newer response shape.
    if let Some(window) = primary
        .as_deref()
        .filter(|window| window.limit_window_seconds.is_none() && !has_session)
    {
        data.session = codex_section_from_window(window);
    }
    if let Some(window) = secondary
        .as_deref()
        .filter(|window| window.limit_window_seconds.is_none() && !has_weekly)
    {
        data.weekly = codex_section_from_window(window);
    }

    Some(data)
}

fn codex_window_kind(window: &CodexRateLimitWindow) -> Option<UsageWindowKind> {
    window.limit_window_seconds.map(|seconds| {
        if seconds <= CODEX_SESSION_WINDOW_MAX_SECONDS {
            UsageWindowKind::Session
        } else {
            UsageWindowKind::Weekly
        }
    })
}

fn codex_section_from_window(window: &CodexRateLimitWindow) -> UsageSection {
    UsageSection {
        percentage: window.used_percent,
        resets_at: unix_to_system_time(Some(window.reset_at)),
    }
}

fn antigravity_credential_watch_signature() -> String {
    let Some(content) = read_windows_generic_credential(ANTIGRAVITY_CREDENTIAL_TARGET) else {
        return format!("{ANTIGRAVITY_CREDENTIAL_TARGET}|missing");
    };

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!(
        "{ANTIGRAVITY_CREDENTIAL_TARGET}|present|{}|{}",
        content.len(),
        hasher.finish()
    )
}

fn fetch_antigravity_usage(token: &str) -> Result<UsageData, PollError> {
    let mut auth_error = false;
    let mut last_error = PollError::RequestFailed;

    for base_url in ANTIGRAVITY_ENDPOINTS {
        match fetch_antigravity_usage_from_endpoint(base_url, token) {
            Ok(data) => return Ok(data),
            Err(PollError::AuthRequired) => auth_error = true,
            Err(error) => last_error = error,
        }
    }

    if auth_error {
        Err(PollError::AuthRequired)
    } else {
        Err(last_error)
    }
}

fn fetch_antigravity_usage_from_endpoint(
    base_url: &str,
    token: &str,
) -> Result<UsageData, PollError> {
    let project = fetch_antigravity_project(base_url, token)?;
    if let Some(project) = project.as_deref() {
        match fetch_antigravity_quota_summary(base_url, token, project) {
            Ok(data) => return Ok(data),
            Err(PollError::AuthRequired) => return Err(PollError::AuthRequired),
            Err(error) => diagnose::log(format!(
                "Antigravity retrieveUserQuotaSummary failed, falling back to model quota: {error:?}"
            )),
        }
    }

    let session = fetch_antigravity_model_quota(base_url, token, project.as_deref())?;
    let weekly = UsageSection::default();

    Ok(UsageData { session, weekly })
}

fn fetch_antigravity_project(base_url: &str, token: &str) -> Result<Option<String>, PollError> {
    let agent = build_agent()?;
    let body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
        }
    });

    let resp = match agent
        .post(&format!("{base_url}/v1internal:loadCodeAssist"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Antigravity loadCodeAssist request failed", error);
            return Err(classified);
        }
    };

    let response: AntigravityLoadResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Antigravity loadCodeAssist response", error);
            return Err(PollError::RequestFailed);
        }
    };

    Ok(response.project.filter(|project| !project.is_empty()))
}

fn fetch_antigravity_model_quota(
    base_url: &str,
    token: &str,
    project: Option<&str>,
) -> Result<UsageSection, PollError> {
    let agent = build_agent()?;
    let body = match project {
        Some(project) => serde_json::json!({ "project": project }),
        None => serde_json::json!({}),
    };

    let resp = match agent
        .post(&format!("{base_url}/v1internal:fetchAvailableModels"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Antigravity fetchAvailableModels request failed", error);
            return Err(classified);
        }
    };

    let response: AntigravityModelsResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error(
                "unable to parse Antigravity fetchAvailableModels response",
                error,
            );
            return Err(PollError::RequestFailed);
        }
    };

    best_antigravity_section(response.models.into_iter().filter_map(|(model, info)| {
        let quota = info.quota_info?;
        if !is_antigravity_display_model(&model) {
            return None;
        }
        antigravity_section_from_quota(quota)
    }))
    .ok_or(PollError::RequestFailed)
}

fn fetch_antigravity_quota_summary(
    base_url: &str,
    token: &str,
    project: &str,
) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let body = serde_json::json!({ "project": project });

    let resp = match agent
        .post(&format!("{base_url}/v1internal:retrieveUserQuotaSummary"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Antigravity retrieveUserQuotaSummary request failed", error);
            return Err(classified);
        }
    };

    let response: AntigravityQuotaSummaryResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error(
                "unable to parse Antigravity retrieveUserQuotaSummary response",
                error,
            );
            return Err(PollError::RequestFailed);
        }
    };

    antigravity_usage_from_summary(response).ok_or(PollError::RequestFailed)
}

fn antigravity_section_from_quota(quota: AntigravityQuotaInfo) -> Option<UsageSection> {
    let remaining = quota.remaining_fraction?.clamp(0.0, 1.0);
    Some(UsageSection {
        percentage: (1.0 - remaining) * 100.0,
        resets_at: parse_iso8601(quota.reset_time.as_deref()),
    })
}

fn antigravity_section_from_summary_bucket(
    bucket: &AntigravityQuotaSummaryBucket,
) -> Option<UsageSection> {
    let remaining = bucket.remaining_fraction?.clamp(0.0, 1.0);
    Some(UsageSection {
        percentage: (1.0 - remaining) * 100.0,
        resets_at: parse_iso8601(bucket.reset_time.as_deref()),
    })
}

fn antigravity_usage_from_summary(response: AntigravityQuotaSummaryResponse) -> Option<UsageData> {
    let mut fallback = None;

    for group in response.groups.unwrap_or_default() {
        let is_gemini = is_antigravity_gemini_summary_group(&group);
        let usage = antigravity_usage_from_summary_group(group);

        if is_gemini && usage.is_some() {
            return usage;
        }

        if fallback.is_none() {
            fallback = usage;
        }
    }

    fallback
}

fn antigravity_usage_from_summary_group(group: AntigravityQuotaSummaryGroup) -> Option<UsageData> {
    let mut data = UsageData::default();
    let mut has_quota = false;

    for bucket in group.buckets.unwrap_or_default() {
        let Some(section) = antigravity_section_from_summary_bucket(&bucket) else {
            continue;
        };

        match bucket.window.as_deref() {
            Some(window) if window.eq_ignore_ascii_case("5h") => {
                data.session = section;
                has_quota = true;
            }
            Some(window) if window.eq_ignore_ascii_case("weekly") => {
                data.weekly = section;
                has_quota = true;
            }
            _ => {}
        }
    }

    has_quota.then_some(data)
}

fn is_antigravity_gemini_summary_group(group: &AntigravityQuotaSummaryGroup) -> bool {
    group
        .display_name
        .as_deref()
        .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"))
        || group
            .description
            .as_deref()
            .is_some_and(|description| description.to_ascii_lowercase().contains("gemini"))
        || group.buckets.as_ref().is_some_and(|buckets| {
            buckets.iter().any(|bucket| {
                bucket
                    .bucket_id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase().starts_with("gemini-"))
                    || bucket
                        .display_name
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"))
            })
        })
}

fn best_antigravity_section<I>(sections: I) -> Option<UsageSection>
where
    I: IntoIterator<Item = UsageSection>,
{
    sections.into_iter().max_by(|a, b| {
        a.percentage
            .partial_cmp(&b.percentage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.resets_at.cmp(&b.resets_at))
    })
}

fn is_antigravity_display_model(model: &str) -> bool {
    model.starts_with("gemini")
        || model.starts_with("claude")
        || model.starts_with("gpt")
        || model.starts_with("image")
        || model.starts_with("imagen")
}

const OPENCODE_GO_STATUS_URL: &str = "https://opencode.ai/console/api/go/status";
const OPENCODE_GO_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/126.0 Safari/537.36";
const OPENCODE_GO_WORKSPACE_ID_ENV: &str = "OPENCODE_GO_WORKSPACE_ID";
const OPENCODE_GO_AUTH_COOKIE_ENV: &str = "OPENCODE_GO_AUTH_COOKIE";
const OPENCODE_GO_CONFIG_FILE_ENV: &str = "OPENCODE_GO_CONFIG_FILE";

#[derive(Deserialize)]
struct OpenCodeDashboardConfig {
    #[serde(alias = "workspaceId", alias = "workspaceID")]
    workspace_id: String,
    #[serde(alias = "authCookie", alias = "cookie")]
    auth_cookie: String,
}

struct OpenCodeCredentials {
    workspace_id: String,
    auth_cookie: String,
    source: String,
}

#[derive(Deserialize)]
struct OpenCodeGoStatus {
    access: Option<OpenCodeGoAccess>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeGoAccess {
    ends_at: String,
    meters: OpenCodeGoMeters,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeGoMeters {
    five_hour: OpenCodeGoTimedMeter,
    week: OpenCodeGoTimedMeter,
    month: OpenCodeGoMeter,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeGoTimedMeter {
    #[serde(flatten)]
    meter: OpenCodeGoMeter,
    // An unused rolling window has no reset time until the first request.
    resets_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeGoMeter {
    limit_micro_cents: String,
    used_micro_cents: String,
}

#[derive(Clone, Debug, PartialEq)]
struct OpenCodeUsageWindow {
    usage_percent: f64,
    resets_at: Option<SystemTime>,
}

#[derive(Debug, Default, PartialEq)]
struct OpenCodeDashboardUsage {
    rolling: Option<OpenCodeUsageWindow>,
    weekly: Option<OpenCodeUsageWindow>,
    monthly: Option<OpenCodeUsageWindow>,
}

fn poll_opencode() -> Result<UsageData, PollError> {
    let credentials = read_opencode_credentials().ok_or_else(|| {
        diagnose::log("OpenCode usage poll failed: no dashboard credentials found".to_string());
        PollError::NoCredentials
    })?;
    let usage =
        fetch_opencode_status(&credentials, OPENCODE_GO_STATUS_URL).inspect_err(|error| {
            diagnose::log(format!(
                "OpenCode dashboard poll failed via {}: {error:?}",
                credentials.source
            ));
        })?;

    if usage.rolling.is_none() && usage.weekly.is_none() && usage.monthly.is_none() {
        diagnose::log(format!(
            "OpenCode dashboard returned no usage windows from {}",
            credentials.source
        ));
        return Err(PollError::RequestFailed);
    }

    let session = usage
        .rolling
        .as_ref()
        .map(opencode_section_from_window)
        .unwrap_or_default();
    // The long-window label ("7d"/"30d") is selected alongside the section but
    // only surfaces in the settings protocol and widget once P5 extends the
    // model; until then the weekly slot carries the selected window.
    let (weekly, _) = select_opencode_long_window(&usage);

    Ok(UsageData { session, weekly })
}

fn select_opencode_long_window(usage: &OpenCodeDashboardUsage) -> (UsageSection, Option<String>) {
    match (&usage.weekly, &usage.monthly) {
        (Some(weekly), Some(monthly)) if monthly.usage_percent > weekly.usage_percent => (
            opencode_section_from_window(monthly),
            Some("30d".to_string()),
        ),
        (Some(weekly), _) => (opencode_section_from_window(weekly), Some("7d".to_string())),
        (None, Some(monthly)) => (
            opencode_section_from_window(monthly),
            Some("30d".to_string()),
        ),
        (None, None) => (UsageSection::default(), None),
    }
}

fn opencode_section_from_window(window: &OpenCodeUsageWindow) -> UsageSection {
    UsageSection {
        percentage: window.usage_percent.clamp(0.0, 100.0),
        resets_at: window.resets_at,
    }
}

fn read_opencode_credentials() -> Option<OpenCodeCredentials> {
    if let (Some(workspace_id), Some(auth_cookie)) = (
        opencode_non_empty_env(OPENCODE_GO_WORKSPACE_ID_ENV),
        opencode_non_empty_env(OPENCODE_GO_AUTH_COOKIE_ENV),
    ) {
        if valid_opencode_workspace_id(&workspace_id) && valid_opencode_cookie(&auth_cookie) {
            return Some(OpenCodeCredentials {
                workspace_id,
                auth_cookie,
                source: "environment".to_string(),
            });
        }
    }

    opencode_config_paths()
        .into_iter()
        .find_map(|path| read_opencode_config(&path))
}

fn read_opencode_config(path: &PathBuf) -> Option<OpenCodeCredentials> {
    let content = std::fs::read_to_string(path).ok()?;
    let config: OpenCodeDashboardConfig = serde_json::from_str(&content).ok()?;
    let workspace_id = config.workspace_id.trim().to_string();
    let auth_cookie = config.auth_cookie.trim().to_string();
    if !valid_opencode_workspace_id(&workspace_id) || !valid_opencode_cookie(&auth_cookie) {
        return None;
    }
    Some(OpenCodeCredentials {
        workspace_id,
        auth_cookie,
        source: path.display().to_string(),
    })
}

fn fetch_opencode_status(
    credentials: &OpenCodeCredentials,
    url: &str,
) -> Result<OpenCodeDashboardUsage, PollError> {
    let agent = build_agent()?;
    let cookie = if credentials.auth_cookie.split(';').any(|part| {
        let part = part.trim_start();
        part.starts_with("auth=") || part.starts_with("__Host-console_session=")
    }) {
        credentials.auth_cookie.clone()
    } else {
        // Bare legacy auth values may contain '=' padding; that alone does
        // not identify a complete Cookie header.
        format!("auth={}", credentials.auth_cookie)
    };

    let resp = match agent
        .get(url)
        .set("Accept", "application/json")
        .set("x-org-id", &credentials.workspace_id)
        .set("Cookie", &cookie)
        .set("User-Agent", OPENCODE_GO_USER_AGENT)
        .call()
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            return Err(PollError::AuthRequired)
        }
        Err(error) => {
            diagnose::log(format!("OpenCode Go status request failed: {error:?}"));
            return Err(PollError::RequestFailed);
        }
    };

    let status = resp
        .into_json::<Option<OpenCodeGoStatus>>()
        .map_err(|error| {
            diagnose::log(format!(
                "OpenCode Go status response is not valid JSON: {error:?}"
            ));
            PollError::RequestFailed
        })?;
    usage_from_opencode_status(status)
}

fn usage_from_opencode_status(
    status: Option<OpenCodeGoStatus>,
) -> Result<OpenCodeDashboardUsage, PollError> {
    let access = status.and_then(|status| status.access).ok_or_else(|| {
        diagnose::log("OpenCode Go status returned no active subscription access".to_string());
        PollError::RequestFailed
    })?;
    Ok(OpenCodeDashboardUsage {
        rolling: Some(window_from_opencode_meter(
            &access.meters.five_hour.meter,
            access.meters.five_hour.resets_at.as_deref(),
        )?),
        weekly: Some(window_from_opencode_meter(
            &access.meters.week.meter,
            access.meters.week.resets_at.as_deref(),
        )?),
        monthly: Some(window_from_opencode_meter(
            &access.meters.month,
            Some(access.ends_at.as_str()),
        )?),
    })
}

fn window_from_opencode_meter(
    meter: &OpenCodeGoMeter,
    resets_at: Option<&str>,
) -> Result<OpenCodeUsageWindow, PollError> {
    let limit = meter.limit_micro_cents.parse::<u128>().map_err(|_| {
        diagnose::log("OpenCode Go status contains an invalid usage limit".to_string());
        PollError::RequestFailed
    })?;
    let used = meter.used_micro_cents.parse::<u128>().map_err(|_| {
        diagnose::log("OpenCode Go status contains an invalid usage amount".to_string());
        PollError::RequestFailed
    })?;
    let resets_at = resets_at
        .map(|value| {
            parse_iso8601(Some(value)).ok_or_else(|| {
                diagnose::log("OpenCode Go status contains an invalid reset time".to_string());
                PollError::RequestFailed
            })
        })
        .transpose()?;
    Ok(OpenCodeUsageWindow {
        usage_percent: if limit == 0 {
            0.0
        } else {
            used as f64 / limit as f64 * 100.0
        },
        resets_at,
    })
}

fn opencode_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = opencode_non_empty_env(OPENCODE_GO_CONFIG_FILE_ENV).map(PathBuf::from) {
        paths.push(path);
    }
    if let Some(app_data) = opencode_non_empty_env("APPDATA").map(PathBuf::from) {
        paths.push(app_data.join("opencode-go").join("config.json"));
    }
    if let Some(config_home) = opencode_non_empty_env("XDG_CONFIG_HOME").map(PathBuf::from) {
        paths.push(config_home.join("opencode-bar").join("opencode-go.json"));
        paths.push(config_home.join("opencode-quota").join("opencode-go.json"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join(".config")
                .join("opencode-bar")
                .join("opencode-go.json"),
        );
        paths.push(
            home.join(".config")
                .join("opencode-quota")
                .join("opencode-go.json"),
        );
    }
    paths
}

fn opencode_non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn valid_opencode_workspace_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_opencode_cookie(value: &str) -> bool {
    !value.is_empty() && !value.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
}

const CURSOR_USAGE_SUMMARY_URL: &str = "https://cursor.com/api/usage-summary";
const CURSOR_SESSION_TOKEN_ENV: &str = "CURSOR_SESSION_TOKEN";
const CURSOR_ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";

// Minimal read-only winsqlite3.dll binding (ships with Windows) — avoids a
// bundled-SQLite dependency just to read Cursor's state database.
const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READONLY: c_int = 0x00000001;

#[link(name = "winsqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        pp_db: *mut *mut c_void,
        flags: c_int,
        z_vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut c_void,
        sql: *const c_char,
        n_byte: c_int,
        pp_stmt: *mut *mut c_void,
        pz_tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut c_void,
        index: c_int,
        value: *const c_char,
        n: c_int,
        destructor: *const c_void,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut c_void) -> c_int;
    fn sqlite3_column_text(stmt: *mut c_void, col: c_int) -> *const c_uchar;
    fn sqlite3_finalize(stmt: *mut c_void) -> c_int;
    fn sqlite3_close_v2(db: *mut c_void) -> c_int;
    fn sqlite3_errmsg(db: *mut c_void) -> *const c_char;
}

#[derive(Deserialize)]
struct CursorUsageSummaryResponse {
    #[serde(rename = "billingCycleEnd")]
    billing_cycle_end: Option<String>,
    #[serde(rename = "individualUsage")]
    individual_usage: Option<CursorIndividualUsage>,
}

#[derive(Deserialize)]
struct CursorIndividualUsage {
    plan: Option<CursorPlanUsage>,
}

#[derive(Deserialize)]
struct CursorPlanUsage {
    #[serde(rename = "autoPercentUsed")]
    auto_percent_used: Option<f64>,
    #[serde(rename = "apiPercentUsed")]
    api_percent_used: Option<f64>,
    #[serde(rename = "totalPercentUsed")]
    total_percent_used: Option<f64>,
}

fn poll_cursor() -> Result<UsageData, PollError> {
    let cookie = read_cursor_session_cookie().ok_or_else(|| {
        diagnose::log(
            "Cursor usage poll failed: no Cursor session found (sign in to Cursor or set CURSOR_SESSION_TOKEN)"
                .to_string(),
        );
        PollError::NoCredentials
    })?;
    fetch_cursor_usage(&cookie, CURSOR_USAGE_SUMMARY_URL)
}

/// Resolve a Cursor dashboard session cookie. An explicit environment value
/// takes priority over the access token persisted by Cursor itself.
fn read_cursor_session_cookie() -> Option<String> {
    if let Some(token) = opencode_non_empty_env(CURSOR_SESSION_TOKEN_ENV) {
        return normalize_cursor_session_cookie(&token);
    }

    let access_token = read_cursor_access_token_from_state_db()?;
    cursor_cookie_from_access_token(&access_token)
}

fn normalize_cursor_session_cookie(token: &str) -> Option<String> {
    if token.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return None;
    }
    let token = token
        .trim()
        .strip_prefix("WorkosCursorSessionToken=")
        .unwrap_or(token.trim())
        .trim();
    if token.is_empty() {
        None
    } else if token.contains("%3A%3A") {
        Some(token.to_string())
    } else if token.contains("::") {
        Some(token.replace("::", "%3A%3A"))
    } else {
        cursor_cookie_from_access_token(token).or_else(|| Some(token.to_string()))
    }
}

fn cursor_cookie_from_access_token(access_token: &str) -> Option<String> {
    let user_id = extract_cursor_user_id(access_token)?;
    Some(format!("{user_id}%3A%3A{access_token}"))
}

fn extract_cursor_user_id(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let decoded = base64_url_decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let subject = json.get("sub")?.as_str()?;
    Some(
        subject
            .rsplit_once('|')
            .map(|(_, id)| id.to_string())
            .unwrap_or_else(|| subject.to_string()),
    )
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    if input.len() % 4 == 1 {
        return None;
    }
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    let padding_mask = (1u32 << bits).saturating_sub(1);
    (buffer & padding_mask == 0).then_some(output)
}

fn cursor_state_db_path() -> Option<PathBuf> {
    let path = PathBuf::from(opencode_non_empty_env("APPDATA")?)
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    path.is_file().then_some(path)
}

fn read_cursor_access_token_from_state_db() -> Option<String> {
    let path = cursor_state_db_path()?;
    match query_cursor_state_value(&path, CURSOR_ACCESS_TOKEN_KEY) {
        Ok(token) => token,
        Err(error) => {
            diagnose::log(format!(
                "Cursor state DB direct read failed ({error}); retrying via temp copy"
            ));
            query_cursor_state_value_from_copy(&path, CURSOR_ACCESS_TOKEN_KEY)
        }
    }
}

fn query_cursor_state_value_from_copy(path: &PathBuf, key: &str) -> Option<String> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!(
        "codex-usage-cursor-state-{}-{unique}.vscdb",
        std::process::id()
    ));
    if let Err(error) = std::fs::copy(path, &temporary) {
        diagnose::log(format!("Cursor state DB temp copy failed: {error}"));
        return None;
    }
    let result = query_cursor_state_value(&temporary, key);
    let _ = std::fs::remove_file(&temporary);
    match result {
        Ok(token) => token,
        Err(error) => {
            diagnose::log(format!("Cursor state DB temp-copy read failed: {error}"));
            None
        }
    }
}

fn query_cursor_state_value(path: &PathBuf, key: &str) -> Result<Option<String>, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "state DB path is not valid Unicode".to_string())?;
    let path_c = CString::new(path_str).map_err(|error| error.to_string())?;
    let sql_c =
        CString::new("SELECT value FROM ItemTable WHERE key = ?1").map_err(|e| e.to_string())?;
    let key_c = CString::new(key).map_err(|error| error.to_string())?;

    unsafe {
        let mut db: *mut c_void = std::ptr::null_mut();
        let rc = sqlite3_open_v2(
            path_c.as_ptr(),
            &mut db,
            SQLITE_OPEN_READONLY,
            std::ptr::null(),
        );
        if rc != SQLITE_OK || db.is_null() {
            if !db.is_null() {
                sqlite3_close_v2(db);
            }
            return Err(format!("state DB open failed: {rc}"));
        }

        let mut stmt: *mut c_void = std::ptr::null_mut();
        let rc = sqlite3_prepare_v2(db, sql_c.as_ptr(), -1, &mut stmt, std::ptr::null_mut());
        if rc != SQLITE_OK || stmt.is_null() {
            let detail = db_message(db);
            sqlite3_close_v2(db);
            return Err(format!("state DB prepare failed: {rc} ({detail})"));
        }

        // SQLITE_TRANSIENT (-1): SQLite copies the bound text immediately.
        let rc = sqlite3_bind_text(stmt, 1, key_c.as_ptr(), -1, (-1isize) as *const c_void);
        if rc != SQLITE_OK {
            sqlite3_finalize(stmt);
            sqlite3_close_v2(db);
            return Err(format!("state DB bind failed: {rc}"));
        }

        let value = match sqlite3_step(stmt) {
            SQLITE_ROW => {
                let text = sqlite3_column_text(stmt, 0);
                if text.is_null() {
                    None
                } else {
                    Some(
                        CStr::from_ptr(text as *const c_char)
                            .to_string_lossy()
                            .into_owned(),
                    )
                }
            }
            SQLITE_DONE => None,
            rc => {
                let detail = db_message(db);
                sqlite3_finalize(stmt);
                sqlite3_close_v2(db);
                return Err(format!("state DB step failed: {rc} ({detail})"));
            }
        };
        sqlite3_finalize(stmt);
        sqlite3_close_v2(db);
        Ok(value.filter(|value| !value.is_empty()))
    }
}

unsafe fn db_message(db: *mut c_void) -> String {
    let message = sqlite3_errmsg(db);
    if message.is_null() {
        return String::new();
    }
    CStr::from_ptr(message).to_string_lossy().into_owned()
}

fn fetch_cursor_usage(cookie: &str, url: &str) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let cookie_header = format!("WorkosCursorSessionToken={cookie}");

    let resp = match agent
        .get(url)
        .set("Cookie", &cookie_header)
        .set("User-Agent", "Mozilla/5.0")
        .call()
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            return Err(PollError::AuthRequired)
        }
        Err(error) => {
            diagnose::log(format!("Cursor usage-summary request failed: {error:?}"));
            return Err(PollError::RequestFailed);
        }
    };

    let response: CursorUsageSummaryResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log(format!(
                "unable to parse Cursor usage-summary response: {error:?}"
            ));
            return Err(PollError::RequestFailed);
        }
    };
    cursor_usage_from_summary(response).ok_or_else(|| {
        diagnose::log("Cursor usage-summary response missing plan usage".to_string());
        PollError::RequestFailed
    })
}

fn cursor_usage_from_summary(response: CursorUsageSummaryResponse) -> Option<UsageData> {
    let plan = response.individual_usage?.plan?;
    let reset = parse_iso8601(response.billing_cycle_end.as_deref());
    let section = |percentage: Option<f64>| {
        percentage
            .map(|percentage| UsageSection {
                percentage: percentage.clamp(0.0, 100.0),
                resets_at: reset,
            })
            .unwrap_or_default()
    };
    // The API-slot label ("API") cannot be stored yet — UsageData gains
    // per-provider labels in P5; until then the weekly slot carries it.
    Some(UsageData {
        session: section(plan.auto_percent_used.or(plan.total_percent_used)),
        weekly: section(plan.api_percent_used),
    })
}

fn get_header_f64(response: &ureq::Response, name: &str) -> f64 {
    response
        .header(name)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn get_header_i64(response: &ureq::Response, name: &str) -> Option<i64> {
    response.header(name).and_then(|s| s.parse::<i64>().ok())
}

fn unix_to_system_time(unix_secs: Option<i64>) -> Option<SystemTime> {
    let secs = unix_secs?;
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

// ---------------------------------------------------------------------------
// Grok Build provider (P3).
//
// Ported from CodeZeno's `src/poller/grok.rs`: Grok Build reads its own
// `/usage` panel from the CLI chat proxy, and the monitor asks the same
// endpoint with the session the CLI already stored. The billing pool lands
// in the weekly slot (Grok has no five-hour window); per-product limits,
// the monthly label, and on-demand credits reach UsageData in P5.
// ---------------------------------------------------------------------------

const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
/// Static marker the proxy uses to route CLI user-token requests.
const GROK_TOKEN_AUTH: &str = "xai-grok-cli";
const GROK_CLIENT_MODE: &str = "cli";
/// Used only when the installed CLI cannot be asked for its own version.
const DEFAULT_GROK_CLIENT_VERSION: &str = "1.0.0";
const GROK_HOME_ENV: &str = "GROK_HOME";
const GROK_CLIENT_VERSION_ENV: &str = "GROK_CLIENT_VERSION";

/// One entry of `auth.json`, keyed by `"{issuer}::{client_id}"` or the
/// legacy `"https://accounts.x.ai/sign-in"` scope.
#[derive(Clone, Debug, Deserialize)]
struct GrokAuthEntry {
    #[serde(default)]
    key: String,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GrokBillingResponse {
    #[serde(default)]
    config: Option<GrokBillingConfig>,
}

#[derive(Debug, Deserialize)]
struct GrokBillingConfig {
    #[serde(default, rename = "creditUsagePercent")]
    credit_usage_percent: Option<f64>,
    #[serde(default, rename = "currentPeriod")]
    current_period: Option<GrokBillingPeriod>,
}

#[derive(Debug, Deserialize)]
struct GrokBillingPeriod {
    #[serde(default)]
    end: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GrokSession {
    access_token: String,
    user_id: String,
    /// The CLI can renew this session silently, without a browser.
    can_refresh: bool,
}

fn poll_grok() -> Result<UsageData, PollError> {
    let path = grok_auth_path().ok_or(PollError::NoCredentials)?;
    let session = match read_grok_session(&path) {
        Some(session) => session,
        None => {
            diagnose::log(
                "Grok usage poll failed: no Grok session found (run 'grok login' to sign in)",
            );
            return Err(PollError::NoCredentials);
        }
    };

    match fetch_grok_usage(&session, GROK_BILLING_URL) {
        Err(PollError::AuthRequired) => {
            // The CLI owns this file and refreshes it silently whenever it
            // needs a token, so let it do the work rather than racing its
            // lock with a refresh of our own. Without a refresh token it
            // would fall back to an interactive browser sign-in, which a
            // background monitor must never trigger.
            if !session.can_refresh {
                return Err(PollError::TokenExpired);
            }
            cli_refresh_grok_token();
            let refreshed = read_grok_session(&path).ok_or(PollError::TokenExpired)?;
            if refreshed.access_token == session.access_token {
                return Err(PollError::TokenExpired);
            }
            fetch_grok_usage(&refreshed, GROK_BILLING_URL)
        }
        other => other,
    }
}

fn grok_auth_path() -> Option<PathBuf> {
    if let Some(home) = opencode_non_empty_env(GROK_HOME_ENV) {
        return Some(PathBuf::from(home).join("auth.json"));
    }
    Some(dirs::home_dir()?.join(".grok").join("auth.json"))
}

fn read_grok_session(path: &Path) -> Option<GrokSession> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            diagnose::log_error(
                &format!("unable to read Grok credentials at {}", path.display()),
                error,
            );
            return None;
        }
    };
    select_grok_session(&content)
}

/// `auth.json` can also hold corporate OIDC tokens and plain API keys. Only
/// xAI sign-in scopes belong at the fixed production billing endpoint.
/// Prefer the latest-expiring usable xAI session.
fn select_grok_session(content: &str) -> Option<GrokSession> {
    let entries: BTreeMap<String, GrokAuthEntry> = serde_json::from_str(content).ok()?;
    entries
        .into_iter()
        .filter(|(scope, _)| {
            scope == "https://accounts.x.ai/sign-in"
                || scope
                    .strip_prefix("https://auth.x.ai::")
                    .is_some_and(|client| !client.is_empty())
        })
        .map(|(_, entry)| entry)
        .filter(|entry| !entry.key.trim().is_empty())
        .max_by_key(|entry| {
            entry
                .expires_at
                .as_deref()
                .and_then(|value| parse_iso8601(Some(value)))
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_secs())
                .unwrap_or(0)
        })
        .map(|entry| GrokSession {
            access_token: entry.key.trim().to_string(),
            user_id: entry.user_id.unwrap_or_default().trim().to_string(),
            can_refresh: entry
                .refresh_token
                .is_some_and(|token| !token.trim().is_empty()),
        })
}

fn fetch_grok_usage(session: &GrokSession, url: &str) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let mut request = agent
        .get(url)
        .set("Authorization", &format!("Bearer {}", session.access_token))
        .set("X-XAI-Token-Auth", GROK_TOKEN_AUTH)
        .set("x-grok-client-mode", GROK_CLIENT_MODE)
        .set("x-grok-client-version", &grok_client_version());
    if !session.user_id.is_empty() {
        request = request.set("x-userid", &session.user_id);
    }

    let resp = match request.call() {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "Grok billing endpoint returned auth error status {code}; refresh required"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(error) => {
            diagnose::log(format!("Grok billing endpoint request failed: {error:?}"));
            return Err(classify_ureq_error(&error));
        }
    };

    let response: GrokBillingResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log(format!("unable to parse Grok billing response: {error:?}"));
            return Err(PollError::RequestFailed);
        }
    };
    grok_usage_from_billing(response).ok_or_else(|| {
        diagnose::log("Grok billing response missing credit usage".to_string());
        PollError::RequestFailed
    })
}

fn grok_usage_from_billing(response: GrokBillingResponse) -> Option<UsageData> {
    let config = response.config?;
    let period = config.current_period.as_ref();
    let resets_at = period.and_then(|period| grok_period_end(period.end.as_ref()));
    // The proxy speaks protobuf JSON, which omits zero values: right after a
    // reset a complete period (with a usable end) arrives without
    // `creditUsagePercent`. That is 0% spent.
    let percentage = config.credit_usage_percent.or(resets_at.map(|_| 0.0))?;
    // Grok spends one pool per billing period and has no five-hour window, so
    // the pool lands in `weekly`, which is what tray badges read. The
    // monthly label and on-demand credits reach UsageData in P5.
    Some(UsageData {
        session: UsageSection::default(),
        weekly: UsageSection {
            percentage: percentage.clamp(0.0, 100.0),
            resets_at,
        },
    })
}

/// The proxy returns an RFC 3339 timestamp, but proto3 JSON also allows a
/// bare number of seconds.
fn grok_period_end(value: Option<&serde_json::Value>) -> Option<SystemTime> {
    match value? {
        serde_json::Value::String(text) => parse_iso8601(Some(text)),
        serde_json::Value::Number(number) => {
            let seconds = number.as_f64()?;
            // Milliseconds once the value is far past any plausible epoch second.
            let seconds = if seconds > 100_000_000_000.0 {
                seconds / 1000.0
            } else {
                seconds
            };
            if seconds <= 0.0 {
                return None;
            }
            UNIX_EPOCH.checked_add(Duration::try_from_secs_f64(seconds).ok()?)
        }
        _ => None,
    }
}

/// Cache a detected CLI version, but retry after transient detection failures.
fn grok_client_version() -> String {
    static VERSION: OnceLock<String> = OnceLock::new();
    cached_grok_cli_detection(&VERSION, || {
        opencode_non_empty_env(GROK_CLIENT_VERSION_ENV).or_else(cli_grok_version)
    })
    .unwrap_or_else(|| DEFAULT_GROK_CLIENT_VERSION.to_string())
}

/// Only successful discovery is stable enough to cache for this process.
fn cached_grok_cli_detection(
    cache: &OnceLock<String>,
    detect: impl FnOnce() -> Option<String>,
) -> Option<String> {
    cache.get().cloned().or_else(|| {
        let detected = detect()?;
        let _ = cache.set(detected.clone());
        Some(detected)
    })
}

fn cli_grok_version() -> Option<String> {
    let version = read_grok_cli_version(&resolve_windows_grok_path(), Duration::from_secs(5))?;
    diagnose::log(format!("Grok CLI reports version {version}"));
    Some(version)
}

fn read_grok_cli_version(path: &str, timeout: Duration) -> Option<String> {
    let start = std::time::Instant::now();
    let mut child = grok_command(path, &["version"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    // Drain while the process runs so a full pipe cannot block its exit.
    // Bound both the captured output and the wait, including inherited pipes.
    std::thread::spawn(move || {
        let mut output = String::new();
        let result = stdout.take(64 * 1024).read_to_string(&mut output);
        let _ = sender.send(result.ok().map(|_| output));
    });
    if !wait_for_grok_command(&mut child, timeout) {
        return None;
    }
    let output = receiver
        .recv_timeout(timeout.saturating_sub(start.elapsed()))
        .ok()??;
    first_grok_version(&output)
}

/// Pick the first `major.minor.patch` token out of arbitrary CLI output.
fn first_grok_version(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|token| token.trim_matches('.'))
        .find(|token| {
            let mut parts = token.split('.');
            let numeric = |part: Option<&str>| {
                part.is_some_and(|part| {
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
                })
            };
            numeric(parts.next()) && numeric(parts.next()) && numeric(parts.next())
        })
        .map(str::to_string)
}

/// `grok models` needs a valid token but no model call, so it refreshes the
/// stored session without spending any of the allowance.
fn cli_refresh_grok_token() {
    let grok_path = resolve_windows_grok_path();
    diagnose::log(format!(
        "attempting Windows Grok token refresh via {grok_path}"
    ));

    let mut child = match grok_command(&grok_path, &["models"]).spawn() {
        Ok(child) => child,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Grok token refresh", error);
            return;
        }
    };
    wait_for_grok_command(&mut child, Duration::from_secs(30));
}

fn grok_command(grok_path: &str, args: &[&str]) -> Command {
    let lowercase = grok_path.to_lowercase();
    let mut command = if lowercase.ends_with(".cmd") || lowercase.ends_with(".bat") {
        let mut command = Command::new("cmd.exe");
        command.arg("/d").arg("/c").arg(grok_path).args(args);
        command
    } else if lowercase.ends_with(".ps1") {
        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(grok_path)
            .args(args);
        command
    } else {
        let mut command = Command::new(grok_path);
        command.args(args);
        command
    };
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    command
}

fn resolve_windows_grok_path() -> String {
    static PATH: OnceLock<String> = OnceLock::new();
    cached_grok_cli_detection(&PATH, || {
        // The official installer drops a real executable in ~/.grok/bin;
        // prefer it over an npm shim that would cost an extra cmd.exe hop.
        for name in ["grok.exe", "grok.cmd", "grok.ps1", "grok"] {
            if let Ok(output) = Command::new("where.exe")
                .arg(name)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if let Some(path) = stdout
                        .lines()
                        .next()
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                    {
                        return Some(path.to_string());
                    }
                }
            }
        }
        None
    })
    .unwrap_or_else(|| "grok.cmd".to_string())
}

fn wait_for_grok_command(child: &mut std::process::Child, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

struct Credentials {
    access_token: String,
    expires_at: Option<i64>,
    source: CredentialSource,
}

#[derive(Clone, Debug)]
enum CredentialSource {
    Windows(PathBuf),
    Wsl { distro: String },
}

fn read_first_credentials() -> Option<Credentials> {
    if let Some(creds) = read_windows_credentials() {
        return Some(creds);
    }

    for distro in list_wsl_distros() {
        if let Some(creds) = read_wsl_credentials(&distro) {
            return Some(creds);
        }
    }

    None
}

fn read_windows_credentials() -> Option<Credentials> {
    let CredentialSource::Windows(cred_path) = windows_credential_source()? else {
        return None;
    };
    let content = match std::fs::read_to_string(&cred_path) {
        Ok(content) => content,
        Err(error) => {
            if diagnose::is_enabled() {
                diagnose::log_error(
                    &format!(
                        "unable to read Windows credentials at {}",
                        cred_path.display()
                    ),
                    error,
                );
            }
            return None;
        }
    };
    parse_credentials(&content, CredentialSource::Windows(cred_path))
}

fn read_credentials_from_source(source: &CredentialSource) -> Option<Credentials> {
    match source {
        CredentialSource::Windows(path) => {
            let content = std::fs::read_to_string(path).ok()?;
            parse_credentials(&content, source.clone())
        }
        CredentialSource::Wsl { distro } => read_wsl_credentials(distro),
    }
}

fn codex_auth_path() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        return Some(codex_home.join("auth.json"));
    }

    Some(dirs::home_dir()?.join(".codex").join("auth.json"))
}

fn read_codex_credentials() -> Option<CodexTokenData> {
    let auth_path = codex_auth_path()?;
    let content = match std::fs::read_to_string(&auth_path) {
        Ok(content) => content,
        Err(error) => {
            diagnose::log_error(
                &format!(
                    "unable to read Codex credentials at {}",
                    auth_path.display()
                ),
                error,
            );
            return None;
        }
    };

    let auth: CodexAuthFile = serde_json::from_str(&content).ok()?;
    auth.tokens.filter(|tokens| !tokens.access_token.is_empty())
}

fn read_antigravity_credentials() -> Option<AntigravityTokenData> {
    let content = read_windows_generic_credential(ANTIGRAVITY_CREDENTIAL_TARGET)?;
    let auth: AntigravityAuthFile = serde_json::from_str(&content).ok()?;
    if auth.token.access_token.is_empty() {
        None
    } else {
        Some(auth.token)
    }
}

fn read_windows_generic_credential(target: &str) -> Option<String> {
    const CRED_TYPE_GENERIC: u32 = 1;

    let mut target_wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let mut credential: *mut CredentialW = std::ptr::null_mut();

    let ok = unsafe {
        CredReadW(
            target_wide.as_mut_ptr(),
            CRED_TYPE_GENERIC,
            0,
            &mut credential,
        )
    };

    if ok == 0 || credential.is_null() {
        diagnose::log(format!(
            "unable to read Windows generic credential target {target}"
        ));
        return None;
    }

    let result = unsafe {
        let cred = &*credential;
        if cred.credential_blob_size == 0 || cred.credential_blob.is_null() {
            CredFree(credential as *mut c_void);
            return None;
        }
        let bytes =
            std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize);
        let text = String::from_utf8(bytes.to_vec()).ok();
        CredFree(credential as *mut c_void);
        text
    };

    result
}

fn read_wsl_credentials(distro: &str) -> Option<Credentials> {
    let output = run_with_timeout(
        Command::new("wsl.exe")
            .arg("-d")
            .arg(distro)
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg("cat ~/.claude/.credentials.json")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    )?;

    if !output.status.success() {
        diagnose::log(format!(
            "WSL credentials probe failed for distro {distro} with status {}",
            output.status
        ));
        return None;
    }

    let content = String::from_utf8(output.stdout).ok()?;
    parse_credentials(
        &content,
        CredentialSource::Wsl {
            distro: distro.to_string(),
        },
    )
}

fn parse_credentials(content: &str, source: CredentialSource) -> Option<Credentials> {
    let json: serde_json::Value = serde_json::from_str(content).ok()?;

    let oauth = json.get("claudeAiOauth")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())?
        .to_string();
    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_i64());

    Some(Credentials {
        access_token,
        expires_at,
        source,
    })
}

fn read_next_credentials_after(source: &CredentialSource) -> Option<Credentials> {
    match source {
        CredentialSource::Windows(_) => {
            for distro in list_wsl_distros() {
                if let Some(creds) = read_wsl_credentials(&distro) {
                    return Some(creds);
                }
            }
        }
        CredentialSource::Wsl { distro } => {
            let mut past_current = false;
            for candidate_distro in list_wsl_distros() {
                if !past_current {
                    past_current = candidate_distro == *distro;
                    continue;
                }
                if let Some(creds) = read_wsl_credentials(&candidate_distro) {
                    return Some(creds);
                }
            }
        }
    }

    None
}

fn list_wsl_distros() -> Vec<String> {
    let output = match run_with_timeout(
        Command::new("wsl.exe")
            .args(["-l", "-q"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    ) {
        Some(output) if output.status.success() => output,
        _ => {
            diagnose::log("unable to enumerate WSL distros");
            return Vec::new();
        }
    };

    let stdout = decode_wsl_text(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn decode_wsl_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    if let Some(decoded) = decode_utf16le(bytes) {
        return decoded;
    }

    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 || bytes.len() % 2 != 0 {
        return None;
    }

    let body = if bytes.starts_with(&[0xFF, 0xFE]) {
        &bytes[2..]
    } else if looks_like_utf16le(bytes) {
        bytes
    } else {
        return None;
    };

    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    Some(String::from_utf16_lossy(&units))
}

fn looks_like_utf16le(bytes: &[u8]) -> bool {
    let sample_len = bytes.len().min(128);
    let units = sample_len / 2;
    if units == 0 {
        return false;
    }

    let nul_high_bytes = bytes[..sample_len]
        .chunks_exact(2)
        .filter(|chunk| chunk[1] == 0)
        .count();

    nul_high_bytes * 2 >= units
}

fn is_token_expired(expires_at: Option<i64>) -> bool {
    let Some(exp) = expires_at else { return false };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now >= exp
}

/// Parse an ISO 8601 timestamp string into a SystemTime.
fn parse_iso8601(s: Option<&str>) -> Option<SystemTime> {
    let s = s?;
    // Strip timezone offset to get "YYYY-MM-DDTHH:MM:SS" or with fractional seconds
    // The API returns formats like "2026-03-05T08:00:00.321598+00:00"
    let datetime_part = s.split('+').next().unwrap_or(s);
    let datetime_part = datetime_part.split('Z').next().unwrap_or(datetime_part);

    // Try parsing with and without fractional seconds
    let formats = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    for fmt in &formats {
        if let Ok(secs) = parse_datetime_to_unix(datetime_part, fmt) {
            return Some(UNIX_EPOCH + Duration::from_secs(secs));
        }
    }
    None
}

/// Minimal datetime parser — avoids pulling in chrono/time crates.
fn parse_datetime_to_unix(s: &str, _fmt: &str) -> Result<u64, ()> {
    // Extract date and time parts from "YYYY-MM-DDTHH:MM:SS[.frac]"
    let (date_str, time_str) = s.split_once('T').ok_or(())?;
    let date_parts: Vec<&str> = date_str.split('-').collect();
    if date_parts.len() != 3 {
        return Err(());
    }

    let year: u64 = date_parts[0].parse().map_err(|_| ())?;
    let month: u64 = date_parts[1].parse().map_err(|_| ())?;
    let day: u64 = date_parts[2].parse().map_err(|_| ())?;

    // Strip fractional seconds
    let time_base = time_str.split('.').next().unwrap_or(time_str);
    let time_parts: Vec<&str> = time_base.split(':').collect();
    if time_parts.len() != 3 {
        return Err(());
    }

    let hour: u64 = time_parts[0].parse().map_err(|_| ())?;
    let min: u64 = time_parts[1].parse().map_err(|_| ())?;
    let sec: u64 = time_parts[2].parse().map_err(|_| ())?;

    // Days from year (using a simplified calculation for dates after 1970)
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }

    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[m as usize];
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += day - 1;

    Ok(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Codex leaves absent windows as default sections. Its reported windows
/// include a reset timestamp, including windows with zero usage.
pub fn format_codex_line(
    section: &UsageSection,
    strings: Strings,
    show_remaining_in_chinese: bool,
    window: UsageWindowKind,
) -> String {
    if section.percentage == 0.0 && section.resets_at.is_none() {
        return "--".to_string();
    }
    format_line(section, strings, show_remaining_in_chinese, window)
}

/// Format a usage section for the compact taskbar display.
pub fn format_line(
    section: &UsageSection,
    strings: Strings,
    show_remaining_in_chinese: bool,
    window: UsageWindowKind,
) -> String {
    if show_remaining_in_chinese {
        return format_simplified_chinese_line(section, window);
    }

    let pct = format!("{:.0}%", section.percentage);
    let cd = format_countdown(section.resets_at, strings);
    if cd.is_empty() {
        pct
    } else {
        format!("{pct} \u{00b7} {cd}")
    }
}

fn format_simplified_chinese_line(section: &UsageSection, window: UsageWindowKind) -> String {
    let remaining = remaining_percentage(section.percentage);
    let reset = section
        .resets_at
        .and_then(native_interop::system_time_to_local);
    format_simplified_chinese_values(remaining, reset, window)
}

fn format_simplified_chinese_values(
    remaining: f64,
    reset: Option<windows::Win32::Foundation::SYSTEMTIME>,
    window: UsageWindowKind,
) -> String {
    let Some(reset) = reset else {
        return format!("{remaining:.0}%");
    };
    match window {
        UsageWindowKind::Session => {
            format!(
                "{remaining:.0}%  {:02}:{:02}重置",
                reset.wHour, reset.wMinute
            )
        }
        UsageWindowKind::Weekly => {
            format!("{remaining:.0}%  {:02}/{:02}重置", reset.wMonth, reset.wDay)
        }
    }
}

pub fn remaining_percentage(used_percentage: f64) -> f64 {
    (100.0 - used_percentage).clamp(0.0, 100.0)
}

fn format_countdown(resets_at: Option<SystemTime>, strings: Strings) -> String {
    let reset = match resets_at {
        Some(t) => t,
        None => return String::new(),
    };

    let remaining = match reset.duration_since(SystemTime::now()) {
        Ok(d) => d,
        Err(_) => return strings.now.to_string(),
    };

    format_countdown_from_secs(remaining.as_secs(), strings)
}

/// Calculate how long until the display text would change
pub fn time_until_display_change(resets_at: Option<SystemTime>) -> Option<Duration> {
    let reset = resets_at?;
    let remaining = reset.duration_since(SystemTime::now()).ok()?;
    Some(time_until_display_change_from_secs(remaining.as_secs()))
}

fn format_countdown_from_secs(total_secs: u64, strings: Strings) -> String {
    let total_mins = total_secs / 60;
    let total_hours = total_secs / 3600;
    let total_days = total_secs / 86400;

    if total_days >= 1 {
        format!("{total_days}{}", strings.day_suffix)
    } else if total_hours >= 1 {
        format!("{total_hours}{}", strings.hour_suffix)
    } else if total_mins >= 1 {
        format!("{total_mins}{}", strings.minute_suffix)
    } else {
        format!("{total_secs}{}", strings.second_suffix)
    }
}

fn time_until_display_change_from_secs(total_secs: u64) -> Duration {
    let total_mins = total_secs / 60;
    let total_hours = total_secs / 3600;
    let total_days = total_secs / 86400;

    let current_bucket_start = if total_days >= 1 {
        total_days * 86400
    } else if total_hours >= 1 {
        total_hours * 3600
    } else if total_mins >= 1 {
        total_mins * 60
    } else {
        total_secs
    };

    Duration::from_secs(total_secs.saturating_sub(current_bucket_start) + 1)
}

// ---- GitHub Copilot provider (ported from CodeZeno src/poller/copilot.rs).
// The quota endpoint the Copilot editors and CLI read for their own usage
// panels. A GitHub OAuth token from any Copilot-enabled login can read it.
const COPILOT_USER_URL: &str = "https://api.github.com/copilot_internal/user";
// The precedence the Copilot CLI itself gives its token variables.
const COPILOT_TOKEN_ENVIRONMENT: [&str; 3] = ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
// Newer Copilot CLI builds store `https://github.com:<login>.copilot-cli`,
// older ones `copilot-cli/https://github.com:<login>`.
const COPILOT_CLI_TARGET_PREFIX: &str = "https://github.com:";
const COPILOT_CLI_TARGET_SUFFIX: &str = ".copilot-cli";
const LEGACY_COPILOT_CLI_TARGET_PREFIX: &str = "copilot-cli/https://github.com:";
// The GitHub CLI keeps the active account under `gh:github.com:` and every
// signed-in account under `gh:github.com:<login>`.
const GH_CLI_TARGET_PREFIX: &str = "gh:github.com:";
const GH_CLI_ACTIVE_TARGET: &str = "gh:github.com:";
const PREMIUM_QUOTA: &str = "premium_interactions";

#[derive(Debug, Deserialize)]
struct CopilotUserResponse {
    #[serde(default)]
    copilot_plan: Option<String>,
    #[serde(default)]
    quota_reset_date_utc: Option<String>,
    // Date-only fallback, e.g. `2026-11-01`.
    #[serde(default)]
    quota_reset_date: Option<String>,
    // Kept as raw values so one unfamiliar snapshot cannot fail the rest.
    #[serde(default)]
    quota_snapshots: BTreeMap<String, serde_json::Value>,
    // Copilot Free accounts without snapshots report what is left of each
    // monthly allowance here, against the totals in `monthly_quotas`.
    #[serde(default)]
    limited_user_quotas: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    monthly_quotas: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    limited_user_reset_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CopilotQuotaSnapshot {
    #[serde(default)]
    entitlement: Option<f64>,
    #[serde(default)]
    remaining: Option<f64>,
    #[serde(default)]
    percent_remaining: Option<f64>,
    #[serde(default)]
    unlimited: bool,
}

impl CopilotQuotaSnapshot {
    fn used_percentage(&self) -> Option<f64> {
        if self.unlimited {
            return Some(0.0);
        }
        if let Some(remaining) = self.percent_remaining.filter(|value| value.is_finite()) {
            return Some((100.0 - remaining).clamp(0.0, 100.0));
        }
        copilot_used_from_remaining(self.entitlement?, self.remaining?)
    }
}

struct CopilotTokenCandidate {
    source: String,
    token: String,
}

struct CopilotStoredCredential {
    target: String,
    secret: String,
    last_written: u64,
}

fn poll_copilot() -> Result<UsageData, PollError> {
    let candidates = copilot_token_candidates();
    if candidates.is_empty() {
        diagnose::log(
            "Copilot usage poll failed: no GitHub login found (sign in with 'copilot' or 'gh auth login', or set GH_TOKEN)",
        );
        return Err(PollError::NoCredentials);
    }

    let mut last_error = PollError::AuthRequired;
    for candidate in &candidates {
        match fetch_copilot_usage(&candidate.token, COPILOT_USER_URL) {
            Ok(usage) => return Ok(usage),
            // Another stored login may still hold a Copilot seat.
            Err(PollError::AuthRequired) => {
                diagnose::log(format!(
                    "Copilot rejected the login from {}",
                    candidate.source
                ));
                last_error = PollError::AuthRequired;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error)
}

fn copilot_token_candidates() -> Vec<CopilotTokenCandidate> {
    let environment = COPILOT_TOKEN_ENVIRONMENT.iter().filter_map(|name| {
        opencode_non_empty_env(name).map(|token| CopilotTokenCandidate {
            source: format!("environment variable {name}"),
            token,
        })
    });
    let stored = copilot_cli_credentials()
        .into_iter()
        .chain(gh_cli_credentials())
        .map(|credential| CopilotTokenCandidate {
            source: format!("Windows credential {}", credential.target),
            token: credential.secret.trim().to_string(),
        });

    let mut candidates: Vec<CopilotTokenCandidate> = Vec::new();
    for candidate in environment.chain(stored) {
        if copilot_is_header_safe_token(&candidate.token)
            && !candidates
                .iter()
                .any(|existing| existing.token == candidate.token)
        {
            candidates.push(candidate);
        }
    }
    candidates
}

fn copilot_cli_credentials() -> Vec<CopilotStoredCredential> {
    let mut credentials = enumerate_windows_generic_credentials(COPILOT_CLI_TARGET_PREFIX)
        .into_iter()
        .filter(|credential| credential.target.ends_with(COPILOT_CLI_TARGET_SUFFIX))
        .chain(enumerate_windows_generic_credentials(
            LEGACY_COPILOT_CLI_TARGET_PREFIX,
        ))
        .collect::<Vec<_>>();
    credentials.sort_by_key(|credential| std::cmp::Reverse(credential.last_written));
    credentials
}

// The active account first, then the most recently written.
fn gh_cli_credentials() -> Vec<CopilotStoredCredential> {
    let mut credentials = enumerate_windows_generic_credentials(GH_CLI_TARGET_PREFIX);
    credentials.sort_by_key(|credential| {
        (
            credential.target != GH_CLI_ACTIVE_TARGET,
            std::cmp::Reverse(credential.last_written),
        )
    });
    credentials
}

fn enumerate_windows_generic_credentials(prefix: &str) -> Vec<CopilotStoredCredential> {
    let mut out = Vec::new();
    let filter_text = format!("{prefix}*");
    let filter_wide: Vec<u16> = filter_text
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut count: u32 = 0;
    let mut credentials: *mut *mut CredentialW = std::ptr::null_mut();
    let ok = unsafe { CredEnumerateW(filter_wide.as_ptr(), 0, &mut count, &mut credentials) };
    if ok == 0 || credentials.is_null() {
        return out;
    }
    unsafe {
        for index in 0..count as usize {
            let credential = *credentials.add(index);
            if credential.is_null() {
                continue;
            }
            let entry = &*credential;
            if entry.target_name.is_null()
                || entry.credential_blob.is_null()
                || entry.credential_blob_size == 0
            {
                continue;
            }
            let mut target_len = 0;
            while *entry.target_name.add(target_len) != 0 {
                target_len += 1;
            }
            let target =
                String::from_utf16_lossy(std::slice::from_raw_parts(entry.target_name, target_len));
            let bytes = std::slice::from_raw_parts(
                entry.credential_blob,
                entry.credential_blob_size as usize,
            );
            let secret = match String::from_utf8(bytes.to_vec()) {
                Ok(secret) => secret,
                Err(_) => continue,
            };
            out.push(CopilotStoredCredential {
                target,
                secret,
                last_written: entry.last_written,
            });
        }
        CredFree(credentials as *mut c_void);
    }
    out
}

fn fetch_copilot_usage(token: &str, url: &str) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let response = match agent
        .get(url)
        .set("Authorization", &format!("token {token}"))
        .set("Accept", "application/json")
        .set(
            "User-Agent",
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        )
        .call()
    {
        Ok(response) => response,
        Err(error) => {
            // Deviation from upstream (which continues the candidate loop on
            // 404 = valid login without a seat): our classifier has no
            // per-status variant past AuthRequired, so 404 surfaces as
            // RequestFailed and stops the loop. Single-login behavior is
            // identical in outcome (an error); revisit if multi-login
            // seat-fallback needs it.
            if let ureq::Error::Status(404, _) = &error {
                diagnose::log("Copilot login has no Copilot access (404)");
            }
            return Err(classify_ureq_error(&error));
        }
    };

    let response: CopilotUserResponse = response.into_json().map_err(|error| {
        diagnose::log(format!("unable to parse Copilot usage response: {error:?}"));
        PollError::RequestFailed
    })?;
    if let Some(plan) = response.copilot_plan.as_deref() {
        diagnose::log(format!("Copilot plan: {plan}"));
    }
    copilot_usage_from_user(response).ok_or_else(|| {
        diagnose::log("Copilot usage response missing quota snapshots");
        PollError::RequestFailed
    })
}

fn copilot_usage_from_user(response: CopilotUserResponse) -> Option<UsageData> {
    let snapshots = response
        .quota_snapshots
        .into_iter()
        .filter_map(|(key, value)| {
            Some((
                key,
                serde_json::from_value::<CopilotQuotaSnapshot>(value).ok()?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let reset = parse_iso8601(response.quota_reset_date_utc.as_deref())
        .or_else(|| copilot_parse_reset_date(response.quota_reset_date.as_deref()));

    let premium = snapshots
        .get(PREMIUM_QUOTA)
        .and_then(CopilotQuotaSnapshot::used_percentage);
    let (pool, resets_at) = match premium {
        Some(premium) => (premium, reset),
        None => {
            // Older Copilot Free responses carry no snapshots at all; chat is
            // the allowance those accounts run out of.
            let reset = reset
                .or_else(|| copilot_parse_reset_date(response.limited_user_reset_date.as_deref()));
            let free =
                copilot_free_quota_usage(&response.limited_user_quotas, &response.monthly_quotas);
            (*free.get("chat")?, reset)
        }
    };

    // Copilot spends one pool per calendar month and has no short window, so
    // the pool lands in `weekly`, which is what tray badges read. The
    // monthly window and the "30d" label need P5 model fields
    // (weekly_label/monthly/limits are not on UsageData yet).
    Some(UsageData {
        session: UsageSection::default(),
        weekly: UsageSection {
            percentage: pool,
            resets_at,
        },
    })
}

fn copilot_free_quota_usage(
    remaining: &BTreeMap<String, serde_json::Value>,
    totals: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, f64> {
    totals
        .iter()
        .filter_map(|(key, total)| {
            let used = copilot_used_from_remaining(total.as_f64()?, remaining.get(key)?.as_f64()?)?;
            Some((key.clone(), used))
        })
        .collect()
}

fn copilot_used_from_remaining(entitlement: f64, remaining: f64) -> Option<f64> {
    if !entitlement.is_finite() || entitlement <= 0.0 || !remaining.is_finite() {
        return None;
    }
    Some(((entitlement - remaining) / entitlement * 100.0).clamp(0.0, 100.0))
}

// Copilot quotas reset at midnight UTC on the given day.
fn copilot_parse_reset_date(date: Option<&str>) -> Option<SystemTime> {
    let date = date?.trim();
    parse_iso8601(Some(&format!("{date}T00:00:00Z")))
}

// Tokens go into a header, so anything that could split it is unusable.
fn copilot_is_header_safe_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copilot_usage(json: serde_json::Value) -> Option<UsageData> {
        copilot_usage_from_user(serde_json::from_value(json).unwrap())
    }

    #[test]
    fn copilot_enterprise_response_maps_premium_requests_to_the_monthly_pool() {
        // Shape captured from api.github.com on an Enterprise seat (upstream
        // fixture); identifiers and organisation details dropped.
        let data = copilot_usage(serde_json::json!({
            "access_type_sku": "copilot_enterprise_seat_quota",
            "copilot_plan": "enterprise",
            "quota_reset_date": "2026-11-01",
            "quota_reset_date_utc": "2026-11-01T00:00:00.000Z",
            "token_based_billing": true,
            "quota_snapshots": {
                "chat": {
                    "entitlement": 0, "remaining": 0, "percent_remaining": 100.0,
                    "quota_id": "chat", "unlimited": true, "overage_permitted": false
                },
                "completions": {
                    "entitlement": 0, "remaining": 0, "percent_remaining": 100.0,
                    "quota_id": "completions", "unlimited": true
                },
                "premium_interactions": {
                    "entitlement": 30000, "remaining": 22500, "percent_remaining": 75.0,
                    "quota_id": "premium_interactions", "unlimited": false,
                    "overage_permitted": true, "credits_used": 0
                }
            }
        }))
        .unwrap();

        assert_eq!(data.weekly.percentage, 25.0);
        assert_eq!(
            data.weekly.resets_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_793_491_200))
        );
        // Copilot has no short window, so the session slot stays default.
        assert_eq!(data.session.percentage, 0.0);
        assert!(data.session.resets_at.is_none());
    }

    #[test]
    fn copilot_metered_snapshots_feed_only_the_pool_without_model_fields() {
        // Upstream exposes metered snapshots as opt-in limits; our UsageData
        // has no limits field yet (P5), so only the pool is asserted here.
        let data = copilot_usage(serde_json::json!({
            "copilot_plan": "free",
            "quota_reset_date_utc": "2026-11-01T00:00:00.000Z",
            "quota_snapshots": {
                "chat": {"entitlement": 50, "remaining": 10, "percent_remaining": 20.0},
                "completions": {"entitlement": 2000, "remaining": 2000, "percent_remaining": 100.0},
                "premium_interactions": {"entitlement": 50, "remaining": 45, "percent_remaining": 90.0}
            }
        }))
        .unwrap();
        assert_eq!(data.weekly.percentage, 10.0);
        assert_eq!(
            data.weekly.resets_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_793_491_200))
        );
    }

    #[test]
    fn copilot_overage_and_missing_percentages_fall_back_to_counts() {
        let data = copilot_usage(serde_json::json!({
            "quota_reset_date": "2026-11-01",
            "quota_snapshots": {
                "premium_interactions": {"entitlement": 300, "remaining": -12}
            }
        }))
        .unwrap();
        assert_eq!(data.weekly.percentage, 100.0);
        // The date-only field still yields a reset time.
        assert_eq!(
            data.weekly.resets_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_793_491_200))
        );

        let data = copilot_usage(serde_json::json!({
            "quota_snapshots": {
                "premium_interactions": {"entitlement": 300, "remaining": 225}
            }
        }))
        .unwrap();
        assert_eq!(data.weekly.percentage, 25.0);
        assert!(data.weekly.resets_at.is_none());
    }

    #[test]
    fn copilot_legacy_free_response_reads_the_chat_allowance() {
        let data = copilot_usage(serde_json::json!({
            "copilot_plan": "individual",
            "limited_user_reset_date": "2026-10-15",
            "limited_user_quotas": {"chat": 40, "completions": 1500},
            "monthly_quotas": {"chat": 50, "completions": 2000}
        }))
        .unwrap();
        assert_eq!(data.weekly.percentage, 20.0);
        assert!(data.weekly.resets_at.is_some());
    }

    #[test]
    fn copilot_responses_without_a_readable_pool_are_rejected() {
        assert!(copilot_usage(serde_json::json!({})).is_none());
        assert!(copilot_usage(serde_json::json!({
            "quota_snapshots": {
                "premium_interactions": "unexpected",
                "chat": {"unlimited": true}
            }
        }))
        .is_none());
        assert!(copilot_usage(serde_json::json!({
            "quota_snapshots": {"premium_interactions": {"entitlement": 0, "remaining": 0}}
        }))
        .is_none());
        assert!(copilot_usage(serde_json::json!({
            "limited_user_quotas": {"chat": 1},
            "monthly_quotas": {"chat": 0}
        }))
        .is_none());
    }

    #[test]
    fn copilot_unlimited_premium_pool_reads_as_idle() {
        let data = copilot_usage(serde_json::json!({
            "quota_snapshots": {"premium_interactions": {"unlimited": true}}
        }))
        .unwrap();
        assert_eq!(data.weekly.percentage, 0.0);
    }

    #[test]
    fn copilot_tokens_that_could_split_a_header_are_refused() {
        assert!(copilot_is_header_safe_token("gho_abc123"));
        assert!(!copilot_is_header_safe_token(""));
        assert!(!copilot_is_header_safe_token("gho_abc\r\nInjected: yes"));
        assert!(!copilot_is_header_safe_token("two words"));
    }

    static COPILOT_TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn copilot_environment_candidates_keep_cli_precedence_order() {
        let _guard = COPILOT_TEST_ENV_LOCK.lock().unwrap();
        let saved: Vec<(String, Option<String>)> = COPILOT_TOKEN_ENVIRONMENT
            .iter()
            .map(|name| (name.to_string(), std::env::var(name).ok()))
            .collect();
        std::env::set_var("COPILOT_GITHUB_TOKEN", "env-first-token");
        std::env::set_var("GH_TOKEN", "env-second-token");
        std::env::set_var("GITHUB_TOKEN", "env-third-token");
        let candidates = copilot_token_candidates();
        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
        // Environment candidates always come first in CLI precedence order,
        // ahead of any stored Windows credentials on the machine.
        assert!(candidates.len() >= 3);
        assert_eq!(candidates[0].token, "env-first-token");
        assert_eq!(candidates[1].token, "env-second-token");
        assert_eq!(candidates[2].token, "env-third-token");
    }

    fn mock_copilot_user_request(
        status: u16,
        body: &str,
    ) -> (Result<UsageData, PollError>, String) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/copilot_internal/user",
            listener.local_addr().unwrap()
        );
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        let result = fetch_copilot_usage("test-token", &url);
        (result, server.join().unwrap())
    }

    #[test]
    fn copilot_request_sends_token_auth_and_maps_premium_pool() {
        let body = serde_json::json!({
            "copilot_plan": "business",
            "quota_reset_date_utc": "2026-11-01T00:00:00.000Z",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 300, "remaining": 225, "percent_remaining": 75.0
                }
            }
        })
        .to_string();
        let (result, request) = mock_copilot_user_request(200, &body);
        assert!(request.contains("GET /copilot_internal/user"));
        assert!(request.contains("Authorization: token test-token"));
        assert!(request.contains("Accept: application/json"));
        assert!(request.contains(&format!("User-Agent: {}/", env!("CARGO_PKG_NAME"))));
        let data = result.unwrap();
        assert_eq!(data.weekly.percentage, 25.0);
        assert_eq!(
            data.weekly.resets_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_793_491_200))
        );
    }

    #[test]
    fn copilot_request_auth_and_status_mapping() {
        let body = serde_json::json!({
            "quota_snapshots": {
                "premium_interactions": {"entitlement": 100, "remaining": 50}
            }
        })
        .to_string();
        let (result, _) = mock_copilot_user_request(401, &body);
        assert!(matches!(result, Err(PollError::AuthRequired)));
        let (result, _) = mock_copilot_user_request(
            404,
            &serde_json::json!({"message": "Not Found"}).to_string(),
        );
        // Documented deviation: 404 (valid login, no seat) surfaces as
        // RequestFailed instead of continuing the candidate loop.
        assert!(matches!(result, Err(PollError::RequestFailed)));
        let (result, _) = mock_copilot_user_request(500, &body);
        assert!(matches!(result, Err(PollError::ServerError)));
        let (result, _) = mock_copilot_user_request(200, "not json");
        assert!(matches!(result, Err(PollError::RequestFailed)));
        let (result, _) = mock_copilot_user_request(200, "{}");
        assert!(matches!(result, Err(PollError::RequestFailed)));
    }

    const OPENCODE_GO_STATUS_FIXTURE: &str = r#"{
        "subscriberUserID": "usr_example",
        "useBalance": false,
        "access": {
            "startsAt": "2026-09-01T00:00:00.000Z",
            "endsAt": "2026-10-01T00:00:00.000Z",
            "meters": {
                "fiveHour": {
                    "limitMicroCents": "2000000000",
                    "usedMicroCents": "250000000",
                    "resetsAt": "2026-09-17T05:00:00.000Z"
                },
                "week": {
                    "limitMicroCents": "10000000000",
                    "usedMicroCents": "4500000000",
                    "resetsAt": "2026-09-21T00:00:00.000Z"
                },
                "month": {
                    "limitMicroCents": "20000000000",
                    "usedMicroCents": "12000000000"
                }
            }
        }
    }"#;

    fn mock_opencode_status_request(
        status: u16,
        body: &str,
        cookie: &str,
    ) -> (Result<OpenCodeDashboardUsage, PollError>, String) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/console/api/go/status",
            listener.local_addr().unwrap()
        );
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        let result = fetch_opencode_status(
            &OpenCodeCredentials {
                workspace_id: "wrk_example".into(),
                auth_cookie: cookie.into(),
                source: "test".into(),
            },
            &url,
        );
        (result, server.join().unwrap())
    }

    #[test]
    fn opencode_status_maps_meter_percentages_and_absolute_resets() {
        let usage =
            usage_from_opencode_status(serde_json::from_str(OPENCODE_GO_STATUS_FIXTURE).unwrap())
                .unwrap();
        let rolling = usage.rolling.as_ref().unwrap();
        assert_eq!(rolling.usage_percent, 12.5);
        assert_eq!(
            rolling.resets_at,
            parse_iso8601(Some("2026-09-17T05:00:00Z"))
        );
        let weekly = usage.weekly.as_ref().unwrap();
        assert_eq!(weekly.usage_percent, 45.0);
        assert_eq!(
            weekly.resets_at,
            parse_iso8601(Some("2026-09-21T00:00:00Z"))
        );
        let monthly = usage.monthly.as_ref().unwrap();
        assert_eq!(monthly.usage_percent, 60.0);
        // The monthly window inherits the subscription end as its reset.
        assert_eq!(
            monthly.resets_at,
            parse_iso8601(Some("2026-10-01T00:00:00Z"))
        );
    }

    #[test]
    fn opencode_status_preserves_idle_rolling_window_without_reset() {
        let json = OPENCODE_GO_STATUS_FIXTURE
            .replace("\"250000000\"", "\"0\"")
            .replace("\"2026-09-17T05:00:00.000Z\"", "null");
        let usage = usage_from_opencode_status(serde_json::from_str(&json).unwrap()).unwrap();
        let section = opencode_section_from_window(usage.rolling.as_ref().unwrap());
        assert_eq!(section.percentage, 0.0);
        assert_eq!(section.resets_at, None);
    }

    #[test]
    fn opencode_status_rejects_missing_access_and_invalid_meter_values() {
        for json in ["null", r#"{"access":null}"#] {
            assert_eq!(
                usage_from_opencode_status(serde_json::from_str(json).unwrap()),
                Err(PollError::RequestFailed)
            );
        }
        for json in [
            OPENCODE_GO_STATUS_FIXTURE.replace("250000000", "-1"),
            OPENCODE_GO_STATUS_FIXTURE.replace("250000000", "NaN"),
            OPENCODE_GO_STATUS_FIXTURE.replace("250000000", "1.5"),
            OPENCODE_GO_STATUS_FIXTURE.replace("2026-09-17T05:00:00.000Z", "invalid-date"),
        ] {
            assert_eq!(
                usage_from_opencode_status(serde_json::from_str(&json).unwrap()),
                Err(PollError::RequestFailed)
            );
        }
    }

    #[test]
    fn opencode_meter_handles_zero_limits_and_clamps_overages() {
        let meter = |used: &str, limit: &str| OpenCodeGoMeter {
            used_micro_cents: used.into(),
            limit_micro_cents: limit.into(),
        };
        assert_eq!(
            window_from_opencode_meter(&meter("0", "0"), None)
                .unwrap()
                .usage_percent,
            0.0
        );
        let window = window_from_opencode_meter(&meter("120", "100"), None).unwrap();
        assert_eq!(opencode_section_from_window(&window).percentage, 100.0);
    }

    #[test]
    fn opencode_selects_most_constrained_long_window_with_label() {
        let usage = OpenCodeDashboardUsage {
            weekly: Some(OpenCodeUsageWindow {
                usage_percent: 40.0,
                resets_at: None,
            }),
            monthly: Some(OpenCodeUsageWindow {
                usage_percent: 70.0,
                resets_at: None,
            }),
            ..Default::default()
        };
        let (section, label) = select_opencode_long_window(&usage);
        assert_eq!(section.percentage, 70.0);
        assert_eq!(label.as_deref(), Some("30d"));

        let usage = OpenCodeDashboardUsage {
            weekly: Some(OpenCodeUsageWindow {
                usage_percent: 80.0,
                resets_at: None,
            }),
            monthly: Some(OpenCodeUsageWindow {
                usage_percent: 20.0,
                resets_at: None,
            }),
            ..Default::default()
        };
        let (section, label) = select_opencode_long_window(&usage);
        assert_eq!(section.percentage, 80.0);
        assert_eq!(label.as_deref(), Some("7d"));

        let (section, label) = select_opencode_long_window(&OpenCodeDashboardUsage::default());
        assert_eq!(section.percentage, 0.0);
        assert_eq!(label, None);
    }

    #[test]
    fn opencode_request_sends_workspace_context_and_normalizes_auth_cookie() {
        for (cookie, expected) in [
            ("test-token", "auth=test-token"),
            ("test-token==", "auth=test-token=="),
            ("auth=test-token; theme=dark", "auth=test-token; theme=dark"),
            (
                "__Host-console_session=SessionToken==",
                "__Host-console_session=SessionToken==",
            ),
            (
                "theme=dark; __Host-console_session=SessionToken",
                "theme=dark; __Host-console_session=SessionToken",
            ),
        ] {
            let (result, request) =
                mock_opencode_status_request(200, OPENCODE_GO_STATUS_FIXTURE, cookie);
            assert_eq!(result.unwrap().rolling.unwrap().usage_percent, 12.5);
            let sent_cookie = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("cookie")
                        .then_some(value.trim_start())
                })
                .unwrap();
            assert_eq!(sent_cookie, expected);
            let request = request.to_ascii_lowercase();
            assert!(request.starts_with("get /console/api/go/status http/1.1\r\n"));
            assert!(request.contains("\r\nx-org-id: wrk_example\r\n"));
            assert!(request.contains("\r\naccept: application/json\r\n"));
        }
    }

    #[test]
    fn opencode_request_distinguishes_auth_errors_from_bad_responses() {
        for status in [401, 403] {
            assert_eq!(
                mock_opencode_status_request(status, "{}", "test-token").0,
                Err(PollError::AuthRequired)
            );
        }
        for (status, body) in [
            (500, "{}"),
            (200, "<html><div id=app></div></html>"),
            (200, "{invalid"),
            (200, "{}"),
            (200, "null"),
            (200, r#"{"access":{"meters":{}}}"#),
        ] {
            assert_eq!(
                mock_opencode_status_request(status, body, "test-token").0,
                Err(PollError::RequestFailed)
            );
        }
    }

    #[test]
    fn opencode_config_file_parses_aliases_and_rejects_invalid_values() {
        let path =
            std::env::temp_dir().join(format!("opencode-go-config-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"workspaceID": "wrk_01-test", "cookie": "auth=abc; theme=dark"}"#,
        )
        .unwrap();
        let credentials = read_opencode_config(&path).unwrap();
        assert_eq!(credentials.workspace_id, "wrk_01-test");
        assert_eq!(credentials.auth_cookie, "auth=abc; theme=dark");
        std::fs::remove_file(&path).unwrap();

        assert!(!valid_opencode_workspace_id("../other"));
        assert!(!valid_opencode_workspace_id(""));
        assert!(valid_opencode_workspace_id("wrk_01-test"));
        assert!(!valid_opencode_cookie("auth=abc\r\nX-Test: injected"));
        assert!(!valid_opencode_cookie(""));
    }

    const CURSOR_USAGE_SUMMARY_FIXTURE: &str = r#"{
        "billingCycleEnd": "2026-08-25T19:27:24.000Z",
        "individualUsage": {
            "plan": {
                "autoPercentUsed": 12.5,
                "apiPercentUsed": 3.0,
                "totalPercentUsed": 10.0
            }
        }
    }"#;
    const CURSOR_TEST_JWT: &str = "header.eyJzdWIiOiJhdXRoMHx1c2VyXzEyMyJ9.signature";

    fn mock_cursor_usage_request(
        status: u16,
        body: &str,
        cookie: &str,
    ) -> (Result<UsageData, PollError>, String) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/api/usage-summary",
            listener.local_addr().unwrap()
        );
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        let result = fetch_cursor_usage(cookie, &url);
        (result, server.join().unwrap())
    }

    fn create_cursor_state_fixture(token: &str) -> PathBuf {
        extern "C" {
            fn sqlite3_open_v2(
                filename: *const c_char,
                pp_db: *mut *mut c_void,
                flags: c_int,
                z_vfs: *const c_char,
            ) -> c_int;
            fn sqlite3_exec(
                db: *mut c_void,
                sql: *const c_char,
                callback: *const c_void,
                arg: *mut c_void,
                errmsg: *mut *mut c_char,
            ) -> c_int;
            fn sqlite3_close_v2(db: *mut c_void) -> c_int;
        }
        const SQLITE_OPEN_READWRITE: c_int = 0x00000002;
        const SQLITE_OPEN_CREATE: c_int = 0x00000004;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cursor-state-{}-{nanos}.vscdb", std::process::id()));
        let setup = format!(
            "CREATE TABLE ItemTable(key TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO ItemTable(key, value) VALUES ('{CURSOR_ACCESS_TOKEN_KEY}', '{token}');"
        );
        unsafe {
            let path_c = CString::new(path.to_str().unwrap()).unwrap();
            let setup_c = CString::new(setup).unwrap();
            let mut db: *mut c_void = std::ptr::null_mut();
            assert_eq!(
                sqlite3_open_v2(
                    path_c.as_ptr(),
                    &mut db,
                    SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                    std::ptr::null(),
                ),
                0
            );
            assert_eq!(
                sqlite3_exec(
                    db,
                    setup_c.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                0
            );
            sqlite3_close_v2(db);
        }
        path
    }

    #[test]
    fn cursor_extracts_user_id_from_jwt_access_token() {
        assert_eq!(
            extract_cursor_user_id(CURSOR_TEST_JWT).as_deref(),
            Some("user_123")
        );
        assert_eq!(
            cursor_cookie_from_access_token(CURSOR_TEST_JWT).as_deref(),
            Some("user_123%3A%3Aheader.eyJzdWIiOiJhdXRoMHx1c2VyXzEyMyJ9.signature")
        );
        assert!(extract_cursor_user_id("not-a-jwt").is_none());
        assert!(extract_cursor_user_id("a.b").is_none());
    }

    #[test]
    fn cursor_rejects_malformed_base64_and_cookie_header_injection() {
        assert!(base64_url_decode("a").is_none());
        assert!(base64_url_decode("***").is_none());
        assert!(normalize_cursor_session_cookie("value\r\nInjected: yes").is_none());
        assert!(normalize_cursor_session_cookie("   ").is_none());
    }

    #[test]
    fn cursor_normalizes_explicit_session_cookie_forms() {
        assert_eq!(
            normalize_cursor_session_cookie("WorkosCursorSessionToken=abc::def").as_deref(),
            Some("abc%3A%3Adef")
        );
        assert_eq!(
            normalize_cursor_session_cookie("abc::def").as_deref(),
            Some("abc%3A%3Adef")
        );
        assert_eq!(
            normalize_cursor_session_cookie("abc%3A%3Adef").as_deref(),
            Some("abc%3A%3Adef")
        );
        assert_eq!(
            normalize_cursor_session_cookie("plain-token").as_deref(),
            Some("plain-token")
        );
    }

    #[test]
    fn cursor_usage_maps_auto_and_api_percentages_with_shared_reset() {
        let response: CursorUsageSummaryResponse =
            serde_json::from_str(CURSOR_USAGE_SUMMARY_FIXTURE).unwrap();
        let data = cursor_usage_from_summary(response).unwrap();
        assert_eq!(data.session.percentage, 12.5);
        assert_eq!(data.weekly.percentage, 3.0);
        assert!(data.session.resets_at.is_some());
        assert_eq!(data.session.resets_at, data.weekly.resets_at);
    }

    #[test]
    fn cursor_missing_metrics_fall_back_to_total_or_empty_sections() {
        // Our UsageSection has no `available` flag: a missing metric is the
        // default section (renders as "--"), an idle metric is 0%.
        let total_only: CursorUsageSummaryResponse =
            serde_json::from_str(r#"{"individualUsage":{"plan":{"totalPercentUsed":0}}}"#).unwrap();
        let data = cursor_usage_from_summary(total_only).unwrap();
        assert_eq!(data.session.percentage, 0.0);
        assert_eq!(data.weekly.percentage, 0.0);
        assert!(data.weekly.resets_at.is_none());

        let api_only: CursorUsageSummaryResponse =
            serde_json::from_str(r#"{"individualUsage":{"plan":{"apiPercentUsed":42}}}"#).unwrap();
        let data = cursor_usage_from_summary(api_only).unwrap();
        assert_eq!(data.session.percentage, 0.0);
        assert!(data.session.resets_at.is_none());
        assert_eq!(data.weekly.percentage, 42.0);

        let missing: CursorUsageSummaryResponse =
            serde_json::from_str(r#"{"individualUsage":{}}"#).unwrap();
        assert!(cursor_usage_from_summary(missing).is_none());
    }

    #[test]
    fn cursor_request_sends_session_cookie_and_handles_statuses() {
        let (result, request) =
            mock_cursor_usage_request(200, CURSOR_USAGE_SUMMARY_FIXTURE, "abc%3A%3Adef");
        let data = result.unwrap();
        assert_eq!(data.session.percentage, 12.5);
        let sent_cookie = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("cookie")
                    .then_some(value.trim_start())
            })
            .unwrap();
        assert_eq!(sent_cookie, "WorkosCursorSessionToken=abc%3A%3Adef");
        assert!(request
            .to_ascii_lowercase()
            .starts_with("get /api/usage-summary http/1.1\r\n"));

        for status in [401, 403] {
            assert!(matches!(
                mock_cursor_usage_request(status, "{}", "abc").0,
                Err(PollError::AuthRequired)
            ));
        }
        for (status, body) in [
            (500, "{}"),
            (200, "{invalid"),
            (200, "{}"),
            (200, r#"{"individualUsage":{}}"#),
        ] {
            assert!(matches!(
                mock_cursor_usage_request(status, body, "abc").0,
                Err(PollError::RequestFailed)
            ));
        }
        // An empty plan object parses fine: both slots stay default sections.
        let (result, _) =
            mock_cursor_usage_request(200, r#"{"individualUsage":{"plan":{}}}"#, "abc");
        let data = result.unwrap();
        assert_eq!(data.session.percentage, 0.0);
        assert!(data.session.resets_at.is_none());
        assert_eq!(data.weekly.percentage, 0.0);
        assert!(data.weekly.resets_at.is_none());
    }

    #[test]
    fn cursor_state_db_round_trip_and_credential_resolution() {
        let fixture = create_cursor_state_fixture(CURSOR_TEST_JWT);
        assert_eq!(
            query_cursor_state_value(&fixture, CURSOR_ACCESS_TOKEN_KEY).unwrap(),
            Some(CURSOR_TEST_JWT.to_string())
        );
        assert_eq!(
            query_cursor_state_value(&fixture, "missing/key").unwrap(),
            None
        );
        assert!(query_cursor_state_value(
            &fixture.with_extension("does-not-exist"),
            CURSOR_ACCESS_TOKEN_KEY
        )
        .is_err());

        let empty_fixture = create_cursor_state_fixture("");
        assert_eq!(
            query_cursor_state_value(&empty_fixture, CURSOR_ACCESS_TOKEN_KEY).unwrap(),
            None
        );

        // Env token wins over the state DB; the DB is used when the env var
        // is absent. A process-wide lock serializes the global env mutation
        // below; both vars are restored before the guard drops.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _env_guard = ENV_LOCK.lock().unwrap();
        let saved_token = std::env::var(CURSOR_SESSION_TOKEN_ENV).ok();
        let saved_appdata = std::env::var("APPDATA").ok();
        unsafe {
            std::env::set_var(CURSOR_SESSION_TOKEN_ENV, "abc::def");
        }
        assert_eq!(
            read_cursor_session_cookie().as_deref(),
            Some("abc%3A%3Adef")
        );

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let appdata =
            std::env::temp_dir().join(format!("cursor-appdata-{}-{nanos}", std::process::id()));
        let db_dir = appdata.join("Cursor").join("User").join("globalStorage");
        std::fs::create_dir_all(&db_dir).unwrap();
        std::fs::copy(&fixture, db_dir.join("state.vscdb")).unwrap();
        unsafe {
            std::env::remove_var(CURSOR_SESSION_TOKEN_ENV);
            std::env::set_var("APPDATA", &appdata);
        }
        assert_eq!(
            read_cursor_session_cookie().as_deref(),
            Some("user_123%3A%3Aheader.eyJzdWIiOiJhdXRoMHx1c2VyXzEyMyJ9.signature")
        );

        unsafe {
            match saved_token {
                Some(value) => std::env::set_var(CURSOR_SESSION_TOKEN_ENV, value),
                None => std::env::remove_var(CURSOR_SESSION_TOKEN_ENV),
            }
            match saved_appdata {
                Some(value) => std::env::set_var("APPDATA", value),
                None => std::env::remove_var("APPDATA"),
            }
        }
        std::fs::remove_file(&fixture).unwrap();
        std::fs::remove_file(&empty_fixture).unwrap();
        std::fs::remove_dir_all(&appdata).unwrap();
    }

    /// Serializes Grok test env mutation. Both request tests pin the same
    /// client version so the process-wide version cache is deterministic
    /// regardless of test execution order.
    static GROK_TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_grok_test_version<R>(f: impl FnOnce() -> R) -> R {
        let _guard = GROK_TEST_ENV_LOCK.lock().unwrap();
        let saved = std::env::var(GROK_CLIENT_VERSION_ENV).ok();
        unsafe {
            std::env::set_var(GROK_CLIENT_VERSION_ENV, "9.9.9-test");
        }
        let result = f();
        unsafe {
            match saved {
                Some(value) => std::env::set_var(GROK_CLIENT_VERSION_ENV, value),
                None => std::env::remove_var(GROK_CLIENT_VERSION_ENV),
            }
        }
        result
    }

    const GROK_BILLING_FIXTURE: &str = r#"{
        "config": {
            "creditUsagePercent": 48.0,
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "end": "2026-09-27T13:10:00.412622+00:00"
            },
            "productUsage": [{"product": "GrokBuild", "usagePercent": 48.0}]
        }
    }"#;

    fn test_grok_session() -> GrokSession {
        GrokSession {
            access_token: "tok".to_string(),
            user_id: "u-1".to_string(),
            can_refresh: false,
        }
    }

    fn mock_grok_billing_request(
        status: u16,
        body: &str,
        session: &GrokSession,
    ) -> (Result<UsageData, PollError>, String) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/billing?format=credits",
            listener.local_addr().unwrap()
        );
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        let result = fetch_grok_usage(session, &url);
        (result, server.join().unwrap())
    }

    #[test]
    fn grok_selects_latest_expiring_xai_session_and_skips_blank() {
        let content = r#"{
            "https://auth.x.ai::old": {
                "key": "older",
                "user_id": "u-old",
                "expires_at": "2026-01-01T00:00:00Z"
            },
            "https://auth.x.ai::new": {
                "key": "newer",
                "user_id": "u-new",
                "refresh_token": "rt-new",
                "expires_at": "2026-06-01T00:00:00Z"
            },
            "https://auth.x.ai::blank": {
                "key": "   ",
                "user_id": "u-blank",
                "expires_at": "2027-01-01T00:00:00Z"
            }
        }"#;

        let session = select_grok_session(content).unwrap();
        assert_eq!(session.access_token, "newer");
        assert_eq!(session.user_id, "u-new");
        assert!(session.can_refresh);
    }

    #[test]
    fn grok_accepts_legacy_signin_scope_without_refresh() {
        let session = select_grok_session(
            r#"{"https://accounts.x.ai/sign-in":{"key":"token","user_id":"u-app"}}"#,
        )
        .unwrap();
        assert_eq!(session.access_token, "token");
        // Nothing to renew with, so the CLI must not be asked to sign in.
        assert!(!session.can_refresh);
        assert!(select_grok_session("{}").is_none());
        assert!(select_grok_session("not json").is_none());
    }

    #[test]
    fn grok_rejects_unrelated_identity_providers_and_api_keys() {
        let unrelated = r#"{
            "https://idp.example.com::client": {
                "key": "corporate-token", "expires_at": "2099-01-01T00:00:00Z"
            },
            "https://auth.x.ai.example.com::client": {"key": "lookalike-token"},
            "xai::api_key": {"key": "api-key"}
        }"#;
        assert!(select_grok_session(unrelated).is_none());
        let mut entries: serde_json::Value = serde_json::from_str(unrelated).unwrap();
        entries["https://auth.x.ai::grok-cli"] = serde_json::json!({
            "key": "session", "expires_at": "2026-01-01T00:00:00Z"
        });
        assert_eq!(
            select_grok_session(&entries.to_string())
                .unwrap()
                .access_token,
            "session"
        );
    }

    #[test]
    fn grok_billing_pool_maps_to_weekly_window() {
        let response: GrokBillingResponse = serde_json::from_str(
            r#"{
                "config": {
                    "creditUsagePercent": 42.5,
                    "currentPeriod": {
                        "periodType": "weekly",
                        "end": "2026-09-28T19:27:24.000Z"
                    }
                }
            }"#,
        )
        .unwrap();

        let data = grok_usage_from_billing(response).unwrap();
        assert_eq!(data.weekly.percentage, 42.5);
        assert!(data.weekly.resets_at.is_some());
        // Grok has no five-hour window, so nothing claims the session slot.
        assert_eq!(data.session.percentage, 0.0);
        assert!(data.session.resets_at.is_none());
    }

    #[test]
    fn grok_fresh_period_without_spending_reads_as_zero() {
        // The proxy speaks protobuf JSON, which drops the zero
        // `creditUsagePercent`: a complete period without it is 0% spent.
        let response: GrokBillingResponse = serde_json::from_str(
            r#"{
                "config": {
                    "currentPeriod": {
                        "type": "USAGE_PERIOD_TYPE_WEEKLY",
                        "end": "2026-10-04T13:10:00.412622+00:00"
                    }
                }
            }"#,
        )
        .unwrap();

        let data = grok_usage_from_billing(response).unwrap();
        assert_eq!(data.weekly.percentage, 0.0);
        assert!(data.weekly.resets_at.is_some());
    }

    #[test]
    fn grok_config_without_period_or_percentage_is_rejected() {
        let response: GrokBillingResponse =
            serde_json::from_str(r#"{"config":{"isUnifiedBillingUser":true}}"#).unwrap();
        assert!(grok_usage_from_billing(response).is_none());
        let empty: GrokBillingResponse = serde_json::from_str("{}").unwrap();
        assert!(grok_usage_from_billing(empty).is_none());
    }

    #[test]
    fn grok_unusable_period_end_does_not_imply_zero_usage() {
        for end in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!("invalid"),
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1e100),
        ] {
            let response = serde_json::from_value(serde_json::json!({
                "config": {"currentPeriod": {"end": end}}
            }))
            .unwrap();
            assert!(grok_usage_from_billing(response).is_none(), "{end}");
        }
    }

    #[test]
    fn grok_explicit_usage_does_not_require_a_usable_period() {
        for period in [
            serde_json::json!(null),
            serde_json::json!({"end": "invalid"}),
        ] {
            for percentage in [0.0, 42.5] {
                let response = serde_json::from_value(serde_json::json!({
                    "config": {"creditUsagePercent": percentage, "currentPeriod": period}
                }))
                .unwrap();
                let data = grok_usage_from_billing(response).unwrap();
                assert_eq!(data.weekly.percentage, percentage);
                assert!(data.weekly.resets_at.is_none());
            }
        }
    }

    #[test]
    fn grok_numeric_period_end_accepts_seconds_and_millis() {
        // A numeric end reads as-is; the monthly label itself arrives in P5.
        let response = serde_json::from_value(serde_json::json!({
            "config": {"currentPeriod": {"end": 1790000000}}
        }))
        .unwrap();
        let data = grok_usage_from_billing(response).unwrap();
        assert_eq!(data.weekly.percentage, 0.0);
        assert_eq!(
            data.weekly.resets_at,
            Some(UNIX_EPOCH + Duration::from_secs(1_790_000_000))
        );
        assert_eq!(
            grok_period_end(Some(&serde_json::json!(1_790_000_000_000u64))),
            Some(UNIX_EPOCH + Duration::from_secs(1_790_000_000))
        );
        for value in [serde_json::json!(1e300), serde_json::json!(1e25)] {
            assert_eq!(grok_period_end(Some(&value)), None);
        }
    }

    #[test]
    fn grok_request_sends_cli_markers_and_maps_billing_pool() {
        with_grok_test_version(|| {
            let (result, request) =
                mock_grok_billing_request(200, GROK_BILLING_FIXTURE, &test_grok_session());
            let data = result.unwrap();
            assert_eq!(data.weekly.percentage, 48.0);
            assert!(data.weekly.resets_at.is_some());
            let request = request.to_ascii_lowercase();
            assert!(request.starts_with("get /v1/billing?format=credits http/1.1\r\n"));
            assert!(request.contains("\r\nauthorization: bearer tok\r\n"));
            assert!(request.contains("\r\nx-xai-token-auth: xai-grok-cli\r\n"));
            assert!(request.contains("\r\nx-grok-client-mode: cli\r\n"));
            assert!(request.contains("\r\nx-grok-client-version: 9.9.9-test\r\n"));
            assert!(request.contains("\r\nx-userid: u-1\r\n"));
        });
    }

    #[test]
    fn grok_request_distinguishes_auth_errors_from_bad_responses() {
        with_grok_test_version(|| {
            let session = test_grok_session();
            for status in [401, 403] {
                assert!(matches!(
                    mock_grok_billing_request(status, "{}", &session).0,
                    Err(PollError::AuthRequired)
                ));
            }
            // Transport-level classification: 500 maps to ServerError.
            assert!(matches!(
                mock_grok_billing_request(500, "{}", &session).0,
                Err(PollError::ServerError)
            ));
            for body in ["<html></html>", "{invalid", "{}", "null"] {
                assert!(matches!(
                    mock_grok_billing_request(200, body, &session).0,
                    Err(PollError::RequestFailed)
                ));
            }
        });
    }

    #[test]
    fn grok_auth_path_prefers_explicit_home() {
        let _guard = GROK_TEST_ENV_LOCK.lock().unwrap();
        let saved = std::env::var(GROK_HOME_ENV).ok();
        let home = std::env::temp_dir().join(format!("grok-home-{}", std::process::id()));
        unsafe {
            std::env::set_var(GROK_HOME_ENV, &home);
        }
        assert_eq!(grok_auth_path().unwrap(), home.join("auth.json"));
        unsafe {
            match saved {
                Some(value) => std::env::set_var(GROK_HOME_ENV, value),
                None => std::env::remove_var(GROK_HOME_ENV),
            }
        }
    }

    #[test]
    fn grok_version_is_picked_out_of_arbitrary_output() {
        assert_eq!(
            first_grok_version("grok 1.24.3 (abc1234)").as_deref(),
            Some("1.24.3")
        );
        assert_eq!(
            first_grok_version("grok-build v2.0.11\n").as_deref(),
            Some("2.0.11")
        );
        assert_eq!(first_grok_version("no version here"), None);
    }

    #[test]
    fn grok_cli_detection_retries_failures_and_caches_success() {
        let cache = OnceLock::new();
        assert_eq!(cached_grok_cli_detection(&cache, || None), None);
        assert_eq!(
            cached_grok_cli_detection(&cache, || Some("1.2.3".into())).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            cached_grok_cli_detection(&cache, || panic!("successful detection must be cached"))
                .as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn credential_signature_detects_same_length_rewrites_within_one_second() {
        let path = std::env::temp_dir().join(format!(
            "quota-credential-signature-{}.json",
            std::process::id()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        use std::io::Write;
        file.write_all(b"fixture").unwrap();
        let time = UNIX_EPOCH + Duration::from_secs(1700000000);
        file.set_modified(time + Duration::from_millis(100))
            .unwrap();
        let before = windows_credential_watch_signature(&path);
        file.set_modified(time + Duration::from_millis(200))
            .unwrap();
        let after = windows_credential_watch_signature(&path);
        assert_ne!(before, after);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn claude_credentials_path_honors_custom_config_directory() {
        assert_eq!(
            windows_credentials_path_from(
                Some(PathBuf::from(r"D:\claude-config")),
                Some(PathBuf::from(r"C:\Users\Ray")),
            ),
            Some(PathBuf::from(r"D:\claude-config\.credentials.json"))
        );
        assert_eq!(
            windows_credentials_path_from(None, Some(PathBuf::from(r"C:\Users\Ray"))),
            Some(PathBuf::from(r"C:\Users\Ray\.claude\.credentials.json"))
        );
    }

    #[test]
    fn remaining_percentage_is_clamped() {
        assert_eq!(remaining_percentage(30.0), 70.0);
        assert_eq!(remaining_percentage(-5.0), 100.0);
        assert_eq!(remaining_percentage(120.0), 0.0);
    }

    #[test]
    fn codex_weekly_only_window_is_not_misreported_as_session_usage() {
        let response: CodexUsageResponse = serde_json::from_str(
            r#"{
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 21,
                        "limit_window_seconds": 604800,
                        "reset_at": 1784500338
                    },
                    "secondary_window": null
                }
            }"#,
        )
        .expect("weekly-only response should deserialize");

        let usage = codex_usage_from_response(response).expect("rate limit should be available");

        assert_eq!(usage.session.percentage, 0.0);
        assert!(usage.session.resets_at.is_none());
        assert_eq!(usage.weekly.percentage, 21.0);
        assert!(usage.weekly.resets_at.is_some());
        for language in [
            crate::localization::LanguageId::SimplifiedChinese,
            crate::localization::LanguageId::English,
        ] {
            assert_eq!(
                format_codex_line(
                    &usage.session,
                    language.strings(),
                    language == crate::localization::LanguageId::SimplifiedChinese,
                    UsageWindowKind::Session,
                ),
                "--"
            );
        }
    }

    #[test]
    fn codex_legacy_windows_keep_positional_mapping_without_durations() {
        let response: CodexUsageResponse = serde_json::from_str(
            r#"{
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 18,
                        "reset_at": 1784500338
                    },
                    "secondary_window": {
                        "used_percent": 33,
                        "reset_at": 1785000000
                    }
                }
            }"#,
        )
        .expect("legacy response should deserialize");

        let usage = codex_usage_from_response(response).expect("rate limit should be available");

        assert_eq!(usage.session.percentage, 18.0);
        assert_eq!(usage.weekly.percentage, 33.0);
    }

    #[test]
    fn codex_session_only_zero_usage_keeps_real_quota_visible() {
        let response: CodexUsageResponse = serde_json::from_str(
            r#"{
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 0,
                        "limit_window_seconds": 18000,
                        "reset_at": 1784500338
                    },
                    "secondary_window": null
                }
            }"#,
        )
        .unwrap();
        let usage = codex_usage_from_response(response).unwrap();
        let strings = crate::localization::LanguageId::SimplifiedChinese.strings();
        assert!(
            format_codex_line(&usage.session, strings, true, UsageWindowKind::Session)
                .starts_with("100%")
        );
        assert_eq!(
            format_codex_line(&usage.weekly, strings, true, UsageWindowKind::Weekly),
            "--"
        );
    }

    #[test]
    fn rate_limit_response_preserves_valid_retry_hints() {
        for (header, expected) in [("120", Some(120_000)), ("invalid", None)] {
            let response: ureq::Response =
                format!("HTTP/1.1 429 Too Many Requests\r\nRetry-After: {header}\r\n\r\n")
                    .parse()
                    .unwrap();
            assert_eq!(
                classify_ureq_error(&ureq::Error::Status(429, response)),
                PollError::RateLimited(expected)
            );
        }
    }

    #[test]
    fn classifies_http_failures_for_user_visible_recovery() {
        assert_eq!(classify_http_status(401), PollError::AuthRequired);
        assert_eq!(classify_http_status(403), PollError::AuthRequired);
        assert_eq!(classify_http_status(429), PollError::RateLimited(None));
        assert_eq!(classify_http_status(500), PollError::ServerError);
        assert_eq!(classify_http_status(503), PollError::ServerError);
        assert_eq!(classify_http_status(404), PollError::RequestFailed);
        assert_eq!(
            PollError::NetworkUnavailable.category(),
            "network_unavailable"
        );
    }

    #[test]
    fn simplified_chinese_line_labels_remaining_usage() {
        let strings = crate::localization::LanguageId::SimplifiedChinese.strings();
        assert_eq!(strings.session_window, "5h");
        assert_eq!(strings.weekly_window, "7d");
        let section = UsageSection {
            percentage: 30.0,
            resets_at: None,
        };
        assert_eq!(
            format_line(&section, strings, true, UsageWindowKind::Session),
            "70%"
        );
        let session_reset = windows::Win32::Foundation::SYSTEMTIME {
            wHour: 18,
            wMinute: 30,
            ..Default::default()
        };
        assert_eq!(
            format_simplified_chinese_values(82.0, Some(session_reset), UsageWindowKind::Session,),
            "82%  18:30重置"
        );
        let weekly_reset = windows::Win32::Foundation::SYSTEMTIME {
            wMonth: 7,
            wDay: 17,
            ..Default::default()
        };
        assert_eq!(
            format_simplified_chinese_values(97.0, Some(weekly_reset), UsageWindowKind::Weekly,),
            "97%  07/17重置"
        );
    }

    #[test]
    fn antigravity_summary_prefers_gemini_group() {
        let response: AntigravityQuotaSummaryResponse = serde_json::from_str(
            r#"{
                "groups": [
                    {
                        "displayName": "Claude and GPT models",
                        "buckets": [
                            {
                                "bucketId": "3p-weekly",
                                "window": "weekly",
                                "resetTime": "2026-06-20T18:32:02Z",
                                "remainingFraction": 1
                            },
                            {
                                "bucketId": "3p-5h",
                                "window": "5h",
                                "resetTime": "2026-06-13T23:32:02Z",
                                "remainingFraction": 1
                            }
                        ]
                    },
                    {
                        "displayName": "Gemini Models",
                        "description": "Models within this group: Gemini Flash, Gemini Pro",
                        "buckets": [
                            {
                                "bucketId": "gemini-weekly",
                                "displayName": "Weekly Limit",
                                "window": "weekly",
                                "resetTime": "2026-06-20T17:08:54Z",
                                "remainingFraction": 0.99304295
                            },
                            {
                                "bucketId": "gemini-5h",
                                "displayName": "Five Hour Limit",
                                "window": "5h",
                                "resetTime": "2026-06-13T22:08:54Z",
                                "remainingFraction": 0.9582575
                            }
                        ]
                    }
                ]
            }"#,
        )
        .expect("summary response should deserialize");

        let usage =
            antigravity_usage_from_summary(response).expect("Gemini quota should be selected");

        assert!((usage.weekly.percentage - 0.695705).abs() < 0.000001);
        assert!((usage.session.percentage - 4.17425).abs() < 0.000001);
        assert!(usage.weekly.resets_at.is_some());
        assert!(usage.session.resets_at.is_some());
    }
}
