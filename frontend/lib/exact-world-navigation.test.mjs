import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";
import { applyNavigationDestination, consumeMessageFocus, conversationIsForeground, foregroundWindowLayer, guardedDetailLoader, raiseWindowLayer, navigationRequests, resolveMessageJump, resolveExactDestination, revealExactMap, runNavigation } from "./exact-world-navigation.ts";

function calledName(expression) {
  if (ts.isIdentifier(expression)) return expression.text;
  if (!ts.isPropertyAccessExpression(expression)) return null;
  const owner = calledName(expression.expression);
  return owner ? `${owner}.${expression.name.text}` : expression.name.text;
}

function directCallbackSink(fn) {
  let node = fn;
  const properties = [];
  while (node.parent) {
    const parent = node.parent;
    if (ts.isPropertyAssignment(parent) && parent.initializer === node) {
      if (ts.isIdentifier(parent.name) || ts.isStringLiteral(parent.name)) properties.push(parent.name.text);
      node = parent.parent;
      continue;
    }
    if (ts.isObjectLiteralExpression(parent) || ts.isArrayLiteralExpression(parent) || ts.isParenthesizedExpression(parent)) {
      node = parent;
      continue;
    }
    if (ts.isCallExpression(parent) && parent.arguments.includes(node)) {
      return { call: parent, name: calledName(parent.expression), properties };
    }
    return null;
  }
  return null;
}

function isSurfaceNavigation(sink) {
  if (sink.name !== "navigate" && sink.name !== "navigateRef.current") return false;
  if (!sink.properties.includes("show")) return false;
  const target = sink.call.arguments[0];
  if (!target || !ts.isObjectLiteralExpression(target)) return false;
  return target.properties.some((property) => ts.isPropertyAssignment(property)
    && (ts.isIdentifier(property.name) || ts.isStringLiteral(property.name))
    && property.name.text === "kind"
    && ts.isStringLiteral(property.initializer)
    && property.initializer.text === "surface");
}

function directValueSink(reference) {
  let node = reference;
  while (node.parent) {
    const parent = node.parent;
    if ((ts.isPropertyAssignment(parent) && parent.initializer === node)
      || ts.isObjectLiteralExpression(parent)
      || ts.isArrayLiteralExpression(parent)
      || ts.isParenthesizedExpression(parent)) {
      node = parent;
      continue;
    }
    if (ts.isCallExpression(parent) && parent.arguments.includes(node)) return calledName(parent.expression);
    return null;
  }
  return null;
}

function isHookDependency(reference) {
  const dependencies = reference.parent;
  if (!ts.isArrayLiteralExpression(dependencies) || !dependencies.elements.includes(reference)) return false;
  const call = dependencies.parent;
  if (!ts.isCallExpression(call) || call.arguments[1] !== dependencies) return false;
  return ["useCallback", "useEffect", "useLayoutEffect", "useMemo", "useForegroundAction"].includes(calledName(call.expression));
}

function foregroundBoundaryViolations(source) {
  const file = ts.createSourceFile("world-game.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const setters = new Set();
  const declarationNames = new Set();
  const visitDeclarations = (node) => {
    if (ts.isVariableDeclaration(node) && node.initializer && ts.isCallExpression(node.initializer)) {
      const hook = calledName(node.initializer.expression);
      if (hook === "useForegroundState" && ts.isArrayBindingPattern(node.name)) {
        const setter = node.name.elements[1];
        if (setter && ts.isBindingElement(setter) && ts.isIdentifier(setter.name)) {
          setters.add(setter.name.text);
          declarationNames.add(setter.name);
        }
      } else if (hook === "useForegroundAction" && ts.isIdentifier(node.name)) {
        setters.add(node.name.text);
        declarationNames.add(node.name);
      }
    }
    ts.forEachChild(node, visitDeclarations);
  };
  visitDeclarations(file);
  const violations = [];
  const visitReferences = (node) => {
    if (ts.isIdentifier(node) && setters.has(node.text) && !declarationNames.has(node)) {
      let current = node;
      let approved = false;
      if (ts.isCallExpression(node.parent) && node.parent.expression === node) {
        while (current.parent) {
          const parent = current.parent;
          if (ts.isFunctionLike(parent)) {
            if (parent.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.AsyncKeyword)) break;
            const sink = directCallbackSink(parent);
            if (!sink) break;
            if (["commitForeground", "showAuxiliary", "navigateSurface", "applyNavigationDestination", "useForegroundAction"].includes(sink.name) || isSurfaceNavigation(sink)) approved = true;
            break;
          }
          current = parent;
        }
      }
      if (!approved && (directValueSink(node) === "applyNavigationDestination" || isHookDependency(node))) approved = true;
      if (!approved) {
        const location = file.getLineAndCharacterOfPosition(node.getStart(file));
        violations.push(`${node.text}:${location.line + 1}`);
      }
    }
    ts.forEachChild(node, visitReferences);
  };
  visitReferences(file);
  return { protectedCount: setters.size, violations };
}

