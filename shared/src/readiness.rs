use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::runners::{RunnerStore, RunnerView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ready,
    NeedsSetup,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Devices,
    Keys,
    Github,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fix {
    Switch { setting: &'static str, runner_id: Option<Uuid>, label: String },
    Command { command: String, runner_id: Uuid, runner: String, interactive: bool },
}

#[derive(Debug, Clone, Serialize)]
pub struct Feature {
    pub key: &'static str,
    pub label: &'static str,
    pub state: State,
    pub reason: String,
    pub step: Option<String>,
    pub action: Option<Action>,
    pub fix: Option<Fix>,
}

impl Feature {
    fn fixed_by(mut self, fix: Option<Fix>) -> Self {
        self.fix = fix;
        self
    }
}

fn switch(setting: &'static str, runner: Option<&RunnerView>, label: impl Into<String>) -> Option<Fix> {
    Some(Fix::Switch { setting, runner_id: runner.map(|r| r.id), label: label.into() })
}

fn command(command: &str, runner: Option<&RunnerView>, interactive: bool) -> Option<Fix> {
    runner.map(|r| Fix::Command { command: command.to_string(), runner_id: r.id, runner: r.name.clone(), interactive })
}

fn first_online<'a>(runners: &[&'a RunnerView]) -> Option<&'a RunnerView> {
    runners.iter().copied().find(|r| online(r))
}

#[derive(Debug, Clone, Serialize)]
pub struct Readiness {
    pub features: Vec<Feature>,
    pub clis: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Signals {
    pub own: Vec<RunnerView>,
    pub cloud_online: bool,
    pub slack_recent: bool,
    pub github_feed: bool,
    pub fal_key: bool,
    pub key_storage: bool,
    pub opencode: bool,
}

pub async fn gather(pool: &PgPool, owner: Uuid) -> Result<Signals> {
    let runners = RunnerStore::new(pool.clone());
    let own = runners.list(owner).await?;
    let cloud_online = runners.cloud_online().await.unwrap_or(false);
    let slack_recent = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM slack_seen WHERE user_id = $1 AND created_at > NOW() - INTERVAL '14 days')",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    let github_feed = sqlx::query_scalar::<_, bool>("SELECT github_feed FROM users WHERE id = $1")
        .bind(owner)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    let opencode = crate::backends::opencode_enabled() || own.iter().any(|r| crate::backends::runner_offers_opencode(&r.capabilities));
    Ok(Signals {
        own,
        cloud_online,
        slack_recent,
        github_feed,
        key_storage: crate::environment::configured(),
        opencode,
        ..Signals::default()
    })
}

fn online(runner: &RunnerView) -> bool {
    runner.status == "online"
}

fn desktop(runner: &RunnerView) -> bool {
    runner.platform == "macos"
}

fn flag(runner: &RunnerView, key: &str) -> Option<bool> {
    runner.capabilities.get(key).and_then(Value::as_bool)
}

fn names<'a>(runners: impl Iterator<Item = &'a RunnerView>) -> String {
    runners.map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn feature(key: &'static str, label: &'static str, state: State, reason: impl Into<String>, step: Option<String>, action: Option<Action>) -> Feature {
    Feature { key, label, state, reason: reason.into(), step, action, fix: None }
}

fn ready(key: &'static str, label: &'static str, reason: impl Into<String>) -> Feature {
    feature(key, label, State::Ready, reason, None, None)
}

fn setup(key: &'static str, label: &'static str, reason: impl Into<String>, step: impl Into<String>, action: Option<Action>) -> Feature {
    feature(key, label, State::NeedsSetup, reason, Some(step.into()), action)
}

fn unavailable(key: &'static str, label: &'static str, reason: impl Into<String>) -> Feature {
    feature(key, label, State::Unavailable, reason, None, None)
}

pub fn clis(signals: &Signals) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for runner in signals.own.iter().filter(|r| online(r)) {
        let Some(backends) = runner.capabilities.get("backends").and_then(Value::as_object) else {
            continue;
        };
        for (cli, on) in backends {
            if on.as_bool() == Some(true) && !found.contains(cli) {
                found.push(cli.clone());
            }
        }
    }
    found.sort();
    found
}

