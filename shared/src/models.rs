use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    Planning,
    PlanReview,
    InProgress,
    Review,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Planning => "planning",
            Self::PlanReview => "plan_review",
            Self::InProgress => "in_progress",
            Self::Review => "review",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug)]
pub struct ParseTaskStatusError;

impl std::str::FromStr for TaskStatus {
    type Err = ParseTaskStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo" => Ok(Self::Todo),
            "planning" => Ok(Self::Planning),
            "plan_review" => Ok(Self::PlanReview),
            "in_progress" => Ok(Self::InProgress),
            "review" => Ok(Self::Review),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(ParseTaskStatusError),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Code,
    Doc,
    Research,
    Ops,
}

impl TaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Doc => "doc",
            Self::Research => "research",
            Self::Ops => "ops",
        }
    }
}

#[derive(Debug)]
pub struct ParseTaskKindError;

impl std::str::FromStr for TaskKind {
    type Err = ParseTaskKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "code" => Ok(Self::Code),
            "doc" => Ok(Self::Doc),
            "research" => Ok(Self::Research),
            "ops" => Ok(Self::Ops),
            _ => Err(ParseTaskKindError),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutputTarget {
    Pr,
    LinearComment,
    LinearDoc,
    SlackMessage,
    Notification,
    #[default]
    None,
}

impl TaskOutputTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::LinearComment => "linear_comment",
            Self::LinearDoc => "linear_doc",
            Self::SlackMessage => "slack_message",
            Self::Notification => "notification",
            Self::None => "none",
        }
    }
}

#[derive(Debug)]
pub struct ParseTaskOutputTargetError;

impl std::str::FromStr for TaskOutputTarget {
    type Err = ParseTaskOutputTargetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pr" => Ok(Self::Pr),
            "linear_comment" => Ok(Self::LinearComment),
            "linear_doc" => Ok(Self::LinearDoc),
            "slack_message" => Ok(Self::SlackMessage),
            "notification" => Ok(Self::Notification),
            "none" => Ok(Self::None),
            _ => Err(ParseTaskOutputTargetError),
        }
    }
}

fn default_orchestrator_max_turns() -> u32 {
    12
}

fn default_orchestrator_max_actions_per_turn() -> usize {
    10
}

fn default_orchestrator_max_subtasks() -> usize {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestratorConfig {
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_orchestrator_max_turns")]
    pub max_turns: u32,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default = "default_orchestrator_max_actions_per_turn")]
    pub max_actions_per_turn: usize,
    #[serde(default = "default_orchestrator_max_subtasks")]
    pub max_subtasks: usize,
    #[serde(default)]
    pub accepts_external_replies: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            allowed_tools: Vec::new(),
            max_turns: default_orchestrator_max_turns(),
            deadline: None,
            max_actions_per_turn: default_orchestrator_max_actions_per_turn(),
            max_subtasks: default_orchestrator_max_subtasks(),
            accepts_external_replies: false,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: Uuid,
    pub user_id: Uuid,
    pub assignee_user_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub repo: Option<String>,
    pub kind: String,
    pub output_target: Option<String>,
    pub output_ref: Option<String>,
    pub source: Option<String>,
    pub parent_task_id: Option<Uuid>,
    pub target_branch: Option<String>,
    pub status: String,
    pub priority: String,
    pub position: f32,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub plan_content: Option<String>,
    pub result_content: Option<String>,
    pub feedback: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub agent_session_id: Option<String>,
    pub error_message: Option<String>,
    pub cli: Option<String>,
    pub model: Option<String>,
    pub task_number: Option<i32>,
    pub cost_usd: f64,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
    pub external_url: Option<String>,
    pub slack_channel: Option<String>,
    pub slack_thread_ts: Option<String>,
    pub assigned_agent_id: Option<Uuid>,
    pub planner_agent_id: Option<Uuid>,
    pub implementer_agent_id: Option<Uuid>,
    pub reviewer_agent_id: Option<Uuid>,
    pub orchestrator: Option<serde_json::Value>,
    pub policy_matches: Option<serde_json::Value>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    #[serde(default)]
    pub time_limit_secs: Option<i32>,
    #[sqlx(default)]
    #[serde(default)]
    pub org_id: Option<Uuid>,
    #[sqlx(default)]
    #[serde(default)]
    pub past_prs: Option<serde_json::Value>,
}

impl AgentTask {
    pub fn status_enum(&self) -> Option<TaskStatus> {
        self.status.parse::<TaskStatus>().ok()
    }

    pub fn target_branch_or_default(&self) -> &str {
        self.target_branch.as_deref().unwrap_or("main")
    }

    pub fn kind_enum(&self) -> TaskKind {
        self.kind.parse::<TaskKind>().unwrap_or_default()
    }

    pub fn output_target_enum(&self) -> TaskOutputTarget {
        self.output_target
            .as_deref()
            .unwrap_or(TaskOutputTarget::None.as_str())
            .parse::<TaskOutputTarget>()
            .unwrap_or_default()
    }

    pub fn orchestrator_config(&self) -> Option<OrchestratorConfig> {
        self.orchestrator
            .clone()
            .and_then(|raw| serde_json::from_value::<OrchestratorConfig>(raw).ok())
    }