function facadeRegistrations(source, facadeName) {
  const file = ts.createSourceFile("navigation-details.ts", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const imports = new Set();
  const collectImports = (node) => {
    if (ts.isImportSpecifier(node)) imports.add(node.name.text);
    ts.forEachChild(node, collectImports);
  };
  collectImports(file);
  const registrations = new Set();
  const visit = (node, insideFacade = false) => {
    const inFacade = insideFacade || (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === facadeName);
    if (inFacade && ts.isPropertyAccessExpression(node) && ts.isIdentifier(node.expression) && node.expression.text.endsWith("Api")) {
      registrations.add(`${node.expression.text}.${node.name.text}`);
    }
    if (inFacade && ts.isCallExpression(node) && ts.isIdentifier(node.expression) && imports.has(node.expression.text)) registrations.add(node.expression.text);
    ts.forEachChild(node, (child) => visit(child, inFacade));
  };
  visit(file);
  return registrations;
}

function namedFunction(file, name) {
  let found;
  const visit = (node) => {
    if (found) return;
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node;
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === name && node.initializer) {
      if (ts.isFunctionLike(node.initializer)) found = node.initializer;
      if (ts.isCallExpression(node.initializer) && node.initializer.arguments[0] && ts.isFunctionLike(node.initializer.arguments[0])) found = node.initializer.arguments[0];
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return found;
}

function detailBoundaryViolations(source, names, registrations, excluded = []) {
  const file = ts.createSourceFile("detail-reader.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owners = new Set([...registrations].filter((registration) => registration.includes(".")).map((registration) => registration.split(".")[0]));
  const functions = new Set([...registrations].filter((registration) => !registration.includes(".")));
  const aliases = new Map([...owners].map((owner) => [owner, owner]));
  const functionAliases = new Map([...functions].map((fn) => [fn, fn]));
  const collectAliases = (node) => {
    if (ts.isImportSpecifier(node)) {
      const imported = node.propertyName?.text ?? node.name.text;
      if (owners.has(imported)) aliases.set(node.name.text, imported);
      if (functions.has(imported)) functionAliases.set(node.name.text, imported);
    }
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer && ts.isIdentifier(node.initializer)) {
      const owner = aliases.get(node.initializer.text);
      if (owner) aliases.set(node.name.text, owner);
      const fn = functionAliases.get(node.initializer.text);
      if (fn) functionAliases.set(node.name.text, fn);
    }
    ts.forEachChild(node, collectAliases);
  };
  collectAliases(file);
  const violations = [];
  const excludedNames = new Set(excluded);
  const inspect = (root) => {
    const visit = (node) => {
      if (ts.isFunctionDeclaration(node) && node.name && excludedNames.has(node.name.text)) return;
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && excludedNames.has(node.name.text)) return;
      let registration;
      if (ts.isPropertyAccessExpression(node) && ts.isIdentifier(node.expression)) registration = `${aliases.get(node.expression.text) ?? node.expression.text}.${node.name.text}`;
      if (ts.isElementAccessExpression(node) && ts.isIdentifier(node.expression) && ts.isStringLiteral(node.argumentExpression)) registration = `${aliases.get(node.expression.text) ?? node.expression.text}.${node.argumentExpression.text}`;
      if (registration && registrations.has(registration)) {
        const location = file.getLineAndCharacterOfPosition(node.getStart(file));
        violations.push(`${registration}:${location.line + 1}`);
      }
      if (ts.isIdentifier(node) && functionAliases.has(node.text)
        && !ts.isImportSpecifier(node.parent)
        && !(ts.isVariableDeclaration(node.parent) && node.parent.name === node)
        && !(ts.isPropertyAccessExpression(node.parent) && node.parent.name === node)) {
        const location = file.getLineAndCharacterOfPosition(node.getStart(file));
        violations.push(`${functionAliases.get(node.text)}:${location.line + 1}`);
      }
      if (ts.isVariableDeclaration(node) && ts.isObjectBindingPattern(node.name) && ts.isIdentifier(node.initializer)) {
        const owner = aliases.get(node.initializer.text) ?? node.initializer.text;
        for (const element of node.name.elements) {
          const member = element.propertyName && (ts.isIdentifier(element.propertyName) || ts.isStringLiteral(element.propertyName))
            ? element.propertyName.text
            : ts.isIdentifier(element.name) ? element.name.text : null;
          const candidate = member ? `${owner}.${member}` : null;
          if (candidate && registrations.has(candidate)) {
            const location = file.getLineAndCharacterOfPosition(element.getStart(file));
            violations.push(`${candidate}:${location.line + 1}`);
          }
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(root);
  };
  if (names === null) inspect(file);
  else for (const name of names) {
      const root = namedFunction(file, name);
      assert.ok(root, `missing ${name}`);
      inspect(root);
    }
  return violations;
}

test("guarded detail reads suppress stale results and errors", async () => {
  const loader = guardedDetailLoader();
  const firstValue = Promise.withResolvers();
  const first = loader.begin();
  const pendingValue = first.read(() => firstValue.promise);
  const second = loader.begin();
  firstValue.resolve("old");
  assert.deepEqual(await pendingValue, { status: "stale" });
  assert.deepEqual(await second.read(async () => "new"), { status: "ready", value: "new" });

  const oldError = Promise.withResolvers();
  const errorIntent = loader.begin();
  const pendingError = errorIntent.read(() => oldError.promise);
  loader.invalidate();
  oldError.reject(new Error("old"));
  assert.deepEqual(await pendingError, { status: "stale" });
});

test("conversation foreground follows the top surface, viewport and browser visibility", () => {
  const desktop = { documentVisible: true, top: "chat", wide: true };
  assert.equal(conversationIsForeground(desktop, "chat"), true);
  assert.equal(conversationIsForeground({ ...desktop, mobileList: true }, "chat"), false);
  assert.equal(conversationIsForeground({ ...desktop, documentVisible: false }, "chat"), false);
  assert.equal(conversationIsForeground({ ...desktop, top: "agent:juniper" }, "agent"), true);
  assert.equal(conversationIsForeground({ ...desktop, top: "reports", sideMounted: true }, "side"), true);
  assert.equal(conversationIsForeground({ ...desktop, top: "reports", sideMounted: true, wide: false }, "side"), false);
  assert.equal(conversationIsForeground({ ...desktop, top: "covered", sideMounted: true }, "side"), false);
  assert.equal(conversationIsForeground({ ...desktop, top: "workbench", workbenchOpen: true }, "workbench"), true);
  assert.equal(conversationIsForeground({ ...desktop, top: "workbench", workbenchOpen: true }, "agent"), false);
});

test("report readers keep the newest intent across A to B to A and rapid revision picks", async () => {
  const reports = guardedDetailLoader();
  let selected = "a";
  const firstAValue = Promise.withResolvers();
  const firstA = reports.begin(() => selected === "a");
  const pendingFirstA = firstA.read(() => firstAValue.promise);
  selected = "b";
  const b = reports.begin(() => selected === "b");
  assert.deepEqual(await b.read(async () => "b"), { status: "ready", value: "b" });
  selected = "a";
  const secondA = reports.begin(() => selected === "a");
  assert.deepEqual(await secondA.read(async () => "new-a"), { status: "ready", value: "new-a" });
  firstAValue.resolve("old-a");
  assert.deepEqual(await pendingFirstA, { status: "stale" });

  const revisions = guardedDetailLoader();
  const oldRevisionValue = Promise.withResolvers();
  const oldRevision = revisions.begin(() => selected === "a");
  const pendingOldRevision = oldRevision.read(() => oldRevisionValue.promise);
  const newRevision = revisions.begin(() => selected === "a");
  assert.deepEqual(await newRevision.read(async () => 3), { status: "ready", value: 3 });
  oldRevisionValue.resolve(2);
  assert.deepEqual(await pendingOldRevision, { status: "stale" });
});

test("controlled detail readers use the guarded API facade", () => {
  const facade = readFileSync(new URL("./navigation-details.ts", import.meta.url), "utf8");
  const registrations = facadeRegistrations(facade, "navigationDetails");
  const collections = facadeRegistrations(facade, "navigationCollections");
  assert.ok(registrations.size > 0);
  assert.ok(collections.size > 0);
  const world = readFileSync(new URL("../components/world/world-game.tsx", import.meta.url), "utf8");
  const reports = readFileSync(new URL("../components/world/reports-ui.tsx", import.meta.url), "utf8");
  const lists = readFileSync(new URL("../components/world/items-ui.tsx", import.meta.url), "utf8");
  const chat = readFileSync(new URL("../components/world/chat.tsx", import.meta.url), "utf8");
  const quest = readFileSync(new URL("../components/world/quest-room.tsx", import.meta.url), "utf8");
  const thread = readFileSync(new URL("../components/world/thread-panel.tsx", import.meta.url), "utf8");
  assert.deepEqual(detailBoundaryViolations(world, null, registrations), []);
  assert.deepEqual(detailBoundaryViolations(reports, null, registrations), []);
  assert.deepEqual(detailBoundaryViolations(lists, null, registrations), []);
  assert.deepEqual(detailBoundaryViolations(chat, null, registrations), []);
  assert.deepEqual(detailBoundaryViolations(quest, null, registrations), []);
  assert.deepEqual(detailBoundaryViolations(thread, null, registrations), []);
  assert.deepEqual(detailBoundaryViolations(world, null, collections, ["loadChatUnread"]), []);
});

test("detail boundary rejects direct, awaited, aliased and destructured reads", () => {
  const facade = readFileSync(new URL("./navigation-details.ts", import.meta.url), "utf8");
  const registrations = facadeRegistrations(facade, "navigationDetails");
  for (const bypass of [
    `async function navigate() { await reportsApi.get("report"); }`,
    `function navigate() { const load = reportsApi.get; return load("report"); }`,
    `async function navigate() { const { get: load } = reportsApi; return await load("report"); }`,
    `async function navigate() { return await reportsApi["get"]("report"); }`,
    `import { reportsApi as api } from "./reports"; async function navigate() { return await api.get("report"); }`,
    `async function navigate() { const api = reportsApi; return await api.get("report"); }`,
    `const load = () => reportsApi.get("report");`,
    `import { resolveMessage as read } from "../components/world/message-links"; const load = () => read("msg");`,
  ]) assert.ok(detailBoundaryViolations(bypass, null, registrations).length >= 1);
});

test("collection boundary rejects a module-level controller bypass", () => {
  const facade = readFileSync(new URL("./navigation-details.ts", import.meta.url), "utf8");
  const collections = facadeRegistrations(facade, "navigationCollections");
  assert.ok(detailBoundaryViolations(`const refresh = () => worldApi.channels();`, null, collections).length >= 1);
});

test("foreground state changes stay inside navigation controller sinks", () => {
  const source = readFileSync(new URL("../components/world/world-game.tsx", import.meta.url), "utf8");
  const result = foregroundBoundaryViolations(source);
  assert.ok(result.protectedCount > 0);
  assert.deepEqual(result.violations, []);
});

test("foreground boundary rejects escaped handlers and delayed completions", () => {
  const valid = `
    const useForegroundState = useState;
    const useForegroundAction = useCallback;
    const [open, setOpen] = useForegroundState(false);
    const [layer, setLayer] = useForegroundState("map");
    const raise = useForegroundAction(() => setLayer("agent"), []);
    commitForeground(() => setOpen(true));
    commitForeground(() => raise());
    navigateSurface(() => setOpen(true));
    applyNavigationDestination(target, current, { open: () => setOpen(true) });
    navigate({ kind: "surface", show: () => setOpen(true) });
    navigateRef.current({ kind: "surface", show: () => setOpen(true) });
    useCallback(() => {}, [raise]);
  `;
  assert.deepEqual(foregroundBoundaryViolations(valid), { protectedCount: 3, violations: [] });
  for (const invalid of [
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); <Button onClick={() => setOpen(true)} />;`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); <Dialog onOpen={setOpen} />;`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); async function completed() { await save(); setOpen(true); }`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); commitForeground(() => wait.then(setOpen));`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); commitForeground(() => setTimeout(setOpen, 0));`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); commitForeground(() => wait.then(() => setOpen(true)));`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); commitForeground(() => { function later() { setOpen(true); } later(); });`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); navigate({ kind: "surface", show: async () => setOpen(true) });`,
    `const useForegroundState = useState; const [open, setOpen] = useForegroundState(false); navigate({ kind: "agent", show: () => setOpen(true) });`,
    `const useForegroundAction = useCallback; const raise = useForegroundAction(() => setLayer("agent"), []); <Button onClick={raise} />;`,
    `const useForegroundAction = useCallback; const raise = useForegroundAction(() => setLayer("agent"), []); useEffect(raise, []);`,
  ]) assert.equal(foregroundBoundaryViolations(invalid).violations.length, 1);
});

