use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::world::{emit_in, AgentMessage, NewEvent, SessionBrief, SessionRunView, WorldEvent, WorldStore};

pub const MEMBER_KINDS: [&str; 3] = ["agent", "person", "session"];
pub const ROLES: [&str; 4] = ["owner", "lead", "helper", "guest"];
pub const NOT_TRUSTED: &str = "Only you and people you trust can join a quest room. Trust them first in People.";
pub const NOT_YOURS: &str = "Only agents of the quest's owner can join its room";
pub const DEFAULT_MEMBER: &str = "The quest's owner, its agent and its sessions are always in the room";

pub fn task_access_sql(task: &str, lead: &str, viewer: &str, admin: &str, write: &str) -> String {
    format!("({admin} OR {task}.user_id = {viewer} OR {lead}.owner_user_id = {viewer} OR ({write} AND {lead}.visibility = 'public'))")
}

pub fn room_access_sql(task: &str, lead: &str, viewer: &str, admin: &str, write: &str) -> String {
    let task_access = task_access_sql(task, lead, viewer, admin, write);
    format!(
        "({task_access}
          OR EXISTS (SELECT 1 FROM quest_room_members rm
                     WHERE rm.quest_id = {task}.id AND rm.member_kind = 'person' AND rm.member_id = {viewer}))"
    )
}

pub fn agent_room_access_sql(room: &str, source: &str, agent: &str) -> String {
    format!(
        "({room}.assigned_agent_id = {agent} OR {source}.assigned_agent_id = {agent}
          OR EXISTS (SELECT 1 FROM quest_room_members rm
                     WHERE rm.quest_id = {room}.id AND rm.member_kind = 'agent' AND rm.member_id = {agent}))"
    )
}

pub fn origin_part_sql(message: &str, question: &str) -> String {
    format!("COALESCE({message}.meta#>>'{{question,part_id}}', {question}.meta#>>'{{question,part_id}}')")
}

pub fn source_part_sql(source: &str, parent: &str, parent_lead: &str) -> String {
    let canonical = canonical_room_sql(source, parent, parent_lead);
    format!("CASE WHEN {source}.id IS DISTINCT FROM {canonical} THEN {source}.id::text END")
}

pub fn room_origin_part_sql(message: &str, question: &str, source: &str, parent: &str, parent_lead: &str) -> String {
    let stated = origin_part_sql(message, question);
    let source_part = source_part_sql(source, parent, parent_lead);
    format!("COALESCE({stated}, {source_part})")
}

pub fn room_origin_part_with_question_sql(
    message: &str,
    question: &str,
    source: (&str, &str, &str),
    question_source: (&str, &str, &str),
) -> String {
    let stated = origin_part_sql(message, question);
    let source_part = source_part_sql(source.0, source.1, source.2);
    let question_part = source_part_sql(question_source.0, question_source.1, question_source.2);
    format!("COALESCE({stated}, {source_part}, {question_part})")
}

