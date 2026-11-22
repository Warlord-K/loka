use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::access::Role;
use crate::world::{NewEvent, WorldStore};

pub const MAX_BODY: usize = 512 * 1024;
pub const MAX_TAGS: usize = 12;
const MAX_TITLE: usize = 200;
const MAX_FOLDER_DEPTH: usize = 6;
const KEEP_REVISIONS: i64 = 100;
const MERGE_SECS: i64 = 300;
const QUIET_CHARS: usize = 600;
const VISIBLE: &str = "(r.org_id = $9 AND (r.owner_user_id = $1 OR r.visibility = 'public'))";
pub(crate) const BRIEF_COLUMNS: &str = "r.id, r.owner_user_id, r.author_agent_id, r.author_user_id, r.title, r.slug, r.tags, r.folder, r.pinned,
  r.visibility, r.revision, r.task_id, r.created_at, r.updated_at, left(r.body, 280) AS excerpt, octet_length(r.body) AS size, r.kind,
  (SELECT COUNT(*) FROM world_canvas_marks m WHERE m.report_id = r.id AND m.status = 'open') AS open_marks,
  CASE WHEN r.kind = 'canvas' AND jsonb_typeof(r.canvas->'items') = 'array' THEN jsonb_array_length(r.canvas->'items') ELSE 0 END AS item_count, r.practice";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Report {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<Uuid>,
    pub title: String,
    pub slug: String,
    pub body: String,
    pub tags: Vec<String>,
    pub folder: String,
    pub pinned: bool,
    pub visibility: String,
    pub links: Vec<Uuid>,
    pub revision: i32,
    pub task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(default)]
    pub org_id: Option<Uuid>,
    #[sqlx(default)]
    pub kind: String,
    #[sqlx(default)]
    pub canvas: Option<serde_json::Value>,
    #[sqlx(default)]
    pub practice: Option<serde_json::Value>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ReportBrief {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<Uuid>,
    pub title: String,
    pub slug: String,
    pub tags: Vec<String>,
    pub folder: String,
    pub pinned: bool,
    pub visibility: String,
    pub revision: i32,
    pub task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub excerpt: String,
    pub size: i32,
    pub kind: String,
    pub open_marks: i64,
    pub item_count: i32,
    pub practice: Option<serde_json::Value>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct RevisionBrief {
    pub revision: i32,
    pub title: String,
    pub folder: String,
    pub tags: Vec<String>,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub size: i32,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Revision {
    pub revision: i32,
    pub title: String,
    pub body: String,
    pub folder: String,
    pub tags: Vec<String>,
    pub author_agent_id: Option<Uuid>,
    pub author_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    pub canvas: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportActor {
    Agent(Uuid),
    User(Uuid),
}

impl ReportActor {
    pub(crate) fn agent(self) -> Option<Uuid> {
        match self {
            ReportActor::Agent(id) => Some(id),
            ReportActor::User(_) => None,
        }
    }

    pub(crate) fn user(self) -> Option<Uuid> {
        match self {
            ReportActor::User(id) => Some(id),
            ReportActor::Agent(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewReport {
    pub owner_user_id: Uuid,
    pub by: ReportActor,
    pub title: String,
    pub body: String,
    pub folder: Option<String>,
    pub tags: Vec<String>,
    pub visibility: Option<String>,
    pub pinned: bool,
    pub task_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReportPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub append: Option<String>,
    pub folder: Option<String>,
    pub tags: Option<Vec<String>>,
    pub pinned: Option<bool>,
    pub visibility: Option<String>,
    #[serde(skip)]
    pub canvas: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReportFilter {
    pub query: Option<String>,
    pub folder: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<i64>,
    pub practice: Option<bool>,
}

pub fn can_read(report: &Report, viewer: Uuid, role: Role, org: Uuid) -> bool {
    report.org_id == Some(org) && (report.owner_user_id == viewer || report.visibility == "public" || role == Role::Admin)
}

pub fn can_edit(report: &Report, viewer: Uuid, role: Role, org: Uuid) -> bool {
    report.org_id == Some(org) && (report.owner_user_id == viewer || role == Role::Admin)
}

pub fn clean_title(title: &str) -> Result<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ").replace(['[', ']', '|'], "");
    let title = title.trim();
    if title.is_empty() {
        bail!("reports need a title");
    }
    if title.chars().count() > MAX_TITLE {
        bail!("titles are at most {MAX_TITLE} characters");
    }
    Ok(title.to_string())
}

pub fn clean_folder(folder: &str) -> Result<String> {
    let parts: Vec<String> = folder
        .split(['/', '\\'])
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty() && p != "." && p != "..")
        .collect();
    if parts.len() > MAX_FOLDER_DEPTH {
        bail!("folders nest at most {MAX_FOLDER_DEPTH} deep");
    }
    if parts.iter().any(|p| p.chars().count() > 60) {
        bail!("folder names are at most 60 characters");
    }
    Ok(parts.join("/"))
}

pub fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let t: String = tag
            .trim()
            .trim_start_matches('#')
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '/'))
            .take(40)
            .collect();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
        if out.len() == MAX_TAGS {
            break;
        }
    }
    out
}

pub const REPO_NOTES_FOLDER: &str = "Repo notes";
pub const REPO_NOTES_TAG: &str = "repo-notes";
const REPO_NOTES_MAX: usize = 12 * 1024;
const REPO_NOTE_MAX: usize = 400;
const REPO_NOTES_PER_TASK: usize = 12;

pub fn repo_notes_title(repo: &str) -> String {
    format!("Repo notes: {}", repo.trim().trim_end_matches(".git"))
}

fn note_key(note: &str) -> String {
    note.trim_start_matches(['-', '*', ' ']).trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn merge_repo_notes(body: &str, notes: &[String]) -> Option<String> {
    let mut lines: Vec<String> = body.lines().filter(|l| l.trim_start().starts_with("- ")).map(|l| l.trim().to_string()).collect();
    let mut seen: Vec<String> = lines.iter().map(|l| note_key(l)).collect();
    let mut added = false;
    for note in notes.iter().take(REPO_NOTES_PER_TASK) {
        let note: String = note.split_whitespace().collect::<Vec<_>>().join(" ").trim_start_matches(['-', '*', ' ']).chars().take(REPO_NOTE_MAX).collect();
        let key = note_key(&note);
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        lines.push(format!("- {note}"));
        added = true;
    }
    if !added {
        return None;
    }
    while lines.iter().map(|l| l.len() + 1).sum::<usize>() > REPO_NOTES_MAX && lines.len() > 1 {
        lines.remove(0);
    }
    Some(format!("Short, durable facts agents learned while working in this repository. Edit or remove anything that is wrong; agents read this before each quest here.\n\n{}\n", lines.join("\n")))
}

pub fn repo_notes_prompt(body: &str, cap: usize) -> String {
    let lines: Vec<&str> = body.lines().filter(|l| l.trim_start().starts_with("- ")).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in lines.iter().rev() {
        if size + line.len() + 1 > cap {
            break;
        }
        size += line.len() + 1;
        kept.push(line);
    }
    if kept.is_empty() {
        return String::new();
    }
    kept.reverse();
    format!("What earlier quests learned about this repository (from the Repo notes report; trust the code if they disagree):\n{}", kept.join("\n"))
}

fn check_body(body: &str) -> Result<()> {
    if body.len() > MAX_BODY {
        bail!("reports are at most 512 KB; split it into linked reports with [[Title]] links");
    }
    Ok(())
}

fn prose(body: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut inline = false;
        for ch in line.chars() {
            if ch == '`' {
                inline = !inline;
            } else if !inline {
                out.push(ch);
            }
        }
        out.push('\n');
    }
    out
}

pub fn wiki_links(body: &str) -> Vec<String> {
    let text = prose(body);
    let mut out: Vec<String> = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        if !inner.contains('\n') {
            let target = inner.split('|').next().unwrap_or("").split('#').next().unwrap_or("").trim();
            if !target.is_empty() && target.chars().count() <= MAX_TITLE && !out.iter().any(|t| t.eq_ignore_ascii_case(target)) {
                out.push(target.to_string());
            }
        }
        rest = &after[end + 2..];
    }
    out
}

pub fn report_refs(body: &str) -> Vec<String> {
    let text = prose(body);
    let mut out: Vec<String> = Vec::new();
    for (i, _) in text.match_indices("report:") {
        let before = text[..i].chars().last();
        if before.is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let id: String = text[i + 7..].chars().take_while(|c| c.is_ascii_hexdigit() || *c == '-').take(36).collect();
        let id = id.to_lowercase();
        if id.replace('-', "").len() >= 6 && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

fn ref_key(key: &str) -> String {
    let key = key.trim();
    let key = key.strip_prefix("report:").unwrap_or(key);
    let key = key.strip_prefix("[[").and_then(|k| k.strip_suffix("]]")).unwrap_or(key);
    key.split('|').next().unwrap_or(key).trim().to_string()
}

fn slug_base(title: &str) -> String {
    let s: String = slug::slugify(title).chars().take(60).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "report".into()
    } else {
        s
    }
}

fn changed_chars(a: &str, b: &str) -> usize {
    let prefix = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let suffix = a.bytes().rev().zip(b.bytes().rev()).take_while(|(x, y)| x == y).count();
    let shared = (prefix + suffix).min(a.len().min(b.len()));
    a.len().max(b.len()) - shared
}

impl WorldStore {
    async fn unused_report_slug(&self, owner: Uuid, title: &str, except: Option<Uuid>) -> Result<String> {
        let base = slug_base(title);
        let taken: Vec<String> = sqlx::query_scalar(
            "SELECT slug FROM world_reports WHERE owner_user_id = $1 AND (slug = $2 OR slug LIKE $2 || '-%') AND id IS DISTINCT FROM $3",
        )
        .bind(owner)
        .bind(&base)
        .bind(except)
        .fetch_all(self.pool())
        .await?;
        if !taken.contains(&base) {
            return Ok(base);
        }
        Ok((2..).map(|n| format!("{base}-{n}")).find(|s| !taken.contains(s)).unwrap_or(base))
    }

    pub(crate) async fn resolve_report_links(&self, owner: Uuid, org: Option<Uuid>, body: &str, me: Option<Uuid>) -> Result<Vec<Uuid>> {
        let titles: Vec<String> = wiki_links(body).iter().map(|t| t.to_lowercase()).collect();
        let refs: Vec<String> = report_refs(body).iter().map(|r| format!("{r}%")).collect();
        if titles.is_empty() && refs.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT r.id FROM world_reports r
             WHERE r.org_id = $5 AND (r.owner_user_id = $1 OR r.visibility = 'public') AND r.id IS DISTINCT FROM $4
               AND (lower(r.title) = ANY($2) OR r.slug = ANY($2) OR r.id::text LIKE ANY($3))",
        )
        .bind(owner)
        .bind(&titles)
        .bind(&refs)
        .bind(me)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    async fn relink_mentions(&self, owner: Uuid, org: Option<Uuid>, report: Uuid, title: &str) -> Result<()> {
        let pending: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT id, owner_user_id, body FROM world_reports
             WHERE id <> $2 AND owner_user_id = $1 AND org_id = $4 AND NOT links @> ARRAY[$2]
               AND strpos(lower(body), '[[' || lower($3)) > 0",
        )
        .bind(owner)
        .bind(report)
        .bind(title)
        .bind(org)
        .fetch_all(self.pool())
        .await?;
        for (id, owner, body) in pending {
            let links = self.resolve_report_links(owner, org, &body, Some(id)).await?;
            sqlx::query("UPDATE world_reports SET links = $2 WHERE id = $1").bind(id).bind(&links).execute(self.pool()).await?;
        }
        Ok(())
    }

    pub async fn create_report(&self, new: NewReport) -> Result<Report> {
        self.insert_report(new, None, None).await
    }

    pub(crate) async fn insert_report(&self, new: NewReport, canvas: Option<&crate::canvas::Canvas>, practice: Option<&serde_json::Value>) -> Result<Report> {
        let title = clean_title(&new.title)?;
        check_body(&new.body)?;
        let folder = clean_folder(new.folder.as_deref().unwrap_or(""))?;
        let tags = clean_tags(&new.tags);
        let visibility = match new.visibility.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            None => "private".to_string(),
            Some(v @ ("private" | "public")) => v.to_string(),
            Some(_) => bail!("visibility is private or public"),
        };
        let slug = self.unused_report_slug(new.owner_user_id, &title, None).await?;
        let org: Option<Uuid> = sqlx::query_scalar(
            "SELECT COALESCE((SELECT org_id FROM agent_profiles WHERE id = $2), (SELECT org_id FROM agent_tasks WHERE id = $3),
                             world_active_org($1), world_only_org(), world_default_org())",
        )
        .bind(new.owner_user_id)
        .bind(new.by.agent())
        .bind(new.task_id)
        .fetch_one(self.pool())
        .await?;
        if let Some(c) = canvas {
            self.check_canvas_uploads(new.owner_user_id, org, c, &crate::canvas::Canvas::default()).await?;
        }
        let canvas = canvas.map(crate::canvas::Canvas::to_value);
        let links = self.resolve_report_links(new.owner_user_id, org, &new.body, None).await?;
        let mut tx = self.pool().begin().await?;
        let report = sqlx::query_as::<_, Report>(
            "INSERT INTO world_reports (owner_user_id, author_agent_id, author_user_id, title, slug, body, tags, folder, pinned, visibility, links, task_id, org_id, kind, canvas, practice)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, CASE WHEN $14::jsonb IS NULL THEN 'doc' ELSE 'canvas' END, $14, $15) RETURNING *",
        )
        .bind(new.owner_user_id)
        .bind(new.by.agent())
        .bind(new.by.user())
        .bind(&title)
        .bind(&slug)
        .bind(&new.body)
        .bind(&tags)
        .bind(&folder)
        .bind(new.pinned)
        .bind(&visibility)
        .bind(&links)
        .bind(new.task_id)
        .bind(org)
        .bind(&canvas)
        .bind(practice)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO world_report_revisions (report_id, revision, title, body, folder, tags, author_agent_id, author_user_id, canvas)
             VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(report.id)
        .bind(&report.title)
        .bind(&report.body)
        .bind(&report.folder)
        .bind(&report.tags)
        .bind(new.by.agent())
        .bind(new.by.user())
        .bind(&canvas)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.relink_mentions(report.owner_user_id, report.org_id, report.id, &report.title).await?;
        self.report_event("report.created", &report, new.by, false).await?;
        Ok(report)
    }

    pub async fn repo_notes(&self, owner: Uuid, org: Option<Uuid>, repo: &str) -> Result<Option<Report>> {
        Ok(sqlx::query_as::<_, Report>(
            "SELECT * FROM world_reports WHERE owner_user_id = $1 AND title = $2 AND ($3::uuid IS NULL OR org_id = $3) ORDER BY created_at LIMIT 1",
        )
        .bind(owner)
        .bind(repo_notes_title(repo))
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn record_repo_notes(&self, owner: Uuid, agent: Uuid, repo: &str, task_id: Option<Uuid>, notes: &[String]) -> Result<Option<Report>> {
        if notes.iter().all(|n| n.trim().is_empty()) {
            return Ok(None);
        }
        let org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(agent).fetch_optional(self.pool()).await?;
        match self.repo_notes(owner, org, repo).await? {
            Some(current) => {
                let Some(body) = merge_repo_notes(&current.body, notes) else { return Ok(None) };
                let patch = ReportPatch { body: Some(body), ..Default::default() };
                self.update_report(current.id, ReportActor::Agent(agent), &patch).await
            }
            None => {
                let Some(body) = merge_repo_notes("", notes) else { return Ok(None) };
                self.create_report(NewReport {
                    owner_user_id: owner,
                    by: ReportActor::Agent(agent),
                    title: repo_notes_title(repo),
                    body,
                    folder: Some(REPO_NOTES_FOLDER.into()),
                    tags: vec![REPO_NOTES_TAG.into()],
                    visibility: None,
                    pinned: false,
                    task_id,
                })
                .await
                .map(Some)
            }
        }
    }

    pub async fn get_report(&self, id: Uuid) -> Result<Option<Report>> {
        Ok(sqlx::query_as::<_, Report>("SELECT * FROM world_reports WHERE id = $1").bind(id).fetch_optional(self.pool()).await?)
    }

    pub async fn find_report(&self, viewer: Uuid, org: Uuid, key: &str) -> Result<Option<Report>> {
        let key = ref_key(key);
        if key.is_empty() {
            return Ok(None);
        }
        let hex = key.replace('-', "");
        let id_prefix = (hex.len() >= 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("{}%", key.to_lowercase()));
        Ok(sqlx::query_as::<_, Report>(&format!(
            "SELECT r.* FROM world_reports r
             WHERE {visible}
               AND (r.id::text LIKE $2 OR lower(r.title) = lower($3) OR r.slug = lower($3))
             ORDER BY (r.owner_user_id = $1) DESC, (lower(r.title) = lower($3)) DESC, r.updated_at DESC LIMIT 1",
            visible = VISIBLE.replace("$9", "$4"),
        ))
        .bind(viewer)
        .bind(id_prefix)
        .bind(&key)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn list_reports(&self, viewer: Uuid, org: Uuid, filter: &ReportFilter) -> Result<Vec<ReportBrief>> {
        let query = filter.query.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(|q| format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
        let folder = match filter.folder.as_deref() {
            Some(f) => Some(clean_folder(f)?),
            None => None,
        };
        let tag = filter.tag.as_deref().and_then(|t| clean_tags(&[t.to_string()]).into_iter().next());
        Ok(sqlx::query_as::<_, ReportBrief>(&format!(
            "SELECT {BRIEF_COLUMNS} FROM world_reports r
             WHERE {visible}
               AND ($2::text IS NULL OR r.title ILIKE $2 OR r.body ILIKE $2 OR array_to_string(r.tags, ' ') ILIKE $2)
               AND ($3::text IS NULL OR r.folder = $3 OR r.folder LIKE $3 || '/%' OR $3 = '')
               AND ($4::text IS NULL OR $4 = ANY(r.tags))
               AND ($7::bool IS NULL OR (r.practice IS NOT NULL) = $7)
             ORDER BY ($2::text IS NOT NULL AND r.title ILIKE $2) DESC, r.pinned DESC, r.updated_at DESC
             LIMIT $5",
            visible = VISIBLE.replace("$9", "$6"),
        ))
        .bind(viewer)
        .bind(query)
        .bind(folder)
        .bind(tag)
        .bind(filter.limit.unwrap_or(300).clamp(1, 1000))
        .bind(org)
        .bind(filter.practice)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn find_report_brief(&self, viewer: Uuid, org: Uuid, key: &str) -> Result<Option<ReportBrief>> {
        let Some(report) = self.find_report(viewer, org, key).await? else { return Ok(None) };
        Ok(sqlx::query_as::<_, ReportBrief>(&format!("SELECT {BRIEF_COLUMNS} FROM world_reports r WHERE {visible} AND r.id = $2", visible = VISIBLE.replace("$9", "$3")))
            .bind(viewer)
            .bind(report.id)
            .bind(org)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn report_backlinks(&self, viewer: Uuid, org: Uuid, id: Uuid) -> Result<Vec<ReportBrief>> {
        Ok(sqlx::query_as::<_, ReportBrief>(&format!(
            "SELECT {BRIEF_COLUMNS} FROM world_reports r WHERE {visible} AND r.links @> ARRAY[$2] ORDER BY r.updated_at DESC LIMIT 100",
            visible = VISIBLE.replace("$9", "$3"),
        ))
        .bind(viewer)
        .bind(id)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn report_outlinks(&self, viewer: Uuid, org: Uuid, report: &Report) -> Result<Vec<ReportBrief>> {
        Ok(sqlx::query_as::<_, ReportBrief>(&format!(
            "SELECT {BRIEF_COLUMNS} FROM world_reports r WHERE {visible} AND r.id = ANY($2) ORDER BY r.title",
            visible = VISIBLE.replace("$9", "$3"),
        ))
        .bind(viewer)
        .bind(&report.links)
        .bind(org)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn report_revisions(&self, id: Uuid) -> Result<Vec<RevisionBrief>> {
        Ok(sqlx::query_as::<_, RevisionBrief>(
            "SELECT revision, title, folder, tags, author_agent_id, author_user_id, created_at, octet_length(body) AS size
             FROM world_report_revisions WHERE report_id = $1 ORDER BY revision DESC",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn report_revision(&self, id: Uuid, revision: i32) -> Result<Option<Revision>> {
        Ok(sqlx::query_as::<_, Revision>(
            "SELECT revision, title, body, folder, tags, author_agent_id, author_user_id, created_at, canvas
             FROM world_report_revisions WHERE report_id = $1 AND revision = $2",
        )
        .bind(id)
        .bind(revision)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn update_report(&self, id: Uuid, by: ReportActor, patch: &ReportPatch) -> Result<Option<Report>> {
        let Some(current) = self.get_report(id).await? else { return Ok(None) };
        let title = match patch.title.as_deref() {
            Some(t) => clean_title(t)?,
            None => current.title.clone(),
        };
        let canvas = match (current.kind.as_str(), &patch.canvas) {
            ("canvas", _) if patch.body.is_some() || patch.append.is_some() => bail!("\"{}\" is a canvas; change it with the canvas tools", current.title),
            ("canvas", Some(value)) => {
                let canvas = crate::canvas::Canvas::parse(Some(value))?;
                canvas.validate()?;
                let before = crate::canvas::Canvas::parse(current.canvas.as_ref())?;
                self.check_canvas_uploads(current.owner_user_id, current.org_id, &canvas, &before).await?;
                Some((canvas.summary(), canvas.to_value()))
            }
            _ => None,
        };
        let (canvas_body, canvas) = match canvas {
            Some((body, value)) => (Some(body), Some(value)),
            None => (None, None),
        };
        let canvas_changed = canvas.is_some() && canvas != current.canvas;
        let mut body = canvas_body.or_else(|| patch.body.clone()).unwrap_or_else(|| current.body.clone());
        if let Some(extra) = patch.append.as_deref().filter(|a| !a.trim().is_empty()) {
            if !body.is_empty() && !body.ends_with("\n\n") {
                body.push_str(if body.ends_with('\n') { "\n" } else { "\n\n" });
            }
            body.push_str(extra.trim_end());
            body.push('\n');
        }
        check_body(&body)?;
        let folder = match patch.folder.as_deref() {
            Some(f) => clean_folder(f)?,
            None => current.folder.clone(),
        };
        let tags = patch.tags.as_deref().map(clean_tags).unwrap_or_else(|| current.tags.clone());
        let visibility = match patch.visibility.as_deref().map(str::trim) {
            None | Some("") => current.visibility.clone(),
            Some(v @ ("private" | "public")) => v.to_string(),
            Some(_) => bail!("visibility is private or public"),
        };
        let pinned = patch.pinned.unwrap_or(current.pinned);
        let content_changed = canvas_changed || title != current.title || body != current.body || folder != current.folder || tags != current.tags;
        if !content_changed && visibility == current.visibility && pinned == current.pinned {
            return Ok(Some(current));
        }
        let renamed = title != current.title;
        let slug = if renamed { self.unused_report_slug(current.owner_user_id, &title, Some(id)).await? } else { current.slug.clone() };
        let links = if body != current.body { self.resolve_report_links(current.owner_user_id, current.org_id, &body, Some(id)).await? } else { current.links.clone() };
        let mut tx = self.pool().begin().await?;
        let (revision, merged) = if content_changed {
            self.record_revision(&mut tx, &current, by, (&title, &body, &folder, &tags), canvas.as_ref()).await?
        } else {
            (current.revision, false)
        };
        let report = sqlx::query_as::<_, Report>(
            "UPDATE world_reports SET title = $2, slug = $3, body = $4, folder = $5, tags = $6, visibility = $7, pinned = $8,
               links = $9, revision = $10, updated_at = CASE WHEN $11 THEN NOW() ELSE updated_at END, canvas = COALESCE($12, canvas)
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(&title)
        .bind(&slug)
        .bind(&body)
        .bind(&folder)
        .bind(&tags)
        .bind(&visibility)
        .bind(pinned)
        .bind(&links)
        .bind(revision)
        .bind(content_changed)
        .bind(&canvas)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        if renamed {
            self.relink_mentions(report.owner_user_id, report.org_id, report.id, &report.title).await?;
        }
        let quiet = merged || !content_changed || (!renamed && changed_chars(&current.body, &report.body) < QUIET_CHARS);
        self.report_event("report.updated", &report, by, quiet).await?;
        Ok(Some(report))
    }

    pub(crate) async fn record_revision(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        current: &Report,
        by: ReportActor,
        (title, body, folder, tags): (&str, &str, &str, &[String]),
        canvas: Option<&serde_json::Value>,
    ) -> Result<(i32, bool)> {
        let id = current.id;
        let last: Option<(Option<Uuid>, Option<Uuid>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT author_agent_id, author_user_id, created_at FROM world_report_revisions
             WHERE report_id = $1 AND revision = $2 AND revision > 1",
        )
        .bind(id)
        .bind(current.revision)
        .fetch_optional(&mut **tx)
        .await?;
        let merged = last.is_some_and(|(agent, user, at)| (agent, user) == (by.agent(), by.user()) && (Utc::now() - at).num_seconds() < MERGE_SECS);
        let canvas = canvas.or(current.canvas.as_ref());
        if merged {
            sqlx::query(
                "UPDATE world_report_revisions SET title = $3, body = $4, folder = $5, tags = $6, canvas = $7, created_at = NOW()
                 WHERE report_id = $1 AND revision = $2",
            )
            .bind(id)
            .bind(current.revision)
            .bind(title)
            .bind(body)
            .bind(folder)
            .bind(tags)
            .bind(canvas)
            .execute(&mut **tx)
            .await?;
            return Ok((current.revision, true));
        }
        let revision = current.revision + 1;
        sqlx::query(
            "INSERT INTO world_report_revisions (report_id, revision, title, body, folder, tags, author_agent_id, author_user_id, canvas)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(revision)
        .bind(title)
        .bind(body)
        .bind(folder)
        .bind(tags)
        .bind(by.agent())
        .bind(by.user())
        .bind(canvas)
        .execute(&mut **tx)
        .await?;
        sqlx::query("DELETE FROM world_report_revisions WHERE report_id = $1 AND revision > 1 AND revision <= $2 - $3")
            .bind(id)
            .bind(revision)
            .bind(KEEP_REVISIONS as i32)
            .execute(&mut **tx)
            .await?;
        Ok((revision, false))
    }

    pub async fn restore_report_revision(&self, id: Uuid, revision: i32, by: ReportActor) -> Result<Option<Report>> {
        let Some(old) = self.report_revision(id, revision).await? else { return Ok(None) };
        self.update_report(
            id,
            by,
            &match old.canvas {
                Some(canvas) => ReportPatch { title: Some(old.title), folder: Some(old.folder), tags: Some(old.tags), canvas: Some(canvas), ..Default::default() },
                None => ReportPatch { title: Some(old.title), body: Some(old.body), folder: Some(old.folder), tags: Some(old.tags), ..Default::default() },
            },
        )
        .await
    }

    pub async fn delete_report(&self, id: Uuid, by: ReportActor) -> Result<Option<Report>> {
        let mut tx = self.pool().begin().await?;
        let report = sqlx::query_as::<_, Report>("DELETE FROM world_reports WHERE id = $1 RETURNING *").bind(id).fetch_optional(&mut *tx).await?;
        if report.is_some() {
            sqlx::query("UPDATE world_reports SET links = array_remove(links, $1) WHERE links @> ARRAY[$1]").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        if let Some(r) = &report {
            self.report_event("report.deleted", r, by, true).await?;
        }
        Ok(report)
    }

    pub(crate) async fn report_event(&self, kind: &str, report: &Report, by: ReportActor, quiet: bool) -> Result<()> {
        let mut event = NewEvent::new(kind).actor(by.user()).task(report.task_id).payload(json!({
            "report_id": report.id,
            "org_id": report.org_id,
            "title": report.title,
            "folder": report.folder,
            "owner_user_id": report.owner_user_id,
            "visibility": report.visibility,
            "revision": report.revision,
            "author_agent_id": by.agent(),
            "quiet": quiet,
        }));
        if let Some(agent) = by.agent() {
            event = event.agent(agent);
        }
        self.emit(event, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn repo_notes_merge_and_cap() {
        let first = merge_repo_notes("", &["Run `cargo test -p x`".into(), " ".into(), "run `cargo test -p x`".into()]).unwrap();
        assert_eq!(first.matches("\n- ").count(), 1);
        assert!(merge_repo_notes(&first, &["- Run   `cargo test -p x`".into()]).is_none());
        let second = merge_repo_notes(&first, &["Migrations live in migrations/".into()]).unwrap();
        assert!(second.ends_with("- Migrations live in migrations/\n"));
        let prompt = repo_notes_prompt(&second, 40);
        assert!(prompt.contains("Migrations") && !prompt.contains("cargo"));
        assert!(repo_notes_prompt("no bullets", 4096).is_empty());
        let many: Vec<String> = (0..2000).map(|i| format!("fact {i} {}", "x".repeat(40))).collect();
        let mut body = String::new();
        for chunk in many.chunks(10) {
            body = merge_repo_notes(&body, chunk).unwrap();
        }
        assert!(body.len() <= REPO_NOTES_MAX + 400);
        assert!(body.contains("fact 1999"));
        assert_eq!(repo_notes_title("acme/app.git"), "Repo notes: acme/app");
    }

    #[test]
    fn parses_links_outside_code() {
        let body = "See [[Cache plan]] and [[Rollout|the rollout]].\n`[[not this]]`\n```\n[[nor this]]\n```\nAlso report:1a2b3c4d and xreport:ffffff.";
        assert_eq!(wiki_links(body), vec!["Cache plan".to_string(), "Rollout".to_string()]);
        assert_eq!(report_refs(body), vec!["1a2b3c4d".to_string()]);
        assert_eq!(clean_folder(" research / infra//q3 ").unwrap(), "research/infra/q3");
        assert!(clean_folder("a/b/c/d/e/f/g").is_err());
        assert_eq!(clean_tags(&["#Perf Work".into(), "perf-work".into(), "".into()]), vec!["perf-work".to_string()]);
        assert_eq!(ref_key("report:abc123"), "abc123");
        assert_eq!(ref_key("[[Cache plan|x]]"), "Cache plan");
        assert_eq!(changed_chars("hello world", "hello brave world"), 6);
    }

    #[tokio::test]
    async fn reports_link_revise_and_delete() -> Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Report tester') RETURNING id")
            .bind(format!("reports-{}@example.com", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Someone else') RETURNING id")
            .bind(format!("reports-{}@example.com", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        let by = ReportActor::User(owner);
        let new = |title: &str, body: &str| NewReport {
            owner_user_id: owner,
            by,
            title: title.into(),
            body: body.into(),
            folder: Some("research/infra".into()),
            tags: vec!["Perf".into()],
            visibility: None,
            pinned: false,
            task_id: None,
        };
        let index = store.create_report(new("Index", "Start at [[Cache plan]].")).await?;
        assert!(index.links.is_empty());
        let plan = store.create_report(new("Cache plan", "Body")).await?;
        assert_eq!(store.get_report(index.id).await?.unwrap().links, vec![plan.id]);
        let back = store.report_backlinks(owner, crate::orgs::DEFAULT_ORG, plan.id).await?;
        assert_eq!(back.iter().map(|b| b.id).collect::<Vec<_>>(), vec![index.id]);
        assert!(store.report_backlinks(other, crate::orgs::DEFAULT_ORG, plan.id).await?.is_empty());
        assert!(store.find_report(other, crate::orgs::DEFAULT_ORG, "Cache plan").await?.is_none());
        assert_eq!(store.find_report(owner, crate::orgs::DEFAULT_ORG, "cache-plan").await?.unwrap().id, plan.id);
        assert_eq!(store.find_report(owner, crate::orgs::DEFAULT_ORG, &format!("report:{}", &plan.id.to_string()[..8])).await?.unwrap().id, plan.id);
        let dup = store.create_report(new("Cache plan", "Second")).await?;
        assert_eq!(dup.slug, "cache-plan-2");

        let p1 = store.update_report(plan.id, by, &ReportPatch { append: Some("More".into()), ..Default::default() }).await?.unwrap();
        assert_eq!(p1.revision, 2);
        assert_eq!(p1.body, "Body\n\nMore\n");
        let p2 = store.update_report(plan.id, by, &ReportPatch { append: Some("Again".into()), ..Default::default() }).await?.unwrap();
        assert_eq!(p2.revision, 2);
        let p3 = store.update_report(plan.id, ReportActor::User(other), &ReportPatch { title: Some("Cache design".into()), ..Default::default() }).await?.unwrap();
        assert_eq!((p3.revision, p3.slug.as_str()), (3, "cache-design"));
        assert_eq!(store.report_revisions(plan.id).await?.len(), 3);
        let restored = store.restore_report_revision(plan.id, 1, by).await?.unwrap();
        assert_eq!((restored.title.as_str(), restored.body.as_str(), restored.revision), ("Cache plan", "Body", 4));

        let listed = store.list_reports(owner, crate::orgs::DEFAULT_ORG, &ReportFilter { folder: Some("research".into()), tag: Some("perf".into()), ..Default::default() }).await?;
        assert_eq!(listed.len(), 3);
        let hits = store.list_reports(owner, crate::orgs::DEFAULT_ORG, &ReportFilter { query: Some("Start at".into()), ..Default::default() }).await?;
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![index.id]);
        assert!(store.list_reports(other, crate::orgs::DEFAULT_ORG, &ReportFilter::default()).await?.is_empty());

        store.delete_report(plan.id, by).await?;
        assert_eq!(store.get_report(index.id).await?.unwrap().links, vec![dup.id]);
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_events WHERE type LIKE 'report.%' AND payload->>'owner_user_id' = $1")
            .bind(owner.to_string())
            .fetch_one(&pool)
            .await?;
        assert!(events >= 6);
        sqlx::query("DELETE FROM world_events WHERE type LIKE 'report.%' AND payload->>'owner_user_id' = $1").bind(owner.to_string()).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, other]).execute(&pool).await?;
        Ok(())
    }
}
