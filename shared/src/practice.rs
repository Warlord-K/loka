use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::academy::{self, Grade};
use crate::canvas::Canvas;
use crate::items::WorldItem;
use crate::reports::{NewReport, Report, ReportActor, ReportBrief, ReportPatch};
use crate::world::{emit_in, NewEvent, WorldAgent, WorldStore};

pub const MAX_STROKES_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_STROKES: usize = 2000;
pub const MAX_POINTS: usize = 100_000;
pub const MAX_SIDE: f64 = 20_000.0;
pub const MAX_SIZE: f64 = 500.0;
pub const MAX_NOTE: usize = 2000;
pub const MAX_CHECK_FIELD: usize = 4000;
pub const MAX_SUBJECT: usize = 40;
pub const HISTORY: i64 = 5;
pub const SUMMARY_STROKES: usize = 60;
pub const TOOLS: &[&str] = &["pen", "pencil", "marker", "eraser"];
pub const FOLDER: &str = "Practice";
pub const TAG: &str = "practice";
pub const NO_COACH: &str = "Pick a coach for this practice first.";
pub const NOT_PRACTICE: &str = "This canvas is not a practice canvas.";
pub const NOT_YOUR_AGENT: &str = "The coach must be one of your own agents.";
pub const NOT_AN_IMAGE: &str = "Upload the drawing as a PNG, JPEG, GIF or WebP image first, then send its upload id.";
pub const TOO_BIG: &str = "Stroke data is at most 2 MB once saved; send fewer points.";
pub const MIN_PREFIX: usize = 8;
pub const NO_IMAGE_TO_RESEND: &str = "That attempt has no picture, so the coach would have nothing to look at. Press Ready again to send a new attempt.";
pub const RESEND_SECS: i64 = 30;
pub const LESSON_REMOVED: &str = "This lesson was removed from the Academy, so it takes no new attempts. Ask an agent to make a new lesson.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotWaiting,
    TooSoon,
    Ambiguous,
    Missing,
    NoImage,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NotWaiting => "That attempt is already checked, so there is nothing to send again.",
            Refusal::TooSoon => "That attempt was just sent to the coach; try again in a little while.",
            Refusal::Ambiguous => "That short id matches more than one attempt; use more of it.",
            Refusal::Missing => "No practice attempt matches that id.",
            Refusal::NoImage => NO_IMAGE_TO_RESEND,
        })
    }
}

impl std::error::Error for Refusal {}

pub const DRAWING_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

const ATTEMPT_COLUMNS: &str = "a.id, a.report_id, a.user_id, a.subject, a.coach_agent_id, a.n, a.image_upload_id, a.note, a.status,
  a.\"check\", a.coach_message_id, a.created_at, a.checked_at,
  CASE WHEN jsonb_typeof(a.strokes->'strokes') = 'array' THEN jsonb_array_length(a.strokes->'strokes') ELSE 0 END AS stroke_count,
  (SELECT g.status FROM challenge_attempts g WHERE g.id = a.id) AS grade_status,
  (SELECT g.score FROM challenge_attempts g WHERE g.id = a.id) AS grade_score,
  (SELECT g.xp_awarded FROM challenge_attempts g WHERE g.id = a.id) AS xp_awarded";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Practice {
    pub subject: String,
    #[serde(default)]
    pub coach_agent_id: Option<Uuid>,
}