pub fn canonical_room_sql(source: &str, parent: &str, parent_lead: &str) -> String {
    format!(
        "CASE WHEN {parent}.id IS NOT NULL AND {parent}.team_id IS NULL
                    AND COALESCE({parent}.asking, {parent_lead}.asking, 'off') = 'crew'
               THEN {parent}.id ELSE {source}.id END"
    )
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, PartialEq)]
pub struct RoomMember {
    pub quest_id: Uuid,
    pub member_kind: String,
    pub member_id: Uuid,
    pub role: String,
    pub name: Option<String>,
    pub added_by: Option<Uuid>,
    pub added_by_kind: String,
    pub added_at: DateTime<Utc>,
    pub default: bool,
    pub cli: Option<String>,
    pub model: Option<String>,
    #[sqlx(skip)]
    pub id: Uuid,
    #[sqlx(skip)]
    pub kind: String,
    #[sqlx(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[sqlx(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    #[sqlx(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

impl RoomMember {
    fn with_aliases(mut self) -> Self {
        self.id = self.member_id;
        self.kind = self.member_kind.clone();
        let id = Some(self.member_id);
        match self.member_kind.as_str() {
            "agent" => self.agent_id = id,
            "person" => self.user_id = id,
            _ => self.session_id = id,
        }
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RoomReach {
    pub room: Option<Uuid>,
    pub lead: Option<Uuid>,
    pub lead_public: bool,
    pub owner: Option<Uuid>,
    pub creator: Option<Uuid>,
    pub people: Vec<Uuid>,
}

pub fn room_open_to(event: &crate::world::WorldEvent, viewer: Uuid, role: crate::access::Role, owned: &std::collections::HashSet<Uuid>) -> Option<bool> {
    let room = event.payload.get("room").filter(|room| room.is_object())?;
    let ids = |key: &str| room.get(key).and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(|v| v.as_str()?.parse::<Uuid>().ok()).collect::<Vec<_>>();
    let lead = room.get("lead").and_then(serde_json::Value::as_str).and_then(|id| id.parse::<Uuid>().ok());
    let lead_public = room.get("lead_public").and_then(serde_json::Value::as_bool) == Some(true);
    Some(
        ids("people").contains(&viewer)
            || role == crate::access::Role::Admin
            || lead.is_some_and(|lead| owned.contains(&lead))
            || (lead_public && role >= crate::access::Role::Write),
    )
}

pub fn scoped_event(event: &crate::world::WorldEvent) -> bool {
    event.payload.get("room").is_some_and(serde_json::Value::is_object)
}

pub fn scoped_room(event: &crate::world::WorldEvent) -> Option<Uuid> {
    event
        .payload
        .pointer("/room/task_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| id.parse().ok())
}

pub fn scoped_part(event: &crate::world::WorldEvent) -> Option<Uuid> {
    event.payload.get("part_task_id").and_then(serde_json::Value::as_str).and_then(|id| id.parse().ok())
}

pub fn event_part(event: &crate::world::WorldEvent, room: Uuid) -> Option<Uuid> {
    scoped_part(event).or_else(|| event.task_id.filter(|task| *task != room))
}

impl RoomReach {
    pub(crate) fn scope(&self, event: NewEvent) -> NewEvent {
        let mut event = match (event.agent_id, self.lead) {
            (None, Some(lead)) => event.agent(lead),
            _ => event,
        };
        if let Some(payload) = event.payload.as_object_mut() {
            if let Some(owner) = self.owner {
                payload.insert("owner_user_id".into(), json!(owner));
            }
            let mut audience: Vec<Uuid> = Vec::new();
            for person in self.owner.iter().chain(self.creator.iter()).chain(self.people.iter()) {
                if !audience.contains(person) {
                    audience.push(*person);
                }
            }
            payload.insert("audience".into(), json!(audience));
            payload.insert("room".into(), json!({ "task_id": self.room, "people": audience, "lead": self.lead, "lead_public": self.lead_public }));
        }
        event
    }
}

pub(crate) async fn room_reach(conn: &mut sqlx::PgConnection, room: Uuid) -> Result<RoomReach> {
    let row = sqlx::query_as::<_, (Option<Uuid>, bool, Option<Uuid>, Option<Uuid>, Vec<Uuid>)>(
        "SELECT t.assigned_agent_id, COALESCE(a.visibility = 'public', FALSE), COALESCE(a.owner_user_id, t.user_id), t.user_id,
                ARRAY(SELECT m.member_id FROM quest_room_members m WHERE m.quest_id = t.id AND m.member_kind = 'person')
         FROM agent_tasks t LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id WHERE t.id = $1",
    )
    .bind(room)
    .fetch_optional(conn)
    .await?;
    Ok(row
        .map(|(lead, lead_public, owner, creator, people)| RoomReach { room: Some(room), lead, lead_public, owner, creator, people })
        .unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_member_in(
    tx: &mut Transaction<'_, Postgres>,
    room: Uuid,
    kind: &str,
    id: Uuid,
    role: &str,
    added_by_kind: &str,
    added_by: Option<Uuid>,
    name: &str,
) -> Result<bool> {
    let added = sqlx::query(
        "INSERT INTO quest_room_members (quest_id, member_kind, member_id, role, added_by, added_by_kind)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
    )
    .bind(room)
    .bind(kind)
    .bind(id)
    .bind(role)
    .bind(added_by)
    .bind(added_by_kind)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    if added {
        let reach = room_reach(tx, room).await?;
        emit_in(
            tx,
            &reach.scope(NewEvent::new("room.member_added").task(Some(room)).payload(json!({
                "task_id": room, "member_kind": kind, "member_id": id, "role": role, "name": name, "quiet": true,
            }))),
            None,
        )
        .await?;
    }
    Ok(added)
}

pub const ROOM_EVENTS: i64 = 300;
pub const ROOM_MESSAGES: i64 = 200;
pub const ROOM_RUNS_PER_SESSION: i64 = 60;

#[derive(Debug, Clone, Serialize)]
pub struct QuestRoom {
    pub task_id: Uuid,
    pub sessions: Vec<SessionBrief>,
    pub runs: Vec<SessionRunView>,
    pub events: Vec<WorldEvent>,
    pub messages: Vec<AgentMessage>,
    pub needs_you_messages: Vec<AgentMessage>,
    pub cursor: i64,
    pub members: Vec<RoomMember>,
    pub asking: Option<RoomAskingView>,
    pub decisions: crate::quest_questions::DecisionSummary,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RoomAskingView {
    pub mode: String,
    pub crew_size: Option<i16>,
    pub per_run: i64,
    pub per_quest: i64,
    pub asked: i64,
}

pub fn quest_pattern(task_id: Uuid) -> String {
    format!("%{task_id}%")
}

impl WorldStore {
    pub async fn open_rooms(&self, rooms: &[Uuid], viewer: &crate::access::Viewer) -> Result<std::collections::HashSet<Uuid>> {
        if rooms.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let access = room_access_sql("t", "lead", "$2", "$3", "$4");
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT t.id FROM agent_tasks t
             LEFT JOIN agent_profiles lead ON lead.id = t.assigned_agent_id
             WHERE t.id = ANY($1) AND t.org_id = $5 AND {access}"
        ))
        .bind(rooms)
        .bind(viewer.user)
        .bind(viewer.role == crate::access::Role::Admin)
        .bind(viewer.role >= crate::access::Role::Write)
        .bind(viewer.org)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .collect())
    }

    pub async fn open_tasks(&self, tasks: &[Uuid], viewer: &crate::access::Viewer) -> Result<std::collections::HashSet<Uuid>> {
        if tasks.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let access = task_access_sql("t", "lead", "$2", "$3", "$4");
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT t.id FROM agent_tasks t
             LEFT JOIN agent_profiles lead ON lead.id = t.assigned_agent_id
             WHERE t.id = ANY($1) AND t.org_id = $5 AND {access}"
        ))
        .bind(tasks)
        .bind(viewer.user)
        .bind(viewer.role == crate::access::Role::Admin)
        .bind(viewer.role >= crate::access::Role::Write)
        .bind(viewer.org)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .collect())
    }

    pub async fn open_room_messages_for(
        &self,
        messages: &[AgentMessage],
        viewer: &crate::access::Viewer,
        allowed_parts: Option<&[Uuid]>,
    ) -> Result<std::collections::HashSet<Uuid>> {
        if messages.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let ids: Vec<Uuid> = messages.iter().map(|message| message.id).collect();
        let origin = room_origin_part_with_question_sql(
            "m",
            "q",
            ("source", "parent", "parent_lead"),
            ("question_source", "question_parent", "question_parent_lead"),
        );
        let access = task_access_sql("part", "part_lead", "$2", "$3", "$4");
        Ok(sqlx::query_scalar::<_, Uuid>(&format!(
            "SELECT m.id FROM agent_messages m
             LEFT JOIN agent_messages q ON q.id::text = COALESCE(m.meta->>'answers_question', m.meta->>'answers')
             LEFT JOIN agent_tasks source ON source.id::text = m.meta->>'task_id'
             LEFT JOIN agent_tasks parent ON parent.id = source.parent_task_id
             LEFT JOIN agent_profiles parent_lead ON parent_lead.id = parent.assigned_agent_id
             LEFT JOIN agent_tasks question_source ON question_source.id::text = q.meta->>'task_id'
             LEFT JOIN agent_tasks question_parent ON question_parent.id = question_source.parent_task_id
             LEFT JOIN agent_profiles question_parent_lead ON question_parent_lead.id = question_parent.assigned_agent_id
             LEFT JOIN agent_tasks part ON part.id::text = {origin}
             LEFT JOIN agent_profiles part_lead ON part_lead.id = part.assigned_agent_id
             WHERE m.id = ANY($1) AND ({origin} IS NULL OR part.org_id = $5 AND {access})
               AND ($6::uuid[] IS NULL OR {origin} IS NULL OR part.id = ANY($6))"
        ))
        .bind(ids)
        .bind(viewer.user)
        .bind(viewer.role == crate::access::Role::Admin)
        .bind(viewer.role >= crate::access::Role::Write)
        .bind(viewer.org)
        .bind(allowed_parts)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .collect())
    }

