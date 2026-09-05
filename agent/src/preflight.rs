
use std::time::Duration;

use tokio::process::Command;
use tracing::{info, warn};

use crate::config::Config;

struct BackendCheck {
    bin: &'static str,
    args: &'static [&'static str],
    is_login_probe: bool,
    hint: &'static str,
}

fn check_for(cli: &str) -> Option<BackendCheck> {
    Some(match cli {
        "claude" => BackendCheck {
            bin: "claude",
            args: &["--version"],
            is_login_probe: false,
            hint: "install Claude Code (curl -fsSL https://claude.ai/install.sh | bash) and sign in with `claude` (or set CLAUDE_CODE_OAUTH_TOKEN)",
        },
        "codex" => BackendCheck {
            bin: "codex",
            args: &["--version"],
            is_login_probe: false,
            hint: "install @openai/codex and run `codex login`",
        },
        "opencode" => BackendCheck {
            bin: "opencode",
            args: &["--version"],
            is_login_probe: false,
            hint: "install opencode, set OPENROUTER_API_KEY and WORLD_ENABLE_OPENCODE=1",
        },
        "cursor" => BackendCheck {
            bin: "cursor-agent",
            args: &["status"],
            is_login_probe: true,
            hint: "run `cursor-agent login` on the agent host",
        },
        "omp" => BackendCheck {
            bin: "omp",
            args: &["--version"],
            is_login_probe: false,
            hint: "install @oh-my-pi/pi-coding-agent with bun, put models.yml in ~/.omp/agent (or OMP_CONFIG_DIR) and set FAL_KEY",
        },
        "grok" => BackendCheck {
            bin: "grok",
            args: &["--version"],
            is_login_probe: false,
            hint: "install grok and run `grok login` on the agent host",
        },
        _ => return None,
    })
}

enum ProbeResult {
    Ok,
    NotFound,
    Failed,
    Timeout,
}

pub async fn check_backends(config: &Config) {
    for cli in config.backends.keys() {
        if !loka_shared::backends::enabled_here(cli) {
            warn!(backend = %cli, "OpenCode is configured but disabled; set WORLD_ENABLE_OPENCODE=1 to use it");
            continue;
        }
        let Some(check) = check_for(cli) else {
            info!(backend = %cli, "No preflight check for backend — skipping");
            continue;
        };
        match probe(&check).await {
            ProbeResult::Ok => {
                if check.is_login_probe {
                    info!(backend = %cli, bin = check.bin, "Backend authenticated");
                } else {
                    info!(backend = %cli, bin = check.bin, "Backend CLI available");
                }
            }
            ProbeResult::NotFound => warn!(
                backend = %cli,
                bin = check.bin,
                hint = check.hint,
                "Backend CLI not found on PATH — tasks using this backend will fail"
            ),
            ProbeResult::Failed => {
                if check.is_login_probe {
                    warn!(
                        backend = %cli,
                        bin = check.bin,
                        hint = check.hint,
                        "Backend not authenticated — tasks using this backend will fail"
                    );
                } else {
                    warn!(
                        backend = %cli,
                        bin = check.bin,
                        hint = check.hint,
                        "Backend CLI check failed"
                    );
                }
            }
            ProbeResult::Timeout => warn!(
                backend = %cli,
                bin = check.bin,
                "Backend preflight timed out — assuming available"
            ),
        }
    }
}

async fn probe(check: &BackendCheck) -> ProbeResult {
    let located = (check.bin == "claude").then(crate::claude::detect::locate).flatten();
    let mut cmd = match &located {
        Some((program, _)) => Command::new(program),
        None => Command::new(check.bin),
    };
    if let Some(path) = located.and_then(|(_, path)| path) {
        cmd.env("PATH", path);
    }
    cmd.env_remove("WORLD_SECRETS_KEY");
    cmd.args(check.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    match tokio::time::timeout(Duration::from_secs(10), cmd.status()).await {
        Err(_) => ProbeResult::Timeout,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => ProbeResult::NotFound,
        Ok(Err(_)) => ProbeResult::Failed,
        Ok(Ok(status)) if status.success() => ProbeResult::Ok,
        Ok(Ok(_)) => ProbeResult::Failed,
    }
}
