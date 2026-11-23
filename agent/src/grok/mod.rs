use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::cli::{collect_stderr, configure_child_environment, CliOptions, CliResult, Redactor, StreamEvent};

const AGENT_PREAMBLE: &str = "\
You are Loka, an autonomous coding agent. You work independently without \
human interaction during execution. If you are uncertain, make the best judgment \
call and document your reasoning in a code comment or commit message.\n\n\
Follow existing code patterns and conventions in each repository. \
Read AGENTS.md or CLAUDE.md if they exist for project-specific instructions.\n\n\
Git commit rules:\n\
- Use Conventional Commits: type(scope): description\n\
- Types: feat, fix, refactor, test, chore, perf, ci\n\
- NEVER add Co-Authored-By trailers to commits\n\
- NEVER add Signed-off-by trailers to commits";

const TEXT_FLUSH_CHARS: usize = 200;

fn ready_text(unflushed: &mut String, redactor: &Redactor, last: bool) -> Option<String> {
    *unflushed = redactor.redact(unflushed);
    if last {
        let text = std::mem::take(unflushed);
        return (!text.is_empty()).then_some(text);
    }
    let held = redactor.longest().saturating_sub(1);
    let total = unflushed.chars().count();
    if total < TEXT_FLUSH_CHARS + held {
        return None;
    }
    let split = unflushed.char_indices().nth(total - held).map_or(unflushed.len(), |(at, _)| at);
    let kept = unflushed.split_off(split);
    Some(std::mem::replace(unflushed, kept))
}

pub async fn run(opts: CliOptions<'_>) -> Result<CliResult> {
    if opts.mcp_has_secrets {
        anyhow::bail!("grok does not support MCP servers with environment secret references");
    }
    let full_prompt = if opts.session_id.is_some() && opts.resume {
        opts.prompt.to_string()
    } else {
        let mut parts = vec![AGENT_PREAMBLE.to_string()];
        if let Some(sys) = opts.system_prompt {
            parts.push(sys.to_string());
        }
        parts.push(opts.prompt.to_string());
        parts.join("\n\n")
    };

    let prompt_path = std::env::temp_dir().join(format!("loka-grok-{}.txt", uuid::Uuid::new_v4()));
    tokio::fs::write(&prompt_path, full_prompt.as_bytes())
        .await
        .with_context(|| {
            format!(
                "Failed to write grok prompt file: {}",
                prompt_path.display()
            )
        })?;

    let result = run_inner(&opts, &prompt_path).await;

    let _ = tokio::fs::remove_file(&prompt_path).await;

    result
}

