use std::cmp::Ordering;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::powers;
use crate::readiness::{self, Signals};
use crate::teams::NewTeam;
use crate::world::{AgentUpdate, NewAgent, NewEvent, ReviewerChoice, WorldAgent, WorldStore};

pub const NIGHTLY: &str = "Nightly notes and reset";
pub const TEAM_NAMES: &[&str] = &["Builders", "Starter builders", "Build crew"];
const CLI_ORDER: &[&str] = &["claude", "codex", "omp", "cursor", "opencode"];
const CHEAP_WORDS: &[&str] = &["haiku", "mini", "nano", "lite", "flash", "small"];
const STRONG_WORDS: &[&str] = &["opus", "pro", "max"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Job {
    Coder,
    Reviewer,
}

impl Job {
    pub fn label(self) -> &'static str {
        match self {
            Job::Coder => "coder",
            Job::Reviewer => "reviewer",
        }
    }

    fn duty(self) -> &'static str {
        match self {
            Job::Coder => "builds quests and opens pull requests",
            Job::Reviewer => "reviews changes before they ship",
        }
    }

    fn kept_powers(self) -> &'static [&'static str] {
        match self {
            Job::Coder => &[],
            Job::Reviewer => &[powers::OVERSEE],
        }
    }

    fn tags(self) -> &'static [&'static str] {
        match self {
            Job::Coder => &["coding", "engineering"],
            Job::Reviewer => &["code review", "testing"],
        }
    }
}

pub const SEATS: &[(&str, Job)] = &[
    ("coder-1", Job::Coder),
    ("coder-2", Job::Coder),
    ("reviewer", Job::Reviewer),
];

struct Identity {
    name: &'static str,
    emoji: &'static str,
    archetype: &'static str,
    quirk: &'static str,
    outfit: &'static str,
    palette: &'static str,
    accessories: &'static [&'static str],
    species: Option<&'static str>,
    gear: &'static str,
    voice: &'static str,
}

const CODERS: &[Identity] = &[
    Identity { name: "Ines", emoji: "⚡", archetype: "nerdy hacker", quirk: "hums old synth tunes while the tests run", outfit: "an oversized hoodie covered in conference stickers", palette: "teal and charcoal", accessories: &["glasses", "headband"], species: None, gear: "a wizard hat and a lantern for long debugging nights", voice: "af_nicole" },
    Identity { name: "Kofi", emoji: "🎸", archetype: "rock-star engineer with spiky hair", quirk: "names every branch after a song", outfit: "a leather jacket over a faded band tee", palette: "red and black", accessories: &["headband"], species: None, gear: "a horned viking helmet and a sword for slaying bugs", voice: "am_puck" },
    Identity { name: "Mei", emoji: "🧩", archetype: "puzzle-loving systems tinkerer", quirk: "keeps a tiny notebook of every bug she ever fixed", outfit: "denim overalls with a tool belt", palette: "mustard and navy", accessories: &["bun"], species: None, gear: "a hard hat and a lantern", voice: "af_sky" },
    Identity { name: "Mateo", emoji: "🌋", archetype: "calm backend craftsman", quirk: "never ships without a way to roll back", outfit: "a flannel shirt with rolled sleeves", palette: "forest green and brown", accessories: &["beard"], species: None, gear: "a hard hat and a sword", voice: "am_eric" },
    Identity { name: "Anika", emoji: "🛰️", archetype: "quiet frontend perfectionist", quirk: "lines up pixels until they are exactly right", outfit: "a crisp white shirt and pastel sneakers", palette: "lilac and white", accessories: &["glasses"], species: None, gear: "a wizard hat and a sword", voice: "bf_isabella" },
];

const REVIEWERS: &[Identity] = &[
    Identity { name: "Odile", emoji: "📚", archetype: "meticulous librarian", quirk: "reads every diff twice, once from the bottom up", outfit: "a cardigan with a pocket full of red pens", palette: "plum and cream", accessories: &["glasses", "scarf"], species: None, gear: "a lantern and a wizard hat", voice: "bf_emma" },
    Identity { name: "Bashir", emoji: "🔍", archetype: "eagle-eyed editor", quirk: "can smell an off-by-one error from across town", outfit: "a tweed waistcoat with a magnifying glass on a chain", palette: "olive and gold", accessories: &["glasses", "beard"], species: None, gear: "a lantern", voice: "bm_george" },
];

fn identities(job: Job) -> &'static [Identity] {
    match job {
        Job::Coder => CODERS,
        Job::Reviewer => REVIEWERS,
    }
}

fn persona(job: Job, id: &Identity) -> String {
    let who = format!("{} is a {} who {}.", id.name, id.archetype, id.quirk);
    let work = match job {
        Job::Coder => "One job: build what quests ask for. Read the code first, make the smallest change that does the job, run the checks, and leave a pull request ready to review. Does not watch inboxes, review other agents' work or manage agents; anything outside building goes back to the coordinator.",
        Job::Reviewer => "One job: review changes before they ship. Reads the quest and the diff, reruns the checks, and reports concrete findings with file and line, most serious first. Approves only what is ready, never rewrites the work, and sends fixes back to the agent who wrote it.",
    };
    format!("{who} {work}")
}

