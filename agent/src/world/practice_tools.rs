use anyhow::{anyhow, bail, Result};
use loka_shared::practice::{self, Attempt, Check, Practice};
use loka_shared::reports::{Report, ReportActor};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

pub const PRACTICE_TOOLS: &[&str] = &["grade_attempt", "practice_check", "create_practice", "read_practice", "update_practice", "list_practice"];
pub const READ_TOOLS: &[&str] = &["read_practice", "list_practice", "list_challenges"];
const STROKES_CHARS: usize = 60_000;

pub fn tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    vec![tool(
        "grade_attempt",
        "Grade a practice workspace lesson attempt you were sent as coach (practice.ready). Look at the image and the stroke order first and compare with earlier attempts. It saves your check, grades the attempt in the Academy (a pass earns xp in the skill the first time, keeps the streak and counts toward the course), posts it in your person's chat with you (with a practice:<id> card) and notifies them, so do not also reply with the same text. Each attempt gets one grade.",
        json!({
            "attempt": s("The attempt id from practice.ready (practice:<id> or its first characters)"),
            "right": s("What is right in this attempt, specific: name strokes, shapes or proportions"),
            "fix": s("The one or two most important things to fix and how"),
            "next": s("The next exercise to do"),
            "score": { "type": "number", "description": "Optional score from 0 to 100; 60 or more passes unless passed says otherwise" },
            "passed": { "type": "boolean", "description": "Optional: whether this attempt passes the lesson; default from the score, or a pass with no score" }
        }),
        &["attempt", "right", "fix", "next"],
    )]
}

#[derive(Debug)]
pub struct CheckArgs {
    pub attempt: String,
    pub check: Check,
    pub passed: Option<bool>,
}

pub fn check_args(args: &Value) -> Result<CheckArgs> {
    let text = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    let attempt = text("attempt").trim();
    if attempt.is_empty() {
        bail!("attempt is required: the id from practice.ready");
    }
    let score = match args.get("score") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(s.trim().trim_end_matches("/100").trim().parse::<f64>().map_err(|_| anyhow!("score is a number from 0 to 100"))?),
        Some(_) => bail!("score is a number from 0 to 100"),
    };
    Ok(CheckArgs {
        passed: args.get("passed").and_then(Value::as_bool),
        attempt: attempt.to_string(),
        check: Check::new(&super::prompts::plain_dashes(text("right")), &super::prompts::plain_dashes(text("fix")), &super::prompts::plain_dashes(text("next")), score)?,
    })
}

async fn find(store: &WorldStore, owner: Uuid, org: Uuid, key: Option<&str>) -> Result<(Report, Practice)> {
    let key = key.ok_or_else(|| anyhow!("workspace is required"))?;
    let report = store.find_report(owner, org, key).await?.ok_or_else(|| anyhow!("no practice workspace matches {key}; see list_practice"))?;
    if report.owner_user_id != owner {
        bail!("\"{}\" belongs to someone else", report.title);
    }
    let practice = Practice::of(&report).ok_or_else(|| anyhow!("\"{}\" is not a practice workspace; make one with create_practice", report.title))?;
    Ok((report, practice))
}

async fn coach_name(store: &WorldStore, id: Option<Uuid>) -> String {
    match id {
        Some(id) => store.get_agent(id).await.ok().flatten().map(|a| format!("{} (@{})", a.name, a.slug)).unwrap_or_else(|| "a removed agent".into()),
        None => "nobody yet".into(),
    }
}

fn attempt_line(a: &Attempt) -> String {
    let note = a.note.as_deref().map(|n| format!(" note: {}", loka_shared::links::excerpt(n, 160))).unwrap_or_default();
    format!("- practice:{} {}{} ({}){note}", a.id, practice::history_line(a), grade_note(a), a.created_at.format("%Y-%m-%d %H:%M UTC"))
}

fn grade_note(a: &Attempt) -> String {
    match (a.grade_status.as_deref(), a.xp_awarded) {
        (Some(status), Some(xp)) if xp > 0 => format!(", {status} +{xp} xp"),
        (Some(status), _) => format!(", {status}"),
        _ => String::new(),
    }
}

