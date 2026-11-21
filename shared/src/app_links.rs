use uuid::Uuid;

pub fn app_url() -> String {
    std::env::var("WORLD_APP_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://loka.ygpt.dev".into())
}

pub fn docs_url() -> String {
    std::env::var("WORLD_DOCS_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://loka.ygpt.dev".into())
}

pub fn agent_url(slug: &str) -> String {
    format!("{}/world?agent={}", app_url(), urlencoding::encode(slug))
}

pub fn message_url(reference: &str) -> String {
    format!("{}/world?msg={}", app_url(), urlencoding::encode(reference))
}

pub fn quest_url(id: Uuid) -> String {
    format!("{}/world?quest={id}", app_url())
}

pub fn report_url(id: Uuid) -> String {
    format!("{}/world?report={id}", app_url())
}

pub fn canvas_item_url(id: Uuid, item: &str) -> String {
    format!("{}/world?report={id}&item={}", app_url(), urlencoding::encode(item))
}

pub fn todo_url(id: Uuid) -> String {
    format!("{}/world?todo={id}", app_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_links_on_the_app_url() {
        let id = Uuid::nil();
        assert!(report_url(id).ends_with(&format!("/world?report={id}")));
        assert!(agent_url("ada lovelace").ends_with("/world?agent=ada%20lovelace"));
        assert!(message_url("ch12").ends_with("/world?msg=ch12"));
        assert!(canvas_item_url(id, "i1a2").ends_with(&format!("/world?report={id}&item=i1a2")));
    }
}