const home = "home";
const away = "away";
const region = (id) => ({ region: { id, name: id } });
const item = (id, kind, region_id = null) => ({ id, kind, region_id, name: id });
const reader = (row) => ({
  home,
  team: async () => ({ id: "team" }),
  item: async () => row,
  region: async (id) => region(id.toLowerCase()),
});

function openExactDestination(link, read, current, actions) {
  return runNavigation(current, () => resolveExactDestination(link, read, current), actions.closePanels, (destination) => {
    actions.updateLink(destination.kind, destination.id);
    if (destination.kind === "team") actions.showTeam();
    else if (destination.kind === "list") actions.showList();
    else if (destination.map) actions.showMap(destination.map);
  }, actions.unavailable);
}

const destinations = [
  { kind: "team", id: "team" },
  { kind: "list", id: "checklist", row: item("checklist", "list") },
  { kind: "list", id: "reminder", row: item("reminder", "timer") },
  { kind: "region", id: home, regionId: home },
  { kind: "region", id: away, regionId: away },
  { kind: "place", id: "building", row: item("building", "building"), regionId: home },
  { kind: "place", id: "npc", row: item("npc", "npc", away), regionId: away },
];

const startingViews = [
  { name: "desktop baseline", current: home },
  { name: "room interior", current: home, room: "room" },
  { name: "saved Quick Chat", current: home, panel: "quick" },
  { name: "open report and side chat", current: home, panel: "report" },
  { name: "Quests tab in an already-mounted Board", current: home, panel: "quests" },
  { name: "another region", current: away },
  { name: "mobile interior", current: home, room: "room", panel: "chat" },
];

