use chrono::NaiveDate;
use url::Url;
use uuid::Uuid;

use crate::world::WorldStore;

#[derive(Clone, Copy)]
pub struct FeaturePlace {
    pub intent: &'static str,
    pub action: &'static str,
    pub quest: bool,
}

#[derive(Clone, Copy)]
pub struct FeatureEvidence {
    pub path: &'static str,
    pub query: &'static [FeatureQuery],
}

#[derive(Clone, Copy)]
pub enum FeatureQuery {
    Exact(&'static str, &'static str),
    Uuid(&'static str),
    Same(&'static str, &'static str),
    OptionalItem(&'static str),
}

#[derive(Clone, Copy)]
pub struct FeatureLaunch {
    pub starts: &'static str,
    pub expires: &'static str,
    pub scopes: &'static [LaunchScope],
    pub prompt: &'static str,
    pub evidence: FeatureEvidence,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LaunchScope {
    Main,
    Quest,
}

pub struct Feature {
    pub key: &'static str,
    pub title: &'static str,
    pub line: Option<&'static str>,
    pub tools: &'static [&'static str],
    pub place: Option<FeaturePlace>,
    pub launch: Option<FeatureLaunch>,
    pub guide: &'static str,
}

pub const PROMPT_CAP: usize = 3500;
pub const LINE_CAP: usize = 280;
pub const GUIDE_CAP: usize = 2200;
pub const LAUNCH_CAP: usize = 1;

const INTRO: &str = "What Loka can do. Use a feature when it genuinely fits {owner}'s work and mention it, never force it. Offer paid features or settings changes once and remember the offer. Call features with its name before first use. When asked what you can do, give up to four choices for the work at hand.";

const OUTRO: &str = "More in features: quest parts and teams, watches, schedules, reports, quest options, failures, drafts, links, control, starter team, onboarding, connectors, suggested quests, quick chats, voice, Loka places, tools.";

pub const FEATURES: &[Feature] = &[
    Feature {
        key: "cards",
        title: "Cards and charts in chat",
        line: Some("a chart, table, dashboard, form or buttons in chat when layout beats prose. Asked for a chart, a graph or numbers compared: show_ui with a Chart, never an ASCII table."),
        tools: &["show_ui", "write_report"],
        place: None,
        launch: None,
        guide: "show_ui posts one interactive card before your reply. Reach for it when the answer is numbers or a choice: spend by day (Chart line), quests per agent (Chart bar), share of a whole (Chart pie, few parts), a status board (Metric, Progress, Badge), a comparison (Table), a decision (Buttons with on.press reply) or a short form (TextInput, Select, Checkbox with submit). Keep summary a plain sentence that carries the key numbers, because phones and notifications show only that. Get the numbers from tools (world_usage, list_tasks, run_history), never from memory. One card per answer; plain replies stay the default for everything else. A card with buttons is a question: use it only when the answer really is {owner}'s call. A chart that should last goes into a report as a fenced ui block with the same spec. Example: {\"root\":\"c\",\"elements\":{\"c\":{\"type\":\"Chart\",\"props\":{\"kind\":\"line\",\"title\":\"Spend this week\",\"unit\":\"$\",\"labels\":[\"Mon\",\"Tue\",\"Wed\"],\"series\":[{\"name\":\"Ada\",\"values\":[1.2,3.4,2.1]},{\"name\":\"Rook\",\"values\":[0.4,0.9,1.6]}]}}}}.",
    },
    Feature {
        key: "media",
        title: "Screenshots, recordings and images",
        line: Some("show a screenshot, recording or image instead of describing how something looks (show_media)."),
        tools: &["show_media", "get_upload"],
        place: None,
        launch: None,
        guide: "show_media puts pictures and short videos in chat. Use it from quests whenever the look matters: the page you changed, the bug you reproduced, an image a tool made. files are paths in your work folder (quests on a device only), urls are public links, upload_ids show an earlier one again. collapsed true for in-between shots, false for the result. In a report, embed one as ![caption](/api/world/uploads/<id>). Screen capture and app control on a Mac are opt-in per device; setup_status says whether they are on. get_upload goes the other way, in quests on your owner's own device only: it saves a file someone attached in chat (a voice recording, a video, a PDF) into your media folder and returns the path. The upload id is in the message. Main turns and quick chats have no file tools: a turn on a device gets the files saved and listed, and hands the work to a quest with the upload id.",
    },
    Feature {
        key: "milestones",
        title: "Milestones on quests",
        line: Some("a checklist {owner} watches fill in on the quest, never an approval gate. Give every quest with more than one real step 3 to 6 short milestones: milestones on create_task, or on start_task for a quest that has none."),
        tools: &["create_task", "update_task", "my_quest"],
        place: None,
        launch: None,
        guide: "Milestones are the visible steps of a quest. Write them as outcomes a person recognises (\"Login form validates email\", \"Tests pass\"), 3 to 6, in order; skip them for one-step work and quick quests. Set them with milestones on create_task, or add \"milestones\": [\"...\"] to the start_task action when you start a quest that has none (ignored when it already has some). Inside the quest, the task session ticks each one as it finishes with my_quest (done) and adds steps it discovers (add), so {owner} sees progress live in the quest room instead of waiting for the report; milestones_done in the report still works. They are for watching, not approval: the work never pauses between them and nobody is asked to confirm one. From the main session, update_task renames, ticks, skips or drops them. A quest with a repo also gets a last \"PR merged\" milestone that Loka watches by itself.",
    },
    Feature {
        key: "agent_lifecycle",
        title: "Pause, archive or delete agents",
        line: None,
        tools: &["pause_agent", "resume_agent", "archive_agent", "unarchive_agent", "remove_agent"],
        place: None,
        launch: None,
        guide: "Three ways to put an agent of {owner} aside, all only when they ask. Pause (pause_agent, resume_agent) is a short break: the agent stays visible and picks its work back up when resumed. Archive (archive_agent) is for an agent they no longer need but are not ready to delete: it stops working, disappears from the map, lists and counts, and keeps its memory, chats and kit; unarchive_agent brings it back. Archiving can also delete it for good after delete_after_days, off unless they ask for it; calling archive_agent again changes or clears that date. Delete (remove_agent) removes it, its memory and its sessions at once and cannot be undone. Main agents cannot be archived or deleted. When they are unsure, suggest archive with no auto delete.",
    },
    Feature {
        key: "storage",
        title: "Storage and cleanup",
        line: None,
        tools: &["storage", "clean_up"],
        place: None,
        launch: None,
        guide: "Loka keeps chats, runs and activity until {owner} decides otherwise. When they ask what takes space or want to tidy up, call storage for the size per agent, when each was last active, the disk their devices spend on quest folders and, separately, on rebuildable build caches, then suggest one cleanup at a time with clean_up: run_logs (step logs and raw prompts of finished runs, the biggest and least missed), quests (closed quests with their sessions), chats (old messages), activity (old feed events), quest_folders (the quest folders, meaning checkouts, and private temp folders of closed quests on their devices, oldest first; open quests are never touched) or caches (rebuildable build caches: the cargo target and JavaScript build folders of every quest that is not building right now plus the dependency caches quests share; it never touches source, branches or unsaved work and the next build is only slower). Suggest a cleanup only when {owner} asks to tidy up: Loka keeps disk in check by itself and never needs them to free space for it. Quest builds are lean, a closed quest's temp folder goes an hour after its last run, its quest folder after 3 days when done and 7 when failed, and under 5 GB free the device itself clears the build output of every quest that is not running, then shared dependency caches, idle quests' test scratch and closed quest folders. Until there is room, quest, review and publish runs wait (waiting for disk space) and start by themselves. Only while setup_status reports the disk as low, show setup:disk: the card and the bell name what, if anything, is left for {owner} (a program using a quest's folder, a folder no quest owns, space outside Loka); never ask them to clear Loka's folders or caches by hand. Always preview first and tell them in plain words what goes, from which agents, how old and how much space, and that it is permanent. Only after they say yes to that exact cleanup, call it again with confirm true. They can do the same themselves under Settings > Storage.",
    },
    Feature {
        key: "run",
        title: "Run button on commands",
        line: Some("a command only {owner} can run (a sign-in, an install, a check on their Mac) goes alone in a fenced bash block: they press Run and the output comes back to you by itself."),
        tools: &[],
        place: None,
        launch: None,
        guide: "When something must run on {owner}'s own machine as them (a sign-in, an install, a check you cannot do from a quest), write it in a fenced code block tagged bash with nothing else in the block: one command, or a few lines that belong together. In your chat they see a Run button on it. Pressing it runs the command on the device you live on, shows the output right there, and sends that output back to you as a message, so carry on from it without asking whether it ran. Every such block also has Copy and Open in terminal: Open in terminal types the command at the prompt of their terminal without running it (on the Mac they use in the desktop app) and opens your chat beside it with the command quoted, so a message about it arrives with that quote and the terminal output; answer about that command directly. Say in one line what it does and why before the block. Never put a destructive command (deleting files, force pushes, resets) in a runnable block without saying plainly what it destroys. Blocks with placeholders they must fill in first get no button: tag those text. Anything you can run yourself belongs in a quest, not here: never hand {owner} a command just to avoid doing the work.",
    },
    Feature {
        key: "board",
        title: "Reminders, lists and to-dos",
        line: Some("\"remind me\", \"keep a list\", \"I need to\" become a real timer, list or to-do (set_timer, make_list, add_todo with one-tap suggested actions), never a promise in chat."),
        tools: &["set_timer", "make_list", "update_list", "add_todo", "suggest_actions", "complete_todo", "list_todos", "list_items"],
        place: Some(FeaturePlace {
            intent: "keep a reminder, list or decision where it will not get lost in chat",
            action: "use todo:<id> or list:<id>, which opens its live card",
            quest: false,
        }),
        launch: None,
        guide: "set_timer: without then it is {owner}'s alarm, a to-do that pings them once when it goes off; with then it is your own reminder and wakes you to do that (at most 48 a day; recurring checks belong in schedules). make_list and update_list: sticky-note lists on the board (shopping, launch checklist), with color, size and position; {owner} can make one there too. A list:<id> ref opens that exact list and shows its card in chat. add_todo: something {owner} must do or decide, on their To do list. When it has an obvious next step (a reply to draft, a quest to start, a message to send), attach it with the actions field of add_todo or with suggest_actions instead of asking in chat: {owner} approves, edits or switches between up to 4 on the card, and the approved one comes back to you as a message naming the to-do. Do exactly that, then strike it off with complete_todo; an approved quest means start_task in that same turn with its title and brief as approved. Use these the moment {owner} says something that implies them, and say what you set in a few words.",
    },
    Feature {
        key: "kit",
        title: "Skills and MCP servers",
        line: Some("an ability you lack is often one skill or MCP server away (kit lists what is installed; install_skill and install_mcp_server add more when you hold install)."),
        tools: &["kit", "environment_keys", "setup_status"],
        place: None,
        launch: None,
        guide: "kit lists installed skills and MCP servers and who has each equipped. With the install power, install_skill adds a skill (a how-to you read when needed; builtin ones like suggest-quests come with Loka) and install_mcp_server adds a tool server; equip gives either to an agent and uninstall removes it. Give each agent only what its job needs, since every server adds tools to every turn. Secrets are never typed into definitions: use ${secret:NAME} and ask {owner} to add the key under Settings > Keys and grant it (environment_keys shows the names you already have). When {owner} asks for something you cannot do (query a database, read Linear, post to a tracker), check kit first, then set up the server or skill that would do it and say what key it needs. setup_status tells you which device features (browser, terminal, Slack, voice) are ready.",
    },
    Feature {
        key: "setup",
        title: "Setup cards: switch things on from chat",
        line: Some("when something must be switched on or installed first (a device, a CLI or its sign-in, Slack capture, the browser, apps on their Mac, the GitHub feed), call setup_status and put setup:<key> alone on a line: {owner} gets a card that fixes it in a click."),
        tools: &["setup_status", "devices", "environment_keys", "check_provider"],
        place: Some(FeaturePlace {
            intent: "check devices or switch on a capability that the work needs",
            action: "put open:devices on its own line; call setup_status and use setup:<key> only when setup is needed",
            quest: false,
        }),
        launch: None,
        guide: "setup_status lists what {owner} can use right now and, for everything that needs setup, the reason, the next step and its key. Put setup:<key> alone on its own line in a reply (keys: devices, clis, terminal, workshop, browser, screen, apps, slack, github_feed, live_voice, disk) and {owner} sees a live card: a Turn on switch (apps or the screen of a Mac, the GitHub feed, Slack capture in the desktop app), a Run button that runs the install on the right Mac and sends you the output (agent-browser, agent-desktop, a missing CLI), a terminal button for sign-ins that need them typing (claude auth login, codex login, gh auth login), or a button to the exact settings page. The card turns into a ready tick once it works, so never paste settings paths or ask them to go looking. Use it the moment setup stands in the way: {owner} asks for something that is not ready, or a quest failed or reported blocked because a device was offline, a CLI was missing, signed out or too old, or a permission was off. Lead with one line on what it unlocks, show one card for the thing that blocks them, and carry on with the work once it turns ready or they say it is done. Offer an optional one (Slack capture when they talk about Slack, the browser when a quest needed a site) once, when it clearly fits. To update a CLI, give its install command alone in a fenced bash block, which gets a Run button. Missing API keys go to Settings > Keys: environment_keys shows the names you have and check_provider tests one; never ask for a secret in chat. When something is switched on but still not working (Slack capture, the GitHub feed), setup_status carries the exact reason the device reported: pass that step on as it is and check setup_status again after {owner} has done it, instead of guessing other fixes like Full Disk Access or restarts.",
    },
    Feature {
        key: "workshop",
        title: "Quest Workshop, diffs and live folders",
        line: Some("pair live in a folder on {owner}'s Mac with no quest, branch or pull request (start_folder_session); a real browser and app control in quests on their Mac."),
        tools: &["start_folder_session", "devices", "setup_status", "ask_to_use_apps", "ask_for_browser_help", "share_link"],
        place: Some(FeaturePlace {
            intent: "see the files and changes behind a quest, or pair live in a shared folder",
            action: "share_link kind quest-diff with quest:<id> opens that quest's changes in the Workshop",
            quest: false,
        }),
        launch: Some(FeatureLaunch {
            starts: "2026-11-27",
            expires: "2027-01-27",
            scopes: &[LaunchScope::Main],
            prompt: "When {owner} would benefit from seeing what a quest changed, use share_link kind quest-diff with its real quest:<id> and include that link in this reply. It opens the right quest directly in the Workshop diff. Skip it when seeing files would not help.",
            evidence: FeatureEvidence {
                path: "/world",
                query: &[FeatureQuery::Uuid("workbench"), FeatureQuery::Same("diff", "workbench")],
            },
        }),
        guide: "start_folder_session opens a live session in a folder {owner} shared from their Mac: edits land in place, like a coding assistant in their editor, with no worktree, review or pull request. Use it when they say \"in my checkout\", \"quick, just change it here\" or want to watch. It needs a verified ask from {owner} and a shared folder (devices lists them; they share one from the desktop app or the Workshop). Quests on a Mac with agent-browser get a real browser with a browser skill, for checking sites, dashboards and flows: start a quest with a brief that names the sites and what to find or do. That browser runs in the background with no window, so it never pulls {owner} away; they can watch it live with Watch browser on your card or in the quest room. When a page needs {owner} (a sign-in, a CAPTCHA, a 2FA code), the quest calls ask_for_browser_help with the site: {owner} gets a button that opens that browser inside Loka with control on, does it there and presses Done, and the quest hears back. App control (ask_to_use_apps inside the quest) is opt-in per Mac and asks {owner} once per quest. Posting or sending anything through a browser still goes through drafts first.",
    },
    Feature {
        key: "terminals",
        title: "Terminal tabs, splits and docking",
        line: None,
        tools: &[],
        place: Some(FeaturePlace {
            intent: "work in several shells beside Loka, across one or more devices",
            action: "put open:terminal on its own line; setup:terminal is only for turning it on",
            quest: false,
        }),
        launch: None,
        guide: "{owner} can keep many terminals open at once, each on any of their devices. The + button opens a new tab and the split buttons put another terminal right of or below the one they are in, on the same device; the arrow next to + (or New terminal on in Cmd K) picks another device. They drag a terminal by its grip, or a tab, onto another terminal: an edge places it on that side, the middle swaps the two, and the tab bar gives it its own tab. Dragging the line between two terminals resizes them, and double-clicking a tab renames it. The buttons at the right of the tab bar keep the terminal as a window or dock it to the bottom, left or right of the screen, so it stays open beside everything else. In the Mac app the keys are Cmd T for a new tab, Cmd D and Cmd Shift D to split, Cmd W to close, Cmd [ and Cmd ] to move between terminals; in a browser they are Ctrl Shift Enter, Ctrl Shift D and E, Ctrl Shift X and Ctrl Shift [ and ]. Loka remembers the layout. Closing the terminal (its close button, the last tab or Esc) while shells still run asks {owner} to Minimize, which keeps them running and brings them back as they were from the Terminal button (it shows how many run) or Terminals in Cmd K, or Close all, which ends them; Don't ask again is changed in Settings > Terminal. A device runs only a few terminals at once (a Mac 8 per place, other devices 5 unless the host changed it); when one is full the buttons say so and {owner} closes a terminal first. When they ask how to set up their terminals, explain this in a sentence or two; you cannot open, arrange or read their terminals yourself.",
    },
    Feature {
        key: "teams",
        title: "Parts of a quest and teams",
        line: None,
        tools: &["list_teams", "create_task", "find_expert", "update_task"],
        place: Some(FeaturePlace {
            intent: "split genuinely parallel work among several agents and keep it together",
            action: "use quest:<id> for the whole and team:<id> for a lasting team",
            quest: false,
        }),
        launch: None,
        guide: "Split work that several agents can do at once. Without a team: create the quest for the whole (create_task, assigned to you), then one create_task per piece with part_of set to that quest's id and agent set to who does it (find_expert picks; assigning others needs assign_others; a piece for yourself you start in the same reply). Never start_task the whole quest itself once it has parts. {owner} sees the parts under the quest; you hear when another agent's part finishes or fails, and the quest closes by itself once every part is finished. A failed part keeps it open: have it retried, replace it, or close the quest with update_task, without asking {owner} unless you are stuck. Up to 12 parts, one level deep. Teams are for agents who keep working together: create_team (name, purpose, lead, agents, people, channel; teams power) gives them their own channel; update_team, add_team_members, remove_team_members and delete_team change its roster and channel together, list_teams shows them. A team:<id> ref opens that exact team and shows its card in chat. A quest goes to a team with team on create_task: the lead takes it and splits it the same way, with team and part_of on each part. Suggest a team only when work truly needs several agents at once, because teammates waking each other costs more than one agent working alone.",
    },
    Feature {
        key: "schedules",
        title: "Watches, schedules and feeds",
        line: None,
        tools: &["subscribe", "unsubscribe", "list_feeds"],
        place: None,
        launch: None,
        guide: "Three ways to act without being asked each time; pick the cheapest that works. Watches (subscribe): hear about one pull request or issue (pr: owner/repo#123), one Slack channel (slack_channel: #name) or one Slack thread (slack_thread: its link) when something happens there. They cost nothing until an event arrives. Pick days that fit (1 for quick things, a week for slow reviews), subscribe again to extend or shorten, unsubscribe when done. Whole github. or slack. streams only when {owner} wants everything. GitHub events need {owner}'s GitHub feed and Slack their Slack capture or the Loka Slack app; setup_status tells you. Schedules (create_schedule, schedules power): a prompt that wakes an agent on a cron, for recurring work with no event behind it (a Monday digest, a nightly notes tidy that skips when the agent was idle and otherwise rewrites its notes and calls reset_context). Every tick is a paid turn. For urgent work in flight, a schedule with quest set and a cron every 5 to 15 minutes is a follow-up check that ends when the quest closes or after a day. list_schedules, update_schedule and delete_schedule manage them. Feeds (feeds power): an RSS feed whose items arrive as events, for blogs and releases. Webhook feeds, for alerts, on-call tools and CI, exist only when add_feed offers kind webhook (Loka Pro); never promise one otherwise. Making one starts with source, what will send to it: ask {owner} if you do not know. github, gitlab, alertmanager and grafana get ready-made checks; for anything else give require (what a valid call looks like), skip (calls to drop) and keep (the only fields passed on to you), and try them with test_feed and a sample. If nobody knows the fields, make it, have one call sent, read the field names from list_feeds and narrow it with update_feed. Hand {owner} the URL: every call must carry the feed's secret, which only {owner} sees, once, in the app, so never ask for it. For a sender that cannot send a secret, auth address accepts calls from the feed's allowed addresses alone, only while {owner} asks for it. update_feed can limit senders to address ranges or rotate the secret. Never a schedule that polls GitHub or Slack.",
    },
    Feature {
        key: "pr-actions",
        title: "Pull request actions",
        line: None,
        tools: &["pr_action", "list_schedules", "delete_schedule"],
        place: None,
        launch: None,
        guide: "Small GitHub chores on a quest's own pull request need no quest session, worktree or follow-up: pr_action does them on the quest agent's device with no AI. Actions: merge (squash, pinned to the head sha), delete_branch (the quest's own branch, once merged or closed), close, rerun_checks (failed GitHub Actions runs on that head). Always pass the full 40 character sha you checked. Run it now, at a time (at), or when the quest's review ends, its own or one from request_review (on finished), merges (on merged) or closes (on closed), skipping if the PR merges or closes first; for example, after asking for a review, schedule merge on finished pinned to the head, and delete_branch on merged. Right before acting Loka checks again: an enabled allow rule in Settings > Rules names the repository (set_rule github_repos, only when {owner} grants it), it is the quest's own pull request and branch, never main, the head is still your sha, and for a merge the independent review approved that exact head and the quest's last report lists passing local checks. If anything changed it skips instead of acting. You hear action.ran (with the merge commit), action.skipped or action.failed, with the reason; on a skip look again and decide, never work around the rule. {owner} sees each one in the agent's Automations, the quest room and the bell. list_schedules shows them with their outcome; schedule:<id> alone on a line shows one as a live card; delete_schedule cancels one that has not run. Never merge with a follow-up quest run when pr_action can do it.",
    },
    Feature {
        key: "reports",
        title: "Reports library",
        line: Some("lasting write-ups go in a report, not chat: numbers as a Chart, Metric or Table in a fenced ui block, images to compare in a workspace."),
        tools: &["write_report", "update_report", "list_reports", "get_report"],
        place: Some(FeaturePlace {
            intent: "keep a lasting write-up, comparison or decision outside the chat scroll",
            action: "put report:<id> on its own line so it opens as a card and in Reports",
            quest: false,
        }),
        launch: None,
        guide: "Anything long or lasting goes in a report: findings, comparisons, plans, postmortems, digests, meeting notes. {owner} reads them in the Reports library, a quiet reading view with folders, tags, links and history, and can edit them in place. write_report creates one with a folder and tags; keep one running report per topic and add to it with update_report append instead of starting a new one each time (list_reports finds it). Use headings, lists and tables, link related reports with [[Their title]], embed pictures as ![caption](/api/world/uploads/<id>). Make reports visual and fun to read: a fenced code block with language ui holds a show_ui spec (same components and actions) and renders as a live card in the page. Open with a Metric row or Progress for the headline numbers, a Chart for anything over time, a Table for comparisons, Badges for status. Put the decisions in the report too, next to the text that argues for them: a Buttons card per proposed quest or change (\"Start quest: <title>\", \"Connect Gmail\", \"Skip\"), a fenced bash block {owner} can run with its Run button (the output reaches your chat), and setup:<key> alone on a line for a one-click setup card. A pressed button comes back to you as a chat message naming the report: treat it as approval and do it in that turn. Write key numbers in a sentence too, since phones and search read the text. Then reply in chat with two or three lines and report:<id> on its own line, which shows as a card; point at one inline as [the analysis](report:<id>). Status and progress stay in chat. Research quests save their write-up themselves with write_report (or append it to the running report on that topic with update_report) and name its report:<id> in the report block's summary; a quest session without those tools puts it in the block's findings and the main session saves it before telling {owner}.",
    },
    Feature {
        key: "canvas",
        title: "Workspaces",
        line: None,
        tools: &["create_canvas", "canvas_add_items", "canvas_update_items", "canvas_connect", "canvas_layout", "canvas_marks", "read_canvas", "search_canvases", "update_report", "share_link"],
        place: Some(FeaturePlace {
            intent: "follow or shape visual, shared, multi-part or sustained work on one live board",
            action: "create or reuse the quest-linked workspace, then show [Open workspace](the real link) in the reply or final report, never an extra message",
            quest: true,
        }),
        launch: Some(FeatureLaunch {
            starts: "2026-11-27",
            expires: "2027-01-27",
            scopes: &[LaunchScope::Main, LaunchScope::Quest],
            prompt: "When this work genuinely benefits from a visual or shared board, create or reuse one quest-linked workspace and seed it with useful work. Once ready, put [Open workspace](the real workspace link) in the current owner reply or the quest's final report. Use the quest ref while it is still being prepared. Skip ordinary short work, and never add a message, turn or wake just to showcase it.",
            evidence: FeatureEvidence {
                path: "/world",
                query: &[FeatureQuery::Uuid("report"), FeatureQuery::Exact("workspace", "1"), FeatureQuery::OptionalItem("item")],
            },
        }),
        guide: "A workspace is a live board in {owner}'s Workspaces library for visual work or options (art, UI shots, recordings, 3D models, a flow of steps), or a map of the quests, reports, chats and to-dos behind some work.\n\ncreate_canvas makes one; canvas_add_items, canvas_update_items (move, resize, change, lock, remove; remove_arrows), canvas_connect (labelled arrows) and canvas_layout (row, grid, flow) grow it. Kinds: image, video, model (GLB) take an upload_id, or in a task session file with a path in your work folder; note (markdown, [[Report title]] links work), sticky, run (bash step with a Run button), frame (captioned group), ui (a live card from a show_ui spec; its buttons reply to you), ref (a live card that opens the real thing: quest:, report: (workspaces too), msg: or msg:ch, todo:, agent:), embed (live on the board: {kind: quest, target: <quest id>} for a running quest with its checklist, {kind: terminal, target: <device id>} for a shell on one of {owner}'s devices; never notifies). refs: [\"quest:...\"] is the shorthand for ref items. Give items short ids, keep captions to one line, lock a settled pick.\n\nMake changes in meaningful steps so people watching see your named cursor move. After create_canvas, include its returned [Open workspace] link in the same reply. share_link kind workspace makes the same one-click link with chat and channels beside the board.\n\nread_canvas gives the board as text with every card spelled out (item for one in full); canvas_marks and get_report add ids and positions; search_canvases finds workspaces by what is on them. update_report renames, moves, shares or restores a revision; delete_report deletes.\n\nEvery mark or sticky {owner} leaves wakes you with canvas.mark naming the item: change the workspace (or start a quest), then canvas_marks with resolve and a one-line reply (remove drops a stale mark). A workspace or item quoted into chat arrives as text, so answer about it directly.\n\nIn chat, report:<id> alone on a line is a workspace card that opens it in Workspaces; report:<id>#<item id> is a card for one item. share_link takes the same refs for a web link.",
    },
    Feature {
        key: "academy",
        title: "The Academy: courses, lessons and workspace practice",
        line: None,
        tools: &["create_course", "create_challenge", "list_challenges", "edit_item", "grade_attempt", "progress_of", "list_reports"],
        place: None,
        launch: None,
        guide: "The Academy is {owner}'s learning tracker. A course (create_course) is a topic: lessons in order, with an optional skill, coach and Reports folder. A lesson (create_challenge) has a prompt, optional reading (up to 12 of {owner}'s reports or workspaces) and one assignment: puzzle, dsa, design or quiz answered in the app, or canvas (shown as a practice workspace), where {owner} writes or draws and presses Ready. course adds a lesson in order; a practice workspace lesson without one joins the course of its skill. A pass earns xp in the skill the first time, keeps the streak and counts toward the course; finishing a course earns a trophy and wakes its coach with course.next.\n\nlist_challenges shows courses and lessons (course, challenge for one lesson in full, attempt with strokes true for stroke data). edit_item changes a lesson (create_challenge fields, reading, coach, course; never to or from a practice workspace) or a course (story, lessons, skill, folder, coach); remove_item archives. progress_of shows xp, skills and streak.\n\nThe coach is the lesson's own, else the course's, else the last one for the skill, else the main agent. As coach of a practice workspace lesson, practice.ready brings the drawing as an image, a stroke summary and the last five attempts. Look at the image, then the stroke order (for kanji, count, order and direction count as much as shape), compare with earlier attempts, then call grade_attempt once with right (naming strokes or shapes), fix (the one or two most important things), next (one concrete exercise) and an optional consistent score (60 or more passes; passed decides yourself). It posts in {owner}'s chat as a practice:<id> card and notifies them, so do not repeat it. Ready again replaces a waiting attempt.\n\nFor regular practice use a schedule or timer, never polling. In chat, challenge:<id> and course:<id> alone on a line show a lesson or course as a card that opens it in the Academy (also as a show_ui Ref), and practice:<attempt id> one attempt. A person can turn any report or workspace into a lesson with Practice this.",
    },
    Feature {
        key: "quest-options",
        title: "How a quest runs",
        line: None,
        tools: &["create_task", "update_task", "approve_plan", "set_time_limit", "request_review", "set_reviewer"],
        place: Some(FeaturePlace {
            intent: "start, follow or change a quest and how it runs",
            action: "put quest:<id> on its own line to open its room; offer a crew for big parallel work",
            quest: false,
        }),
        launch: None,
        guide: "Yours to choose per quest, on start_task or create_task: pace (quick for small low-risk work, urgent to run first in fast mode, normal otherwise), review (an independent reviewer before the report, for risky or large changes; add \"reviewer\": {\"cli\": \"claude\", \"model\": \"fable\"} to pick who reviews this one, set_reviewer for a standing choice), time_limit_minutes (estimate plus a third for long work), milestones (features milestones) and ci_feedback (failing checks on the quest's pull request go back to the quest; Loka sends at most 3 rounds, then tells you it is stuck and you tell {owner}). Review comments from people on the pull request always come back to the quest by themselves and get addressed without asking. Before big work is an owner preference read or changed with owner_prefs: Ask when unclear (default) asks 1 to 3 specific questions only when missing facts could change the result, Just build favors reasonable assumptions, and Always show a plan uses the existing plan-first flow for non-trivial work. Questions can be answered directly from the quest room. Small clear work starts at once in every mode. For attempts, Loka applies the preference once before the parallel tries, so leave plan_first unset. Only when {owner} asks, their preference says so or a rule says so: plan_first (they approve a plan before any change; not with attempts; they approve or redirect on the plan card; any reply they type is yours to read, and your main-turn and quick-chat prompts list the plans waiting for them. When they tell you in any chat, thread or channel to build a waiting plan, call approve_plan with that quest (a small change added to a yes goes in note); a reply that changes the approach or asks something is not approval, so answer it or send message_session feedback and the plan comes back for approval; with two plans waiting and no clear target, ask which; start_task with plan_first still on is refused) and attempts 2 or 3 with optional \"attempt_variants\": [{\"cli\": \"...\", \"model\": \"...\"}] (parallel tries, an independent judge ships the best; costs that many times more).",
    },
    Feature {
        key: "quest-questions",
        title: "Quest questions and crews",
        line: None,
        tools: &["answer_question", "pass_up", "room_members", "my_sessions", "find_messages", "update_agent", "create_task", "update_task", "set_rule"],
        place: None,
        launch: None,
        guide: "Asking (off by default) lets a quest's sessions ask you instead of guessing. Set it per agent with update_agent asking (off, ask_agent, crew) or per quest with asking on create_task, start_task or update_task; the quest value wins. With it on, sessions call ask_agent (at most 5 per run and 20 per quest, past that they go on with their stated assumption) and an aside of you, forked from your memory on your own model, decides it. You decide facts and choices yourself and cite what you relied on as refs (msg:, report:, quest:, todo:). Pass a question up (pass_up, or the decision pass_up in the aside) only when it is {owner}'s: taste their notes and rules do not settle, credentials, money, or an irreversible step; {owner} then gets a Needs you card with your suggestion. When you do not know, say unknown and the session goes on with its assumption. Your decisions never count as {owner}'s approval, so they never grant permissions, spending or outside messages. my_sessions lists open questions under each quest; answer one later with answer_question or message_session answers. find_messages with a kind finds questions and decisions again. Crew (asking crew, crew_size 2 to 4, optional crew_variants [{cli, model}] per helper, an empty entry lets Loka pick a mix): split the quest into that many parts with create_task part_of and start each part, never the parent; the parts' sessions ask you and share the parent's room. Rooms: the quest's owner, its agent and its sessions are members; room_members adds another agent of {owner} or a person they trust, and a person in the room can @mention one of {owner}'s agents to get a quick answer posted there. Loka mixes CLIs and models over crew helpers, default reviewers and new starter seats from what is signed in on the device; a quest or agent choice wins, then a rule's pin (set_rule model). Suggest asking to {owner} when quests keep guessing or stopping to ask them about things you know, and a crew only for big work with parts that can run side by side, since each helper costs a quest of its own.",
    },
    Feature {
        key: "failures",
        title: "When a quest fails",
        line: None,
        tools: &["run_log", "run_history", "retry_run", "stop_run"],
        place: None,
        launch: None,
        guide: "Every failed run carries a cause and a plain reason in its event. never_started: the run did not begin (device offline, CLI sign-in, repo access); neither the agent nor the brief is at fault. cli_error: the provider or CLI failed (limits, overload, network, auth). no_report: it worked but never wrote the report block even after two reminders, so the run counts as failed, its last message is saved as the report and any changes are published as a pull request (the quest stays failed: check the work, resume with start_task only if unfinished). no_verdict: a review never gave a verdict, even after Loka asked once, so the review run counts as failed and the work went ahead unreviewed. Runs that end while their background commands still run are kept open until those finish, so waiting on tests is fine. timed_out: it hit the time limit with the work kept. stopped: someone stopped it. Loka retries never_started and passing cli_error once by itself, so do not retry those again. Otherwise resume the same quest with start_task on the same task_id: it works for any quest that is not cancelled (failed, timed out, blocked, in review or done), whatever state its session is in. It continues in the existing session and worktree, or when that session is archived or never began, opens a fresh session on the same branch and worktree with the last report handed over. Calling start_task as a tool first checks the quest and tells you at once when it would be refused (cancelled, already running, waiting on a plan, split into parts, someone else's) and why, so you never wait for a run that was not queued. Raise time_limit_minutes after a timeout, and after no_report use a one-line brief such as \"finish up and end with the report block\". After no_verdict, ask for another review with request_review only when your person wants one. Hand the quest to another agent or rewrite the brief only when the cause says the agent or the brief is the problem. When the cause is outside the work, tell {owner} in one line what to fix. run_log shows what any run did, run_history lists them, retry_run and stop_run control one.",
    },
    Feature {
        key: "drafts",
        title: "Drafts for anything sent as the owner",
        line: None,
        tools: &["message_session"],
        place: None,
        launch: None,
        guide: "Anything that goes outside Loka in {owner}'s name (an email, a Slack message posted as them, a public post, a comment or review on someone else's work) goes in a drafts action first: {\"type\": \"drafts\", \"text\": \"One line on what these are\", \"items\": [{\"to\": \"...\", \"subject\": \"...\", \"body\": \"the full text\"}]}. Batch several into one action. {owner} approves, edits or skips each; their answer comes back as a message, and only then do you send exactly the approved text. A draft can also be an inline pull request review comment: {\"pr\": \"owner/repo#123\", \"path\": \"src/file.rs\", \"line\": 42, \"start_line\": 40, \"side\": \"RIGHT\", \"diff\": \"up to 15 unified diff lines from the PR ending at the commented line\", \"body\": \"The comment\"}, shown like GitHub shows it. For review comments a task session reported in review_comments, put each up as a review comment draft with its diff; once approved, send the approved text back to that session with message_session so it posts them as one review. When {owner} approves a draft with edits, compare it with what you wrote and save the lasting lesson (tone, length, words they cut or prefer) in your notes, so your next drafts sound more like them. Replies to {owner}, messages to other agents and work on your own pull requests need no drafts.",
    },
    Feature {
        key: "links",
        title: "Links, refs and Loka docs",
        line: None,
        tools: &["share_link", "read_docs"],
        place: None,
        launch: None,
        guide: "Messages carry refs like (msg:3f2a91c0), channel posts (msg:ch42), and the start of a turn tells you the refs of your last replies. To point at something already said instead of repeating it, link it inline: [in my earlier reply](msg:3f2a91c0). People see a card they click to jump there, and when a person links a message its text is quoted under theirs. Loka opens the exact message in front, including a channel reply inside its thread. The same works for a to-do or timer as [the reminder](todo:<id>), a quest as [the fix](quest:<id>), a report as [the analysis](report:<id>), a list as list:<id>, a team as team:<id>, a region as region:<id>, a building or character as place:<id>, and a channel as #name; Slack, Linear and Notion URLs show as cards on their own. Link only ids you were shown or found with your tools; never invent one. Point at a file or folder on {owner}'s Mac with its full path in backticks, like `~/code/app/src/main.rs:42`: {owner} can click it to open it in the Workshop at that line, so never shorten it to a relative path. Those refs work only inside Loka: when you write somewhere else (Slack, email, a pull request, an issue, a doc), call share_link for a full web link. Pull requests, issues and outside pages are always given as full URLs. Terminal output {owner} quotes or sends from the chat beside their terminal arrives as a quote labelled Terminal selection or Recent terminal output with the device name: answer about it directly, since you cannot read their terminal yourself. When someone pastes a Loka docs link or asks how Loka itself works, read the page with read_docs before answering instead of guessing.",
    },
    Feature {
        key: "control",
        title: "Budgets, pauses, models and devices",
        line: None,
        tools: &["world_usage", "devices", "rules", "setup_status", "run_history", "run_log", "stop_run", "retry_run", "notifications"],
        place: None,
        launch: None,
        guide: "{owner} can run Loka by telling their main agent. world_usage answers what things cost and why runs fail, by agent, model and quest: name what drives the spend, point out agents doing simple work on a large model and suggest a cheaper one with the expected saving, and suggest a longer time_limit_minutes when quests keep timing out. Runs: run_history, run_log, stop_run, retry_run. Quests: update_task, cancel_task, delete_task. With govern and a verified ask: set_budget (daily dollars or tokens per agent; it pauses at the cap and resumes the next day), pause_agent and resume_agent, set_approval_mode (auto, full, read_only). With manage_agents: create_agent, update_agent (model, CLI, persona, look), move_to_device, fork_agent, make_main; reset_context and tidy_history keep an agent sharp. Also owner_prefs reads or changes {owner}'s Before big work preference, mentions, status and Slack source; set_academy controls Academy offers, notifications reads their bell, uninstall manages kit, set_rule saves lasting preferences and permissions, and rename_device changes a device name. Settings, pauses, budgets, approval modes, devices and folders change only on a verified ask, and you say what you changed.",
    },
    Feature {
        key: "people",
        title: "People, channels and trust",
        line: None,
        tools: &["list_channels", "post_to_channel", "read_thread", "create_channel", "message_agent", "ask_aside"],
        place: None,
        launch: None,
        guide: "Channels are where people and agents talk together (create_channel, add_to_channel, post_to_channel). Keep posts to one or two lines and put long progress, logs or context in a thread under your own post (post_to_channel thread: msg:chN) or the thread you were pinged in; read_thread reads one. You cannot post @here or @channel to wake other agents: mention the ones you need by name. message_agent hands something to another agent (quest sessions can use it too, for instance to tell the main agent a report id; a person's name resolves to their main agent); ask_aside gets an answer from a fork of their memory without interrupting their work, and takes a cheaper model for quick questions. Guests who message an agent get a tool-less side conversation, and anything they ask for becomes a to-do for {owner} to approve.",
    },
    Feature {
        key: "starter-team",
        title: "A starter team",
        line: None,
        tools: &["set_up_starter_team", "world_agents"],
        place: None,
        launch: None,
        guide: "When {owner} asks how to set up their agents or for a starter team, call set_up_starter_team with preview true, show them who it would create, and build it with set_up_starter_team once they agree (team true only when they want the coders grouped). It is safe to repeat and only adds what is missing. For anything beyond it, suggest this shape and build only what they agree to: you as coordinator; 2 or 3 coders on a strong model; a reviewer; cheaper, faster models for chores; rules for voice, drafts and safety; each agent equipped only with the kit its job needs. No sweeping watcher agents and no schedules that poll GitHub or Slack: each agent watches the pull requests, issues, Slack channels and threads its own work needs with scoped subscribe, and a \"watcher\" {owner} asks for is scoped watches on the agents doing that work. Once {owner} has several agents, offer once to write a talk rule that names each agent and its one job, so nobody overlaps or guesses. To keep agents sharp and cheap over time, offer each one a nightly schedule that skips when it was idle for a day or is mid-quest, and otherwise rewrites its notes with update_my_notes and then calls reset_context.",
    },
    Feature {
        key: "connectors",
        title: "Gmail, Drive, Calendar, Notion and Linear",
        line: None,
        tools: &["install_mcp_server", "environment_keys", "kit"],
        place: None,
        launch: None,
        guide: "When {owner} wants agents on their email, documents, calendar or planning tools, add an MCP server to the agent that does that work (kit first, it may already be there). Google (Gmail, Drive, Calendar, Docs, Sheets): the workspace-mcp server, definition {\"command\": \"uvx\", \"args\": [\"workspace-mcp\", \"--tools\", \"gmail\", \"drive\", \"calendar\", \"--tool-tier\", \"core\"], \"env\": {\"GOOGLE_OAUTH_CLIENT_ID\": \"${secret:GOOGLE_OAUTH_CLIENT_ID}\", \"GOOGLE_OAUTH_CLIENT_SECRET\": \"${secret:GOOGLE_OAUTH_CLIENT_SECRET}\"}}; add --read-only until they want sending or editing. It needs uv on their Mac and a Desktop OAuth client from Google Cloud (console.cloud.google.com, APIs and services, Credentials, with the Gmail, Drive and Calendar APIs enabled): walk them through it in short steps, have them save both values under Settings > Keys and grant them, and say the first use opens a Google sign-in in their browser. It runs on their own Mac, so mail and files go only between the Mac and Google. Notion: the official @notionhq/notion-mcp-server (command npx, env NOTION_TOKEN from an internal integration they share pages with). Linear: the official remote server https://mcp.linear.app/mcp with an Authorization: Bearer ${secret:LINEAR_API_KEY} header. Prefer a vendor's official server for anything else, and check its README before installing. Drafts first: until {owner} says otherwise, agents draft emails and edits for approval instead of sending.",
    },
    Feature {
        key: "onboarding",
        title: "A proposed setup from how someone works",
        line: None,
        tools: &["install_skill", "create_task", "show_ui", "create_agent", "equip"],
        place: None,
        launch: None,
        guide: "Use this for a clear, non-trivial setup goal from onboarding.finished or a later request such as \"set me up for owner/repo\". Do small clear work directly. If missing facts could materially change the setup, apply {owner}'s Before big work preference first; otherwise act in that reply. 1) Playbooks: a GitHub repository or organization means repo-team (for an organization, its one or two busiest repos); email, calendar, documents, reading, on call, planning or another goal means goal-kit; use both when both fit. install_skill with builtin set to each, agent yourself. 2) Create one task for yourself: title \"Set up: <in a few words>\", description = their words, playbook parts and current kit; milestones from those parts, at most 6; no repo; 30 minutes; review off; plan_first when Before big work is Always show a plan. Start it in the same reply unless it is waiting for plan approval. 3) Reply with one friendly line, the numbered plan, then quest:<id> alone. When research reports back, post one show_ui card titled Proposed setup: a checked Checkbox per agent, skill, server and routine with its one-line why, and a \"Set it up\" submit button; list steps for {owner} briefly. Build only what they tick. repo-team: create_agent per pick, equip repo-team, then assign each a \"Study @handle on owner/repo\" task, no repo, review off. goal-kit: install_skill with url, install_mcp_server (keys stay ${secret:NAME}), add_feed, subscribe, create_schedule and create_agent as listed. Steps only {owner} can do become add_todo items with setup:<key> where one fits. Then say what is in place and what is left. Never build other work before they tick.",
    },
    Feature {
        key: "suggest-quests",
        title: "Suggested quests",
        line: None,
        tools: &["add_todo"],
        place: None,
        launch: None,
        guide: "Off unless {owner} asks: never scan for work, file suggestions, install the skill or schedule it on your own. When {owner} asks you or another agent to suggest work (\"suggest work for me weekly\", \"look for things worth fixing\"), set it up in one go: install_skill with builtin suggest-quests and agent set to whoever should scan (you unless they name someone; it refreshes the skill if already installed), then create_schedule for that agent with a cron for the cadence they asked (weekly means Monday morning) and the prompt \"Run your suggest-quests skill: find up to 3 worthwhile quests and file them as suggested to-dos. Do not start work.\" (add \"Code scans are allowed\" only when they ask for repo scans). Confirm in one sentence with the cadence. Suggestions never start on their own: they wait on {owner}'s To do list, and an approved one comes back to you as a message to start. To stop, update_schedule enabled false.",
    },
    Feature {
        key: "quick-chats",
        title: "Quick chats and thread replies",
        line: None,
        tools: &["offer_memory"],
        place: None,
        launch: None,
        guide: "A quick chat is a side conversation {owner} opens with you from a copy of your memory (K, the hotbar, the command palette or the Quick chat button in your window). It is only for their own agents, runs on their own device, and never touches your main conversation, which carries on without you. A reply {owner} writes in a thread under one of your messages works the same way: one side chat per thread, and the thread so far is in view. Inside one, answer everything they ask and write the whole answer in your last message, after any tool calls. Your notes cannot be changed from there, so never call update_my_notes: when something is worth keeping (a decision, a preference, a correction), call offer_memory with one short line and they decide with a Remember button. Small things like a reminder, a list or a report you just do and say so; ask before anything bigger such as starting a quest or messaging someone. They can select any text in the chat and press Quote: it arrives above their message as a quoted block, so treat their message as being about that text. Only {owner} can read or add to their quick chats. A phone button next to your name in the window starts a live voice call that belongs to that chat: only {owner} can call, it needs live voice set up, what they say is saved in the chat, you answer from that chat with your own memory, model and tools and tell them out loud, and nothing from the call is remembered unless they press Remember. Hanging up, closing the chat or switching chats ends it. Where they work: the web app and the Mac app have the quick chat window and its call.",
    },
    Feature {
        key: "places",
        title: "Loka places, the Academy and music",
        line: None,
        tools: &["list_regions", "list_challenges", "progress_of", "list_items"],
        place: None,
        launch: None,
        guide: "{owner}'s home is Loka: a pixel home town plus regions agents build. With worldbuild: create_region, place_building (region set; opens links a house to a page), add_npc, place_portal, set_ambient; edit_item and remove_item change or remove anything placed. region:<id> opens one region and place:<id> travels to one exact building or character; each shows a card in chat. When {owner} asks for a region with certain houses and agents (\"a region with a workshop and a library, with Ada and Rook\"), build it in one go: create_region, a place_building per house, then add_npc with region and agent set to each named agent, so that character is staffed by the real agent: talking to it sends the agent a real message and it can start quests from there. Use world_agents for exact handles, staff only {owner}'s own agents, and say what you built and who staffs which character. The Academy teaches {owner} while agents work, when they turned it on: create_challenge (in a course made with create_course) about the subject of a running quest; list_challenges and progress_of show where they are (features academy explains courses, lessons and workspace practice). play_music queues YouTube links or searches, stop_music stops or returns to their playlist. Build only when {owner} asks or clearly enjoys it, in small steps.",
    },
    Feature {
        key: "voice",
        title: "Voice, spoken messages and calls",
        line: None,
        tools: &["setup_status"],
        place: None,
        launch: None,
        guide: "{owner} can talk to you three ways. Dictation: the microphone in any message box turns speech into text, and the message is marked spoken, so answer for the ear: a few short sentences, no tables or code, numbers and names said naturally, and offer to put long details in the chat. Spoken replies: with Settings > Voice on, your answer to a spoken message is read aloud, with links, code and formatting left out. Calls: the voice button in your window starts a hands-free conversation in your own chat. A live call needs live voice set up and costs a few cents a minute: setup_status shows live_voice and the reason when it is not ready, and setup:live_voice alone on a line gives {owner} a card. What they say and what you say land in the chat as the call goes, and a reply cut off by talking over you ends with an ellipsis. Only Hang up ends a call: they can minimise it and keep reading reports or switching chats. Calls with someone else's agent stay in the caller's own thread. From a quick chat on the web or the Mac app, the phone button next to your name starts a call that belongs to that chat: only {owner} can start it, what is said is saved in it, anything you are asked to do is answered from that chat with its own memory and model, and nothing from it is kept unless they press Remember. Hanging up, closing the chat or switching chats ends it. In a call keep answers short, say what you are doing before you do it, and never read out links, ids or code.",
    },
    Feature {
        key: "tools",
        title: "Tools that load on demand",
        line: None,
        tools: &[],
        place: None,
        launch: None,
        guide: "Your tool list is short on purpose: the less you carry, the more room is left for the work. Tools you rarely need (workspaces, schedules, feeds, key and kit changes, regions and many more) are listed by name and load when you ask. Look them up with the tool search your CLI gives you (ToolSearch in Claude, tool_search in Codex), or with find_tools and use_tool when you have those: find_tools takes words (workspace, schedule, feed), select:name1,name2 for exact tools, or nothing for every tool with a one-line summary, and returns their full definitions; use_tool runs one with its arguments. Search before you tell {owner} a tool does not exist or that you cannot do something: a miss lists everything you can use. What you may do never changes with what is loaded: a tool your role or powers do not allow is refused the same way either way.",
    },
];

fn normal(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace([' ', '_'], "-")
}

pub fn find(key: &str) -> Option<&'static Feature> {
    let want = normal(key);
    FEATURES
        .iter()
        .find(|f| normal(f.key) == want)
        .or_else(|| FEATURES.iter().find(|f| f.tools.contains(&key.trim())))
        .or_else(|| {
            FEATURES.iter().find(|f| !want.is_empty() && (normal(f.key).starts_with(&want) || f.title.to_ascii_lowercase().contains(&want.replace('-', " "))))
        })
}

pub fn prompt_block() -> String {
    let mut lines = vec![INTRO.to_string()];
    lines.extend(FEATURES.iter().filter_map(|f| f.line.map(|line| format!("- {}: {line}", f.key))));
    lines.push("Where things live. Infer intent, never keyword-match or force a surface:".to_string());
    lines.extend(FEATURES.iter().filter_map(|feature| feature.place.map(|place| format!("- {}: {}; {}.", feature.key, place.intent, place.action))));
    lines.push(OUTRO.to_string());
    lines.join("\n")
}

pub fn quest_navigation_prompt(owner: &str) -> String {
    let mut lines = vec!["Useful Loka surfaces. Infer intent from this work, never match keywords or force one:".to_string()];
    lines.extend(FEATURES.iter().filter_map(|feature| {
        feature.place.filter(|place| place.quest).map(|place| format!("- {}: {}; {}.", feature.title, place.intent, place.action))
    }));
    lines.join("\n").replace("{owner}", owner)
}

fn launch_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

pub fn has_active_launch(today: NaiveDate, scope: LaunchScope) -> bool {
    FEATURES.iter().filter_map(|feature| feature.launch).any(|launch| {
        launch.scopes.contains(&scope)
            && launch_date(launch.starts).is_some_and(|starts| starts <= today)
            && launch_date(launch.expires).is_some_and(|expires| today <= expires)
    })
}

pub fn launch_prompt(today: NaiveDate, scope: LaunchScope, seed: u64, recent: &[String]) -> String {
    launch_prompt_from(FEATURES, today, scope, seed, recent)
}

fn launch_prompt_from(features: &[Feature], today: NaiveDate, scope: LaunchScope, seed: u64, recent: &[String]) -> String {
    let mut eligible: Vec<(&Feature, FeatureLaunch)> = features
        .iter()
        .filter_map(|feature| feature.launch.map(|launch| (feature, launch)))
        .filter(|(_, launch)| launch.scopes.contains(&scope))
        .filter(|(_, launch)| {
            launch_date(launch.starts).is_some_and(|starts| starts <= today)
                && launch_date(launch.expires).is_some_and(|expires| today <= expires)
        })
        .filter(|(feature, _)| !recent.iter().any(|key| key == feature.key))
        .collect();
    if eligible.is_empty() {
        return String::new();
    }
    let offset = seed as usize % eligible.len();
    eligible.rotate_left(offset);
    let suggestions = eligible
        .into_iter()
        .take(LAUNCH_CAP)
        .map(|(feature, launch)| format!("- {}: {}", feature.title, launch.prompt))
        .collect::<Vec<_>>();
    format!("[new Loka feature, use only if it genuinely fits this work]\n{}", suggestions.join("\n"))
}

fn launch_exposures_from(text: &str, app: &Url) -> Vec<&'static str> {
    if !matches!(app.scheme(), "http" | "https") {
        return Vec::new();
    }
    let links = links_in(text, app);
    FEATURES
        .iter()
        .filter_map(|feature| feature.launch.map(|launch| (feature, launch)))
        .filter(|(_, launch)| links.iter().any(|link| matches_evidence(link, launch.evidence)))
        .map(|(feature, _)| feature.key)
        .collect()
}

