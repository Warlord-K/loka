(async () => {
  const batch = window.__lokaNavigationBatch ?? "core";
  const wait = (ms = 45) => new Promise((resolve) => setTimeout(resolve, ms));
  const until = async (find, description) => {
    for (let attempt = 0; attempt < 160; attempt += 1) {
      const value = find();
      if (value) return value;
      await wait();
    }
    throw new Error(description);
  };
  const useViewport = async (width, height) => {
    window.__lokaNavigationViewportRequest = { width, height };
    await until(() => Math.abs(innerWidth - width) <= 1 && Math.abs(innerHeight - height) <= 1, `the browser viewport did not reach ${width}x${height}`);
    window.__lokaNavigationViewportRequest = null;
  };
  const useKey = async (key, complete, description) => {
    window.__lokaNavigationKeyboardRequest = key;
    await until(complete, description);
    window.__lokaNavigationKeyboardRequest = null;
  };
  const visible = (selector) => [...document.querySelectorAll(selector)].filter((element) => element.getClientRects().length);
  const exactText = (selector, value) => visible(selector).find((element) => element.textContent.trim() === value);
  const modal = (name) => visible('[role="dialog"]').find((element) => element.getAttribute('aria-label') === name);
  const setValue = (field, value) => {
    const prototype = field instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, 'value').set.call(field, value);
    field.dispatchEvent(new Event('input', { bubbles: true }));
  };
  const openPalette = () => {
    const search = visible('button[aria-label="Search everything"]')[0];
    if (innerWidth <= 500 && search) search.click();
    else window.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', code: 'KeyK', metaKey: true, bubbles: true, cancelable: true }));
  };
  const paletteInput = async (value) => {
    const field = await until(() => visible('[cmdk-input]').find((element) => element.getAttribute('placeholder')?.startsWith('Search everything')), 'Cmd K input did not open');
    setValue(field, value);
  };
  const paletteHit = (value) => visible('[cmdk-item]').find((element) => element.dataset.value === value);
  const choose = async (query, value) => {
    openPalette();
    await paletteInput(query);
    const item = await until(() => paletteHit(value), `Cmd K result ${value} did not appear`);
    item.click();
    await until(() => !visible('[cmdk-input]').length, 'Cmd K did not close after selection');
  };
  const clickText = async (selector, value) => {
    const button = await until(() => exactText(selector, value), `${value} did not appear`);
    button.click();
    return button;
  };
  const run = async (name, check) => {
    try {
      await check();
      return [name, 'pass'];
    } catch (error) {
      return [name, error instanceof Error ? error.message : String(error)];
    }
  };
  const launch = 'c0000000-0000-0000-0000-000000000002';
  const report = 'f0000000-0000-0000-0000-000000000002';
  const otherReport = 'f0000000-0000-0000-0000-000000000001';
  const list = 'b0000000-0000-0000-0000-000000000002';
  const createdChannel = 'c0000000-0000-0000-0000-000000009001';
  const latestChannel = 'c0000000-0000-0000-0000-000000009002';
  const createdCanvas = 'f0000000-0000-0000-0000-000000009001';
  const createdList = 'b0000000-0000-0000-0000-000000009001';
  const savedReportBase = 'f0000000-0000-0000-0000-00000000910';
  const oldTodo = 'abcdef00-0000-0000-0000-000000000002';
  const academyLesson = 'feedca00-0000-0000-0000-000000000002';
  const mainAgent = 'a0000000-0000-0000-0000-000000000001';
  const selectedQuest = 'e0000000-0000-0000-0000-000000000003';
  const workshopSession = 'e9000000-0000-4000-8000-000000000001';
  const workshopFile = 'frontend/lib/feature-links.ts';
  const workspaceCanvas = 'f9000000-0000-4000-8000-000000000002';
  const workspaceItem = 'Focus_Note-7';
  const libraryCanvas = 'f9000000-0000-4000-8000-000000000003';
  const libraryItem = 'Library_Note-8';
  const exactTeam = 'abcdef12-3456-4789-abcd-ef1234567890';
  const fakeMessage = 9189901;
  const todoMessage = 9189902;
  const replyRoot = 9189903;
  const exactReply = 9189904;
  const forkCopyBase = 'a9000000-0000-0000-0001-';
  const savedQuick = 'd9000000-0000-0000-0000-000000000001';
  const heldQuick = 'd9000000-0000-0000-0000-000000000002';
  const currentQuick = 'd9000000-0000-0000-0000-000000000003';
  const mutationReportB = 'f9000000-0000-0000-0002-000000000001';
  const pinReport = 'f9000000-0000-0000-0002-000000000002';
  const deleteReport = 'f9000000-0000-0000-0002-000000000003';
  const restoreReport = 'f9000000-0000-0000-0002-000000000004';
  const detailReportA = 'f9000000-0000-0000-0003-000000000001';
  const detailReportB = 'f9000000-0000-0000-0003-000000000002';
  const detailAgentCardReport = 'f9000000-0000-0000-0003-000000000003';
  const exactCardAgent = 'a9000000-0000-0000-0003-000000000001';
  const originalFetch = window.fetch;
  const originalConfirm = window.confirm;
  const now = new Date().toISOString();
  const requestIdPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  const workspaceBatch = batch === 'workspace-link' || batch === 'workspace-completion';
  await until(() => {
    const gate = exactText('button', 'Continue in the browser anyway') ?? exactText('button', 'Start exploring');
    if (gate) {
      gate.click();
      return false;
    }
    return visible('button[aria-label="Search everything"]')[0];
  }, 'World did not finish its startup gates');
  const launchDetail = await originalFetch(`/api/world/channels/${launch}`).then((response) => response.json());
  const sourceReport = await originalFetch(`/api/world/reports/${report}`).then((response) => response.json());
  const sourceList = await originalFetch(`/api/world/items/${list}`).then((response) => response.json());
  const sourceAgents = batch === 'foreground' || batch === 'async-starts' || batch === 'detail-races' ? await originalFetch('/api/world/agents').then((response) => response.json()) : null;
  const sourceQuestRoom = batch === 'quest-catchup' || batch === 'workspace-completion' ? await originalFetch(`/api/world/tasks/${selectedQuest}/room`).then((response) => response.json()) : null;
  const messageAgent = batch === 'workspace-completion' ? sourceQuestRoom?.task.assigned_agent_id ?? mainAgent : mainAgent;
  const sourceAgentMessages = batch === 'agent-panel-races' || batch === 'visibility' || batch === 'feature-links' || batch === 'workspace-link' || batch === 'workspace-completion' ? await originalFetch(`/api/world/agents/${messageAgent}/messages?limit=200`).then((response) => response.json()) : null;
  const sourceTodo = batch === 'review' || batch === 'foreground' ? await originalFetch('/api/world/todos/90000000-0000-0000-0000-000000000004').then((response) => response.json()) : null;
  const sourceChannelMessages = batch === 'review' ? await originalFetch(`/api/world/channels/${launch}/messages`).then((response) => response.json()) : null;
  if (batch === 'detail-races') {
    openPalette();
    await paletteInput('Juniper');
    await until(() => paletteHit(`agent:${mainAgent}`), 'map roster did not finish loading before the detail races');
    visible('button[aria-label="Close command palette"]')[0]?.click();
    await until(() => !visible('[cmdk-input]').length, 'Cmd K did not close after the roster check');
  }
  const agedTodo = sourceTodo ? { ...sourceTodo, id: oldTodo, text: 'Aged exact to-do fixture', status: 'done', added_at: '2025-01-02T03:04:05.000Z', done_at: '2025-01-03T03:04:05.000Z', updated_at: '2025-01-03T03:04:05.000Z' } : null;
  const searchShape = { agents: [], messages: [], reports: [], quests: [], todos: [], lists: [], channels: [], schedules: [], feeds: [], teams: [], regions: [], places: [], rules: [], kit: [], devices: [], people: [] };
  const staleSearch = await originalFetch(`/api/world/search?q=${encodeURIComponent('Spring launch plan')}`).then((response) => response.json());
  const currentSearch = await originalFetch(`/api/world/search?q=${encodeURIComponent('launch')}`).then((response) => response.json());
  const stalePaletteResults = { ...searchShape, reports: (staleSearch.reports ?? []).filter((item) => item.id === report) };
  const currentPaletteResults = { ...searchShape, channels: currentSearch.channels.filter((item) => item.id === launch) };
  let channelGate;
  let canvasGate;
  let listGate;
  let messagesGate;
  let itemGate;
  let paletteGate;
  let academyReportGate;
  let reportCreateGate;
  let agentCreateGate;
  let tellGate;
  let forkGate;
  let quickStartGate;
  let reportMutationGate;
  let reportDetailGate;
  let revisionListGate;
  let revisionDetailGate;
  let exactAgentGate;
  let questPrefetchGate;
  let agentThreadGate;
  let markReads = [];
  let holdChannel = false;
  let holdCanvas = false;
  let holdList = false;
  let holdMessages = false;
  let holdItem = false;
  let channelHeld = false;
  let canvasHeld = false;
  let listHeld = false;
  let messagesHeld = false;
  let itemHeld = false;
  let paletteHeld = false;
  let academyReportHeld = false;
  let holdAcademyReport = false;
  let failItem = false;
  let reportCreateHeld = false;
  let tellHeld = false;
  let holdReportCreate = false;
  let holdAgentCreate = false;
  let holdTell = false;
  let holdFork = false;
  let holdQuickStart = false;
  let heldReportMutation = null;
  let holdReportDetail = null;
  let reportPosts = 0;
  let reportPatches = [];
  let oldTodoReads = 0;
  let todoPatches = 0;
  let failedItemResponses = 0;
  let savedReport;
  let createdCanvasReads = 0;
  let failCanvasResponse = false;
  const canvasRequestIds = [];
  let createdListReads = 0;
  let createdChannelReads = 0;
  let createdChannelPosts = 0;
  let tellPosts = 0;
  let forkHeld = false;
  let quickStartHeld = false;
  let forkReturns = 0;
  let forkRosterReads = 0;
  let reportMutationHeld = false;
  let reportDetailHeld = false;
  let revisionListHeld = false;
  let revisionDetailHeld = false;
  let exactAgentHeld = false;
  let holdRevisionList = null;
  let holdRevisionDetail = null;
  let holdExactAgent = false;
  let includeExactAgentInList = false;
  let agentCreateHeld = false;
  let agentCreateReturns = 0;
  let agentRosterReads = 0;
  let exactTeamReads = 0;
  let exactAgentReads = 0;
  let questPrefetchHeld = false;
  let questCatchupUrl = null;
  let agentMessageReads = 0;
  let agentSeenRequests = 0;
  let includeIncomingAgentMessage = false;
  let agentThreadReads = 0;
  let agentThreadHeld = false;
  let holdAgentThread = false;
  let workspaceAvailable = true;
  let workspaceDetailReads = 0;
  let workspaceLibraryDeleted = false;
  let workspaceSearchFailure = false;
  let holdWorkspaceSearch = false;
  let workspaceSearchHeld = false;
  let workspaceSearchGate;
  let workspaceCompletionPhase = 'event-first';
  let workspaceNotificationsCleared = false;
  const mutationDetailReads = new Map();
  let listCreated = false;
  const createdAgents = new Map();
  const createdQuicks = new Map();
  const createdChannels = new Map();
  const mutationReports = new Map([
    [mutationReportB, { ...sourceReport.report, id: mutationReportB, title: 'Mutation fixture B', slug: 'mutation-fixture-b', body: 'Selected report B must stay in front.', pinned: false, revision: 2, updated_at: now }],
    [pinReport, { ...sourceReport.report, id: pinReport, title: 'Pin mutation fixture A', slug: 'pin-mutation-fixture-a', body: 'Pin mutation fixture body.', pinned: false, revision: 2, updated_at: now }],
    [deleteReport, { ...sourceReport.report, id: deleteReport, title: 'Delete mutation fixture A', slug: 'delete-mutation-fixture-a', body: 'Delete mutation fixture body.', pinned: false, revision: 2, updated_at: now }],
    [restoreReport, { ...sourceReport.report, id: restoreReport, title: 'Restore mutation fixture A', slug: 'restore-mutation-fixture-a', body: 'Restore mutation fixture body.', pinned: false, revision: 2, updated_at: now }],
  ]);
  const detailReports = new Map([
    [detailReportA, { ...sourceReport.report, id: detailReportA, title: 'Detail fixture A', slug: 'detail-fixture-a', body: 'Current detail A body.', revision: 2, updated_at: now }],
    [detailReportB, { ...sourceReport.report, id: detailReportB, title: 'Detail fixture B', slug: 'detail-fixture-b', body: 'Current detail B body.', revision: 2, updated_at: now }],
    [detailAgentCardReport, { ...sourceReport.report, id: detailAgentCardReport, title: 'Agent card fixture', slug: 'agent-card-fixture', kind: 'canvas', body: '', canvas: { items: [{ id: 'agent-card', kind: 'ref', target: `agent:${exactCardAgent}`, x: 80, y: 80, w: 300, h: 140, by: 'person' }], edges: [] }, revision: 1, updated_at: now }],
  ]);
  const mutationBrief = (item) => ({ ...item, excerpt: item.body, size: item.body.length, open_marks: 0, item_count: 0 });
  const detailRevision = (item, revision) => ({ revision, title: revision === 1 ? `${item.title} old` : item.title, body: revision === 1 ? `Old ${item.title} body.` : item.body, canvas: null, folder: item.folder, tags: item.tags, author_agent_id: item.author_agent_id, author_user_id: item.author_user_id, created_at: revision === 1 ? item.created_at : item.updated_at });
  const fakeCanvas = { ...sourceReport.report, id: createdCanvas, title: 'Held create workspace', slug: 'held-create-workspace', kind: 'canvas', body: 'Workspace with 0 items and 0 arrows.', practice: null, canvas: { items: [], edges: [] }, links: [], created_at: now, updated_at: now };
  const workspaceReport = { ...sourceReport.report, id: workspaceCanvas, title: 'Shared workspace fixture', slug: 'shared-workspace-fixture', kind: 'canvas', body: '', canvas: { items: [{ id: workspaceItem, kind: 'sticky', text: 'Focused handoff note', color: 'mint', x: 80, y: 80, w: 220, h: 180, by: 'person' }], edges: [] }, links: [], created_at: now, updated_at: now };
  const workspaceBrief = { ...workspaceReport, excerpt: '', size: 0, open_marks: 0, item_count: 1 };
  const libraryReport = { ...workspaceReport, id: libraryCanvas, title: 'Library workspace fixture', slug: 'library-workspace-fixture', canvas: { items: [{ id: libraryItem, kind: 'sticky', text: 'Library board note', color: 'lemon', x: 120, y: 100, w: 220, h: 180, by: 'person' }], edges: [] } };
  const libraryBrief = { ...libraryReport, excerpt: '', size: 0, open_marks: 0, item_count: 1 };
  let currentLibraryReport = libraryReport;
  const fakeList = { ...sourceList, id: createdList, name: 'Held create list', data: { items: [] }, created_at: now, updated_at: now };
  const fakeTeam = {
    id: exactTeam,
    owner_user_id: 'd0000000-0000-0000-0000-000000000001',
    name: 'Exact Navigation Team',
    slug: 'exact-navigation-team',
    purpose: 'A mounted exact-link fixture',
    rules: '',
    lead_agent_id: mainAgent,
    channel_id: null,
    channel_name: null,
    created_at: now,
    updated_at: now,
    members: [{ team_id: exactTeam, member_kind: 'agent', member_id: mainAgent, name: 'Juniper', slug: 'juniper', emoji: '🦊' }],
    quests: [],
    open_quests: 0,
    cost_today_usd: 0,
    cost_week_usd: 0,
  };
  const exactAgent = sourceAgents?.length ? {
    ...(sourceAgents.find((agent) => agent.id === mainAgent) ?? sourceAgents[0]),
    id: exactCardAgent,
    name: 'Exact Card Agent',
    slug: 'exact-card-agent',
    is_main: false,
    created_at: now,
    updated_at: now,
  } : null;
  const questSince = sourceQuestRoom?.room.messages.reduce((latest, message) => {
    const at = message.edited_at && message.edited_at > message.created_at ? message.edited_at : message.created_at;
    return at > latest ? at : latest;
  }, '') || null;
  const questCatchupEvent = sourceQuestRoom ? {
    seq: sourceQuestRoom.room.cursor + 1,
    type: 'task.finished',
    agent_id: sourceQuestRoom.task.assigned_agent_id,
    task_id: selectedQuest,
    run_id: null,
    session_id: sourceQuestRoom.room.sessions.at(-1)?.id ?? null,
    payload: {},
    created_at: '2035-01-02T03:04:05.000Z',
  } : null;
  const questCatchupMessage = sourceQuestRoom ? {
    id: 'm9000000-0000-0000-0004-000000000001',
    agent_id: sourceQuestRoom.task.assigned_agent_id ?? mainAgent,
    session_id: sourceQuestRoom.room.sessions.at(-1)?.id ?? null,
    author_kind: 'agent',
    author_user_id: null,
    author_agent_id: sourceQuestRoom.task.assigned_agent_id ?? mainAgent,
    body: 'Quest catch-up fixture arrived',
    created_at: '2035-01-02T03:04:06.000Z',
    session_role: 'task',
    meta: null,
  } : null;
  const prefetchedQuestRoom = sourceQuestRoom ? {
    ...sourceQuestRoom,
    task: { ...sourceQuestRoom.task, status: 'in_progress' },
    room: { ...sourceQuestRoom.room, events: sourceQuestRoom.room.events.filter((event) => event.type !== 'task.finished') },
  } : null;
  const caughtUpQuestRoom = sourceQuestRoom ? {
    task: { ...sourceQuestRoom.task, status: 'done', updated_at: '2035-01-02T03:04:06.000Z' },
    room: {
      ...sourceQuestRoom.room,
      events: [questCatchupEvent],
      messages: [questCatchupMessage],
      cursor: questCatchupEvent.seq,
    },
  } : null;
  const savedQuickMessage = {
    id: 'm9000000-0000-0000-0000-000000000001',
    agent_id: mainAgent,
    session_id: savedQuick,
    author_kind: 'agent',
    author_user_id: null,
    author_agent_id: mainAgent,
    body: 'Saved chat fixture response',
    created_at: now,
    session_role: 'aside',
    meta: null,
  };
  const savedQuickChat = {
    id: savedQuick,
    agent_id: mainAgent,
    title: 'Saved navigation chat',
    backend: 'claude',
    model: 'fixture',
    status: 'active',
    thread_root: null,
    busy: false,
    messages: [savedQuickMessage],
  };
  const incomingAgentMessage = sourceAgentMessages ? {
    ...(sourceAgentMessages.at(-1) ?? {}),
    id: 'm9000000-0000-0000-0005-000000000001',
    agent_id: mainAgent,
    session_id: null,
    author_kind: 'agent',
    author_user_id: null,
    author_agent_id: mainAgent,
    body: 'Covered agent message fixture',
    created_at: '2035-01-02T03:04:07.000Z',
    session_role: 'main',
    meta: null,
  } : null;
  const workshopMessage = sourceAgentMessages ? {
    ...(sourceAgentMessages.at(-1) ?? {}),
    id: 'm9000000-0000-0000-0007-000000000001',
    agent_id: mainAgent,
    session_id: workshopSession,
    author_kind: 'agent',
    author_user_id: null,
    author_agent_id: mainAgent,
    body: `The changes are ready.\n\n${location.origin}/world?workbench=${workshopSession}&diff=${workshopSession}`,
    created_at: '2035-01-02T03:04:08.000Z',
    session_role: 'task',
    meta: null,
  } : null;
  const workspaceMessage = sourceAgentMessages ? {
    ...(sourceAgentMessages.at(-1) ?? {}),
    id: 'm9000000-0000-0000-0008-000000000001',
    agent_id: mainAgent,
    session_id: null,
    author_kind: 'agent',
    author_user_id: null,
    author_agent_id: mainAgent,
    body: `Open the focused workspace.\n\n${location.origin}/world?report=${workspaceCanvas.toUpperCase()}&workspace=1&item=${workspaceItem}`,
    created_at: '2035-01-02T03:04:09.000Z',
    session_role: 'main',
    meta: null,
  } : null;
  const workspaceCompletionEvent = batch === 'workspace-completion' && sourceQuestRoom ? {
    seq: sourceQuestRoom.room.cursor + 1,
    type: 'task.finished',
    agent_id: sourceQuestRoom.task.assigned_agent_id,
    task_id: selectedQuest,
    run_id: null,
    session_id: sourceQuestRoom.room.sessions.at(-1)?.id ?? null,
    payload: {
      report: {
        status: 'done',
        summary: `Completion workspace fixture\n\n[Open workspace](${location.origin}/world?report=${workspaceCanvas.toUpperCase()}&workspace=1&item=${workspaceItem})`,
        findings: [{ severity: 'note', issue: 'The focused note remains available in the completion details.' }],
      },
      review: { verdict: 'approve', summary: 'The completion evidence is clear.' },
    },
    created_at: '2035-01-02T03:04:10.000Z',
  } : null;
  const workspaceCompletionMessage = workspaceCompletionEvent && sourceQuestRoom ? {
    ...(sourceAgentMessages?.at(-1) ?? {}),
    id: 'm9000000-0000-0000-0008-000000000002',
    agent_id: workspaceCompletionEvent.agent_id,
    session_id: null,
    author_kind: 'system',
    author_user_id: null,
    author_agent_id: workspaceCompletionEvent.agent_id,
    body: workspaceCompletionEvent.payload.report.summary,
    created_at: '2035-01-02T03:04:10.001Z',
    session_role: 'main',
    meta: {
      kind: 'quest',
      title: sourceQuestRoom.task.title,
      status: 'done',
      task_id: selectedQuest,
      session_id: workspaceCompletionEvent.session_id,
    },
  } : null;
  const workspaceQuestRoom = workspaceCompletionEvent && workspaceCompletionMessage ? {
    ...sourceQuestRoom,
    task: { ...sourceQuestRoom.task, status: 'done', updated_at: workspaceCompletionEvent.created_at },
    room: {
      ...sourceQuestRoom.room,
      events: [...sourceQuestRoom.room.events.filter((event) => event.type !== 'task.finished'), workspaceCompletionEvent],
      messages: [...sourceQuestRoom.room.messages, workspaceCompletionMessage],
      cursor: workspaceCompletionEvent.seq,
    },
  } : null;
  const workspaceRoomPage = (includeEvent, includeMessage, cursor) => workspaceQuestRoom ? {
    ...workspaceQuestRoom,
    room: {
      ...workspaceQuestRoom.room,
      events: includeEvent ? [workspaceCompletionEvent] : [],
      messages: includeMessage ? [workspaceCompletionMessage] : [],
      cursor,
    },
  } : null;
  const workspaceNotification = workspaceCompletionEvent ? {
    events: [workspaceCompletionEvent],
    items: [{ id: 990274, seq: workspaceCompletionEvent.seq, kind: 'quest', agent_id: workspaceCompletionEvent.agent_id, created_at: workspaceCompletionEvent.created_at, cleared_at: null }],
    unread: 1,
    unread_seqs: [workspaceCompletionEvent.seq],
    by_agent: { [workspaceCompletionEvent.agent_id]: 1 },
  } : null;
  const workshopBrief = {
    id: workshopSession,
    agent_id: mainAgent,
    role: 'task',
    backend: 'codex',
    model: 'fixture',
    status: 'finished',
    task_id: selectedQuest,
    task_title: 'Feature link fixture',
    title: 'Feature link fixture',
    summary: null,
    cost_usd: 0,
    tokens: 0,
    runs: 1,
    running: false,
    created_at: now,
    updated_at: now,
    working_dir: `tasks/${selectedQuest}`,
    base_branch: 'main',
  };
  const originalTauri = window.__TAURI__;
  const workshopCalls = [];
  const workshopSessionQueries = [];
  let workshopChangeReads = 0;
  let workshopChangeMode = 'initial';
  let workshopModeReads = 0;
  const savedQuickBrief = { ...savedQuickChat, messages: undefined, created_at: now, updated_at: now };
  const replyTemplate = sourceChannelMessages?.at(-1);
  const fakeReplyRoot = replyTemplate ? { ...replyTemplate, id: replyRoot, channel_id: launch, body: 'Synthetic thread root', created_at: '2025-01-04T03:04:05.000Z', reply_to: null, reply_count: 1 } : null;
  const fakeReply = replyTemplate ? { ...replyTemplate, id: exactReply, channel_id: launch, body: 'fixture reply only cobalt', created_at: '2025-01-04T03:05:05.000Z', reply_to: replyRoot, reply_count: 0 } : null;
  const replyLink = fakeReply ? { ref: `ch${exactReply}`, kind: 'channel', agent_id: null, agent_name: null, agent_emoji: null, channel_id: launch, channel_name: launchDetail.name, channel_kind: launchDetail.kind, author: fakeReply.author_name ?? 'Alex Morgan', author_kind: fakeReply.author_kind, author_emoji: fakeReply.author_emoji, excerpt: fakeReply.body, created_at: fakeReply.created_at } : null;
  const agentReplyRootId = 'f9000000-0000-0000-0006-000000000001';
  const agentReplyId = 'f9000000-0000-0000-0006-000000000002';
  const agentReplyTemplate = sourceAgentMessages?.at(-1) ?? { id: agentReplyRootId, agent_id: mainAgent, session_id: null, author_kind: 'agent', author_user_id: null, author_agent_id: mainAgent, body: '', created_at: now, session_role: 'main', meta: null };
  const agentReplyRoot = { ...agentReplyTemplate, id: agentReplyRootId, agent_id: mainAgent, body: 'Visibility thread root fixture', created_at: '2025-01-04T04:04:05.000Z', reply_to: null };
  const agentReply = { ...agentReplyTemplate, id: agentReplyId, agent_id: mainAgent, body: 'visibility exact reply cobalt', created_at: '2025-01-04T04:05:05.000Z', reply_to: agentReplyRootId };
  const agentReplyLink = { ref: agentReplyId, kind: 'agent', agent_id: mainAgent, channel_id: null, channel_name: null, channel_kind: null, author: 'Juniper', author_kind: agentReply.author_kind, author_emoji: null, excerpt: agentReply.body, created_at: agentReply.created_at };
  window.fetch = async (path, init) => {
    const method = init?.method ?? 'GET';
    if (batch === 'feature-links' && typeof path === 'string' && path.startsWith(`/api/world/agents/${mainAgent}/messages?`) && method === 'GET' && sourceAgentMessages && workshopMessage) {
      return Response.json([...sourceAgentMessages, workshopMessage]);
    }
    if (batch === 'workspace-link' && typeof path === 'string' && path.startsWith(`/api/world/agents/${mainAgent}/messages?`) && method === 'GET' && sourceAgentMessages && workspaceMessage) {
      return Response.json([...sourceAgentMessages, workspaceMessage]);
    }
    if (batch === 'workspace-completion' && typeof path === 'string' && path.startsWith(`/api/world/agents/${messageAgent}/messages?`) && method === 'GET' && sourceAgentMessages && workspaceCompletionMessage) {
      return Response.json([...sourceAgentMessages, workspaceCompletionMessage]);
    }
    if (batch === 'workspace-completion' && typeof path === 'string' && path.startsWith(`/api/world/tasks/${selectedQuest}/room`) && method === 'GET' && workspaceQuestRoom) {
      if (workspaceCompletionPhase === 'event-first') return Response.json(workspaceRoomPage(true, false, workspaceCompletionEvent.seq));
      if (workspaceCompletionPhase === 'event-with-card') return Response.json(workspaceRoomPage(false, true, workspaceCompletionEvent.seq));
      if (workspaceCompletionPhase === 'card-first') return Response.json(workspaceRoomPage(false, true, sourceQuestRoom.room.cursor));
      if (workspaceCompletionPhase === 'card-with-event') return Response.json(workspaceRoomPage(true, false, workspaceCompletionEvent.seq));
    }
    if (batch === 'workspace-completion' && typeof path === 'string' && new URL(path, location.href).pathname === '/api/world/notifications' && method === 'GET' && workspaceNotification) {
      return Response.json(workspaceNotificationsCleared ? { events: [], items: [], unread: 0, unread_seqs: [], by_agent: {} } : workspaceNotification);
    }
    if (batch === 'workspace-completion' && path === '/api/world/notifications/clear' && method === 'POST') {
      workspaceNotificationsCleared = true;
      return new Response(null, { status: 204 });
    }
    if (workspaceBatch && typeof path === 'string' && new URL(path, location.href).pathname === '/api/world/reports/find' && new URL(path, location.href).searchParams.get('key')?.toLowerCase() === workspaceCanvas) {
      return Response.json(workspaceBrief);
    }
    if (batch === 'workspace-link' && typeof path === 'string' && new URL(path, location.href).pathname === '/api/world/reports' && new URL(path, location.href).searchParams.get('kind') === 'canvas' && method === 'GET') {
      const url = new URL(path, location.href);
      const offset = Number(url.searchParams.get('offset') ?? '0');
      const query = url.searchParams.get('query')?.trim().toLowerCase() ?? '';
      const currentLibraryBrief = { ...libraryBrief, ...currentLibraryReport, excerpt: currentLibraryReport.body ?? '' };
      const available = [
        ...(!workspaceLibraryDeleted ? [currentLibraryBrief] : []),
        ...(workspaceAvailable ? [workspaceBrief] : []),
      ].filter((report) => !query || `${report.title} ${report.excerpt}`.toLowerCase().includes(query));
      const page = offset === 0 ? available : [];
      if (query && workspaceSearchFailure) {
        workspaceSearchFailure = false;
        return new Response('Workspace search failed', { status: 503 });
      }
      if (query && holdWorkspaceSearch) {
        workspaceSearchHeld = true;
        await workspaceSearchGate;
      }
      return Response.json(page);
    }
    if (workspaceBatch && path === `/api/world/reports/${workspaceCanvas}` && method === 'GET') {
      if (batch === 'workspace-link') {
        workspaceDetailReads += 1;
        if (!workspaceAvailable) return new Response(null, { status: 404 });
      }
      return Response.json({ ...sourceReport, report: workspaceReport, backlinks: [], outlinks: [], marks: [], can_edit: true });
    }
    if (batch === 'workspace-link' && path === `/api/world/reports/${libraryCanvas}` && method === 'GET') {
      return workspaceLibraryDeleted
        ? new Response(null, { status: 404 })
        : Response.json({ ...sourceReport, report: currentLibraryReport, backlinks: [], outlinks: [], marks: [], can_edit: true });
    }
    if (batch === 'workspace-link' && path === `/api/world/reports/${libraryCanvas}` && method === 'PATCH') {
      currentLibraryReport = { ...currentLibraryReport, ...JSON.parse(init?.body ?? '{}'), updated_at: new Date().toISOString() };
      return Response.json(currentLibraryReport);
    }
    if (batch === 'workspace-link' && path === `/api/world/reports/${libraryCanvas}` && method === 'DELETE') {
      workspaceLibraryDeleted = true;
      return new Response(null, { status: 204 });
    }
    if (workspaceBatch && path === `/api/world/reports/${workspaceCanvas}/presence` && method === 'POST') return new Response(null, { status: 204 });
    if (batch === 'feature-links' && typeof path === 'string' && new URL(path, location.href).pathname === '/api/world/sessions' && method === 'GET') {
      const query = new URL(path, location.href).searchParams;
      workshopSessionQueries.push(query.toString());
      return Response.json(query.get('id') === workshopSession
        ? [workshopBrief]
        : Array.from({ length: 100 }, (_, index) => ({ ...workshopBrief, id: `f9000000-0000-4000-8000-${String(index + 1).padStart(12, '0')}` })));
    }
    if (batch === 'feature-links' && typeof path === 'string' && new URL(path, location.href).pathname === `/api/world/sessions/${workshopSession}/runs` && method === 'GET') return Response.json([]);
    if (batch === 'agent-panel-races' && typeof path === 'string' && path.startsWith(`/api/world/agents/${mainAgent}/messages?`) && method === 'GET' && sourceAgentMessages && incomingAgentMessage) {
      agentMessageReads += 1;
      return Response.json(includeIncomingAgentMessage ? [...sourceAgentMessages, incomingAgentMessage] : sourceAgentMessages);
    }
    if (batch === 'agent-panel-races' && path === '/api/world/notifications/seen' && method === 'POST') {
      const body = JSON.parse(init?.body ?? '{}');
      if (body.agent_id === mainAgent) agentSeenRequests += 1;
      return new Response(null, { status: 204 });
    }
    if (batch === 'quest-catchup' && typeof path === 'string' && path.startsWith(`/api/world/tasks/${selectedQuest}/room`) && method === 'GET' && prefetchedQuestRoom && caughtUpQuestRoom) {
      const url = new URL(path, location.href);
      if (!url.searchParams.has('after')) {
        questPrefetchHeld = true;
        await questPrefetchGate;
        return Response.json(prefetchedQuestRoom);
      }
      questCatchupUrl = url;
      return Response.json(caughtUpQuestRoom);
    }
    if (typeof path === 'string' && path.startsWith('/api/world/search?')) {
      const query = new URL(path, location.href).searchParams.get('q');
      if (query === 'held palette results') {
        paletteHeld = true;
        await paletteGate;
        return Response.json(stalePaletteResults);
      }
      if (query === 'visible old palette results') return Response.json(stalePaletteResults);
      if (query === 'current palette results') return Response.json(currentPaletteResults);
      if (query === 'fixture reply only cobalt' && replyLink) return Response.json({ ...searchShape, messages: [replyLink] });
      if (query === 'visibility exact reply cobalt' && agentReplyLink) return Response.json({ ...searchShape, messages: [agentReplyLink] });
    }
    if (batch === 'visibility' && path === `/api/world/messages/${agentReplyId}` && method === 'GET' && agentReplyLink) return Response.json(agentReplyLink);
    if (batch === 'visibility' && typeof path === 'string' && (path === `/api/world/agents/${mainAgent}/messages/${agentReplyId}/replies` || path === `/api/world/agents/${mainAgent}/messages/${agentReplyRootId}/replies`) && method === 'GET' && agentReplyRoot && agentReply) {
      agentThreadReads += 1;
      if (holdAgentThread && agentThreadReads > 1) {
        agentThreadHeld = true;
        await agentThreadGate;
      }
      return Response.json([agentReplyRoot, agentReply]);
    }
    if (batch === 'async-starts' && path === `/api/world/agents/${mainAgent}/fork` && method === 'POST' && sourceAgents?.length) {
      forkHeld = true;
      if (holdFork) await forkGate;
      const sourceAgent = sourceAgents.find((agent) => agent.id === mainAgent) ?? sourceAgents[0];
      const ordinal = forkReturns + 1;
      const id = `${forkCopyBase}${String(ordinal).padStart(12, '0')}`;
      const copy = {
        ...sourceAgent,
        id,
        name: `Forked Navigation ${ordinal}`,
        slug: `forked-navigation-${ordinal}`,
        is_main: false,
        visibility: 'private',
        created_at: now,
        updated_at: now,
      };
      createdAgents.set(id, copy);
      forkReturns += 1;
      return Response.json(copy);
    }
    if (batch === 'foreground' && path === '/api/world/agents' && method === 'POST' && sourceAgents?.length) {
      const body = JSON.parse(init?.body ?? '{}');
      const sourceAgent = sourceAgents.find((agent) => agent.id === mainAgent) ?? sourceAgents[0];
      const ordinal = createdAgents.size + 1;
      const id = `a9000000-0000-0000-0000-${String(ordinal).padStart(12, '0')}`;
      const agent = {
        ...sourceAgent,
        ...body,
        id,
        slug: `${String(body.name ?? 'fixture-agent').toLowerCase().replace(/[^a-z0-9]+/g, '-')}-${ordinal}`,
        name: body.name,
        avatar_emoji: sourceAgent.avatar_emoji,
        runner_id: sourceAgent.runner_id,
        runner_name: sourceAgent.runner_name,
        status: 'idle',
        status_detail: null,
        is_main: false,
        cost_usd: 0,
        tokens: 0,
        jobs: [],
        created_at: now,
        updated_at: now,
      };
      agentCreateHeld = true;
      if (holdAgentCreate) await agentCreateGate;
      createdAgents.set(id, agent);
      agentCreateReturns += 1;
      return Response.json(agent);
    }
    if ((batch === 'foreground' || batch === 'async-starts') && path === '/api/world/agents' && method === 'GET' && sourceAgents) {
      if (batch === 'foreground') agentRosterReads += 1;
      else forkRosterReads += 1;
      return Response.json([...sourceAgents.filter((agent) => !createdAgents.has(agent.id)), ...createdAgents.values()]);
    }
    if (batch === 'detail-races' && path === '/api/world/agents' && method === 'GET' && sourceAgents) {
      agentRosterReads += 1;
      const response = includeExactAgentInList ? [...sourceAgents, exactAgent] : sourceAgents;
      includeExactAgentInList = false;
      return Response.json(response);
    }
    if (batch === 'detail-races' && path === `/api/world/agents/${exactCardAgent}` && method === 'GET' && exactAgent) {
      exactAgentReads += 1;
      exactAgentHeld = true;
      if (holdExactAgent) await exactAgentGate;
      return Response.json(exactAgent);
    }
    if (batch === 'detail-races' && typeof path === 'string' && path.startsWith(`/api/world/agents/${exactCardAgent}/messages`) && method === 'GET') return Response.json([]);
    if ((batch === 'foreground' || batch === 'async-starts') && typeof path === 'string' && [...createdAgents.keys()].some((id) => path.startsWith(`/api/world/agents/${id}/messages`)) && method === 'GET') {
      return Response.json([]);
    }
    if ((batch === 'foreground' || batch === 'async-starts') && path === '/api/world/notifications/seen' && method === 'POST') {
      const body = JSON.parse(init?.body ?? '{}');
      if (createdAgents.has(body.agent_id)) return new Response(null, { status: 204 });
    }
    if (batch === 'async-starts' && typeof path === 'string' && path.startsWith('/api/world/quick?') && method === 'GET') {
      const created = [...createdQuicks.values()].map(({ messages, ...chat }) => ({ ...chat, created_at: now, updated_at: now }));
      return Response.json([savedQuickBrief, ...created]);
    }
    if (batch === 'async-starts' && path === `/api/world/quick/${savedQuick}` && method === 'GET') return Response.json(savedQuickChat);
    if (batch === 'async-starts' && typeof path === 'string' && path.startsWith('/api/world/quick/') && method === 'GET') {
      const id = path.slice('/api/world/quick/'.length);
      const chat = createdQuicks.get(id);
      if (chat) return Response.json(chat);
    }
    if (batch === 'async-starts' && path === `/api/world/agents/${mainAgent}/quick` && method === 'POST') {
      const body = JSON.parse(init?.body ?? '{}');
      quickStartHeld = true;
      if (holdQuickStart) await quickStartGate;
      const current = body.text === 'Current quick start fixture';
      const id = current ? currentQuick : heldQuick;
      const message = {
        ...savedQuickMessage,
        id: current ? 'm9000000-0000-0000-0000-000000000003' : 'm9000000-0000-0000-0000-000000000002',
        session_id: id,
        author_kind: 'user',
        author_user_id: 'd0000000-0000-0000-0000-000000000001',
        author_agent_id: null,
        body: body.text,
      };
      createdQuicks.set(id, {
        ...savedQuickChat,
        id,
        title: current ? 'Current fixture chat' : 'Held fixture chat',
        busy: false,
        messages: [message],
      });
      return Response.json({ session_id: id, run_id: `fixture-${id}`, message });
    }
    if (batch === 'board' && path === `/api/world/teams/${exactTeam}` && method === 'GET') {
      exactTeamReads += 1;
      return Response.json(fakeTeam);
    }
    if (batch === 'review' && path === `/api/world/messages/ch${exactReply}` && method === 'GET' && replyLink) return Response.json(replyLink);
    if (batch === 'review' && typeof path === 'string' && (path === `/api/world/channels/${launch}/messages/${exactReply}/replies` || path === `/api/world/channels/${launch}/messages/${replyRoot}/replies`) && method === 'GET' && fakeReplyRoot && fakeReply) return Response.json([fakeReplyRoot, fakeReply]);
    if (batch === 'review' && path === '/api/world/todos' && method === 'GET') {
      const todos = await originalFetch(path, init).then((response) => response.json());
      return Response.json(todos.filter((todo) => todo.id !== oldTodo));
    }
    if (batch === 'review' && path === `/api/world/todos/${oldTodo}` && method === 'GET' && agedTodo) {
      oldTodoReads += 1;
      return Response.json(agedTodo);
    }
    if (batch === 'foreground' && sourceTodo && path === `/api/world/todos/${sourceTodo.id}` && method === 'PATCH') {
      todoPatches += 1;
      return Response.json({ ...sourceTodo, ...JSON.parse(init?.body ?? '{}') });
    }
    if (typeof path === 'string' && path === `/api/world/agents/${mainAgent}/messages` && method === 'POST') {
      const body = JSON.parse(init?.body ?? '{}');
      if (typeof body.body === 'string' && body.body.startsWith('Navigation fixture Tell')) {
        tellPosts += 1;
        if (holdTell) {
          tellHeld = true;
          await tellGate;
        }
        return Response.json({ id: `fixture-tell-${tellPosts}`, agent_id: mainAgent, session_id: null, author_kind: 'user', author_user_id: null, author_agent_id: null, body: body.body, created_at: now, session_role: null, meta: null });
      }
    }
    if (path === '/api/world/channels' && method === 'POST') {
      createdChannelPosts += 1;
      if (holdChannel) {
        channelHeld = true;
        await channelGate;
      }
      const body = JSON.parse(init?.body ?? '{}');
      const id = body.name === 'latest-create-channel' ? latestChannel : createdChannel;
      const channel = { ...launchDetail, id, name: body.name, topic: body.topic || null };
      createdChannels.set(id, channel);
      return Response.json(channel);
    }
    if (path === '/api/world/channels' && method === 'GET' && createdChannels.size) {
      const channels = await originalFetch(path, init).then((response) => response.json());
      const known = new Set(channels.map((channel) => channel.id));
      const added = [...createdChannels.values()].filter((channel) => !known.has(channel.id)).map((channel) => ({ id: channel.id, name: channel.name, topic: channel.topic, kind: channel.kind, member: true, unread: 0, last_message_at: null }));
      return Response.json([...channels, ...added]);
    }
    const createdChannelMatch = typeof path === 'string' ? /^\/api\/world\/channels\/(c0000000-0000-0000-0000-00000000900[12])$/.exec(path) : null;
    if (createdChannelMatch && method === 'GET') {
      createdChannelReads += 1;
      return Response.json(createdChannels.get(createdChannelMatch[1]));
    }
    if (typeof path === 'string' && /^\/api\/world\/channels\/c0000000-0000-0000-0000-00000000900[12]\/messages$/.test(path) && method === 'GET') return Response.json([]);
    if (batch === 'mutation-races' && typeof path === 'string' && path.startsWith('/api/world/reports?') && method === 'GET') {
      return Response.json([...mutationReports.values()].map(mutationBrief));
    }
    if (batch === 'detail-races' && typeof path === 'string' && path.startsWith('/api/world/reports?') && method === 'GET') {
      return Response.json([...detailReports.values()].map(mutationBrief));
    }
    const detailReportMatch = batch === 'detail-races' && typeof path === 'string' ? /^\/api\/world\/reports\/(f9000000-0000-0000-0003-00000000000[1-3])$/.exec(path) : null;
    if (detailReportMatch && method === 'GET') {
      const item = detailReports.get(detailReportMatch[1]);
      return Response.json({ ...sourceReport, report: item, backlinks: [], outlinks: [], marks: [], can_edit: true });
    }
    const detailRevisionsMatch = batch === 'detail-races' && typeof path === 'string' ? /^\/api\/world\/reports\/(f9000000-0000-0000-0003-00000000000[12])\/revisions$/.exec(path) : null;
    if (detailRevisionsMatch && method === 'GET') {
      const id = detailRevisionsMatch[1];
      if (holdRevisionList === id) {
        revisionListHeld = true;
        await revisionListGate;
      }
      const item = detailReports.get(id);
      return Response.json([2, 1].map((revision) => {
        const value = detailRevision(item, revision);
        return { ...value, body: undefined, canvas: undefined, size: value.body.length };
      }));
    }
    const detailRevisionMatch = batch === 'detail-races' && typeof path === 'string' ? /^\/api\/world\/reports\/(f9000000-0000-0000-0003-00000000000[12])\/revisions\/(\d+)$/.exec(path) : null;
    if (detailRevisionMatch && method === 'GET') {
      const id = detailRevisionMatch[1];
      const revision = Number(detailRevisionMatch[2]);
      if (holdRevisionDetail === `${id}:${revision}`) {
        revisionDetailHeld = true;
        await revisionDetailGate;
      }
      return Response.json(detailRevision(detailReports.get(id), revision));
    }
    const mutationReportMatch = batch === 'mutation-races' && typeof path === 'string' ? /^\/api\/world\/reports\/(f9000000-0000-0000-0002-00000000000[1-4])$/.exec(path) : null;
    if (mutationReportMatch && method === 'GET') {
      const id = mutationReportMatch[1];
      mutationDetailReads.set(id, (mutationDetailReads.get(id) ?? 0) + 1);
      if (holdReportDetail === id) {
        reportDetailHeld = true;
        await reportDetailGate;
      }
      const item = mutationReports.get(id);
      return item ? Response.json({ ...sourceReport, report: item, backlinks: [], outlinks: [], marks: [], can_edit: true }) : new Response(null, { status: 404 });
    }
    if (mutationReportMatch && method === 'PATCH') {
      const id = mutationReportMatch[1];
      if (heldReportMutation === `PATCH:${id}`) {
        reportMutationHeld = true;
        await reportMutationGate;
      }
      const item = mutationReports.get(id);
      const updated = { ...item, ...JSON.parse(init?.body ?? '{}'), updated_at: new Date().toISOString() };
      mutationReports.set(id, updated);
      return Response.json(updated);
    }
    if (mutationReportMatch && method === 'DELETE') {
      const id = mutationReportMatch[1];
      if (heldReportMutation === `DELETE:${id}`) {
        reportMutationHeld = true;
        await reportMutationGate;
      }
      mutationReports.delete(id);
      return new Response(null, { status: 204 });
    }
    const mutationRevisionsMatch = batch === 'mutation-races' && typeof path === 'string' ? /^\/api\/world\/reports\/(f9000000-0000-0000-0002-00000000000[1-4])\/revisions$/.exec(path) : null;
    if (mutationRevisionsMatch && method === 'GET') {
      const item = mutationReports.get(mutationRevisionsMatch[1]);
      return Response.json([
        { revision: 2, title: item.title, folder: item.folder, tags: item.tags, author_agent_id: item.author_agent_id, author_user_id: item.author_user_id, created_at: item.updated_at, size: item.body.length },
        { revision: 1, title: item.title, folder: item.folder, tags: item.tags, author_agent_id: item.author_agent_id, author_user_id: item.author_user_id, created_at: item.created_at, size: item.body.length },
      ]);
    }
    const mutationRestoreMatch = batch === 'mutation-races' && typeof path === 'string' ? /^\/api\/world\/reports\/(f9000000-0000-0000-0002-00000000000[1-4])\/revisions\/(\d+)\/restore$/.exec(path) : null;
    if (mutationRestoreMatch && method === 'POST') {
      const id = mutationRestoreMatch[1];
      if (heldReportMutation === `RESTORE:${id}`) {
        reportMutationHeld = true;
        await reportMutationGate;
      }
      const item = mutationReports.get(id);
      const updated = { ...item, title: 'Restored mutation fixture A', revision: item.revision + 1, updated_at: new Date().toISOString() };
      mutationReports.set(id, updated);
      return Response.json(updated);
    }
    if (path === '/api/world/reports' && method === 'POST') {
      const body = JSON.parse(init?.body ?? '{}');
      if (body.canvas) {
        canvasRequestIds.push(body.request_id);
        if (holdCanvas) {
          canvasHeld = true;
          await canvasGate;
        }
        if (failCanvasResponse) {
          failCanvasResponse = false;
          return new Response('Workspace response lost', { status: 503 });
        }
        return Response.json(fakeCanvas);
      }
      if (batch === 'report') {
        reportPosts += 1;
        if (holdReportCreate) {
          reportCreateHeld = true;
          await reportCreateGate;
        }
        const id = `${savedReportBase}${reportPosts}`;
        savedReport = { ...sourceReport.report, ...body, id, slug: `navigation-save-${reportPosts}`, revision: 1, created_at: now, updated_at: now };
        return Response.json(savedReport);
      }
    }
    if (batch === 'report' && savedReport && path === `/api/world/reports/${savedReport.id}` && method === 'PATCH') {
      const body = JSON.parse(init?.body ?? '{}');
      reportPatches.push({ id: savedReport.id, body });
      savedReport = { ...savedReport, ...body, revision: savedReport.revision + 1, updated_at: new Date().toISOString() };
      return Response.json(savedReport);
    }
    if (batch === 'report' && savedReport && path === `/api/world/reports/${savedReport.id}` && method === 'GET') {
      return Response.json({ ...sourceReport, report: savedReport, backlinks: [], outlinks: [], marks: [], can_edit: true });
    }
    if (path === `/api/world/reports/${report}` && method === 'GET' && holdAcademyReport) {
      academyReportHeld = true;
      await academyReportGate;
      return Response.json({ ...sourceReport, report: { ...sourceReport.report, practice: { subject: 'drawing', coach_agent_id: null } }, lesson: academyLesson });
    }
    if (path === `/api/world/reports/${createdCanvas}` && method === 'GET') {
      createdCanvasReads += 1;
      return Response.json({ ...sourceReport, report: fakeCanvas });
    }
    if (path === '/api/world/items' && method === 'POST') {
      if (holdList) {
        listHeld = true;
        await listGate;
      }
      listCreated = true;
      return Response.json(fakeList);
    }
    if (path === '/api/world/items' && method === 'GET' && listCreated) {
      const items = await originalFetch(path, init).then((response) => response.json());
      return Response.json(items.some((item) => item.id === createdList) ? items : [...items, fakeList]);
    }
    if (path === `/api/world/items/${createdList}` && method === 'GET') {
      createdListReads += 1;
      return Response.json(fakeList);
    }
    if (path === `/api/world/items/${list}` && method === 'GET' && holdItem) {
      const response = failItem ? new Response(null, { status: 404 }) : await originalFetch(path, init);
      itemHeld = true;
      await itemGate;
      if (failItem) failedItemResponses += 1;
      return response;
    }
    if (path === `/api/world/channels/${launch}/messages` && method === 'GET') {
      const messages = await originalFetch(path, init).then((response) => response.json());
      if (holdMessages) {
        messagesHeld = true;
        await messagesGate;
      }
      const last = messages.at(-1);
      const fixtures = [{ ...last, id: fakeMessage, channel_id: launch, body: 'Unread foreground guard fixture', created_at: now }];
      if (batch === 'review') fixtures.push({ ...last, id: todoMessage, channel_id: launch, body: `Exact to-do fixture\n\ntodo:${oldTodo}`, created_at: now });
      return Response.json([...messages, ...fixtures]);
    }
    if (path === `/api/world/channels/${launch}/read` && method === 'POST') {
      markReads.push(JSON.parse(init?.body ?? '{}').last_id);
      return new Response(null, { status: 204 });
    }
    if (batch === 'review' && path === `/api/world/challenges/${academyLesson}` && method === 'GET') {
      return Response.json({ card: { id: academyLesson, title: 'Stale Academy redirect', type: 'canvas', skill: 'drawing', difficulty: 'easy', xp: 10, minutes: 5, report_id: report, state: 'new', attempts: 0 }, prompt: '', hints: [], reading: [], tests: null, choices: [], coach: null, attempts: [] });
    }
    return originalFetch(path, init);
  };
  const workshopLayoutKey = `world.workbench.v1:${workshopSession}`;
  const savedWorkshopLayout = batch === 'feature-links' ? localStorage.getItem(workshopLayoutKey) : null;
  if (batch === 'feature-links') localStorage.removeItem(workshopLayoutKey);
  const workspaceLayoutKey = `world.workspace.${sourceReport.report.owner_user_id}`;
  const savedWorkspaceLayout = workspaceBatch ? localStorage.getItem(workspaceLayoutKey) : null;
  if (workspaceBatch) localStorage.removeItem(workspaceLayoutKey);
  const workspaceLibraryKey = 'world.workspaces.library';
  const savedWorkspaceLibrary = batch === 'workspace-link' ? localStorage.getItem(workspaceLibraryKey) : null;
  if (batch === 'workspace-link') localStorage.removeItem(workspaceLibraryKey);
  const sideChatKey = 'world.sidechat';
  const savedSideChat = batch === 'workspace-link' ? localStorage.getItem(sideChatKey) : null;
  if (batch === 'workspace-link') localStorage.removeItem(sideChatKey);
  const checks = [];
  try {
    if (batch === 'core') {
    const showMobileChatList = async () => {
      if (innerWidth > 500) return;
      const back = await until(() => visible('button[aria-label="Back to chats"]')[0], 'mobile Back to chats did not appear');
      back.click();
      await until(() => visible('button[data-chat-item]').length, 'mobile chat list did not appear');
    };
    const openWorkspaceLibrary = async () => {
      await choose('Open workspaces', 'action:workspace');
      const surface = await until(() => modal('Workspaces') ?? modal('Workspace'), 'Workspaces did not open');
      if (surface.getAttribute('aria-label') === 'Workspace') {
        const back = await until(() => [...surface.querySelectorAll('header button')].find((element) => element.getClientRects().length && (element.title === 'Back to all workspaces' || element.textContent.trim() === 'All workspaces')), 'All workspaces did not appear');
        back.click();
      }
      return until(() => modal('Workspaces'), 'Workspaces library did not open');
    };
    const newWorkspaceButton = (surface) => [...surface.querySelectorAll('header button')].find((element) => element.getClientRects().length && element.textContent.trim() === 'New workspace');
    checks.push(await run('a real channel palette hit comes to the front over Reports', async () => {
      await choose('Spring launch plan', `hit:report:${report}`);
      await until(() => modal('Reports'), 'Reports did not open');
      await choose('launch', `hit:channel:${launch}`);
      await until(() => modal('Chat') && exactText('header p', 'launch'), 'Chat did not come to the front');
    }));
    checks.push(await run('the mounted palette drops displayed and late hits from a superseded query', async () => {
      let release;
      paletteGate = new Promise((resolve) => { release = resolve; });
      openPalette();
      await paletteInput('visible old palette results');
      await until(() => paletteHit(`hit:report:${report}`), 'initial palette result did not appear');
      await paletteInput('held palette results');
      await until(() => !paletteHit(`hit:report:${report}`), 'old query hit remained visible after input changed');
      await until(() => paletteHeld, 'superseded palette request was not held');
      await paletteInput('current palette results');
      await until(() => paletteHit(`hit:channel:${launch}`), 'current palette query result did not appear');
      release();
      await wait(500);
      if (paletteHit(`hit:report:${report}`) || !paletteHit(`hit:channel:${launch}`)) throw new Error('late palette response replaced current hits');
      visible('button[aria-label="Close command palette"]')[0].click();
      await until(() => !visible('[cmdk-input]').length, 'Cmd K did not close after the query race');
    }));
    checks.push(await run('real palette and Chat sidebar keep the newer channel over a late exact result', async () => {
      let release;
      const gate = new Promise((resolve) => { release = resolve; });
      const original = window.fetch;
      let held = false;
      window.fetch = async (path, init) => {
        const response = await original(path, init);
        if (path === `/api/world/items/${list}` && !held) {
          held = true;
          await gate;
        }
        return response;
      };
      try {
        await choose('launch', `hit:channel:${launch}`);
        await showMobileChatList();
        await choose('Bugs to triage', `hit:list:${list}`);
        await until(() => held, 'exact list response was not held');
        const general = await until(() => visible('button[data-chat-item]').find((element) => element.textContent.trim().startsWith('general')), 'general channel did not appear');
        general.click();
        await until(() => general.dataset.current === 'true' && exactText('header p', 'general'), 'real Chat sidebar did not select general');
        release();
        await wait(500);
        if (!modal('Chat') || !exactText('header p', 'general')) throw new Error('late exact result replaced the sidebar selection');
      } finally {
        release();
        window.fetch = original;
      }
    }));
    checks.push(await run('a held channel create cannot replace a newer sidebar channel', async () => {
      await choose('launch', `hit:channel:${launch}`);
      await showMobileChatList();
      visible('button[aria-label="New channel"]')[0].click();
      const name = await until(() => visible('input[placeholder="name"]')[0], 'new channel form did not open');
      setValue(name, 'held-create-channel');
      let release;
      const postsBefore = createdChannelPosts;
      channelGate = new Promise((resolve) => { release = resolve; });
      holdChannel = true;
      name.closest('form').requestSubmit();
      await until(() => channelHeld, 'channel create was not held');
      const general = await until(() => visible('button[data-chat-item]').find((element) => element.textContent.trim().startsWith('general')), 'general channel did not appear');
      general.click();
      await until(() => general.dataset.current === 'true' && exactText('header p', 'general'), 'newer channel did not select');
      release();
      await wait(500);
      holdChannel = false;
      if (createdChannelPosts !== postsBefore + 1) throw new Error('channel form submitted more than once');
      if (!exactText('header p', 'general') || new URL(location.href).searchParams.get('channel') === createdChannel) throw new Error('completed create replaced the newer channel');
    }));
    checks.push(await run('a channel create invalidates an older exact lookup and opens without another read', async () => {
      await choose('launch', `hit:channel:${launch}`);
      await showMobileChatList();
      let release;
      itemGate = new Promise((resolve) => { release = resolve; });
      holdItem = true;
      itemHeld = false;
      await choose('Bugs to triage', `hit:list:${list}`);
      await until(() => itemHeld, 'older exact list lookup was not held');
      let name = visible('input[placeholder="name"]')[0];
      if (!name) {
        visible('button[aria-label="New channel"]')[0].click();
        name = await until(() => visible('input[placeholder="name"]')[0], 'new channel form did not open');
      }
      setValue(name, 'latest-create-channel');
      await wait(100);
      const postsBefore = createdChannelPosts;
      createdChannelReads = 0;
      name.closest('form').requestSubmit();
      await until(() => exactText('header p', 'latest-create-channel'), 'latest channel create did not open');
      release();
      await wait(500);
      holdItem = false;
      if (createdChannelPosts !== postsBefore + 1) throw new Error('channel form submitted more than once');
      if (createdChannelReads !== 0) throw new Error('channel creation performed a duplicate exact read');
      if (!modal('Chat') || !exactText('header p', 'latest-create-channel') || new URL(location.href).searchParams.get('list') === list) throw new Error('older exact list replaced the created channel');
    }));
    checks.push(await run('a held workspace create cannot replace a newer report', async () => {
      let release = () => {};
      try {
        const workspaces = await openWorkspaceLibrary();
        canvasGate = new Promise((resolve) => { release = resolve; });
        holdCanvas = true;
        const requestStart = canvasRequestIds.length;
        const create = await until(() => newWorkspaceButton(workspaces), 'New workspace did not appear');
        create.click();
        await until(() => canvasHeld, 'workspace create was not held');
        await choose('Competitor pricing, pro tiers', `hit:report:${otherReport}`);
        const newerReport = () => {
          const reports = modal('Reports');
          return reports && [...reports.querySelectorAll('h2')].find((element) => element.textContent.trim() === 'Competitor pricing, pro tiers');
        };
        await until(newerReport, 'newer report did not select');
        release();
        await wait(500);
        const heldRequestId = canvasRequestIds[requestStart];
        if (!requestIdPattern.test(heldRequestId)) throw new Error('workspace create did not use a valid request ID');
        if (!newerReport()) throw new Error('completed workspace create replaced the newer report');
      } finally {
        holdCanvas = false;
        release();
        await wait(100);
        canvasHeld = false;
      }
    }));
    checks.push(await run('a retried workspace create reuses its request ID and opens without another exact read', async () => {
      try {
        const workspaces = await openWorkspaceLibrary();
        createdCanvasReads = 0;
        const requestStart = canvasRequestIds.length;
        failCanvasResponse = true;
        const create = await until(() => newWorkspaceButton(workspaces), 'New workspace did not appear');
        create.click();
        const alert = await until(() => workspaces.querySelector('[role="alert"]'), 'workspace create failure did not appear');
        const retry = await until(() => [...alert.querySelectorAll('button')].find((element) => element.getClientRects().length && element.textContent.trim() === 'Try again'), 'workspace create retry did not appear');
        retry.click();
        const workspace = await until(() => {
          const opened = modal('Workspace');
          return opened?.querySelector('header p')?.textContent.includes(fakeCanvas.title) ? opened : null;
        }, 'retried workspace create did not open');
        if (!workspace.isConnected) throw new Error('retried workspace was not mounted');
        const retryIds = canvasRequestIds.slice(requestStart);
        if (retryIds.length !== 2 || retryIds[0] !== retryIds[1]) throw new Error('workspace retry changed its request ID');
        if (!requestIdPattern.test(retryIds[0])) throw new Error('workspace retry did not use a valid request ID');
        if (retryIds[0] === canvasRequestIds[requestStart - 1]) throw new Error('separate workspace creates reused a request ID');
        if (createdCanvasReads !== 0) throw new Error('workspace creation performed a duplicate exact read');
      } finally {
        failCanvasResponse = false;
        holdCanvas = false;
        canvasHeld = false;
      }
    }));
    checks.push(await run('a held list create cannot replace a newer exact list', async () => {
      openPalette();
      await paletteInput('Open your lists');
      const action = await until(() => paletteHit('action:lists'), 'Lists palette action did not appear');
      action.click();
      await until(() => modal('Your lists'), 'Lists did not open');
      await clickText('button', 'New list');
      const title = await until(() => visible('input[aria-label="List name"]')[0], 'new list form did not open');
      setValue(title, 'Held create list');
      let releaseCreate;
      let releaseItem;
      listGate = new Promise((resolve) => { releaseCreate = resolve; });
      itemGate = new Promise((resolve) => { releaseItem = resolve; });
      holdList = true;
      holdItem = true;
      listHeld = false;
      itemHeld = false;
      try {
        title.closest('form').requestSubmit();
        await until(() => listHeld, 'list create was not held');
        await choose('Bugs to triage', `hit:list:${list}`);
        await until(() => itemHeld, 'newer exact list lookup was not held');
        releaseCreate();
        await wait(350);
        if (!title.isConnected || title.value !== 'Held create list') throw new Error('stale create completion closed the newer pending list form');
        if (new URL(location.href).searchParams.get('list') === createdList || exactText('h1,h2', 'Held create list')) throw new Error('completed list create replaced the pending newer list');
        releaseItem();
        await until(() => exactText('h1,h2', 'Bugs to triage') || new URL(location.href).searchParams.get('list') === list, 'newer list did not select');
        if (new URL(location.href).searchParams.get('list') !== list || exactText('h1,h2', 'Held create list')) throw new Error('completed list create replaced the newer list');
      } finally {
        releaseCreate();
        releaseItem();
        holdList = false;
        holdItem = false;
      }
    }));
    checks.push(await run('a latest list create opens from its response without another exact read', async () => {
      openPalette();
      await paletteInput('Open your lists');
      (await until(() => paletteHit('action:lists'), 'Lists palette action did not appear')).click();
      await until(() => modal('Your lists'), 'Lists did not open');
      await clickText('button', 'New list');
      const title = await until(() => visible('input[aria-label="List name"]')[0], 'new list form did not open');
      setValue(title, 'Held create list');
      createdListReads = 0;
      title.closest('form').requestSubmit();
      await until(() => new URL(location.href).searchParams.get('list') === createdList && exactText('h1,h2', 'Held create list'), 'latest list create did not open');
      if (createdListReads !== 0) throw new Error('list creation performed a duplicate exact read');
    }));
    checks.push(await run('covered Chat does not mark a late page read and marks it when Reports closes', async () => {
      let release;
      messagesGate = new Promise((resolve) => { release = resolve; });
      holdMessages = true;
      markReads = [];
      await choose('launch', `hit:channel:${launch}`);
      await until(() => messagesHeld, 'channel messages were not held');
      await choose('Spring launch plan', `hit:report:${report}`);
      await until(() => modal('Reports'), 'Reports did not cover Chat');
      release();
      await wait(500);
      if (markReads.includes(fakeMessage)) throw new Error('covered Chat marked the late message read');
      holdMessages = false;
      modal('Reports').querySelector('button[aria-label="Close"]').click();
      await until(() => modal('Chat'), 'closing Reports did not restore Chat');
      await until(() => markReads.includes(fakeMessage), 'foreground Chat did not mark the message read');
    }));
    }
    if (batch === 'review') {
      checks.push(await run('a stale loaded-report Academy redirect cannot replace a newer exact list', async () => {
        let releaseReport = () => {};
        let releaseItem = () => {};
        try {
          await choose('Competitor pricing, pro tiers', `hit:report:${otherReport}`);
          await until(() => modal('Reports'), 'Reports did not open for the Academy redirect race');
          if (innerWidth <= 500) {
            const back = await until(() => visible('button[aria-label="Back to the library"]')[0], 'mobile report Back to the library did not appear');
            back.click();
            await until(() => !visible('button[aria-label="Back to the library"]').length, 'mobile report library did not return');
          }
          academyReportGate = new Promise((resolve) => { releaseReport = resolve; });
          holdAcademyReport = true;
          academyReportHeld = false;
          const row = await until(() => visible(`button[data-report="${report}"]`)[0], 'loaded report row did not appear');
          row.click();
          await until(() => academyReportHeld, 'loaded report detail was not held');
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'newer exact list lookup was not held');
          releaseReport();
          await wait(450);
          if (modal('Academy') || exactText('h1,h2,h3', 'Academy') || exactText('h1,h2,h3', 'Stale Academy redirect')) throw new Error('stale report detail opened Academy');
          releaseItem();
          await until(() => modal('Your lists') && new URL(location.href).searchParams.get('list') === list, 'newer exact list did not win');
        } finally {
          releaseReport();
          releaseItem();
          holdAcademyReport = false;
          holdItem = false;
        }
      }));
      checks.push(await run('a reply-only Cmd K result opens its root and focuses the exact reply', async () => {
        await choose('fixture reply only cobalt', `hit:message:ch${exactReply}`);
        const reply = await until(() => visible(`aside[aria-label="Thread"] [data-message-id="${exactReply}"]`).find((element) => element.classList.contains('ring-w-gold')), 'exact reply did not receive focus');
        if (!reply.textContent.includes('fixture reply only cobalt')) throw new Error('the focused row was not the exact reply');
        if (!document.querySelector(`aside[aria-label="Thread"] [data-message-id="${replyRoot}"]`)) throw new Error('the reply opened without its thread root');
      }));
      checks.push(await run('an exact report dismisses the organization switcher', async () => {
        await choose('Switch organization', 'action:orgs');
        await until(() => modal('Organizations'), 'organization switcher did not open');
        await choose('Spring launch plan', `hit:report:${report}`);
        await until(() => !modal('Organizations') && modal('Reports'), 'organization switcher still covered Reports');
      }));
      checks.push(await run('report A to B to A reapplies the exact report selection', async () => {
        const row = `[data-report="${report}"]`;
        await until(() => document.querySelector(row)?.getAttribute('aria-current') === 'true', 'report A was not selected');
        await choose('Competitor pricing, pro tiers', `hit:report:${otherReport}`);
        await until(() => document.querySelector(row)?.getAttribute('aria-current') === 'false', 'report B was not selected');
        await choose('Spring launch plan', `hit:report:${report}`);
        await until(() => document.querySelector(row)?.getAttribute('aria-current') === 'true', 'report A was not reselected');
      }));
      checks.push(await run('an exact to-do card opens a record outside the loaded collection', async () => {
        await choose('launch', `hit:channel:${launch}`);
        const message = await until(() => visible(`[data-message-id="ch${todoMessage}"]`)[0], 'exact to-do fixture message did not mount');
        const card = await until(() => [...message.querySelectorAll('button')].find((element) => element.textContent.includes(agedTodo.text)), 'exact to-do card did not become available');
        const readsBefore = oldTodoReads;
        card.click();
        await until(() => oldTodoReads > readsBefore, 'to-do card did not perform the authorized exact read');
        await until(() => visible('textarea').find((element) => element.value === agedTodo.text), 'exact to-do outside the loaded list did not open');
      }));
      checks.push(await run('the Cmd K main agent action raises the agent over Chat', async () => {
        await choose('launch', `hit:channel:${launch}`);
        await until(() => modal('Chat'), 'Chat did not open before the main agent action');
        await choose('Open your main agent', 'action:assistant');
        await until(() => visible('[role="dialog"]').some((element) => element.textContent.includes('Juniper')) && !modal('Chat'), 'main agent did not come to the foreground');
      }));
    }
    if (batch === 'report') {
      const reportRace = async (name, lookupFirst) => {
        checks.push(await run(name, async () => {
          let releaseCreate = () => {};
          let releaseItem = () => {};
          const postsBefore = reportPosts;
          const patchesBefore = reportPatches.length;
          const failuresBefore = failedItemResponses;
          try {
            await choose('Spring launch plan', `hit:report:${report}`);
            await until(() => modal('Reports'), 'Reports did not open for the create race');
            await clickText('button', 'New report');
            const title = await until(() => visible('textarea[aria-label="Title"]')[0], 'new report editor did not open');
            const initialTitle = lookupFirst ? 'Lookup finishes before create' : 'Create finishes before lookup';
            const continuedTitle = `${initialTitle}, with later edits`;
            setValue(title, initialTitle);
            reportCreateGate = new Promise((resolve) => { releaseCreate = resolve; });
            holdReportCreate = true;
            reportCreateHeld = false;
            (await until(() => exactText('button', 'Create report'), 'Create report action did not appear')).click();
            await until(() => reportCreateHeld, 'report create was not held');
            setValue(title, continuedTitle);
            itemGate = new Promise((resolve) => { releaseItem = resolve; });
            holdItem = true;
            failItem = true;
            itemHeld = false;
            await choose('Bugs to triage', `hit:list:${list}`);
            await until(() => itemHeld, 'newer failed lookup was not held');
            const existingToasts = new Set(visible('div,p,span').filter((element) => element.textContent.trim() === 'That link is no longer available.'));
            const freshFailure = async () => {
              await until(() => failedItemResponses > failuresBefore, 'newer failed lookup response did not return');
              await until(() => visible('div,p,span').some((element) => element.textContent.trim() === 'That link is no longer available.' && !existingToasts.has(element)), 'newer lookup did not show its own failure');
            };
            if (lookupFirst) {
              releaseItem();
              await freshFailure();
              if (!title.isConnected || title.value !== continuedTitle || !modal('Reports')) throw new Error('failed lookup discarded the report draft');
              releaseCreate();
            } else {
              releaseCreate();
              await until(() => exactText('button', 'Done') && title.isConnected, 'saved report ID was not reconciled while the lookup was pending');
              if (title.value !== continuedTitle) throw new Error('edits made during create were lost');
              releaseItem();
              await freshFailure();
            }
            await until(() => exactText('button', 'Done') && title.isConnected, 'created report did not remain in its editor');
            await until(() => reportPatches.length > patchesBefore, 'later edits were not patched to the saved report');
            await wait(250);
            if (reportPosts !== postsBefore + 1) throw new Error('the same draft was posted more than once');
            if (reportPatches.length !== patchesBefore + 1) throw new Error('the saved draft produced unexpected patch churn');
            const patch = reportPatches.at(-1);
            if (patch.id !== savedReport.id || patch.body.title !== continuedTitle) throw new Error('later edits were not patched to the reconciled report ID');
            if (!modal('Reports') || title.value !== continuedTitle) throw new Error('the failed lookup replaced the saved report editor');
          } finally {
            releaseCreate();
            releaseItem();
            holdReportCreate = false;
            holdItem = false;
            failItem = false;
          }
        }));
      };
      await reportRace('a create completed before a newer failed lookup keeps the saved ID and later edits', false);
      await reportRace('a newer failed lookup completed before create still reconciles the saved ID once', true);
    }
    if (batch === 'mobile') {
      checks.push(await run('mobile Back to the library cancels a held exact lookup', async () => {
        if (innerWidth > 500) throw new Error(`mobile batch ran at ${innerWidth}px wide`);
        let releaseItem = () => {};
        try {
          await choose('Spring launch plan', `hit:report:${report}`);
          const back = await until(() => visible('button[aria-label="Back to the library"]')[0], 'mobile Back to the library did not appear');
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'held exact list lookup did not start');
          back.click();
          await until(() => !visible('button[aria-label="Back to the library"]').length && modal('Reports'), 'Back did not return to the report library');
          releaseItem();
          await wait(500);
          if (!modal('Reports') || modal('Your lists') || new URL(location.href).searchParams.has('list')) throw new Error('late exact lookup replaced the report library');
        } finally {
          releaseItem();
          holdItem = false;
        }
      }));
    }
    if (batch === 'selections') {
      checks.push(await run('the organization chip cancels a held exact lookup', async () => {
        let releaseItem = () => {};
        try {
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'held exact list lookup did not start');
          const chip = await until(() => visible('button[title="Switch organization"]')[0], 'organization chip did not appear');
          chip.click();
          await until(() => modal('Organizations'), 'organization chip did not open the switcher');
          releaseItem();
          await wait(500);
          if (!modal('Organizations') || modal('Your lists') || new URL(location.href).searchParams.has('list')) throw new Error('late exact lookup replaced the organization switcher');
        } finally {
          releaseItem();
          holdItem = false;
        }
      }));
      if (window.matchMedia('(min-width: 1024px)').matches) checks.push(await run('the side chat agent picker cancels a held exact lookup', async () => {
        let releaseItem = () => {};
        try {
          await choose('Spring launch plan', `hit:report:${report}`);
          await until(() => modal('Reports'), 'Reports did not open for the side chat race');
          const toggle = visible('button[aria-label="Talk to an agent here"]')[0];
          if (toggle) toggle.click();
          const picker = await until(() => visible('button[aria-label="Agent to talk to"]')[0], 'side chat agent picker did not appear');
          const previous = picker.textContent.trim();
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'held exact list lookup did not start');
          picker.click();
          const nextAgent = previous.includes('Pixel') ? 'Forge' : 'Pixel';
          const option = await until(() => visible('[role="option"]').find((element) => element.textContent.trim() === nextAgent), 'another side chat agent did not appear');
          const selected = option.textContent.trim();
          option.click();
          await until(() => picker.textContent.trim() === selected, 'side chat agent did not change');
          releaseItem();
          await wait(500);
          if (!modal('Reports') || modal('Your lists') || picker.textContent.trim() !== selected) throw new Error('late exact lookup replaced the side chat selection');
        } finally {
          releaseItem();
          holdItem = false;
        }
      }));
      checks.push(await run('a held Tell cannot open the main agent over a newer report', async () => {
        let releaseTell = () => {};
        try {
          tellGate = new Promise((resolve) => { releaseTell = resolve; });
          holdTell = true;
          tellHeld = false;
          openPalette();
          await paletteInput('Navigation fixture Tell held');
          const tell = await until(() => paletteHit('tell:main'), 'Tell action did not appear');
          tell.click();
          await until(() => tellHeld, 'Tell send was not held');
          await paletteInput('Spring launch plan');
          const newer = await until(() => paletteHit(`hit:report:${report}`), 'newer report result did not appear');
          newer.click();
          await until(() => !visible('[cmdk-input]').length && modal('Reports'), 'newer report did not open');
          releaseTell();
          await wait(500);
          if (!modal('Reports') || modal('Talk to Juniper')) throw new Error('held Tell opened the main agent over the newer report');
        } finally {
          releaseTell();
          holdTell = false;
        }
      }));
      checks.push(await run('a current Tell opens the main agent after sending', async () => {
        const postsBefore = tellPosts;
        openPalette();
        await paletteInput('Navigation fixture Tell current');
        const tell = await until(() => paletteHit('tell:main'), 'Tell action did not appear');
        tell.click();
        await until(() => !visible('[cmdk-input]').length && modal('Talk to Juniper'), 'current Tell did not open the main agent');
        if (tellPosts !== postsBefore + 1) throw new Error('current Tell was not sent exactly once');
      }));
    }
    if (batch === 'mobile-chat') {
      checks.push(await run('mobile Back to chats cancels a held exact lookup', async () => {
        if (innerWidth !== 390 || innerHeight !== 844) throw new Error(`mobile chat batch ran at ${innerWidth}x${innerHeight}`);
        let releaseItem = () => {};
        try {
          await choose('launch', `hit:channel:${launch}`);
          const back = await until(() => visible('button[aria-label="Back to chats"]')[0], 'mobile Back to chats did not appear');
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'held exact list lookup did not start');
          back.click();
          await until(() => !visible('button[aria-label="Back to chats"]').length && visible('button[data-chat-item]').length, 'Back did not return to the chat list');
          releaseItem();
          await wait(500);
          if (!modal('Chat') || modal('Your lists') || !visible('button[data-chat-item]').length || new URL(location.href).searchParams.has('list')) throw new Error('late exact lookup replaced the chat list');
        } finally {
          releaseItem();
          holdItem = false;
        }
      }));
    }
    if (batch === 'board') {
      const openResidents = async () => {
        await choose('Open quests', 'action:quests');
        const board = await until(() => modal('Quest board'), 'Quest board did not open');
        const residents = await until(() => visible('[role="tab"]').find((element) => element.textContent.trim().startsWith('Residents')), 'Residents tab did not appear');
        residents.click();
        await until(() => residents.getAttribute('aria-selected') === 'true', 'Residents tab did not select');
        return board;
      };
      const raceBoardAction = async (name, action, expected) => {
        checks.push(await run(name, async () => {
          let releaseItem = () => {};
          try {
            await openResidents();
            itemGate = new Promise((resolve) => { releaseItem = resolve; });
            holdItem = true;
            itemHeld = false;
            await choose('Bugs to triage', `hit:list:${list}`);
            await until(() => itemHeld, 'held exact list lookup did not start');
            const control = await until(action, 'Board action did not appear');
            control.click();
            await until(expected, 'Board action did not reach its destination');
            releaseItem();
            await wait(500);
            if (!expected()) throw new Error('late exact lookup displaced the Board destination');
            if (modal('Your lists') || new URL(location.href).searchParams.has('list')) throw new Error('late exact lookup replaced the Board action');
          } finally {
            releaseItem();
            holdItem = false;
          }
        }));
      };
      await raceBoardAction(
        'New Agent wins over a held exact lookup',
        () => exactText('button', 'New agent'),
        () => modal('New agent'),
      );
      modal('New agent')?.querySelector('button[aria-label="Close"]')?.click();
      await until(() => !modal('New agent'), 'New agent did not close');
      await raceBoardAction(
        'a place Visit wins over a held exact lookup',
        () => visible('button[aria-label^="Visit "]')[0],
        () => !modal('Quest board') && document.activeElement?.tagName === 'CANVAS',
      );
      let visitedResident = '';
      await raceBoardAction(
        'a resident Visit wins over a held exact lookup',
        () => {
          const control = exactText('button', 'Visit');
          visitedResident = control?.closest('li')?.querySelector('p span')?.textContent.trim() ?? '';
          return control;
        },
        () => !modal('Quest board') && (document.activeElement?.tagName === 'CANVAS' || Boolean(visitedResident && modal(`Talk to ${visitedResident}`))),
      );
      checks.push(await run('an uppercase exact team link leaves Quick Chat for its canonical team', async () => {
        await choose('Quick chat with an agent', 'action:quick');
        await until(() => modal('Quick chat'), 'Quick Chat did not open');
        const readsBefore = exactTeamReads;
        window.dispatchEvent(new CustomEvent('world:open-link', { detail: { kind: 'team', id: exactTeam.toUpperCase() } }));
        const board = await until(() => modal('Quest board'), 'exact team did not open the Quest board');
        const teams = await until(() => visible('[role="tab"]').find((element) => element.textContent.trim().startsWith('Teams')), 'Teams tab did not appear');
        await until(() => teams.getAttribute('aria-selected') === 'true', 'exact team did not select the Teams tab');
        await until(() => exactText('p', fakeTeam.name), 'authorized exact team missing from the focused view');
        await until(() => !modal('Quick chat'), 'Quick Chat still covered the exact team');
        if (exactTeamReads !== readsBefore + 1) throw new Error('exact team did not use one canonical authorized read');
        if (new URL(location.href).searchParams.get('team') !== exactTeam) throw new Error('exact team URL was not canonical');
        if (!board.contains(teams)) throw new Error('Teams tab was not inside the foreground board');
      }));
    }
    if (batch === 'foreground') {
      const openNewAgent = async (name) => {
        await choose('New agent', 'action:new-agent');
        const dialog = await until(() => modal('New agent'), 'New agent did not open');
        setValue(await until(() => dialog.querySelector('input[placeholder="Rook"]'), 'New agent name field did not appear'), name);
        setValue(await until(() => dialog.querySelector('[aria-label="Persona"]'), 'New agent persona field did not appear'), 'Tests navigation foreground ordering without creating a real resident.');
        return until(() => {
          const create = exactText('button', 'Create bot');
          return create && !create.disabled ? create : null;
        }, 'New agent form did not become ready');
      };
      const closeReportsOver = async (under, description, escape = false, whileCovered = () => {}) => {
        await choose('Open reports', 'action:reports');
        const reportsPanel = await until(() => modal('Reports'), `Reports did not open over ${description}`);
        if (!under.isConnected) throw new Error(`${description} unmounted when Reports opened`);
        if (under.getClientRects().length) throw new Error(`${description} stayed visible behind Reports`);
        if (!under.closest('[inert]')) throw new Error(`${description} was not inert behind Reports`);
        await whileCovered(reportsPanel);
        if (escape) window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', bubbles: true, cancelable: true }));
        else reportsPanel.querySelector('button[aria-label="Close"]').click();
        await until(() => under.isConnected && under.getClientRects().length, `${description} did not return after Reports closed`);
        return under;
      };
      checks.push(await run('Open notifications wins over a held exact lookup', async () => {
        let releaseItem = () => {};
        try {
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'held exact list lookup did not start');
          await choose('Open notifications', 'action:notifications');
          await until(() => modal('Notifications'), 'Notifications did not open');
          releaseItem();
          await wait(500);
          if (!modal('Notifications') || modal('Your lists')) throw new Error('late exact lookup replaced Notifications');
        } finally {
          releaseItem();
          holdItem = false;
        }
      }));
      modal('Notifications')?.querySelector('button[aria-label="Close"]')?.click();
      await until(() => !modal('Notifications'), 'Notifications did not close');
      const agentRace = async (name, createFirst) => {
        checks.push(await run(name, async () => {
          let releaseCreate = () => {};
          let releaseItem = () => {};
          const createdName = createFirst ? 'Create First Fixture' : 'Lookup First Fixture';
          const returnedBefore = agentCreateReturns;
          try {
            agentCreateGate = new Promise((resolve) => { releaseCreate = resolve; });
            holdAgentCreate = true;
            agentCreateHeld = false;
            const create = await openNewAgent(createdName);
            create.click();
            await until(() => agentCreateHeld, 'New agent POST was not held');
            itemGate = new Promise((resolve) => { releaseItem = resolve; });
            holdItem = true;
            itemHeld = false;
            await choose('Bugs to triage', `hit:list:${list}`);
            await until(() => itemHeld, 'newer exact list lookup was not held');
            const rosterBefore = agentRosterReads;
            if (createFirst) {
              releaseCreate();
              await until(() => agentCreateReturns > returnedBefore, 'New agent POST did not complete');
              await until(() => agentRosterReads > rosterBefore, 'New agent completion did not refresh the roster');
              if (modal(`Talk to ${createdName}`)) throw new Error('older New agent completion came to the foreground');
              releaseItem();
            } else {
              releaseItem();
              await until(() => modal('Your lists') && new URL(location.href).searchParams.get('list') === list, 'newer exact list did not open');
              releaseCreate();
              await until(() => agentCreateReturns > returnedBefore, 'New agent POST did not complete');
              await until(() => agentRosterReads > rosterBefore, 'New agent completion did not refresh the roster');
            }
            await until(() => modal('Your lists') && new URL(location.href).searchParams.get('list') === list, 'newer exact list did not remain in front');
            if (modal(`Talk to ${createdName}`)) throw new Error('older New agent completion replaced the newer list');
          } finally {
            releaseCreate();
            releaseItem();
            holdAgentCreate = false;
            holdItem = false;
          }
        }));
        const stranded = visible('[role="dialog"]').find((element) => /^Talk to (Create First|Lookup First) Fixture$/.test(element.getAttribute('aria-label') ?? ''));
        stranded?.querySelector('button[aria-label="Close"]')?.click();
        modal('New agent')?.querySelector('button[aria-label="Close"]')?.click();
        await wait(150);
      };
      await agentRace('a New Agent completion before a newer lookup cannot replace it and still refreshes the roster', true);
      await agentRace('a newer lookup completion before New Agent cannot be replaced and still refreshes the roster', false);
      checks.push(await run('a stale successful New Agent form survives a newer failed lookup without submitting twice', async () => {
        let releaseCreate = () => {};
        let releaseItem = () => {};
        const returnedBefore = agentCreateReturns;
        const failuresBefore = failedItemResponses;
        try {
          agentCreateGate = new Promise((resolve) => { releaseCreate = resolve; });
          holdAgentCreate = true;
          agentCreateHeld = false;
          const create = await openNewAgent('Failed Lookup Fixture');
          create.click();
          await until(() => agentCreateHeld, 'New agent POST was not held');
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          failItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'newer failed exact lookup was not held');
          releaseCreate();
          await until(() => agentCreateReturns > returnedBefore, 'New agent POST did not complete');
          const form = await until(() => modal('New agent')?.querySelector('form'), 'stale successful New agent form did not remain mounted');
          const created = await until(() => exactText('button', 'Created'), 'New agent form did not reconcile its successful create');
          if (!created.disabled) throw new Error('reconciled New agent form could submit again');
          releaseItem();
          await until(() => failedItemResponses > failuresBefore, 'newer failed lookup did not finish');
          await wait(300);
          if (!modal('New agent') || !created.isConnected) throw new Error('newer failed lookup discarded the reconciled New agent form');
          form.requestSubmit();
          await wait(250);
          if (agentCreateReturns !== returnedBefore + 1) throw new Error('reconciled New agent form created a duplicate resident');
          modal('New agent').querySelector('button[aria-label="Close"]').click();
          await until(() => !modal('New agent'), 'reconciled New agent form could not close');
        } finally {
          releaseCreate();
          releaseItem();
          holdAgentCreate = false;
          holdItem = false;
          failItem = false;
        }
      }));
      checks.push(await run('a current New Agent completion raises the resident over mounted Chat', async () => {
        const createdName = 'Current Agent Fixture';
        await choose('launch', `hit:channel:${launch}`);
        const chatPanel = await until(() => modal('Chat'), 'Chat did not open under New agent');
        const returnedBefore = agentCreateReturns;
        const rosterBefore = agentRosterReads;
        holdAgentCreate = false;
        agentCreateHeld = false;
        const create = await openNewAgent(createdName);
        create.click();
        await until(() => agentCreateReturns > returnedBefore, 'current New agent POST did not complete');
        await until(() => modal(`Talk to ${createdName}`), 'created resident did not come to the foreground');
        await until(() => agentRosterReads > rosterBefore, 'current New agent completion did not refresh the roster');
        if (!chatPanel.isConnected) throw new Error('Chat was unmounted under the created resident');
        if (modal('Chat')) throw new Error('Chat still covered the created resident');
      }));
      checks.push(await run('Reports restores the focused List as the same mounted panel', async () => {
        await choose('Bugs to triage', `hit:list:${list}`);
        const listsPanel = await until(() => modal('Your lists'), 'focused List did not open');
        await until(() => listsPanel.textContent.includes(sourceList.name), 'focused List title did not appear');
        const restored = await closeReportsOver(listsPanel, 'the focused List');
        if (modal('Your lists') !== restored || !restored.textContent.includes(sourceList.name)) throw new Error('Reports reopened a different List panel or lost its selection');
      }));
      checks.push(await run('Reports preserves a Lists draft in the same mounted panel', async () => {
        await choose('Open your lists', 'action:lists');
        const listsPanel = await until(() => modal('Your lists'), 'Lists did not open for the draft check');
        const allLists = exactText('button', 'All lists');
        if (allLists) allLists.click();
        await clickText('button', 'New list');
        const title = await until(() => visible('input[aria-label="List name"]')[0], 'List draft did not open');
        const items = await until(() => visible('textarea[aria-label="List items, one per line"]')[0], 'List draft items did not appear');
        setValue(title, 'Reports retention draft');
        setValue(items, 'First retained item\nSecond retained item');
        const restored = await closeReportsOver(listsPanel, 'the Lists draft');
        if (modal('Your lists') !== restored || !title.isConnected || !items.isConnected) throw new Error('Reports reopened a different Lists draft');
        if (title.value !== 'Reports retention draft' || items.value !== 'First retained item\nSecond retained item') throw new Error('Reports discarded the Lists draft');
        await clickText('button', 'Cancel');
        await until(() => !visible('input[aria-label="List name"]').length, 'List draft did not cancel');
      }));
      checks.push(await run('Reports Escape restores a selected to-do without closing or saving it', async () => {
        await choose('Thank the beta testers', `hit:todo:${sourceTodo.id}`);
        const detail = await until(() => modal('To-do'), 'To-do detail did not open');
        const text = await until(() => detail.querySelector('textarea[aria-label="To-do text"]'), 'To-do detail text did not appear');
        const patchesBefore = todoPatches;
        if (text.value !== sourceTodo.text) throw new Error('Cmd K did not focus the exact seeded to-do');
        await closeReportsOver(detail, 'the selected to-do', true);
        if (modal('To-do') !== detail || text.value !== sourceTodo.text) throw new Error('Reports Escape closed or changed the hidden to-do');
        if (todoPatches !== patchesBefore) throw new Error('Reports Escape saved the hidden to-do');
        detail.querySelector('button[aria-label="Close"]').click();
        await until(() => !modal('To-do'), 'To-do detail did not close after restoration');
        if (todoPatches !== patchesBefore) throw new Error('unchanged restored to-do was saved on close');
      }));
      checks.push(await run('Reports restores the selected Board tab as the same mounted panel', async () => {
        await choose('Open quests', 'action:quests');
        const boardPanel = await until(() => modal('Quest board'), 'Quest board did not open');
        const residents = await until(() => visible('[role="tab"]').find((element) => element.textContent.trim().startsWith('Residents')), 'Residents tab did not appear');
        residents.click();
        await until(() => residents.getAttribute('aria-selected') === 'true', 'Residents tab did not select');
        const restored = await closeReportsOver(boardPanel, 'the Quest board', true);
        if (modal('Quest board') !== restored || residents.getAttribute('aria-selected') !== 'true') throw new Error('Reports reopened a different Board panel or lost its tab');
      }));
      checks.push(await run('Reports hides a quest status portal and restores the selected Board row', async () => {
        await choose('Open quests', 'action:quests');
        const boardPanel = await until(() => modal('Quest board'), 'Quest board did not open for the selected quest check');
        const quests = await until(() => visible('[role="tab"]').find((element) => element.textContent.trim().startsWith('Quests')), 'Quests tab did not appear');
        quests.click();
        await until(() => quests.getAttribute('aria-selected') === 'true', 'Quests tab did not select');
        const row = await until(() => boardPanel.querySelector(`[data-quest="${selectedQuest}"]`), 'seeded quest row did not appear');
        const select = await until(() => row.querySelector('button[aria-label="Select quest"]'), 'seeded quest select action did not appear');
        select.click();
        await until(() => select.getAttribute('aria-label') === 'Deselect quest' && select.getAttribute('aria-pressed') === 'true' && exactText('span', '1 selected'), 'seeded quest row did not select');
        boardPanel.querySelector(`[data-quest-status="${selectedQuest}"]`).click();
        await until(() => visible('[cmdk-input]').find((element) => element.getAttribute('placeholder') === 'Change status'), 'quest status picker did not open');
        const restored = await closeReportsOver(boardPanel, 'the selected Quest board', true, () => {
          if (visible('[cmdk-input]').some((element) => element.getAttribute('placeholder') === 'Change status')) throw new Error('quest status picker stayed visible above Reports');
        });
        if (modal('Quest board') !== restored || select.getAttribute('aria-label') !== 'Deselect quest' || select.getAttribute('aria-pressed') !== 'true' || !exactText('span', '1 selected')) throw new Error('Reports lost the selected Board row');
        if (visible('[cmdk-input]').some((element) => element.getAttribute('placeholder') === 'Change status')) {
          window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', bubbles: true, cancelable: true }));
          await until(() => !visible('[cmdk-input]').some((element) => element.getAttribute('placeholder') === 'Change status'), 'restored status picker did not close');
        }
        const clear = await until(() => visible('button[aria-label="Clear selection"]')[0], 'Clear selection did not appear');
        clear.click();
        await until(() => select.getAttribute('aria-label') === 'Select quest' && select.getAttribute('aria-pressed') === 'false', 'selected Board row did not clear');
      }));
      checks.push(await run('Reports restores an exact selected quest as the same mounted room', async () => {
        await choose('Redesign checkout drawer', `hit:quest:${selectedQuest}`);
        const questRoom = await until(() => modal('Quest room'), 'exact quest room did not open');
        await until(() => questRoom.textContent.includes('Redesign checkout drawer'), 'exact quest was not selected');
        const restored = await closeReportsOver(questRoom, 'the exact quest room');
        if (modal('Quest room') !== restored || !restored.textContent.includes('Redesign checkout drawer')) throw new Error('Reports reopened a different quest room or lost its selection');
      }));
    }
    if (batch === 'mutation-races') {
      window.confirm = () => true;
      checks.push(await run('Settings Devices wins over a held exact checklist lookup', async () => {
        let releaseItem = () => {};
        try {
          await choose('Open settings', 'action:you');
          const settings = await until(() => modal('Settings'), 'Settings did not open');
          const devicesSelector = innerWidth <= 500 ? 'button[data-nav="devices"]' : 'button[data-side="devices"]';
          const devices = await until(() => visible(devicesSelector).find((button) => settings.contains(button)), 'Devices did not appear in Settings navigation');
          itemGate = new Promise((resolve) => { releaseItem = resolve; });
          holdItem = true;
          itemHeld = false;
          await choose('Bugs to triage', `hit:list:${list}`);
          await until(() => itemHeld, 'exact checklist lookup was not held');
          devices.click();
          await until(() => devices.getAttribute('aria-current') === 'page' && settings.textContent.includes('Agents use these computers for repositories and coding tools.'), 'Devices did not select inside Settings');
          releaseItem();
          await wait(350);
          if (modal('Settings') !== settings || devices.getAttribute('aria-current') !== 'page') throw new Error('late checklist lookup replaced Settings Devices');
          if (modal('Your lists') || new URL(location.href).searchParams.has('list')) throw new Error('late checklist lookup opened Lists over Settings Devices');
          settings.querySelector('button[aria-label="Close"]')?.click();
          await until(() => !modal('Settings'), 'Settings did not close');
        } finally {
          releaseItem();
          holdItem = false;
        }
      }));
      if (innerWidth > 500) {
      const openReports = async () => {
        await choose('Open reports', 'action:reports');
        const panel = await until(() => modal('Reports'), 'Reports did not open');
        await until(() => panel.querySelector(`button[data-report="${mutationReportB}"]`), 'mutation report fixtures did not appear');
        return panel;
      };
      const selectReport = async (panel, id, title) => {
        const row = await until(() => panel.querySelector(`button[data-report="${id}"]`), `${title} row did not appear`);
        row.click();
        await until(() => exactText('h2', title), `${title} did not become the report reader`);
        return row;
      };
      const expectReportB = (panel) => {
        const row = panel.querySelector(`button[data-report="${mutationReportB}"]`);
        return row?.getAttribute('aria-current') === 'true' && exactText('h2', 'Mutation fixture B');
      };
      const prepareMutation = (key, detailId = null) => {
        let releaseMutation = () => {};
        let releaseDetail = () => {};
        reportMutationGate = new Promise((resolve) => { releaseMutation = resolve; });
        reportDetailGate = new Promise((resolve) => { releaseDetail = resolve; });
        heldReportMutation = key;
        holdReportDetail = detailId;
        reportMutationHeld = false;
        reportDetailHeld = false;
        return { releaseMutation, releaseDetail };
      };
      const panel = await openReports();
      checks.push(await run('a late Pin response cannot replace a newer report selection', async () => {
        const row = await selectReport(panel, pinReport, 'Pin mutation fixture A');
        const { releaseMutation, releaseDetail } = prepareMutation(`PATCH:${pinReport}`, pinReport);
        try {
          const pin = await until(() => visible('button[aria-label="Pin"]').find((button) => panel.contains(button)), 'Pin did not appear');
          pin.click();
          await until(() => reportMutationHeld, 'Pin response was not held');
          await selectReport(panel, mutationReportB, 'Mutation fixture B');
          releaseMutation();
          await until(() => row.querySelector('[aria-label="Pinned"]'), 'Pin did not refresh the report collection');
          if (reportDetailHeld && !expectReportB(panel)) throw new Error('late Pin detail read replaced report B before release');
          releaseDetail();
          await wait(300);
          if (!expectReportB(panel)) throw new Error('late Pin response replaced report B');
        } finally {
          releaseMutation();
          releaseDetail();
          heldReportMutation = null;
          holdReportDetail = null;
        }
      }));
      checks.push(await run('a current Unpin updates its reader and collection', async () => {
        const row = await selectReport(panel, pinReport, 'Pin mutation fixture A');
        const unpin = await until(() => visible('button[aria-label="Unpin"]').find((button) => panel.contains(button)), 'Unpin did not appear');
        unpin.click();
        await until(() => visible('button[aria-label="Pin"]').find((button) => panel.contains(button)), 'current Unpin did not refresh the reader');
        await until(() => !row.querySelector('[aria-label="Pinned"]'), 'current Unpin did not refresh the collection');
      }));
      checks.push(await run('reselecting the same report owns its reader over a late Pin', async () => {
        const row = await selectReport(panel, deleteReport, 'Delete mutation fixture A');
        const { releaseMutation } = prepareMutation(`PATCH:${deleteReport}`);
        try {
          const pin = await until(() => visible('button[aria-label="Pin"]').find((button) => panel.contains(button)), 'Pin did not appear for same-report reselection');
          pin.click();
          await until(() => reportMutationHeld, 'same-report Pin response was not held');
          const readsBefore = mutationDetailReads.get(deleteReport) ?? 0;
          row.click();
          await until(() => (mutationDetailReads.get(deleteReport) ?? 0) > readsBefore, 'same-report reselection did not read its detail');
          const readsAfterReselect = mutationDetailReads.get(deleteReport) ?? 0;
          releaseMutation();
          await until(() => row.querySelector('[aria-label="Pinned"]'), 'late Pin did not refresh the report collection');
          await wait(300);
          if (row.getAttribute('aria-current') !== 'true' || !exactText('h2', 'Delete mutation fixture A')) throw new Error('late Pin displaced the same-report reselection');
          if ((mutationDetailReads.get(deleteReport) ?? 0) !== readsAfterReselect) throw new Error('late Pin issued another detail refresh after same-report reselection');
        } finally {
          releaseMutation();
          heldReportMutation = null;
          holdReportDetail = null;
        }
      }));
      checks.push(await run('a late Delete response keeps report B selected and refreshes the collection', async () => {
        await selectReport(panel, deleteReport, 'Delete mutation fixture A');
        const { releaseMutation } = prepareMutation(`DELETE:${deleteReport}`);
        try {
          const remove = await until(() => visible('button[aria-label="Delete"]').find((button) => panel.contains(button)), 'Delete did not appear');
          remove.click();
          await until(() => reportMutationHeld, 'Delete response was not held');
          await selectReport(panel, mutationReportB, 'Mutation fixture B');
          releaseMutation();
          await until(() => !panel.querySelector(`button[data-report="${deleteReport}"]`), 'Delete did not refresh the report collection');
          await wait(250);
          if (!expectReportB(panel)) throw new Error('late Delete response cleared report B');
        } finally {
          releaseMutation();
          heldReportMutation = null;
          holdReportDetail = null;
        }
      }));
      checks.push(await run('a late Restore response cannot replace a newer report selection', async () => {
        const row = await selectReport(panel, restoreReport, 'Restore mutation fixture A');
        const history = await until(() => visible('button[aria-label="History"]').find((button) => panel.contains(button)), 'History did not appear');
        history.click();
        const restore = await until(() => visible('button[aria-label="Restore revision 1"]').find((button) => panel.contains(button)), 'Restore revision did not appear');
        const { releaseMutation, releaseDetail } = prepareMutation(`RESTORE:${restoreReport}`, restoreReport);
        try {
          restore.click();
          await until(() => reportMutationHeld, 'Restore response was not held');
          await selectReport(panel, mutationReportB, 'Mutation fixture B');
          releaseMutation();
          await until(() => row.textContent.includes('Restored mutation fixture A'), 'Restore did not refresh the report collection');
          if (reportDetailHeld && !expectReportB(panel)) throw new Error('late Restore detail read replaced report B before release');
          releaseDetail();
          await wait(300);
          if (!expectReportB(panel)) throw new Error('late Restore response replaced report B');
        } finally {
          releaseMutation();
          releaseDetail();
          heldReportMutation = null;
          holdReportDetail = null;
        }
      }));
      }
    }
    if (batch === 'quest-catchup') {
      checks.push(await run('a prefetched quest catches up after subscribing', async () => {
        let releasePrefetch = () => {};
        try {
          questPrefetchGate = new Promise((resolve) => { releasePrefetch = resolve; });
          questPrefetchHeld = false;
          questCatchupUrl = null;
          await choose('Redesign checkout drawer', `hit:quest:${selectedQuest}`);
          await until(() => questPrefetchHeld, 'quest room prefetch was not held');
          if (modal('Quest room')) throw new Error('quest room mounted before its prefetch completed');
          window.dispatchEvent(new CustomEvent('world-event', { detail: questCatchupEvent }));
          releasePrefetch();
          const room = await until(() => modal('Quest room'), 'quest room did not open after the prefetch completed');
          await until(() => questCatchupUrl, 'quest room did not request a catch-up page after subscribing');
          if (questCatchupUrl.searchParams.get('after') !== String(sourceQuestRoom.room.cursor)) throw new Error('quest room catch-up did not continue from the prefetched cursor');
          if (questSince && questCatchupUrl.searchParams.get('since') !== questSince) throw new Error('quest room catch-up did not continue from the newest prefetched message');
          await until(() => room.textContent.includes('Quest room · Done'), 'quest room did not render the caught-up completed status');
          await until(() => room.querySelector(`[data-message-id="${questCatchupMessage.id}"]`)?.textContent.includes(questCatchupMessage.body), 'quest room did not render the caught-up message');
          await until(() => [...room.querySelectorAll('span')].some((element) => element.textContent.includes('finished the quest')), 'quest room did not render the caught-up completion event');
        } finally {
          releasePrefetch();
        }
      }));
    }
    if (batch === 'detail-races') {
      const openLibrary = async (panel) => {
        if (innerWidth > 500 || panel.querySelector(`button[data-report="${detailReportA}"]`)?.getClientRects().length) return;
        const back = await until(() => visible('button[aria-label="Back to the library"]').find((button) => panel.contains(button)), 'Back to the library did not appear');
        back.click();
        await until(() => panel.querySelector(`button[data-report="${detailReportA}"]`)?.getClientRects().length, 'report library did not return');
      };
      const openReports = async () => {
        await choose('Open reports', 'action:reports');
        const panel = await until(() => modal('Reports'), 'Reports did not open');
        await openLibrary(panel);
        await until(() => panel.querySelector(`button[data-report="${detailReportA}"]`), 'detail report fixtures did not appear');
        return panel;
      };
      const selectReport = async (panel, id, title) => {
        await openLibrary(panel);
        const row = await until(() => visible(`button[data-report="${id}"]`).find((button) => panel.contains(button)), `${title} row did not appear`);
        row.click();
        await until(() => exactText('h2', title), `${title} did not become the report reader`);
      };
      const openHistory = async (panel) => {
        const history = await until(() => visible('button[aria-label="History"]').find((button) => panel.contains(button)), 'History did not appear');
        if (history.getAttribute('aria-pressed') !== 'true') history.click();
        await until(() => visible('section[aria-label="Revision history"]').find((section) => panel.contains(section)), 'revision history did not open');
      };
      const revisionButton = async (panel, revision) => {
        const label = await until(() => visible('section[aria-label="Revision history"] span').find((span) => panel.contains(span) && span.textContent.trim() === `r${revision}`), `revision ${revision} did not appear`);
        return label.closest('button');
      };
      const revisionTitle = (panel, title) => visible('section[aria-label="Revision history"] span').find((span) => panel.contains(span) && span.textContent.includes(title));
      const panel = await openReports();
      checks.push(await run('a late revision list for report A cannot replace report B history', async () => {
        let releaseA = () => {};
        try {
          await selectReport(panel, detailReportA, 'Detail fixture A');
          revisionListGate = new Promise((resolve) => { releaseA = resolve; });
          holdRevisionList = detailReportA;
          revisionListHeld = false;
          await openHistory(panel);
          await until(() => revisionListHeld, 'report A revision list was not held');
          await selectReport(panel, detailReportB, 'Detail fixture B');
          holdRevisionList = null;
          await openHistory(panel);
          await until(() => revisionTitle(panel, 'Detail fixture B old'), 'report B revisions did not load');
          releaseA();
          await wait(300);
          if (revisionTitle(panel, 'Detail fixture A old')) throw new Error('late report A revision list replaced report B history');
          if (!exactText('h2', 'Detail fixture B') || !revisionTitle(panel, 'Detail fixture B old')) throw new Error('report B history did not remain current');
        } finally {
          releaseA();
          holdRevisionList = null;
        }
      }));
      checks.push(await run('a late revision detail for report A cannot replace report B', async () => {
        let releaseA = () => {};
        try {
          await selectReport(panel, detailReportA, 'Detail fixture A');
          await openHistory(panel);
          revisionDetailGate = new Promise((resolve) => { releaseA = resolve; });
          holdRevisionDetail = `${detailReportA}:1`;
          revisionDetailHeld = false;
          (await revisionButton(panel, 1)).click();
          await until(() => revisionDetailHeld, 'report A revision detail was not held');
          await selectReport(panel, detailReportB, 'Detail fixture B');
          releaseA();
          await wait(300);
          if (!exactText('h2', 'Detail fixture B') || document.body.textContent.includes('Old Detail fixture A body.')) throw new Error('late report A revision detail replaced report B');
        } finally {
          releaseA();
          holdRevisionDetail = null;
        }
      }));
      if (innerWidth >= 768) {
        checks.push(await run('clearing a revision view wins over its late detail', async () => {
          let releaseA = () => {};
          try {
            await selectReport(panel, detailReportA, 'Detail fixture A');
            await openHistory(panel);
            revisionDetailGate = new Promise((resolve) => { releaseA = resolve; });
            holdRevisionDetail = `${detailReportA}:1`;
            revisionDetailHeld = false;
            (await revisionButton(panel, 1)).click();
            await until(() => revisionDetailHeld, 'report A revision detail was not held');
            (await revisionButton(panel, 2)).click();
            releaseA();
            await wait(300);
            if (!exactText('h2', 'Detail fixture A') || document.body.textContent.includes('Old Detail fixture A body.')) throw new Error('late revision detail reopened a cleared revision');
          } finally {
            releaseA();
            holdRevisionDetail = null;
          }
        }));
      }
      panel.querySelector('button[aria-label="Close"]')?.click();
      await until(() => !modal('Reports'), 'Reports did not close after detail races');
      const openAgentCard = async () => {
        const cardPanel = await openReports();
        const row = await until(() => visible(`button[data-report="${detailAgentCardReport}"]`).find((button) => cardPanel.contains(button)), 'Agent card fixture row did not appear');
        includeExactAgentInList = true;
        row.click();
        return until(() => [...cardPanel.querySelectorAll('button')].find((button) => button.textContent.includes('Exact Card Agent')), 'exact agent card did not render');
      };
      checks.push(await run('a late agent card fallback cannot replace a newer destination', async () => {
        let releaseAgent = () => {};
        try {
          const card = await openAgentCard();
          exactAgentGate = new Promise((resolve) => { releaseAgent = resolve; });
          holdExactAgent = true;
          exactAgentHeld = false;
          card.click();
          await until(() => exactAgentHeld, 'agent card exact fallback was not held');
          await choose('Open your lists', 'action:lists');
          await until(() => modal('Your lists'), 'newer Lists navigation did not open');
          releaseAgent();
          await wait(300);
          if (!modal('Your lists') || modal('Talk to Exact Card Agent')) throw new Error('late agent card fallback replaced Lists');
        } finally {
          releaseAgent();
          holdExactAgent = false;
        }
      }));
      checks.push(await run('an agent card opens through its authorized exact fallback', async () => {
        modal('Your lists')?.querySelector('button[aria-label="Close"]')?.click();
        await until(() => !modal('Your lists'), 'Lists did not close before the current agent card');
        holdExactAgent = false;
        exactAgentHeld = false;
        const card = await openAgentCard();
        const readsBefore = exactAgentReads;
        card.click();
        await until(() => exactAgentReads > readsBefore, 'agent card did not perform an exact read');
        await until(() => modal('Talk to Exact Card Agent'), 'exact agent did not open from its card');
        modal('Talk to Exact Card Agent').querySelector('button[aria-label="Close"]')?.click();
        await until(() => !modal('Talk to Exact Card Agent'), 'exact agent dialog did not close');
      }));
    }
    if (batch === 'visibility') {
      const setDocumentVisibility = (value) => {
        Object.defineProperty(document, 'visibilityState', { configurable: true, value });
        document.dispatchEvent(new Event('visibilitychange'));
      };
      const openLaunch = async () => {
        await choose('launch', `hit:channel:${launch}`);
        await until(() => modal('Chat') && exactText('header p', launchDetail.name), 'launch channel did not open');
      };
      checks.push(await run('only the visible channel conversation clears unread state', async () => {
        const ownVisibility = Object.getOwnPropertyDescriptor(document, 'visibilityState');
        let releaseMobileRead = () => {};
        let releaseHiddenRead = () => {};
        try {
          setDocumentVisibility('visible');
          markReads = [];
          await openLaunch();
          await until(() => markReads.includes(fakeMessage), 'visible channel conversation did not mark messages read');
          if (innerWidth <= 500) {
            messagesGate = new Promise((resolve) => { releaseMobileRead = resolve; });
            holdMessages = true;
            messagesHeld = false;
            markReads = [];
            window.dispatchEvent(new CustomEvent('world-event', { detail: { seq: 990101, type: 'channel.message', agent_id: null, task_id: null, run_id: null, session_id: null, payload: { channel_id: launch, message_id: fakeMessage }, created_at: now } }));
            await until(() => messagesHeld, 'mobile channel response was not held');
            const back = await until(() => visible('button[aria-label="Back to chats"]')[0], 'mobile Back to chats did not appear');
            back.click();
            await until(() => !visible('button[aria-label="Back to chats"]').length && visible('button[data-chat-item]').length, 'mobile chat list did not replace the conversation');
            releaseMobileRead();
            await wait(350);
            if (markReads.includes(fakeMessage)) throw new Error('mobile chat list marked the hidden conversation read');
            holdMessages = false;
            const launchRow = await until(() => visible('button[data-chat-item]').find((element) => element.textContent.trim().startsWith(launchDetail.name)), 'launch channel did not appear in the mobile chat list');
            launchRow.click();
            await until(() => visible('button[aria-label="Back to chats"]')[0], 'mobile launch conversation did not return');
            await until(() => markReads.includes(fakeMessage), 'restored mobile conversation did not mark messages read');
          }
          messagesGate = new Promise((resolve) => { releaseHiddenRead = resolve; });
          holdMessages = true;
          messagesHeld = false;
          markReads = [];
          window.dispatchEvent(new CustomEvent('world-event', { detail: { seq: 990102, type: 'channel.message', agent_id: null, task_id: null, run_id: null, session_id: null, payload: { channel_id: launch, message_id: fakeMessage }, created_at: now } }));
          await until(() => messagesHeld, 'hidden-tab channel response was not held');
          setDocumentVisibility('hidden');
          releaseHiddenRead();
          await wait(350);
          if (markReads.includes(fakeMessage)) throw new Error('hidden browser tab marked the channel read');
          holdMessages = false;
          setDocumentVisibility('visible');
          await until(() => markReads.includes(fakeMessage), 'visible browser tab did not catch up the channel receipt');
        } finally {
          releaseMobileRead();
          releaseHiddenRead();
          holdMessages = false;
          if (ownVisibility) Object.defineProperty(document, 'visibilityState', ownVisibility);
          else delete document.visibilityState;
          document.dispatchEvent(new Event('visibilitychange'));
        }
      }));
      checks.push(await run('Give a quest reaches the visible agent surface once', async () => {
        await choose('Open chat', 'action:chat');
        const chat = await until(() => modal('Chat'), 'Chat did not open');
        if (!visible('button[data-chat-item]').some((element) => element.textContent.includes('Juniper'))) {
          const back = await until(() => visible('button[aria-label="Back to chats"]')[0], 'mobile chat list could not be reached');
          back.click();
        }
        const agentRow = await until(() => visible('button[data-chat-item]').find((element) => element.textContent.includes('Juniper')), 'Juniper did not appear in the chat list');
        agentRow.click();
        const embedded = await until(() => visible('[role="region"][aria-label="Talk to Juniper"]')[0], 'embedded Juniper conversation did not open');
        const talk = await until(() => [...embedded.querySelectorAll('[role="tab"]')].find((element) => element.textContent.trim() === 'Talk'), 'embedded Talk tab did not appear');
        if (talk.getAttribute('aria-selected') !== 'true') talk.click();
        await until(() => talk.getAttribute('aria-selected') === 'true', 'embedded Talk tab did not select');
        await choose('Give Juniper a quest', `give:${mainAgent}`);
        const standalone = await until(() => modal('Talk to Juniper'), 'visible Juniper dialog did not open');
        const quests = await until(() => [...standalone.querySelectorAll('[role="tab"]')].find((element) => element.textContent.trim() === 'Quests'), 'visible Quests tab did not appear');
        await until(() => quests.getAttribute('aria-selected') === 'true', 'visible agent surface did not receive the quest request');
        if (talk.getAttribute('aria-selected') !== 'true') throw new Error('hidden embedded agent consumed the quest request');
        standalone.querySelector('button[aria-label="Close"]')?.click();
        await until(() => modal('Chat') === chat && visible('[role="region"][aria-label="Talk to Juniper"]')[0], 'closing the agent did not restore Chat');
        if (talk.getAttribute('aria-selected') !== 'true') throw new Error('restored embedded conversation lost its selected tab');
      }));
      checks.push(await run('a thread focus interrupted by Cmd K reloads and focuses on reveal', async () => {
        let release = () => {};
        try {
          agentThreadReads = 0;
          agentThreadHeld = false;
          holdAgentThread = true;
          agentThreadGate = new Promise((resolve) => { release = resolve; });
          await choose('visibility exact reply cobalt', `hit:message:${agentReplyId}`);
          await until(() => agentThreadHeld, 'mounted agent thread read was not held');
          openPalette();
          await paletteInput('Juniper');
          release();
          await wait(350);
          if (!visible('[cmdk-input]').length) throw new Error('Cmd K closed while the thread read completed');
          visible('button[aria-label="Close command palette"]')[0]?.click();
          await until(() => !visible('[cmdk-input]').length, 'Cmd K did not close after the held thread read');
          await until(() => agentThreadReads > 2, 'restored thread did not rerun its discarded read');
          const reply = await until(() => visible(`aside[aria-label="Thread"] [data-message-id="${agentReplyId}"]`).find((element) => element.classList.contains('ring-w-gold')), 'restored thread did not focus the exact reply');
          if (!reply.textContent.includes(agentReply.body)) throw new Error('restored thread focused the wrong reply');
        } finally {
          release();
          holdAgentThread = false;
        }
      }));
    }
    if (batch === 'agent-panel-races') {
      const closePanel = async (name) => {
        const panel = modal(name);
        panel?.querySelector('button[aria-label="Close"]')?.click();
        if (panel) await until(() => !modal(name), `${name} did not close`);
      };
      const holdChecklist = async () => {
        let release = () => {};
        itemGate = new Promise((resolve) => { release = resolve; });
        holdItem = true;
        itemHeld = false;
        await choose('Bugs to triage', `hit:list:${list}`);
        await until(() => itemHeld, 'exact checklist response was not held');
        return release;
      };
      checks.push(await run('a session sidebar pick invalidates a held exact lookup', async () => {
        let release = () => {};
        try {
          await choose('Open session history', 'action:history');
          const panel = await until(() => modal('Session history'), 'Session history did not open');
          const row = await until(() => [...panel.querySelectorAll('ul button')].find((button) => button.getClientRects().length && button.querySelector('.font-semibold')?.textContent.trim()), 'session sidebar did not load a selectable session');
          const title = row.querySelector('.font-semibold').textContent.trim();
          release = await holdChecklist();
          row.click();
          await until(() => exactText('h3', title), 'session sidebar pick did not open its session');
          release();
          await wait(350);
          if (!modal('Session history') || !exactText('h3', title)) throw new Error('late exact lookup replaced the session sidebar pick');
        } finally {
          release();
          holdItem = false;
          await closePanel('Your lists');
          await closePanel('Session history');
        }
      }));
      for (const label of ['Quests', 'Settings']) {
        checks.push(await run(`the ${label} agent tab invalidates a held exact lookup`, async () => {
          let release = () => {};
          try {
            await choose('Juniper', `agent:${mainAgent}`);
            const dialog = await until(() => modal('Talk to Juniper'), 'Juniper did not open');
            const tab = await until(() => [...dialog.querySelectorAll('[role="tab"]')].find((button) => button.textContent.trim() === label), `${label} tab did not appear`);
            release = await holdChecklist();
            tab.click();
            await until(() => tab.getAttribute('aria-selected') === 'true', `${label} tab did not select`);
            release();
            await wait(350);
            if (!modal('Talk to Juniper') || tab.getAttribute('aria-selected') !== 'true') throw new Error(`late exact lookup replaced the ${label} tab`);
          } finally {
            release();
            holdItem = false;
            await closePanel('Your lists');
            await closePanel('Talk to Juniper');
          }
        }));
      }
      checks.push(await run('a covered agent conversation does not mark incoming messages seen', async () => {
        let reports;
        try {
          await choose('Juniper', `agent:${mainAgent}`);
          let dialog = await until(() => modal('Talk to Juniper'), 'Juniper did not open for the receipt race');
          const talk = await until(() => [...dialog.querySelectorAll('[role="tab"]')].find((button) => button.textContent.trim() === 'Talk'), 'Talk tab did not appear');
          if (talk.getAttribute('aria-selected') !== 'true') talk.click();
          await until(() => talk.getAttribute('aria-selected') === 'true', 'Talk tab did not select');
          await until(() => agentMessageReads > 0, 'agent conversation did not load messages');
          await wait(200);
          await choose('Open reports', 'action:reports');
          reports = await until(() => modal('Reports'), 'Reports did not cover the agent conversation');
          const hideSide = visible('button[aria-label="Hide agent chat"]')[0];
          hideSide?.click();
          if (hideSide) await until(() => !visible('button[aria-label="Hide agent chat"]').length && !modal('Talk to Juniper'), 'Reports side chat did not close');
          if (modal('Talk to Juniper')) throw new Error('Juniper remained visible beside Reports');
          await wait(150);
          const seenBeforeCover = agentSeenRequests;
          const readsBeforeEvent = agentMessageReads;
          includeIncomingAgentMessage = true;
          window.dispatchEvent(new CustomEvent('world-event', {
            detail: {
              seq: 990001,
              type: 'message.created',
              agent_id: mainAgent,
              task_id: null,
              run_id: null,
              session_id: null,
              payload: { message_id: incomingAgentMessage.id, author_kind: 'agent', author_agent_id: mainAgent, body: incomingAgentMessage.body },
              created_at: incomingAgentMessage.created_at,
            },
          }));
          await until(() => agentMessageReads > readsBeforeEvent, 'covered agent conversation did not refresh its incoming message');
          await wait(250);
          const seenWhileCovered = agentSeenRequests;
          reports.querySelector('button[aria-label="Close"]')?.click();
          reports = null;
          dialog = await until(() => modal('Talk to Juniper'), 'agent conversation did not return after Reports closed');
          await until(() => dialog.textContent.includes(incomingAgentMessage.body), 'refreshed agent message did not appear after reveal');
          if (seenWhileCovered === seenBeforeCover) await until(() => agentSeenRequests > seenWhileCovered, 'revealed agent conversation did not mark messages seen');
          if (seenWhileCovered !== seenBeforeCover) throw new Error('covered agent conversation marked an incoming message seen');
        } finally {
          reports?.querySelector('button[aria-label="Close"]')?.click();
          includeIncomingAgentMessage = false;
          await closePanel('Talk to Juniper');
        }
      }));
    }
    if (batch === 'async-starts') {
      const closePalette = async () => {
        const close = await until(() => visible('button[aria-label="Close command palette"]')[0], 'Cmd K close action did not appear');
        close.click();
        await until(() => !visible('[cmdk-input]').length, 'Cmd K did not close');
      };
      const expectAgentInPalette = async (agent) => {
        openPalette();
        await paletteInput(agent.name);
        await until(() => paletteHit(`agent:${agent.id}`), `${agent.name} did not appear in Cmd K`);
        await closePalette();
      };
      const openMainAgent = async () => {
        await choose('Juniper', `agent:${mainAgent}`);
        return until(() => modal('Talk to Juniper'), 'Juniper did not open');
      };
      const confirmFork = async (dialog) => {
        const button = await until(() => visible(`button[aria-label="Fork Juniper"]`).find((element) => dialog.contains(element)), 'Fork action did not appear');
        button.click();
        await until(() => button.textContent.includes('Fork a private copy?'), 'Fork action did not ask for confirmation');
        button.click();
      };
      if (innerWidth >= 768) {
        checks.push(await run('a current Fork opens its copy and refreshes the resident roster', async () => {
          holdFork = false;
          forkHeld = false;
          const returnedBefore = forkReturns;
          const rosterBefore = forkRosterReads;
          const dialog = await openMainAgent();
          await confirmFork(dialog);
          await until(() => forkReturns > returnedBefore, 'current Fork did not finish');
          const copy = [...createdAgents.values()].at(-1);
          await until(() => modal(`Talk to ${copy.name}`), 'current Fork did not open its copy');
          await until(() => forkRosterReads > rosterBefore, 'current Fork did not refresh the resident roster');
          await expectAgentInPalette(copy);
          const copyDialog = modal(`Talk to ${copy.name}`);
          copyDialog?.querySelector('button[aria-label="Close"]')?.click();
          if (copyDialog) await until(() => !modal(`Talk to ${copy.name}`), 'current Fork copy did not close');
        }));
        checks.push(await run('a delayed Fork refreshes the roster without taking focus from Reports', async () => {
          let releaseFork = () => {};
          try {
            forkGate = new Promise((resolve) => { releaseFork = resolve; });
            holdFork = true;
            forkHeld = false;
            const returnedBefore = forkReturns;
            const rosterBefore = forkRosterReads;
            const dialog = await openMainAgent();
            await confirmFork(dialog);
            await until(() => forkHeld, 'Fork response was not held');
            await choose('Open reports', 'action:reports');
            await until(() => modal('Reports'), 'Reports did not open over the held Fork');
            const unavailableBefore = visible('[role="status"] span').filter((element) => element.textContent.trim() === 'That link is no longer available.').length;
            releaseFork();
            await until(() => forkReturns > returnedBefore, 'held Fork did not finish');
            const copy = [...createdAgents.values()].at(-1);
            await until(() => forkRosterReads > rosterBefore, 'held Fork did not refresh the resident roster');
            await wait(300);
            if (!modal('Reports') || modal(`Talk to ${copy.name}`)) throw new Error('held Fork took focus from Reports');
            const unavailableAfter = visible('[role="status"] span').filter((element) => element.textContent.trim() === 'That link is no longer available.').length;
            if (unavailableAfter !== unavailableBefore) throw new Error('held Fork attempted a stale navigation');
            await expectAgentInPalette(copy);
            if (!modal('Reports') || modal(`Talk to ${copy.name}`)) throw new Error('checking the refreshed roster displaced Reports');
            modal('Reports').querySelector('button[aria-label="Close"]').click();
            await until(() => !modal('Reports'), 'Reports did not close after the held Fork');
          } finally {
            releaseFork();
            holdFork = false;
          }
        }));
      }
      const openQuick = async () => {
        await choose('Quick chat with an agent', 'action:quick');
        const dialog = await until(() => modal('Quick chat'), 'Quick Chat did not open');
        await until(() => [...dialog.querySelectorAll('button')].find((button) => button.textContent.trim().includes(savedQuickChat.title)), 'saved Quick Chat fixture did not appear');
        return dialog;
      };
      const pickQuick = async (dialog, title) => {
        const button = await until(() => [...dialog.querySelectorAll('button')].find((element) => element.textContent.trim().includes(title)), `${title} did not appear`);
        button.click();
      };
      const startQuick = async (dialog, text) => {
        await pickQuick(dialog, 'New chat');
        const input = await until(() => visible('[aria-label="Message"]').find((element) => dialog.contains(element) && element.getAttribute('placeholder')?.startsWith('Ask ')), 'new Quick Chat message field did not appear');
        setValue(input, text);
        const send = await until(() => {
          const button = input.closest('form')?.querySelector('button[type="submit"]');
          return button && !button.disabled ? button : null;
        }, 'Quick Chat Send did not become ready');
        send.click();
      };
      checks.push(await run('a delayed first Quick Chat message cannot replace a saved chat selection', async () => {
        let releaseQuick = () => {};
        try {
          quickStartGate = new Promise((resolve) => { releaseQuick = resolve; });
          holdQuickStart = true;
          quickStartHeld = false;
          const dialog = await openQuick();
          await startQuick(dialog, 'Held quick start fixture');
          await until(() => quickStartHeld, 'first Quick Chat response was not held');
          await pickQuick(dialog, savedQuickChat.title);
          await until(() => exactText('p', savedQuickMessage.body), 'saved Quick Chat did not load');
          releaseQuick();
          await until(() => exactText('span', 'Held fixture chat'), 'completed Quick Chat did not appear in the chat list');
          await wait(300);
          if (!exactText('p', savedQuickMessage.body)) throw new Error('held first message replaced the saved Quick Chat');
          if (exactText('p', 'Held quick start fixture')) throw new Error('held first message came to the foreground');
        } finally {
          releaseQuick();
          holdQuickStart = false;
        }
      }));
      checks.push(await run('a current first Quick Chat message selects its returned session', async () => {
        holdQuickStart = false;
        quickStartHeld = false;
        const dialog = modal('Quick chat') ?? await openQuick();
        await startQuick(dialog, 'Current quick start fixture');
        await until(() => exactText('p', 'Current quick start fixture'), 'current first Quick Chat message did not appear');
        await until(() => exactText('span', 'Current fixture chat'), 'current Quick Chat did not appear in the chat list');
        if (exactText('p', savedQuickMessage.body)) throw new Error('current Quick Chat left the saved conversation selected');
      }));
    }
    if (batch === 'feature-links') {
      checks.push(await run('a chat change link opens the exact Workshop diff without starting a terminal', async () => {
        class FixtureChannel {
          onmessage = () => {};
        }
        const fixtureTauri = {
          core: {
            Channel: FixtureChannel,
            invoke: async (command, args) => {
              workshopCalls.push(command);
              if (command === 'desktop_status') return { available: true, runner_id: null, name: 'Fixture Mac', status: 'idle', server: location.origin, workspace_root: '/fixture', repositories: [], backends: [], last_error: null, features: ['workbench'] };
              if (command === 'wb_open') return { key: 'tasks/canonical-feature-link-fixture', root: '/fixture', name: 'fixture', is_git: true, branch: 'feature', base: 'main', head: 'fixture' };
              if (command === 'wb_changes') {
                workshopChangeReads += 1;
                workshopModeReads += 1;
                if (workshopChangeMode === 'initial' && workshopModeReads === 1) throw new Error('fixture change read failed');
                if (workshopChangeMode === 'normal' && workshopModeReads === 1) return { base: 'main', merge_base: 'base', files: [] };
                return { base: 'main', merge_base: 'base', files: [{ path: workshopFile, status: 'M', old_path: null }] };
              }
              if (command === 'wb_read') return { text: 'export const featureLinkFixture = true;\n', sha: 'fixture-sha', size: 40, binary: false, too_large: false };
              if (command === 'wb_base') return { exists: true, text: 'export const featureLinkFixture = false;\n', binary: false };
              if (command === 'wb_list') return [];
              if (command === 'wb_watch') return undefined;
              if (command === 'wb_unwatch') return undefined;
              throw new Error(`Unexpected Workshop command: ${command}`);
            },
          },
        };
        await choose('Open chat', 'action:chat');
        const chat = await until(() => modal('Chat'), 'Chat did not open');
        if (!visible('button[data-chat-item]').some((element) => element.textContent.includes('Juniper'))) {
          const back = await until(() => visible('button[aria-label="Back to chats"]')[0], 'chat list could not be reached');
          back.click();
        }
        const agentRow = await until(() => visible('button[data-chat-item]').find((element) => element.textContent.includes('Juniper')), 'Juniper did not appear in the chat list');
        agentRow.click();
        const embedded = await until(() => chat.querySelector('[role="region"][aria-label="Talk to Juniper"]'), 'Juniper conversation did not open');
        const changes = await until(() => exactText('button', 'View changes'), 'View changes did not appear in chat');
        window.__TAURI__ = fixtureTauri;
        changes.click();
        const workshop = await until(() => modal('Workshop'), 'Workshop did not open from chat');
        if (!workshopSessionQueries.some((query) => new URLSearchParams(query).get('id') === workshopSession)) throw new Error('the exact Workshop session was not requested');
        const tab = await until(() => workshopChangeReads === 2 && visible('[role="tab"]').find((element) => element.getAttribute('title') === workshopFile && element.getAttribute('aria-selected') === 'true' && element.textContent.includes('diff')), 'the first changed file did not open');
        if (workshopChangeReads !== 2) throw new Error(`the failed change read retried ${workshopChangeReads - 1} times`);
        if (tab.getAttribute('aria-selected') !== 'true' || !tab.textContent.includes('diff')) throw new Error('the changed file did not open in diff mode');
        if (!exactText('button', 'Diff')?.matches('[aria-pressed="true"]')) throw new Error('the diff view was not selected');
        if (workshopCalls.some((command) => command.startsWith('term_'))) throw new Error('opening the diff started a terminal');
        if (!workshop.isConnected || !embedded.isConnected) throw new Error('Workshop navigation replaced the chat instead of opening over it');
        const opens = workshopCalls.filter((command) => command === 'wb_open').length;
        let redispatched = 0;
        const onRedispatch = () => { redispatched += 1; };
        window.addEventListener('world:open-workbench', onRedispatch);
        changes.click();
        await wait(100);
        window.removeEventListener('world:open-workbench', onRedispatch);
        if (redispatched) throw new Error('the same-session diff request reached the close guard');
        if (modal('Workshop') !== workshop || workshopCalls.filter((command) => command === 'wb_open').length !== opens) throw new Error('the same-session diff request reopened the Workshop');
        workshopChangeMode = 'normal';
        workshopModeReads = 0;
        workshop.querySelector('button[aria-label="Close workshop"]')?.click();
        await until(() => !modal('Workshop'), 'Workshop did not close before the normal-open check');
        localStorage.setItem(workshopLayoutKey, JSON.stringify({ terminal: false }));
        window.dispatchEvent(new CustomEvent('world:open-workbench', { detail: { sessionId: workshopSession } }));
        const normalWorkshop = await until(() => modal('Workshop'), 'the normal Workshop did not open');
        await until(() => workshopModeReads === 1, 'the normal Workshop did not load its empty changes');
        changes.click();
        const refreshedTab = await until(() => workshopModeReads === 2 && visible('[role="tab"]').find((element) => element.getAttribute('title') === workshopFile && element.getAttribute('aria-selected') === 'true' && element.textContent.includes('diff')), 'the direct link did not refresh the empty Workshop');
        if (workshopModeReads !== 2 || refreshedTab.getAttribute('aria-selected') !== 'true' || !refreshedTab.textContent.includes('diff')) throw new Error('the direct link did not own the refreshed diff result');
        if (modal('Workshop') !== normalWorkshop) throw new Error('the direct link remounted the normal Workshop');
      }));
    }
    if (batch === 'workspace-link') {
      const workspaceSurface = async () => until(() => modal('Workspace'), 'Workspace did not open');
      const workspaceLibrary = async () => until(() => modal('Workspaces'), 'Workspaces library did not open');
      const closeSurface = async () => {
        const surface = modal('Workspace') ?? modal('Workspaces');
        surface?.querySelector('button[aria-label="Close"]')?.click();
        await until(() => !modal('Workspace') && !modal('Workspaces'), 'Workspaces did not close');
      };
      const openExactWorkspace = async () => {
        window.dispatchEvent(new CustomEvent('world:open-workspace', { detail: { id: workspaceCanvas, item: workspaceItem } }));
        const surface = await workspaceSurface();
        await until(() => visible(`.react-flow__node[data-id="${workspaceItem}"].selected`)[0], 'the explicit workspace item was not focused');
        return surface;
      };
      const hideSideChat = async () => {
        const hide = visible('button[aria-label="Hide agent chat"]')[0];
        if (!hide) return;
        hide.click();
        await until(() => !visible('aside[aria-label^="Talk to "]').length, 'the workspace side chat did not close');
      };
      const backToLibrary = async (surface) => {
        const back = await until(() => [...surface.querySelectorAll('header button')].find((element) => element.getClientRects().length && (element.title === 'Back to all workspaces' || element.textContent.trim() === 'All workspaces')), 'All workspaces did not appear');
        back.click();
        return workspaceLibrary();
      };
      const assertLibraryInteractive = async (surface, description) => {
        const search = await until(() => {
          const field = surface.querySelector('input[aria-label="Search workspaces"]');
          if (!(field instanceof HTMLInputElement) || !field.getClientRects().length || getComputedStyle(field).visibility !== 'visible') return null;
          const library = field.closest('.flex.min-h-0.w-full.flex-col');
          if (!(library instanceof HTMLElement) || library.inert || library.getAttribute('aria-hidden') === 'true') return null;
          const bounds = field.getBoundingClientRect();
          const hit = document.elementFromPoint(bounds.left + bounds.width / 2, bounds.top + bounds.height / 2);
          if (hit !== field && !field.contains(hit)) return null;
          return field;
        }, `${description} did not make workspace search visible and interactive`);
        search.focus();
        if (document.activeElement !== search) throw new Error(`${description} could not focus workspace search`);
        return search;
      };
      const assertDockLabels = async (description) => {
        const nav = await until(() => {
          const hotbar = document.querySelector('nav[aria-label="Hotbar"]');
          return hotbar instanceof HTMLElement && hotbar.getClientRects().length && getComputedStyle(hotbar).visibility === 'visible' ? hotbar : null;
        }, `${description} did not show the dock`);
        const buttons = [...nav.querySelectorAll('button')].filter((button) => button.getClientRects().length);
        if (!buttons.length) throw new Error(`${description} had no dock labels`);
        const navBounds = nav.getBoundingClientRect();
        for (const button of buttons) {
          const label = button.querySelector(':scope > span.whitespace-normal');
          if (!(label instanceof HTMLSpanElement)) throw new Error(`${description} could not find the label for ${button.getAttribute('aria-label')}`);
          const buttonBounds = button.getBoundingClientRect();
          const labelBounds = label.getBoundingClientRect();
          if (label.scrollWidth > label.clientWidth + 1) throw new Error(`${description} clipped ${label.textContent.trim()}`);
          if (labelBounds.left < buttonBounds.left - 1 || labelBounds.right > buttonBounds.right + 1 || labelBounds.top < buttonBounds.top - 1 || labelBounds.bottom > buttonBounds.bottom + 1) throw new Error(`${description} placed ${label.textContent.trim()} outside its button`);
          if (buttonBounds.left < navBounds.left - 1 || buttonBounds.right > navBounds.right + 1 || buttonBounds.top < navBounds.top - 1 || buttonBounds.bottom > navBounds.bottom + 1) throw new Error(`${description} placed ${label.textContent.trim()} outside the dock`);
          const style = getComputedStyle(label);
          if (style.textOverflow === 'ellipsis' || style.whiteSpace === 'nowrap' || style.overflow === 'hidden') throw new Error(`${description} truncated ${label.textContent.trim()}`);
        }
        const expectedMainAgent = 'An unusually long main agent display name that still fits';
        const main = buttons.find((button) => button.getAttribute('aria-keyshortcuts') === 'P');
        if (!main || !main.textContent.includes(expectedMainAgent)) throw new Error(`${description} did not render the full long main-agent name`);
      };
      checks.push(await run('B returns a board selected in the Workspaces library to the library', async () => {
        window.dispatchEvent(new CustomEvent('world:open-workspace'));
        const library = await workspaceLibrary();
        const row = await until(() => library.querySelector(`button[data-report="${libraryCanvas}"]`), 'the flat workspace fixture did not appear in the library');
        row.click();
        await until(() => modal('Workspaces')?.querySelector(`.react-flow__node[data-id="${libraryItem}"]`), 'the library did not open the workspace as a board');
        if (modal('Workspace')) throw new Error('the library selection unexpectedly entered workspace mode');
        document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB', bubbles: true, cancelable: true }));
        const returned = await workspaceLibrary();
        await until(() => !returned.querySelector(`.react-flow__node[data-id="${libraryItem}"]`) && returned.querySelector('input[data-find-input="true"]'), 'B did not clear the selected library board');
        await assertLibraryInteractive(returned, 'B from a selected board');
      }));
      checks.push(await run('a shared workspace link opens its exact focused item from chat', async () => {
        const agentName = document.querySelector('nav[aria-label="Hotbar"] button[aria-keyshortcuts="P"] span:not(:empty)')?.textContent.trim() ?? 'Juniper';
        await choose('Open chat', 'action:chat');
        const chat = await until(() => modal('Chat'), 'Chat did not open');
        if (!visible('button[data-chat-item]').some((element) => element.textContent.includes(agentName))) {
          const back = await until(() => visible('button[aria-label="Back to chats"]')[0], 'chat list could not be reached');
          back.click();
        }
        const agentRow = await until(() => visible('button[data-chat-item]').find((element) => element.textContent.includes(agentName)), `${agentName} did not appear in the chat list`);
        agentRow.click();
        const embedded = await until(() => [...chat.querySelectorAll('[role="region"]')].find((element) => element.getAttribute('aria-label') === `Talk to ${agentName}`), `${agentName} conversation did not open`);
        const open = await until(() => [...embedded.querySelectorAll('button')].find((element) => element.textContent.includes('Workspace') && element.textContent.includes(workspaceReport.title)), 'the shared workspace card did not appear in chat');
        if (localStorage.getItem(workspaceLayoutKey) !== null) throw new Error('the workspace already had a saved layout before the shared link opened');
        open.click();
        const workspace = await until(() => modal('Workspace'), 'the shared link did not open in workspace mode');
        await until(() => workspace.querySelector('button[aria-label="Hide channels"]'), 'the workspace channel control did not appear');
        await until(() => [...workspace.querySelectorAll('header button')].find((element) => element.getClientRects().length && (element.title === 'Back to all workspaces' || element.textContent.trim() === 'All workspaces')), 'the workspace header controls did not appear');
        await until(() => visible(`.react-flow__node[data-id="${workspaceItem}"].selected`)[0], 'the shared workspace item was not focused');
      }));
      checks.push(await run('All workspaces and generic Cmd K and B requests leave an exact board for the library', async () => {
        const linked = await workspaceSurface();
        await backToLibrary(linked);
        let exactReads = workspaceDetailReads;
        await openExactWorkspace();
        if (workspaceDetailReads <= exactReads) throw new Error('the explicit workspace request did not read its exact board');
        exactReads = workspaceDetailReads;
        await choose('Open workspaces', 'action:workspace');
        await assertLibraryInteractive(await workspaceLibrary(), 'Cmd K from an exact board');
        if (workspaceDetailReads !== exactReads) throw new Error('Cmd K reopened the remembered board instead of the library');
        await openExactWorkspace();
        exactReads = workspaceDetailReads;
        document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB', bubbles: true, cancelable: true }));
        await assertLibraryInteractive(await workspaceLibrary(), 'B from an exact board');
        if (workspaceDetailReads !== exactReads) throw new Error('B reopened the remembered board instead of the library');
      }));
      checks.push(await run('dock, Cmd K and B ignore valid and deleted remembered boards after they close', async () => {
        const actions = [
          ['dock', async (dock) => dock.click()],
          ['Cmd K', () => choose('Open workspaces', 'action:workspace')],
          ['B', async () => document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB', bubbles: true, cancelable: true }))],
        ];
        try {
          for (const deleted of [false, true]) {
            for (const [label, open] of actions) {
              const state = deleted ? 'deleted remembered board' : 'valid remembered board';
              workspaceAvailable = true;
              await openExactWorkspace();
              await until(() => {
                try {
                  return JSON.parse(localStorage.getItem(workspaceLayoutKey) ?? '{}').last === workspaceCanvas;
                } catch {
                  return false;
                }
              }, `${label} from a ${state} did not persist the remembered board`);
              await closeSurface();
              const chat = modal('Chat');
              if (chat) {
                chat.querySelector('button[aria-label="Close"]')?.click();
                await until(() => !modal('Chat'), `${label} from a ${state} could not close the restored Chat`);
              }
              const dock = await until(() => {
                const button = document.querySelector('nav[aria-label="Hotbar"] button[aria-label="Workspaces"]');
                if (!(button instanceof HTMLButtonElement) || !button.getClientRects().length || button.closest('[aria-hidden="true"]')) return null;
                const style = getComputedStyle(button);
                const navStyle = getComputedStyle(button.closest('nav'));
                return style.visibility === 'visible' && navStyle.visibility === 'visible' && style.pointerEvents !== 'none' ? button : null;
              }, `${label} from a ${state} did not return to the visible HUD`);
              workspaceAvailable = !deleted;
              const exactReads = workspaceDetailReads;
              await open(dock);
              const library = await until(() => modal('Workspaces'), `${label} from a ${state} did not open the Workspaces library`);
              await assertLibraryInteractive(library, `${label} from a ${state}`);
              if (workspaceDetailReads !== exactReads) throw new Error(`${label} tried to open the ${state}`);
            }
          }
        } finally {
          workspaceAvailable = true;
        }
      }));
      checks.push(await run('every dock label fits at wide and constrained desktop sizes', async () => {
        await closeSurface();
        const chat = modal('Chat');
        chat?.querySelector('button[aria-label="Close"]')?.click();
        if (chat) await until(() => !modal('Chat'), 'Chat did not close before the dock check');
        await useViewport(1440, 900);
        try {
          await assertDockLabels('the wide desktop dock');
          await useViewport(1024, 820);
          await assertDockLabels('the constrained desktop dock');
        } finally {
          if (innerWidth !== 1440 || innerHeight !== 900) await useViewport(1440, 900);
        }
      }));
      checks.push(await run('the saved reader preference cannot hide the compact workspace library', async () => {
        window.dispatchEvent(new CustomEvent('world:open-workspace'));
        let librarySurface = await workspaceLibrary();
        await hideSideChat();
        const row = await until(() => librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`), 'the workspace row did not appear before the compact check');
        row.click();
        await until(() => librarySurface.querySelector(`.react-flow__node[data-id="${libraryItem}"]`), 'the workspace reader did not open before saving the library preference');
        const hideLibrary = await until(() => visible('button[aria-label="Hide the library"]')[0], 'the wide library toggle did not appear');
        hideLibrary.click();
        await until(() => visible('button[aria-label="Show the library"]')[0], 'the hidden reader preference did not save');
        document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB', bubbles: true, cancelable: true }));
        librarySurface = await workspaceLibrary();
        let search = await assertLibraryInteractive(librarySurface, 'B with a saved hidden reader preference');
        const showChat = await until(() => [...librarySurface.querySelectorAll('button')].find((element) => element.getAttribute('aria-label') === 'Talk to an agent here'), 'the side chat toggle did not appear');
        showChat.click();
        await until(() => visible('aside[aria-label^="Talk to "]')[0], 'the side chat did not open');
        try {
          await useViewport(1024, 820);
          const rootRem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
          await until(() => librarySurface.getBoundingClientRect().width < 42 * rootRem, 'the real desktop resize did not make Workspaces compact');
          search = await assertLibraryInteractive(librarySurface, 'the resized workspace library with side chat');
          setValue(search, 'library workspace');
          await until(() => librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`), 'the compact search did not find its workspace');
          search.focus();
          await useKey('ArrowDown', () => document.activeElement?.matches(`button[data-report="${libraryCanvas}"]`), 'Arrow Down did not move from search into the compact workspace list');
          await useKey('Enter', () => Boolean(modal('Workspaces')?.querySelector(`.react-flow__node[data-id="${libraryItem}"]`)), 'Enter did not open the focused compact workspace');
          await useViewport(1440, 900);
          await until(() => visible('button[aria-label="Show the library"]')[0], 'widening the reader did not restore its saved library preference');
          document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB', bubbles: true, cancelable: true }));
          librarySurface = await workspaceLibrary();
          await assertLibraryInteractive(librarySurface, 'B after widening a hidden reader');
        } finally {
          if (innerWidth !== 1440 || innerHeight !== 900) await useViewport(1440, 900);
          await hideSideChat();
        }
      }));
      checks.push(await run('active workspace searches refresh after edits, events and deletion', async () => {
        document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB', bubbles: true, cancelable: true }));
        const librarySurface = await workspaceLibrary();
        const search = await assertLibraryInteractive(librarySurface, 'the active-query fixture');
        setValue(search, 'library workspace');
        let row = await until(() => librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`), 'the rename fixture did not match its starting query');
        row.click();
        await until(() => librarySurface.querySelector(`.react-flow__node[data-id="${libraryItem}"]`), 'the rename fixture reader did not open');
        const rename = await until(() => [...librarySurface.querySelectorAll('button')].find((button) => button.title === 'Rename' && button.textContent.includes(currentLibraryReport.title)), 'the workspace rename control did not appear');
        rename.click();
        const name = await until(() => librarySurface.querySelector('input[aria-label="Workspace name"]'), 'the workspace name field did not appear');
        setValue(name, 'Renamed workspace fixture');
        name.blur();
        await until(() => currentLibraryReport.title === 'Renamed workspace fixture', 'the workspace rename did not reach the fixture API');
        const showLibrary = await until(() => visible('button[aria-label="Show the library"]')[0], 'the saved hidden preference did not expose its Show library control');
        showLibrary.click();
        await until(() => !librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`) && librarySurface.textContent.includes('No workspaces match.'), 'renaming a workspace did not refresh the active query');
        if (librarySurface.querySelector('[role="alert"]') || librarySurface.textContent.includes('Searching the library')) throw new Error('the empty workspace result showed another feedback state');

        setValue(search, 'edited cobalt');
        await until(() => librarySurface.textContent.includes('No workspaces match.'), 'the content query did not settle empty');
        currentLibraryReport = { ...currentLibraryReport, body: 'The edited cobalt note.', updated_at: new Date().toISOString() };
        window.dispatchEvent(new CustomEvent('world:reports-stale', { detail: { report_id: libraryCanvas } }));
        await until(() => librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`), 'a live workspace edit did not refresh the active query');

        currentLibraryReport = { ...currentLibraryReport, title: 'Library workspace fixture', updated_at: new Date().toISOString() };
        setValue(search, 'library workspace');
        await until(() => librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`), 'the out-of-order fixture did not reach its starting result');
        holdWorkspaceSearch = true;
        workspaceSearchHeld = false;
        let releaseWorkspaceSearch = () => {};
        workspaceSearchGate = new Promise((resolve) => { releaseWorkspaceSearch = resolve; });
        window.dispatchEvent(new CustomEvent('world:reports-stale', { detail: { report_id: libraryCanvas } }));
        await until(() => workspaceSearchHeld, 'the stale workspace query did not pause');
        currentLibraryReport = { ...currentLibraryReport, title: 'Renamed away workspace', body: '', updated_at: new Date().toISOString() };
        holdWorkspaceSearch = false;
        window.dispatchEvent(new CustomEvent('world:reports-stale', { detail: { report_id: libraryCanvas } }));
        await until(() => !librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`) && librarySurface.textContent.includes('No workspaces match.'), 'the current empty query did not beat the held result');
        releaseWorkspaceSearch();
        await wait(150);
        if (librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`) || !librarySurface.textContent.includes('No workspaces match.')) throw new Error('the held workspace query replaced the current empty result');

        currentLibraryReport = { ...currentLibraryReport, title: 'Library workspace fixture', updated_at: new Date().toISOString() };
        workspaceSearchFailure = true;
        window.dispatchEvent(new CustomEvent('world:reports-stale', { detail: { report_id: libraryCanvas } }));
        const retry = await until(() => [...librarySurface.querySelectorAll('button')].find((button) => button.textContent.trim() === 'Try again' && button.closest('[role="alert"]')?.textContent.includes('Could not search')), 'the active query failure did not show its retry');
        if (librarySurface.textContent.includes('No workspaces match.') || librarySurface.textContent.includes('Searching the library')) throw new Error('the failed workspace search showed another feedback state');
        retry.click();
        row = await until(() => librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`), 'retrying the active query did not recover');
        if (librarySurface.querySelector('[role="alert"]')) throw new Error('the recovered workspace search kept its failure state');
        row.click();
        await until(() => librarySurface.querySelector(`.react-flow__node[data-id="${libraryItem}"]`), 'the delete fixture reader did not open');
        const deleted = await window.fetch(`/api/world/reports/${libraryCanvas}`, { method: 'DELETE' });
        if (!deleted.ok) throw new Error('the external workspace delete failed');
        window.dispatchEvent(new CustomEvent('world:reports-stale', { detail: { report_id: libraryCanvas } }));
        await until(() => workspaceLibraryDeleted
          && !librarySurface.querySelector(`.react-flow__node[data-id="${libraryItem}"]`)
          && !librarySurface.querySelector(`button[data-report="${libraryCanvas}"]`)
          && librarySurface.textContent.includes('This workspace was deleted or is not shared with you.')
          && librarySurface.textContent.includes('No workspaces match.'), 'an external workspace deletion did not refresh its open board and active query');
      }));
    }
    if (batch === 'workspace-completion') {
      const workspaceActions = (root) => [...root.querySelectorAll('button')].filter((button) => button.textContent.trim() === 'Open workspace');
      checks.push(await run('an event-first completion keeps one workspace action when its quest card arrives', async () => {
        workspaceCompletionPhase = 'event-first';
        await choose('Redesign checkout drawer', `hit:quest:${selectedQuest}`);
        const room = await until(() => modal('Quest room'), 'Quest room did not open for the completion workspace link');
        await until(() => room.textContent.includes('Completion workspace fixture') && workspaceActions(room).length === 1, 'the event-only completion fallback did not render one workspace action');
        if (localStorage.getItem(workspaceLayoutKey) !== null) throw new Error('the workspace already had a saved layout before the event fallback opened');
        workspaceActions(room)[0].click();
        const fallbackWorkspace = await until(() => modal('Workspace'), 'the event-only completion fallback did not open the workspace');
        await until(() => visible(`.react-flow__node[data-id="${workspaceItem}"].selected`)[0], 'the event-only completion fallback did not focus its workspace item');
        fallbackWorkspace.querySelector('button[aria-label="Close"]')?.click();
        await until(() => modal('Quest room') === room, 'the quest room did not return after the event fallback closed');
        workspaceCompletionPhase = 'event-with-card';
        window.dispatchEvent(new CustomEvent('world-event', { detail: { seq: workspaceCompletionEvent.seq + 1, type: 'message.created', agent_id: workspaceCompletionEvent.agent_id, task_id: selectedQuest, run_id: null, session_id: workspaceCompletionEvent.session_id, payload: { message_id: workspaceCompletionMessage.id }, created_at: workspaceCompletionMessage.created_at } }));
        await until(() => room.textContent.includes('Quest complete') && workspaceActions(room).length === 1, 'the delivered quest card did not replace the duplicate event workspace action');
        if (!room.textContent.includes('The focused note remains available in the completion details.')) throw new Error('the event report lost its unique finding');
        if (!room.textContent.includes('The completion evidence is clear.')) throw new Error('the completion event lost its review detail');
        room.querySelector('button[aria-label="Close"]')?.click();
        await until(() => !modal('Quest room'), 'the event-first quest room did not close');
      }));
      checks.push(await run('a card-first completion keeps one workspace action when its event arrives', async () => {
        workspaceCompletionPhase = 'card-first';
        await choose('Redesign checkout drawer', `hit:quest:${selectedQuest}`);
        const room = await until(() => modal('Quest room'), 'Quest room did not reopen for the card-first completion');
        await until(() => room.textContent.includes('Quest complete') && workspaceActions(room).length === 1, 'the card-first completion did not render one workspace action');
        workspaceCompletionPhase = 'card-with-event';
        window.dispatchEvent(new CustomEvent('world-event', { detail: workspaceCompletionEvent }));
        await until(() => room.textContent.includes('The focused note remains available in the completion details.') && workspaceActions(room).length === 1, 'the later completion event duplicated the workspace action');
        const open = workspaceActions(room)[0];
        open.click();
        const workspace = await until(() => modal('Workspace'), 'the completion link did not open in workspace mode');
        await until(() => workspace.querySelector('button[aria-label="Hide channels"]'), 'the workspace channel control did not appear');
        await until(() => visible('button[title="Back to all workspaces"]')[0], 'the workspace header controls did not appear');
        await until(() => visible(`.react-flow__node[data-id="${workspaceItem}"].selected`)[0], 'the completion link did not focus its workspace item');
        workspace.querySelector('button[aria-label="Close"]')?.click();
        await until(() => !modal('Workspace'), 'the completion workspace did not close before the notification check');
      }));
      checks.push(await run('the web bell reaches the delivered quest card and its exact workspace item', async () => {
        workspaceNotificationsCleared = false;
        window.dispatchEvent(new Event('world:notes-stale'));
        await choose('Open notifications', 'action:notifications');
        const notifications = await until(() => modal('Notifications'), 'Notifications did not open for the completion');
        const completion = await until(() => [...notifications.querySelectorAll('button')].find((button) => button.textContent.includes('finished a quest')), 'the completion notification did not appear');
        completion.click();
        const agent = await until(() => visible('[role="dialog"]').find((element) => element.getAttribute('aria-label')?.startsWith('Talk to ')), 'the completion notification did not open the agent');
        const open = await until(() => workspaceActions(agent)[0], 'the delivered quest card workspace action did not appear');
        open.click();
        const workspace = await until(() => modal('Workspace'), 'the bell quest card did not open the workspace');
        await until(() => visible(`.react-flow__node[data-id="${workspaceItem}"].selected`)[0], 'the bell quest card did not focus its workspace item');
      }));
    }
    if (!['core', 'review', 'report', 'mobile', 'selections', 'mobile-chat', 'board', 'foreground', 'async-starts', 'mutation-races', 'detail-races', 'quest-catchup', 'agent-panel-races', 'visibility', 'feature-links', 'workspace-link', 'workspace-completion'].includes(batch)) throw new Error(`Unknown navigation batch: ${batch}`);
    const failed = checks.filter(([, result]) => result !== 'pass');
    if (failed.length) throw new Error(JSON.stringify(failed));
    return { batch, checks, viewport: [innerWidth, innerHeight] };
  } finally {
    window.__lokaNavigationViewportRequest = null;
    window.__lokaNavigationKeyboardRequest = null;
    window.fetch = originalFetch;
    window.confirm = originalConfirm;
    if (batch === 'feature-links') {
      if (savedWorkshopLayout === null) localStorage.removeItem(workshopLayoutKey);
      else localStorage.setItem(workshopLayoutKey, savedWorkshopLayout);
    }
    if (workspaceBatch) {
      if (savedWorkspaceLayout === null) localStorage.removeItem(workspaceLayoutKey);
      else localStorage.setItem(workspaceLayoutKey, savedWorkspaceLayout);
    }
    if (batch === 'workspace-link') {
      if (savedWorkspaceLibrary === null) localStorage.removeItem(workspaceLibraryKey);
      else localStorage.setItem(workspaceLibraryKey, savedWorkspaceLibrary);
      if (savedSideChat === null) localStorage.removeItem(sideChatKey);
      else localStorage.setItem(sideChatKey, savedSideChat);
    }
    if (originalTauri) window.__TAURI__ = originalTauri;
    else delete window.__TAURI__;
  }
})()