for (const destination of destinations) {
  for (const start of startingViews) {
    test(`${destination.id} from ${start.name}`, async () => {
      const steps = [];
      let current = start.current;
      let room = start.room;
      let panel = start.panel;
      const engine = {
        currentPlace: () => room,
        leaveRoom(instant) {
          assert.equal(instant, true);
          steps.push("leave");
          room = null;
        },
      };
      const reveal = (place) => steps.push(place?.id ?? "outdoors");
      const travel = (target) => {
        steps.push(`travel:${target.regionId}`);
        current = target.regionId;
        room = null;
        revealExactMap(target, current, engine, travel, reveal);
      };
      await openExactDestination(
        { kind: destination.kind, id: destination.id.toUpperCase() },
        reader(destination.row),
        () => true,
        {
          closePanels: () => { steps.push("close"); panel = null; },
          updateLink: (kind, id) => steps.push(`${kind}:${id}`),
          showTeam: () => { steps.push("teams"); panel = "teams"; },
          showList: () => { steps.push("lists"); panel = "lists"; },
          showMap: (target) => revealExactMap(target, current, engine, travel, reveal),
          unavailable: () => assert.fail("valid destination"),
        },
      );
      const expected = ["close", `${destination.kind}:${destination.id}`];
      if (destination.regionId) {
        if (destination.regionId !== start.current) expected.push(`travel:${destination.regionId}`);
        else if (start.room) expected.push("leave");
        expected.push(destination.row?.id ?? "outdoors");
        assert.equal(panel, null);
      } else {
        expected.push(destination.kind === "team" ? "teams" : "lists");
        assert.equal(room, start.room);
      }
      assert.deepEqual(steps, expected);
    });
  }
}