impl Practice {
    pub fn of(report: &Report) -> Option<Practice> {
        report.practice.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

fn half() -> f64 {
    0.5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    #[serde(default = "half")]
    pub p: f64,
    #[serde(default)]
    pub t: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub tool: String,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<String>,
    pub size: f64,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Strokes {
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub strokes: Vec<Stroke>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub right: String,
    pub fix: String,
    pub next: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Attempt {
    pub id: Uuid,
    pub report_id: Uuid,
    pub user_id: Uuid,
    pub subject: String,
    pub coach_agent_id: Option<Uuid>,
    pub n: i32,
    pub image_upload_id: Option<Uuid>,
    pub note: Option<String>,
    pub status: String,
    pub check: Option<Value>,
    pub coach_message_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub checked_at: Option<DateTime<Utc>>,
    pub stroke_count: i32,
    #[sqlx(default)]
    pub grade_status: Option<String>,
    #[sqlx(default)]
    pub grade_score: Option<i32>,
    #[sqlx(default)]
    pub xp_awarded: Option<i32>,
    #[sqlx(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strokes: Option<Value>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PracticeBrief {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub report: ReportBrief,
    pub attempts: i64,
    pub last_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptCard {
    pub id: Uuid,
    pub report_id: Uuid,
    pub title: String,
    pub subject: String,
    pub coach_agent_id: Option<Uuid>,
    pub coach_name: Option<String>,
    pub n: i32,
    pub image_upload_id: Option<Uuid>,
    pub status: String,
    pub check: Option<Value>,
    pub short_check: Option<String>,
    pub created_at: DateTime<Utc>,
    pub checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoachOption {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub expertise: Option<String>,
    pub is_main: bool,
    pub paused_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ready {
    pub image_upload_id: Uuid,
    pub strokes: Value,
    #[serde(default)]
    pub note: Option<String>,
}

pub fn clean_subject(subject: &str) -> Result<String> {
    let words: Vec<String> = subject
        .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let subject = words.join("-");
    if subject.is_empty() {
        bail!("Name a subject, like japanese or drawing.");
    }
    if subject.chars().count() > MAX_SUBJECT {
        bail!("Subjects are at most {MAX_SUBJECT} characters.");
    }
    Ok(subject)
}

pub fn subject_title(subject: &str) -> String {
    let words: Vec<String> = subject
        .split('-')
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect();
    format!("{} practice", words.join(" "))
}

fn color_ok(color: &str) -> bool {
    (1..=40).contains(&color.len())
        && color.starts_with(|c: char| c.is_ascii_lowercase())
        && color.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn rgb_ok(rgb: &str) -> bool {
    rgb.len() == 7 && rgb.starts_with('#') && rgb[1..].chars().all(|c| c.is_ascii_hexdigit())
}

pub fn parse_strokes(value: &Value) -> Result<Strokes> {
    let mut strokes: Strokes = serde_json::from_value(value.clone())
        .map_err(|e| anyhow!("Strokes are {{width, height, strokes: [{{tool, color, size, points: [{{x, y, p, t}}]}}]}}: {e}"))?;
    let side = |v: f64| v.is_finite() && v > 0.0 && v <= MAX_SIDE;
    if !side(strokes.width) || !side(strokes.height) {
        bail!("Canvas width and height are between 1 and {MAX_SIDE}.");
    }
    if strokes.strokes.len() > MAX_STROKES {
        bail!("An attempt has at most {MAX_STROKES} strokes.");
    }
    let mut total = 0;
    for (i, stroke) in strokes.strokes.iter().enumerate() {
        let n = i + 1;
        if !TOOLS.contains(&stroke.tool.as_str()) {
            bail!("Stroke {n} uses tool {:?}; tools are {}.", stroke.tool, TOOLS.join(", "));
        }
        if !color_ok(&stroke.color) {
            bail!("Stroke {n} needs a colour token name like w-ink.");
        }
        if stroke.rgb.as_deref().is_some_and(|rgb| !rgb_ok(rgb)) {
            bail!("Stroke {n} rgb is a colour like #1f2933.");
        }
        if !stroke.size.is_finite() || stroke.size <= 0.0 || stroke.size > MAX_SIZE {
            bail!("Stroke {n} size is above 0 and at most {MAX_SIZE}.");
        }
        if stroke.points.is_empty() {
            bail!("Stroke {n} has no points.");
        }
        total += stroke.points.len();
        let far = MAX_SIDE * 2.0;
        if stroke.points.iter().any(|p| !(p.x.is_finite() && p.y.is_finite() && p.x.abs() <= far && p.y.abs() <= far)) {
            bail!("Stroke {n} has a point outside the canvas.");
        }
        if stroke.points.iter().any(|p| !(p.p.is_finite() && (0.0..=1.0).contains(&p.p))) {
            bail!("Stroke {n} pressure p is between 0 and 1.");
        }
        if stroke.points.iter().any(|p| !(p.t.is_finite() && p.t >= 0.0)) {
            bail!("Stroke {n} time t is milliseconds from the start, 0 or more.");
        }
    }
    if total > MAX_POINTS {
        bail!("An attempt has at most {MAX_POINTS} points.");
    }
    for stroke in &mut strokes.strokes {
        stroke.rgb = stroke.rgb.take().map(|rgb| rgb.to_ascii_lowercase());
    }
    if serde_json::to_vec(&strokes)?.len() > MAX_STROKES_BYTES {
        bail!(TOO_BIG);
    }
    Ok(strokes)
}

fn direction(dx: f64, dy: f64) -> &'static str {
    let angle = dy.atan2(dx).to_degrees();
    match angle {
        a if a.abs() <= 22.5 => "left to right",
        a if a > 22.5 && a <= 67.5 => "down-right",
        a if a > 67.5 && a <= 112.5 => "top to bottom",
        a if a > 112.5 && a <= 157.5 => "down-left",
        a if a.abs() > 157.5 => "right to left",
        a if (-67.5..-22.5).contains(&a) => "up-right",
        a if (-112.5..-67.5).contains(&a) => "bottom to top",
        _ => "up-left",
    }
}

pub fn stroke_line(index: usize, stroke: &Stroke, diagonal: f64) -> String {
    let first = &stroke.points[0];
    let last = &stroke.points[stroke.points.len() - 1];
    let length: f64 = stroke.points.windows(2).map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y)).sum();
    let straight = (last.x - first.x).hypot(last.y - first.y);
    let dot = (diagonal * 0.015).max(2.0);
    let shape = if length < dot {
        "dot".to_string()
    } else if straight < length * 0.15 {
        "closed loop".to_string()
    } else if straight < length * 0.7 {
        format!("{}, curved", direction(last.x - first.x, last.y - first.y))
    } else {
        direction(last.x - first.x, last.y - first.y).to_string()
    };
    format!(
        "{index}. {} {shape}, {} points, ({:.0}, {:.0}) to ({:.0}, {:.0}), length {:.0}",
        stroke.tool,
        stroke.points.len(),
        first.x,
        first.y,
        last.x,
        last.y,
        length
    )
}

pub fn stroke_summary(strokes: &Strokes) -> String {
    if strokes.strokes.is_empty() {
        return format!("No strokes on a {:.0}x{:.0} canvas (only the picture).", strokes.width, strokes.height);
    }
    let diagonal = strokes.width.hypot(strokes.height);
    let mut tools: Vec<(String, usize)> = Vec::new();
    for stroke in &strokes.strokes {
        match tools.iter_mut().find(|(t, _)| *t == stroke.tool) {
            Some((_, n)) => *n += 1,
            None => tools.push((stroke.tool.clone(), 1)),
        }
    }
    let tools: Vec<String> = tools.iter().map(|(t, n)| format!("{t} {n}")).collect();
    let span = strokes.strokes.iter().flat_map(|s| s.points.iter().map(|p| p.t)).fold(0.0_f64, f64::max);
    let mut lines = vec![format!(
        "{} strokes on a {:.0}x{:.0} canvas ({}), drawn over {:.1} s, in drawing order (y grows downward):",
        strokes.strokes.len(),
        strokes.width,
        strokes.height,
        tools.join(", "),
        span / 1000.0
    )];
    lines.extend(strokes.strokes.iter().take(SUMMARY_STROKES).enumerate().map(|(i, s)| stroke_line(i + 1, s, diagonal)));
    if strokes.strokes.len() > SUMMARY_STROKES {
        lines.push(format!("...and {} more; read_practice has them all.", strokes.strokes.len() - SUMMARY_STROKES));
    }
    lines.join("\n")
}

fn check_field(value: &str, what: &str) -> Result<String> {
    let value = value.replace('\u{2014}', " - ").trim().to_string();
    if value.is_empty() {
        bail!("{what} is required: be specific about this attempt.");
    }
    if value.chars().count() > MAX_CHECK_FIELD {
        bail!("{what} is at most {MAX_CHECK_FIELD} characters.");
    }
    Ok(value)
}

impl Check {
    pub fn new(right: &str, fix: &str, next: &str, score: Option<f64>) -> Result<Check> {
        if score.is_some_and(|s| !s.is_finite() || !(0.0..=100.0).contains(&s)) {
            bail!("score is a number from 0 to 100.");
        }
        Ok(Check {
            right: check_field(right, "right")?,
            fix: check_field(fix, "fix")?,
            next: check_field(next, "next")?,
            score: score.map(|s| (s * 10.0).round() / 10.0),
        })
    }

    pub fn of(value: Option<&Value>) -> Option<Check> {
        value.and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn short(&self) -> String {
        let score = self.score.map(|s| format!("{s}/100. ")).unwrap_or_default();
        format!("{score}Fix: {}", crate::links::excerpt(&self.fix, 160))
    }

    pub fn message(&self, n: i32, title: &str, attempt: Uuid) -> String {
        let score = self.score.map(|s| format!("\n\n**Score:** {s}/100")).unwrap_or_default();
        format!(
            "Attempt {n} of \"{title}\"{score}\n\n**Right:** {}\n\n**Fix:** {}\n\n**Next:** {}\n\npractice:{attempt}",
            self.right, self.fix, self.next
        )
    }
}

pub fn history_line(attempt: &Attempt) -> String {
    match Check::of(attempt.check.as_ref()) {
        Some(c) => format!(
            "#{} checked{}: right: {}; fix: {}; next: {}",
            attempt.n,
            c.score.map(|s| format!(", score {s}")).unwrap_or_default(),
            crate::links::excerpt(&c.right, 200),
            crate::links::excerpt(&c.fix, 200),
            crate::links::excerpt(&c.next, 200)
        ),
        None => format!("#{} {} ({} strokes)", attempt.n, attempt.status, attempt.stroke_count),
    }
}

pub fn quote_text(subject: &str, n: i32, check: Option<&Check>) -> String {
    let title = subject_title(subject);
    match check {
        Some(c) => format!(
            "{title}, attempt {n}{}. Right: {} Fix: {} Next: {}",
            c.score.map(|s| format!(", score {s}/100")).unwrap_or_default(),
            c.right,
            c.fix,
            c.next
        ),
        None => format!("{title}, attempt {n} (waiting for the coach)"),
    }
}

pub fn course_title(subject: &str) -> String {
    subject_title(subject).trim_end_matches(" practice").to_string()
}

pub fn lesson_data(subject: &str, coach: Option<Uuid>, task: Option<Uuid>, report: Option<Uuid>) -> Value {
    json!({
        "type": academy::CANVAS,
        "prompt": academy::CANVAS_PROMPT,
        "hints": [],
        "skill": subject,
        "difficulty": "medium",
        "xp": 40,
        "minutes": 15,
        "tests": null,
        "reading": [],
        "coach_agent_id": coach,
        "task_id": task,
        "report_id": report,
    })
}

pub fn ledger_feedback(check: &Check) -> String {
    format!("Right: {}\n\nFix: {}\n\nNext: {}", check.right, check.fix, check.next)
}

pub fn attempt_key(raw: &str) -> String {
    raw.trim().trim_start_matches("practice:").trim().to_lowercase()
}

impl WorldStore {
    pub(crate) async fn owned_coach(&self, owner: Uuid, org: Option<Uuid>, agent: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM agent_profiles WHERE id = $1 AND owner_user_id = $2 AND org_id IS NOT DISTINCT FROM $3 AND archived_at IS NULL)",
        )
        .bind(agent)
        .bind(owner)
        .bind(org)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn subject_course(&self, owner: Uuid, org: Uuid, subject: &str, coach: Option<Uuid>, agent: Option<Uuid>) -> Result<WorldItem> {
        Ok(self.subject_course_made(owner, org, subject, coach, agent).await?.0)
    }

    pub(crate) async fn subject_course_made(&self, owner: Uuid, org: Uuid, subject: &str, coach: Option<Uuid>, agent: Option<Uuid>) -> Result<(WorldItem, bool)> {
        let subject = clean_subject(subject)?;
        let find = "SELECT * FROM world_items WHERE kind = 'adventure' AND status <> 'archived' AND owner_user_id = $1 AND org_id = $2 AND data->>'skill' = $3
                    ORDER BY (data->>'folder' = $4) DESC NULLS LAST, created_at LIMIT 1";
        if let Some(course) = sqlx::query_as::<_, WorldItem>(find).bind(owner).bind(org).bind(&subject).bind(FOLDER).fetch_optional(self.pool()).await? {
            return Ok((course, false));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))").bind(format!("course:{owner}:{org}:{subject}")).execute(&mut *tx).await?;
        if let Some(course) = sqlx::query_as::<_, WorldItem>(find).bind(owner).bind(org).bind(&subject).bind(FOLDER).fetch_optional(&mut *tx).await? {
            return Ok((course, false));
        }
        let data = json!({ "story": "", "challenges": [], "folder": FOLDER, "skill": subject, "coach_agent_id": coach });
        let course = self.insert_academy_item(&mut tx, "adventure", &course_title(&subject), owner, org, agent, data, None).await?;
        tx.commit().await?;
        self.item_event("item.created", &course).await?;
        let mut event = NewEvent::new("adventure.created").actor(Some(owner)).payload(json!({
            "adventure_id": course.id,
            "title": course.name,
            "challenges": [],
            "region_id": null,
            "owner_user_id": owner,
        }));
        if let Some(a) = agent {
            event = event.agent(a);
        }
        self.emit(event, None).await?;
        Ok((course, true))
    }

    pub async fn attach_canvas(&self, lesson: &WorldItem, course: Option<&WorldItem>, by: ReportActor) -> Result<Report> {
        let owner = lesson.owner_user_id.ok_or_else(|| anyhow!("that lesson has no owner"))?;
        let subject = clean_subject(lesson.data.get("skill").and_then(Value::as_str).unwrap_or("practice"))?;
        let coach = self.coach_for(lesson).await?;
        let folder = course.and_then(|c| c.data.get("folder").and_then(Value::as_str)).filter(|f| !f.is_empty()).unwrap_or(FOLDER).to_string();
        let practice = json!(Practice { subject: subject.clone(), coach_agent_id: coach });
        let report = self
            .create_canvas_with(
                NewReport {
                    owner_user_id: owner,
                    by,
                    title: lesson.name.clone(),
                    body: String::new(),
                    folder: Some(folder),
                    tags: vec![TAG.into(), subject],
                    visibility: None,
                    pinned: false,
                    task_id: academy::uuid_in(&lesson.data, "task_id"),
                },
                Canvas::default(),
                Some(&practice),
            )
            .await?;
        let linked = self.link_canvas(lesson.id, report.id).await;
        let report = match linked {
            Ok(report) => report,
            Err(e) => {
                self.delete_report(report.id, by).await?;
                return Err(e);
            }
        };
        if let Err(e) = self.announce_practice(&report, by).await {
            tracing::warn!(error = %e, report = %report.id, "could not announce a new practice canvas");
        }
        Ok(report)
    }

    async fn link_canvas(&self, lesson: Uuid, report: Uuid) -> Result<Report> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("UPDATE world_items SET data = data || jsonb_build_object('report_id', $2::text), updated_at = NOW() WHERE id = $1")
            .bind(lesson)
            .bind(report)
            .execute(&mut *tx)
            .await?;
        let report = sqlx::query_as::<_, Report>("UPDATE world_reports SET challenge_id = $2 WHERE id = $1 RETURNING *").bind(report).bind(lesson).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(report)
    }

    pub(crate) async fn drop_unattached(&self, lesson: Uuid, made_course: bool) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        let course: Option<Option<String>> = sqlx::query_scalar("SELECT data->>'adventure_id' FROM world_items WHERE id = $1 FOR UPDATE").bind(lesson).fetch_optional(&mut *tx).await?;
        if let Some(course) = course.flatten().and_then(|c| c.parse::<Uuid>().ok()) {
            sqlx::query(
                "UPDATE world_items SET data = jsonb_set(data, '{challenges}', COALESCE((SELECT jsonb_agg(x) FROM jsonb_array_elements(data->'challenges') x WHERE x <> to_jsonb($2::text)), '[]'::jsonb)), updated_at = NOW()
                 WHERE id = $1 AND kind = 'adventure'",
            )
            .bind(course)
            .bind(lesson.to_string())
            .execute(&mut *tx)
            .await?;
            if made_course {
                sqlx::query("UPDATE world_items SET status = 'archived', updated_at = NOW() WHERE id = $1 AND kind = 'adventure' AND jsonb_array_length(data->'challenges') = 0")
                    .bind(course)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE world_items SET status = 'archived', data = data - 'adventure_id' - 'order', updated_at = NOW() WHERE id = $1").bind(lesson).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn sync_canvas_report(&self, id: Uuid, owner: Uuid, name: &str, skill: &str, coach: Option<Option<Uuid>>) -> Result<()> {
        let Some(report) = self.get_report(id).await?.filter(|r| r.owner_user_id == owner) else { return Ok(()) };
        let Some(current) = Practice::of(&report) else { return Ok(()) };
        let title = crate::reports::clean_title(name)?;
        let mut report = report;
        if title != report.title {
            report = self
                .update_report(id, ReportActor::User(owner), &ReportPatch { title: Some(title), ..Default::default() })
                .await?
                .ok_or_else(|| anyhow!("That practice canvas is gone."))?;
        }
        let next = Practice { subject: clean_subject(skill)?, coach_agent_id: coach.unwrap_or(current.coach_agent_id) };
        if next != current {
            let mut tags: Vec<String> = report.tags.iter().filter(|t| **t != current.subject).cloned().collect();
            if !tags.contains(&next.subject) {
                tags.push(next.subject.clone());
            }
            report = sqlx::query_as::<_, Report>("UPDATE world_reports SET practice = $2, tags = $3, updated_at = NOW() WHERE id = $1 RETURNING *")
                .bind(id)
                .bind(json!(next))
                .bind(crate::reports::clean_tags(&tags))
                .fetch_one(self.pool())
                .await?;
            if let Some(c) = next.coach_agent_id.filter(|_| coach.is_some()) {
                self.save_coach_pref(owner, &next.subject, c).await?;
            }
            self.announce_practice(&report, ReportActor::User(owner)).await?;
        }
        Ok(())
    }

    pub async fn ensure_lesson(&self, report: &Report) -> Result<WorldItem> {
        if let Some(id) = report.challenge_id {
            if let Some(item) = self.get_item(id).await?.filter(|i| i.kind == "challenge") {
                return Ok(item);
            }
        }
        let practice = Practice::of(report).ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let org = report.org_id.ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let course = self.subject_course(report.owner_user_id, org, &practice.subject, practice.coach_agent_id, report.author_agent_id).await?;
        let mut tx = self.pool().begin().await?;
        let lesson = self
            .insert_academy_item(&mut tx, "challenge", &report.title, report.owner_user_id, org, report.author_agent_id, lesson_data(&practice.subject, practice.coach_agent_id, report.task_id, Some(report.id)), None)
            .await?;
        self.append_lesson(&mut tx, course.id, lesson.id).await?;
        let linked: Option<Uuid> = sqlx::query_scalar("UPDATE world_reports SET challenge_id = $2 WHERE id = $1 AND challenge_id IS NULL RETURNING id")
            .bind(report.id)
            .bind(lesson.id)
            .fetch_optional(&mut *tx)
            .await?;
        if linked.is_none() {
            drop(tx);
            let current: Option<Uuid> = sqlx::query_scalar("SELECT challenge_id FROM world_reports WHERE id = $1").bind(report.id).fetch_optional(self.pool()).await?.flatten();
            return match current {
                Some(id) => self.get_item(id).await?.ok_or_else(|| anyhow!("that lesson is gone")),
                None => bail!("That practice canvas is gone."),
            };
        }
        tx.commit().await?;
        Ok(self.get_item(lesson.id).await?.unwrap_or(lesson))
    }

    pub async fn move_lesson(&self, tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, lesson: Uuid, from: Option<Uuid>, to: Uuid) -> Result<i32> {
        if let Some(old) = from.filter(|o| *o != to) {
            sqlx::query(
                "UPDATE world_items SET data = jsonb_set(data, '{challenges}', COALESCE((SELECT jsonb_agg(x) FROM jsonb_array_elements(data->'challenges') x WHERE x <> to_jsonb($2::text)), '[]'::jsonb)), updated_at = NOW()
                 WHERE id = $1 AND kind = 'adventure'",
            )
            .bind(old)
            .bind(lesson.to_string())
            .execute(&mut **tx)
            .await?;
        }
        self.append_lesson(tx, to, lesson).await
    }

    pub async fn find_coach(&self, owner: Uuid, org: Uuid, key: &str) -> Result<Uuid> {
        let key = key.trim().trim_start_matches('@').to_lowercase();
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM agent_profiles
             WHERE owner_user_id = $1 AND org_id = $2 AND archived_at IS NULL
               AND (id::text = $3 OR lower(slug) = $3 OR lower(name) = $3)
             ORDER BY (id::text = $3) DESC, (lower(slug) = $3) DESC LIMIT 1",
        )
        .bind(owner)
        .bind(org)
        .bind(&key)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| anyhow!("No agent of yours is called {key}. {NOT_YOUR_AGENT}"))
    }

    pub async fn coach_pref(&self, owner: Uuid, org: Uuid, subject: &str) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT p.agent_id FROM world_practice_coach_prefs p JOIN agent_profiles a ON a.id = p.agent_id
             WHERE p.user_id = $1 AND p.subject = $2 AND a.owner_user_id = $1 AND a.org_id = $3 AND a.archived_at IS NULL",
        )
        .bind(owner)
        .bind(subject)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn default_coach(&self, owner: Uuid, org: Uuid, subject: &str) -> Result<Option<Uuid>> {
        if let Some(pref) = self.coach_pref(owner, org, subject).await? {
            return Ok(Some(pref));
        }
        Ok(sqlx::query_scalar(
            "SELECT a.id FROM agent_profiles a
             WHERE a.id = COALESCE((SELECT main_agent_id FROM org_members WHERE user_id = $1 AND org_id = $2),
                                   (SELECT main_agent_id FROM users WHERE id = $1))
               AND a.owner_user_id = $1 AND a.org_id = $2 AND a.archived_at IS NULL",
        )
        .bind(owner)
        .bind(org)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn save_coach_pref(&self, owner: Uuid, subject: &str, agent: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO world_practice_coach_prefs (user_id, subject, agent_id) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, subject) DO UPDATE SET agent_id = EXCLUDED.agent_id, updated_at = NOW()",
        )
        .bind(owner)
        .bind(subject)
        .bind(agent)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn practice_coaches(&self, owner: Uuid, org: Uuid) -> Result<Vec<CoachOption>> {
        Ok(self
            .list_agents_in(Some(org))
            .await?
            .into_iter()
            .filter(|a| a.owner_user_id == Some(owner) && a.archived_at.is_none())
            .map(|a| CoachOption { id: a.id, slug: a.slug, name: a.name, expertise: a.expertise, is_main: a.is_main, paused_reason: a.paused_reason })
            .collect())
    }

    async fn announce_practice(&self, report: &Report, by: ReportActor) -> Result<()> {
        let mut event = NewEvent::new("practice.updated").payload(json!({
            "report_id": report.id,
            "org_id": report.org_id,
            "owner_user_id": report.owner_user_id,
            "title": report.title,
            "practice": report.practice,
            "quiet": true,
        }));
        event = match by {
            ReportActor::Agent(a) => event.agent(a),
            ReportActor::User(u) => event.actor(Some(u)),
        };
        self.emit(event, None).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_practice(
        &self,
        owner: Uuid,
        org: Uuid,
        by: ReportActor,
        subject: &str,
        title: Option<&str>,
        coach: Option<Uuid>,
        task_id: Option<Uuid>,
    ) -> Result<Report> {
        let subject = clean_subject(subject)?;
        let coach = match coach {
            Some(c) => Some(c),
            None => self.default_coach(owner, org, &subject).await?,
        };
        if let Some(c) = coach {
            if !self.owned_coach(owner, Some(org), c).await? {
                bail!(NOT_YOUR_AGENT);
            }
        }
        let title = title.map(str::trim).filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| subject_title(&subject));
        let (course, made_course) = self.subject_course_made(owner, org, &subject, coach, by.agent()).await?;
        let mut tx = self.pool().begin().await?;
        let lesson = self.insert_academy_item(&mut tx, "challenge", &title, owner, org, by.agent(), lesson_data(&subject, coach, task_id, None), None).await?;
        self.append_lesson(&mut tx, course.id, lesson.id).await?;
        tx.commit().await?;
        let lesson = self.get_item(lesson.id).await?.unwrap_or(lesson);
        let report = match self.attach_canvas(&lesson, Some(&course), by).await {
            Ok(report) => report,
            Err(e) => {
                self.drop_unattached(lesson.id, made_course).await?;
                return Err(e);
            }
        };
        if let Some(c) = coach {
            self.save_coach_pref(owner, &subject, c).await?;
        }
        let mut event = NewEvent::new("challenge.created").actor(by.user()).payload(json!({
            "challenge_id": lesson.id,
            "title": lesson.name,
            "type": academy::CANVAS,
            "skill": subject,
            "adventure_id": course.id,
            "report_id": report.id,
            "owner_user_id": owner,
            "quiet": true,
        }));
        if let Some(a) = by.agent() {
            event = event.agent(a);
        }
        self.emit(event, None).await?;
        Ok(report)
    }

    pub async fn update_practice(&self, report: &Report, by: ReportActor, subject: Option<&str>, coach: Option<Uuid>, title: Option<&str>) -> Result<Report> {
        let current = Practice::of(report).ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let subject = match subject {
            Some(s) => clean_subject(s)?,
            None => current.subject.clone(),
        };
        if let Some(c) = coach {
            if !self.owned_coach(report.owner_user_id, report.org_id, c).await? {
                bail!(NOT_YOUR_AGENT);
            }
        }
        let mut report = report.clone();
        if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty() && *t != report.title) {
            report = self
                .update_report(report.id, by, &ReportPatch { title: Some(title.to_string()), ..Default::default() })
                .await?
                .ok_or_else(|| anyhow!("That practice canvas is gone."))?;
        }
        let next = Practice { subject: subject.clone(), coach_agent_id: coach.or(current.coach_agent_id) };
        if next != current {
            let mut tags: Vec<String> = report.tags.iter().filter(|t| **t != current.subject).cloned().collect();
            if !tags.contains(&subject) {
                tags.push(subject.clone());
            }
            report = sqlx::query_as::<_, Report>("UPDATE world_reports SET practice = $2, tags = $3, updated_at = NOW() WHERE id = $1 RETURNING *")
                .bind(report.id)
                .bind(json!(next))
                .bind(crate::reports::clean_tags(&tags))
                .fetch_one(self.pool())
                .await?;
        }
        if let Some(c) = next.coach_agent_id.filter(|_| coach.is_some() || next.subject != current.subject) {
            self.save_coach_pref(report.owner_user_id, &subject, c).await?;
        }
        if next != current || title.is_some() {
            self.sync_lesson(&report, &next, &current).await?;
        }
        self.announce_practice(&report, by).await?;
        Ok(report)
    }

    async fn sync_lesson(&self, report: &Report, next: &Practice, current: &Practice) -> Result<()> {
        let lesson = self.ensure_lesson(report).await?;
        let org = report.org_id.ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let target = if next.subject != current.subject {
            Some(self.subject_course(report.owner_user_id, org, &next.subject, next.coach_agent_id, None).await?)
        } else {
            None
        };
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM world_items WHERE id = $1 FOR UPDATE").bind(lesson.id).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE world_items SET name = $2, data = data || jsonb_build_object('skill', $3::text, 'coach_agent_id', $4::uuid), updated_at = NOW() WHERE id = $1",
        )
        .bind(lesson.id)
        .bind(&report.title)
        .bind(&next.subject)
        .bind(next.coach_agent_id)
        .execute(&mut *tx)
        .await?;
        if let Some(course) = target {
            self.move_lesson(&mut tx, lesson.id, academy::uuid_in(&lesson.data, "adventure_id"), course.id).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_practice(&self, owner: Uuid, org: Uuid, subject: Option<&str>, query: Option<&str>, limit: i64) -> Result<Vec<PracticeBrief>> {
        let subject = subject.map(str::trim).filter(|s| !s.is_empty()).map(clean_subject).transpose()?;
        let pattern = query.map(str::trim).filter(|q| !q.is_empty()).map(crate::search::like_pattern);
        Ok(sqlx::query_as::<_, PracticeBrief>(&format!(
            "SELECT {columns},
                    (SELECT COUNT(*) FROM world_practice_attempts a WHERE a.report_id = r.id) AS attempts,
                    (SELECT MAX(a.created_at) FROM world_practice_attempts a WHERE a.report_id = r.id) AS last_attempt_at
             FROM world_reports r
             WHERE r.owner_user_id = $1 AND r.org_id = $2 AND r.practice IS NOT NULL
               AND ($3::text IS NULL OR r.practice->>'subject' = $3)
               AND ($4::text IS NULL OR r.title ILIKE $4 OR r.body ILIKE $4 OR r.practice->>'subject' ILIKE $4)
             ORDER BY r.pinned DESC, r.updated_at DESC LIMIT $5",
            columns = crate::reports::BRIEF_COLUMNS
        ))
        .bind(owner)
        .bind(org)
        .bind(subject)
        .bind(pattern)
        .bind(limit.clamp(1, 300))
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn practice_attempts(&self, report: Uuid, limit: i64, strokes: bool) -> Result<Vec<Attempt>> {
        Ok(sqlx::query_as::<_, Attempt>(&format!(
            "SELECT {ATTEMPT_COLUMNS}, CASE WHEN $3 THEN a.strokes END AS strokes FROM world_practice_attempts a
             WHERE a.report_id = $1 ORDER BY a.n DESC LIMIT $2"
        ))
        .bind(report)
        .bind(limit.clamp(1, 500))
        .bind(strokes)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn practice_attempt(&self, id: Uuid) -> Result<Option<Attempt>> {
        Ok(sqlx::query_as::<_, Attempt>(&format!("SELECT {ATTEMPT_COLUMNS}, a.strokes FROM world_practice_attempts a WHERE a.id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }

    pub async fn find_practice_attempt(&self, owner: Uuid, org: Uuid, key: &str) -> Result<Option<Attempt>> {
        let key = attempt_key(key);
        let hex = key.replace('-', "");
        if hex.len() < 6 || !key.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Ok(None);
        }
        Ok(sqlx::query_as::<_, Attempt>(&format!(
            "SELECT {ATTEMPT_COLUMNS}, a.strokes FROM world_practice_attempts a JOIN world_reports r ON r.id = a.report_id
             WHERE a.user_id = $1 AND r.org_id = $2 AND replace(a.id::text, '-', '') LIKE $3
             ORDER BY a.created_at DESC LIMIT 1"
        ))
        .bind(owner)
        .bind(org)
        .bind(format!("{hex}%"))
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn attempt_card(&self, id: Uuid) -> Result<Option<(AttemptCard, Report)>> {
        let Some(a) = self.practice_attempt(id).await? else { return Ok(None) };
        let Some(report) = self.get_report(a.report_id).await? else { return Ok(None) };
        let coach_name: Option<String> = match a.coach_agent_id {
            Some(c) => sqlx::query_scalar("SELECT name FROM agent_profiles WHERE id = $1").bind(c).fetch_optional(self.pool()).await?,
            None => None,
        };
        let card = AttemptCard {
            short_check: Check::of(a.check.as_ref()).map(|c| c.short()),
            id: a.id,
            report_id: a.report_id,
            title: report.title.clone(),
            subject: a.subject,
            coach_agent_id: a.coach_agent_id,
            coach_name,
            n: a.n,
            image_upload_id: a.image_upload_id,
            status: a.status,
            check: a.check,
            created_at: a.created_at,
            checked_at: a.checked_at,
        };
        Ok(Some((card, report)))
    }

    pub async fn practice_ready(&self, report: &Report, user: Uuid, ready: Ready) -> Result<Attempt> {
        let practice = Practice::of(report).ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let org = report.org_id.ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let strokes = parse_strokes(&ready.strokes)?;
        let note = ready.note.map(|n| n.replace('\u{2014}', " - ").trim().to_string()).filter(|n| !n.is_empty());
        if note.as_ref().is_some_and(|n| n.chars().count() > MAX_NOTE) {
            bail!("A note is at most {MAX_NOTE} characters.");
        }
        let upload = self
            .own_uploads(&[ready.image_upload_id], user, Some(org))
            .await?
            .into_iter()
            .find(|u| DRAWING_MIMES.contains(&u.mime.as_str()))
            .ok_or_else(|| anyhow!(NOT_AN_IMAGE))?;
        let lesson = self.ensure_lesson(report).await?;
        if lesson.status == "archived" {
            bail!(LESSON_REMOVED);
        }
        let report = &self.get_report(report.id).await?.ok_or_else(|| anyhow!("That practice canvas is gone."))?;
        let coach = self.current_coach(report, &practice).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM world_items WHERE id = $1 FOR UPDATE").bind(lesson.id).execute(&mut *tx).await?;
        sqlx::query("SELECT id FROM world_reports WHERE id = $1 FOR UPDATE").bind(report.id).execute(&mut *tx).await?;
        let (id, n): (Uuid, i32) = sqlx::query_as(
            "INSERT INTO world_practice_attempts (report_id, user_id, subject, coach_agent_id, n, image_upload_id, strokes, note)
             VALUES ($1, $2, $3, $4, (SELECT COALESCE(MAX(n), 0) + 1 FROM world_practice_attempts WHERE report_id = $1), $5, $6, $7)
             RETURNING id, n",
        )
        .bind(report.id)
        .bind(user)
        .bind(&practice.subject)
        .bind(coach)
        .bind(upload.id)
        .bind(json!(strokes))
        .bind(&note)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match e.as_database_error().and_then(|d| d.code()).as_deref() {
            Some("23514") => anyhow!(TOO_BIG),
            _ => e.into(),
        })?;
        sqlx::query("UPDATE challenge_attempts SET status = 'error', feedback = $3, graded_at = NOW() WHERE user_id = $1 AND challenge_id = $2 AND status = 'pending'")
            .bind(user)
            .bind(lesson.id)
            .bind(format!("Replaced by attempt {n}."))
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO challenge_attempts (id, user_id, challenge_id, answer, grader_agent_id) VALUES ($1, $2, $3, $4, $5)")
            .bind(id)
            .bind(user)
            .bind(lesson.id)
            .bind(note.clone().unwrap_or_else(|| format!("Canvas attempt {n}")))
            .bind(coach)
            .execute(&mut *tx)
            .await?;
        let attempt = self.emit_ready(&mut tx, report, id, coach, &strokes).await?;
        sqlx::query("UPDATE world_reports SET updated_at = NOW() WHERE id = $1").bind(report.id).execute(&mut *tx).await?;
        tx.commit().await?;
        self.save_coach_pref(user, &practice.subject, coach).await?;
        Ok(attempt)
    }

    async fn current_coach(&self, report: &Report, practice: &Practice) -> Result<Uuid> {
        let org = report.org_id.ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        if let Some(lesson) = match report.challenge_id {
            Some(id) => self.get_item(id).await?,
            None => None,
        } {
            if let Some(c) = self.coach_for(&lesson).await? {
                return Ok(c);
            }
        }
        match practice.coach_agent_id {
            Some(c) if self.owned_coach(report.owner_user_id, Some(org), c).await? => Ok(c),
            _ => self.default_coach(report.owner_user_id, org, &practice.subject).await?.ok_or_else(|| anyhow!(NO_COACH)),
        }
    }

    async fn emit_ready(&self, tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, report: &Report, id: Uuid, coach: Uuid, strokes: &Strokes) -> Result<Attempt> {
        let attempt = sqlx::query_as::<_, Attempt>(&format!("SELECT {ATTEMPT_COLUMNS}, NULL::jsonb AS strokes FROM world_practice_attempts a WHERE a.id = $1"))
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        let history = sqlx::query_as::<_, Attempt>(&format!(
            "SELECT {ATTEMPT_COLUMNS}, NULL::jsonb AS strokes FROM world_practice_attempts a WHERE a.report_id = $1 AND a.n < $2 ORDER BY a.n DESC LIMIT $3"
        ))
        .bind(report.id)
        .bind(attempt.n)
        .bind(HISTORY)
        .fetch_all(&mut **tx)
        .await?;
        let upload: Option<(String, String)> = sqlx::query_as("SELECT mime, name FROM world_uploads WHERE id = $1").bind(attempt.image_upload_id).fetch_optional(&mut **tx).await?;
        let attachments: Vec<Value> = match (attempt.image_upload_id, upload) {
            (Some(upload_id), Some((mime, name))) => vec![json!({ "id": upload_id, "mime": mime, "name": name })],
            _ => Vec::new(),
        };
        let author: Option<String> = sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(attempt.user_id).fetch_optional(&mut **tx).await?.flatten();
        let event = NewEvent::new("practice.ready").agent(coach).actor(Some(attempt.user_id)).task(report.task_id).payload(json!({
            "report_id": report.id,
            "org_id": report.org_id,
            "owner_user_id": report.owner_user_id,
            "title": report.title,
            "subject": attempt.subject,
            "attempt_id": attempt.id,
            "n": attempt.n,
            "status": attempt.status,
            "coach_agent_id": coach,
            "image_upload_id": attempt.image_upload_id,
            "attachments": attachments,
            "stroke_count": strokes.strokes.len(),
            "strokes_summary": stroke_summary(strokes),
            "note": attempt.note,
            "history": history.iter().map(|a| json!({ "attempt_id": a.id, "n": a.n, "status": a.status, "check": a.check, "created_at": a.created_at, "line": history_line(a) })).collect::<Vec<_>>(),
            "author": author.unwrap_or_else(|| "Your person".into()),
        }));
        emit_in(tx, &event, Some((coach, "practice.ready"))).await?;
        Ok(attempt)
    }

    pub async fn resend_practice(&self, report: &Report, attempt: Uuid) -> Result<Attempt> {
        let practice = Practice::of(report).ok_or_else(|| anyhow!(NOT_PRACTICE))?;
        let lesson = self.ensure_lesson(report).await?;
        if lesson.status == "archived" {
            bail!(LESSON_REMOVED);
        }
        let report = &self.get_report(report.id).await?.unwrap_or_else(|| report.clone());
        let coach = self.current_coach(report, &practice).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM world_items WHERE id = $1 FOR UPDATE").bind(lesson.id).execute(&mut *tx).await?;
        let found: Option<(String, Option<Uuid>)> =
            sqlx::query_as("SELECT status, image_upload_id FROM world_practice_attempts WHERE id = $1 AND report_id = $2 FOR UPDATE")
                .bind(attempt)
                .bind(report.id)
                .fetch_optional(&mut *tx)
                .await?;
        let ledger: Option<String> = sqlx::query_scalar("SELECT status FROM challenge_attempts WHERE id = $1 FOR UPDATE").bind(attempt).fetch_optional(&mut *tx).await?;
        match found {
            Some((status, _)) if status != "waiting" || ledger.as_deref().is_some_and(|s| s != "pending") => return Err(Refusal::NotWaiting.into()),
            Some((_, None)) => return Err(Refusal::NoImage.into()),
            Some(_) => {}
            None => return Err(Refusal::Missing.into()),
        }
        sqlx::query("UPDATE challenge_attempts SET grader_agent_id = $2 WHERE id = $1").bind(attempt).bind(coach).execute(&mut *tx).await?;
        let strokes: Value = sqlx::query_scalar("UPDATE world_practice_attempts SET coach_agent_id = $2 WHERE id = $1 RETURNING strokes")
            .bind(attempt)
            .bind(coach)
            .fetch_one(&mut *tx)
            .await?;
        let recent: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM world_events WHERE type = 'practice.ready' AND payload->>'attempt_id' = $1::text AND created_at > NOW() - make_interval(secs => $2))",
        )
        .bind(attempt)
        .bind(RESEND_SECS as f64)
        .fetch_one(&mut *tx)
        .await?;
        if recent {
            return Err(Refusal::TooSoon.into());
        }
        let strokes: Strokes = serde_json::from_value(strokes)?;
        let sent = self.emit_ready(&mut tx, report, attempt, coach, &strokes).await?;
        tx.commit().await?;
        Ok(sent)
    }

    pub async fn resolve_attempt(&self, viewer: Uuid, role: crate::access::Role, org: Uuid, key: &str) -> Result<Uuid> {
        let key = attempt_key(key);
        if let Ok(id) = Uuid::parse_str(&key) {
            return Ok(id);
        }
        let hex = key.replace('-', "");
        if hex.len() < MIN_PREFIX || hex.len() > 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Refusal::Missing.into());
        }
        let found: Vec<Uuid> = sqlx::query_scalar(
            "SELECT a.id FROM world_practice_attempts a JOIN world_reports r ON r.id = a.report_id
             WHERE r.org_id = $2 AND (r.owner_user_id = $1 OR r.visibility = 'public' OR $4)
               AND replace(a.id::text, '-', '') LIKE $3 LIMIT 2",
        )
        .bind(viewer)
        .bind(org)
        .bind(format!("{hex}%"))
        .bind(role == crate::access::Role::Admin)
        .fetch_all(self.pool())
        .await?;
        match found.as_slice() {
            [id] => Ok(*id),
            [] => Err(Refusal::Missing.into()),
            _ => Err(Refusal::Ambiguous.into()),
        }
    }

    pub async fn expand_practice_quote(&self, viewer: Uuid, role: crate::access::Role, org: Uuid, quote: &mut Value) -> Result<()> {
        let Some(key) = quote.get("source").and_then(Value::as_str).and_then(|s| s.strip_prefix("practice:")).map(String::from) else { return Ok(()) };
        let readable = match self.resolve_attempt(viewer, role, org, &key).await {
            Ok(id) => self.attempt_card(id).await?.filter(|(_, report)| crate::reports::can_read(report, viewer, role, org)),
            Err(e) if e.downcast_ref::<Refusal>().is_some() => None,
            Err(e) => return Err(e),
        };
        let Some(object) = quote.as_object_mut() else { return Ok(()) };
        match readable {
            Some((card, _)) => {
                let text = quote_text(&card.subject, card.n, Check::of(card.check.as_ref()).as_ref());
                object.insert("text".into(), json!(text.chars().take(crate::quotes::TEXT_CHARS).collect::<String>()));
                object.insert("source".into(), json!(format!("practice:{}", card.id)));
                object.insert("label".into(), json!(card.title));
            }
            None => {
                object.remove("source");
            }
        }
        Ok(())
    }

    pub async fn practice_check(&self, coach: &WorldAgent, attempt: Uuid, check: Check, passed: Option<bool>) -> Result<Attempt> {
        let report: Option<Uuid> = sqlx::query_scalar("SELECT report_id FROM world_practice_attempts WHERE id = $1").bind(attempt).fetch_optional(self.pool()).await?;
        if let Some(report) = match report {
            Some(id) => self.get_report(id).await?,
            None => None,
        } {
            if report.challenge_id.is_none() && Practice::of(&report).is_some() {
                self.ensure_lesson(&report).await?;
            }
        }
        let mut tx = self.pool().begin().await?;
        let lesson: Option<Uuid> = sqlx::query_scalar("SELECT r.challenge_id FROM world_practice_attempts a JOIN world_reports r ON r.id = a.report_id WHERE a.id = $1")
            .bind(attempt)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
        if let Some(lesson) = lesson {
            sqlx::query("SELECT id FROM world_items WHERE id = $1 FOR UPDATE").bind(lesson).execute(&mut *tx).await?;
        }
        let updated = sqlx::query_as::<_, Attempt>(&format!(
            "UPDATE world_practice_attempts a SET status = 'checked', \"check\" = $3, checked_at = NOW(), coach_agent_id = $2
             WHERE a.id = $1 AND a.status = 'waiting'
               AND (a.coach_agent_id = $2
                    OR (a.coach_agent_id IS NULL AND EXISTS (
                      SELECT 1 FROM world_reports r LEFT JOIN world_items i ON i.id = r.challenge_id
                      WHERE r.id = a.report_id AND (r.practice->>'coach_agent_id' = $2::text OR i.data->>'coach_agent_id' = $2::text))))
             RETURNING {ATTEMPT_COLUMNS}, NULL::jsonb AS strokes"
        ))
        .bind(attempt)
        .bind(coach.id)
        .bind(json!(check))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(mut saved) = updated else {
            let found: Option<(String, Option<Uuid>, i32)> =
                sqlx::query_as("SELECT status, coach_agent_id, n FROM world_practice_attempts WHERE id = $1").bind(attempt).fetch_optional(&mut *tx).await?;
            match found {
                None => bail!("No practice attempt {attempt}."),
                Some((status, _, n)) if status == "checked" => bail!("Attempt {n} is already checked; each attempt gets one check. Wait for the next practice.ready."),
                Some((_, None, _)) => bail!("Attempt {attempt} lost its coach; only the canvas's current coach can check it."),
                Some(_) => bail!("Attempt {attempt} was sent to another coach."),
            }
        };
        let (title, task_id, owner, org): (String, Option<Uuid>, Uuid, Option<Uuid>) =
            sqlx::query_as("SELECT title, task_id, owner_user_id, org_id FROM world_reports WHERE id = $1").bind(saved.report_id).fetch_one(&mut *tx).await?;
        let grade = Grade {
            passed: passed.unwrap_or_else(|| academy::passed_from_score(check.score)),
            score: check.score.map(|s| s.round().clamp(0.0, 100.0) as i32),
            feedback: ledger_feedback(&check),
        };
        sqlx::query(
            "INSERT INTO challenge_attempts (id, user_id, challenge_id, answer, grader_agent_id)
             SELECT a.id, a.user_id, r.challenge_id, COALESCE(NULLIF(btrim(a.note), ''), 'Canvas attempt ' || a.n), $2
             FROM world_practice_attempts a JOIN world_reports r ON r.id = a.report_id
             WHERE a.id = $1 AND r.challenge_id IS NOT NULL
             ON CONFLICT DO NOTHING",
        )
        .bind(saved.id)
        .bind(coach.id)
        .execute(&mut *tx)
        .await?;
        let graded = self.grade_in(&mut tx, saved.id, Some(grade), None, Some(coach.id)).await?;
        if let Some(g) = &graded {
            saved.grade_status = Some(g.attempt.status.clone());
            saved.grade_score = g.attempt.score;
            saved.xp_awarded = Some(g.attempt.xp_awarded);
        }
        let mut body = check.message(saved.n, &title, saved.id);
        if let Some(g) = &graded {
            let verdict = if g.attempt.status == "passed" { "Passed" } else { "Not passed yet" };
            let xp = if g.attempt.xp_awarded > 0 { format!(", +{} xp", g.attempt.xp_awarded) } else { String::new() };
            body = body.replacen("\n\n**Right:**", &format!("\n\n**{verdict}**{xp}\n\n**Right:**"), 1);
        }
        let meta = json!({ "practice": { "attempt_id": saved.id, "report_id": saved.report_id, "n": saved.n, "subject": saved.subject } });
        let message: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta) VALUES ($1, 'agent', $1, $2, $3) RETURNING id",
        )
        .bind(coach.id)
        .bind(&body)
        .bind(&meta)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE world_practice_attempts SET coach_message_id = $2 WHERE id = $1").bind(saved.id).bind(message).execute(&mut *tx).await?;
        saved.coach_message_id = Some(message);
        emit_in(
            &mut tx,
            &NewEvent::new("message.created").agent(coach.id).payload(json!({
                "message_id": message,
                "author": coach.name,
                "author_kind": "agent",
                "body": body,
                "practice_attempt_id": saved.id,
            })),
            None,
        )
        .await?;
        emit_in(
            &mut tx,
            &NewEvent::new("practice.checked").agent(coach.id).task(task_id).payload(json!({
                "report_id": saved.report_id,
                "org_id": org,
                "owner_user_id": owner,
                "title": title,
                "subject": saved.subject,
                "attempt_id": saved.id,
                "n": saved.n,
                "status": saved.status,
                "check": saved.check,
                "grade_status": saved.grade_status,
                "xp_awarded": saved.xp_awarded,
                "coach_agent_id": coach.id,
                "coach_message_id": message,
                "quiet": true,
            })),
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Role;

    fn point(x: f64, y: f64, t: f64) -> Value {
        json!({ "x": x, "y": y, "p": 0.5, "t": t })
    }

    fn line(points: Vec<Value>) -> Value {
        json!({ "tool": "pen", "color": "w-ink", "size": 4, "points": points })
    }

    #[test]
    fn subjects_are_lowercase_slugs() {
        assert_eq!(clean_subject("  Japanese ").unwrap(), "japanese");
        assert_eq!(clean_subject("Life drawing").unwrap(), "life-drawing");
        assert_eq!(clean_subject("kanji_N5!").unwrap(), "kanji-n5");
        assert!(clean_subject(" !! ").is_err());
        assert!(clean_subject(&"a".repeat(41)).is_err());
        assert_eq!(subject_title("life-drawing"), "Life Drawing practice");
    }

    #[test]
    fn well_formed_strokes_parse_and_fill_defaults() {
        let raw = json!({ "width": 800, "height": 600, "strokes": [line(vec![json!({ "x": 1, "y": 2 }), point(50.0, 2.0, 120.0)])] });
        let strokes = parse_strokes(&raw).unwrap();
        assert_eq!(strokes.strokes[0].points[0].p, 0.5);
        assert_eq!(strokes.strokes[0].points[0].t, 0.0);
        assert!(parse_strokes(&json!({ "width": 10, "height": 10, "strokes": [] })).unwrap().strokes.is_empty());
    }

    #[test]
    fn near_miss_strokes_are_refused_with_a_reason() {
        let base = |stroke: Value| json!({ "width": 100, "height": 100, "strokes": [stroke] });
        let cases = [
            (json!({ "width": 0, "height": 100, "strokes": [] }), "width and height"),
            (json!({ "width": 100, "strokes": [] }), "Strokes are"),
            (base(json!({ "tool": "brush", "color": "w-ink", "size": 4, "points": [point(1.0, 1.0, 0.0)] })), "tool"),
            (base(json!({ "tool": "pen", "color": "#000000", "size": 4, "points": [point(1.0, 1.0, 0.0)] })), "colour"),
            (base(json!({ "tool": "pen", "color": "w-ink", "size": 0, "points": [point(1.0, 1.0, 0.0)] })), "size"),
            (base(json!({ "tool": "pen", "color": "w-ink", "size": 4, "points": [] })), "no points"),
            (base(json!({ "tool": "pen", "color": "w-ink", "size": 4, "points": [{ "x": 1, "y": 1, "p": 1.2, "t": 0 }] })), "pressure"),
            (base(json!({ "tool": "pen", "color": "w-ink", "size": 4, "points": [{ "x": 1, "y": 1, "p": 0.2, "t": -1 }] })), "time t"),
            (base(json!({ "tool": "pen", "color": "w-ink", "size": 4, "points": [{ "x": 1e9, "y": 1, "p": 0.2, "t": 0 }] })), "outside"),
        ];
        for (raw, reason) in cases {
            let error = parse_strokes(&raw).unwrap_err().to_string();
            assert!(error.contains(reason), "{raw} gave {error}");
        }
    }

    #[test]
    fn oversized_strokes_are_refused() {
        let points: Vec<Value> = (0..60_000).map(|i| point(i as f64 % 100.0, 1.0, i as f64)).collect();
        let raw = json!({ "width": 100, "height": 100, "strokes": [line(points)] });
        assert!(parse_strokes(&raw).unwrap_err().to_string().contains("MB"));
        let many: Vec<Value> = (0..2001).map(|_| line(vec![point(1.0, 1.0, 0.0)])).collect();
        assert!(parse_strokes(&json!({ "width": 100, "height": 100, "strokes": many })).unwrap_err().to_string().contains("at most 2000 strokes"));
    }

    #[test]
    fn summary_names_order_and_direction_of_each_stroke() {
        let raw = json!({ "width": 400, "height": 400, "strokes": [
            line(vec![point(50.0, 100.0, 0.0), point(350.0, 102.0, 300.0)]),
            line(vec![point(200.0, 20.0, 500.0), point(201.0, 380.0, 900.0)]),
            line(vec![point(300.0, 50.0, 1000.0), point(100.0, 300.0, 1300.0)]),
            line(vec![point(10.0, 10.0, 1400.0), point(11.0, 10.0, 1410.0)]),
            line(vec![point(100.0, 100.0, 1500.0), point(200.0, 100.0, 1600.0), point(200.0, 200.0, 1700.0), point(100.0, 200.0, 1800.0), point(100.0, 110.0, 1900.0)]),
        ] });
        let summary = stroke_summary(&parse_strokes(&raw).unwrap());
        assert!(summary.starts_with("5 strokes on a 400x400 canvas (pen 5), drawn over 1.9 s"), "{summary}");
        assert!(summary.contains("1. pen left to right, 2 points, (50, 100) to (350, 102), length 300"));
        assert!(summary.contains("2. pen top to bottom"));
        assert!(summary.contains("3. pen down-left"));
        assert!(summary.contains("4. pen dot"));
        assert!(summary.contains("5. pen closed loop"));
        assert_eq!(direction(-10.0, 0.0), "right to left");
        assert_eq!(direction(0.0, -10.0), "bottom to top");
        assert_eq!(direction(10.0, 10.0), "down-right");
        assert_eq!(direction(10.0, -10.0), "up-right");
    }

    #[test]
    fn checks_need_every_part_and_a_sane_score() {
        let check = Check::new(" Good balance ", "Stroke 3 \u{2014} too short", "Write 水 five times", Some(72.04)).unwrap();
        assert_eq!(check.right, "Good balance");
        assert_eq!(check.fix, "Stroke 3  -  too short");
        assert_eq!(check.score, Some(72.0));
        assert!(Check::new("a", "", "c", None).is_err());
        assert!(Check::new("a", "b", "c", Some(101.0)).is_err());
        assert!(Check::new("a", "b", "c", Some(f64::NAN)).is_err());
        let id = Uuid::nil();
        let message = check.message(3, "Kanji", id);
        assert!(message.starts_with("Attempt 3 of \"Kanji\"\n\n**Score:** 72/100"));
        assert!(message.ends_with(&format!("practice:{id}")));
        assert!(check.short().starts_with("72/100. Fix: Stroke 3"));
    }

    #[test]
    fn strokes_keep_a_checked_lowercase_rgb() {
        let stroke = |rgb: Value| json!({ "width": 10, "height": 10, "strokes": [{ "tool": "pen", "color": "w-ink", "rgb": rgb, "size": 2, "points": [point(1.0, 1.0, 0.0)] }] });
        let parsed = parse_strokes(&stroke(json!("#1F2933"))).unwrap();
        assert_eq!(parsed.strokes[0].rgb.as_deref(), Some("#1f2933"));
        assert_eq!(serde_json::to_value(&parsed).unwrap()["strokes"][0]["rgb"], "#1f2933");
        assert!(serde_json::to_value(parse_strokes(&json!({ "width": 10, "height": 10, "strokes": [line(vec![point(1.0, 1.0, 0.0)])] })).unwrap()).unwrap()["strokes"][0].get("rgb").is_none());
        for bad in ["1f2933", "#1f293", "#1f29334", "#gg2933", "red"] {
            assert!(parse_strokes(&stroke(json!(bad))).unwrap_err().to_string().contains("rgb"), "{bad}");
        }
    }

    #[test]
    fn practice_refs_are_valid_card_and_canvas_targets() {
        let spec = json!({ "root": "r", "elements": { "r": { "type": "Ref", "props": { "ref": "practice:abcd1234" } } } });
        assert!(crate::ui_cards::normalize(&spec).is_ok());
        let canvas = Canvas::from_raw(&json!({ "items": [{ "id": "a", "kind": "ref", "target": "practice:abcd1234" }] }), "person").unwrap();
        assert!(canvas.validate().is_ok());
        assert!(Canvas::from_raw(&json!({ "items": [{ "id": "a", "kind": "ref", "target": "drill:abcd1234" }] }), "person").is_err());
    }

    #[test]
    fn attempt_keys_drop_the_ref_prefix() {
        assert_eq!(attempt_key(" practice:ABC123 "), "abc123");
        assert_eq!(attempt_key("abc123"), "abc123");
    }

    async fn agent(store: &WorldStore, owner: Uuid, tag: &str, name: &str) -> Result<Uuid> {
        Ok(sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, $2, 'claude', 'sonnet', $3) RETURNING id")
            .bind(format!("{name}-{tag}"))
            .bind(name)
            .bind(owner)
            .fetch_one(store.pool())
            .await?)
    }

    #[tokio::test]
    async fn practice_attempts_count_up_and_check_once() -> Result<()> {
        let Ok(url) = std::env::var("PRACTICE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Practice tester') RETURNING id").bind(format!("{tag}@practice.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        let org = crate::orgs::DEFAULT_ORG;
        let hana = agent(&store, owner, &tag, "hana").await?;
        let vincent = agent(&store, owner, &tag, "vincent").await?;
        let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Other') RETURNING id").bind(format!("{tag}-x@practice.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, stranger, None).await?;
        let theirs = agent(&store, stranger, &tag, "theirs").await?;

        assert_eq!(store.find_coach(owner, org, "@HANA").await?, hana);
        assert!(store.find_coach(owner, org, &format!("theirs-{tag}")).await.is_err());
        assert!(store.create_practice(owner, org, ReportActor::User(owner), "japanese", None, Some(theirs), None).await.is_err());

        let canvas = store.create_practice(owner, org, ReportActor::User(owner), "Japanese", None, Some(hana), None).await?;
        assert_eq!(canvas.kind, "canvas");
        assert_eq!(canvas.title, "Japanese practice");
        assert_eq!(Practice::of(&canvas), Some(Practice { subject: "japanese".into(), coach_agent_id: Some(hana) }));
        assert_eq!(store.default_coach(owner, org, "japanese").await?, Some(hana));
        let second = store.create_practice(owner, org, ReportActor::User(owner), "japanese", Some("Kana"), None, None).await?;
        assert_eq!(Practice::of(&second).and_then(|p| p.coach_agent_id), Some(hana));

        let drawing = store.update_practice(&second, ReportActor::User(owner), Some("drawing"), Some(vincent), None).await?;
        assert_eq!(Practice::of(&drawing), Some(Practice { subject: "drawing".into(), coach_agent_id: Some(vincent) }));
        assert!(drawing.tags.contains(&"drawing".to_string()) && !drawing.tags.contains(&"japanese".to_string()));
        assert_eq!(store.default_coach(owner, org, "drawing").await?, Some(vincent));
        assert_eq!(store.list_practice(owner, org, Some("japanese"), None, 50).await?.len(), 1);
        assert_eq!(store.list_practice(owner, org, None, Some("kana"), 50).await?.len(), 1);

        let png: Uuid = sqlx::query_scalar("INSERT INTO world_uploads (mime, name, size, bytes, uploaded_by, org_id) VALUES ('image/png', 'a.png', 3, '\\x010203', $1, $2) RETURNING id")
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let theirs_png: Uuid = sqlx::query_scalar("INSERT INTO world_uploads (mime, name, size, bytes, uploaded_by, org_id) VALUES ('image/png', 'b.png', 3, '\\x010203', $1, $2) RETURNING id")
            .bind(stranger)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let strokes = json!({ "width": 100, "height": 100, "strokes": [line(vec![point(10.0, 50.0, 0.0), point(90.0, 50.0, 200.0)])] });
        let ready = |image: Uuid| Ready { image_upload_id: image, strokes: strokes.clone(), note: Some("first try".into()) };
        let heic: Uuid = sqlx::query_scalar("INSERT INTO world_uploads (mime, name, size, bytes, uploaded_by, org_id) VALUES ('image/heic', 'a.heic', 3, '\\x010203', $1, $2) RETURNING id")
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        assert!(store.practice_ready(&canvas, owner, ready(theirs_png)).await.unwrap_err().to_string().contains("image"));
        assert_eq!(store.practice_ready(&canvas, owner, ready(heic)).await.unwrap_err().to_string(), NOT_AN_IMAGE);
        let first = store.practice_ready(&canvas, owner, ready(png)).await?;
        let second_try = store.practice_ready(&canvas, owner, ready(png)).await?;
        assert_eq!((first.n, second_try.n, first.status.as_str()), (1, 2, "waiting"));
        assert_eq!(first.coach_agent_id, Some(hana));

        let (kind, payload): (String, Value) = sqlx::query_as("SELECT kind, payload FROM agent_inbox WHERE agent_id = $1 ORDER BY id DESC LIMIT 1").bind(hana).fetch_one(&pool).await?;
        assert_eq!(kind, "practice.ready");
        assert_eq!(payload["n"], 2);
        assert_eq!(payload["attachments"][0]["id"], json!(png));
        assert_eq!(payload["from_owner"], true);
        assert!(payload["strokes_summary"].as_str().unwrap_or("").contains("1. pen left to right"));
        assert_eq!(payload["history"].as_array().map(Vec::len), Some(1));

        let hana_agent = store.get_agent(hana).await?.ok_or_else(|| anyhow!("no hana"))?;
        let vincent_agent = store.get_agent(vincent).await?.ok_or_else(|| anyhow!("no vincent"))?;
        let check = Check::new("Even spacing", "Stroke 1 starts too far left", "Write 三 three times", Some(80.0))?;
        assert!(store.practice_check(&vincent_agent, first.id, check.clone(), None).await.unwrap_err().to_string().contains("another coach"));
        let checked = store.practice_check(&hana_agent, first.id, check.clone(), None).await?;
        assert_eq!(checked.status, "checked");
        assert!(store.practice_check(&hana_agent, first.id, check, None).await.unwrap_err().to_string().contains("already checked"));
        let message: (String, Value) = sqlx::query_as("SELECT body, meta FROM agent_messages WHERE id = $1").bind(checked.coach_message_id).fetch_one(&pool).await?;
        assert!(message.0.contains(&format!("practice:{}", first.id)));
        assert_eq!(message.1["practice"]["n"], 1);
        let bell: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_notifications WHERE user_id = $1 AND kind = 'message'").bind(owner).fetch_one(&pool).await?;
        assert_eq!(bell, 1);

        sqlx::query("UPDATE world_practice_attempts SET coach_agent_id = NULL WHERE id = $1").bind(second_try.id).execute(&pool).await?;
        let check = Check::new("Steady line", "Lift at the end", "Write 一 again", None)?;
        assert!(store.practice_check(&vincent_agent, second_try.id, check.clone(), None).await.unwrap_err().to_string().contains("lost its coach"));
        let adopted = store.practice_check(&hana_agent, second_try.id, check, None).await?;
        assert_eq!((adopted.status.as_str(), adopted.coach_agent_id), ("checked", Some(hana)));

        let points: Vec<Value> = (0..40_000).map(|i| point((i % 997) as f64 + 0.25, (i % 613) as f64 + 0.5, i as f64)).collect();
        let big = json!({ "width": 1000, "height": 1000, "strokes": [line(points)] });
        assert!(serde_json::to_vec(&parse_strokes(&big)?)?.len() > 1_500_000);
        let large = store.practice_ready(&canvas, owner, Ready { image_upload_id: png, strokes: big, note: None }).await?;
        assert_eq!(large.n, 3);

        let attempts = store.practice_attempts(canvas.id, 10, false).await?;
        assert_eq!(attempts.iter().map(|a| a.n).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert!(attempts[0].strokes.is_none() && attempts[0].stroke_count == 1);
        let (card, card_report) = store.attempt_card(first.id).await?.ok_or_else(|| anyhow!("no card"))?;
        assert_eq!(card.short_check.as_deref(), Some("80/100. Fix: Stroke 1 starts too far left"));
        assert_eq!((card.coach_name.as_deref(), card.subject.as_str()), (Some("hana"), "japanese"));
        assert!(crate::reports::can_read(&card_report, owner, crate::access::Role::Write, org));
        assert!(!crate::reports::can_read(&card_report, stranger, crate::access::Role::Write, org));
        assert!(store.find_practice_attempt(owner, org, &format!("practice:{}", &first.id.to_string()[..8])).await?.is_some());

        let short = first.id.simple().to_string()[..MIN_PREFIX].to_string();
        assert_eq!(store.resolve_attempt(owner, Role::Write, org, &format!("practice:{short}")).await?, first.id);
        assert_eq!(store.resolve_attempt(owner, Role::Write, org, &first.id.to_string()).await?, first.id);
        let refusal = |r: Result<Uuid>| r.unwrap_err().downcast::<Refusal>().ok();
        assert_eq!(refusal(store.resolve_attempt(stranger, Role::Write, org, &short).await), Some(Refusal::Missing));
        assert_eq!(refusal(store.resolve_attempt(owner, Role::Write, org, &short[..6]).await), Some(Refusal::Missing));
        let twin = &tag[..8];
        for (i, n) in [(1, 90), (2, 91)] {
            sqlx::query("INSERT INTO world_practice_attempts (id, report_id, user_id, subject, n, strokes) VALUES ($1, $2, $3, 'japanese', $4, '{\"width\":1,\"height\":1,\"strokes\":[]}')")
                .bind(Uuid::parse_str(&format!("{twin}-0000-4000-8000-00000000000{i}"))?)
                .bind(canvas.id)
                .bind(owner)
                .bind(n)
                .execute(&pool)
                .await?;
        }
        assert_eq!(refusal(store.resolve_attempt(owner, Role::Write, org, twin).await), Some(Refusal::Ambiguous));
        let twin_one = Uuid::parse_str(&format!("{twin}-0000-4000-8000-000000000001"))?;
        assert_eq!(store.resend_practice(&canvas, twin_one).await.map(|a| a.id).unwrap_err().to_string(), NO_IMAGE_TO_RESEND);

        let viewer = |user: Uuid| crate::access::Viewer { user, org: Some(org), role: Role::Write, org_role: None, instance_admin: false };
        assert!(store.upload_for_viewer(png, &viewer(owner)).await?.is_some());
        assert!(store.upload_for_viewer(png, &viewer(stranger)).await?.is_none());
        sqlx::query("UPDATE world_reports SET visibility = 'public' WHERE id = $1").bind(canvas.id).execute(&pool).await?;
        assert!(store.upload_for_viewer(png, &viewer(stranger)).await?.is_some());
        assert_eq!(store.resolve_attempt(stranger, Role::Write, org, &short).await?, first.id);
        assert_eq!(refusal(store.resolve_attempt(stranger, Role::Write, org, twin).await), Some(Refusal::Ambiguous));
        sqlx::query("UPDATE world_reports SET visibility = 'private' WHERE id = $1").bind(canvas.id).execute(&pool).await?;
        assert!(store.upload_for_viewer(png, &viewer(stranger)).await?.is_none());

        let quoted = |source: String| crate::quotes::clean(Some(&json!({ "text": "x", "source": source }))).unwrap();
        let mut quote = quoted(format!("practice:{short}"));
        store.expand_practice_quote(owner, Role::Write, org, &mut quote).await?;
        assert!(quote["text"].as_str().unwrap_or("").starts_with("Japanese practice, attempt 1, score 80/100. Right: Even spacing"));
        assert_eq!((quote["source"].clone(), quote["label"].clone()), (json!(format!("practice:{}", first.id)), json!("Japanese practice")));
        let mut waiting = quoted(format!("practice:{}", second_try.id));
        store.expand_practice_quote(owner, Role::Write, org, &mut waiting).await?;
        assert!(waiting["text"].as_str().unwrap_or("").contains("attempt 2"));
        let mut hidden = quoted(format!("practice:{}", first.id));
        store.expand_practice_quote(stranger, Role::Write, org, &mut hidden).await?;
        assert_eq!((hidden["text"].as_str(), hidden.get("source")), (Some("x"), None));

        let moved = store.update_practice(&canvas, ReportActor::User(owner), None, Some(vincent), None).await?;
        sqlx::query("UPDATE world_events SET created_at = NOW() - interval '1 hour' WHERE type = 'practice.ready' AND payload->>'attempt_id' = $1")
            .bind(large.id.to_string())
            .execute(&pool)
            .await?;
        let resent = store.resend_practice(&moved, large.id).await?;
        assert_eq!((resent.n, resent.coach_agent_id), (3, Some(vincent)));
        let (kind, payload): (String, Value) = sqlx::query_as("SELECT kind, payload FROM agent_inbox WHERE agent_id = $1 ORDER BY id DESC LIMIT 1").bind(vincent).fetch_one(&pool).await?;
        assert_eq!((kind.as_str(), payload["attempt_id"].as_str()), ("practice.ready", Some(large.id.to_string().as_str())));
        assert_eq!(payload["history"].as_array().map(Vec::len), Some(2));
        assert_eq!(payload["attachments"][0]["id"], json!(png));
        assert_eq!(store.resend_practice(&moved, large.id).await.map(|a| a.id).unwrap_err().downcast::<Refusal>().ok(), Some(Refusal::TooSoon));
        assert_eq!(store.resend_practice(&moved, first.id).await.map(|a| a.id).unwrap_err().downcast::<Refusal>().ok(), Some(Refusal::NotWaiting));

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1 OR agent_id = ANY($2)").bind(owner.to_string()).bind(vec![hana, vincent, theirs]).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE owner_user_id = ANY($1)").bind(vec![owner, stranger]).execute(&pool).await?;
        sqlx::query("DELETE FROM world_uploads WHERE id = ANY($1)").bind(vec![png, theirs_png, heic]).execute(&pool).await?;
        Ok(())
    }

    async fn seed_attempt(pool: &sqlx::PgPool, report: Uuid, owner: Uuid, coach: Uuid, n: i32, check: Option<Value>) -> Result<Uuid> {
        Ok(sqlx::query_scalar(
            "INSERT INTO world_practice_attempts (report_id, user_id, subject, coach_agent_id, n, strokes, status, \"check\", checked_at)
             SELECT $1, $2, practice->>'subject', $3, $4, '{\"width\":10,\"height\":10,\"strokes\":[]}', CASE WHEN $5::jsonb IS NULL THEN 'waiting' ELSE 'checked' END, $5, CASE WHEN $5::jsonb IS NULL THEN NULL ELSE NOW() END
             FROM world_reports WHERE id = $1 RETURNING id",
        )
        .bind(report)
        .bind(owner)
        .bind(coach)
        .bind(n)
        .bind(check)
        .fetch_one(pool)
        .await?)
    }

    #[tokio::test]
    async fn migration_folds_old_practice_into_courses_without_losing_attempts() -> Result<()> {
        let Ok(url) = std::env::var("PRACTICE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Old practiser') RETURNING id").bind(format!("{tag}@fold.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        let org = crate::orgs::DEFAULT_ORG;
        let hana = agent(&store, owner, &tag, "hana").await?;
        let kana = store.create_practice(owner, org, ReportActor::User(owner), "japanese", Some("Kana"), Some(hana), None).await?;
        let kanji = store.create_practice(owner, org, ReportActor::User(owner), "japanese", Some("Kanji"), Some(hana), None).await?;
        let gesture = store.create_practice(owner, org, ReportActor::User(owner), "drawing", Some("Gestures"), Some(hana), None).await?;
        sqlx::query("UPDATE world_reports SET challenge_id = NULL WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_items WHERE owner_user_id = $1 AND kind IN ('challenge', 'adventure')").bind(owner).execute(&pool).await?;
        sqlx::query("INSERT INTO user_progress (user_id, xp, level, skills, streak, best_streak, last_active_day) VALUES ($1, 10, 1, '{\"logic\": 10}', 3, 4, CURRENT_DATE)")
            .bind(owner)
            .execute(&pool)
            .await?;
        let check = |score: Option<f64>| json!({ "right": "r", "fix": "f", "next": "n", "score": score });
        let a1 = seed_attempt(&pool, kana.id, owner, hana, 1, Some(check(Some(80.0)))).await?;
        let a2 = seed_attempt(&pool, kana.id, owner, hana, 2, Some(check(Some(40.0)))).await?;
        let a3 = seed_attempt(&pool, kana.id, owner, hana, 3, None).await?;
        let a4 = seed_attempt(&pool, kana.id, owner, hana, 4, None).await?;
        let b1 = seed_attempt(&pool, kanji.id, owner, hana, 1, Some(json!({ "right": "r", "fix": "f", "next": "n" }))).await?;
        let c1 = seed_attempt(&pool, gesture.id, owner, hana, 1, Some(check(Some(59.0)))).await?;

        sqlx::raw_sql(include_str!("../../migrations/143_academy_courses.sql")).execute(&pool).await?;
        sqlx::raw_sql(include_str!("../../migrations/143_academy_courses.sql")).execute(&pool).await?;

        let courses = store.adventures_for(owner, Some(org)).await?;
        assert_eq!(courses.len(), 2, "one course per subject");
        let japanese = courses.iter().find(|c| c.skill.as_deref() == Some("japanese")).ok_or_else(|| anyhow!("no japanese course"))?;
        assert_eq!((japanese.title.as_str(), japanese.folder.as_deref(), japanese.coach_agent_id), ("Japanese", Some(FOLDER), Some(hana)));
        let linked: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as("SELECT id, challenge_id FROM world_reports WHERE id = ANY($1) ORDER BY created_at").bind(vec![kana.id, kanji.id]).fetch_all(&pool).await?;
        assert_eq!(japanese.challenges, linked.iter().filter_map(|(_, c)| *c).collect::<Vec<_>>(), "lessons keep the canvases' order");
        let cards = store.challenge_cards(owner, Some(org)).await?;
        assert_eq!(cards.len(), 3);
        assert!(cards.iter().all(|c| c.challenge_type == "canvas" && c.report_id.is_some()));

        let ledger = |id: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query_as::<_, (String, Option<i32>, i32)>("SELECT status, score, xp_awarded FROM challenge_attempts WHERE id = $1").bind(id).fetch_one(&pool).await }
        };
        assert_eq!(ledger(a1).await?, ("passed".into(), Some(80), 40));
        assert_eq!(ledger(a2).await?, ("failed".into(), Some(40), 0));
        assert_eq!(ledger(a3).await?.0, "error");
        assert_eq!(ledger(a4).await?.0, "pending");
        assert_eq!(ledger(b1).await?, ("passed".into(), None, 40));
        assert_eq!(ledger(c1).await?, ("failed".into(), Some(59), 0));

        let progress = store.progress(owner).await?;
        assert_eq!((progress.xp, progress.level, progress.streak, progress.best_streak), (90, academy::level_for(90), 3, 4));
        assert_eq!((progress.skills["japanese"].as_i64(), progress.skills["logic"].as_i64()), (Some(80), Some(10)));
        let unlocks: Vec<String> = progress.unlocks.as_array().map(|a| a.iter().filter_map(|u| u["id"].as_str().map(String::from)).collect()).unwrap_or_default();
        assert!(unlocks.contains(&format!("trophy:{}", japanese.id)), "a course already fully passed gets its trophy: {unlocks:?}");
        assert!(unlocks.contains(&"lantern-post".to_string()), "level unlocks follow the backfilled xp: {unlocks:?}");
        let drawing = courses.iter().find(|c| c.skill.as_deref() == Some("drawing")).ok_or_else(|| anyhow!("no drawing course"))?;
        assert!(!unlocks.contains(&format!("trophy:{}", drawing.id)), "a course with a failed lesson gets no trophy");
        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_practice_attempts WHERE report_id = ANY($1)").bind(vec![kana.id, kanji.id, gesture.id]).fetch_one(&pool).await?;
        assert_eq!(kept, 6, "the old attempts stay where they were");

        let waiting = store.get_report(kana.id).await?.ok_or_else(|| anyhow!("no kana"))?;
        assert_eq!(store.resend_practice(&waiting, a3).await.map(|a| a.id).unwrap_err().downcast::<Refusal>().ok(), Some(Refusal::NotWaiting));

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1 OR agent_id = $2").bind(owner.to_string()).bind(hana).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_items WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn canvas_lessons_grade_into_the_academy() -> Result<()> {
        let Ok(url) = std::env::var("PRACTICE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Learner') RETURNING id").bind(format!("{tag}@lesson.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        let org = crate::orgs::DEFAULT_ORG;
        let hana = agent(&store, owner, &tag, "hana").await?;
        let vincent = agent(&store, owner, &tag, "vincent").await?;
        let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Other') RETURNING id").bind(format!("{tag}-x@lesson.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, stranger, None).await?;

        let report = store.create_practice(owner, org, ReportActor::User(owner), "kanji", None, Some(hana), None).await?;
        let lesson = store.get_item(report.challenge_id.ok_or_else(|| anyhow!("practice made no lesson"))?).await?.ok_or_else(|| anyhow!("no lesson"))?;
        assert_eq!((lesson.kind.as_str(), lesson.data["type"].as_str(), lesson.data["report_id"].as_str()), ("challenge", Some("canvas"), Some(report.id.to_string().as_str())));
        let course = store.get_item(academy::uuid_in(&lesson.data, "adventure_id").ok_or_else(|| anyhow!("no course"))?).await?.ok_or_else(|| anyhow!("no course"))?;
        assert_eq!((course.name.as_str(), course.data["skill"].as_str(), course.data["challenges"].clone()), ("Kanji", Some("kanji"), json!([lesson.id])));
        assert_eq!(store.coach_for(&lesson).await?, Some(hana));
        let again = store.create_practice(owner, org, ReportActor::User(owner), "kanji", Some("Radicals"), None, None).await?;
        let again_lesson = store.get_item(again.challenge_id.unwrap_or_default()).await?.ok_or_else(|| anyhow!("no second lesson"))?;
        assert_eq!(academy::uuid_in(&again_lesson.data, "adventure_id"), Some(course.id), "the same subject shares one course");

        let png: Uuid = sqlx::query_scalar("INSERT INTO world_uploads (mime, name, size, bytes, uploaded_by, org_id) VALUES ('image/png', 'a.png', 3, '\\x010203', $1, $2) RETURNING id")
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let strokes = json!({ "width": 100, "height": 100, "strokes": [line(vec![point(10.0, 50.0, 0.0), point(90.0, 50.0, 200.0)])] });
        let ready = || Ready { image_upload_id: png, strokes: strokes.clone(), note: None };
        let first = store.practice_ready(&report, owner, ready()).await?;
        let second = store.practice_ready(&report, owner, ready()).await?;
        let status = |id: Uuid| {
            let pool = pool.clone();
            async move { sqlx::query_as::<_, (String, Option<String>)>("SELECT status, feedback FROM challenge_attempts WHERE id = $1").bind(id).fetch_one(&pool).await }
        };
        assert_eq!(status(first.id).await?, ("error".into(), Some("Replaced by attempt 2.".into())), "one pending attempt per lesson");
        assert_eq!(status(second.id).await?.0, "pending");
        assert_eq!(store.resend_practice(&report, first.id).await.map(|a| a.id).unwrap_err().downcast::<Refusal>().ok(), Some(Refusal::NotWaiting));

        let hana_agent = store.get_agent(hana).await?.ok_or_else(|| anyhow!("no hana"))?;
        let vincent_agent = store.get_agent(vincent).await?.ok_or_else(|| anyhow!("no vincent"))?;
        let good = Check::new("Balanced", "Stroke 2 is short", "Write it five times", Some(75.0))?;
        assert!(store.practice_check(&vincent_agent, second.id, good.clone(), None).await.unwrap_err().to_string().contains("another coach"));
        let graded = store.practice_check(&hana_agent, second.id, good, None).await?;
        assert_eq!((graded.grade_status.as_deref(), graded.grade_score, graded.xp_awarded), (Some("passed"), Some(75), Some(40)));
        let body: String = sqlx::query_scalar("SELECT body FROM agent_messages WHERE id = $1").bind(graded.coach_message_id).fetch_one(&pool).await?;
        assert!(body.contains("**Passed**, +40 xp"), "{body}");
        let progress = store.progress(owner).await?;
        assert_eq!((progress.xp, progress.skills["kanji"].as_i64(), progress.streak), (40, Some(40), 1));
        let next: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_inbox WHERE agent_id = $1 AND kind = 'course.next'").bind(hana).fetch_one(&pool).await?;
        assert_eq!(next, 0, "the course still has an open lesson");

        let stale = store.practice_check(&hana_agent, first.id, Check::new("ok", "ok", "ok", Some(90.0))?, None).await?;
        assert_eq!((stale.grade_status.as_deref(), stale.xp_awarded), (Some("error"), Some(0)), "a replaced attempt keeps its check but earns nothing");

        let second_report = store.get_report(again.id).await?.ok_or_else(|| anyhow!("no radicals"))?;
        let radical = store.practice_ready(&second_report, owner, ready()).await?;
        let failed = store.practice_check(&hana_agent, radical.id, Check::new("ok", "ok", "ok", Some(30.0))?, None).await?;
        assert_eq!((failed.grade_status.as_deref(), failed.xp_awarded), (Some("failed"), Some(0)));
        let retry = store.practice_ready(&second_report, owner, ready()).await?;
        store.practice_check(&hana_agent, retry.id, Check::new("ok", "ok", "ok", Some(30.0))?, Some(true)).await?;
        let (kind, payload): (String, Value) = sqlx::query_as("SELECT kind, payload FROM agent_inbox WHERE agent_id = $1 AND kind = 'course.next'").bind(hana).fetch_one(&pool).await?;
        assert_eq!((kind.as_str(), payload["title"].as_str(), payload["lessons"].as_i64()), ("course.next", Some("Kanji"), Some(2)));

        let theirs = store.create_canvas_with(
            NewReport { owner_user_id: stranger, by: ReportActor::User(stranger), title: "Theirs".into(), body: String::new(), folder: None, tags: vec![], visibility: Some("public".into()), pinned: false, task_id: None },
            Canvas::default(),
            None,
        ).await?;
        let lesson_args = |reading: Vec<String>| json!({ "title": "Stroke order", "type": "canvas", "skill": "kanji", "course": "Kanji", "reading": reading, "coach": format!("vincent-{tag}") });
        assert!(store.create_challenge(owner, org, Some(hana), &lesson_args(vec![theirs.id.to_string()]), None).await.is_err(), "reading is only the owner's own reports");
        let (made, _) = store.create_challenge(owner, org, Some(hana), &lesson_args(vec![format!("report:{}", report.id)]), None).await?;
        assert_eq!((made.data["reading"].clone(), academy::uuid_in(&made.data, "coach_agent_id")), (json!([report.id]), Some(vincent)));
        assert!(made.data["report_id"].is_string(), "a canvas lesson gets its canvas");
        assert_eq!(store.get_item(course.id).await?.map(|c| c.data["challenges"].as_array().map(Vec::len)), Some(Some(3)));
        assert!(store.edit_lesson(&made, &json!({ "type": "quiz", "choices": ["a"], "correct": [1] })).await.is_err(), "a lesson never leaves the canvas type");
        let detail = store.challenge_detail(owner, Some(org), made.id).await?.ok_or_else(|| anyhow!("no detail"))?;
        assert_eq!((detail.reading.len(), detail.coach.map(|c| c.id)), (1, Some(vincent)));
        assert!(store.challenge_cards(stranger, Some(org)).await?.is_empty(), "lessons are private to their person");

        let lessons_now = store.get_item(course.id).await?.map(|c| c.data["challenges"].clone()).unwrap_or_default();
        assert!(store.create_challenge(owner, org, Some(hana), &json!({ "title": "[[ ]]", "type": "canvas", "skill": "kanji", "course": "Kanji" }), None).await.is_err(), "a title no canvas can take fails");
        assert_eq!(store.get_item(course.id).await?.map(|c| c.data["challenges"].clone()).unwrap_or_default(), lessons_now, "the half-made lesson leaves the course");
        let stray: Vec<String> = sqlx::query_scalar("SELECT status FROM world_items WHERE owner_user_id = $1 AND kind = 'challenge' AND name = '[[ ]]'").bind(owner).fetch_all(&pool).await?;
        assert_eq!(stray, vec!["archived".to_string()]);

        let made_report = store.get_report(made.data["report_id"].as_str().unwrap_or_default().parse()?).await?.ok_or_else(|| anyhow!("no stroke order canvas"))?;
        let imported: Uuid = sqlx::query_scalar(
            "INSERT INTO world_practice_attempts (report_id, user_id, subject, coach_agent_id, n, strokes) VALUES ($1, $2, 'kanji', $3, 1, '{\"width\":10,\"height\":10,\"strokes\":[]}') RETURNING id",
        )
        .bind(made_report.id)
        .bind(owner)
        .bind(vincent)
        .fetch_one(&pool)
        .await?;
        let earned = store.practice_check(&vincent_agent, imported, Check::new("ok", "ok", "ok", Some(85.0))?, None).await?;
        assert_eq!((earned.grade_status.as_deref(), earned.xp_awarded), (Some("passed"), Some(40)), "an attempt with no ledger row still earns its xp");

        let (title, fresh) = store.edit_lesson(&made, &json!({ "title": "Brush strokes", "skill": "calligraphy", "coach": format!("hana-{tag}") })).await?.ok_or_else(|| anyhow!("nothing edited"))?;
        assert_eq!(title, "Brush strokes");
        let synced = store.get_report(made_report.id).await?.ok_or_else(|| anyhow!("canvas gone"))?;
        assert_eq!((synced.title.as_str(), Practice::of(&synced)), ("Brush strokes", Some(Practice { subject: "calligraphy".into(), coach_agent_id: Some(hana) })));
        let moved_to = store.get_item(academy::uuid_in(&fresh, "adventure_id").ok_or_else(|| anyhow!("no course"))?).await?.ok_or_else(|| anyhow!("no course"))?;
        assert_eq!(moved_to.data["skill"].as_str(), Some("calligraphy"), "a skill change moves the lesson to that skill's course");

        let travel = store.create_course(owner, org, Some(hana), &json!({ "title": "Travel Japanese", "coach": format!("vincent-{tag}") }), None).await?;
        let (phrase, _) = store.create_challenge(owner, org, Some(hana), &json!({ "title": "Station signs", "type": "canvas", "skill": "kana", "course": "Travel Japanese" }), None).await?;
        let phrase_report = phrase.data["report_id"].as_str().unwrap_or_default().parse::<Uuid>()?;
        let coach_of = |id: Uuid| {
            let store = store.clone();
            async move { Ok::<_, anyhow::Error>(store.get_report(id).await?.and_then(|r| Practice::of(&r)).and_then(|p| p.coach_agent_id)) }
        };
        assert_eq!(coach_of(phrase_report).await?, Some(vincent), "the canvas takes the course coach");
        let (_, kept) = store.edit_lesson(&phrase, &json!({ "title": "Station signs, day two", "skill": "kanji" })).await?.ok_or_else(|| anyhow!("nothing edited"))?;
        assert_eq!(coach_of(phrase_report).await?, Some(vincent), "an edit that does not name a coach keeps the canvas coach");
        assert_eq!(academy::uuid_in(&kept, "adventure_id"), Some(travel.id), "a skill change leaves a course the person made alone");
        assert_eq!(store.get_report(phrase_report).await?.and_then(|r| Practice::of(&r)).map(|p| p.subject), Some("kanji".to_string()));

        sqlx::query("UPDATE world_items SET status = 'archived' WHERE id = $1").bind(made.id).execute(&pool).await?;
        assert_eq!(store.practice_ready(&synced, owner, ready()).await.unwrap_err().to_string(), LESSON_REMOVED);

        store.delete_report(again.id, ReportActor::User(owner)).await?;
        assert_eq!(store.get_item(again_lesson.id).await?.map(|i| i.status), Some("archived".into()), "deleting a lesson's canvas archives the lesson");

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1 OR agent_id = ANY($2)").bind(owner.to_string()).bind(vec![hana, vincent]).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = ANY($1)").bind(vec![owner, stranger]).execute(&pool).await?;
        sqlx::query("DELETE FROM world_items WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_uploads WHERE id = $1").bind(png).execute(&pool).await?;
        Ok(())
    }

    #[tokio::test]
    async fn practice_this_makes_one_lesson_per_report_and_renames_stay_in_sync() -> Result<()> {
        let Ok(url) = std::env::var("PRACTICE_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Reader') RETURNING id").bind(format!("{tag}@this.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, owner, None).await?;
        let org = crate::orgs::DEFAULT_ORG;
        let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Other') RETURNING id").bind(format!("{tag}-x@this.test")).fetch_one(&pool).await?;
        crate::orgs::join_default_for_tests(&pool, stranger, None).await?;
        let new = |title: &str, tags: Vec<String>| NewReport {
            owner_user_id: owner,
            by: ReportActor::User(owner),
            title: title.into(),
            body: "Raft elects a leader, then replicates a log.".into(),
            folder: Some("Notes".into()),
            tags,
            visibility: None,
            pinned: false,
            task_id: None,
        };

        let doc = store.create_report(new("Raft in a page", vec!["distributed-systems".into()])).await?;
        let (lesson, created) = store.practice_this(owner, org, doc.id).await?;
        assert!(created);
        assert_eq!((lesson.data["type"].as_str(), lesson.data["skill"].as_str()), (Some("reading"), Some("distributed-systems")));
        assert_eq!((lesson.data["reading"].clone(), lesson.data["source_report"].as_str()), (json!([doc.id]), Some(doc.id.to_string().as_str())));
        assert!(lesson.data["rubric"].is_string() && academy::uuid_in(&lesson.data, "adventure_id").is_some());
        let (again, created_again) = store.practice_this(owner, org, doc.id).await?;
        assert_eq!((again.id, created_again), (lesson.id, false), "practising the same report opens its lesson");
        assert!(store.practice_this(stranger, org, doc.id).await.is_err(), "only the owner practises a report");
        let excerpts = store.reading_excerpts(owner, &lesson).await?;
        assert_eq!(excerpts, vec![("Raft in a page".to_string(), "Raft elects a leader, then replicates a log.".to_string())]);

        let board = store.create_canvas_with(new("Hiragana chart", vec![]), Canvas::default(), None).await?;
        let (drawn, _) = store.practice_this(owner, org, board.id).await?;
        let pad = academy::uuid_in(&drawn.data, "report_id").ok_or_else(|| anyhow!("no practice canvas"))?;
        assert_ne!(pad, board.id, "the source canvas stays as reading, practice gets its own pad");
        assert_eq!((drawn.data["type"].as_str(), drawn.data["reading"].clone()), (Some("canvas"), json!([board.id])));
        assert_eq!(store.practice_this(owner, org, board.id).await?.0.id, drawn.id);

        let practice = store.create_practice(owner, org, ReportActor::User(owner), "kanji", Some("Numbers"), None, None).await?;
        let (own, made) = store.practice_this(owner, org, practice.id).await?;
        assert_eq!((Some(own.id), made), (practice.challenge_id, false), "a practice canvas already is its lesson");

        let renamed = store.update_report(pad, ReportActor::User(owner), &ReportPatch { title: Some("Hiragana from memory".into()), ..Default::default() }).await?.ok_or_else(|| anyhow!("no pad"))?;
        assert_eq!(renamed.title, "Hiragana from memory");
        assert_eq!(store.get_item(drawn.id).await?.map(|i| i.name), Some("Hiragana from memory".to_string()), "renaming the canvas renames its lesson");
        let current = store.get_item(drawn.id).await?.ok_or_else(|| anyhow!("no lesson"))?;
        let (title, _) = store.edit_lesson(&current, &json!({ "title": "Kana drills" })).await?.ok_or_else(|| anyhow!("nothing edited"))?;
        assert_eq!(title, "Kana drills");
        assert_eq!(store.get_report(pad).await?.map(|r| r.title), Some("Kana drills".to_string()), "renaming the lesson renames its canvas");
        assert_eq!(store.get_item(drawn.id).await?.map(|i| i.name), Some("Kana drills".to_string()));

        let linked = store.get_report(practice.id).await?.ok_or_else(|| anyhow!("no practice"))?;
        assert_eq!(store.live_lesson(&linked).await?, Some(own.id));
        store.update_item(own.id, None, None, Some("archived")).await?;
        let removed = store.get_report(practice.id).await?.ok_or_else(|| anyhow!("no practice"))?;
        assert_eq!(store.live_lesson(&removed).await?, None, "a removed lesson is no lesson for its canvas");
        assert_eq!(store.practice_this(owner, org, practice.id).await.unwrap_err().to_string(), LESSON_REMOVED, "a removed lesson is never reopened");
        store.update_item(drawn.id, None, None, Some("archived")).await?;
        let pad_report = store.get_report(pad).await?.ok_or_else(|| anyhow!("no pad"))?;
        assert_eq!(store.live_lesson(&pad_report).await?, None);
        let (fresh, made_fresh) = store.practice_this(owner, org, board.id).await?;
        assert!(made_fresh && fresh.id != drawn.id && fresh.status != "archived", "practising a source again after its lesson was removed makes a new lesson");

        sqlx::query("DELETE FROM world_events WHERE payload->>'owner_user_id' = $1").bind(owner.to_string()).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_items WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }
}