    pub async fn open_room_messages(&self, messages: &[AgentMessage], viewer: &crate::access::Viewer) -> Result<std::collections::HashSet<Uuid>> {
        self.open_room_messages_for(messages, viewer, None).await
    }

    pub async fn contextual_quest_clarification(
        &self,
        task_id: Uuid,
        agent_id: Uuid,
        owner_user_id: Uuid,
    ) -> Result<Option<AgentMessage>> {
        Ok(sqlx::query_as::<_, AgentMessage>(
            "SELECT m.*
             FROM agent_messages m
             JOIN agent_profiles a ON a.id = m.agent_id AND a.owner_user_id = $3
             JOIN agent_tasks t ON t.id = $1 AND t.status = 'done' AND t.assigned_agent_id = m.agent_id
             WHERE m.agent_id = $2 AND m.author_kind = 'agent' AND m.meta->>'kind' = 'ask'
               AND m.meta->'clarify_request'->>'context_task_id' = $1::text
               AND NOT EXISTS (
                 SELECT 1 FROM agent_messages answer
                 WHERE answer.agent_id = m.agent_id AND answer.meta->>'answers' = m.id::text
               )
             ORDER BY m.created_at DESC, m.id DESC LIMIT 1",
        )
        .bind(task_id)
        .bind(agent_id)
        .bind(owner_user_id)
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn quest_sessions(&self, task_id: Uuid) -> Result<Vec<SessionBrief>> {
        self.quest_sessions_for(&[task_id]).await
    }

    async fn quest_sessions_for(&self, task_ids: &[Uuid]) -> Result<Vec<SessionBrief>> {
        Ok(sqlx::query_as::<_, SessionBrief>(
            "SELECT s.id, s.agent_id, s.role, s.backend, s.model, s.status, s.task_id, t.title AS task_title,
                    COALESCE(s.title, t.title) AS title, s.summary,
                    COALESCE((SELECT SUM(cost_usd) FROM session_runs r WHERE r.session_id = s.id), 0)::float8 AS cost_usd,
                    COALESCE((SELECT SUM(input_tokens + cached_input_tokens + output_tokens) FROM session_runs r WHERE r.session_id = s.id), 0)::int8 AS tokens,
                    (SELECT COUNT(*) FROM session_runs r WHERE r.session_id = s.id) AS runs,
                    EXISTS (SELECT 1 FROM session_runs r WHERE r.session_id = s.id AND r.status IN ('queued', 'running')) AS running,
                    s.created_at, s.updated_at, s.working_dir, t.target_branch AS base_branch,
                    s.plan_state, s.attempt, s.attempt_state
             FROM agent_sessions s JOIN agent_tasks t ON t.id = s.task_id
             WHERE s.task_id = ANY($1)
             ORDER BY s.created_at",
        )
        .bind(task_ids)
        .fetch_all(self.pool())
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn quest_room(
        &self,
        task_id: Uuid,
        since: DateTime<Utc>,
        agents: &[Uuid],
        viewer: Uuid,
        after_seq: Option<i64>,
        after_message: Option<DateTime<Utc>>,
        focus: Option<Uuid>,
    ) -> Result<QuestRoom> {
        let room = self.room_asking(task_id).await?;
        let task_id = room.as_ref().map(|room| room.room).unwrap_or(task_id);
        let mut task_ids = vec![task_id];
        if room.is_some_and(|room| room.mode() == crate::quest_questions::Asking::Crew) {
            task_ids.extend(sqlx::query_scalar::<_, Uuid>("SELECT id FROM agent_tasks WHERE parent_task_id = $1 ORDER BY created_at, id")
                .bind(task_id)
                .fetch_all(self.pool())
                .await?);
        }
        let sessions: Vec<SessionBrief> = self
            .quest_sessions_for(&task_ids)
            .await?
            .into_iter()
            .filter(|s| agents.contains(&s.agent_id))
            .collect();
        let session_ids: Vec<Uuid> = sessions.iter().map(|s| s.id).collect();
        let mut runs = Vec::new();
        for session in &sessions {
            runs.extend(self.list_runs_page(session.id, None, ROOM_RUNS_PER_SESSION, true).await?);
        }
        runs.sort_by_key(|r| r.run.created_at);
        let cursor = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(seq) FROM world_events")
            .fetch_one(self.pool())
            .await?
            .unwrap_or(0);
        let mut events = sqlx::query_as::<_, WorldEvent>(
            "SELECT * FROM (
               SELECT * FROM world_events WHERE task_id = ANY($1) AND seq > $3 AND seq <= $5
               UNION
               SELECT * FROM world_events WHERE session_id = ANY($2) AND seq > $3 AND seq <= $5
             ) e
             WHERE e.type NOT IN ('agent.progress', 'agent.status', 'message.created', 'run.activity', 'task.changed')
             ORDER BY seq DESC LIMIT $4",
        )
        .bind(&task_ids)
        .bind(&session_ids)
        .bind(after_seq.unwrap_or(0))
        .bind(ROOM_EVENTS)
        .bind(cursor)
        .fetch_all(self.pool())
        .await?;
        events.reverse();
        let canonical = canonical_room_sql("source", "parent", "parent_lead");
        let task_keys: Vec<String> = task_ids.iter().map(Uuid::to_string).collect();
        let mut messages = sqlx::query_as::<_, AgentMessage>(&format!(
            "WITH eligible AS (
             SELECT m.*, s.role AS session_role FROM agent_messages m
             LEFT JOIN agent_sessions s ON s.id = m.session_id
             LEFT JOIN agent_tasks source ON source.id::text = m.meta->>'task_id'
             LEFT JOIN agent_tasks parent ON parent.id = source.parent_task_id
             LEFT JOIN agent_profiles parent_lead ON parent_lead.id = parent.assigned_agent_id
             WHERE m.meta ? 'room_only' AND {canonical} = $1
                OR m.agent_id = ANY($2)
                  AND (m.meta->>'task_id' = $1::text
                    OR COALESCE(m.meta->'clarification'->>'task_id', m.meta->'clarify_request'->>'task_id') = ANY($10)
                    OR m.meta->'clarify_request'->>'context_task_id' = ANY($10)
                    OR m.meta->>'context_task_id' = ANY($10)
                    OR m.run_id IN (
                      SELECT r.id FROM session_runs r
                      WHERE r.agent_id = ANY($2) AND r.created_at >= $3
                        AND (r.task_id = ANY($9)
                          OR (r.kind = 'turn' AND r.prompt LIKE $5))
                    ))
                  AND (m.meta->>'kind' = 'quest' AND {}
                    OR NOT (m.meta ? 'room_only') AND {})
             ),
             focused AS (
               SELECT id::text AS message, CASE
                 WHEN kind = 'question' THEN id::text
                 ELSE COALESCE(meta->>'answers_question', meta->>'answers')
               END AS question
               FROM eligible WHERE id = $8
             )
             SELECT * FROM (
               (SELECT * FROM eligible
                WHERE created_at >= $3 AND ($4::timestamptz IS NULL OR created_at > $4 OR edited_at > $4)
                ORDER BY created_at DESC LIMIT $6)
               UNION
               (SELECT e.* FROM eligible e, focused f
                WHERE $8::uuid IS NOT NULL AND (e.id::text = f.message OR f.question IS NOT NULL
                  AND (e.id::text = f.question OR e.meta->>'answers_question' = f.question OR e.meta->>'answers' = f.question)))
             ) picked ORDER BY created_at DESC",
            crate::guests::thread_filter("m.agent_id", "$7"),
            crate::guests::thread_filter("m.agent_id", "$7")
        ))
        .bind(task_id)
        .bind(agents)
        .bind(since)
        .bind(after_message)
        .bind(quest_pattern(task_id))
        .bind(ROOM_MESSAGES)
        .bind(viewer)
        .bind(focus)
        .bind(&task_ids)
        .bind(&task_keys)
        .fetch_all(self.pool())
        .await?;
        messages.reverse();
        let question_room = canonical_room_sql("question_source", "question_parent", "question_parent_lead");
        let clarification_room = canonical_room_sql("clarification_source", "clarification_parent", "clarification_parent_lead");
        let mut needs_you_messages = sqlx::query_as::<_, AgentMessage>(&format!(
            "WITH passed_questions AS (
               SELECT q.id
               FROM agent_messages q
               LEFT JOIN agent_tasks question_source ON question_source.id::text = q.meta->>'task_id'
               LEFT JOIN agent_tasks question_parent ON question_parent.id = question_source.parent_task_id
               LEFT JOIN agent_profiles question_parent_lead ON question_parent_lead.id = question_parent.assigned_agent_id
               WHERE q.meta->>'task_id' = ANY($5) AND q.meta ? 'room_only' AND q.kind = 'question'
                 AND q.decision_status = 'passed_up' AND {question_room} = $1
             ),
             pending_cards AS (
               SELECT DISTINCT ON (q.id) m.id
               FROM passed_questions passed
               JOIN agent_messages q ON q.id = passed.id
               JOIN agent_messages m ON m.agent_id = q.agent_id
                 AND m.kind = 'needs_you' AND m.meta->>'answers_question' = q.id::text
               JOIN agent_profiles card_agent ON card_agent.id = m.agent_id AND card_agent.owner_user_id = $2
               WHERE m.agent_id = ANY($3) AND {}
               ORDER BY q.id, m.created_at DESC, m.id DESC
             ),
             legacy AS (
               SELECT card.id
               FROM agent_sessions active
               JOIN agent_tasks clarification_source ON clarification_source.id = active.task_id
                 AND clarification_source.assigned_agent_id = active.agent_id
                 AND clarification_source.status IN ('in_progress', 'failed')
               LEFT JOIN agent_tasks clarification_parent ON clarification_parent.id = clarification_source.parent_task_id
               LEFT JOIN agent_profiles clarification_parent_lead ON clarification_parent_lead.id = clarification_parent.assigned_agent_id
               JOIN LATERAL (
                 SELECT m.id
                 FROM agent_messages m
                 JOIN agent_profiles card_agent ON card_agent.id = m.agent_id AND card_agent.owner_user_id = $2
                 WHERE m.agent_id = active.agent_id AND m.meta ? 'clarification'
                   AND m.meta#>>'{{clarification,session_id}}' = active.id::text
                   AND m.meta#>>'{{clarification,task_id}}' = clarification_source.id::text
                   AND m.agent_id = ANY($3) AND {}
                 ORDER BY m.created_at DESC, m.id DESC LIMIT 1
               ) card ON TRUE
               WHERE active.task_id = ANY($4) AND active.status <> 'archived' AND active.plan_state = 'clarifying'
                 AND {clarification_room} = $1
             ),
             wanted AS (SELECT id FROM passed_questions UNION SELECT id FROM pending_cards UNION SELECT id FROM legacy)
             SELECT card.*, session.role AS session_role
             FROM wanted
             JOIN agent_messages card ON card.id = wanted.id
             LEFT JOIN agent_sessions session ON session.id = card.session_id
             ORDER BY card.created_at, card.id",
            crate::guests::thread_filter("m.agent_id", "$2"),
            crate::guests::thread_filter("m.agent_id", "$2")
        ))
        .bind(task_id)
        .bind(viewer)
        .bind(agents)
        .bind(&task_ids)
        .bind(&task_keys)
        .fetch_all(self.pool())
        .await?;
        needs_you_messages.dedup_by_key(|message| message.id);
        let members = self.room_members(task_id).await?;
        let asking = match self.room_asking(task_id).await? {
            Some(room) => Some(RoomAskingView {
                mode: room.mode().as_str().to_string(),
                crew_size: room.crew_size,
                per_run: crate::quest_questions::PER_RUN,
                per_quest: crate::quest_questions::PER_QUEST,
                asked: self.questions_asked(room.room, None).await?.1,
            }),
            None => None,
        };
        let decisions = self.decision_summary(task_id).await?;
        Ok(QuestRoom { task_id, sessions, runs, events, messages, needs_you_messages, cursor, members, asking, decisions })
    }

    pub async fn room_members(&self, task_id: Uuid) -> Result<Vec<RoomMember>> {
        let task_id = self.room_of(task_id).await?;
        Ok(sqlx::query_as::<_, RoomMember>(
            "WITH quest AS (
               SELECT t.id, t.created_at, t.assigned_agent_id, COALESCE(a.owner_user_id, t.user_id) AS owner
               FROM agent_tasks t LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id WHERE t.id = $1
             ),
             defaults AS (
               SELECT q.id AS quest_id, 'person' AS member_kind, q.owner AS member_id, 'owner' AS role,
                      COALESCE(u.name, split_part(u.email, '@', 1)) AS name, NULL::uuid AS added_by, 'loka' AS added_by_kind,
                      q.created_at AS added_at, NULL::text AS cli, NULL::text AS model, 0 AS ord
               FROM quest q JOIN users u ON u.id = q.owner
               UNION ALL
               SELECT q.id, 'agent', a.id, 'lead', a.name, NULL, 'loka', q.created_at, a.cli, a.model, 1
               FROM quest q JOIN agent_profiles a ON a.id = q.assigned_agent_id
               UNION ALL
               SELECT q.id, 'session', s.id, 'helper', COALESCE(s.title, a.name), NULL, 'loka', s.created_at, s.backend, s.model, 2
               FROM quest q
               JOIN agent_sessions s ON s.role = 'task'
                 AND (s.task_id = q.id OR s.task_id IN (SELECT k.id FROM agent_tasks k WHERE k.parent_task_id = q.id))
               JOIN agent_profiles a ON a.id = s.agent_id
             )
             SELECT quest_id, member_kind, member_id, role, name, added_by, added_by_kind, added_at, TRUE AS \"default\", cli, model
             FROM (SELECT DISTINCT ON (member_kind, member_id) * FROM defaults ORDER BY member_kind, member_id, ord) d
             UNION ALL
             SELECT m.quest_id, m.member_kind, m.member_id, m.role,
                    CASE m.member_kind WHEN 'agent' THEN a.name WHEN 'person' THEN COALESCE(u.name, split_part(u.email, '@', 1)) ELSE s.title END,
                    m.added_by, m.added_by_kind, m.added_at, FALSE, COALESCE(a.cli, s.backend), COALESCE(a.model, s.model)
             FROM quest_room_members m
             LEFT JOIN agent_profiles a ON m.member_kind = 'agent' AND a.id = m.member_id
             LEFT JOIN users u ON m.member_kind = 'person' AND u.id = m.member_id
             LEFT JOIN agent_sessions s ON m.member_kind = 'session' AND s.id = m.member_id
             WHERE m.quest_id = $1
               AND NOT EXISTS (SELECT 1 FROM defaults d WHERE d.member_kind = m.member_kind AND d.member_id = m.member_id)
             ORDER BY 9 DESC, added_at",
        )
        .bind(task_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(RoomMember::with_aliases)
        .collect())
    }

    pub async fn room_members_for(&self, task_id: Uuid, agents: &[Uuid]) -> Result<Vec<RoomMember>> {
        let mut members = self.room_members(task_id).await?;
        let sessions: Vec<Uuid> = members.iter().filter(|member| member.member_kind == "session").map(|member| member.member_id).collect();
        let visible_sessions: std::collections::HashSet<Uuid> = sqlx::query_scalar(
            "SELECT id FROM agent_sessions WHERE id = ANY($1) AND agent_id = ANY($2)",
        )
        .bind(&sessions)
        .bind(agents)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .collect();
        members.retain(|member| match member.member_kind.as_str() {
            "session" => visible_sessions.contains(&member.member_id),
            "agent" if member.role != "lead" => agents.contains(&member.member_id),
            _ => true,
        });
        Ok(members)
    }

    pub async fn room_owner(&self, task_id: Uuid) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT COALESCE(a.owner_user_id, t.user_id) FROM agent_tasks t
             LEFT JOIN agent_profiles a ON a.id = t.assigned_agent_id WHERE t.id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    pub async fn is_room_person(&self, task_id: Uuid, user: Uuid) -> Result<bool> {
        let task_id = self.room_of(task_id).await?;
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM quest_room_members WHERE quest_id = $1 AND member_kind = 'person' AND member_id = $2)",
        )
        .bind(task_id)
        .bind(user)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn add_room_member(&self, task_id: Uuid, kind: &str, id: Uuid, role: Option<&str>, added_by: (&str, Option<Uuid>)) -> Result<RoomMember> {
        let task_id = self.room_of(task_id).await?;
        let role = role.unwrap_or("guest");
        if !matches!(kind, "agent" | "person") {
            bail!("member_kind must be agent or person");
        }
        if !(role == "guest" || (role == "helper" && kind == "agent")) {
            bail!("added agents join as guest or helper, people as guest");
        }
        let Some(owner) = self.room_owner(task_id).await? else {
            bail!("no such quest");
        };
        let name = match kind {
            "agent" => {
                let agent = sqlx::query_as::<_, (String, Option<Uuid>, Option<DateTime<Utc>>)>("SELECT name, owner_user_id, archived_at FROM agent_profiles WHERE id = $1")
                    .bind(id)
                    .fetch_optional(self.pool())
                    .await?;
                match agent {
                    Some((name, Some(agent_owner), None)) if agent_owner == owner => name,
                    _ => bail!(NOT_YOURS),
                }
            }
            _ => {
                let trusted = id == owner
                    || sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM world_trusted WHERE owner_user_id = $1 AND user_id = $2)")
                        .bind(owner)
                        .bind(id)
                        .fetch_one(self.pool())
                        .await?;
                if !trusted {
                    bail!(NOT_TRUSTED);
                }
                self.user_display_name(id).await?.unwrap_or_else(|| "someone".into())
            }
        };
        if self.room_members(task_id).await?.iter().any(|m| m.member_kind == kind && m.member_id == id) {
            bail!("already a member");
        }
        let mut tx = self.pool().begin().await?;
        add_member_in(&mut tx, task_id, kind, id, role, added_by.0, added_by.1, &name).await?;
        tx.commit().await?;
        self.room_members(task_id)
            .await?
            .into_iter()
            .find(|m| m.member_kind == kind && m.member_id == id)
            .ok_or_else(|| anyhow::anyhow!("member vanished"))
    }