fn devices(s: &Signals) -> Feature {
    const KEY: &str = "devices";
    const LABEL: &str = "Execution devices";
    let live: Vec<&RunnerView> = s.own.iter().filter(|r| online(r)).collect();
    if !live.is_empty() {
        return ready(KEY, LABEL, format!("Online: {}.", names(live.into_iter())));
    }
    if s.cloud_online {
        return ready(KEY, LABEL, "Agents run on the world server.");
    }
    if !s.own.is_empty() {
        return setup(
            KEY,
            LABEL,
            format!("Your devices are offline: {}.", names(s.own.iter())),
            "Open the Loka desktop app on one of them, or start its runner.",
            Some(Action::Devices),
        );
    }
    setup(
        KEY,
        LABEL,
        "No execution device is connected, so agents have nowhere to run.",
        "Install the Loka desktop app on your Mac and connect it.",
        Some(Action::Devices),
    )
}

fn terminal(s: &Signals) -> Feature {
    const KEY: &str = "terminal";
    const LABEL: &str = "Terminal";
    let capable: Vec<&RunnerView> = s.own.iter().filter(|r| flag(r, "terminal") != Some(false)).collect();
    let live: Vec<&RunnerView> = capable.iter().copied().filter(|r| online(r)).collect();
    if !live.is_empty() {
        return ready(KEY, LABEL, format!("Open a shell on {}.", names(live.into_iter())));
    }
    if !capable.is_empty() {
        return setup(
            KEY,
            LABEL,
            format!("None of your devices are online ({}).", names(capable.into_iter())),
            "Start one of them, then open the terminal again.",
            Some(Action::Devices),
        );
    }
    setup(
        KEY,
        LABEL,
        "You have no execution device connected.",
        "Connect your Mac with the Loka desktop app.",
        Some(Action::Devices),
    )
}

fn browser(s: &Signals) -> Feature {
    const KEY: &str = "browser";
    const LABEL: &str = "Browser for agents";
    let with: Vec<&RunnerView> = s.own.iter().filter(|r| flag(r, "browser") == Some(true)).collect();
    if with.iter().any(|r| online(r)) {
        return ready(KEY, LABEL, format!("Agents on {} can browse the web.", names(with.into_iter().filter(|r| online(r)))));
    }
    if !with.is_empty() {
        return setup(
            KEY,
            LABEL,
            format!("The device with a browser is offline ({}).", names(with.into_iter())),
            "Open the Loka desktop app on it.",
            Some(Action::Devices),
        );
    }
    let macs: Vec<&RunnerView> = s.own.iter().filter(|r| desktop(r)).collect();
    if !macs.is_empty() {
        let fix = command("npm install -g agent-browser", first_online(&macs), false);
        return setup(
            KEY,
            LABEL,
            format!("agent-browser is not installed on {}.", names(macs.into_iter())),
            "Run npm install -g agent-browser on that Mac, then restart the Loka app.",
            Some(Action::Devices),
        )
        .fixed_by(fix);
    }
    setup(
        KEY,
        LABEL,
        "Only desktop devices can browse for agents, and you have none connected.",
        "Connect your Mac with the Loka desktop app and install agent-browser on it.",
        Some(Action::Devices),
    )
}

