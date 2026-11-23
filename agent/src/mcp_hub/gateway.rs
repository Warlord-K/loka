use crate::world::tool_catalog::{self, Catalog, Delivery, Role};
use serde_json::{json, Value};
use std::collections::HashSet;

pub enum Step {
    Forward(String),
    Answer(String),
}

pub struct Gateway {
    server: String,
    asked_for_tools: HashSet<String>,
    full: Option<Vec<Value>>,
}

fn key(id: &Value) -> String {
    id.to_string()
}

fn text_result(id: &Value, text: String, is_error: bool) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": text }], "isError": is_error } }).to_string()
}

impl Gateway {
    pub fn new(server: &str) -> Self {
        Self { server: server.to_string(), asked_for_tools: HashSet::new(), full: None }
    }

    fn catalog(&self) -> Option<Catalog> {
        self.full.clone().map(|tools| Catalog::new(Role::Server, Delivery::Index, tools).labelled(&self.server))
    }

    pub fn on_client_line(&mut self, line: &str) -> Step {
        let Ok(mut message) = serde_json::from_str::<Value>(line) else {
            return Step::Forward(line.to_string());
        };
        match message.get("method").and_then(Value::as_str) {
            Some("tools/list") => {
                if let Some(id) = message.get("id") {
                    self.asked_for_tools.insert(key(id));
                }
                Step::Forward(line.to_string())
            }
            Some("tools/call") => {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                let Some(catalog) = self.catalog() else {
                    return Step::Forward(line.to_string());
                };
                match message.pointer("/params/name").and_then(Value::as_str) {
                    Some(tool_catalog::FIND_TOOLS) => {
                        let query = message.pointer("/params/arguments/query").and_then(Value::as_str).unwrap_or("");
                        Step::Answer(text_result(&id, catalog.find(query), false))
                    }
                    Some(tool_catalog::USE_TOOL) => {
                        let (name, arguments) = tool_catalog::use_tool_target(message.pointer("/params/arguments").unwrap_or(&Value::Null));
                        if name.is_empty() || name == tool_catalog::USE_TOOL || name == tool_catalog::FIND_TOOLS || !catalog.contains(&name) {
                            let near = catalog.near(&name);
                            let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.join(", ")) };
                            return Step::Answer(text_result(&id, format!("{} has no tool named \"{name}\".{hint} Run find_tools with words to search, or with no query to list them.", self.server), true));
                        }
                        message["params"]["name"] = json!(name);
                        message["params"]["arguments"] = arguments;
                        Step::Forward(message.to_string())
                    }
                    _ => Step::Forward(line.to_string()),
                }
            }
            _ => Step::Forward(line.to_string()),
        }
    }

    pub fn on_server_line(&mut self, line: &str) -> String {
        let Ok(mut message) = serde_json::from_str::<Value>(line) else {
            return line.to_string();
        };
        let Some(id) = message.get("id").map(key) else {
            return line.to_string();
        };
        if !self.asked_for_tools.remove(&id) {
            return line.to_string();
        }
        let Some(tools) = message.pointer("/result/tools").and_then(Value::as_array).cloned() else {
            return line.to_string();
        };
        let already_indexed = [tool_catalog::FIND_TOOLS, tool_catalog::USE_TOOL].iter().all(|meta| tools.iter().any(|tool| tool.get("name").and_then(Value::as_str) == Some(meta)));
        if already_indexed || message.pointer("/result/nextCursor").is_some() || !tool_catalog::gateway_worthwhile(&tools) {
            self.full = None;
            return line.to_string();
        }
        let listed = Catalog::new(Role::Server, Delivery::Index, tools.clone()).labelled(&self.server).listed();
        self.full = Some(tools);
        message["result"]["tools"] = Value::Array(listed);
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_tools(count: usize) -> Vec<Value> {
        (0..count).map(|i| json!({ "name": format!("tool_{i}"), "description": format!("does thing {i}. {}", "x".repeat(300)), "inputSchema": { "type": "object", "properties": {} } })).collect()
    }

    fn list_response(id: u64, tools: Vec<Value>) -> String {
        json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } }).to_string()
    }

    fn answered(step: Step) -> Value {
        match step {
            Step::Answer(text) => serde_json::from_str(&text).unwrap(),
            Step::Forward(text) => panic!("forwarded: {text}"),
        }
    }

    fn after_list(count: usize) -> Gateway {
        let mut gateway = Gateway::new("linear");
        assert!(matches!(gateway.on_client_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#), Step::Forward(_)));
        let listed: Value = serde_json::from_str(&gateway.on_server_line(&list_response(1, big_tools(count)))).unwrap();
        assert_eq!(listed["id"], 1);
        gateway
    }

    #[test]
    fn a_big_server_lists_only_the_two_meta_tools() {
        let mut gateway = Gateway::new("linear");
        gateway.on_client_line(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#);
        let out: Value = serde_json::from_str(&gateway.on_server_line(&list_response(7, big_tools(30)))).unwrap();
        let names: Vec<&str> = out["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["find_tools", "use_tool"]);
        assert!(out["result"]["tools"][0]["description"].as_str().unwrap().contains("linear"));
    }

    #[test]
    fn a_small_server_is_left_alone() {
        let mut gateway = Gateway::new("tiny");
        gateway.on_client_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let line = list_response(2, big_tools(2));
        assert_eq!(gateway.on_server_line(&line), line);
        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tool_0","arguments":{}}}"#;
        assert!(matches!(gateway.on_client_line(call), Step::Forward(forwarded) if forwarded == call));
    }

    #[test]
    fn a_server_that_already_indexes_its_tools_is_not_wrapped_again() {
        let mut gateway = Gateway::new("world");
        gateway.on_client_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let mut tools = big_tools(30);
        tools.push(json!({ "name": "find_tools", "description": "x", "inputSchema": {} }));
        tools.push(json!({ "name": "use_tool", "description": "x", "inputSchema": {} }));
        let line = list_response(2, tools);
        assert_eq!(gateway.on_server_line(&line), line);
        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_tools","arguments":{"query":"x"}}}"#;
        assert!(matches!(gateway.on_client_line(call), Step::Forward(forwarded) if forwarded == call));
    }

    #[test]
    fn a_paged_list_is_never_hidden_behind_the_gateway() {
        let mut gateway = Gateway::new("paged");
        gateway.on_client_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let line = json!({ "jsonrpc": "2.0", "id": 2, "result": { "tools": big_tools(30), "nextCursor": "c" } }).to_string();
        assert_eq!(gateway.on_server_line(&line), line);
    }

    #[test]
    fn find_tools_answers_locally_with_full_schemas() {
        let mut gateway = after_list(30);
        let found = answered(gateway.on_client_line(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"find_tools","arguments":{"query":"tool_12"}}}"#));
        assert_eq!(found["id"], 5);
        assert!(found["result"]["content"][0]["text"].as_str().unwrap().contains("## tool_12"), "{found}");
    }

    #[test]
    fn use_tool_forwards_the_plain_call_with_the_same_id() {
        let mut gateway = after_list(30);
        let Step::Forward(line) = gateway.on_client_line(r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"use_tool","arguments":{"name":"tool_3","arguments":{"q":"x"}}}}"#) else { panic!("answered") };
        let sent: Value = serde_json::from_str(&line).unwrap();
        assert_eq!((sent["id"].clone(), sent["params"].clone()), (json!(9), json!({ "name": "tool_3", "arguments": { "q": "x" } })));
    }

    #[test]
    fn a_wrong_name_gets_near_names_and_never_reaches_the_server() {
        let mut gateway = after_list(30);
        let refused = answered(gateway.on_client_line(r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"use_tool","arguments":{"name":"tool_99"}}}"#));
        assert_eq!(refused["result"]["isError"], true);
        assert!(refused["result"]["content"][0]["text"].as_str().unwrap().contains("find_tools"), "{refused}");
    }

    #[test]
    fn other_traffic_passes_through_untouched() {
        let mut gateway = after_list(30);
        for line in [r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "not json"] {
            assert!(matches!(gateway.on_client_line(line), Step::Forward(forwarded) if forwarded == line));
            assert_eq!(gateway.on_server_line(line), line);
        }
    }
}