const unexpectedNavigation = {
  closePanels: () => assert.fail("closed the current panel"),
  updateLink: () => assert.fail("changed the current URL"),
  showTeam: () => assert.fail("opened Teams"),
  showList: () => assert.fail("opened Lists"),
  showMap: () => assert.fail("changed the map"),
  unavailable: () => assert.fail("announced a superseded result"),
};

test("a panel that refuses to close keeps its view and URL", async () => {
  await openExactDestination({ kind: "team", id: "team" }, reader(), () => true, {
    ...unexpectedNavigation,
    closePanels: () => false,
  });
});

for (const destination of destinations) {
  test(`a superseded ${destination.id} response keeps the current view`, async () => {
    let current = true;
    const pending = openExactDestination(
      destination,
      reader(destination.row),
      () => current,
      unexpectedNavigation,
    );
    current = false;
    await pending;
  });
}

test("a list response held until after opening a report cannot navigate", async () => {
  let selection = "list";
  const list = Promise.withResolvers();
  const pending = openExactDestination(
    { kind: "list", id: "checklist" },
    { ...reader(), item: () => list.promise },
    () => selection === "list",
    unexpectedNavigation,
  );
  selection = "report";
  list.resolve(item("checklist", "list"));
  await pending;
});

for (const [link, read] of [
  [{ kind: "team", id: "hidden" }, { ...reader(), team: async () => { throw new Error("404"); } }],
  [{ kind: "list", id: "wrong-kind" }, reader(item("wrong-kind", "building"))],
  [{ kind: "place", id: "wrong-kind" }, reader(item("wrong-kind", "list"))],
  [{ kind: "place", id: "closed-road" }, { ...reader(item("closed-road", "building", away)), region: async () => { throw new Error("404"); } }],
  [{ kind: "region", id: "hidden" }, { ...reader(), region: async () => { throw new Error("404"); } }],
]) {
  test(`${link.kind} validation failure keeps the current view and URL`, async () => {
    let unavailable = 0;
    await openExactDestination(link, read, () => true, {
      ...unexpectedNavigation,
      unavailable: () => { unavailable += 1; },
    });
    assert.equal(unavailable, 1);
  });
}