fn screen(s: &Signals) -> Feature {
    const KEY: &str = "screen";
    const LABEL: &str = "Screenshots of your Mac";
    let macs: Vec<&RunnerView> = s.own.iter().filter(|r| desktop(r) && flag(r, "screen") == Some(true)).collect();
    if macs.is_empty() {
        return unavailable(KEY, LABEL, "Only the Loka app on a Mac can share its screen.");
    }
    let on: Vec<&RunnerView> = macs.iter().copied().filter(|r| r.screen_access).collect();
    if on.iter().any(|r| online(r)) {
        return ready(KEY, LABEL, format!("Quests on {} can capture the screen to show you.", names(on.into_iter().filter(|r| online(r)))));
    }
    if !on.is_empty() {
        return setup(KEY, LABEL, format!("The Mac that shares its screen is offline ({}).", names(on.into_iter())), "Open the Loka desktop app on it.", Some(Action::Devices));
    }
    let target = first_online(&macs).or(macs.first().copied());
    setup(
        KEY,
        LABEL,
        "Screen capture is off on your Macs.",
        "Turn on Let quests see this screen under Devices, then allow Loka under Screen Recording in System Settings.",
        Some(Action::Devices),
    )
    .fixed_by(switch("screen_access", target, format!("Let quests see the screen of {}", target.map(|r| r.name.as_str()).unwrap_or("this Mac"))))
}

fn apps(s: &Signals) -> Feature {
    const KEY: &str = "apps";
    const LABEL: &str = "Apps on your Mac";
    let macs: Vec<&RunnerView> = s.own.iter().filter(|r| desktop(r)).collect();
    if macs.is_empty() {
        return unavailable(KEY, LABEL, "Only the Loka app on a Mac can let quests use its apps.");
    }
    let with: Vec<&RunnerView> = macs.iter().copied().filter(|r| flag(r, "apps") == Some(true)).collect();
    if with.is_empty() {
        let fix = command("npm install -g agent-desktop", first_online(&macs), false);
        return setup(
            KEY,
            LABEL,
            format!("agent-desktop is not installed on {}.", names(macs.into_iter())),
            "Run npm install -g agent-desktop on that Mac, then restart the Loka app.",
            Some(Action::Devices),
        )
        .fixed_by(fix);
    }
    let on: Vec<&RunnerView> = with.iter().copied().filter(|r| r.app_control).collect();
    if on.iter().any(|r| online(r)) {
        return ready(KEY, LABEL, format!("Quests on {} can use its apps once you allow it for that quest.", names(on.into_iter().filter(|r| online(r)))));
    }
    if !on.is_empty() {
        return setup(KEY, LABEL, format!("The Mac that allows apps is offline ({}).", names(on.into_iter())), "Open the Loka desktop app on it.", Some(Action::Devices));
    }
    let target = first_online(&with).or(with.first().copied());
    setup(
        KEY,
        LABEL,
        "Using apps is off on your Macs.",
        "Turn on Let quests use apps on this Mac under Devices, then allow Loka under Accessibility in System Settings.",
        Some(Action::Devices),
    )
    .fixed_by(switch("app_control", target, format!("Let quests use apps on {}", target.map(|r| r.name.as_str()).unwrap_or("this Mac"))))
}

fn slack(s: &Signals) -> Feature {
    const KEY: &str = "slack";
    const LABEL: &str = "Slack";
    if s.slack_recent {
        return ready(KEY, LABEL, "Slack notifications are reaching your agents.");
    }
    let macs: Vec<&RunnerView> = s.own.iter().filter(|r| desktop(r)).collect();
    if !macs.is_empty() {
        return setup(
            KEY,
            LABEL,
            "No Slack notifications have arrived in the last two weeks.",
            format!("Turn on Slack capture in the Loka desktop app on {}.", names(macs.into_iter())),
            Some(Action::Devices),
        )
        .fixed_by(switch("slack_capture", None, "Capture Slack notifications on this Mac"));
    }
    setup(
        KEY,
        LABEL,
        "Slack capture runs in the Loka desktop app, and you have no Mac connected.",
        "Install the Loka desktop app on the Mac where you use Slack and turn on Slack capture.",
        Some(Action::Devices),
    )
}

