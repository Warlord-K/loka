import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { consumeDiffHref, externalFeatureHref, featureLinkFromHref, workspaceHref } from "./feature-links.ts";

const origin = "https://loka.example";
const session = "12345678-1234-4234-8234-123456789abc";
const other = "abcdef12-3456-4789-abcd-ef1234567890";

test("feature links recognize the exact same-origin Workshop diff and workspace", () => {
  assert.deepEqual(featureLinkFromHref(`${origin}/world?workbench=${session}&diff=${session}`, origin), { kind: "workbench", sessionId: session });
  assert.deepEqual(featureLinkFromHref(`${origin}/world?report=${other}&workspace=1`, origin), { kind: "workspace", reportId: other });
  assert.deepEqual(featureLinkFromHref(`${origin}/world?report=ABCDEF12-3456-4789-ABCD-EF1234567890&workspace=1&item=Focus_Note-7`, origin), {
    kind: "workspace",
    reportId: other,
    item: "Focus_Note-7",
  });
  assert.deepEqual(featureLinkFromHref(`${origin}/world?report=${other}&workspace=1&item=not%20valid`, origin), { kind: "workspace", reportId: other });
  assert.equal(featureLinkFromHref(`${origin}/world?report=${other}&item=Focus_Note-7`, origin), null);
});

test("workspace links preserve their exact focused item through parsing", () => {
  const href = workspaceHref(origin, other, "Focus_Note-7");
  assert.equal(href, `${origin}/world?report=${other}&item=Focus_Note-7&workspace=1`);
  assert.deepEqual(featureLinkFromHref(href, origin), { kind: "workspace", reportId: other, item: "Focus_Note-7" });
  assert.deepEqual(featureLinkFromHref(workspaceHref(origin, other), origin), { kind: "workspace", reportId: other });
});

test("Workshop links stay bound to one exact session on this Loka", () => {
  assert.equal(featureLinkFromHref(`${origin}/world?workbench=${session}&diff=${other}`, origin), null);
  assert.equal(featureLinkFromHref(`${origin}/world?workbench=12345678&diff=12345678`, origin), null);
  assert.equal(featureLinkFromHref(`https://outside.example/world?workbench=${session}&diff=${session}`, origin), null);
  assert.equal(featureLinkFromHref(`https://outside.example/world?report=${other}&workspace=1`, origin), null);
  assert.equal(featureLinkFromHref(`${origin}/world?workbench=${session}`, origin), null);
  assert.equal(externalFeatureHref(`https://outside.example/world?workbench=${session}&diff=${session}`), true);
  assert.equal(externalFeatureHref(`https://outside.example/world?report=${other}&workspace=1`), true);
  assert.equal(externalFeatureHref(`${origin}/world?report=${other}&workspace=1&item=note`), true);
  assert.equal(externalFeatureHref(`${origin}/world?report=${other}&item=note`), false);
});

test("consuming a Workshop diff keeps unrelated navigation and refuses another session", () => {
  assert.equal(consumeDiffHref(`${origin}/world?quest=${other}&diff=${session}#changes`, session), `/world?quest=${other}#changes`);
  assert.equal(consumeDiffHref(`${origin}/world?diff=${other}`, session), null);
});

test("chat feature links use the existing openers and Workshop diff mode", () => {
  const embeds = readFileSync(new URL("../components/world/embeds.tsx", import.meta.url), "utf8");
  const bridge = readFileSync(new URL("../components/world/workbench/bridge.ts", import.meta.url), "utf8");
  const workshop = readFileSync(new URL("../components/world/workbench/workbench.tsx", import.meta.url), "utf8");
  assert.match(embeds, /detail=\{\{ sessionId, diff: true \}\}/);
  assert.match(embeds, /new CustomEvent\(OPEN_WORKSPACE, \{ detail: \{ id: report\.id, item \} \}\)/);
  assert.match(embeds, /<ReportEmbed refId=\{embed\.ref\} item=\{embed\.item\} workspace/);
  assert.match(workshop, /openChange\(changes\[0\]\)/);
  assert.match(workshop, /consumeDiffHref\(window\.location\.href, requestedDiff\)/);
  assert.match(bridge, /detail\.sessionId === openWorkshop\?\.sessionId[\s\S]*openWorkshop\.openDiff\(\)[\s\S]*return;/);
  assert.match(workshop, /openedDiff\.current === requestedDiffToken \|\| !changes\[0\]\) return;/);
  assert.match(workshop, /changesLoadedForToken !== requestedDiffToken/);
  assert.match(workshop, /\[loadChanges, changeRetry, directDiff\]/);
  assert.match(embeds, /window\.dispatchEvent\(new CustomEvent\(OPEN_TERMINAL_EVENT\)\)/);
  assert.match(embeds, /requestSetup\("devices"\)/);
  assert.match(embeds, /open:\(terminal\|devices\)/);
});

test("quest completion summaries use the same rich feature-link opener", () => {
  const panels = readFileSync(new URL("../components/world/panels.tsx", import.meta.url), "utf8");
  const room = readFileSync(new URL("../components/world/quest-room.tsx", import.meta.url), "utf8");
  assert.match(panels, /<RichText text=\{str\(d\.summary\)\} handles=\{handles\} onOpenAgent=\{onOpenAgent\} className="mt-1" \/>/);
  assert.match(panels, /<ReportCard key=\{j\} card=\{card\} handles=\{handles\} onOpenAgent=\{onOpenAgent\} \/>/);
  assert.match(room, /function deliveredCompletionEvents\(messages: AgentMessage\[\], events: WorldEvent\[\], taskId: string\)/);
  assert.match(room, /message\.author_kind === "system"[\s\S]*meta\.status !== "started"[\s\S]*meta\.task_id === taskId/);
  assert.match(room, /message\.meta\?\.session_id !== eventSessionId \|\| message\.body !== plainDashes\(str\(report\.summary\)\)/);
  assert.match(room, /deliveredSummary \? \{ \.\.\.report, summary: "" \} : report/);
  assert.match(room, /eventCards\(e, entry\.deliveredSummary\)/);
  assert.match(room, /<ReportCard card=\{card\} handles=\{handles\} onOpenAgent=\{onOpenAgent\} \/>/);
});

test("workspace chat cards carry their focused item into exact navigation", () => {
  const game = readFileSync(new URL("../components/world/world-game.tsx", import.meta.url), "utf8");
  assert.match(game, /navigateRef\.current\(\{ kind: "report", id, item: detail\?\.item \?\? null, workspace: true \}\)/);
  assert.match(game, /item: validFeatureItem\(item\) \? item : null/);
});
