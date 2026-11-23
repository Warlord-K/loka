use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use loka_shared::access::{self, Role, Viewer};
use loka_shared::api_tokens::{Scope, TokenGrant};
use loka_shared::app_links;
use loka_shared::world::{AgentMessage, WorldAgent};
use serde_json::{json, Value};
use tracing::{error, info};
use uuid::Uuid;

use crate::auth::UserId;
use crate::AppState;

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const CALLS_PER_MINUTE: u32 = 120;
const MAX_WAIT_SECS: u64 = 120;
const DEFAULT_WAIT_SECS: u64 = 60;
const BODY_CHARS: usize = 4000;

const INSTRUCTIONS: &str = "Loka runs persistent coding agents owned by people. Use list_agents to find one, message_agent to talk to it (then wait_for_reply), and create_quest to hand it work that runs in the background: the agent forks a task session, works in a worktree, and opens a PR. Quests return ids at once; follow them with get_quest and quest_events. Agents you do not own treat you as a guest. Links in results open the Loka app.";

pub struct Principal {
    pub user: Uuid,
    pub role: Role,
    pub grant: TokenGrant,
    pub viewer: Viewer,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token.trim())
}

fn unauthorized(description: &str) -> Response {
    let challenge = format!("Bearer realm=\"world\", error=\"invalid_token\", error_description=\"{description}\"");
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": -32001, "message": description } })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

#[allow(clippy::result_large_err)]
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Principal, Response> {
    let Some(token) = bearer(headers) else {
        return Err(unauthorized("Send a Loka API token as Authorization: Bearer wld_..."));
    };
    let grant = match state.world.authenticate_api_token(token).await {
        Ok(Some(grant)) => grant,
        Ok(None) => return Err(unauthorized("The token is unknown or revoked")),
        Err(e) => {
            error!(error = %e, "mcp token lookup failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let org_role = match state.world.role_in_org(grant.user_id, Some(grant.org_id)).await {
        Ok(Some(role)) => role,
        Ok(None) => return Err(unauthorized("The token's organization no longer includes you")),
        Err(e) => {
            error!(error = %e, "mcp role lookup failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let role = grant.scope.cap(org_role.ladder());
    let viewer = Viewer { user: grant.user_id, org: Some(grant.org_id), role, org_role: Some(org_role), instance_admin: false };
    Ok(Principal { user: grant.user_id, role, grant, viewer })
}

fn windows() -> &'static Mutex<HashMap<Uuid, (Instant, u32)>> {
    static WINDOWS: OnceLock<Mutex<HashMap<Uuid, (Instant, u32)>>> = OnceLock::new();
    WINDOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn rate_limited(token: Uuid) -> Option<u64> {
    let mut windows = windows().lock().ok()?;
    let now = Instant::now();
    if windows.len() > 10_000 {
        windows.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
    }
    let entry = windows.entry(token).or_insert((now, 0));
    if now.duration_since(entry.0) >= Duration::from_secs(60) {
        *entry = (now, 0);
    }
    entry.1 += 1;
    (entry.1 > CALLS_PER_MINUTE).then(|| 60u64.saturating_sub(now.duration_since(entry.0).as_secs()).max(1))
}

const MAX_BATCH: usize = 20;

fn slow_down(wait: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": -32002, "message": format!("Slow down: {CALLS_PER_MINUTE} calls a minute per token. Retry in {wait} s.") } })),
    )
        .into_response();
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait));
    response
}

pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(p) => p,
        Err(response) => return response,
    };
    if let Some(wait) = rate_limited(principal.grant.token_id) {
        return slow_down(wait);
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_error(Value::Null, -32700, "Parse error")).into_response(),
    };
    let ctx = Ctx { state: &state, p: &principal };
    match message {
        Value::Array(items) if items.len() > MAX_BATCH => {
            Json(rpc_error(Value::Null, -32600, &format!("Batches hold at most {MAX_BATCH} calls"))).into_response()
        }
        Value::Array(items) if !items.is_empty() => {
            for _ in 1..items.len() {
                if let Some(wait) = rate_limited(principal.grant.token_id) {
                    return slow_down(wait);
                }
            }
            let mut out = Vec::new();
            for item in items {
                if let Some(reply) = ctx.dispatch(item).await {
                    out.push(reply);
                }
            }
            if out.is_empty() {
                StatusCode::ACCEPTED.into_response()
            } else {
                Json(Value::Array(out)).into_response()
            }
        }
        Value::Object(_) => match ctx.dispatch(message).await {
            Some(reply) => Json(reply).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
        _ => Json(rpc_error(Value::Null, -32600, "Invalid Request")).into_response(),
    }
}

pub async fn no_stream() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

struct ToolDef {
    name: &'static str,
    write: bool,
    description: &'static str,
    schema: Value,
}

fn s(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_agents",
            write: false,
            description: "List the Loka agents you can see: yours, public ones and, for admins, everyone's. Each has a status, what it is working on and a chat link.",
            schema: object(json!({ "mine": { "type": "boolean", "description": "Only agents you own" } }), &[]),
        },
        ToolDef {
            name: "agent_status",
            write: false,
            description: "One agent's status, current jobs, recent runs and any questions it is waiting on you to answer.",
            schema: object(json!({ "agent": s("Agent id, slug or name") }), &["agent"]),
        },
        ToolDef {
            name: "list_quests",
            write: false,
            description: "List quests (background tasks) you can see, newest activity first.",
            schema: object(
                json!({
                    "agent": s("Only quests assigned to this agent (id, slug or name)"),
                    "status": s("Only this status, for example in_progress, review, done, failed"),
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Default 20" }
                }),
                &[],
            ),
        },
        ToolDef {
            name: "get_quest",
            write: false,
            description: "A quest's status, result, PR, milestones and its active runs (pass a run id to stop_run).",
            schema: object(json!({ "quest": s("Quest id") }), &["quest"]),
        },
        ToolDef {
            name: "quest_events",
            write: false,
            description: "What happened on a quest: events and discussion. Pass the returned cursor as after to get only newer events.",
            schema: object(
                json!({ "quest": s("Quest id"), "after": { "type": "integer", "description": "Event cursor from a previous call" } }),
                &["quest"],
            ),
        },
        ToolDef {
            name: "usage",
            write: false,
            description: "Cost, tokens, run outcomes and failures for your agents over recent days.",
            schema: object(
                json!({ "agent": s("Only this agent of yours"), "days": { "type": "integer", "minimum": 1, "maximum": 90, "description": "Default 7" } }),
                &[],
            ),
        },
        ToolDef {
            name: "message_agent",
            write: true,
            description: "Send a chat message to an agent as you. Your own agents treat it as from their owner; other people's public agents answer you as a guest. Returns at once with a message id; call wait_for_reply for the answer.",
            schema: object(
                json!({ "agent": s("Agent id, slug or name"), "message": s("What to say"), "quest": s("Optional quest id this message is about") }),
                &["agent", "message"],
            ),
        },
        ToolDef {
            name: "wait_for_reply",
            write: true,
            description: "Wait for the agent's reply to a message you sent with message_agent. Returns as soon as it answers, or after the timeout with the agent's status so you can call again.",
            schema: object(
                json!({
                    "message": s("message_id returned by message_agent"),
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": MAX_WAIT_SECS, "description": "Default 60, at most 120" }
                }),
                &["message"],
            ),
        },
        ToolDef {
            name: "create_quest",
            write: true,
            description: "Hand one of your agents a quest. It starts in the background (a task session in a worktree, optional review, then a PR) and this returns the quest id at once. Follow it with get_quest and quest_events.",
            schema: object(
                json!({
                    "agent": s("One of your agents (id, slug or name)"),
                    "brief": s("What to do and what done looks like"),
                    "title": s("Short title, defaults to the first line of the brief"),
                    "repo": s("owner/repo, optional"),
                    "review": { "type": "boolean", "description": "Ask for an independent review before publishing" },
                    "wait_for_merge": { "type": "boolean", "description": "With a repo, keep the quest open until the PR merges. Default true when a repo is set" },
                    "time_limit_minutes": { "type": "integer", "minimum": 5, "maximum": 1440, "description": "Most minutes each run may take" }
                }),
                &["agent", "brief"],
            ),
        },
        ToolDef {
            name: "answer_ask",
            write: true,
            description: "Answer a question one of your agents asked you (see open_asks in agent_status).",
            schema: object(json!({ "ask": s("Id of the ask message"), "answer": s("Your answer, a listed choice or your own words") }), &["ask", "answer"]),
        },
        ToolDef {
            name: "stop_run",
            write: true,
            description: "Stop a running quest run of an agent you own, or withdraw a queued one so it never starts. mode run (default) stops or withdraws the whole run; step only interrupts the current step of a running run and lets the agent carry on.",
            schema: object(
                json!({ "run": s("Run id from get_quest or agent_status"), "mode": { "type": "string", "enum": ["run", "step"] } }),
                &["run"],
            ),
        },
    ]
}