fn github_feed(s: &Signals) -> Feature {
    const KEY: &str = "github_feed";
    const LABEL: &str = "GitHub feed";
    let macs: Vec<&RunnerView> = s.own.iter().filter(|r| desktop(r) && r.status != "revoked").collect();
    if macs.is_empty() {
        return setup(
            KEY,
            LABEL,
            "The GitHub feed reads your notifications on your Mac with its gh login, and you have no Mac connected.",
            "Install the Loka desktop app on a Mac where gh is signed in.",
            Some(Action::Devices),
        );
    }
    if !s.github_feed {
        return setup(KEY, LABEL, "The GitHub feed is off, so agents only hear about GitHub when they look.", "Turn it on in You, under GitHub.", Some(Action::Github))
            .fixed_by(switch("github_feed", None, "Send my GitHub notifications to my agents"));
    }
    let cap = |r: &RunnerView| r.capabilities.get("github_feed").cloned().unwrap_or(Value::Null);
    let live: Vec<&RunnerView> = macs.iter().copied().filter(|r| online(r)).collect();
    if let Some(reader) = live
        .iter()
        .find(|r| cap(r).get("available").and_then(Value::as_bool) == Some(true) && cap(r).get("problem").is_none_or(Value::is_null))
    {
        let login = cap(reader).get("login").and_then(Value::as_str).map(|l| format!(" as @{l}")).unwrap_or_default();
        return ready(KEY, LABEL, format!("Reading your GitHub notifications on {}{login}.", reader.name));
    }
    if let Some((runner, problem)) = live.iter().find_map(|r| cap(r).get("problem").and_then(Value::as_str).map(|p| (*r, p.to_string()))) {
        return setup(KEY, LABEL, format!("{} cannot read GitHub: {problem}", runner.name), "Run gh auth login in a terminal on that Mac.", Some(Action::Github))
            .fixed_by(command("gh auth login", Some(runner), true));
    }
    if !live.is_empty() {
        return setup(
            KEY,
            LABEL,
            format!("{} runs a Loka desktop app without the GitHub feed.", names(live.into_iter())),
            "Update the Loka desktop app.",
            Some(Action::Devices),
        );
    }
    setup(
        KEY,
        LABEL,
        format!("{} is offline, so GitHub notifications wait until it is back.", names(macs.into_iter())),
        "Open the Loka desktop app on your Mac.",
        Some(Action::Devices),
    )
}

fn coding_clis(s: &Signals) -> Feature {
    const KEY: &str = "clis";
    const LABEL: &str = "Coding CLIs";
    let macs: Vec<&RunnerView> = s.own.iter().filter(|r| desktop(r) && r.capabilities.get("tools").is_some_and(Value::is_object)).collect();
    let Some(mac) = first_online(&macs).or(macs.first().copied()) else {
        let found = clis(s);
        return if found.is_empty() {
            unavailable(KEY, LABEL, "No device reports which coding CLIs it has.")
        } else {
            ready(KEY, LABEL, format!("On your online devices: {}.", found.join(", ")))
        };
    };
    let tools = &mac.capabilities["tools"];
    let problems = crate::onboarding::tool_problems(tools);
    if problems.is_empty() {
        return ready(KEY, LABEL, format!("Signed in on {}: {}.", mac.name, crate::onboarding::ready_tool_names(tools).join(", ")));
    }
    let blocking = crate::onboarding::ready_clis(tools).is_empty();
    let first = &problems[0];
    let reason = format!(
        "On {}: {}.{}",
        mac.name,
        problems.iter().map(|p| format!("{} is {}", p.name, if p.missing { "not installed" } else { "not signed in" })).collect::<Vec<_>>().join(", "),
        if blocking { " Agents there cannot run until one coding CLI is signed in." } else { "" }
    );
    let step = if first.missing {
        format!("Install {} with {}, then sign in with {}.", first.name, first.install, first.sign_in)
    } else {
        format!("Sign in to {} with {} in a terminal on that Mac.", first.name, first.sign_in)
    };
    let fix = if first.missing { command(first.install, Some(mac).filter(|m| online(m)), false) } else { command(first.sign_in, Some(mac).filter(|m| online(m)), true) };
    setup(KEY, LABEL, reason, step, Some(Action::Devices)).fixed_by(fix)
}