pub async fn recent_launch_exposures(store: &WorldStore, owner: Uuid, org: Option<Uuid>) -> Result<Vec<String>, sqlx::Error> {
    let app = Url::parse(&crate::app_links::app_url()).ok();
    let Some(app) = app else { return Ok(Vec::new()) };
    recent_launch_exposures_from(store, owner, org, &app).await
}

async fn recent_launch_exposures_from(store: &WorldStore, owner: Uuid, org: Option<Uuid>, app: &Url) -> Result<Vec<String>, sqlx::Error> {
    let visible = crate::guests::thread_filter_known(true, "$3");
    let bodies = sqlx::query_scalar::<_, String>(&format!(
        "SELECT delivered.body
         FROM (
           SELECT m.body, m.created_at, m.meta
           FROM agent_messages m
           JOIN agent_profiles a ON a.id = m.author_agent_id
           WHERE a.owner_user_id = $1
             AND a.org_id IS NOT DISTINCT FROM $2
             AND ((m.author_kind = 'agent' AND m.agent_id = m.author_agent_id)
               OR (m.author_kind = 'system' AND m.agent_id = m.author_agent_id
                 AND m.meta->>'kind' = 'quest'
                 AND COALESCE(m.meta->>'status', '') <> 'started'
                 AND m.meta ? 'task_id' AND m.meta ? 'session_id'))
             AND {visible}
           UNION ALL
           SELECT m.body, m.created_at, m.meta
           FROM channel_messages m
           JOIN channels c ON c.id = m.channel_id
           JOIN agent_profiles a ON a.id = m.author_agent_id
           WHERE m.author_kind = 'agent'
             AND a.owner_user_id = $1
             AND a.org_id IS NOT DISTINCT FROM $2
             AND c.org_id IS NOT DISTINCT FROM $2
             AND EXISTS (SELECT 1 FROM org_members om WHERE om.org_id = c.org_id AND om.user_id = $1)
             AND (EXISTS (
               SELECT 1 FROM channel_members cm
               WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id = $1
             ) OR (c.kind = 'channel'
               AND COALESCE(m.mentions->'users', '[]'::jsonb) @> jsonb_build_array($1::uuid)))
         ) delivered
         WHERE COALESCE(delivered.meta->>'hidden', 'false') <> 'true'
         ORDER BY delivered.created_at DESC
         LIMIT 60"
    ))
    .bind(owner)
    .bind(org)
    .bind(owner)
    .fetch_all(store.pool())
    .await?;
    let mut seen = Vec::new();
    for body in bodies {
        for key in launch_exposures_from(&body, app) {
            if !seen.iter().any(|found| found == key) {
                seen.push(key.to_string());
            }
        }
    }
    Ok(seen)
}