fn attempt_detail(a: &Attempt, strokes: bool) -> String {
    let mut out = format!("Attempt {} practice:{} ({}{}, {} strokes, image upload {})", a.n, a.id, a.status, grade_note(a), a.stroke_count, a.image_upload_id.map(|i| i.to_string()).unwrap_or_else(|| "gone".into()));
    if let Some(note) = &a.note {
        out.push_str(&format!("\nNote from your person: {note}"));
    }
    if let Some(c) = Check::of(a.check.as_ref()) {
        out.push_str(&format!("\nCheck: right: {}\nfix: {}\nnext: {}{}", c.right, c.fix, c.next, c.score.map(|s| format!("\nscore: {s}")).unwrap_or_default()));
    }
    if let Some(parsed) = a.strokes.as_ref().and_then(|v| practice::parse_strokes(v).ok()) {
        out.push_str(&format!("\n\n{}", practice::stroke_summary(&parsed)));
        if strokes {
            let raw = serde_json::to_string(&parsed).unwrap_or_default();
            if raw.len() > STROKES_CHARS {
                let cut: String = raw.chars().take(STROKES_CHARS).collect();
                out.push_str(&format!("\n\nStrokes JSON (first {STROKES_CHARS} characters of {}):\n{cut}", raw.len()));
            } else {
                out.push_str(&format!("\n\nStrokes JSON:\n{raw}"));
            }
        }
    }
    out
}

pub fn grade_line(status: Option<&str>, xp: i32) -> String {
    match status {
        Some("passed") if xp > 0 => format!(" It passed and earned {xp} xp."),
        Some("passed") => " It passed; this lesson was already passed, so no new xp.".to_string(),
        Some("failed") => " It did not pass yet, so it earns no xp.".to_string(),
        _ => " A newer attempt replaced it, so it earns no xp.".to_string(),
    }
}