const foregroundDestinations = [
  { name: "agent", application: { kind: "agent" }, panel: "agent" },
  { name: "quest", application: { kind: "quest" }, panel: "history" },
  { name: "report", application: { kind: "report" }, panel: "reports", keepSideChat: true },
  { name: "canvas", application: { kind: "canvas" }, panel: "reports", keepSideChat: true },
  { name: "team", application: { kind: "team" }, panel: "board", board: "teams", closeTerminal: true },
  { name: "list", application: { kind: "list" }, panel: "lists", keepSideChat: true, closeTerminal: true },
  { name: "to-do", application: { kind: "todo" }, panel: "lists", keepSideChat: true },
  { name: "person", application: { kind: "person" }, panel: "people" },
  { name: "channel", application: { kind: "channel" }, panel: "chat" },
  { name: "channel message", application: { kind: "message" }, options: { messageTarget: "channel" }, panel: "chat" },
  { name: "agent message", application: { kind: "message" }, options: { messageTarget: "agent" }, panel: "agent", policyPanel: "chat" },
  { name: "reply", application: { kind: "message" }, options: { messageTarget: "channel", threadedMessage: true }, panel: "chat" },
  { name: "region", application: { kind: "region" }, panel: "map", leaveInterior: true, closeTerminal: true },
  { name: "place", application: { kind: "place" }, panel: "map", leaveInterior: true, closeTerminal: true },
  { name: "practice", application: { kind: "practice" }, panel: "academy", keepSideChat: true },
  { name: "lesson", application: { kind: "lesson" }, panel: "academy", keepSideChat: true },
  { name: "academy", application: { kind: "academy" }, panel: "academy", keepSideChat: true },
];

const foregroundStarts = [
  { name: "world map", panel: "map" },
  { name: "building interior", panel: "map", interior: "library" },
  { name: "Quick Chat", panel: "quick" },
  { name: "side panel", panel: "reports", sideChat: true },
  { name: "Reports", panel: "reports" },
  { name: "wrong board tab", panel: "board", board: "quests" },
  { name: "organization switcher", panel: "map", overlay: "org" },
  { name: "Settings", panel: "settings", overlay: "settings" },
  { name: "another region", panel: "map", region: away },
  { name: "mobile width", panel: "chat", viewport: 390, sideChat: true },
  { name: "Folder Workshop", panel: "workshop", overlay: "workshop" },
  { name: "Changelog", panel: "changelog", overlay: "changelog" },
  { name: "mounted terminal", panel: "terminal", terminal: true },
];

function foregroundActions(state, mapKind) {
  const show = (panel) => { state.panel = panel; };
  return {
    closePanels: () => {
      state.closed = (state.closed ?? 0) + 1;
      state.panel = "map";
      state.overlay = null;
      state.board = null;
    },
    showSideChat: () => { state.sideChat = true; },
    closeTerminal: () => { state.terminal = false; },
    showBoard: (board) => { state.board = board; },
    prepare: () => { state.prepared += 1; },
    surface: () => show("surface"),
    agent: () => show("agent"),
    quest: () => show("history"),
    reports: () => {
      state.overlay = null;
      show("reports");
    },
    academy: () => show("academy"),
    todo: () => show("lists"),
    person: () => show("people"),
    channel: () => show("chat"),
    chatAgent: () => show("chat"),
    messageAgent: () => show("agent"),
    messageChannel: () => show("chat"),
    message: () => { state.messageApplied += 1; },
    team: () => show("board"),
    list: () => show("lists"),
    map: () => {
      const target = { regionId: home, region: null, place: mapKind === "place" ? item("building", "building") : null, label: "Home" };
      const engine = {
        currentPlace: () => state.interior,
        leaveRoom: () => { state.interior = null; },
      };
      const reveal = () => show("map");
      const travel = (next) => {
        state.region = next.regionId;
        state.interior = null;
        revealExactMap(next, state.region, engine, travel, reveal);
      };
      revealExactMap(target, state.region, engine, travel, reveal);
    },
  };
}

