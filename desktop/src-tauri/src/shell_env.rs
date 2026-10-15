use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub(crate) const LOGIN_ENV: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CONFIG_DIR",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_HOME",
    "CURSOR_API_KEY",
    "WORLD_METRICS_URL",
    "WORLD_LOGS_URL",
];

const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
const MARKER: &str = "__loka_env_start__";

pub(crate) fn logins() -> &'static [(String, String)] {
    static CACHE: OnceLock<Vec<(String, String)>> = OnceLock::new();
    CACHE.get_or_init(|| missing_from_app(read_shell().as_deref().unwrap_or_default()))
}

fn missing_from_app(output: &str) -> Vec<(String, String)> {
    parse(output)
        .into_iter()
        .filter(|(name, _)| std::env::var_os(name).filter(|value| !value.is_empty()).is_none())
        .collect()
}

fn parse(output: &str) -> Vec<(String, String)> {
    let Some((_, listing)) = output.split_once(MARKER) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = Vec::new();
    for line in listing.lines() {
        let Some((name, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        if LOGIN_ENV.contains(&name) && !value.is_empty() && !found.iter().any(|(seen, _)| seen == name) {
            found.push((name.to_string(), value.to_string()));
        }
    }
    found
}

fn read_shell() -> Option<String> {
    let shell = crate::workbench::login_shell();
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", &format!("echo {MARKER}; /usr/bin/env")])
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SHELL_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_login_variables_after_the_marker() {
        let output = format!(
            "CLAUDE_CODE_OAUTH_TOKEN=from-the-motd\n{MARKER}\nPATH=/usr/bin\nCLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat-1\nAWS_SECRET_ACCESS_KEY=nope\nOPENAI_API_KEY=\nCODEX_HOME=/Users/me/.codex\n"
        );
        let found = parse(&output);
        assert_eq!(
            found,
            vec![
                ("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "sk-ant-oat-1".to_string()),
                ("CODEX_HOME".to_string(), "/Users/me/.codex".to_string()),
            ]
        );
        assert!(parse("CLAUDE_CODE_OAUTH_TOKEN=x\n").is_empty(), "nothing before the marker counts");
    }
}
