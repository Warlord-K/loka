use axum::{extract::State, http::StatusCode, Extension, Json};
use loka_shared::access::Role;
use loka_shared::onboarding::{self as ob, Onboarding, PersonaChoice, Step};
use loka_shared::starter_team::{Plan, StarterOptions};
use loka_shared::world::{AgentUpdate, NewEvent, NewMessage, WorldAgent};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::UserId;
use crate::routes::world::{apply_agent_update, ensure_assistant, internal, invalid_cli_switch, load, opencode_update_allowed};
use crate::AppState;

#[derive(Serialize)]
pub struct Started {
    pending: bool,
    agent_id: Option<Uuid>,
}

pub async fn start(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Extension(role): Extension<Role>,
) -> Result<Json<Started>, StatusCode> {
    let done = Json(Started { pending: false, agent_id: None });
    if role < Role::Write || !state.world.onboarding_pending(user.0).await.map_err(internal)? {
        return Ok(done);
    }
    let main = state.world.main_agent_for(user.0).await.map_err(internal)?.map(|a| a.id);
    if let Some(existing) = state.world.onboarding(user.0).await.map_err(internal)? {
        if Some(existing.agent_id) == main {
            return Ok(Json(Started { pending: true, agent_id: main }));
        }
        state.world.finish_onboarding(user.0).await.map_err(internal)?;
        return Ok(done);
    }
    let _ = ensure_assistant(State(state.clone()), Extension(user.clone()), None).await?;
    let agent = state.world.main_agent_for(user.0).await.map_err(internal)?.ok_or(StatusCode::CONFLICT)?;
    let tools = mac_tools(&state, user.0, &agent).await?.filter(|tools| !ob::tools_ready(tools));
    let step = if tools.is_some() { Step::Tools } else { Step::Cli };
    let begun = Onboarding { step, agent_id: agent.id, cli: None, changed: Vec::new(), goal: None };
    if state.world.begin_onboarding(user.0, &begun).await.map_err(internal)? {
        let person = state.world.user_display_name(user.0).await.map_err(internal)?;
        let hello = match person.as_deref().and_then(|p| p.split_whitespace().next()) {
            Some(first) => format!("Hi {first}!"),
            None => "Hi there!".to_string(),
        };
        let intro = format!(
            "{hello} I am {name}, your main agent. I remember what we work on, keep watch on the repos, threads and checks you point me at, and bring you only the decisions that are yours.\n\nLet us set me up with a few quick questions. Tap an answer or type your own.",
            name = agent.name,
        );
        match tools {
            Some(tools) => say(&state, &agent, &format!("{intro}\n\n{}", ob::tools_report(&tools)), choices(ob::tools_choices(&tools)), Step::Tools).await?,
            None => ask_cli(&state, user.0, &agent, &format!("{intro}\n\nFirst up.")).await?,
        }
    }
    Ok(Json(Started { pending: true, agent_id: Some(agent.id) }))
}