fn annotations(def: &ToolDef) -> Value {
    json!({
        "readOnlyHint": !def.write,
        "destructiveHint": def.name == "stop_run",
        "openWorldHint": false,
    })
}

type ToolResult = Result<Value, String>;

fn fail(e: anyhow::Error) -> String {
    error!(error = %e, "mcp tool failed");
    "Loka hit an internal error. Try again in a moment.".into()
}

fn status_fail(status: StatusCode) -> String {
    match status {
        StatusCode::NOT_FOUND => "Not found, or hidden from you.".into(),
        StatusCode::FORBIDDEN => "You are not allowed to do that.".into(),
        StatusCode::CONFLICT => "That conflicts with what is happening right now. Check the status and try again.".into(),
        StatusCode::BAD_REQUEST => "Loka rejected those arguments.".into(),
        _ => "Loka hit an internal error. Try again in a moment.".into(),
    }
}

fn chat_url(agent: &WorldAgent) -> String {
    app_links::agent_url(&agent.slug)
}

fn message_url(id: Uuid) -> String {
    app_links::message_url(&id.to_string())
}

fn quest_url(id: Uuid) -> String {
    app_links::quest_url(id)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push_str(" [truncated]");
        out
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn need_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    arg_str(args, key).ok_or_else(|| format!("{key} is required"))
}

