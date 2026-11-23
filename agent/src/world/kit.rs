use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::warn;
use uuid::Uuid;

use super::Ctx;

pub struct Kit {
    pub servers: Map<String, Value>,
    pub skills_index: String,
    pub skill_files: Vec<PathBuf>,
    pub has_secret_refs: bool,
}

pub struct Wired {
    pub mcp_json: Option<String>,
    pub _registration: Option<crate::mcp_hub::Registration>,
    pub pids: crate::mcp_hub::Pids,
}

pub async fn tool_delivery(cli: &str, model: &str, granted: &BTreeMap<String, String>) -> super::tool_catalog::Delivery {
    let claude = match cli {
        "claude" => super::tool_catalog::ClaudeFacts {
            version: crate::claude::detect::detected().await.ok().and_then(|launch| launch.caps.version.clone()),
            first_party: crate::claude::detect::route_is_first_party(&crate::claude::detect::host_route_lookup(granted)),
        },
        _ => Default::default(),
    };
    super::tool_catalog::delivery_for(cli, model, &claude)
}

pub fn world_server(agent_id: Uuid, delivery: super::tool_catalog::Delivery) -> Map<String, Value> {
    world_server_for(agent_id, None, None, delivery)
}

pub fn world_server_for(agent_id: Uuid, quick: Option<Uuid>, aside: Option<Uuid>, delivery: super::tool_catalog::Delivery) -> Map<String, Value> {
    let mut servers = Map::new();
    if let Some(mut world) = super::mcp::config_json_for(agent_id, quick, aside, delivery)
        .and_then(|j| serde_json::from_str::<Value>(&j).ok())
        .and_then(|v| v.pointer("/mcpServers/world").cloned())
    {
        world["shared"] = Value::Bool(false);
        servers.insert("world".into(), world);
    }
    servers
}

pub fn without_duplicates(servers: Map<String, Value>) -> Map<String, Value> {
    let mut seen: Vec<Value> = Vec::new();
    let mut out = Map::new();
    for (name, definition) in servers {
        let mut comparable = definition.clone();
        if let Some(object) = comparable.as_object_mut() {
            object.remove("shared");
        }
        if seen.contains(&comparable) {
            continue;
        }
        seen.push(comparable);
        out.insert(name, definition);
    }
    out
}

pub async fn wire(mut servers: Map<String, Value>, owner: Option<Uuid>, agent: Uuid, cwd: &Path, tools: super::tool_catalog::Delivery) -> Result<Wired> {
    let pids: crate::mcp_hub::Pids = Default::default();
    if servers.is_empty() {
        return Ok(Wired { mcp_json: None, _registration: None, pids });
    }
    if let Some(workspaces) = std::env::var_os("WORKSPACES_DIR").filter(|dir| !dir.is_empty()) {
        let who = owner.unwrap_or(agent).to_string();
        crate::mcp_hub::pin_sign_in_dirs(&mut servers, &Path::new(&workspaces).join(".mcp-auth").join(who));
    }
    let (servers, registration) = match crate::mcp_hub::get() {
        Some(hub) => {
            let job = crate::mcp_hub::Job { isolation: crate::mcp_hub::isolation_for(owner, agent), agent, cwd, pids: pids.clone(), passthrough: &[], tools };
            let prepared = hub.prepare(job, &servers).await?;
            (prepared.servers, prepared.registration)
        }
        None => {
            let mut servers = servers;
            crate::mcp_hub::strip_shared(&mut servers);
            (servers, None)
        }
    };
    let mut root = Map::new();
    root.insert("mcpServers".into(), Value::Object(servers));
    Ok(Wired { mcp_json: Some(Value::Object(root).to_string()), _registration: registration, pids })
}

pub async fn for_agent(ctx: &Ctx, agent_id: Uuid) -> Result<Kit> {
    let mut servers = ctx
        .config
        .mcp_config_json()
        .and_then(|v| v.get("mcpServers").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    let mut has_secret_refs = false;
    let mut environment = None;
    for row in ctx.store.mcp_servers_for(agent_id).await? {
        let mut definition = loka_shared::environment::runnable_definition(row.definition);
        let secret_names = match loka_shared::environment::secret_reference_names(&definition) {
            Ok(names) => names,
            Err(error) => {
                warn!(%agent_id, server = %row.name, %error, "Skipping an MCP server with an unreadable definition");
                continue;
            }
        };
        if !secret_names.is_empty() {
            if row.scope == "global" {
                warn!(%agent_id, server = %row.name, "Skipping a global MCP server that uses environment secrets");
                continue;
            }
            if environment.is_none() {
                environment = Some(ctx.store.environment_for_agent(agent_id).await?);
            }
            if let Err(error) = resolve_definition(&row.name, &mut definition, environment.as_ref().expect("environment loaded")) {
                warn!(%agent_id, server = %row.name, %error, "Skipping an MCP server whose keys are not granted");
                continue;
            }
            has_secret_refs = true;
        }
        servers.insert(row.name, definition);
    }
    let servers = without_duplicates(servers);
    let (skills_index, skill_files) = skills_index(ctx, agent_id).await;
    Ok(Kit {
        servers,
        skills_index,
        skill_files,
        has_secret_refs,
    })
}

pub async fn for_review(ctx: &Ctx, agent_id: Uuid) -> Result<Kit> {
    let mut servers = ctx
        .config
        .mcp_config_json()
        .and_then(|value| value.get("mcpServers").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    for row in ctx.store.mcp_servers_for(agent_id).await? {
        let definition = loka_shared::environment::runnable_definition(row.definition);
        if loka_shared::environment::secret_reference_names(&definition).is_ok_and(|names| names.is_empty()) {
            servers.insert(row.name, definition);
        }
    }
    let (skills_index, skill_files) = skills_index(ctx, agent_id).await;
    Ok(Kit {
        servers: without_duplicates(servers),
        skills_index,
        skill_files,
        has_secret_refs: false,
    })
}

fn resolve_definition(
    name: &str,
    definition: &mut Value,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    let object = definition
        .as_object_mut()
        .ok_or_else(|| anyhow!("MCP server {name} definition must be an object"))?;
    for (field, value) in object {
        match field.as_str() {
            "env" | "headers" => {
                let values = value
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("MCP server {name} {field} must be an object"))?;
                for value in values.values_mut() {
                    let text = value.as_str().ok_or_else(|| {
                        anyhow!("MCP server {name} {field} values must be strings")
                    })?;
                    *value = Value::String(resolve_text(text, environment)?);
                }
            }
            _ if value_contains_secret_reference(value) => {
                bail!("MCP server {name} secret references are only allowed in env and headers")
            }
            _ => {}
        }
    }
    Ok(())
}

fn value_contains_secret_reference(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("${secret:"),
        Value::Array(values) => values.iter().any(value_contains_secret_reference),
        Value::Object(values) => values.iter().any(|(key, value)| {
            key.contains("${secret:") || value_contains_secret_reference(value)
        }),
        _ => false,
    }
}