fn look(id: &Identity, job: Job) -> Value {
    let mut look = json!({
        "archetype": id.archetype,
        "outfit": id.outfit,
        "palette": id.palette,
        "accessories": id.accessories,
        "voice": id.voice,
        "role": job.label(),
    });
    if let Some(species) = id.species {
        look["species"] = json!(species);
    }
    look
}

const COORDINATOR_DUTY: &str = "talks with the person, routes work to the right agent and keeps quests moving";

fn notes(name: &str, slug: &str, label: &str, duty: &str) -> (String, String) {
    let skill = format!("{slug}-notes");
    let body = format!(
        "---\nname: {skill}\ndescription: {name}'s working notes, read before every job: role, people, open threads and lessons.\n---\n# {name}'s notes\n\n## Role\n{name} is the {label}: {duty}.\n\n## Open threads\n- none yet\n\n## Already handled\n- none yet\n\n## Lessons\n- none yet\n"
    );
    (skill, body)
}

pub const NIGHTLY_PROMPT: &str = "Nightly notes and reset. If you did no work in the last 24 hours, or one of your quests is still running, stop here and end the turn without a reply. Otherwise rewrite your notes with update_my_notes so your next self has what it needs: people, open threads, decisions, what you already handled and lessons, kept short. Then call reset_context. Say nothing to anyone.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CliModels {
    pub cli: String,
    pub models: Vec<String>,
}

fn fallback_models(cli: &str) -> &'static [&'static str] {
    match cli {
        "claude" => &["opus", "sonnet", "haiku"],
        "codex" => &["gpt-5.4", "gpt-5.4-mini"],
        "omp" => &["fal/kimi-k3", "fal/glm-5.3"],
        _ => &[],
    }
}

fn runner_models(caps: &Value, cli: &str) -> Vec<String> {
    caps.get("models")
        .and_then(|m| m.get(cli))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|pair| pair.get(0).and_then(Value::as_str).or_else(|| pair.as_str()))
        .map(str::to_string)
        .collect()
}

fn add_cli(found: &mut Vec<CliModels>, opencode: bool, cli: &str, models: Vec<String>) {
    if !crate::backends::AGENT_CLIS.contains(&cli) || (cli == crate::backends::OPENCODE && !opencode) {
        return;
    }
    match found.iter_mut().find(|c| c.cli == cli) {
        Some(entry) => {
            for m in models {
                if !entry.models.contains(&m) {
                    entry.models.push(m);
                }
            }
        }
        None => found.push(CliModels { cli: cli.to_string(), models }),
    }
}

pub fn available_clis(signals: &Signals, main_cli: &str, main_model: &str) -> Vec<CliModels> {
    let mut found: Vec<CliModels> = Vec::new();
    let add = |found: &mut Vec<CliModels>, cli: &str, models: Vec<String>| add_cli(found, signals.opencode, cli, models);
    let live = |r: &&crate::runners::RunnerView| r.status == "online";
    for runner in signals.own.iter().filter(live) {
        let Some(backends) = runner.capabilities.get("backends").and_then(Value::as_object) else {
            continue;
        };
        for (cli, on) in backends {
            if on.as_bool() == Some(true) {
                add(&mut found, cli, runner_models(&runner.capabilities, cli));
            }
        }
    }
    if found.is_empty() || !found.iter().any(|c| c.cli == main_cli) {
        add(&mut found, main_cli, Vec::new());
    }
    for entry in found.iter_mut() {
        if entry.models.is_empty() {
            entry.models = fallback_models(&entry.cli).iter().map(|m| m.to_string()).collect();
        }
        if entry.cli == main_cli && !main_model.is_empty() && !entry.models.iter().any(|m| m == main_model) {
            entry.models.push(main_model.to_string());
        }
    }
    found.retain(|c| !c.models.is_empty());
    found.sort_by_key(|c| CLI_ORDER.iter().position(|o| *o == c.cli).unwrap_or(CLI_ORDER.len()));
    found
}

fn tier(model: &str) -> i32 {
    let m = model.to_lowercase();
    if CHEAP_WORDS.iter().any(|w| m.contains(w)) {
        0
    } else if m == "auto" || m == "default" {
        1
    } else if m.contains("sonnet") {
        2
    } else if STRONG_WORDS.iter().any(|w| m.contains(w)) {
        4
    } else {
        3
    }
}

fn version(model: &str) -> Vec<u64> {
    model
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty() && p.len() < 8)
        .filter_map(|p| p.parse().ok())
        .collect()
}

fn by_strength(a: &str, b: &str) -> Ordering {
    tier(a).cmp(&tier(b)).then_with(|| version(a).cmp(&version(b)))
}

pub fn strongest(models: &[String]) -> Option<&String> {
    models.iter().rev().max_by(|a, b| by_strength(a, b))
}