for (const destination of foregroundDestinations) {
  for (const start of foregroundStarts) {
    test(`${destination.name} production navigation from ${start.name}`, async () => {
      const state = {
        panel: start.panel,
        overlay: start.overlay ?? null,
        interior: start.interior ?? null,
        region: start.region ?? home,
        board: start.board ?? null,
        sideChat: Boolean(start.sideChat),
        terminal: Boolean(start.terminal),
        closed: 0,
        prepared: 0,
        messageApplied: 0,
      };
      const requests = navigationRequests();
      const request = requests.begin();
      let transition;
      await runNavigation(request.current, () => destination.application, () => true, (application) => {
        transition = applyNavigationDestination(application, {
          wide: start.viewport !== 390,
          sideChatSource: Boolean(start.sideChat),
        }, foregroundActions(state, destination.application.kind), {
          ...destination.options,
        });
      }, () => assert.fail("available destination"));
      assert.equal(transition.panel, destination.policyPanel ?? destination.panel);
      assert.equal(transition.board, destination.board);
      assert.equal(transition.keepConversations, transition.panel !== "map");
      assert.equal(transition.leaveInterior, destination.leaveInterior);
      assert.equal(transition.showSideChat, Boolean(destination.keepSideChat && start.sideChat && start.viewport !== 390));
      assert.equal(transition.closeTerminal, Boolean(destination.closeTerminal));
      assert.equal(transition.overlay, destination.application.kind === "report" || destination.application.kind === "canvas" ? true : undefined);
      assert.equal(state.panel, destination.panel);
      assert.equal(state.overlay, null);
      assert.equal(state.interior, transition.leaveInterior ? null : start.interior ?? null);
      assert.equal(state.region, transition.leaveInterior ? home : start.region ?? home);
      assert.equal(state.board, transition.overlay ? start.board ?? null : destination.board ?? null);
      assert.equal(state.sideChat, Boolean(start.sideChat));
      assert.equal(state.terminal, transition.closeTerminal ? false : Boolean(start.terminal));
      assert.equal(state.closed, transition.overlay ? 0 : 1);
      assert.equal(state.prepared, transition.overlay ? 0 : 1);
      assert.equal(state.messageApplied, destination.application.kind === "message" ? 1 : 0);
    });
  }
}

for (const underlying of [
  { name: "Lists", panel: "lists", list: "weekly" },
  { name: "Quest Board", panel: "board", board: "quests" },
  { name: "selected quest", panel: "history", quest: "quest-1" },
]) {
  test(`Reports overlays ${underlying.name} without replacing its state`, () => {
    const state = { ...underlying, sideChat: true, visible: underlying.panel };
    let sideChatPrepared = 0;
    const transition = applyNavigationDestination({ kind: "report" }, {
      wide: true,
      sideChatSource: true,
    }, {
      ...foregroundActions(state, "report"),
      closePanels: () => assert.fail("replaced the underlying panel"),
      prepare: () => assert.fail("ran destructive foreground preparation"),
      showSideChat: () => { sideChatPrepared += 1; },
      reports: () => { state.visible = "reports"; },
    });
    assert.equal(transition.overlay, true);
    assert.equal(state.visible, "reports");
    assert.equal(state.panel, underlying.panel);
    assert.equal(state.list, underlying.list);
    assert.equal(state.board, underlying.board);
    assert.equal(state.quest, underlying.quest);
    assert.equal(sideChatPrepared, 1);
  });
}

test("a loaded list keeps the mounted terminal", async () => {
  const state = { panel: "terminal", overlay: null, interior: null, region: home, board: null, sideChat: false, terminal: true, prepared: 0, messageApplied: 0 };
  const requests = navigationRequests();
  const request = requests.begin();
  let transition;
  await runNavigation(request.current, () => ({ kind: "list" }), () => true, (destination) => {
    transition = applyNavigationDestination(destination, {
    wide: true,
    sideChatSource: false,
    }, foregroundActions(state, "list"), { loadedList: true });
  }, () => assert.fail("loaded list"));
  assert.equal(transition.closeTerminal, false);
  assert.equal(state.terminal, true);
  assert.equal(state.panel, "lists");
});

test("every repeated selection gets a new request", () => {
  const requests = navigationRequests();
  const first = requests.begin();
  const second = requests.begin();
  assert.equal(first.request, 1);
  assert.equal(second.request, 2);
  assert.equal(first.current(), false);
  assert.equal(second.current(), true);
});