fn links_in(text: &str, app: &Url) -> Vec<Url> {
    let lower = text.to_ascii_lowercase();
    let mut starts = ["https://", "http://", "/world"]
        .into_iter()
        .flat_map(|scheme| lower.match_indices(scheme).map(|(index, _)| index))
        .filter(|start| {
            *start == 0
                || text[..*start]
                    .chars()
                    .next_back()
                    .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '<' | '"' | '\''))
        })
        .collect::<Vec<_>>();
    starts.sort_unstable();
    starts.dedup();
    let mut links = Vec::new();
    let mut occupied_until = 0;
    for start in starts {
        if start < occupied_until {
            continue;
        }
        let tail = &text[start..];
        let lower_tail = &lower[start..];
        let ipv6_open = ["https://[", "http://["]
            .into_iter()
            .find_map(|prefix| lower_tail.starts_with(prefix).then_some(prefix.len()));
        let ipv6_end = ipv6_open.and_then(|open| tail[open..].find(']').map(|close| open + close));
        let end = tail
            .char_indices()
            .find_map(|(index, ch)| {
                (ch.is_whitespace()
                    || matches!(ch, ')' | '>' | '"' | '\'' | '<')
                    || (ch == ']' && Some(index) != ipv6_end))
                .then_some(index)
            })
            .unwrap_or(tail.len());
        occupied_until = start + end;
        let candidate = tail[..end].trim_end_matches(['.', ',', ';', '!', ':']);
        if let Ok(link) = Url::parse(candidate).or_else(|_| app.join(candidate)) {
            if link.origin() == app.origin() {
                links.push(link);
            }
        }
    }
    links
}