fn live_voice(s: &Signals) -> Feature {
    const KEY: &str = "live_voice";
    const LABEL: &str = "Live voice";
    if s.fal_key {
        return ready(KEY, LABEL, "Realtime voice through fal is available.");
    }
    if s.key_storage {
        return setup(
            KEY,
            LABEL,
            "Live voice needs a fal API key. On-device voice still works.",
            "Add FAL_KEY in You, under Environment.",
            Some(Action::Keys),
        );
    }
    unavailable(KEY, LABEL, "The operator has not set FAL_KEY and key storage is off. On-device voice still works.")
}

fn keys(s: &Signals) -> Feature {
    if s.key_storage {
        ready("keys", "Keys", "You can store API keys and grant them to private agents.")
    } else {
        unavailable("keys", "Keys", "Secure key storage is not configured by the operator.")
    }
}

fn opencode(s: &Signals) -> Feature {
    if s.opencode {
        ready("opencode", "OpenCode", "Agents can run on OpenCode.")
    } else {
        unavailable("opencode", "OpenCode", "OpenCode is turned off on this server and on your devices.")
    }
}

pub fn evaluate(signals: &Signals) -> Readiness {
    Readiness {
        features: vec![
            devices(signals),
            coding_clis(signals),
            terminal(signals),
            browser(signals),
            screen(signals),
            apps(signals),
            slack(signals),
            github_feed(signals),
            live_voice(signals),
            keys(signals),
            opencode(signals),
        ],
        clis: clis(signals),
    }
}

pub const SHOW_HOW: &str = "To help your person fix one that needs setup, say in one line what it unlocks and put setup:<key> alone on its own line: they get a card with the switch, the command to run or the right settings page, so never send them hunting through settings. A command a card cannot cover goes alone in a fenced bash block, which gets a Run button.";

pub fn find<'a>(readiness: &'a Readiness, key: &str) -> Option<&'a Feature> {
    let want = key.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    readiness.features.iter().find(|f| f.key == want)
}