pub async fn answer(state: &AppState, user: &UserId, agent_id: Uuid, text: &str) -> Result<bool, StatusCode> {
    let Some(mut progress) = state.world.onboarding_for(user.0, agent_id).await.map_err(internal)? else {
        return Ok(false);
    };
    let agent = load(state, agent_id).await?;
    if progress.step == Step::Starter {
        match ob::wants_starter(text) {
            Some(true) => {
                let org = state.world.active_org(user.0).await.map_err(internal)?;
                let extra = match crate::routes::starter_team::run(state, user, org, &StarterOptions::default()).await {
                    Ok(view) => {
                        progress.changed.push("starter_team".into());
                        moved_in(&view.built.plan)
                    }
                    Err(_) => " I could not set up the starter team just now; you can try again from the Quest board under Residents.".to_string(),
                };
                finish(state, user, &agent, &progress, false, &extra).await?;
            }
            Some(false) => finish(state, user, &agent, &progress, false, " No starter team for now; you can add one any time from the Quest board under Residents, or just ask me.").await?,
            None => {
                let body = "Should I set up the starter team? Pick one below.";
                say(state, &agent, body, starter_choices(), Step::Starter).await?;
            }
        }
        return Ok(true);
    }
    if progress.step == Step::Scan {
        scan_answer(state, user, &agent, &mut progress, text).await?;
        return Ok(true);
    }
    if progress.step == Step::Goal {
        goal_answer(state, user, &agent, &mut progress, text).await?;
        return Ok(true);
    }
    if ob::is_skip(text) {
        finish(state, user, &agent, &progress, true, "").await?;
        return Ok(true);
    }
    match progress.step {
        Step::Tools => {
            let tools = mac_tools(state, user.0, &agent).await?;
            let checking = ob::wants_check_again(text);
            let ready = tools.as_ref().is_none_or(ob::tools_ready);
            let can_think = tools.as_ref().is_none_or(|tools| !ob::ready_clis(tools).is_empty());
            match tools {
                Some(tools) if (checking && !ready) || !can_think => {
                    let lead = if checking { "Still looking." } else { "I cannot start without a coding CLI yet." };
                    say(state, &agent, &format!("{lead} {}", ob::tools_report(&tools)), choices(ob::tools_choices(&tools)), Step::Tools).await?;
                }
                _ => {
                    progress.step = Step::Cli;
                    state.world.set_onboarding(user.0, &progress).await.map_err(internal)?;
                    let lead = if ready { "Everything I need is ready." } else { "Okay, we can finish the rest later." };
                    ask_cli(state, user.0, &agent, lead).await?;
                }
            }
        }
        Step::Cli => {
            let clis = available_clis(state, user.0, &agent).await?;
            match ob::pick_cli(text, &clis) {
                Some(cli) => {
                    let (default, models) = models_for(state, user.0, &cli).await?;
                    progress.cli = Some(cli.clone());
                    progress.step = Step::Model;
                    state.world.set_onboarding(user.0, &progress).await.map_err(internal)?;
                    let body = if default.is_empty() {
                        format!("{cli} it is. Which model should I use? Type its name.")
                    } else {
                        format!("{cli} it is. Which model should I use? {default} is a good default.")
                    };
                    say(state, &agent, &body, choices(ob::model_choices(&default, &models)), Step::Model).await?;
                }
                None => {
                    let body = format!("I did not catch a CLI there. I can run {}. Which one?", or_list(&clis));
                    say(state, &agent, &body, choices(clis), Step::Cli).await?;
                }
            }
        }
        Step::Model => {
            let cli = progress.cli.clone().unwrap_or_else(|| agent.cli.clone());
            let (default, models) = models_for(state, user.0, &cli).await?;
            let Some(model) = ob::pick_model(text, &default, &models) else {
                let body = "Which model? Pick one, or type its exact name.";
                say(state, &agent, body, choices(ob::model_choices(&default, &models)), Step::Model).await?;
                return Ok(true);
            };
            let update = AgentUpdate { cli: Some(cli.clone()), model: Some(model.clone()), ..Default::default() };
            if let Err(why) = apply(state, user, agent_id, update).await? {
                let body = format!("I could not switch to {cli} {model}: {why}. Try another model.");
                say(state, &agent, &body, choices(ob::model_choices(&default, &models)), Step::Model).await?;
                return Ok(true);
            }
            progress.step = Step::Name;
            state.world.set_onboarding(user.0, &progress).await.map_err(internal)?;
            let body = format!(
                "Done, I now think with {cli} {model}. What should I be called? Keep {name}, or type a new name.",
                name = agent.name
            );
            say(state, &agent, &body, choices(vec![format!("Keep {}", agent.name)]), Step::Name).await?;
        }
        Step::Name => {
            let Some(name) = ob::pick_name(text, &agent.name) else {
                let body = format!("Names are best short, at most {} characters. What should I be called?", ob::NAME_MAX);
                say(state, &agent, &body, choices(vec![format!("Keep {}", agent.name)]), Step::Name).await?;
                return Ok(true);
            };
            if name != agent.name {
                if let Err(why) = apply(state, user, agent_id, AgentUpdate { name: Some(name.clone()), ..Default::default() }).await? {
                    let body = format!("That name did not stick: {why}. Try another?");
                    say(state, &agent, &body, choices(vec![format!("Keep {}", agent.name)]), Step::Name).await?;
                    return Ok(true);
                }
            }
            if name != agent.name {
                progress.changed.push("name".into());
            }
            progress.step = Step::Persona;
            state.world.set_onboarding(user.0, &progress).await.map_err(internal)?;
            let body = format!(
                "Nice to meet you, I am {name}. Last one: do you have a persona in mind for me? Tell me the vibe and what you want help with, or pick one below."
            );
            let mut options = vec![ob::KEEP_PERSONA.to_string()];
            options.extend(ob::PERSONAS.iter().map(|(label, _)| label.to_string()));
            say(state, &agent, &body, choices(options), Step::Persona).await?;
        }
        Step::Persona => {
            if let Some(PersonaChoice::Set(persona)) = ob::pick_persona(text) {
                if let Err(why) = apply(state, user, agent_id, AgentUpdate { persona: Some(persona), ..Default::default() }).await? {
                    let body = format!("That persona did not stick: {why}. Try again, or keep the current one.");
                    say(state, &agent, &body, choices(vec![ob::KEEP_PERSONA.to_string()]), Step::Persona).await?;
                    return Ok(true);
                }
                progress.changed.push("persona".into());
            }
            offer_scan(state, user, &agent, &mut progress).await?;
        }
        Step::Scan | Step::Starter | Step::Goal => {}
    }
    Ok(true)
}