async fn run_inner(opts: &CliOptions<'_>, prompt_path: &std::path::Path) -> Result<CliResult> {
    let mut cmd = Command::new("grok");
    configure_child_environment(&mut cmd, &opts.child_environment())?;

    cmd.arg("--no-alt-screen");
    cmd.arg("--cwd").arg(opts.working_dir);
    cmd.arg("--prompt-file").arg(prompt_path);
    cmd.arg("--output-format").arg("streaming-json");

    cmd.arg("--permission-mode").arg("bypassPermissions");

    cmd.arg("-m").arg(opts.model);
    cmd.arg("--max-turns").arg(opts.max_turns.to_string());

    if let Some(sid) = opts.session_id {
        if opts.resume {
            cmd.arg("--resume").arg(sid);
        }
    }

    if !opts.image_paths.is_empty() {
        warn!(
            count = opts.image_paths.len(),
            "Grok backend does not support image inputs — images will be ignored"
        );
    }

    if opts.mcp_config_json.is_some() {
        debug!("grok backend reads MCP servers from machine config, not per-task mcp_config_json");
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    info!(
        dir = %opts.working_dir.display(),
        model = opts.model,
        resuming = opts.resume && opts.session_id.is_some(),
        "Invoking Grok CLI"
    );

    let mut child = cmd
        .spawn()
        .context("Failed to spawn grok CLI — is it installed and logged in? Run: grok login")?;

    let stderr_handle = {
        let stderr = child.stderr.take().expect("stderr piped");
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            reader.read_to_string(&mut buf).await.ok();
            buf
        })
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let mut lines = BufReader::new(stdout).lines();

    let mut output = String::new();
    let mut unflushed = String::new();
    let redactor = opts.redactor();
    let mut session_id: Option<String> = None;
    let mut error_message: Option<String> = None;

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        match json.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "text" => {
                if let Some(data) = json.get("data").and_then(|v| v.as_str()) {
                    output.push_str(data);
                    unflushed.push_str(data);
                    if let Some(text) = ready_text(&mut unflushed, &redactor, false) {
                        if let Some(tx) = &opts.event_tx {
                            let _ = tx.send(StreamEvent::AssistantText(text));
                        }
                    }
                }
            }
            "thought" => {}
            "end" => {
                if let Some(sid) = json.get("sessionId").and_then(|v| v.as_str()) {
                    session_id = Some(sid.to_string());
                }
            }
            "error" => {
                let msg = json
                    .get("message")
                    .or_else(|| json.pointer("/error/message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                if let Some(tx) = &opts.event_tx {
                    let _ = tx.send(StreamEvent::Error(msg.clone()));
                }
                error_message = Some(msg);
            }
            _ => {}
        }
    }

    if let Some(text) = ready_text(&mut unflushed, &redactor, true) {
        if let Some(tx) = &opts.event_tx {
            let _ = tx.send(StreamEvent::AssistantText(text));
        }
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);
    let stderr = collect_stderr(stderr_handle).await;

    debug!(exit_code, stderr_len = stderr.len(), "Grok CLI finished");
    if !stderr.is_empty()
        && (opts.environment.is_empty() && opts.provider_keys.is_empty())
        && !opts.mcp_has_secrets
        && !opts.sensitive
    {
        debug!(stderr = %stderr, "Grok stderr");
    }

    let output = output.trim().to_string();

    if let Some(msg) = error_message {
        if output.is_empty() {
            anyhow::bail!("Grok CLI returned error: {msg}");
        }
    }

    if exit_code != 0 && output.is_empty() {
        anyhow::bail!("Grok CLI exited with code {exit_code}: {stderr}");
    }

    info!(
        exit_code,
        session_id = session_id.as_deref().unwrap_or("none"),
        "Grok CLI completed"
    );

    Ok(CliResult {
        output,
        session_id,
        cost_usd: 0.0,
        exit_code,
        usage: Default::default(),
        checkpoint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{ready_text, TEXT_FLUSH_CHARS};
    use crate::cli::Redactor;

    #[test]
    fn a_secret_split_across_streamed_chunks_is_hidden_in_every_flushed_piece() {
        let secret = "sk-live-0123456789abcdef";
        let redactor = Redactor::new([secret]);
        let text = format!("{}{secret}{}", "a".repeat(TEXT_FLUSH_CHARS * 3 - 10), "b".repeat(TEXT_FLUSH_CHARS));
        for chunk in [1usize, 7, 24, 61, 199] {
            let mut unflushed = String::new();
            let mut shown = Vec::new();
            let chars: Vec<char> = text.chars().collect();
            for piece in chars.chunks(chunk) {
                unflushed.extend(piece);
                shown.extend(ready_text(&mut unflushed, &redactor, false));
            }
            shown.extend(ready_text(&mut unflushed, &redactor, true));
            for piece in &shown {
                for start in 0..=secret.len() - loka_shared::redaction::MIN_SECRET_BYTES {
                    assert!(!piece.contains(&secret[start..start + loka_shared::redaction::MIN_SECRET_BYTES]), "chunk {chunk}: a piece of the secret was flushed");
                }
            }
            assert!(shown.iter().map(String::len).sum::<usize>() > text.len() - secret.len(), "chunk {chunk}: text was lost");
        }
    }
}