test("an explicit Chat selection wins against a delayed checklist read", async () => {
  const requests = navigationRequests();
  const checklist = Promise.withResolvers();
  const listRequest = requests.begin();
  const visible = [];
  const pending = runNavigation(listRequest.current,
    () => resolveExactDestination({ kind: "list", id: "checklist" }, { ...reader(), item: () => checklist.promise }, listRequest.current),
    () => true, (target) => visible.push(target.id), () => assert.fail("superseded lookup"));
  const chatRequest = requests.begin();
  await runNavigation(chatRequest.current, () => ({ id: "channel" }), () => true, (target) => visible.push(target.id), () => assert.fail("existing channel"));
  checklist.resolve(item("checklist", "list"));
  await pending;
  assert.deepEqual(visible, ["channel"]);
});

test("cancelling invalidates the pending request", () => {
  const requests = navigationRequests();
  const pending = requests.begin();
  assert.equal(requests.current(pending.request), true);
  requests.cancel();
  assert.equal(pending.current(), false);
  assert.equal(requests.current(pending.request), false);
});

test("an exact uppercase team id resolves to its canonical id", async () => {
  const requests = navigationRequests();
  const request = requests.begin();
  const resolved = await resolveExactDestination(
    { kind: "team", id: "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE" },
    { ...reader(), team: async () => ({ id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee" }) },
    request.current,
  );
  assert.equal(resolved?.id, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
});

test("a query matching only a reply opens its root and targets that exact reply", async () => {
  const rows = [{ id: 18, body: "root" }, { id: 20, reply_to: 18, body: "replyonlycobalt" }];
  const answers = rows.filter(({ body }) => body.includes("replyonlycobalt"));
  assert.deepEqual(answers.map(({ id }) => id), [20]);
  const jump = await resolveMessageJump("launch", answers[0].id, async (channel, id) => {
    assert.equal(channel, "launch");
    assert.equal(id, 20);
    return rows;
  });
  assert.deepEqual(jump, { thread: { root: 18, reply: 20 } });
});

test("a top-level message outside loaded history remains the exact target", async () => {
  const message = { id: 18, body: "root" };
  assert.deepEqual(await resolveMessageJump("launch", 18, async () => [message]), { message });
  await assert.rejects(resolveMessageJump("launch", 19, async () => [message]));
});

test("a declined or superseded leave guard keeps the original view and URL", async () => {
  for (const allowed of [false, true]) {
    const requests = navigationRequests();
    const request = requests.begin();
    const guard = Promise.withResolvers();
    const state = { panel: "editing report", overlay: null, interior: null, region: home, board: null, sideChat: false, terminal: false, prepared: 0, messageApplied: 0 };
    let url = "?report=draft";
    const pending = runNavigation(request.current, () => ({ kind: "list", id: "checklist" }),
      () => guard.promise, (target) => {
        applyNavigationDestination(target, { wide: true, sideChatSource: false }, {
          ...foregroundActions(state, "list"),
          list: (destination) => {
            state.panel = destination.id;
            url = `?list=${destination.id}`;
          },
        });
      }, () => assert.fail("valid checklist"));
    await Promise.resolve();
    if (allowed) requests.begin();
    guard.resolve(allowed);
    await pending;
    assert.equal(state.panel, "editing report");
    assert.equal(url, "?report=draft");
  }
});

test("closing nested window layers restores their most recent foreground order", () => {
  const open = { terminal: true, agent: true, chat: true };
  const order = raiseWindowLayer(raiseWindowLayer(["terminal", "agent", "chat"], "terminal"), "agent");
  assert.equal(foregroundWindowLayer(order, open), "agent");
  assert.equal(foregroundWindowLayer(order, { ...open, agent: false }), "terminal");
  assert.equal(foregroundWindowLayer(order, { ...open, agent: false, terminal: false }), "chat");
  assert.equal(foregroundWindowLayer(order, { terminal: false, agent: false, chat: false }), undefined);
});

test("an exact reply focus is consumed once and the same reply may be selected again", () => {
  const last = { current: undefined };
  assert.equal(consumeMessageFocus(last, 2), true);
  assert.equal(consumeMessageFocus(last, 2), false);
  assert.equal(consumeMessageFocus(last, 3), true);
  assert.equal(consumeMessageFocus(last, 3), false);
});

test("a mounted pick takes over before the missing record lookup resolves", async () => {
  const requests = navigationRequests();
  const missing = Promise.withResolvers();
  const first = requests.begin();
  const shown = [];
  const pending = runNavigation(first.current, () => missing.promise, () => true, (id) => shown.push(id), () => {});
  const pick = requests.begin();
  await runNavigation(pick.current, () => "already-loaded", () => true, (id) => shown.push(id), () => {});
  assert.deepEqual(shown, ["already-loaded"]);
  missing.resolve("late-record");
  await pending;
  assert.deepEqual(shown, ["already-loaded"]);
});