    pub async fn remove_room_member(&self, task_id: Uuid, kind: &str, id: Uuid) -> Result<bool> {
        let task_id = self.room_of(task_id).await?;
        if self.room_members(task_id).await?.iter().any(|m| m.default && m.member_kind == kind && m.member_id == id) {
            bail!(DEFAULT_MEMBER);
        }
        let mut tx = self.pool().begin().await?;
        let removed = sqlx::query("DELETE FROM quest_room_members WHERE quest_id = $1 AND member_kind = $2 AND member_id = $3")
            .bind(task_id)
            .bind(kind)
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        if removed {
            let mut reach = room_reach(&mut tx, task_id).await?;
            if kind == "person" {
                reach.people.push(id);
            }
            emit_in(
                &mut tx,
                &reach.scope(NewEvent::new("room.member_removed").task(Some(task_id)).payload(json!({
                    "task_id": task_id, "member_kind": kind, "member_id": id, "quiet": true,
                }))),
                None,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quest_room_gathers_sessions_events_and_messages() -> anyhow::Result<()> {
        let Ok(url) = std::env::var("ROOMS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let max = std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(2);
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(max).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ($1, $2) RETURNING id")
            .bind(format!("room {tag}"))
            .bind(format!("{tag}@room.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_profiles (slug, name, avatar_emoji, cli, model, owner_user_id, visibility)
             VALUES ($1, $1, '🦊', 'claude', 'sonnet', $2, 'private') RETURNING id",
        )
        .bind(format!("room-{tag}"))
        .bind(owner)
        .fetch_one(&pool)
        .await?;
        let task: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, assigned_agent_id, asking, status) VALUES ($1, 'Fix the login', $2, 'crew', 'in_progress') RETURNING id")
            .bind(owner)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let part: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_tasks (user_id, title, assigned_agent_id, parent_task_id) VALUES ($1, 'Test the login', $2, $3) RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .bind(task)
        .fetch_one(&pool)
        .await?;
        let other: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title) VALUES ($1, 'Something else') RETURNING id")
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let main: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model) VALUES ($1, 'main', 'claude', 'sonnet') RETURNING id")
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let work: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, task_id) VALUES ($1, 'task', 'claude', 'sonnet', $2) RETURNING id")
            .bind(agent)
            .bind(task)
            .fetch_one(&pool)
            .await?;
        let helper: Uuid = sqlx::query_scalar("INSERT INTO agent_sessions (agent_id, role, backend, model, task_id) VALUES ($1, 'task', 'claude', 'sonnet', $2) RETURNING id")
            .bind(agent)
            .bind(part)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'task', 'running', 'Task: Fix the login')")
            .bind(work)
            .bind(agent)
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO session_runs (session_id, agent_id, task_id, kind, status, prompt) VALUES ($1, $2, $3, 'task', 'running', 'Task: Test the login')")
            .bind(helper)
            .bind(agent)
            .bind(part)
            .execute(&pool)
            .await?;
        let turn: Uuid = sqlx::query_scalar("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'completed', $3) RETURNING id")
            .bind(main)
            .bind(agent)
            .bind(format!("[event] message from you:\nhow is it going\n\nquest:{task}"))
            .fetch_one(&pool)
            .await?;
        let unrelated: Uuid = sqlx::query_scalar("INSERT INTO session_runs (session_id, agent_id, kind, status, prompt) VALUES ($1, $2, 'turn', 'completed', 'hello') RETURNING id")
            .bind(main)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta, created_at) VALUES ($1, 'user', $2, 'how is it going', $3, NOW() - INTERVAL '3 seconds')")
            .bind(agent)
            .bind(owner)
            .bind(serde_json::json!({ "task_id": task }))
            .execute(&pool)
            .await?;
        let guest: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ('room guest', $1) RETURNING id")
            .bind(format!("guest-{tag}@room.test"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, guest, None).await?;
        let writer: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ('room writer', $1) RETURNING id")
            .bind(format!("writer-{tag}@room.test"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, writer, None).await?;
        let outsider: Uuid = sqlx::query_scalar("INSERT INTO users (name, email) VALUES ('room outsider', $1) RETURNING id")
            .bind(format!("outsider-{tag}@room.test"))
            .fetch_one(&pool)
            .await?;
        crate::orgs::join_default_for_tests(&pool, outsider, None).await?;
        sqlx::query("INSERT INTO quest_room_members (quest_id, member_kind, member_id, role, added_by_kind) VALUES ($1, 'person', $2, 'guest', 'loka')")
            .bind(task)
            .bind(guest)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta) VALUES ($1, 'user', $2, 'owner private note', $3)")
            .bind(agent)
            .bind(owner)
            .bind(serde_json::json!({ "task_id": task }))
            .execute(&pool)
            .await?;
        let question: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta, kind, decision_status, created_at)
             VALUES ($1, 'user', $2, 'older room question', $3, 'question', 'answered', NOW() - INTERVAL '2 days') RETURNING id",
        )
        .bind(agent)
        .bind(owner)
        .bind(serde_json::json!({ "task_id": task, "room_only": true, "question": { "part_id": part } }))
        .fetch_one(&pool)
        .await?;
        let answer: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'older room answer', $2, 'answer', NOW() - INTERVAL '2 days' + INTERVAL '1 second') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "room_only": true, "answers_question": question }))
        .fetch_one(&pool)
        .await?;
        let shared_question: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_user_id, body, meta, kind, decision_status, created_at)
             VALUES ($1, 'user', $2, 'shared old question', $3, 'question', 'answered', NOW() - INTERVAL '3 days') RETURNING id",
        )
        .bind(agent)
        .bind(owner)
        .bind(serde_json::json!({ "task_id": task, "room_only": true }))
        .fetch_one(&pool)
        .await?;
        let shared_answer: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'shared old answer', $2, 'answer', NOW() - INTERVAL '3 days' + INTERVAL '1 second') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "room_only": true, "answers_question": shared_question }))
        .fetch_one(&pool)
        .await?;
        let private_card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'owner card', $2, 'needs_you', NOW() - INTERVAL '3 days' + INTERVAL '2 seconds') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "answers_question": shared_question }))
        .fetch_one(&pool)
        .await?;
        let source_part_question: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, decision_status, created_at)
             VALUES ($1, 'agent', $1, 'source part question', $2, 'question', 'answered', NOW() - INTERVAL '4 days') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": part, "room_only": true }))
        .fetch_one(&pool)
        .await?;
        let source_part_answer: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'source part answer', $2, 'answer', NOW() - INTERVAL '4 days' + INTERVAL '1 second') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "room_only": true, "answers_question": source_part_question }))
        .fetch_one(&pool)
        .await?;
        let pending_question: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, decision_status, created_at)
             VALUES ($1, 'agent', $1, 'pending room question', $2, 'question', 'passed_up', NOW() - INTERVAL '6 days') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "room_only": true }))
        .fetch_one(&pool)
        .await?;
        let stale_pending_card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'stale pending card', $2, 'needs_you', NOW() - INTERVAL '6 days' + INTERVAL '1 second') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "answers_question": pending_question, "suggestion": "private stale" }))
        .fetch_one(&pool)
        .await?;
        let pending_card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'current pending card', $2, 'needs_you', NOW() - INTERVAL '6 days' + INTERVAL '2 seconds') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "answers_question": pending_question, "suggestion": "private current" }))
        .fetch_one(&pool)
        .await?;
        let part_pending_question: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, decision_status, created_at)
             VALUES ($1, 'agent', $1, 'pending part question', $2, 'question', 'passed_up', NOW() - INTERVAL '6 days') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "room_only": true, "question": { "part_id": part } }))
        .fetch_one(&pool)
        .await?;
        let part_pending_card: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, kind, created_at)
             VALUES ($1, 'agent', $1, 'current part card', $2, 'needs_you', NOW() - INTERVAL '6 days' + INTERVAL '2 seconds') RETURNING id",
        )
        .bind(agent)
        .bind(serde_json::json!({ "task_id": task, "answers_question": part_pending_question, "suggestion": "private part" }))
        .fetch_one(&pool)
        .await?;
        sqlx::query("UPDATE agent_sessions SET plan_state = 'clarifying' WHERE id = $1").bind(work).execute(&pool).await?;
        let stale_clarification: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, session_id, meta, created_at)
             VALUES ($1, 'agent', $1, 'stale clarification', $2, $3, NOW() - INTERVAL '6 days') RETURNING id",
        )
        .bind(agent)
        .bind(main)
        .bind(serde_json::json!({ "kind": "ask", "task_id": task, "clarification": { "task_id": task, "session_id": work } }))
        .fetch_one(&pool)
        .await?;
        let current_clarification: Uuid = sqlx::query_scalar(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, session_id, meta, created_at)
             VALUES ($1, 'agent', $1, 'current clarification', $2, $3, NOW() - INTERVAL '6 days' + INTERVAL '1 second') RETURNING id",
        )
        .bind(agent)
        .bind(main)
        .bind(serde_json::json!({ "kind": "ask", "task_id": task, "clarification": { "task_id": task, "session_id": work } }))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, meta, created_at)
             SELECT $1, 'agent', $1, 'room filler ' || n, jsonb_build_object('task_id', $2::text, 'room_only', true),
                    NOW() - INTERVAL '5 days' + n * INTERVAL '1 second'
             FROM generate_series(1, 205) n",
        )
        .bind(agent)
        .bind(task)
        .execute(&pool)
        .await?;
        for (run, body) in [(turn, "going well"), (unrelated, "hi there")] {
            sqlx::query("INSERT INTO agent_messages (agent_id, author_kind, author_agent_id, body, run_id) VALUES ($1, 'agent', $1, $2, $3)")
                .bind(agent)
                .bind(body)
                .bind(run)
                .execute(&pool)
                .await?;
        }
        sqlx::query(
            "INSERT INTO agent_messages (agent_id, session_id, author_kind, author_agent_id, body, meta, created_at)
             VALUES ($1, $2, 'agent', $1, 'Which workspace?', $3, NOW() + INTERVAL '1 second'),
                    ($1, $4, 'agent', $1, 'Which database?', $5, NOW() + INTERVAL '2 seconds')",
        )
        .bind(agent)
        .bind(main)
        .bind(serde_json::json!({ "kind": "ask", "clarify_request": { "task_id": task } }))
        .bind(work)
        .bind(serde_json::json!({ "kind": "ask", "clarification": { "task_id": task, "session_id": work } }))
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, task_id, payload) VALUES ('task.started', $1, $2, '{}')")
            .bind(agent)
            .bind(task)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, task_id, payload) VALUES ('task.started', $1, $2, '{}')")
            .bind(agent)
            .bind(part)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, session_id, payload) VALUES ('review.finished', $1, $2, '{}')")
            .bind(agent)
            .bind(work)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO world_events (type, agent_id, task_id, payload) VALUES ('task.started', $1, $2, '{}')")
            .bind(agent)
            .bind(other)
            .execute(&pool)
            .await?;
        let since = chrono::Utc::now() - chrono::Duration::hours(1);

        let room = store.quest_room(task, since, &[agent], owner, None, None, None).await?;
        assert_eq!(room.task_id, task);
        assert_eq!(room.sessions.iter().map(|s| s.id).collect::<Vec<_>>(), vec![work, helper]);
        assert_eq!(room.runs.len(), 2);
        assert!(room.runs[0].run.prompt.is_empty());
        let types: Vec<&str> = room.events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"task.started") && types.contains(&"review.finished"));
        assert!(room.events.iter().any(|e| e.task_id == Some(part)));
        assert!(room.events.iter().all(|e| e.task_id != Some(other)));
        let bodies: Vec<&str> = room.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies.len(), 5);
        assert!(bodies.contains(&"how is it going") && bodies.contains(&"going well") && bodies.contains(&"owner private note"));
        assert!(bodies.contains(&"Which workspace?") && bodies.contains(&"Which database?"));
        let snapshot: std::collections::HashSet<Uuid> = room.needs_you_messages.iter().map(|message| message.id).collect();
        assert_eq!(snapshot, [pending_question, pending_card, part_pending_question, part_pending_card, current_clarification].into_iter().collect());
        assert!(!snapshot.contains(&stale_pending_card) && !snapshot.contains(&stale_clarification));
        let outsider_room = store.quest_room(task, since, &[agent], outsider, None, None, None).await?;
        assert!(outsider_room.messages.iter().all(|message| !["Which workspace?", "Which database?"].contains(&message.body.as_str())));

        let paged = store.quest_room(task, Utc::now() - chrono::Duration::days(10), &[agent], owner, None, None, None).await?;
        assert_eq!(paged.messages.len(), ROOM_MESSAGES as usize);
        assert!(paged.messages.iter().all(|message| message.id != pending_question && message.id != pending_card));
        assert_eq!(paged.needs_you_messages.iter().map(|message| message.id).collect::<std::collections::HashSet<_>>(), snapshot);
        let future_cutoff = Utc::now() + chrono::Duration::minutes(1);
        let incremental = store.quest_room(task, since, &[agent], owner, None, Some(future_cutoff), None).await?;
        assert!(incremental.messages.is_empty());
        assert_eq!(incremental.needs_you_messages.iter().map(|message| message.id).collect::<std::collections::HashSet<_>>(), snapshot);
        let owner_focused = store.quest_room(task, since, &[agent], owner, None, Some(future_cutoff), Some(answer)).await?;
        assert_eq!(owner_focused.messages.iter().map(|message| message.id).collect::<Vec<_>>(), vec![question, answer]);
        assert_eq!(owner_focused.needs_you_messages.iter().map(|message| message.id).collect::<std::collections::HashSet<_>>(), snapshot);

        let focused = store.quest_room(task, since, &[agent], guest, None, Some(future_cutoff), Some(answer)).await?;
        assert_eq!(focused.messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), vec!["older room question", "older room answer"]);
        assert!(focused.messages.iter().all(|m| m.body != "owner private note"));
        let shared_focus = store.quest_room(task, since, &[agent], guest, None, Some(future_cutoff), Some(shared_answer)).await?;
        assert_eq!(shared_focus.messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), vec!["shared old question", "shared old answer"]);
        let private_focus = store.quest_room(task, since, &[agent], guest, None, Some(future_cutoff), Some(private_card)).await?;
        assert!(private_focus.messages.is_empty());
        let source_part_focus = store.quest_room(task, since, &[agent], guest, None, Some(future_cutoff), Some(source_part_answer)).await?;
        assert_eq!(source_part_focus.messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), vec!["source part question", "source part answer"]);
        let viewer = |user, role| crate::access::Viewer {
            user,
            org: Some(crate::orgs::DEFAULT_ORG),
            role,
            org_role: None,
            instance_admin: false,
        };
        let guest_viewer = viewer(guest, crate::access::Role::Read);
        assert_eq!(focused.needs_you_messages.iter().map(|message| message.id).collect::<std::collections::HashSet<_>>(), [pending_question, part_pending_question].into_iter().collect());
        let guest_snapshot = store.open_room_messages(&focused.needs_you_messages, &guest_viewer).await?;
        assert_eq!(guest_snapshot, [pending_question].into_iter().collect());
        assert!(store.open_room_messages(&focused.messages, &guest_viewer).await?.is_empty());
        assert!(store.open_room_messages(&source_part_focus.messages, &guest_viewer).await?.is_empty());
        assert!(!store.sees_message_as(source_part_answer, &guest_viewer).await?);
        let owner_viewer = viewer(owner, crate::access::Role::Write);
        assert_eq!(store.open_room_messages(&focused.messages, &owner_viewer).await?.len(), 2);
        assert_eq!(store.open_room_messages(&source_part_focus.messages, &owner_viewer).await?.len(), 2);
        let hidden_members = store.room_members_for(task, &[]).await?;
        assert!(hidden_members.iter().all(|member| member.member_kind != "session"));
        assert!(hidden_members.iter().any(|member| member.member_kind == "agent" && member.role == "lead"));
        assert_eq!(store.room_members_for(task, &[agent]).await?.iter().filter(|member| member.member_kind == "session").count(), 2);
        let owner_search = store.search(owner, crate::access::Role::Write, crate::orgs::DEFAULT_ORG, "older room question", 20).await?;
        assert!(owner_search.messages.iter().any(|message| message.reference == question.to_string()));
        let guest_search = store.search(guest, crate::access::Role::Read, crate::orgs::DEFAULT_ORG, "older room question", 20).await?;
        assert!(guest_search.messages.iter().all(|message| message.reference != question.to_string()));
        let guest_source_search = store.search(guest, crate::access::Role::Read, crate::orgs::DEFAULT_ORG, "source part question", 20).await?;
        assert!(guest_source_search.messages.iter().all(|message| message.reference != source_part_question.to_string()));
        let guest_source_answer_search = store.search(guest, crate::access::Role::Read, crate::orgs::DEFAULT_ORG, "source part answer", 20).await?;
        assert!(guest_source_answer_search.messages.iter().all(|message| message.reference != source_part_answer.to_string()));
        sqlx::query("UPDATE agent_profiles SET visibility = 'public' WHERE id = $1").bind(agent).execute(&pool).await?;
        let writer_viewer = viewer(writer, crate::access::Role::Write);
        let writer_room = store.quest_room(task, since, &[agent], writer, None, None, None).await?;
        assert_eq!(writer_room.needs_you_messages.iter().map(|message| message.id).collect::<std::collections::HashSet<_>>(), [pending_question, part_pending_question].into_iter().collect());
        assert_eq!(store.open_room_messages(&writer_room.needs_you_messages, &writer_viewer).await?.len(), 2);
        let admin_viewer = viewer(writer, crate::access::Role::Admin);
        assert_eq!(store.open_room_messages(&writer_room.needs_you_messages, &admin_viewer).await?.len(), 2);
        assert_eq!(store.open_room_messages(&focused.messages, &writer_viewer).await?.len(), 2);
        assert_eq!(store.open_room_messages(&source_part_focus.messages, &writer_viewer).await?.len(), 2);
        assert!(store.sees_message_as(source_part_answer, &writer_viewer).await?);
        assert!(store.open_rooms(&[task], &writer_viewer).await?.contains(&task));
        let writer_search = store.search(writer, crate::access::Role::Write, crate::orgs::DEFAULT_ORG, "older room question", 20).await?;
        assert!(writer_search.messages.iter().any(|message| message.reference == question.to_string()));
        let writer_source_search = store.search(writer, crate::access::Role::Write, crate::orgs::DEFAULT_ORG, "source part question", 20).await?;
        assert!(writer_source_search.messages.iter().any(|message| message.reference == source_part_question.to_string()));
        let writer_source_answer_search = store.search(writer, crate::access::Role::Write, crate::orgs::DEFAULT_ORG, "source part answer", 20).await?;
        assert!(writer_source_answer_search.messages.iter().any(|message| message.reference == source_part_answer.to_string()));
        let outsider_viewer = viewer(outsider, crate::access::Role::Read);
        assert!(!store.open_rooms(&[task], &outsider_viewer).await?.contains(&task));

        let hidden = store.quest_room(task, since, &[], owner, None, None, None).await?;
        assert!(hidden.sessions.is_empty() && hidden.messages.is_empty());

        sqlx::query("UPDATE agent_messages SET decision_status = 'parked' WHERE id = ANY($1)")
            .bind(vec![pending_question, part_pending_question])
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE agent_sessions SET plan_state = 'approved' WHERE id = $1").bind(work).execute(&pool).await?;
        let settled = store.quest_room(task, since, &[agent], owner, None, Some(Utc::now()), None).await?;
        assert!(settled.needs_you_messages.is_empty());

        let later = store.quest_room(task, since, &[agent], owner, Some(room.cursor), room.messages.last().map(|m| m.created_at), None).await?;
        assert!(later.events.is_empty() && later.messages.is_empty());

        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = ANY($1)").bind(vec![task, part, other]).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(guest).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(writer).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(outsider).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn quest_pattern_wraps_the_full_id() {
        let id = Uuid::nil();
        assert_eq!(quest_pattern(id), "%00000000-0000-0000-0000-000000000000%");
    }
}