fn resolve_text(text: &str, environment: &BTreeMap<String, String>) -> Result<String> {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find("${secret:") {
        result.push_str(&remaining[..start]);
        let reference = &remaining[start + 9..];
        let end = reference
            .find('}')
            .ok_or_else(|| anyhow!("invalid environment secret reference"))?;
        let name = &reference[..end];
        validate_reference_name(name)?;
        let value = environment
            .get(name)
            .ok_or_else(|| anyhow!("environment key {name} is not granted to this agent"))?;
        result.push_str(value);
        remaining = &reference[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}

fn validate_reference_name(name: &str) -> Result<()> {
    if !loka_shared::environment::valid_name(name) {
        bail!("invalid environment key {name}");
    }
    Ok(())
}

async fn skills_index(ctx: &Ctx, agent_id: Uuid) -> (String, Vec<PathBuf>) {
    let mut skills: Vec<(String, String, String)> = ctx
        .config
        .skills
        .iter()
        .map(|s| {
            let mut body = s.prompt.clone().unwrap_or_default();
            if let Some(cmd) = &s.command {
                body.push_str(&format!("\n\nCommand: `{cmd}`"));
            }
            (s.name.clone(), s.description.clone(), body)
        })
        .collect();
    match ctx.store.skills_for(agent_id).await {
        Ok(rows) => {
            for row in rows {
                skills.retain(|(n, _, _)| *n != row.name);
                skills.push((row.name, row.description, row.body));
            }
        }
        Err(e) => warn!(error = %e, "Could not load skills"),
    }
    if skills.is_empty() {
        return (String::new(), Vec::new());
    }
    let root = ctx.config.workspaces_dir.join("skills");
    let mut lines = Vec::new();
    let mut files = Vec::new();
    for (name, description, body) in skills {
        let safe: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let path = root.join(&safe).join("SKILL.md");
        if let Some(dir) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                warn!(error = %e, "Could not create skill dir");
                continue;
            }
        }
        let trimmed = body.trim_start();
        let frontmatter = trimmed
            .strip_prefix("---\n")
            .or_else(|| trimmed.strip_prefix("---\r\n"))
            .is_some_and(|rest| {
                rest.lines()
                    .any(|line| line.trim_end_matches('\r') == "---")
            });
        let content = if frontmatter {
            body
        } else {
            format!(
                "---\nname: {}\ndescription: {}\n---\n{body}\n",
                serde_json::to_string(&name).unwrap(),
                serde_json::to_string(&description).unwrap()
            )
        };
        let current = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        if current != content {
            if let Err(e) = tokio::fs::write(&path, content).await {
                warn!(error = %e, "Could not write skill");
                continue;
            }
        }
        lines.push(format!("- {name}: {description} ({})", path.display()));
        files.push(path);
    }
    let index = format!(
        "Skills you can use. When one fits the work, read its file first and follow it:\n{}",
        lines.join("\n")
    );
    (index, files)
}

#[cfg(test)]
mod tests {
    use super::resolve_definition;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn resolves_exact_and_embedded_references_once() {
        let mut definition = json!({
            "command": "server",
            "env": { "TOKEN": "${secret:TOKEN}", "AUTH": "Bearer ${secret:TOKEN}" },
            "headers": { "X-Key": "${secret:OTHER}" }
        });
        let environment = BTreeMap::from([
            ("TOKEN".to_string(), "tøken-${secret:OTHER}".to_string()),
            ("OTHER".to_string(), "value".to_string()),
        ]);
        resolve_definition("test", &mut definition, &environment).unwrap();
        assert_eq!(definition["env"]["TOKEN"], "tøken-${secret:OTHER}");
        assert_eq!(definition["env"]["AUTH"], "Bearer tøken-${secret:OTHER}");
        assert_eq!(definition["headers"]["X-Key"], "value");
    }

    #[test]
    fn rejects_missing_and_unsupported_references() {
        let mut definition = json!({ "command": "server", "env": { "TOKEN": "${secret:TOKEN}" } });
        assert!(resolve_definition("test", &mut definition, &BTreeMap::new()).is_err());
    }
}