async fn offer_scan(state: &AppState, user: &UserId, agent: &WorldAgent, progress: &mut Onboarding) -> Result<(), StatusCode> {
    let agent = load(state, agent.id).await?;
    match state.world.setup_scan_device(user.0, &agent).await.map_err(internal)? {
        Ok(_) => {
            progress.step = Step::Scan;
            state.world.set_onboarding(user.0, progress).await.map_err(internal)?;
            if let Err(why) = state.world.post_setup_request(user.0, &agent, true).await.map_err(internal)? {
                state.world.post_setup_note(&agent, &why).await.map_err(internal)?;
                return ask_goal(state, user, &agent, progress).await;
            }
            Ok(())
        }
        Err(_) => {
            let note = "Tip: once your Mac is connected, ask me to suggest a setup from how you already use Claude Code, Codex, Cursor and omp.";
            state.world.post_setup_note(&agent, note).await.map_err(internal)?;
            ask_goal(state, user, &agent, progress).await
        }
    }
}

async fn scan_answer(state: &AppState, user: &UserId, agent: &WorldAgent, progress: &mut Onboarding, text: &str) -> Result<(), StatusCode> {
    if loka_shared::setup_scan::is_decline(text) || ob::is_skip(text) || ob::wants_starter(text) == Some(false) {
        return ask_goal(state, user, agent, progress).await;
    }
    let yes = matches!(ob::wants_starter(text), Some(true)) || text.trim().eq_ignore_ascii_case(loka_shared::setup_scan::LOOK);
    if !yes {
        let body = "Tick the CLIs on the card above and press Look at my setup, or say skip.";
        return say(state, agent, body, Vec::new(), Step::Scan).await;
    }
    let note = match state.world.setup_scan_device(user.0, agent).await.map_err(internal)? {
        Ok(device) => {
            let request = loka_shared::setup_scan::ScanRequest::new(&device.sources, loka_shared::setup_scan::DEFAULT_DAYS);
            match request {
                Some(request) => match state.world.start_setup_scan(user.0, agent, &request).await.map_err(internal)? {
                    Ok(_) => format!("Reading the last {} days of {} on your Mac now; my suggestions will show up here as a card.", request.days, request.names()),
                    Err(why) => why,
                },
                None => "None of the CLIs I can read is installed on your Mac.".to_string(),
            }
        }
        Err(why) => why,
    };
    state.world.post_setup_note(agent, &note).await.map_err(internal)?;
    ask_goal(state, user, agent, progress).await
}

