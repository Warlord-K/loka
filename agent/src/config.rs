use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
pub struct BackendConfig {}

pub type Backends = indexmap::IndexMap<String, BackendConfig>;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub github_token: String,
    pub repos_dir: PathBuf,
    pub workspaces_dir: PathBuf,
    pub backends: Backends,
    pub repos: Vec<RepoConfig>,
    pub skills: Vec<Skill>,
    pub mcp_servers: Vec<McpServer>,
    pub signing: Option<SigningConfig>,
}

#[derive(Clone, Debug)]
pub struct SigningConfig {
    pub ssh_key_path: PathBuf,
    pub allowed_signers_path: Option<PathBuf>,
}

const SIGNING_MOUNT_DIR: &str = "/home/agent/.ssh/signing";

const SIGNING_KEY_NAMES: &[&str] = &["signing_key", "id_ed25519", "id_ecdsa", "id_rsa"];

impl SigningConfig {
    fn auto_detect() -> Option<Self> {
        let dir = Path::new(SIGNING_MOUNT_DIR);
        if !dir.is_dir() {
            return None;
        }

        for name in SIGNING_KEY_NAMES {
            let key_path = dir.join(name);
            if key_path.is_file() {
                let allowed_signers = dir.join("allowed_signers");
                return Some(Self {
                    ssh_key_path: key_path,
                    allowed_signers_path: if allowed_signers.is_file() {
                        Some(allowed_signers)
                    } else {
                        None
                    },
                });
            }
        }

        None
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RepoConfig {
    pub repo: String,
    #[serde(default = "default_branch")]
    pub branch: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServer {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    repos: Vec<RepoConfig>,
    #[serde(default)]
    agent: AgentFileConfig,
    #[serde(default)]
    skills: Vec<Skill>,
    #[serde(default)]
    mcp_servers: Vec<McpServer>,
    #[serde(default)]
    signing: Option<SigningFileConfig>,
}

#[derive(Deserialize)]
struct SigningFileConfig {
    ssh_key_path: String,
    #[serde(default)]
    allowed_signers_path: Option<String>,
}

#[derive(Deserialize, Default)]
struct AgentFileConfig {
    #[serde(default = "default_backends")]
    backends: Backends,
}

fn default_backends() -> Backends {
    let mut backends = Backends::new();
    backends.insert(
        "claude".to_string(),
        BackendConfig {},
    );
    backends.insert(
        "codex".to_string(),
        BackendConfig {},
    );
    backends.insert(
        "cursor".to_string(),
        BackendConfig {},
    );
    backends.insert(
        "grok".to_string(),
        BackendConfig {},
    );
    backends
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config_path =
            loka_shared::loka_env::config_path();

        let path = std::path::Path::new(&config_path);
        let file_config = if path.is_file() {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {config_path}"))?;
            serde_yaml::from_str::<ConfigFile>(&contents)
                .with_context(|| format!("Failed to parse config file: {config_path}"))?
        } else {
            tracing::warn!(
                "Config file not found at {config_path}, using defaults (repos/schedules from DB)"
            );
            ConfigFile::default()
        };

        let repos_dir =
            PathBuf::from(std::env::var("REPOS_DIR").unwrap_or_else(|_| "/workspace".to_string()));
        let workspaces_dir = PathBuf::from(
            std::env::var("WORKSPACES_DIR")
                .unwrap_or_else(|_| repos_dir.join(".workspaces").to_string_lossy().to_string()),
        );

        let mut skills = file_config.skills;
        let skills_dir = Path::new(&config_path)
            .parent()
            .unwrap_or(Path::new("."))
            .join("skills");
        if skills_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "md").unwrap_or(false) {
                        if let Ok(skill) = load_skill_file(&path) {
                            if !skills.iter().any(|s| s.name == skill.name) {
                                skills.push(skill);
                            }
                        }
                    }
                }
            }
        }

        let mcp_servers: Vec<McpServer> = file_config
            .mcp_servers
            .into_iter()
            .filter_map(|mut server| {
                let mut missing = Vec::new();
                server.env = server
                    .env
                    .into_iter()
                    .map(|(k, v)| {
                        let resolved = match v.strip_prefix("${").and_then(|rest| rest.strip_suffix('}')) {
                            Some(var_name) => match std::env::var(var_name) {
                                Ok(value) if !value.trim().is_empty() => value,
                                _ => {
                                    missing.push(var_name.to_string());
                                    v
                                }
                            },
                            None => v,
                        };
                        (k, resolved)
                    })
                    .collect();
                if missing.is_empty() {
                    return Some(server);
                }
                tracing::info!(server = %server.name, missing = %missing.join(", "), "Skipping MCP server until its environment is set");
                None
            })
            .collect();

        let signing = if let Some(sc) = file_config.signing {
            Some(SigningConfig {
                ssh_key_path: PathBuf::from(sc.ssh_key_path),
                allowed_signers_path: sc.allowed_signers_path.map(PathBuf::from),
            })
        } else if let Ok(key_path) = std::env::var("GIT_SIGNING_KEY") {
            Some(SigningConfig {
                ssh_key_path: PathBuf::from(key_path),
                allowed_signers_path: std::env::var("GIT_ALLOWED_SIGNERS").ok().map(PathBuf::from),
            })
        } else {
            SigningConfig::auto_detect()
        };

        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is required")?,
            github_token: std::env::var("GITHUB_TOKEN").unwrap_or_default(),
            repos_dir,
            workspaces_dir,
            backends: file_config.agent.backends,
            repos: file_config.repos,
            skills,
            mcp_servers,
            signing,
        })
    }

    pub fn mcp_config_json(&self) -> Option<serde_json::Value> {
        if self.mcp_servers.is_empty() {
            return None;
        }

        let mut servers = serde_json::Map::new();
        for server in &self.mcp_servers {
            let mut entry = serde_json::Map::new();

            if let Some(server_type) = &server.r#type {
                if server_type == "sse" {
                    entry.insert("type".into(), "sse".into());
                    if let Some(url) = &server.url {
                        entry.insert("url".into(), url.clone().into());
                    }
                    servers.insert(server.name.clone(), entry.into());
                    continue;
                }
            }

            if let Some(cmd) = &server.command {
                entry.insert("command".into(), cmd.clone().into());
            }
            if !server.args.is_empty() {
                entry.insert("args".into(), server.args.clone().into());
            }
            if !server.env.is_empty() {
                entry.insert("env".into(), serde_json::json!(server.env));
            }
            servers.insert(server.name.clone(), entry.into());
        }

        Some(serde_json::json!({ "mcpServers": servers }))
    }

    pub fn opencode_config_json(&self) -> Option<serde_json::Value> {
        if self.mcp_servers.is_empty() {
            return None;
        }

        let mut servers = serde_json::Map::new();
        for server in &self.mcp_servers {
            let mut entry = serde_json::Map::new();
            let is_remote = matches!(
                server.r#type.as_deref(),
                Some("sse") | Some("http") | Some("remote")
            );

            if is_remote {
                entry.insert("type".into(), "remote".into());
                if let Some(url) = &server.url {
                    entry.insert("url".into(), url.clone().into());
                }
                if !server.env.is_empty() {
                    entry.insert("headers".into(), serde_json::json!(server.env));
                }
            } else {
                entry.insert("type".into(), "local".into());
                let mut command_arr: Vec<serde_json::Value> = Vec::new();
                if let Some(cmd) = &server.command {
                    command_arr.push(cmd.clone().into());
                }
                for a in &server.args {
                    command_arr.push(a.clone().into());
                }
                if !command_arr.is_empty() {
                    entry.insert("command".into(), serde_json::Value::Array(command_arr));
                }
                if !server.env.is_empty() {
                    entry.insert("environment".into(), serde_json::json!(server.env));
                }
            }

            entry.insert("enabled".into(), true.into());
            servers.insert(server.name.clone(), entry.into());
        }

        Some(serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": servers,
        }))
    }

    pub fn write_opencode_global_config(&self) -> anyhow::Result<()> {
        if !self.backends.contains_key("opencode") || !loka_shared::backends::opencode_enabled() {
            return Ok(());
        }
        let Some(cfg) = self.opencode_config_json() else {
            return Ok(());
        };
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/agent".to_string());
        let dir = std::path::Path::new(&home).join(".config/opencode");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create opencode config dir: {}", dir.display()))?;
        let path = dir.join("opencode.json");
        let content = serde_json::to_string_pretty(&cfg)?;
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write opencode config: {}", path.display()))?;
        tracing::info!(path = %path.display(), server_count = self.mcp_servers.len(), "Wrote opencode MCP config");
        Ok(())
    }
}

pub fn load_skill_file(path: &Path) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    if let Some(rest) = content.strip_prefix("---") {
        if let Some(idx) = rest.find("---") {
            let frontmatter = &rest[..idx];
            let body = rest[idx + 3..].trim();

            #[derive(Deserialize)]
            struct SkillFrontmatter {
                #[serde(default)]
                description: Option<String>,
                #[serde(default)]
                command: Option<String>,
            }

            let fm: SkillFrontmatter = serde_yaml::from_str(frontmatter)?;

            return Ok(Skill {
                name,
                description: fm.description.unwrap_or_default(),
                command: fm.command,
                prompt: if body.is_empty() {
                    None
                } else {
                    Some(body.to_string())
                },
            });
        }
    }

    Ok(Skill {
        name,
        description: String::new(),
        command: None,
        prompt: Some(content),
    })
}