pub fn cheapest(models: &[String]) -> Option<&String> {
    models.iter().rev().min_by(|a, b| tier(a).cmp(&tier(b)).then_with(|| version(b).cmp(&version(a))))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pick {
    pub cli: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Picks {
    pub coder: Pick,
    pub reviewer: Pick,
}

pub fn pick_models(clis: &[CliModels]) -> Option<Picks> {
    let first = clis.first()?;
    let strong = |c: &CliModels| Pick { cli: c.cli.clone(), model: strongest(&c.models).cloned().unwrap_or_default() };
    let coder = strong(first);
    let reviewer = clis
        .iter()
        .skip(1)
        .find(|c| matches!(c.cli.as_str(), "claude" | "codex"))
        .map(strong)
        .unwrap_or_else(|| coder.clone());
    Some(Picks { coder, reviewer })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StarterOptions {
    #[serde(default)]
    pub team: bool,
    #[serde(default)]
    pub seats: Option<Vec<SeatPick>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatPick {
    pub seat: String,
    #[serde(default)]
    pub cli: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

pub fn seat_pick(clis: &[CliModels], default: &Pick, wanted: Option<&SeatPick>) -> Pick {
    let Some(wanted) = wanted else { return default.clone() };
    let Some(entry) = wanted.cli.as_deref().and_then(|cli| clis.iter().find(|c| c.cli == cli)) else {
        return default.clone();
    };
    let model = wanted
        .model
        .as_deref()
        .and_then(|m| entry.models.iter().find(|known| known.eq_ignore_ascii_case(m)))
        .or_else(|| if entry.cli == default.cli { None } else { strongest(&entry.models) })
        .cloned();
    match model {
        Some(model) => Pick { cli: entry.cli.clone(), model },
        None => default.clone(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedSeat {
    pub seat: &'static str,
    pub job: Job,
    pub job_label: &'static str,
    pub duty: &'static str,
    pub name: String,
    pub emoji: String,
    pub cli: String,
    pub model: String,
    pub agent_id: Option<Uuid>,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub coordinator: Option<String>,
    pub seats: Vec<PlannedSeat>,
    pub team: Option<String>,
    pub rule: bool,
    pub complete: bool,
    pub clis: Vec<CliModels>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Built {
    #[serde(flatten)]
    pub plan: Plan,
    pub created: Vec<String>,
    pub schedules: usize,
    pub notes: usize,
}

impl Built {
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        if self.created.is_empty() {
            lines.push("Your starter team was already in place, so nothing new was created.".to_string());
        } else {
            lines.push(format!("Created {}.", list(&self.created)));
        }
        for seat in self.plan.seats.iter().filter(|s| s.exists) {
            lines.push(format!("- {} ({}, {} {}): {}.", seat.name, seat.job_label, seat.cli, seat.model, seat.duty));
        }
        if let Some(team) = &self.plan.team {
            lines.push(format!("The coders work together as team {team}."));
        }
        if self.schedules > 0 || self.notes > 0 {
            lines.push(format!(
                "Added {} schedule{} and {} notes skill{}; every agent rewrites its notes and resets nightly when it worked that day.",
                self.schedules,
                if self.schedules == 1 { "" } else { "s" },
                self.notes,
                if self.notes == 1 { "" } else { "s" }
            ));
        }
        lines.join("\n")
    }
}

fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn agent_slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "agent".into()
    } else {
        out
    }
}

fn revoked_for(job: Job, lead: bool) -> Vec<String> {
    powers::ALL
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !job.kept_powers().contains(p))
        .filter(|p| !(lead && (*p == powers::ASSIGN_OTHERS || *p == powers::TEAMS || *p == powers::SCHEDULES)))
        .map(String::from)
        .collect()
}

fn roster(coordinator: &str, seats: &[(Job, String)], team: Option<(&str, &str)>) -> String {
    let mut lines = vec![
        "These are the agents of this World and their one job each. Use only these names, stay in your own lane, and hand anything outside your job to the agent who owns it (the coordinator when unsure):".to_string(),
        format!("- {coordinator} (coordinator): {COORDINATOR_DUTY}."),
    ];
    let coders: Vec<String> = seats.iter().filter(|(j, _)| *j == Job::Coder).map(|(_, n)| n.clone()).collect();
    if !coders.is_empty() {
        let (label, duty) = if coders.len() > 1 { ("coders", "build quests and open pull requests") } else { ("coder", Job::Coder.duty()) };
        lines.push(format!("- {} ({label}): {duty}.", list(&coders)));
    }
    for (job, name) in seats.iter().filter(|(j, _)| *j != Job::Coder) {
        lines.push(format!("- {name} ({}): {}.", job.label(), job.duty()));
    }
    if let Some((team, lead)) = team {
        lines.push(format!("- Team {team} (lead {lead}): give it quests that need both coders."));
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Copy)]
struct Scope {
    owner: Uuid,
    org: Option<Uuid>,
}

impl Scope {
    fn key(&self) -> Uuid {
        self.org.unwrap_or_default()
    }
}

struct Existing {
    seats: Vec<(String, WorldAgent)>,
    rule_id: Option<Uuid>,
    team_id: Option<Uuid>,
    started: bool,
}

impl WorldStore {
    async fn starter_main(&self, owner: Uuid, org: Option<Uuid>) -> Result<Option<WorldAgent>> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT COALESCE(
               (SELECT m.main_agent_id FROM org_members m WHERE m.user_id = $1 AND m.org_id = $2),
               (SELECT u.main_agent_id FROM users u WHERE u.id = $1 AND $2::uuid IS NULL))",
        )
        .bind(owner)
        .bind(org)
        .fetch_one(self.pool())
        .await?;
        match id {
            Some(id) => Ok(self.get_agent(id).await?.filter(|a| a.owner_user_id == Some(owner))),
            None => Ok(None),
        }
    }

    pub async fn active_org(&self, owner: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar("SELECT world_active_org($1)").bind(owner).fetch_one(self.pool()).await?)
    }

    async fn starter_existing(&self, scope: Scope) -> Result<Existing> {
        let rows: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT s.seat, s.agent_id FROM starter_team_seats s
             JOIN agent_profiles a ON a.id = s.agent_id AND a.owner_user_id = s.owner_user_id
             WHERE s.owner_user_id = $1 AND s.org_key = $2 AND COALESCE(a.org_id, $2) = $2",
        )
        .bind(scope.owner)
        .bind(scope.key())
        .fetch_all(self.pool())
        .await?;
        let mut seats = Vec::new();
        for (seat, id) in rows {
            if let Some(agent) = self.get_agent(id).await? {
                seats.push((seat, agent));
            }
        }
        let head: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT t.rule_id, CASE WHEN EXISTS (SELECT 1 FROM agent_teams g WHERE g.id = t.team_id AND g.owner_user_id = t.owner_user_id) THEN t.team_id END
             FROM starter_teams t WHERE t.owner_user_id = $1 AND t.org_key = $2",
        )
        .bind(scope.owner)
        .bind(scope.key())
        .fetch_optional(self.pool())
        .await?;
        Ok(Existing {
            seats,
            rule_id: head.and_then(|h| h.0),
            team_id: head.and_then(|h| h.1),
            started: head.is_some(),
        })
    }

    async fn starter_signals(&self, owner: Uuid) -> Result<Signals> {
        readiness::gather(self.pool(), owner).await
    }

    pub async fn starter_plan(&self, owner: Uuid, org: Option<Uuid>) -> Result<Plan> {
        let main = self.starter_main(owner, org).await?;
        let scope = Scope { owner, org: main.as_ref().and_then(|m| m.org_id).or(org) };
        let signals = self.starter_signals(owner).await?;
        let existing = self.starter_existing(scope).await?;
        self.plan_from(scope, main.as_ref(), &signals, &existing, None).await
    }

    async fn plan_from(&self, scope: Scope, main: Option<&WorldAgent>, signals: &Signals, existing: &Existing, only: Option<&[SeatPick]>) -> Result<Plan> {
        let (main_cli, main_model) = main.map(|m| (m.cli.as_str(), m.model.as_str())).unwrap_or(("claude", "sonnet"));
        let clis = available_clis(signals, main_cli, main_model);
        let picks = pick_models(&clis);
        let taken = self.taken_agent_names(scope).await?;
        let mut used: Vec<String> = Vec::new();
        let mut seats = Vec::new();
        for (seat, job) in SEATS {
            if let Some((_, agent)) = existing.seats.iter().find(|(s, _)| s == seat) {
                used.push(agent.name.to_lowercase());
                seats.push(PlannedSeat {
                    seat,
                    job: *job,
                    job_label: job.label(),
                    duty: job.duty(),
                    name: agent.name.clone(),
                    emoji: agent.avatar_emoji.clone(),
                    cli: agent.cli.clone(),
                    model: agent.model.clone(),
                    agent_id: Some(agent.id),
                    exists: true,
                });
            }
        }
        for (seat, job) in SEATS {
            if seats.iter().any(|s| s.seat == *seat) {
                continue;
            }
            let wanted = only.map(|list| list.iter().find(|p| p.seat == *seat));
            if wanted == Some(None) {
                continue;
            }
            let Some(picks) = picks.as_ref() else { continue };
            let default = match job {
                Job::Coder => &picks.coder,
                Job::Reviewer => &picks.reviewer,
            };
            let pick = &seat_pick(&clis, default, wanted.flatten());
            let identity = identities(*job)
                .iter()
                .find(|i| {
                    let lower = i.name.to_lowercase();
                    !taken.contains(&lower) && !used.contains(&lower)
                })
                .unwrap_or(&identities(*job)[0]);
            used.push(identity.name.to_lowercase());
            seats.push(PlannedSeat {
                seat,
                job: *job,
                job_label: job.label(),
                duty: job.duty(),
                name: identity.name.to_string(),
                emoji: identity.emoji.to_string(),
                cli: pick.cli.clone(),
                model: pick.model.clone(),
                agent_id: None,
                exists: false,
            });
        }
        seats.sort_by_key(|s| SEATS.iter().position(|(k, _)| *k == s.seat));
        let team = match existing.team_id {
            Some(id) => self.get_team(id).await?.map(|t| t.name),
            None => None,
        };
        let complete = seats.iter().all(|s| s.exists) && existing.started;
        Ok(Plan {
            coordinator: main.map(|m| m.name.clone()),
            seats,
            team,
            rule: existing.rule_id.is_some(),
            complete,
            clis,
        })
    }

    async fn taken_agent_names(&self, scope: Scope) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>("SELECT lower(name) FROM agent_profiles WHERE owner_user_id = $1 OR org_id = $2")
            .bind(scope.owner)
            .bind(scope.org)
            .fetch_all(self.pool())
            .await?)
    }

    async fn free_agent_slug(&self, name: &str) -> Result<String> {
        let base = agent_slug(name);
        let mut slug = base.clone();
        let mut n = 2;
        while self.slug_taken(&slug).await? {
            slug = format!("{base}-{n}");
            n += 1;
        }
        Ok(slug)
    }

    async fn give_notes(&self, agent: &WorldAgent, owner: Uuid, label: &str, duty: &str) -> Result<bool> {
        if self.own_notes_skill(agent.id, Some(owner)).await?.is_some() {
            return Ok(false);
        }
        let (skill, body) = notes(&agent.name, &agent.slug, label, duty);
        let orphan: Option<Uuid> = sqlx::query_scalar(
            "SELECT s.id FROM world_skills s WHERE s.name = $1 AND s.scope = 'agent' AND s.created_by = $2
               AND NOT EXISTS (SELECT 1 FROM agent_skills a WHERE a.skill_id = s.id)",
        )
        .bind(&skill)
        .bind(owner)
        .fetch_optional(self.pool())
        .await?;
        if let Some(id) = orphan {
            sqlx::query("INSERT INTO agent_skills (agent_id, skill_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(agent.id)
                .bind(id)
                .execute(self.pool())
                .await?;
            return Ok(true);
        }
        let description = crate::world::check_notes(&skill, &body)?;
        self.save_own_notes(agent.id, Some(owner), None, &skill, &description, &body).await?;
        Ok(true)
    }

    async fn give_schedule(&self, owner: Uuid, agent: Uuid, name: &str, prompt: &str, cron: &str, kind: &str) -> Result<bool> {
        let has = self.list_agent_schedules(agent).await?.iter().any(|s| s.name == name);
        if has {
            return Ok(false);
        }
        self.create_agent_schedule(owner, agent, name, prompt, Some(cron), None, kind).await?;
        Ok(true)
    }

    pub async fn build_starter_team(&self, owner: Uuid, org: Option<Uuid>, options: &StarterOptions) -> Result<Built> {
        let Some(main) = self.starter_main(owner, org).await? else {
            bail!("set up your main agent first: it becomes the coordinator of the starter team");
        };
        let scope = Scope { owner, org: main.org_id.or(org) };
        let mut lock = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('starter_team:' || $1::text || ':' || $2::text))")
            .bind(owner)
            .bind(scope.key())
            .execute(&mut *lock)
            .await?;
        let signals = self.starter_signals(owner).await?;
        let existing = self.starter_existing(scope).await?;
        let plan = self.plan_from(scope, Some(&main), &signals, &existing, options.seats.as_deref()).await?;
        let first_run = !existing.started;
        let mut created = Vec::new();
        let mut schedules = 0;
        let mut notes = 0;
        let mut seat_agents: Vec<(&'static str, Job, WorldAgent)> =
            existing.seats.iter().filter_map(|(s, a)| SEATS.iter().find(|(k, _)| k == s).map(|(k, j)| (*k, *j, a.clone()))).collect();
        for (index, planned) in plan.seats.iter().enumerate() {
            if planned.exists {
                continue;
            }
            let identity = identities(planned.job).iter().find(|i| i.name == planned.name).unwrap_or(&identities(planned.job)[0]);
            let slug = self.free_agent_slug(&planned.name).await?;
            let agent = self
                .create_agent(NewAgent {
                    slug: &slug,
                    name: &planned.name,
                    avatar_emoji: &planned.emoji,
                    cli: &planned.cli,
                    model: &planned.model,
                    persona: Some(&persona(planned.job, identity)),
                    role: "worker",
                    look: look(identity, planned.job),
                    gear: json!({ "quest": identity.gear }),
                    visibility: "private",
                    owner_user_id: Some(owner),
                    org_id: scope.org,
                })
                .await?;
            let update = AgentUpdate {
                tags: Some(planned.job.tags().iter().map(|t| t.to_string()).collect()),
                expertise: Some(format!("The {}: {}.", planned.job.label(), planned.job.duty())),
                revoked_powers: Some(revoked_for(planned.job, false)),
                ..Default::default()
            };
            self.update_agent(agent.id, &update).await?;
            sqlx::query(
                "INSERT INTO starter_team_seats (owner_user_id, org_key, seat, agent_id) VALUES ($1, $4, $2, $3)
                 ON CONFLICT (owner_user_id, org_key, seat) DO UPDATE SET agent_id = EXCLUDED.agent_id, created_at = NOW()",
            )
            .bind(owner)
            .bind(planned.seat)
            .bind(agent.id)
            .bind(scope.key())
            .execute(self.pool())
            .await?;
            self.emit(
                NewEvent::new("agent.created")
                    .agent(agent.id)
                    .actor(Some(owner))
                    .payload(json!({ "name": agent.name, "slug": agent.slug, "starter_team": planned.seat, "created_by": main.name })),
                None,
            )
            .await?;
            if self.give_notes(&agent, owner, planned.job.label(), planned.job.duty()).await? {
                notes += 1;
            }
            let minute = (index * 7) % 60;
            if self.give_schedule(owner, agent.id, NIGHTLY, NIGHTLY_PROMPT, &format!("{minute} 3 * * *"), "routine").await? {
                schedules += 1;
            }
            created.push(agent.name.clone());
            seat_agents.push((planned.seat, planned.job, agent));
        }
        seat_agents.sort_by_key(|(k, _, _)| SEATS.iter().position(|(s, _)| s == k));
        if first_run {
            if self.give_notes(&main, owner, "coordinator", COORDINATOR_DUTY).await.unwrap_or(false) {
                notes += 1;
            }
            if self.give_schedule(owner, main.id, NIGHTLY, NIGHTLY_PROMPT, "50 2 * * *", "routine").await? {
                schedules += 1;
            }
        }
        let coders: Vec<&WorldAgent> = seat_agents.iter().filter(|(_, j, _)| *j == Job::Coder).map(|(_, _, a)| a).collect();
        let reviewer = seat_agents.iter().find(|(_, j, _)| *j == Job::Reviewer).map(|(_, _, a)| a);
        if let Some(reviewer) = reviewer {
            for coder in coders.iter().filter(|c| created.contains(&c.name) || created.contains(&reviewer.name)) {
                if coder.cli == reviewer.cli && coder.model == reviewer.model {
                    continue;
                }
                if let Ok(choice) = ReviewerChoice::new(Some(&reviewer.cli), &reviewer.model) {
                    self.update_agent(coder.id, &AgentUpdate { reviewer: Some(choice.to_value()), ..Default::default() }).await?;
                }
            }
        }
        let mut team_id = existing.team_id;
        if options.team && coders.len() >= 2 {
            let lead = coders[0];
            match team_id {
                Some(id) => {
                    let ids: Vec<Uuid> = coders.iter().map(|c| c.id).collect();
                    self.add_team_members(id, &ids, &[]).await?;
                }
                None => {
                    let mut name = None;
                    for candidate in TEAM_NAMES {
                        if self.find_team(owner, scope.org, candidate).await?.is_none() {
                            name = Some(*candidate);
                            break;
                        }
                    }
                    if let Some(name) = name {
                        let team = self
                            .create_team(
                                owner,
                                NewTeam {
                                    name: name.to_string(),
                                    purpose: "Build quests together: the lead splits bigger quests between the coders and keeps everyone posted here.".into(),
                                    rules: "Split work so the coders never edit the same files at once. Post one short line when you start and when you finish.".into(),
                                    lead: Some(lead.id),
                                    agents: coders.iter().map(|c| c.id).collect(),
                                    people: Vec::new(),
                                    channel: Some(agent_slug(name)),
                                    created_by_agent: None,
                                    org: scope.org,
                                },
                            )
                            .await?;
                        team_id = Some(team.id);
                    }
                }
            }
            self.update_agent(lead.id, &AgentUpdate { revoked_powers: Some(revoked_for(Job::Coder, true)), ..Default::default() }).await?;
        }
        let team = match team_id {
            Some(id) => self.get_team(id).await?,
            None => None,
        };
        let team_label = team.as_ref().map(|t| {
            let lead = seat_agents.iter().find(|(_, _, a)| Some(a.id) == t.lead_agent_id).map(|(_, _, a)| a.name.clone()).unwrap_or_default();
            (t.name.clone(), lead)
        });
        let named: Vec<(Job, String)> = seat_agents.iter().map(|(_, j, a)| (*j, a.name.clone())).collect();
        let body = roster(&main.name, &named, team_label.as_ref().map(|(t, l)| (t.as_str(), l.as_str())));
        let rule_alive = match existing.rule_id {
            Some(id) => sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM world_rules WHERE id = $1 AND user_id = $2)")
                .bind(id)
                .bind(owner)
                .fetch_one(self.pool())
                .await?,
            None => false,
        };
        let changed = !created.is_empty() || team_id != existing.team_id;
        let rule_id = match (rule_alive, existing.rule_id) {
            (true, Some(id)) => {
                if changed {
                    self.update_rule(id, owner, None, None, Some(&body), None).await?;
                }
                Some(id)
            }
            _ if first_run || changed => {
                let rule = self.create_rule(Some(owner), None, "talk", &body).await?;
                sqlx::query("UPDATE world_rules SET org_id = $2 WHERE id = $1").bind(rule.id).bind(scope.org).execute(self.pool()).await?;
                Some(rule.id)
            }
            _ => None,
        };
        sqlx::query(
            "INSERT INTO starter_teams (owner_user_id, org_key, rule_id, team_id) VALUES ($1, $4, $2, $3)
             ON CONFLICT (owner_user_id, org_key) DO UPDATE SET rule_id = EXCLUDED.rule_id, team_id = EXCLUDED.team_id, updated_at = NOW()",
        )
        .bind(owner)
        .bind(rule_id)
        .bind(team_id)
        .bind(scope.key())
        .execute(self.pool())
        .await?;
        let after = self.starter_existing(scope).await?;
        let plan = self.plan_from(scope, Some(&main), &signals, &after, options.seats.as_deref()).await?;
        lock.commit().await?;
        Ok(Built { plan, created, schedules, notes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runners::RunnerView;
    use sqlx::postgres::PgPoolOptions;

    fn runner(status: &str, caps: Value) -> RunnerView {
        serde_json::from_value(json!({
            "id": Uuid::new_v4(),
            "name": "Mac",
            "platform": "macos",
            "status": status,
            "last_seen_at": null,
            "owner_user_id": Uuid::new_v4(),
            "capabilities": caps,
            "created_at": "2026-01-01T00:00:00Z",
            "shared": false
        }))
        .unwrap()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn starter_models_follow_what_the_person_has() {
        let both = Signals {
            own: vec![runner(
                "online",
                json!({
                    "backends": { "claude": true, "codex": true, "opencode": true, "cursor": false },
                    "models": {
                        "claude": [["claude-sonnet-5-5", "Sonnet"], ["claude-opus-5-5", "Opus"], ["claude-haiku-5-5", "Haiku"]],
                        "codex": [["gpt-5.4-mini", "mini"], ["gpt-5.4", "5.4"], ["gpt-5.3", "5.3"]]
                    }
                }),
            )],
            ..Signals::default()
        };
        let clis = available_clis(&both, "claude", "sonnet");
        assert_eq!(clis.iter().map(|c| c.cli.as_str()).collect::<Vec<_>>(), vec!["claude", "codex"]);
        let picks = pick_models(&clis).unwrap();
        assert_eq!(picks.coder, Pick { cli: "claude".into(), model: "claude-opus-5-5".into() });
        assert_eq!(picks.reviewer, Pick { cli: "codex".into(), model: "gpt-5.4".into() });

        let codex_only = available_clis(&Signals::default(), "codex", "gpt-5.4");
        let picks = pick_models(&codex_only).unwrap();
        assert_eq!(picks.coder.model, "gpt-5.4");
        assert_eq!(picks.reviewer, picks.coder);

        let offline = Signals { own: vec![runner("offline", json!({ "backends": { "codex": true } }))], ..Signals::default() };
        let cursor = available_clis(&offline, "cursor", "auto");
        assert_eq!(cursor, vec![CliModels { cli: "cursor".into(), models: strings(&["auto"]) }]);
        let picks = pick_models(&cursor).unwrap();
        assert_eq!(picks.coder.model, "auto");

        assert_eq!(strongest(&strings(&["haiku", "sonnet", "opus"])).unwrap(), "opus");
        assert_eq!(cheapest(&strings(&["opus", "sonnet"])).unwrap(), "sonnet");
        assert!(roster("Nia", &[(Job::Coder, "Ines".into()), (Job::Coder, "Kofi".into()), (Job::Reviewer, "Odile".into())], None)
            .contains("- Ines and Kofi (coders)"));
        assert!(!revoked_for(Job::Coder, true).contains(&powers::ASSIGN_OTHERS.to_string()));
        assert!(revoked_for(Job::Reviewer, false).iter().all(|p| p != powers::OVERSEE));
    }

    async fn person(pool: &sqlx::PgPool) -> Result<Uuid> {
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Sam') RETURNING id")
            .bind(format!("{}@starter.test", Uuid::new_v4()))
            .fetch_one(pool)
            .await?;
        let main: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, role) VALUES ($1, 'Nia', 'claude', 'sonnet', $2, 'assistant') RETURNING id",
        )
        .bind(format!("nia-{}", &Uuid::new_v4().simple().to_string()[..8]))
        .bind(owner)
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE users SET main_agent_id = $2 WHERE id = $1").bind(owner).bind(main).execute(pool).await?;
        crate::orgs::join_default_for_tests(pool, owner, Some(main)).await?;
        Ok(owner)
    }

    async fn counts(pool: &sqlx::PgPool, owner: Uuid) -> Result<(i64, i64, i64, i64, i64)> {
        Ok(sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM agent_profiles WHERE owner_user_id = $1),
                    (SELECT COUNT(*) FROM schedules s JOIN agent_profiles a ON a.id = s.agent_id WHERE a.owner_user_id = $1),
                    (SELECT COUNT(*) FROM world_rules WHERE user_id = $1),
                    (SELECT COUNT(*) FROM agent_teams WHERE owner_user_id = $1),
                    (SELECT COUNT(*) FROM agent_skills k JOIN agent_profiles a ON a.id = k.agent_id WHERE a.owner_user_id = $1)",
        )
        .bind(owner)
        .fetch_one(pool)
        .await?)
    }

    #[tokio::test]
    async fn starter_team_is_idempotent_and_owner_scoped() -> Result<()> {
        let Ok(url) = std::env::var("STARTER_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(6).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let owner = person(&pool).await?;
        let other = person(&pool).await?;

        let plan = store.starter_plan(owner, None).await?;
        assert!(!plan.complete);
        assert_eq!(plan.seats.iter().map(|s| s.seat).collect::<Vec<_>>(), vec!["coder-1", "coder-2", "reviewer"]);

        let (first, again) = tokio::join!(
            store.build_starter_team(owner, None, &StarterOptions { team: true, seats: None }),
            store.build_starter_team(owner, None, &StarterOptions { team: true, seats: None })
        );
        let (first, again) = (first?, again?);
        assert_eq!(first.created.len() + again.created.len(), 3);
        assert!(again.plan.complete && again.plan.team.is_some());
        let built = counts(&pool, owner).await?;
        assert_eq!(built, (4, 4, 1, 1, 4));
        let heartbeats: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedules s JOIN agent_profiles a ON a.id = s.agent_id WHERE a.owner_user_id = $1 AND s.kind = 'heartbeat'")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        assert_eq!(heartbeats, 0);

        let third = store.build_starter_team(owner, None, &StarterOptions { team: true, seats: None }).await?;
        assert!(third.created.is_empty());
        assert_eq!(counts(&pool, owner).await?, built);

        let rule: String = sqlx::query_scalar("SELECT body FROM world_rules WHERE user_id = $1 AND body LIKE '%(coordinator)%'").bind(owner).fetch_one(&pool).await?;
        for seat in &third.plan.seats {
            assert!(rule.contains(&seat.name), "{rule}");
        }
        assert!(rule.contains("Nia (coordinator)"));
        let reviewer_seat = third.plan.seats.iter().find(|s| s.job == Job::Reviewer).unwrap();
        let revoked: Vec<String> = sqlx::query_scalar("SELECT revoked_powers FROM agent_profiles WHERE id = $1")
            .bind(reviewer_seat.agent_id)
            .fetch_one(&pool)
            .await?;
        assert!(revoked.contains(&powers::MANAGE_AGENTS.to_string()) && !revoked.contains(&powers::OVERSEE.to_string()));

        assert_eq!(counts(&pool, other).await?, (1, 0, 0, 0, 0));
        let theirs = store.build_starter_team(other, None, &StarterOptions::default()).await?;
        assert_eq!(theirs.created.len(), 3);
        assert!(theirs.plan.team.is_none());
        assert_eq!(counts(&pool, owner).await?, built);
        let foreign: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM starter_team_seats s JOIN agent_profiles a ON a.id = s.agent_id WHERE a.owner_user_id <> s.owner_user_id",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(foreign, 0);

        let reviewer = third.plan.seats.iter().find(|s| s.job == Job::Reviewer).unwrap().agent_id.unwrap();
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(reviewer).execute(&pool).await?;
        assert!(!store.starter_plan(owner, None).await?.complete);
        let refill = store.build_starter_team(owner, None, &StarterOptions::default()).await?;
        assert_eq!(refill.created.len(), 1);
        assert!(refill.plan.complete);
        assert_eq!(counts(&pool, owner).await?, built);

        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Side gig') RETURNING id")
            .bind(format!("side-{}", &Uuid::new_v4().simple().to_string()[..8]))
            .fetch_one(&pool)
            .await?;
        let side_main: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, role, org_id) VALUES ($1, 'Tao', 'claude', 'sonnet', $2, 'assistant', $3) RETURNING id",
        )
        .bind(format!("tao-{}", &Uuid::new_v4().simple().to_string()[..8]))
        .bind(owner)
        .bind(org)
        .fetch_one(&pool)
        .await?;
        sqlx::query("INSERT INTO org_members (org_id, user_id, role, main_agent_id) VALUES ($1, $2, 'owner', $3)")
            .bind(org)
            .bind(owner)
            .bind(side_main)
            .execute(&pool)
            .await?;
        let side = store.build_starter_team(owner, Some(org), &StarterOptions::default()).await?;
        assert_eq!(side.created.len(), 3);
        assert_eq!(side.plan.coordinator.as_deref(), Some("Tao"));
        let in_side: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE owner_user_id = $1 AND org_id = $2")
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        assert_eq!(in_side, 4);
        let home_org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1")
            .bind(refill.plan.seats[0].agent_id)
            .fetch_one(&pool)
            .await?;
        let home_rules = store.rules_for(Some(owner), home_org).await?;
        assert_eq!(home_rules.iter().filter(|r| r.body.contains("(coordinator)")).count(), 1);
        assert!(home_rules.iter().all(|r| !r.body.contains("Tao (coordinator)")));
        let side_rules = store.rules_for(Some(owner), Some(org)).await?;
        assert!(side_rules.iter().any(|r| r.body.contains("Tao (coordinator)")) && side_rules.iter().all(|r| !r.body.contains("Nia (coordinator)")));
        assert!(store.build_starter_team(owner, None, &StarterOptions::default()).await?.created.is_empty());

        let lonely: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{}@starter.test", Uuid::new_v4()))
            .fetch_one(&pool)
            .await?;
        assert!(store.build_starter_team(lonely, None, &StarterOptions::default()).await.is_err());
        Ok(())
    }
}