pub async fn after_scan(state: &AppState, user: &UserId, agent_id: Uuid) -> Result<(), StatusCode> {
    let Some(mut progress) = state.world.onboarding_for(user.0, agent_id).await.map_err(internal)? else {
        return Ok(());
    };
    if progress.step != Step::Scan {
        return Ok(());
    }
    let agent = load(state, agent_id).await?;
    ask_goal(state, user, &agent, &mut progress).await
}

async fn ask_goal(state: &AppState, user: &UserId, agent: &WorldAgent, progress: &mut Onboarding) -> Result<(), StatusCode> {
    progress.step = Step::Goal;
    state.world.set_onboarding(user.0, progress).await.map_err(internal)?;
    let body = "Last question, and the one that matters most: tell me about your work. What do you do, and where does your day go? For example:\n- Do you work in one GitHub repository, or across your organization's repos?\n- Do you spend a lot of your day in email, your calendar or documents?\n- Do you read a lot of articles, papers or newsletters?\n- Are you on call, or keeping an eye on alerts and incidents?\n- Do you plan work in Linear, Notion or Slack?\n\nA few sentences is plenty. I will come back with a proposed setup that fits how you work, the agents, connections and routines, for you to approve, and you can watch every step as I put it together.";
    say(state, agent, body, choices(vec![ob::GOAL_LATER.to_string()]), Step::Goal).await
}

async fn goal_answer(state: &AppState, user: &UserId, agent: &WorldAgent, progress: &mut Onboarding, text: &str) -> Result<(), StatusCode> {
    if ob::is_skip(text) {
        return finish(state, user, agent, progress, false, "").await;
    }
    match ob::pick_goal(text) {
        Some(goal) => {
            progress.goal = Some(goal);
            progress.changed.push("goal".into());
            finish(state, user, agent, progress, false, "").await
        }
        None => offer_starter(state, user, agent, progress).await,
    }
}

fn starter_choices() -> Vec<String> {
    vec![ob::STARTER_YES.to_string(), ob::STARTER_NO.to_string()]
}

fn moved_in(plan: &Plan) -> String {
    let names: Vec<String> = plan.seats.iter().filter(|s| s.exists).map(|s| format!("{} ({})", s.name, s.job_label)).collect();
    if names.is_empty() {
        return String::new();
    }
    format!(" Your starter team just moved in: {}. I stay your coordinator and hand each of them their work.", or_list(&names).replacen(" or ", " and ", 1))
}

async fn offer_starter(state: &AppState, user: &UserId, agent: &WorldAgent, progress: &mut Onboarding) -> Result<(), StatusCode> {
    let org = state.world.active_org(user.0).await.map_err(internal)?;
    let plan = state.world.starter_plan(user.0, org).await.map_err(internal)?;
    if plan.complete || plan.seats.iter().all(|s| s.exists) {
        return finish(state, user, agent, progress, false, "").await;
    }
    progress.step = Step::Starter;
    state.world.set_onboarding(user.0, progress).await.map_err(internal)?;
    let agent = load(state, agent.id).await?;
    let mut lines = vec![
        "Thanks! One more, optional: I can bring in a small starter team so you do not have to design one. I would stay your coordinator, and these would move in:".to_string(),
    ];
    for seat in plan.seats.iter().filter(|s| !s.exists) {
        lines.push(format!("- {} {}, {} on {} {}: {}", seat.emoji, seat.name, seat.job_label, seat.cli, seat.model, seat.duty));
    }
    lines.push(
        "Everyone keeps their own notes and resets them nightly. Agents watch the pull requests, Slack channels and threads that matter on their own, so nobody sweeps on a timer. You can remove any of them later."
            .to_string(),
    );
    say(state, &agent, &lines.join("\n"), starter_choices(), Step::Starter).await
}

