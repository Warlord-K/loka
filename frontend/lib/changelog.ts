export type ChangelogEntry = {
  date: string;
  title: string;
  items: string[];
};

export const CHANGELOG: ChangelogEntry[] = [
  {
    date: "2026-11-22",
    title: "Settings, Academy courses, calls in quick chats and new art",
    items: [
      "**Settings**: the window behind your name is now called Settings, with a gear on your name card. Cmd K finds it when you type settings, preferences or profile.",
      "**Quick chats do more**: select text and press **Quote** to ask about exactly that, or press the phone button for a live call that stays in that quick chat.",
      "**Open commands in your terminal**: every command an agent gives you has **Open in terminal** and **Copy** next to **Run**. Open in terminal types it at the prompt without running it and opens that agent beside the terminal. Select terminal output and press **Quote** to ask your agent about it.",
      "**Plan approvals in your own words**: reply to a plan with \"go ahead\" and your agent builds it, \"yes, but use a view\" builds it with your change, and a question gets an answer.",
      "**Quest commits show Verified**: GitHub signs them when a quest publishes, so they show Verified without a signing key.",
      "**Safer report edits**: saving a report never overwrites a newer version. Loka merges the parts you did not touch and asks which version to keep when you both changed the same part.",
      "**A fresh music playlist**: 52 cozy game, Ghibli and lofi songs that shuffle without repeats. Songs that will not play are skipped, and asking an agent to play a song by name works again.",
      "Minor fixes: dragging cards on the to-do board works in the app, a quest that fixes an open pull request updates it instead of opening a duplicate, a quest's browser tabs and build files are cleaned up when it ends, chats catch up after a dropped connection, Cmd K opens the exact list, team or place you picked, and agents can open audio and video you attach in chat.",
      "**New art (preview)**: switch on New art (preview) in Settings > Appearance to see Loka newly drawn: every agent walks at a bigger size in its own colours, and houses, civic buildings, trees and decorations are redrawn, with palms, snowmen, mossy logs and boulders in the regions. The classic art stays the default.",
      "**Calls happen in the chat**: a voice call now shows your whole chat with that agent, and both sides of the call land in it as you talk, so nothing is lost when a call drops mid-sentence. Call from the chat next to a report and the call stays there.",
      "**Keep a call going while you work**: the X on a call minimises it to a small bar at the top with mute and Hang up, so you can read reports, open quests and switch chats while you talk. Click the bar to come back.",
      "**Live dictation**: click the microphone in a message box and your words appear as you speak, right where your cursor is, in a lighter colour until they are final. When you stop they stay in the box to edit, so you can type or talk again before you send.",
      "**Zoom into pictures**: when you open a picture from a chat, a quest, a report or a canvas, zoom in with the scroll wheel, a pinch, the + and - buttons or keys, or a double-click, and drag to look around. Pixel art stays sharp.",
      "Agents can now be a businessman with a briefcase, a kid, a unicorn, a cactus or a mushroom. Person is now called human.",
      "Quests can now message other agents, such as the main agent that started them, by name or handle.",
      "The Slack tools your agents use now know which Slack workspace and account Loka is watching, so they post as you in the right workspace.",
      "Sign-ins of MCP servers that use mcp-remote stay out of quest folders and are never committed.",
      "**Courses in the Academy**: learn a topic as a course of lessons, each with reading from your reports and one task. Practice canvases are now lessons: start one from Reports or Cmd K, write or draw with a pen, pencil, marker or your mouse, press Ready and your coach tells you what is right and what to fix. A pass earns XP and counts toward the course.",
      "**Auto context limit**: in an agent's context menu (top of its chat), set a token count and pick Compact or Reset, and the agent tidies its conversation on its own after a turn that passes it. Use default goes back to the built-in limit of about 70% of the window.",
      "**New art (preview)** now also redraws the home town's ducks, cats, sparrows, koi and the peacock, every piece of furniture inside buildings and houses, and agents' work, tools, sparks and status bubbles.",
      "**A full disk no longer stops every quest**: a closed quest's temp folder is deleted an hour after the quest closes, Settings > Storage counts temp folders, and while your Mac has under 5 GB free new quests fail with a plain \"disk is full\" note instead of breaking halfway.",
      "Quests sign their commits only with your own git signing key or Loka's own key (~/.ssh/loka_signing), never another key they find on your Mac. When neither exists, Loka creates its own key and your main agent tells you once how to add it to GitHub so commits show Verified.",
      "A review that stops without giving a verdict is asked again for one, and if none comes the result says \"Review ended without a verdict\".",
      "On a voice call, what you say lands in the chat as one whole message after you finish talking, and short pauses mid-sentence no longer split it.",
      "Quests on your Mac can build iOS apps with xcodebuild inside the sandbox and create branches from origin/main without failing to save their tracking setup.",
    ],
  },
  {
    date: "2026-11-21",
    title: "Canvases, lean quests, safer by default",
    items: [
      "**Canvases**: agents can lay out images, videos, 3D models, notes and runnable steps on a board with arrows, right in Reports. Pan, zoom, drag, draw, and pin a mark or stick a note on anything; the agent that made the canvas picks it up, iterates and replies under your mark. Put quests, reports, to-dos, messages and live cards on a canvas with L, find canvases and their items with Cmd K, and quote a canvas or one item into chat.",
      "**Your disk no longer fills up with old quests**: Loka removes a quest's checkout 3 days after it is done (7 days when it failed) and an hour after it is cancelled, and clears closed ones oldest first when your disk drops below 5 GB, with a bell notice. You > Storage shows what quest workspaces take and can clear closed ones.",
      "**Locked to the app**: Loka's local server now answers only the Loka window and this launch's secret, so websites in your browser and other programs on your Mac can no longer act as you. Opening Loka in a browser tab needs a fresh link from `loka-api login-link`.",
      "**Quests fail safe**: when a coding CLI cannot run in auto mode, the quest stops and says how to fix it instead of quietly taking full access, and quests stay in the sandbox unless you turn it off. Agents can read only your ssh config, known hosts and signing keys, never your other private keys.",
      "**Lean quests by default**: new quests start from the agent's notes, rules, repo notes and recent quests instead of its whole main chat, so they no longer run out of room on the first turn. Pick Lean, Auto or Full per agent under Access, or for one quest next to How to approach it.",
      "**Import a .env file**: under You > Environment, press Import and paste a .env block to save many keys at once. You see which keys are new, which would replace a saved value and which names cannot be used before anything is saved.",
      "**Keys from 1Password**: save a key as an op://vault/item/field reference instead of its value, by hand or in a pasted .env. Your Mac reads it from 1Password when a run starts and keeps it only for that run, and the key shows a Vault badge.",
      "**More looks for agents**: agents can now be an octopus, bunny, crab, dino, dragon, frog, penguin, shark, robot, ghost, axolotl, bee and more, and wear a wizard hat, crown, beanie, glasses, headphones or a cape. Pick them with a live preview when you create an agent or edit its look.",
      "Quests from an agent whose conversation is close to full now start fresh from its notes and recent quests instead of failing within seconds, and resetting such an agent's context works and keeps its saved notes.",
      "Quests whose branch has no work of its own, or whose work is already on main after a squash merge, now finish cleanly instead of opening an empty pull request, and a branch with no shared history with main fails with a plain note to rebase it.",
      "When a chat message can't be sent, Loka now says why, like a full disk, a restart or sending too fast, and keeps retrying for about 30 seconds when the problem is temporary.",
      "Quests that finish after a long session are no longer marked failed when the agent writes its final report in a slightly different format.",
      "Agents in a quest can open the dev server they started on localhost in their browser to check their work, and the browser, screen capture and app control no longer refuse to start inside quests.",
      "Quests can now save and update their own reports, and agents on a team can post updates in the team channel while they work.",
      "Quests can run git init and git clone in their folder, build with Xcode and Swift Package Manager and boot iOS simulators inside the sandbox, and git no longer prints credential store errors on every fetch.",
      "Smaller UI fixes and polish, like the Workshop executor panel following dark mode.",
    ],
  },
  {
    date: "2026-11-20",
    title: "Quote anything into your reply",
    items: [
      "**Quote what you select**: select part of a message, channel post or report and press Quote to ask about exactly that. Your agent sees the quote and the text around it; quotes from a report go to your main agent, and to-dos have Ask about it.",
      "**Approved actions**: tell your main agent what it may always do, like cleaning up its own temp folders or running a repo's scripts, and quests stop getting blocked on it. When a quest is blocked, your agent asks once whether to allow it for good.",
      "**More accurate spending**: an agent's spend no longer jumps after the app restarts or a run is stopped, and starting a quest no longer counts the turn that started it twice. Dollar figures now say they are API list prices, which a Claude or ChatGPT plan does not bill.",
      "Table columns in reports can be dragged wider or narrower, and keep the widths you pick. Double-click the edge to reset.",
      "Sending a channel message no longer shows an error and puts your text back after it was already sent.",
      "Smaller UI fixes and polish, like short chats starting at the bottom next to the message box.",
    ],
  },
  {
    date: "2026-11-14",
    title: "Guides for running Loka from source",
    items: [
      "**Run Loka from source**: the README says what you need on your Mac, how to build the app yourself and how the pieces fit together.",
      "CONTRIBUTING.md shows how to run the API, the web app and a runner straight from source, the checks to run before a pull request, and the house rules.",
      "Three guides in the docs folder: how Loka works inside, where your data lives and how to back it up, and how the coding CLIs compare on speed, cost and features.",
    ],
  },
  {
    date: "2026-11-13",
    title: "One broken connection no longer stops your agents",
    items: [
      "An MCP connection that is missing a key is now left out of that run instead of stopping the agent from working at all.",
      "Notion and other sign-in based connections remember your sign-in on this Mac, so you sign in once instead of every quest, and their sign-in files never end up in a pull request.",
      "Agents can rewrite and force push their own quest branch, for example to remove a file committed by mistake, without asking you first.",
      "Agents you create while your Mac is offline wait for it instead of never starting.",
      "Notifications at the top of the screen go away once you have dealt with them, for example by opening that chat or answering the question, stay put while you hover them, and leave sooner and more gently on their own.",
      "Loka starts on Macs with very long account names instead of stopping at launch.",
    ],
  },
  {
    date: "2026-11-12",
    title: "A quieter bell",
    items: [
      "**Opening the bell clears it**: messages, mentions, reminders and quest results count as seen once you open the bell, and only questions waiting for your answer stay. They still show until you close it, and Cleared keeps the history.",
      "An agent's unread number in Chat goes away once you have read the chat, thread replies included, and no longer counts quest results or reminders.",
    ],
  },
  {
    date: "2026-11-11",
    title: "Chats open where you left off",
    items: [
      "Switching back to an agent's chat opens it at the latest message again, instead of partway up the conversation.",
    ],
  },
  {
    date: "2026-11-02",
    title: "Updates on your terms",
    items: [
      "**Updates in You**: You > Updates shows which version of Loka you have, checks for a new one on demand, and has an Update automatically switch that installs new versions on its own once your agents finish their work.",
      "**Slack capture says what is wrong**: turning on Slack capture connects right away, Devices says exactly what to do when it cannot (sign in to Slack, allow the Keychain prompt, or Full Disk Access only when macOS blocks it), it works with Slack from the Mac App Store, and your agent sees the same reason instead of guessing.",
      "When you give your main agent a new name during setup, its @mention changes to match, and the old one keeps working.",
    ],
  },
  {
    date: "2026-11-01",
    title: "Memory you can see",
    items: [
      "**See how full an agent's memory is**: the agent panel shows how much of its context is used, with a bar that turns amber near the limit. The button next to it compacts the conversation, where the agent's CLI supports it, or resets it after you confirm.",
      "Agents can keep their memory tidy on their own: set a token limit and choose compact or reset, and once a conversation grows past the limit the agent cleans it up after its reply.",
      "**Dark mode works again**: the dark theme had stopped applying, so Loka stayed light even when you picked dark.",
      "An empty chat with an agent offers a few one-tap starters, like saying hi or asking what it can help with.",
      "In Reports, the On this page outline follows where you are, and you can hide the library to read wide.",
      "Cards in chat and reports now send what you actually picked, and an answered card keeps showing your picks.",
      "Quests that end with a long report no longer get stuck asking for one.",
      "Codex agents signed in with a ChatGPT account no longer fail with a sign-in error when an OpenAI key is also set.",
      "Every Loka Mac download and update is signed and notarized by Apple, so macOS opens it without warnings.",
      "Smaller UI fixes and polish across Loka.",
    ],
  },
  {
    date: "2026-11-01",
    title: "Updates that keep your agents working",
    items: [
      "**Updates without a restart**: when only Loka's server or app changed, Loka swaps that part in place and your agents keep working through it. If the new part does not start, Loka goes back to the one you had. Turn on Update the server and app on their own in the update card to skip the prompt.",
    ],
  },
  {
    date: "2026-11-01",
    title: "Open files straight from chat",
    items: [
      "**Paths your agents mention open with one click**: click a file or folder path in an agent's chat, quick chat, thread or report to open it in the Workshop with that agent, right at the line it named. Loka reuses the folder session you already have, or shares the project folder for you.",
      "When the Workshop is already open on that folder, the file opens in a new tab. The same menu can show the file in Finder, and every path has Copy path.",
      "**Reply in a thread without tagging**: in a channel thread, your reply goes to the agent who last spoke there, or the one the thread started with. Tag someone else and only they hear it.",
      "When an agent is writing in a thread, you see it typing in that thread instead of in the channel.",
      "**Agents keep working while Loka restarts**: if Loka's server is away for a while, your agents carry on, and everything they did shows up in order once it is back.",
    ],
  },
  {
    date: "2026-10-31",
    title: "A smoother first run and updates",
    items: [
      "**Setup checks your tools**: the first time you open Loka, your main agent shows which of Claude Code, Codex, Cursor and the GitHub CLI are installed and signed in, with the exact command to install or sign in to each, and only offers the ones that are ready.",
      "The guided setup now starts on a fresh install, right after you tell Loka your name.",
      "**Updates**: Loka checks for a new version in the background, downloads it, and shows a card when it is ready. Restart now pauses agents that are mid-quest and they pick up where they left off once Loka reopens, or pick When they finish to restart once they are done. Check for Updates is in the Loka menu and Cmd K.",
    ],
  },
  {
    date: "2026-10-31",
    title: "A new look",
    items: [
      "**A new look, by Ashwin**: Loka has a new look, and chat, quests, reports, lists and settings now look and work the same way.",
      "The tidier look of the last few weeks, with its smooth windows and pixel portraits, is his work too.",
    ],
  },
  {
    date: "2026-10-25",
    title: "Start from what you want to get done",
    items: [
      "**Setup starts from what you want to get done**: the last setup question asks about your work, whether you live in a GitHub repository, in email and documents, in articles, on call or in planning tools. Your main agent comes back with a proposed setup for you to approve, and nothing is created until you tick it.",
      "**Teams that learn from a repository's own people**: point your main agent at a GitHub repository and it finds who gets pull requests merged there and who reviews them, then suggests agents modelled on how they work.",
      "**A kit for any other goal**: for email, reading, being on call or planning, your main agent proposes the skills, MCP servers and routines that fit, and lists the few steps only you can do.",
      "**Quick chats**: press K, or Quick chat in an agent's window, for a side conversation that starts from the agent's memory. Its main conversation and work carry on untouched, and nothing from the chat is kept unless you press Remember on a line the agent offers.",
      "**Threads with your agents stay on the side**: replying in a thread in your own agent's chat now opens a quick chat, so the agent keeps working and its main conversation is never interrupted.",
      "**Approve right from a report**: agents end their reports with the decisions they argue for, like starting a quest or picking an option, as buttons you press in place. Commands in a report get the same Run button as commands in chat.",
    ],
  },
  {
    date: "2026-10-24",
    title: "Agents that know what Loka can do",
    items: [
      "**Agents use what Loka has**: every agent now knows what Loka can do and looks up the how-to when it needs one. A chart, a checklist, a reminder or a watch turns up when it fits the work, even if you never knew to ask for it.",
      "**Agents finish what you asked for**: once you have said what you want, agents carry it through without asking for a go-ahead between steps. They answer review comments and fix failing checks on their own pull requests for up to three rounds, then tell you what keeps failing. They stop only for decisions that are yours.",
      "**Quests with parts**: an agent can split a big quest into parts for several agents. The quest room lists each part and who has it, and the quest closes by itself once all of them are done.",
      "**Live quest checklists**: agents add milestones to quests with more than one step and tick them off as the work happens. The quest room shows the checklist.",
      "**Run button on commands**: when an agent asks you to run something, press Run on the command and confirm. The output shows in chat and goes back to the agent, so it carries on without you.",
      "**Next-step buttons**: when there is a real choice of where to go next, a reply ends with up to three buttons you can press.",
      "**Setup in one click, from chat**: when something has to be switched on or installed first (Slack capture, apps or the screen of your Mac, the browser for agents, the GitHub feed, a coding CLI or its sign-in), your agent shows a card that turns it on, runs the install, or opens the exact page.",
      "**Charts**: ask for a chart and get a bar, line, area or pie chart in chat or in a report. Hover for exact values or flip it to a table. Agents also put charts, number tiles and tables into reports on their own now.",
      "**Screenshots and recordings in chat**: agents can show what they saw, as pictures or short videos, and fold away the in-between ones so the result stands out. Capturing your Mac's screen stays off until you turn on Let quests see this screen in Devices.",
      "**Apps on your Mac**: turn on Let quests use apps on this Mac in Devices, and a quest can open and click through apps like Xcode or Simulator. It asks you first, every quest.",
      "**The bell holds only what needs you**: questions, messages from your agents, mentions, reminders, failed quests and paused or stuck agents. Each one stays until you answer it, open it or clear it, then moves to Cleared. Finished quests no longer pile up there.",
      "**Archive agents**: put away an agent you no longer need without deleting it. It stops working, leaves the map and your lists, and keeps its memory, chats and kit under Residents > Archived, where Restore brings it back. You can have it deleted for good after 7, 30 or 90 days, off unless you pick it. Your main agent can archive, restore or delete agents when you ask.",
      "**Storage and cleanup**: You > Storage shows how much space each of your agents takes and when it was last active. Clean up old run logs, closed quests, chats or activity when you want to: see what would go, then confirm. Loka never deletes anything on its own, and your main agent asks before it cleans up for you.",
      "**Keys stay off the agent**: Claude Code, Codex, oh-my-pi and OpenCode get a placeholder in place of your model key, and Loka adds the real key on the way out. Keys you grant an agent work the same way: its commands run with the real values, and the output shows placeholders. An agent that reads its environment finds nothing it can use elsewhere.",
      "**Several values per key**: keep a staging token or a second account next to a key's default value under You > Keys and pick which one each agent uses. You can also ask your main agent to switch, for a few hours or for good.",
      "**Command rules**: block commands like git push for your agents, or allow only a short list, under You > Keys or by asking your main agent. They apply to Claude Code and OpenCode quests, and stay off until you add one.",
      "**Edit reports like a doc**: press Edit, E or double-click the page and write right on it. Type / for headings, lists, to-dos, tables and code, [[ to link another report, and select text to format it. Changes save as you type.",
      "**Copy a whole report**: Copy at the top of a report puts the full text on your clipboard as Markdown, title included, and the download button saves it as a .md file. Shell output, a quest's brief, each draft an agent writes for you and any list have copy buttons now too.",
      "**Find with Cmd F**: search the open window with every match highlighted. Enter and Shift Enter step through them.",
      "**Resize panes**: drag the edge of the reports library, the chat list, a thread, session history or the side chat. Double-click the edge to reset it.",
      "**Delete your channel messages**: hover a message you wrote and pick Delete. You have five seconds to undo. Messages that already have replies stay.",
      "Threads and the quest room take pictures and files like the main chat.",
      "In chat, Up edits your last message. Alt Up and Alt Down move between chats, and adding Shift jumps to the next one with unread messages.",
      "Pictures in messages open in a viewer, and arrow keys step through several at once.",
      "A live call with an agent opens with a short greeting in its own voice, recorded once per agent.",
      "A schedule now waits until its last run has been picked up before it fires again, so checks no longer pile up while an agent is paused or held up.",
      "The side chat beside Reports and Lists closes with the window when it opened on its own, so your main agent no longer stays pinned next to every report.",
      "File attachments and exports save to your Downloads folder, with a note when they land.",
      "Quests start again on Macs that have agent-browser installed.",
      "Messages look and work the same in channels, agent chats, threads and the quest room, plus smaller fixes and polish.",
    ],
  },
  {
    date: "2026-10-17",
    title: "GitHub, threads and voice everywhere",
    items: [
      "The first time you open Loka it asks what your agents should call you, and greets you by name after that.",
      "**GitHub feed**: turn it on in You > GitHub and the Loka app passes your new GitHub notifications (review requests, mentions, comments, CI) to the agents that subscribe to them. It uses your gh login on this Mac and nothing is marked read. Comments, reviews and failing CI on a pull request one of your quests opened go back to that quest within a minute or so.",
      "Agents can watch a single pull request or issue, like \"watch acme/api#12\", and stop when you ask. Each agent's watches are listed under its subscriptions.",
      "**Watch a Slack channel or thread**: ask an agent to watch #incidents or paste a thread link, and every new message there reaches it through Slack capture, whether or not it mentions you. Watched messages never ring your bell.",
      "**Urgent work gets checked on**: when a quest is urgent, your main agent or a team lead checks on it every few minutes and stops by itself when the quest is done.",
      "Agents push through more on their own: when one part is blocked they finish the rest, say what they assumed and report what is left, and only ask when the answer changes the work.",
      "New starter teams no longer include watcher agents that check every 30 minutes. Agents watch the pull requests, channels and threads their own work needs.",
      "**Start from how you already work**: your main agent can read your recent Claude Code, Codex, Cursor and omp sessions on this Mac and suggest a team, skills, rules, MCP servers and starting notes. The reading stays on your Mac, and only what you tick is created. It is offered during setup and in Cmd K.",
      "**Threads in chat and channels**: reply to any message in a thread from its hover menu. Threads open on the side, update live, show who replied and when, and an agent you message in a thread answers in that same thread.",
      "**@here** asks only the agents that are around right now, while **@channel** reaches every agent in the channel. Paused agents are skipped by both.",
      "**See when an agent is replying**: \"Priya is typing\" shows above the message box while it works on its answer, and **Seen by** under your latest message shows which agents picked it up.",
      "**Voice, everywhere**: every message box has a microphone you can tap or hold, plus **Option V** while typing. Agents know when you spoke and answer in a few short sentences meant to be heard, read aloud without links or code, and music pauses while they speak.",
      "Chats and threads stay at the latest message while replies, typing and pictures arrive, hold your place when you scroll up, and land at the bottom when you send.",
      "**Your agents work like the CLIs they run on.** Quests and folder sessions no longer run inside Loka's extra sandbox, so gcloud, kubectl, SSH, local servers and any website work as they do in your terminal. The agent's approval setting (Read only, Auto, Full) is the limit, and `WORLD_TASK_SANDBOX=strict` brings the sandbox back.",
      "Agents can sign their commits, merge commits included, whether the key is a file in ~/.ssh or held by your SSH agent, and commits no longer carry a Claude co-author line.",
      "Tell your main agent a lasting permission, like \"force pushes are okay\", and it saves it as a rule every quest and folder session follows. Quests whose branch was rebased publish again.",
      "Cmd K does more: open any window or You page, switch theme, music, sounds and text size, start a quest or a new agent, and find a quest by its id.",
      "Esc closes only the window on top.",
      "Smaller UI fixes and polish.",
    ],
  },
  {
    date: "2026-10-11",
    title: "Bigger text and budgets on every CLI",
    items: [
      "Agents work with the coding tools already signed in on your Mac, like Claude Code and Codex, and open pull requests from your own GitHub login.",
      "**Pick your text size**: press Cmd + or Cmd - to make text and windows bigger or smaller, and Cmd 0 to go back to the default. Loka remembers your choice, and big screens scale up on their own.",
      "Esc closes Reports, Lists, You and What's new again.",
      "Agents can read Loka's docs on their own when you paste a docs link or ask how something works.",
      "**Budgets that hold on every CLI**: spend from Claude, Codex, Cursor, omp and OpenCode counts toward an agent's daily budget while work runs, so a long quest stops when the budget runs out. Failed and timed out runs count too.",
      "**Token budgets**: give an agent a daily token limit next to the dollar one (like 2M), which also covers models with no known price.",
      "**Cmd K finds everything**: search finds agents, messages, reports, quests, schedules, timers, teams, regions, characters, feeds, rules, skills and MCP servers, and opens each one where it lives. Press Cmd Enter to send anything else to your main agent.",
      "**Cursor and Codex show what they cost**: Loka prices their tokens from public model list prices and your main agent can explain where your spend goes. These are estimates at API prices, so they can differ from what your plan bills.",
      "**Your main agent can do what you can**: ask it in chat to pause or resume an agent, set a budget or approval mode, copy or reset an agent, or make another agent main.",
      "Cursor, OpenCode and omp agents answer chat faster, start quests from their own conversation and can rewind.",
      "Codex and omp agents tidy up long conversations on their own, the way Claude agents do.",
      "Loka finds Claude Code installed with nvm, Volta, Bun, pnpm or the old local installer, and a missing, outdated or signed out Claude Code says exactly what to do.",
      "Connect Claude Code, Cursor or any MCP client to your agents with a token from You > Connect other tools. Tokens expire after 30 days, 90 days or a year.",
      "**Loka Pro is coming**: organizations for your team, the iPhone, iPad and Apple Watch apps, and shared agents and devices. Join the waitlist from You or the command palette.",
    ],
  },
  {
    date: "2026-10-10",
    title: "Agent teams and a tidier Loka",
    items: [
      "**Starter team in one click**: at the end of setup, or later from the Quest board, get two coders, a reviewer and a PR watcher, each with its own job, notes and a nightly fresh start. Your main agent stays the coordinator.",
      "**Agent teams**: group your agents into teams with a purpose, a lead, their own rules and a team channel. Give a team a quest and its lead splits it up, and the quest closes once every part is done.",
      "Claude quests no longer finish without changing anything when auto mode is not allowed for your account.",
      "**Lighter MCP servers**: agents share one copy of each MCP server that is safe to share, like GitHub or Context7, instead of starting one per chat and quest. Add `\"shared\": true` or `false` to choose yourself.",
      "Keys your MCP servers use are no longer written to disk, and agents only get the MCP servers you equipped them with, never the ones set up on your Mac for yourself.",
      "Agents waste less: no thank-you messages to each other, quiet scheduled checks send nothing, and they learn your voice from the edits you make to their drafts.",
      "A tidier look across Loka.",
    ],
  },
  {
    date: "2026-10-04",
    title: "Interactive cards from agents",
    items: [
      "**Interactive cards**: agents can show you a small dashboard, table or form right in chat. Pick an option or fill it in to answer.",
      "Reports can hold the same live cards, so a weekly dashboard or a choice sits right in the write-up.",
      "**Chat beside Reports and Lists**: keep an agent open next to what you are reading and keep giving instructions.",
      "In the terminal, Cmd+Backspace deletes the line and Option+Backspace a word, like your Mac terminal.",
    ],
  },
  {
    date: "2026-10-03",
    title: "Real share links",
    items: [
      "Agents can hand you a link to a message, quest, report, to-do or agent, and pasted links show as cards.",
      "Cancelled quests stay cancelled even when work was still finishing.",
    ],
  },
  {
    date: "2026-09-26",
    title: "Reports and quest options",
    items: [
      "**Reports**: agents write long findings into a linked markdown library with wiki links, backlinks and revisions. Open it with R or from the Library on Hilltop.",
      "**Quest options**: ask for a plan first, several attempts judged by a reviewer, or failing CI sent back to the quest. All off unless you ask.",
      "**Quest rooms**: one live view per quest with its agents, discussion, events and steps.",
      "**Quick and urgent quests**: small changes skip the extra review and report, and urgent work runs ahead of everything else.",
      "**Suggested quests**: tell your main agent to suggest work weekly and it files ideas on your To do list for approval.",
      "Agents remember what they learned about each repo in a Repo notes report.",
      "**Failures explain themselves**: a quest that stops says why in plain words, and runs that hit a passing hiccup are retried once on their own.",
      "Terminals open your real login shell, so Tab completes like in your own terminal.",
    ],
  },
  {
    date: "2026-09-19",
    title: "Loka grows up",
    items: [
      "Regions with portals and travel, plus the University, Workshop Quarter and Hilltop districts joined by roads.",
      "The Academy: graded challenges, XP and cosmetics while you wait on quests. Switch it on under Academy Progress.",
      "Characters staffed by real agents: talking to one sends that agent a message.",
      "Civic places with live signs and interiors: Clock Tower, Post Office, Market, Forge, Library and Observatory.",
      "Folder sessions: work with an agent directly in a folder on your Mac, no quest or PR.",
      "Background music with a calm default playlist, and agents can play anything you ask for.",
      "Quest time limits, quick asides with a model choice, and a usage report your agent can explain.",
    ],
  },
  {
    date: "2026-09-12",
    title: "Clear failures and spending limits",
    items: [
      "CLI failures are explained in plain language, usage limits are waited out instead of failing, and rejected keys pause the agent with a fix.",
      "Daily budgets per agent, with a warning at 80% and a pause at 100%.",
    ],
  },
  {
    date: "2026-09-06",
    title: "Setup by asking",
    items: [
      "A first-run chat with your main agent sets everything up.",
      "Feeds: RSS that wakes an agent when something new is posted.",
      "Per-agent omp models, setup hints that point at the next step, sound effects and a shortcut sheet on ?.",
    ],
  },
  {
    date: "2026-09-05",
    title: "Loka",
    items: [
      "**Agents that remember**: each agent keeps one long conversation in a real coding CLI, Claude Code, Codex or Cursor. What you told it last week is still there today.",
      "**Quests**: hand an agent some work and it forks that conversation into its own copy of the repository, with full tools. When it is done the work is committed, pushed and opened as a pull request, and the agent reports back to you.",
      "**Everything runs on your Mac**: the app brings its own database, server and web UI, so there is nothing to host. Agents use the coding CLIs you are already signed in to.",
      "A quest can get an independent review: a fresh reviewer reads the work and sends it back with what to fix before the pull request opens.",
      "A small pixel town where your agents live. They walk to work when a quest starts and visit each other when they talk.",
      "A main agent that is always yours. It answers you, hands work to the others and keeps track of it.",
      "Chat with each agent, and channels for the whole group, with history and notifications.",
      "A kit for each agent: the skills and MCP servers its job needs. Keys are stored encrypted, never shown back, and each agent gets only the ones you grant it.",
      "Voice mode: talk to your main agent and hear it answer. Agents stay warm between messages, so replies come fast.",
    ],
  },
];

const SEEN_KEY = "world.changelog.seen";

export function latestChangelogDate(): string {
  return CHANGELOG[0]?.date ?? "";
}

export function changelogSeen(): string | null {
  try {
    return window.localStorage.getItem(SEEN_KEY);
  } catch {
    return null;
  }
}

export function markChangelogSeen() {
  try {
    window.localStorage.setItem(SEEN_KEY, latestChangelogDate());
  } catch {}
}

export function unseenChangelog(): ChangelogEntry[] {
  const seen = changelogSeen();
  if (!seen) return [];
  return CHANGELOG.filter((e) => e.date > seen);
}

export function primeChangelog() {
  if (changelogSeen() === null) markChangelogSeen();
}

export const OPEN_CHANGELOG = "world:open-changelog";

export function openChangelog() {
  window.dispatchEvent(new CustomEvent(OPEN_CHANGELOG));
}

export function changelogDate(date: string): string {
  return new Date(`${date}T12:00:00Z`).toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric", timeZone: "UTC" });
}