fn need_uuid(args: &Value, key: &str) -> Result<Uuid, String> {
    need_str(args, key)?.parse().map_err(|_| format!("{key} must be an id"))
}

#[derive(Clone, Copy, PartialEq)]
enum Need {
    See,
    Use,
    Manage,
}

struct Ctx<'a> {
    state: &'a AppState,
    p: &'a Principal,
}

impl Ctx<'_> {
    async fn dispatch(&self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            if message.get("result").is_some() || message.get("error").is_some() {
                return None;
            }
            return Some(rpc_error(id.unwrap_or(Value::Null), -32600, "Invalid Request"));
        };
        let id = id?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "world", "title": "Loka", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => rpc_result(id, json!({})),
            "tools/list" => {
                let tools: Vec<Value> = tool_defs()
                    .iter()
                    .filter(|d| self.may_call(d))
                    .map(|d| json!({ "name": d.name, "description": d.description, "inputSchema": d.schema, "annotations": annotations(d) }))
                    .collect();
                rpc_result(id, json!({ "tools": tools }))
            }
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(rpc_error(id, -32602, "tools/call needs a tool name"));
                };
                let defs = tool_defs();
                let Some(def) = defs.iter().find(|d| d.name == name) else {
                    return Some(rpc_error(id, -32602, &format!("Unknown tool: {name}")));
                };
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let outcome = if self.may_call(def) {
                    self.call(name, &args).await
                } else {
                    Err("This token is read only. Create a write token in Loka (Settings > API tokens) to use this tool.".into())
                };
                rpc_result(
                    id,
                    match outcome {
                        Ok(value) => json!({
                            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                            "structuredContent": value,
                            "isError": false,
                        }),
                        Err(message) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
                    },
                )
            }
            _ => rpc_error(id, -32601, &format!("Method not found: {method}")),
        })
    }

    fn may_call(&self, def: &ToolDef) -> bool {
        !def.write || self.p.grant.scope == Scope::Write
    }

    async fn call(&self, name: &str, args: &Value) -> ToolResult {
        match name {
            "list_agents" => self.list_agents(args).await,
            "agent_status" => self.agent_status(args).await,
            "list_quests" => self.list_quests(args).await,
            "get_quest" => self.get_quest(args).await,
            "quest_events" => self.quest_events(args).await,
            "usage" => self.usage(args).await,
            "message_agent" => self.message_agent(args).await,
            "wait_for_reply" => self.wait_for_reply(args).await,
            "create_quest" => self.create_quest(args).await,
            "answer_ask" => self.answer_ask(args).await,
            "stop_run" => self.stop_run(args).await,
            _ => Err(format!("Unknown tool: {name}")),
        }
    }

    fn sees(&self, agent: &WorldAgent) -> bool {
        self.p.grant.allows_agent(agent.id) && access::can_see(&self.p.viewer, agent)
    }

    async fn visible_agents(&self) -> Result<Vec<WorldAgent>, String> {
        Ok(self.state.world.list_agents_in(self.p.viewer.org).await.map_err(fail)?.into_iter().filter(|a| self.sees(a)).collect())
    }

    async fn agent(&self, wanted: &str, need: Need) -> Result<WorldAgent, String> {
        let wanted = wanted.trim_start_matches('@');
        let id = wanted.parse::<Uuid>().ok();
        let agent = self
            .visible_agents()
            .await?
            .into_iter()
            .find(|a| Some(a.id) == id || a.slug.eq_ignore_ascii_case(wanted) || a.name.eq_ignore_ascii_case(wanted))
            .ok_or_else(|| format!("No agent called {wanted} that this token can see. Call list_agents."))?;
        self.check(&agent, need)?;
        Ok(agent)
    }

    fn check(&self, agent: &WorldAgent, need: Need) -> Result<(), String> {
        let ok = match need {
            Need::See => true,
            Need::Use => access::can_use(&self.p.viewer, agent),
            Need::Manage => access::can_manage(&self.p.viewer, agent),
        };
        if ok {
            return Ok(());
        }
        Err(match need {
            Need::Manage => format!("Only the owner of {} can do that.", agent.name),
            _ if self.p.role < Role::Write => format!("Talking to {} needs a write token and write access to Loka.", agent.name),
            _ => format!("{} is private to its owner.", agent.name),
        })
    }

    async fn agent_by_id(&self, id: Uuid, need: Need) -> Result<WorldAgent, String> {
        let agent = self
            .state
            .world
            .get_agent(id)
            .await
            .map_err(fail)?
            .filter(|a| self.sees(a))
            .ok_or_else(|| "Not found, or hidden from this token.".to_string())?;
        self.check(&agent, need)?;
        Ok(agent)
    }

    async fn refuse_spend(&self, agent: &WorldAgent) -> Result<(), String> {
        let spent = self.state.world.spent_today(&[agent.id]).await.map_err(fail)?.first().map_or(0.0, |(_, spent)| *spent);
        match loka_shared::remote::spend_refusal(agent, spent) {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    fn agent_brief(&self, agent: &WorldAgent) -> Value {
        json!({
            "id": agent.id,
            "name": agent.name,
            "slug": agent.slug,
            "emoji": agent.avatar_emoji,
            "cli": agent.cli,
            "model": agent.model,
            "status": agent.status,
            "status_detail": agent.status_detail,
            "paused": agent.paused_reason,
            "main_agent": agent.is_main,
            "yours": agent.owner_user_id == Some(self.p.user),
            "visibility": agent.visibility,
            "tags": agent.tags,
            "expertise": agent.expertise,
            "jobs": agent.jobs,
            "chat_url": chat_url(agent),
        })
    }

    fn message_view(&self, m: &AgentMessage) -> Value {
        json!({
            "id": m.id,
            "author": m.author_kind,
            "body": clip(&m.body, BODY_CHARS),
            "at": m.created_at,
            "ask": (m.meta.get("kind").and_then(Value::as_str) == Some("ask")).then(|| m.meta.get("choices").cloned().unwrap_or(json!([]))),
            "url": message_url(m.id),
        })
    }

    async fn list_agents(&self, args: &Value) -> ToolResult {
        let mine = args.get("mine").and_then(Value::as_bool).unwrap_or(false);
        let agents: Vec<Value> = self
            .visible_agents()
            .await?
            .iter()
            .filter(|a| !mine || a.owner_user_id == Some(self.p.user))
            .map(|a| self.agent_brief(a))
            .collect();
        Ok(json!({ "agents": agents }))
    }

    async fn agent_status(&self, args: &Value) -> ToolResult {
        let agent = self.agent(need_str(args, "agent")?, Need::See).await?;
        let mut out = self.agent_brief(&agent);
        if self.check(&agent, Need::Use).is_ok() {
            let manage = self.check(&agent, Need::Manage).is_ok();
            let runs: Vec<Value> = self
                .state
                .world
                .list_runs(agent.id, 8)
                .await
                .map_err(fail)?
                .iter()
                .filter(|r| (r.kind != "turn" || r.status != "completed") && (manage || !loka_shared::guests::CHAT_RUNS.contains(&r.kind.as_str())))
                .map(|r| {
                    json!({
                        "id": r.id,
                        "kind": r.kind,
                        "status": r.status,
                        "quest": r.task_id,
                        "started_at": r.started_at,
                        "finished_at": r.finished_at,
                        "cost_usd": r.cost_usd,
                        "error": r.error.as_deref().map(|e| clip(e, 600)),
                    })
                })
                .collect();
            out["recent_runs"] = json!(runs);
        }
        if agent.owner_user_id == Some(self.p.user) {
            let asks: Vec<Value> = self.state.world.open_asks(agent.id, 10).await.map_err(fail)?.iter().map(|m| self.message_view(m)).collect();
            out["open_asks"] = json!(asks);
            out["spent_today_usd"] = json!(self.state.world.spent_today(&[agent.id]).await.map_err(fail)?.first().map_or(0.0, |(_, spent)| *spent));
            out["daily_budget_usd"] = json!(agent.daily_budget_usd);
        }
        Ok(out)
    }

    async fn quest(&self, id: Uuid) -> Result<loka_shared::models::AgentTask, String> {
        let task = crate::routes::world::quest_task(self.state, &self.p.viewer, id).await.map_err(status_fail)?;
        match task.assigned_agent_id {
            Some(agent) if !self.p.grant.allows_agent(agent) => Err("Not found, or hidden from this token.".into()),
            None if self.p.grant.agent_ids.is_some() => Err("Not found, or hidden from this token.".into()),
            _ => Ok(task),
        }
    }

    async fn list_quests(&self, args: &Value) -> ToolResult {
        let agents = self.visible_agents().await?;
        let only_agent = match arg_str(args, "agent") {
            Some(wanted) => Some(self.agent(wanted, Need::See).await?.id),
            None => None,
        };
        let status = arg_str(args, "status");
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
        let quests: Vec<Value> = self
            .state
            .world
            .quests(self.p.user, self.p.viewer.org(), 300)
            .await
            .map_err(fail)?
            .into_iter()
            .filter(|t| match t.assigned_agent_id {
                Some(a) => agents.iter().any(|v| v.id == a),
                None => self.p.role >= Role::Write && self.p.grant.agent_ids.is_none(),
            })
            .filter(|t| only_agent.is_none_or(|a| t.assigned_agent_id == Some(a)))
            .filter(|t| status.is_none_or(|s| t.status == s))
            .take(limit)
            .map(|t| {
                json!({
                    "id": t.id,
                    "title": t.title,
                    "status": t.status,
                    "repo": t.repo,
                    "pr_url": t.pr_url,
                    "agent": t.assigned_agent_id.and_then(|a| agents.iter().find(|v| v.id == a)).map(|v| v.name.clone()),
                    "updated_at": t.updated_at,
                    "url": quest_url(t.id),
                })
            })
            .collect();
        Ok(json!({ "quests": quests }))
    }

    async fn room(&self, task: &loka_shared::models::AgentTask, after: Option<i64>) -> Result<loka_shared::rooms::QuestRoom, String> {
        let canonical = self.state.world.room_of(task.id).await.map_err(fail)?;
        let task = if canonical == task.id { task.clone() } else { self.quest(canonical).await? };
        let usable: Vec<Uuid> = self
            .state
            .world
            .list_agents_in(self.p.viewer.org)
            .await
            .map_err(fail)?
            .iter()
            .filter(|a| self.p.grant.allows_agent(a.id) && access::can_use(&self.p.viewer, a))
            .map(|a| a.id)
            .collect();
        let since = task.created_at.unwrap_or_else(Utc::now) - chrono::Duration::minutes(5);
        let mut room = self.state.world.quest_room(task.id, since, &usable, self.p.user, after, None, None).await.map_err(fail)?;
        room.members = self.state.world.room_members_for(task.id, &usable).await.map_err(fail)?;
        let parts = self.state.world.quest_parts(task.id).await.map_err(fail)?;
        let visible_parts: Vec<Uuid> = parts
            .iter()
            .filter(|part| part.assigned_agent_id.is_some_and(|agent| usable.contains(&agent)))
            .map(|part| part.id)
            .collect();
        let open_messages = self
            .state
            .world
            .open_room_messages_for(&room.messages, &self.p.viewer, Some(&visible_parts))
            .await
            .map_err(fail)?;
        room.messages.retain(|message| open_messages.contains(&message.id));
        let owned = self.state.world.owned_agent_ids(self.p.user).await.map_err(fail)?;
        let user = self.p.user;
        room.events.retain(|e| {
            let scoped = loka_shared::rooms::scoped_event(e);
            let part = loka_shared::rooms::event_part(e, task.id);
            !loka_shared::slack::hidden_from(e, user)
                && !loka_shared::loka::hidden_from(e, user)
                && !loka_shared::shell::hidden_from(e, &owned)
                && part.is_none_or(|part| visible_parts.contains(&part))
                && (scoped || !loka_shared::event_scope::private_from(e, user, self.p.viewer.role, &owned))
        });
        Ok(room)
    }

    async fn get_quest(&self, args: &Value) -> ToolResult {
        let task = self.quest(need_uuid(args, "quest")?).await?;
        let room = self.room(&task, None).await?;
        let milestones = loka_shared::milestones::MilestoneStore::new(self.state.world.pool().clone())
            .for_task(task.id)
            .await
            .map_err(fail)?;
        let active: Vec<Value> = room
            .runs
            .iter()
            .filter(|r| matches!(r.run.status.as_str(), "queued" | "running"))
            .map(|r| json!({ "id": r.run.id, "kind": r.run.kind, "status": r.run.status, "waiting_for": r.waiting_for, "started_at": r.run.started_at }))
            .collect();
        let cost: f64 = room.runs.iter().map(|r| r.run.cost_usd).sum();
        Ok(json!({
            "id": task.id,
            "title": task.title,
            "brief": task.description.as_deref().map(|d| clip(d, BODY_CHARS)),
            "status": task.status,
            "repo": task.repo,
            "branch": task.branch,
            "pr_url": task.pr_url,
            "agent_id": task.assigned_agent_id,
            "result": task.result_content.as_deref().map(|r| clip(r, BODY_CHARS)),
            "error": task.error_message,
            "milestones": milestones.iter().map(|m| json!({ "title": m.title, "kind": m.kind, "status": m.status })).collect::<Vec<_>>(),
            "active_runs": active,
            "runs": room.runs.len(),
            "cost_usd": cost,
            "created_at": task.created_at,
            "updated_at": task.updated_at,
            "url": quest_url(task.id),
        }))
    }

    async fn quest_events(&self, args: &Value) -> ToolResult {
        let task = self.quest(need_uuid(args, "quest")?).await?;
        let after = args.get("after").and_then(Value::as_i64);
        let room = self.room(&task, after).await?;
        let events: Vec<Value> = room
            .events
            .iter()
            .map(|e| json!({ "seq": e.seq, "type": e.event_type, "agent_id": e.agent_id, "run_id": e.run_id, "at": e.created_at, "payload": clip_payload(&e.payload) }))
            .collect();
        let messages: Vec<Value> = if after.is_none() { room.messages.iter().map(|m| self.message_view(m)).collect() } else { Vec::new() };
        let cursor = room.events.iter().map(|e| e.seq).max().unwrap_or(room.cursor).max(after.unwrap_or(0));
        Ok(json!({ "quest": task.id, "status": task.status, "events": events, "messages": messages, "cursor": cursor, "url": quest_url(task.id) }))
    }

    async fn usage(&self, args: &Value) -> ToolResult {
        let agent = match arg_str(args, "agent") {
            Some(wanted) => {
                let agent = self.agent(wanted, Need::See).await?;
                if agent.owner_user_id != Some(self.p.user) {
                    return Err(format!("Usage is only shown for your own agents, and {} is not yours.", agent.name));
                }
                Some(agent.id)
            }
            None => None,
        };
        let days = args.get("days").and_then(Value::as_i64).unwrap_or(7).clamp(1, loka_shared::usage::MAX_DAYS as i64) as i32;
        let mut report = self.state.world.usage_report(self.p.user, agent, days).await.map_err(fail)?;
        if let Some(ids) = &self.p.grant.agent_ids {
            report.rows.retain(|r| ids.contains(&r.agent_id));
        }
        Ok(json!({ "summary": loka_shared::usage::render(&report), "report": report }))
    }

    async fn message_agent(&self, args: &Value) -> ToolResult {
        let agent = self.agent(need_str(args, "agent")?, Need::Use).await?;
        let text = need_str(args, "message")?;
        if text.starts_with('!') {
            return Err("Shell commands (messages starting with !) only run from the Loka app.".into());
        }
        let quest = match arg_str(args, "quest") {
            Some(q) => Some(self.quest(q.parse().map_err(|_| "quest must be an id".to_string())?).await?.id),
            None => None,
        };
        self.refuse_spend(&agent).await?;
        let body = serde_json::from_value(json!({ "body": text, "task_id": quest })).map_err(|e| fail(e.into()))?;
        let (_, Json(msg)) = crate::routes::world::post_message(
            State(self.state.clone()),
            Extension(UserId(self.p.user)),
            Extension(self.p.role),
            Extension(self.p.viewer.clone()),
            Path(agent.id),
            Json(body),
        )
        .await
        .map_err(status_fail)?;
        Ok(json!({
            "message_id": msg.id,
            "agent": agent.name,
            "as": if agent.owner_user_id == Some(self.p.user) { "owner" } else { "guest" },
            "chat_url": chat_url(&agent),
            "message_url": message_url(msg.id),
            "next": "Call wait_for_reply with this message_id.",
        }))
    }

    async fn wait_for_reply(&self, args: &Value) -> ToolResult {
        let id = need_uuid(args, "message")?;
        let timeout = args.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(DEFAULT_WAIT_SECS).clamp(1, MAX_WAIT_SECS);
        let sent = self
            .state
            .world
            .find_message(id)
            .await
            .map_err(fail)?
            .filter(|m| m.author_user_id == Some(self.p.user))
            .ok_or_else(|| "No message of yours with that id.".to_string())?;
        let agent = self.agent_by_id(sent.agent_id, Need::Use).await?;
        let mut events = self.state.world_tx.subscribe();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        loop {
            let replies = self.replies(&agent, sent.created_at).await?;
            if !replies.is_empty() {
                return Ok(json!({ "replied": true, "replies": replies, "chat_url": chat_url(&agent) }));
            }
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Err(_) => break,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => tokio::time::sleep_until(deadline).await,
                Ok(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        let replies = self.replies(&agent, sent.created_at).await?;
        if !replies.is_empty() {
            return Ok(json!({ "replied": true, "replies": replies, "chat_url": chat_url(&agent) }));
        }
        let fresh = self.state.world.get_agent(agent.id).await.map_err(fail)?.unwrap_or(agent);
        Ok(json!({
            "replied": false,
            "status": fresh.status,
            "status_detail": fresh.status_detail,
            "jobs": fresh.jobs,
            "next": "No reply yet. Call wait_for_reply again with the same message id.",
            "chat_url": chat_url(&fresh),
        }))
    }

    async fn replies(&self, agent: &WorldAgent, after: DateTime<Utc>) -> Result<Vec<Value>, String> {
        let owner = agent.owner_user_id == Some(self.p.user);
        Ok(self
            .state
            .world
            .agent_replies_after(agent.id, self.p.user, after, 20)
            .await
            .map_err(fail)?
            .iter()
            .filter(|m| owner || (m.meta.get("kind").and_then(Value::as_str) != Some("shell") && m.meta.get("shell_share").is_none()))
            .map(|m| self.message_view(m))
            .collect())
    }

    async fn create_quest(&self, args: &Value) -> ToolResult {
        let agent = self.agent(need_str(args, "agent")?, Need::Manage).await?;
        let brief = need_str(args, "brief")?;
        let title: String = match arg_str(args, "title") {
            Some(t) => t.chars().take(120).collect(),
            None => brief.lines().find(|l| !l.trim().is_empty()).unwrap_or(brief).trim().chars().take(80).collect(),
        };
        let repo = arg_str(args, "repo");
        let review = args.get("review").and_then(Value::as_bool).unwrap_or(false);
        let wait_for_merge = args.get("wait_for_merge").and_then(Value::as_bool).unwrap_or(repo.is_some());
        let time_limit = args.get("time_limit_minutes").and_then(Value::as_i64);
        if time_limit.is_some_and(|m| !(loka_shared::world::WorldStore::MIN_TIME_LIMIT_MINUTES..=loka_shared::world::WorldStore::MAX_TIME_LIMIT_MINUTES).contains(&m)) {
            return Err("time_limit_minutes must be between 5 and 1440.".into());
        }
        self.refuse_spend(&agent).await?;
        let mut description = brief.to_string();
        if review {
            description.push_str("\n\nRun this with an independent review before publishing.");
        }
        let body = serde_json::from_value(json!({
            "title": title,
            "description": description,
            "repo": repo,
            "assigned_agent_id": agent.id,
            "time_limit_minutes": time_limit,
            "wait_for_merge": wait_for_merge,
        }))
        .map_err(|e| fail(e.into()))?;
        let (_, Json(task)) = crate::routes::tasks::create(State(self.state.clone()), Extension(UserId(self.p.user)), Extension(self.p.viewer.clone()), Json(body))
            .await
            .map_err(status_fail)?;
        Ok(json!({
            "quest_id": task.id,
            "title": task.title,
            "status": task.status,
            "agent": agent.name,
            "url": quest_url(task.id),
            "next": "The agent picks it up on its own. Follow it with get_quest or quest_events.",
        }))
    }

    async fn answer_ask(&self, args: &Value) -> ToolResult {
        let id = need_uuid(args, "ask")?;
        let answer = need_str(args, "answer")?;
        let ask = self
            .state
            .world
            .find_message(id)
            .await
            .map_err(fail)?
            .filter(|m| m.author_kind == "agent" && m.meta.get("kind").and_then(Value::as_str) == Some("ask"))
            .ok_or_else(|| "No ask with that id.".to_string())?;
        let agent = self.agent_by_id(ask.agent_id, Need::Use).await?;
        if agent.owner_user_id != Some(self.p.user) {
            return Err(format!("Only the owner of {} can answer its questions.", agent.name));
        }
        self.refuse_spend(&agent).await?;
        let body = serde_json::from_value(json!({ "body": answer, "answers": ask.id })).map_err(|e| fail(e.into()))?;
        let (_, Json(msg)) = crate::routes::world::post_message(
            State(self.state.clone()),
            Extension(UserId(self.p.user)),
            Extension(self.p.role),
            Extension(self.p.viewer.clone()),
            Path(agent.id),
            Json(body),
        )
        .await
        .map_err(status_fail)?;
        Ok(json!({ "message_id": msg.id, "agent": agent.name, "chat_url": chat_url(&agent), "next": "Call wait_for_reply with this message_id to see what it does next." }))
    }

    async fn stop_run(&self, args: &Value) -> ToolResult {
        let id = need_uuid(args, "run")?;
        let agent_id = self.state.world.run_agent_id(id).await.map_err(fail)?.ok_or_else(|| "No run with that id.".to_string())?;
        let agent = self.agent_by_id(agent_id, Need::Manage).await?;
        let mode = match arg_str(args, "mode") {
            Some("step") => "step",
            Some("run") | None => "run",
            Some(_) => return Err("mode must be run or step".into()),
        };
        let body = serde_json::from_value(json!({ "mode": mode })).map_err(|e| fail(e.into()))?;
        let (_, Json(outcome)) = crate::routes::world::stop_run(State(self.state.clone()), Extension(UserId(self.p.user)), Path(id), Json(body))
            .await
            .map_err(|status| match status {
                StatusCode::CONFLICT => "That run is neither running nor queued (chat turns cannot be stopped, and only running runs stop by step).".to_string(),
                other => status_fail(other),
            })?;
        info!(run = %id, user = %self.p.user, "run stopped over mcp");
        Ok(json!({ "stopped": true, "mode": mode, "agent": agent.name, "detail": outcome }))
    }
}

fn clip_payload(payload: &Value) -> Value {
    match payload {
        Value::String(s) => Value::String(clip(s, 1200)),
        Value::Array(items) => Value::Array(items.iter().take(20).map(clip_payload).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), clip_payload(v))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tokens_see_only_read_tools() {
        let read: Vec<&str> = tool_defs().iter().filter(|d| !d.write).map(|d| d.name).collect();
        assert_eq!(read, ["list_agents", "agent_status", "list_quests", "get_quest", "quest_events", "usage"]);
        for def in tool_defs() {
            assert_eq!(def.schema["type"], "object");
            assert!(!def.description.contains('\u{2014}'));
        }
    }

    #[test]
    fn bearer_parsing() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer(&headers), None);
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("bearer wld_abc"));
        assert_eq!(bearer(&headers), Some("wld_abc"));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&headers), None);
    }

    #[test]
    fn rate_limit_window() {
        let token = Uuid::new_v4();
        for _ in 0..CALLS_PER_MINUTE {
            assert!(rate_limited(token).is_none());
        }
        assert!(rate_limited(token).is_some());
        assert!(rate_limited(Uuid::new_v4()).is_none());
    }

    #[test]
    fn unauthorized_has_challenge() {
        let response = unauthorized("nope");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let challenge = response.headers().get(header::WWW_AUTHENTICATE).unwrap().to_str().unwrap();
        assert!(challenge.starts_with("Bearer realm=\"world\""));
    }

    #[test]
    fn clips_long_payloads() {
        let long = "x".repeat(5000);
        let clipped = clip_payload(&json!({ "a": long, "b": [1, 2] }));
        assert!(clipped["a"].as_str().unwrap().len() < 1300);
        assert_eq!(clipped["b"], json!([1, 2]));
    }
}