pub async fn read(store: &WorldStore, owner: Uuid, org: Uuid, canvas: Option<&str>, attempt: Option<&str>, strokes: bool) -> Result<String> {
    if let Some(key) = attempt {
        let attempt = store.find_practice_attempt(owner, org, key).await?.ok_or_else(|| anyhow!("no attempt matches {key}"))?;
        let report = store.get_report(attempt.report_id).await?.ok_or_else(|| anyhow!("that practice workspace is gone"))?;
        return Ok(format!("Practice workspace report:{} \"{}\" ({})\n{}", report.id, report.title, attempt.subject, attempt_detail(&attempt, strokes)));
    }
    let (report, practice) = find(store, owner, org, canvas).await?;
    let attempts = store.practice_attempts(report.id, 30, false).await?;
    let mut out = format!(
        "Practice workspace report:{} \"{}\": subject {}, coach {}, {} attempts shown.",
        report.id,
        report.title,
        practice.subject,
        coach_name(store, practice.coach_agent_id).await,
        attempts.len()
    );
    if !attempts.is_empty() {
        out.push('\n');
        out.push_str(&attempts.iter().map(attempt_line).collect::<Vec<_>>().join("\n"));
    }
    if strokes {
        if let Some(latest) = attempts.first() {
            let full = store.practice_attempt(latest.id).await?.ok_or_else(|| anyhow!("that attempt is gone"))?;
            out.push_str(&format!("\n\nLatest attempt:\n{}", attempt_detail(&full, true)));
        }
    }
    Ok(out)
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, quest: Option<Uuid>, name: &str, args: &Value) -> Result<String> {
    let owner = caller.owner_user_id.ok_or_else(|| anyhow!("you have no person to coach"))?;
    let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let by = ReportActor::Agent(caller.id);
    match name {
        "create_practice" => {
            let subject = practice::clean_subject(arg("subject").unwrap_or(""))?;
            let coach = match arg("coach") {
                Some(key) => store.find_coach(owner, org, key).await?,
                None => store.coach_pref(owner, org, &subject).await?.unwrap_or(caller.id),
            };
            let report = store.create_practice(owner, org, by, &subject, arg("title"), Some(coach), quest).await?;
            Ok(format!(
                "Made practice workspace report:{} \"{}\" for {subject}, coached by {}. Tell your person in a line and put report:{} on its own line so it shows as a card; they draw in it and press Ready.",
                report.id,
                report.title,
                coach_name(store, Some(coach)).await,
                report.id
            ))
        }
        "read_practice" => read(store, owner, org, arg("canvas"), arg("attempt"), args.get("strokes").and_then(Value::as_bool).unwrap_or(false)).await,
        "update_practice" => {
            let (report, _) = find(store, owner, org, arg("canvas")).await?;
            let coach = match arg("coach") {
                Some(key) => Some(store.find_coach(owner, org, key).await?),
                None => None,
            };
            if coach.is_none() && arg("subject").is_none() && arg("title").is_none() {
                bail!("give coach, subject or title to change");
            }
            let report = store.update_practice(&report, by, arg("subject"), coach, arg("title")).await?;
            let practice = Practice::of(&report).ok_or_else(|| anyhow!("that is not a practice workspace any more"))?;
            Ok(format!("Updated report:{} \"{}\": subject {}, coach {}.", report.id, report.title, practice.subject, coach_name(store, practice.coach_agent_id).await))
        }
        "list_practice" => {
            let found = store.list_practice(owner, org, arg("subject"), arg("query"), 50).await?;
            if found.is_empty() {
                return Ok("No practice workspaces yet; create_practice makes one.".into());
            }
            let mut lines = Vec::new();
            for p in found {
                let practice: Option<Practice> = p.report.practice.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok());
                let coach = coach_name(store, practice.as_ref().and_then(|x| x.coach_agent_id)).await;
                lines.push(format!(
                    "- report:{} \"{}\": {}, coach {coach}, {} attempts",
                    p.report.id,
                    p.report.title,
                    practice.as_ref().map(|x| x.subject.as_str()).unwrap_or("?"),
                    p.attempts
                ));
            }
            Ok(lines.join("\n"))
        }
        "grade_attempt" | "practice_check" => {
            let parsed = check_args(args)?;
            let attempt = store.find_practice_attempt(owner, org, &parsed.attempt).await?.ok_or_else(|| anyhow!("no attempt matches {}", parsed.attempt))?;
            let saved = store.practice_check(caller, attempt.id, parsed.check, parsed.passed).await?;
            let grade = grade_line(saved.grade_status.as_deref(), saved.xp_awarded.unwrap_or(0));
            Ok(format!(
                "Graded attempt {} and posted it in your person's chat as msg:{}.{grade} End your turn without repeating it.",
                saved.n,
                saved.coach_message_id.map(|m| m.to_string()[..8].to_string()).unwrap_or_default()
            ))
        }
        _ => bail!("unknown practice tool {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_args_read_every_field_and_tolerate_score_strings() {
        let parsed = check_args(&json!({ "attempt": " practice:abc123 ", "right": "Balanced", "fix": "Stroke 2", "next": "Again", "score": "72/100" })).unwrap();
        assert_eq!(parsed.attempt, "practice:abc123");
        assert_eq!(parsed.check.score, Some(72.0));
        assert_eq!(check_args(&json!({ "attempt": "a", "right": "r", "fix": "f", "next": "n" })).unwrap().check.score, None);
        assert_eq!(check_args(&json!({ "attempt": "a", "right": "r", "fix": "f", "next": "n", "score": 55 })).unwrap().check.score, Some(55.0));
    }

    #[test]
    fn grade_replies_name_the_outcome() {
        assert_eq!(grade_line(Some("passed"), 40), " It passed and earned 40 xp.");
        assert!(grade_line(Some("passed"), 0).contains("already passed"));
        assert!(grade_line(Some("failed"), 0).contains("did not pass"));
        for status in [Some("error"), Some("pending"), None] {
            let line = grade_line(status, 0);
            assert!(line.contains("newer attempt") && !line.contains("It error"), "{line}");
        }
    }

    #[test]
    fn check_args_refuse_missing_parts() {
        assert!(check_args(&json!({ "right": "r", "fix": "f", "next": "n" })).unwrap_err().to_string().contains("attempt"));
        assert!(check_args(&json!({ "attempt": "a", "right": "r", "next": "n" })).unwrap_err().to_string().contains("fix"));
        assert!(check_args(&json!({ "attempt": "a", "right": "r", "fix": "f", "next": "n", "score": "great" })).is_err());
        assert!(check_args(&json!({ "attempt": "a", "right": "r", "fix": "f", "next": "n", "score": [1] })).is_err());
    }

    #[test]
    fn only_grade_attempt_is_listed_and_the_old_names_still_dispatch() {
        fn tool(name: &str, _: &str, _: Value, _: &[&str]) -> Value {
            json!(name)
        }
        let listed: Vec<Value> = tools(tool);
        assert_eq!(listed, vec![json!("grade_attempt")]);
        assert!(PRACTICE_TOOLS.contains(&"grade_attempt"));
        assert!(!listed.contains(&json!("create_practice")) && !listed.contains(&json!("practice_check")), "the old practice tools stay as hidden aliases");
    }
}
