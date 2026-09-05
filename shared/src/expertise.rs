use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::world::{WorldAgent, WorldStore};

pub const MAX_TAGS: usize = 8;

pub fn normalize_tag(tag: &str) -> Option<String> {
    let t: String = tag
        .trim()
        .trim_start_matches('#')
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '/' || c == '-' || c == '.' { c } else { '-' })
        .collect();
    let t = t.trim_matches('-').to_string();
    (!t.is_empty() && t.len() <= 40).then_some(t)
}

pub fn merge_tags(existing: &[String], add: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in existing.iter().chain(add.iter()) {
        if let Some(n) = normalize_tag(t) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out.truncate(MAX_TAGS);
    out
}

fn words(text: &str) -> Vec<String> {
    const STOP: &[&str] = &["the", "and", "for", "with", "who", "should", "this", "that", "task", "work", "into", "from", "some", "about", "make", "need", "what"];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOP.contains(w))
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, FromRow)]
struct Evidence {
    title: String,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertMatch {
    pub agent_id: Uuid,
    pub name: String,
    pub score: f64,
    pub status: String,
    pub open_tasks: i64,
    pub tags: Vec<String>,
    pub evidence: Vec<String>,
}

impl WorldStore {
    pub async fn set_expertise(&self, agent_id: Uuid, tags: &[String], expertise: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE agent_profiles SET tags = $2, expertise = COALESCE($3, expertise), updated_at = NOW() WHERE id = $1")
            .bind(agent_id)
            .bind(tags)
            .bind(expertise)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn find_experts(&self, topic: &str, candidates: &[WorldAgent]) -> Result<Vec<ExpertMatch>> {
        let terms = words(topic);
        let mut out = Vec::new();
        for a in candidates {
            let mut score = 0.0;
            for t in &terms {
                if a.tags.iter().any(|tag| tag == t || tag.contains(t.as_str()) || t.contains(tag.as_str())) {
                    score += 3.0;
                }
                let blob = format!("{} {}", a.persona.as_deref().unwrap_or(""), a.expertise.as_deref().unwrap_or("")).to_lowercase();
                if blob.contains(t.as_str()) {
                    score += 1.5;
                }
            }
            let patterns: Vec<String> = terms.iter().map(|t| format!("%{t}%")).collect();
            let evidence: Vec<Evidence> = if patterns.is_empty() {
                Vec::new()
            } else {
                sqlx::query_as(
                    "SELECT title, status FROM agent_tasks
                     WHERE assigned_agent_id = $1
                       AND (title ILIKE ANY($2) OR description ILIKE ANY($2) OR result_content ILIKE ANY($2) OR repo ILIKE ANY($2))
                     ORDER BY (status = 'done' OR status = 'review') DESC, updated_at DESC NULLS LAST LIMIT 5",
                )
                .bind(a.id)
                .bind(&patterns)
                .fetch_all(self.pool())
                .await?
            };
            score += evidence.iter().map(|e| if e.status == "done" || e.status == "review" { 2.0 } else { 1.0 }).sum::<f64>();
            let open_tasks: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_tasks WHERE assigned_agent_id = $1 AND status NOT IN ('done', 'cancelled', 'failed')",
            )
            .bind(a.id)
            .fetch_one(self.pool())
            .await?;
            if score > 0.0 {
                out.push(ExpertMatch {
                    agent_id: a.id,
                    name: a.name.clone(),
                    score,
                    status: a.status.clone(),
                    open_tasks,
                    tags: a.tags.clone(),
                    evidence: evidence.into_iter().map(|e| format!("{} [{}]", e.title, e.status)).collect(),
                });
            }
        }
        out.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal).then(x.open_tasks.cmp(&y.open_tasks)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_normalize_and_merge() {
        assert_eq!(normalize_tag(" #Research Experiments "), Some("research-experiments".into()));
        assert_eq!(merge_tags(&["rust".into()], &["Rust".into(), "repo:a/b".into()]), vec!["rust".to_string(), "repo-a/b".to_string()]);
        assert_eq!(words("Who should do the research task on caching?"), vec!["research", "caching"]);
    }
}