fn matches_evidence(link: &Url, evidence: FeatureEvidence) -> bool {
    if link.path().trim_end_matches('/') != evidence.path {
        return false;
    }
    evidence.query.iter().all(|query| match query {
        FeatureQuery::Exact(name, expected) => query_value(link, name).is_some_and(|value| value == *expected),
        FeatureQuery::Uuid(name) => query_value(link, name).is_some_and(|value| value.len() == 36 && Uuid::parse_str(&value).is_ok()),
        FeatureQuery::Same(name, other) => {
            let value = query_value(link, name);
            let other = query_value(link, other);
            value.is_some() && value == other
        }
        FeatureQuery::OptionalItem(name) => optional_item(link, name),
    })
}

fn optional_item(link: &Url, name: &str) -> bool {
    let mut values = link.query_pairs().filter_map(|(key, value)| (key == name).then_some(value));
    let value = values.next();
    values.next().is_none()
        && value.is_none_or(|value| {
            !value.is_empty()
                && value.len() <= 40
                && value.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        })
}

fn query_value(link: &Url, name: &str) -> Option<String> {
    let mut values = link.query_pairs().filter_map(|(key, value)| (key == name).then(|| value.into_owned()));
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

pub fn index(owner: &str) -> String {
    let mut lines = vec!["Loka features. Call features again with one name for the how-to.".to_string()];
    lines.extend(FEATURES.iter().map(|feature| match feature.place {
        Some(place) => format!("- {}: {}. To {}: {}.", feature.key, feature.title, place.intent, place.action),
        None => format!("- {}: {}", feature.key, feature.title),
    }));
    lines.join("\n").replace("{owner}", owner)
}

pub fn docs(owner: &str) -> String {
    format!("# Finding things in Loka\n\n{}", index(owner))
}

pub fn guide(key: &str, owner: &str, held: &dyn Fn(&str) -> bool) -> Result<String, String> {
    let Some(feature) = find(key) else {
        return Err(format!("no feature called {key}. Known: {}", FEATURES.iter().map(|f| f.key).collect::<Vec<_>>().join(", ")));
    };
    let mut out = format!("{}\n{}", feature.title, feature.guide);
    if let Some(place) = feature.place {
        out.push_str(&format!("\nWhere it fits: {}. Open it: {}.", place.intent, place.action));
    }
    let missing: Vec<&str> = feature.tools.iter().copied().filter(|t| !held(t)).collect();
    if !missing.is_empty() {
        out.push_str(&format!("\nNot available to you in this session: {}. Some tools exist only inside a quest or only in a main session, and others need a power you do not hold; from a quest, put that step in your report.", missing.join(", ")));
    }
    Ok(out.replace("{owner}", owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_block_stays_small() {
        let block = prompt_block();
        assert!(block.len() <= PROMPT_CAP, "feature map is {} bytes, cap {PROMPT_CAP}: move detail into the guides", block.len());
        for f in FEATURES {
            if let Some(line) = f.line {
                assert!(line.len() <= LINE_CAP, "{} line is {} bytes, cap {LINE_CAP}", f.key, line.len());
            }
            assert!(f.guide.len() <= GUIDE_CAP, "{} guide is {} bytes, cap {GUIDE_CAP}", f.key, f.guide.len());
            assert!(!f.guide.contains('\u{2014}') && !f.line.unwrap_or("").contains('\u{2014}'));
            if let Some(place) = f.place {
                assert!(!place.intent.contains('\u{2014}') && !place.action.contains('\u{2014}'));
            }
            if let Some(launch) = f.launch {
                assert!(launch_date(launch.starts).is_some_and(|starts| launch_date(launch.expires).is_some_and(|expires| starts <= expires)));
                assert!(!launch.prompt.contains('\u{2014}'));
            }
        }
        assert!(!block.contains('\u{2014}'));
    }

    #[test]
    fn keys_are_unique_and_findable() {
        let mut keys: Vec<&str> = FEATURES.iter().map(|f| f.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), FEATURES.len());
        assert_eq!(find("Cards").map(|f| f.key), Some("cards"));
        assert_eq!(find("show_ui").map(|f| f.key), Some("cards"));
        assert_eq!(find("quest options").map(|f| f.key), Some("quest-options"));
        assert_eq!(find("mile").map(|f| f.key), Some("milestones"));
        assert_eq!(find("parts").map(|f| f.key), Some("teams"));
        assert!(find("nope").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn every_listed_key_works_however_it_is_spelled() {
        for f in FEATURES {
            let underscored = f.key.replace('-', "_");
            let spaced = f.key.replace(['-', '_'], " ").to_ascii_uppercase();
            for asked in [f.key, underscored.as_str(), spaced.as_str()] {
                assert_eq!(find(asked).map(|found| found.key), Some(f.key), "{asked} should find {}", f.key);
                assert!(guide(asked, "Ada", &|_| true).is_ok(), "{asked} should give a guide");
            }
        }
        assert_eq!(find("agent_lifecycle").map(|f| f.key), Some("agent_lifecycle"));
        assert_eq!(find("Agent-Lifecycle").map(|f| f.key), Some("agent_lifecycle"));
        assert_eq!(find("quick chat").map(|f| f.key), Some("quick-chats"));
        assert_eq!(find("Voice").map(|f| f.key), Some("voice"));
        assert_eq!(find("offer_memory").map(|f| f.key), Some("quick-chats"));
        let listed: Vec<String> = index("Ada").lines().skip(1).filter_map(|l| l.strip_prefix("- ")?.split(':').next().map(String::from)).collect();
        assert_eq!(listed.len(), FEATURES.len());
        assert!(listed.iter().all(|key| find(key).is_some_and(|f| f.key == key)));
    }

    #[test]
    fn guides_name_the_owner_and_missing_tools() {
        let all = guide("run", "Ada", &|_| true).unwrap();
        assert!(all.contains("Ada") && !all.contains("{owner}") && !all.contains("Not available"));
        let some = guide("cards", "Ada", &|t| t != "write_report").unwrap();
        assert!(some.contains("Not available to you in this session: write_report"));
        assert!(guide("nope", "Ada", &|_| true).unwrap_err().contains("cards"));
        assert!(index("Ada").lines().count() == FEATURES.len() + 1);
    }

    #[test]
    fn one_registry_feeds_navigation_and_launches() {
        let map = prompt_block();
        let index = index("Ada");
        let workshop = guide("workshop", "Ada", &|_| true).unwrap();
        let quest = quest_navigation_prompt("Ada");
        for text in [&map, &index, &workshop] {
            assert!(text.contains("quest-diff"));
        }
        assert!(map.contains("workspace") && index.contains("workspace"));
        assert!(quest.contains("Open workspace") && quest.contains("final report"));
    }

    #[test]
    fn launches_expire_cap_and_rotate_toward_unseen_features() {
        let active = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let expired = NaiveDate::from_ymd_opt(2027, 1, 28).unwrap();
        let first = launch_prompt(active, LaunchScope::Main, 0, &[]);
        let second = launch_prompt(active, LaunchScope::Main, 1, &[]);
        assert_eq!(first.lines().filter(|line| line.starts_with("- ")).count(), LAUNCH_CAP);
        assert_eq!(second.lines().filter(|line| line.starts_with("- ")).count(), LAUNCH_CAP);
        assert_ne!(first, second);
        let app = Url::parse("https://loka.test").unwrap();
        let exposed = |text: &str| launch_exposures_from(text, &app);
        let workspace_url = "https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1";
        let workshop_url = "https://loka.test/world?workbench=00000000-0000-0000-0000-000000000000&diff=00000000-0000-0000-0000-000000000000";
        assert_eq!(exposed(workspace_url), ["canvas"]);
        assert_eq!(exposed(workshop_url), ["workshop"]);
        assert_eq!(exposed(&format!("[Open workspace]({workspace_url})")), ["canvas"]);
        assert_eq!(exposed("[Open workspace](/world?report=00000000-0000-0000-0000-000000000000&workspace=1&item=Focus_Note-7)"), ["canvas"]);
        assert_eq!(
            exposed("HTTPS://loka.test:443/world?report=00000000-0000-0000-0000-000000000000&workspace=1"),
            ["canvas"]
        );
        let self_hosted = Url::parse("http://localhost:4123").unwrap();
        assert_eq!(
            launch_exposures_from(
                "http://localhost:4123/world?report=00000000-0000-0000-0000-000000000000&workspace=1",
                &self_hosted,
            ),
            ["canvas"]
        );
        assert!(launch_exposures_from(
            "/world?report=00000000-0000-0000-0000-000000000000&workspace=1",
            &Url::parse("ftp://loka.test").unwrap(),
        )
        .is_empty());
        let ipv6 = Url::parse("http://[::1]:3000").unwrap();
        assert_eq!(
            launch_exposures_from(
                "[Open workspace](http://[::1]:3000/world?report=00000000-0000-0000-0000-000000000000&workspace=1)",
                &ipv6,
            ),
            ["canvas"]
        );
        assert!(launch_exposures_from(
            "http://[::1]:3001/world?report=00000000-0000-0000-0000-000000000000&workspace=1",
            &ipv6,
        )
        .is_empty());
        assert!(exposed("https://outside.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1").is_empty());
        assert!(exposed("https://loka.test.evil/world?report=00000000-0000-0000-0000-000000000000&workspace=1").is_empty());
        assert!(exposed("http://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1").is_empty());
        assert!(exposed("https://loka.test:444/world?report=00000000-0000-0000-0000-000000000000&workspace=1").is_empty());
        assert!(exposed("//outside.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1").is_empty());
        assert!(exposed("https://outside.test/?next=https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1").is_empty());
        assert!(exposed("https://outside.test/?next=(https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1)").is_empty());
        assert!(exposed("The Workshop shows a diff").is_empty());
        assert!(exposed("https://loka.test/world?report=not-an-id&workspace=1").is_empty());
        assert!(exposed("https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=true").is_empty());
        assert_eq!(
            exposed("https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1&item=Focus_Note-7"),
            ["canvas"]
        );
        assert!(exposed("https://loka.test/world?report=00000000000000000000000000000000&workspace=1").is_empty());
        assert!(exposed("https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1&item=").is_empty());
        assert!(exposed("https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1&item=a%20b").is_empty());
        assert!(exposed("https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1&item=abcdefghijklmnopqrstuvwxyz0123456789extra").is_empty());
        assert!(exposed("https://loka.test/world?report=00000000-0000-0000-0000-000000000000&workspace=1&item=one&item=two").is_empty());
        assert!(exposed("https://loka.test/world?workbench=00000000-0000-0000-0000-000000000000&diff=11111111-1111-1111-1111-111111111111").is_empty());
        let after_workspace = launch_prompt(active, LaunchScope::Main, 0, &["canvas".into()]);
        assert!(after_workspace.contains("Workshop") && !after_workspace.contains("Workspaces"));
        let after_both = launch_prompt(active, LaunchScope::Main, 0, &["canvas".into(), "workshop".into()]);
        assert!(after_both.is_empty());
        assert!(launch_prompt(expired, LaunchScope::Main, 0, &[]).is_empty());
    }

    #[test]
    fn quest_launches_are_selected_from_registry_scope() {
        let active = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        let launch = launch_prompt(active, LaunchScope::Quest, 0, &[]);
        assert!(launch.contains("workspace") && !launch.contains("quest-diff"));
        let feature = |key: &'static str, title: &'static str, prompt: &'static str| Feature {
            key,
            title,
            line: None,
            tools: &[],
            place: None,
            launch: Some(FeatureLaunch {
                starts: "2026-11-27",
                expires: "2027-01-27",
                scopes: &[LaunchScope::Quest],
                prompt,
                evidence: FeatureEvidence { path: "/world", query: &[] },
            }),
            guide: "guide",
        };
        let registry = [feature("first", "First", "first fit"), feature("second", "Second", "different fit")];
        let first = launch_prompt_from(&registry, active, LaunchScope::Quest, 0, &[]);
        let second = launch_prompt_from(&registry, active, LaunchScope::Quest, 1, &[]);
        assert!(first.contains("first fit"));
        assert!(second.contains("different fit"));
    }

    #[tokio::test]
    async fn recent_exposure_is_shared_by_owner_and_scoped_to_organization() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("FEATURES_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?;
        let tag = Uuid::new_v4().simple().to_string().chars().take(12).collect::<String>();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{tag}@features.test"))
            .fetch_one(&pool)
            .await?;
        let other_owner: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(format!("{tag}-other@features.test"))
            .fetch_one(&pool)
            .await?;
        let org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Feature team') RETURNING id")
            .bind(format!("feature-{tag}"))
            .fetch_one(&pool)
            .await?;
        let other_org: Uuid = sqlx::query_scalar("INSERT INTO orgs (slug, name) VALUES ($1, 'Other feature team') RETURNING id")
            .bind(format!("feature-other-{tag}"))
            .fetch_one(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role) VALUES
             ($1, $2, 'owner'), ($1, $3, 'member'), ($4, $2, 'owner')",
        )
        .bind(org)
        .bind(owner)
        .bind(other_owner)
        .bind(other_org)
        .execute(&pool)
        .await?;
        let agent_a: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'A', 'codex', 'gpt', $2, $3) RETURNING id")
            .bind(format!("feature-a-{tag}"))
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let agent_b: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'B', 'codex', 'gpt', $2, $3) RETURNING id")
            .bind(format!("feature-b-{tag}"))
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let foreign_agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'Foreign', 'codex', 'gpt', $2, $3) RETURNING id")
            .bind(format!("feature-foreign-{tag}"))
            .bind(other_owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let other_org_agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id, org_id) VALUES ($1, 'Other org', 'codex', 'gpt', $2, $3) RETURNING id")
            .bind(format!("feature-other-org-{tag}"))
            .bind(owner)
            .bind(other_org)
            .fetch_one(&pool)
            .await?;
        let report = Uuid::new_v4();
        let session = Uuid::new_v4();
        let store = WorldStore::new(pool);
        let app = Url::parse("https://loka.test").unwrap();
        let agent_workspace = format!("[Open workspace](https://loka.test/world?report={report}&workspace=1)");
        let delivered_agent_message = store
            .insert_message(crate::world::NewMessage {
                agent_id: agent_a,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent_a),
                body: &agent_workspace,
                session_id: None,
                run_id: None,
                meta: serde_json::Value::Null,
            })
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta) VALUES ($1, 'agent', $1, $2, '{\"hidden\":true}'::jsonb), ($3, 'agent', $1, $2, '{}'::jsonb), ($1, 'agent', $1, $2, jsonb_build_object('thread_user_id', $4::text))")
            .bind(agent_b)
            .bind(format!("https://loka.test/world?workbench={session}&diff={session}"))
            .bind(agent_a)
            .bind(other_owner)
            .execute(store.pool())
            .await?;
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta) VALUES
             ($1, 'system', $1, $2, jsonb_build_object('kind', 'quest', 'status', 'done', 'task_id', $3::text, 'session_id', $3::text, 'hidden', true)),
             ($1, 'system', $1, $2, jsonb_build_object('kind', 'notice', 'status', 'done', 'task_id', $3::text, 'session_id', $3::text)),
             ($1, 'system', $1, $2, jsonb_build_object('kind', 'quest', 'status', 'done', 'task_id', $3::text, 'session_id', $3::text, 'thread_user_id', $4::text))",
        )
        .bind(agent_a)
        .bind(format!("https://loka.test/world?workbench={session}&diff={session}"))
        .bind(session)
        .bind(other_owner)
        .execute(store.pool())
        .await?;
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);
        assert!(recent_launch_exposures_from(&store, owner, Some(other_org), &app).await?.is_empty());
        assert!(recent_launch_exposures_from(&store, other_owner, Some(org), &app).await?.is_empty());
        sqlx::query("DELETE FROM agent_messages WHERE id = $1")
            .bind(delivered_agent_message.id)
            .execute(store.pool())
            .await?;
        store
            .insert_message(crate::world::NewMessage {
                agent_id: agent_a,
                author_kind: "agent",
                author_user_id: None,
                author_agent_id: Some(agent_a),
                body: &format!("https://outside.test/?next=https://loka.test/world?report={report}&workspace=1"),
                session_id: None,
                run_id: None,
                meta: serde_json::Value::Null,
            })
            .await?;
        assert!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?.is_empty());

        let focused_workspace = format!("/world?report={report}&workspace=1&item=Focus_Note-7");
        store
            .insert_message(crate::world::NewMessage {
                agent_id: agent_a,
                author_kind: "system",
                author_user_id: None,
                author_agent_id: Some(agent_a),
                body: &focused_workspace,
                session_id: None,
                run_id: None,
                meta: serde_json::json!({
                    "kind": "quest",
                    "status": "done",
                    "task_id": session,
                    "session_id": session,
                }),
            })
            .await?;
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);

        let channel = store.create_channel(Some(org), &format!("feature-{tag}"), None, owner).await?;
        let root = store
            .post_channel_reply(&channel, crate::channels::Author::User(owner), "Show me the changes", serde_json::Value::Null, None)
            .await?;
        let workshop = format!("https://loka.test/world?workbench={session}&diff={session}");
        store
            .post_channel_reply(
                &channel,
                crate::channels::Author::Agent(agent_b),
                &workshop,
                serde_json::json!({ "hidden": true }),
                Some(root.id),
            )
            .await?;
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);

        let private_channel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO channels (name, kind, created_by, org_id) VALUES ($1, 'dm', $2, $3) RETURNING id",
        )
        .bind(format!("private-{tag}"))
        .bind(owner)
        .bind(org)
        .fetch_one(store.pool())
        .await?;
        let private_channel = store.get_channel(private_channel_id).await?.unwrap();
        store
            .post_channel_reply(&private_channel, crate::channels::Author::Agent(agent_b), &workshop, serde_json::Value::Null, None)
            .await?;
        store
            .post_channel_reply(&channel, crate::channels::Author::Agent(foreign_agent), &workshop, serde_json::Value::Null, Some(root.id))
            .await?;
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);

        let other_channel = store.create_channel(Some(other_org), &format!("feature-other-{tag}"), None, owner).await?;
        store
            .post_channel_reply(&other_channel, crate::channels::Author::Agent(other_org_agent), &workshop, serde_json::Value::Null, None)
            .await?;
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(other_org), &app).await?, ["workshop"]);

        let teammate_channel = store
            .create_channel(Some(org), &format!("feature-teammate-{tag}"), None, other_owner)
            .await?;
        let teammate_root = store
            .post_channel_reply(
                &teammate_channel,
                crate::channels::Author::User(other_owner),
                "Can you share the changes?",
                serde_json::Value::Null,
                None,
            )
            .await?;
        let unaddressed_reply = store
            .post_channel_reply(
                &teammate_channel,
                crate::channels::Author::Agent(agent_b),
                &workshop,
                serde_json::Value::Null,
                Some(teammate_root.id),
            )
            .await?;
        assert!(!store.is_member(teammate_channel.id, "user", owner).await?);
        assert!(!unaddressed_reply.mentions["users"]
            .as_array()
            .is_some_and(|users| users.iter().any(|id| id == &serde_json::json!(owner))));
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);

        let addressed_reply = store
            .post_channel_reply(
                &teammate_channel,
                crate::channels::Author::Agent(agent_b),
                &format!("@{tag} {workshop}"),
                serde_json::Value::Null,
                Some(teammate_root.id),
            )
            .await?;
        assert!(!store.is_member(teammate_channel.id, "user", owner).await?);
        assert!(addressed_reply.mentions["users"]
            .as_array()
            .is_some_and(|users| users.iter().any(|id| id == &serde_json::json!(owner))));
        let mut addressed = recent_launch_exposures_from(&store, owner, Some(org), &app).await?;
        addressed.sort();
        assert_eq!(addressed, ["canvas", "workshop"]);
        sqlx::query("DELETE FROM channel_messages WHERE id = $1")
            .bind(addressed_reply.id)
            .execute(store.pool())
            .await?;
        assert_eq!(recent_launch_exposures_from(&store, owner, Some(org), &app).await?, ["canvas"]);

        store
            .post_channel_reply(&channel, crate::channels::Author::Agent(agent_b), &workshop, serde_json::Value::Null, Some(root.id))
            .await?;
        let mut delivered = recent_launch_exposures_from(&store, owner, Some(org), &app).await?;
        delivered.sort();
        assert_eq!(delivered, ["canvas", "workshop"]);
        assert!(launch_prompt(NaiveDate::from_ymd_opt(2026, 11, 27).unwrap(), LaunchScope::Main, 0, &delivered).is_empty());
        Ok(())
    }
}