async fn finish(state: &AppState, user: &UserId, before: &WorldAgent, progress: &Onboarding, skipped: bool, extra: &str) -> Result<(), StatusCode> {
    if !state.world.finish_onboarding(user.0).await.map_err(internal)? {
        return Ok(());
    }
    let agent = load(state, before.id).await?;
    let has_device = match state.runners.agent_runner(user.0, agent.id).await.map_err(internal)? {
        Some(runner) => runner.status == "online",
        None => agent.runner_id.is_none() && state.runners.cloud_online().await.map_err(internal)?,
    };
    let device = if has_device {
        String::new()
    } else {
        " One thing before I can think: I need an execution device. Open your profile card at the top left and pair this computer under Devices.".to_string()
    };
    let goal = progress.goal.as_deref().filter(|g| !g.trim().is_empty());
    let body = if skipped {
        format!(
            "No problem, I kept my defaults: {} {}. You can change my CLI, model and persona any time in my Settings tab.{device} Say hi whenever you are ready.",
            agent.cli, agent.model
        )
    } else {
        match goal {
            Some(_) if has_device => format!(
                "All set. I am {}, thinking with {} {}. Now the real work: I am putting together a proposed setup for how you work. You can watch it come together step by step, and nothing is added or installed until you approve it.",
                agent.name, agent.cli, agent.model
            ),
            Some(_) => format!(
                "All set. I am {}, thinking with {} {}.{device} As soon as I can, I will put together a proposed setup for how you work, for you to approve.",
                agent.name, agent.cli, agent.model
            ),
            None => format!(
                "All set. I am {}, thinking with {} {}.{extra}{device} Tell me what you are working on{}.",
                agent.name,
                agent.cli,
                agent.model,
                if progress.changed.iter().any(|c| c == "starter_team") { " and I will hand it to the right one" } else { ", or ask me to bring in some agents to help" }
            ),
        }
    };
    say(state, &agent, &body, Vec::new(), Step::Persona).await?;
    let person = state.world.user_display_name(user.0).await.map_err(internal)?;
    state
        .world
        .emit(
            NewEvent::new(ob::FINISHED_KIND)
                .agent(agent.id)
                .actor(Some(user.0))
                .payload(json!({
                    "author": person,
                    "skipped": skipped,
                    "cli": agent.cli,
                    "model": agent.model,
                    "name": agent.name,
                    "renamed": progress.changed.iter().any(|c| c == "name"),
                    "persona_changed": progress.changed.iter().any(|c| c == "persona"),
                    "starter_team": progress.changed.iter().any(|c| c == "starter_team"),
                    "goal": goal,
                    "has_device": has_device,
                    "quiet": true,
                })),
            Some((agent.id, ob::FINISHED_KIND)),
        )
        .await
        .map_err(internal)?;
    Ok(())
}

async fn apply(state: &AppState, user: &UserId, id: Uuid, update: AgentUpdate) -> Result<Result<(), String>, StatusCode> {
    if opencode_update_allowed(state, user.0, id, &update).await.is_err() {
        return Ok(Err("OpenCode is off on this device, pick another CLI".into()));
    }
    if let Some(message) = invalid_cli_switch(state, id, &update).await? {
        return Ok(Err(message));
    }
    match apply_agent_update(state.clone(), user.clone(), id, update).await {
        Ok(_) => Ok(Ok(())),
        Err((status, Json(body))) if status.is_client_error() => {
            Ok(Err(body.get("message").and_then(Value::as_str).unwrap_or("that was refused").replace('_', " ")))
        }
        Err((status, _)) => Err(status),
    }
}