    pub fn agent_branch_name(&self) -> String {
        let ticket = [
            self.title.as_str(),
            self.description.as_deref().unwrap_or(""),
        ]
        .into_iter()
        .flat_map(|text| text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .enumerate()
        .find(|(index, word)| {
            word.split_once('-').is_some_and(|(team, number)| {
                team.len() >= 2
                    && team
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || (*index == 0 && c.is_ascii_lowercase()))
                    && number.len() >= 2
                    && number.chars().all(|c| c.is_ascii_digit())
            })
        })
        .map(|(_, word)| word);
        let title = ticket.map_or(self.title.clone(), |ticket| self.title.replace(ticket, " "));
        let slug = slug::slugify(title)
            .split('-')
            .skip_while(|word| *word == "retry")
            .take(5)
            .collect::<Vec<_>>()
            .join("-");
        match ticket {
            Some(ticket) if !slug.is_empty() => format!("{}/{slug}", ticket.to_lowercase()),
            Some(ticket) => ticket.to_lowercase(),
            None if slug.is_empty() => format!("task-{}", &self.id.to_string()[..8]),
            None => format!("{slug}-{}", &self.id.to_string()[..8]),
        }
    }

    pub fn repos(&self) -> Vec<&str> {
        match &self.repo {
            Some(r) => r.split(',').map(|s| s.trim()).collect(),
            None => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentLog {
    pub id: Uuid,
    pub task_id: Uuid,
    pub phase: String,
    pub message: String,
    pub log_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct TaskAttachment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
    pub size_bytes: i64,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub prompt: String,
    pub repos: Option<String>,
    pub cron_expression: Option<String>,
    pub run_at: Option<DateTime<Utc>>,
    pub skills: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<String>>,
    pub max_open_tasks: i32,
    pub task_prefix: Option<String>,
    pub priority: String,
    pub cli: Option<String>,
    pub model: Option<String>,
    pub enabled: bool,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Schedule {
    pub fn repos(&self) -> Vec<&str> {
        match &self.repos {
            Some(r) => r.split(',').map(|s| s.trim()).collect(),
            None => Vec::new(),
        }
    }

    pub fn is_one_shot(&self) -> bool {
        self.run_at.is_some()
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ScheduledRun {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub summary_markdown: Option<String>,
    pub tasks_created: Option<Vec<Uuid>>,
    pub cost_usd: f64,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ScheduledRunLog {
    pub id: Uuid,
    pub run_id: Uuid,
    pub level: String,
    pub message: String,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Policy {
    pub id: Uuid,
    pub name: String,
    pub repo_pattern: String,
    pub path_glob: String,
    pub message: String,
    pub severity: String,
    pub enabled: bool,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub avatar_emoji: String,
    pub cli: String,
    pub model: String,
    pub system_prompt_addendum: Option<String>,
    pub paused_reason: Option<String>,
    pub is_default: bool,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RepoProfile {
    pub id: Uuid,
    pub user_id: Uuid,
    pub repo: String,
    pub branch: String,
    pub status: String,
    pub summary: Option<String>,
    pub profile_json: Option<serde_json::Value>,
    pub last_onboarded_at: Option<DateTime<Utc>>,
    pub last_commit_sha: Option<String>,
    pub error_message: Option<String>,
    pub cost_usd: f64,
    pub sync_issues: bool,
    pub webhook_status: String,
    pub webhook_error: Option<String>,
    pub webhook_url: Option<String>,
    pub review_prs: bool,
    pub review_agent_id: Option<Uuid>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PrReviewLog {
    pub id: Uuid,
    pub review_id: Uuid,
    pub phase: String,
    pub message: String,
    pub log_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PrReview {
    pub id: Uuid,
    pub repo: String,
    pub pr_number: i32,
    pub pr_url: Option<String>,
    pub head_sha: String,
    pub author: Option<String>,
    pub reviewer_agent_id: Option<Uuid>,
    pub status: String,
    pub comments_posted: i32,
    pub cost_usd: f64,
    pub error_message: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PrReviewFinding {
    pub id: Uuid,
    pub review_id: Uuid,
    pub path: String,
    pub line: i32,
    pub start_line: Option<i32>,
    pub side: String,
    pub body: String,
    pub posted: bool,
    pub skip_reason: Option<String>,
    pub severity: String,
    pub created_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::{TaskKind, TaskOutputTarget};

    #[test]
    fn parses_task_kind_values() {
        assert_eq!("code".parse::<TaskKind>().unwrap().as_str(), "code");
        assert_eq!("doc".parse::<TaskKind>().unwrap().as_str(), "doc");
        assert_eq!("research".parse::<TaskKind>().unwrap().as_str(), "research");
        assert_eq!("ops".parse::<TaskKind>().unwrap().as_str(), "ops");
    }

    #[test]
    fn rejects_invalid_task_kind_values() {
        assert!("invalid".parse::<TaskKind>().is_err());
    }

    #[test]
    fn parses_output_target_values() {
        assert_eq!("pr".parse::<TaskOutputTarget>().unwrap().as_str(), "pr");
        assert_eq!(
            "linear_comment"
                .parse::<TaskOutputTarget>()
                .unwrap()
                .as_str(),
            "linear_comment"
        );
        assert_eq!(
            "linear_doc".parse::<TaskOutputTarget>().unwrap().as_str(),
            "linear_doc"
        );
        assert_eq!(
            "slack_message"
                .parse::<TaskOutputTarget>()
                .unwrap()
                .as_str(),
            "slack_message"
        );
        assert_eq!(
            "notification".parse::<TaskOutputTarget>().unwrap().as_str(),
            "notification"
        );
        assert_eq!("none".parse::<TaskOutputTarget>().unwrap().as_str(), "none");
    }

    #[test]
    fn rejects_invalid_output_target_values() {
        assert!("email".parse::<TaskOutputTarget>().is_err());
    }
}