pub fn summary(readiness: &Readiness) -> String {
    let mut lines: Vec<String> = readiness
        .features
        .iter()
        .map(|f| {
            let state = match f.state {
                State::Ready => "ready",
                State::NeedsSetup => "needs setup",
                State::Unavailable => "unavailable",
            };
            match &f.step {
                Some(step) => {
                    let card = match &f.fix {
                        Some(Fix::Switch { .. }) => "one-click switch",
                        Some(Fix::Command { interactive: false, .. }) => "command with a Run button",
                        Some(Fix::Command { .. }) => "command and a terminal button",
                        None => "button to the right settings page",
                    };
                    format!("- {} ({}): {} Next step: {} Show it with setup:{} ({card}).", f.label, state, f.reason, step, f.key)
                }
                None => format!("- {} ({}): {}", f.label, state, f.reason),
            }
        })
        .collect();
    lines.push(if readiness.clis.is_empty() {
        "- CLIs on online devices: none".to_string()
    } else {
        format!("- CLIs on online devices: {}", readiness.clis.join(", "))
    });
    lines.push(SHOW_HOW.to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn runner(name: &str, platform: &str, status: &str, capabilities: Value) -> RunnerView {
        RunnerView {
            id: Uuid::new_v4(),
            name: name.into(),
            platform: platform.into(),
            status: status.into(),
            last_seen_at: Some(Utc::now()),
            owner_user_id: Uuid::nil(),
            capabilities,
            created_at: Utc::now(),
            shared: false,
            screen_access: false,
            app_control: false,
        }
    }

    fn state(readiness: &Readiness, key: &str) -> State {
        readiness.features.iter().find(|f| f.key == key).map(|f| f.state).unwrap()
    }

    #[test]
    fn nothing_connected_asks_for_a_device() {
        let r = evaluate(&Signals::default());
        assert_eq!(state(&r, "devices"), State::NeedsSetup);
        assert_eq!(state(&r, "terminal"), State::NeedsSetup);
        assert_eq!(state(&r, "live_voice"), State::Unavailable);
        assert!(r.clis.is_empty());
    }

    #[test]
    fn online_mac_unlocks_terminal_and_browser() {
        let mac = runner("Studio", "macos", "online", json!({"browser": true, "backends": {"claude": true, "codex": false}}));
        let r = evaluate(&Signals { own: vec![mac], key_storage: true, ..Signals::default() });
        assert_eq!(state(&r, "devices"), State::Ready);
        assert_eq!(state(&r, "terminal"), State::Ready);
        assert_eq!(state(&r, "browser"), State::Ready);
        assert_eq!(state(&r, "live_voice"), State::NeedsSetup);
        assert_eq!(r.clis, vec!["claude".to_string()]);
    }

    #[test]
    fn features_that_need_setup_carry_their_fix() {
        let mut mac = runner("Studio", "macos", "online", json!({"apps": true, "screen": true, "tools": {"claude": {"installed": true, "signed_in": false}, "gh": {"installed": false}}}));
        let id = mac.id;
        let r = evaluate(&Signals { own: vec![mac.clone()], ..Signals::default() });
        let fix = |key: &str| r.features.iter().find(|f| f.key == key).and_then(|f| f.fix.clone());
        assert_eq!(fix("apps"), Some(Fix::Switch { setting: "app_control", runner_id: Some(id), label: "Let quests use apps on Studio".into() }));
        assert_eq!(fix("screen"), Some(Fix::Switch { setting: "screen_access", runner_id: Some(id), label: "Let quests see the screen of Studio".into() }));
        assert_eq!(fix("browser"), Some(Fix::Command { command: "npm install -g agent-browser".into(), runner_id: id, runner: "Studio".into(), interactive: false }));
        assert_eq!(fix("slack"), Some(Fix::Switch { setting: "slack_capture", runner_id: None, label: "Capture Slack notifications on this Mac".into() }));
        assert_eq!(fix("github_feed"), Some(Fix::Switch { setting: "github_feed", runner_id: None, label: "Send my GitHub notifications to my agents".into() }));
        assert_eq!(fix("clis"), Some(Fix::Command { command: "claude auth login".into(), runner_id: id, runner: "Studio".into(), interactive: true }));
        assert_eq!(state(&r, "clis"), State::NeedsSetup);
        let text = summary(&r);
        assert!(text.contains("Show it with setup:apps (one-click switch)"));
        assert!(text.contains("setup:browser (command with a Run button)"));
        assert!(text.contains("setup:clis (command and a terminal button)"));
        assert!(text.ends_with(SHOW_HOW));
        assert_eq!(find(&r, "GitHub-Feed").map(|f| f.key), Some("github_feed"));

        mac.capabilities = json!({"apps": false, "tools": {"claude": {"installed": true, "signed_in": true}, "gh": {"installed": true, "signed_in": true}}});
        let r = evaluate(&Signals { own: vec![mac.clone()], ..Signals::default() });
        assert_eq!(state(&r, "clis"), State::Ready);
        assert!(r.features.iter().any(|f| f.key == "apps" && matches!(&f.fix, Some(Fix::Command { command, .. }) if command == "npm install -g agent-desktop")));

        mac.status = "offline".into();
        mac.capabilities = json!({"tools": {"claude": {"installed": false}}});
        let r = evaluate(&Signals { own: vec![mac], ..Signals::default() });
        assert!(r.features.iter().find(|f| f.key == "clis").is_some_and(|f| f.state == State::NeedsSetup && f.fix.is_none()));
        assert_eq!(state(&evaluate(&Signals::default()), "clis"), State::Unavailable);
    }

}