async fn say(state: &AppState, agent: &WorldAgent, body: &str, choices: Vec<String>, step: Step) -> Result<(), StatusCode> {
    let meta = if choices.is_empty() {
        json!({ "onboarding": "done" })
    } else {
        json!({ "kind": "ask", "choices": choices, "onboarding": step })
    };
    let msg = state
        .world
        .insert_message(NewMessage {
            agent_id: agent.id,
            author_kind: "agent",
            author_user_id: None,
            author_agent_id: Some(agent.id),
            body,
            session_id: None,
            run_id: None,
            meta,
        })
        .await
        .map_err(internal)?;
    state
        .world
        .emit(
            NewEvent::new("message.created").agent(agent.id).payload(json!({
                "message_id": msg.id,
                "author": agent.name,
                "author_kind": "agent",
                "body": body,
                "onboarding": true,
                "quiet": true,
            })),
            None,
        )
        .await
        .map_err(internal)?;
    Ok(())
}

fn choices(mut options: Vec<String>) -> Vec<String> {
    options.push(ob::SKIP.to_string());
    options
}

fn or_list(items: &[String]) -> String {
    match items {
        [] => "nothing yet".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

async fn backends(state: &AppState, user: Uuid) -> Result<serde_json::Map<String, Value>, StatusCode> {
    Ok(crate::routes::config::resolved(state, user)
        .await?
        .get("backends")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

async fn mac_tools(state: &AppState, user: Uuid, agent: &WorldAgent) -> Result<Option<Value>, StatusCode> {
    let runner = state.runners.agent_runner(user, agent.id).await.map_err(internal)?;
    Ok(runner
        .filter(|runner| runner.platform == "macos")
        .and_then(|runner| runner.capabilities.get("tools").cloned())
        .filter(|tools| tools.as_object().is_some_and(|tools| !tools.is_empty())))
}

async fn ask_cli(state: &AppState, user: Uuid, agent: &WorldAgent, lead: &str) -> Result<(), StatusCode> {
    let clis = available_clis(state, user, agent).await?;
    let body = format!("{lead} Which CLI should I think with? Here I can run {}.", or_list(&clis));
    say(state, agent, &body, choices(clis), Step::Cli).await
}

async fn available_clis(state: &AppState, user: Uuid, agent: &WorldAgent) -> Result<Vec<String>, StatusCode> {
    let known = backends(state, user).await?;
    let runner = state.runners.agent_runner(user, agent.id).await.map_err(internal)?;
    let missing = |cli: &str| {
        runner
            .as_ref()
            .and_then(|r| r.capabilities.get("backends"))
            .and_then(|b| b.get(cli))
            .and_then(Value::as_bool)
            == Some(false)
    };
    let ready = runner
        .as_ref()
        .and_then(|r| r.capabilities.get("tools"))
        .filter(|tools| tools.as_object().is_some_and(|tools| !tools.is_empty()))
        .map(ob::ready_clis);
    let mut clis: Vec<String> = ob::CLIS
        .iter()
        .filter(|c| known.contains_key(**c) && !missing(c) && ready.as_ref().is_none_or(|ready| ready.iter().any(|r| r == **c)))
        .map(|c| c.to_string())
        .collect();
    if clis.is_empty() && !clis.contains(&agent.cli) {
        clis.insert(0, agent.cli.clone());
    }
    Ok(clis)
}

async fn models_for(state: &AppState, user: Uuid, cli: &str) -> Result<(String, Vec<String>), StatusCode> {
    let known = backends(state, user).await?;
    let entry = known.get(cli);
    let models: Vec<String> = entry
        .and_then(|e| e.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_string))
        .collect();
    let default = entry
        .and_then(|e| e.get("default_model"))
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| models.first().cloned())
        .unwrap_or_default();
    Ok((default, models))
}
