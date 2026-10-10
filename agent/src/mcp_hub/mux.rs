use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub type ClientId = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Server(Value),
    Client(ClientId, Value),
}

const INIT_ID: u64 = 0;
const NO_CLIENT: i64 = -32601;
const GONE: i64 = -32603;

struct Pending {
    client: ClientId,
    id: Value,
    progress: Option<String>,
}

enum Init {
    Idle,
    Waiting(Vec<(ClientId, Value)>),
    Ready(Value),
}

pub struct Mux {
    shared: bool,
    next_id: u64,
    next_ask: u64,
    seq: u64,
    init: Init,
    pending: HashMap<u64, Pending>,
    asks: HashMap<String, (ClientId, Value)>,
    progress: HashMap<String, (ClientId, Value)>,
    clients: BTreeMap<ClientId, u64>,
}

fn is_request(msg: &Value) -> bool {
    msg.get("method").is_some() && msg.get("id").is_some_and(|id| !id.is_null())
}

fn is_notification(msg: &Value) -> bool {
    msg.get("method").is_some() && msg.get("id").is_none_or(Value::is_null)
}

fn method(msg: &Value) -> &str {
    msg.get("method").and_then(Value::as_str).unwrap_or_default()
}

fn with_id(msg: &Value, id: Value) -> Value {
    let mut msg = msg.clone();
    msg["id"] = id;
    msg
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

impl Mux {
    pub fn new(shared: bool) -> Self {
        Self {
            shared,
            next_id: INIT_ID + 1,
            next_ask: 1,
            seq: 0,
            init: Init::Idle,
            pending: HashMap::new(),
            asks: HashMap::new(),
            progress: HashMap::new(),
            clients: BTreeMap::new(),
        }
    }

    pub fn ready(&self) -> bool {
        matches!(self.init, Init::Ready(_))
    }

    pub fn attach(&mut self, client: ClientId, init_params: &Value) -> Vec<Out> {
        self.seq += 1;
        self.clients.entry(client).or_insert(self.seq);
        if !matches!(self.init, Init::Idle) {
            return Vec::new();
        }
        self.init = Init::Waiting(Vec::new());
        let mut params = init_params.clone();
        if !params.is_object() {
            params = json!({});
        }
        if params.get("protocolVersion").is_none() {
            params["protocolVersion"] = json!("2025-06-18");
        }
        if params.get("clientInfo").is_none() {
            params["clientInfo"] = json!({ "name": "world-mcp-hub", "version": env!("CARGO_PKG_VERSION") });
        }
        if self.shared || params.get("capabilities").is_none() {
            params["capabilities"] = json!({});
        }
        vec![Out::Server(json!({ "jsonrpc": "2.0", "id": INIT_ID, "method": "initialize", "params": params }))]
    }

    pub fn on_client(&mut self, client: ClientId, msg: Value) -> Vec<Out> {
        if is_request(&msg) {
            self.seq += 1;
            if let Some(last) = self.clients.get_mut(&client) {
                *last = self.seq;
            }
            let id = msg["id"].clone();
            match method(&msg) {
                "ping" => return vec![Out::Client(client, result(id, json!({})))],
                "initialize" => {
                    return match &self.init {
                        Init::Ready(cached) => vec![Out::Client(client, result(id, cached.clone()))],
                        Init::Waiting(_) => {
                            self.queue(client, msg);
                            Vec::new()
                        }
                        Init::Idle => {
                            let params = msg.get("params").cloned().unwrap_or_default();
                            let out = self.attach(client, &params);
                            self.queue(client, msg);
                            out
                        }
                    };
                }
                _ => {}
            }
            match self.init {
                Init::Ready(_) => {}
                Init::Waiting(_) => {
                    self.queue(client, msg);
                    return Vec::new();
                }
                Init::Idle => return vec![Out::Client(client, error(id, GONE, "The MCP server is not initialized; start the session again"))],
            }
            let upstream = self.next_id;
            self.next_id += 1;
            let mut forwarded = with_id(&msg, json!(upstream));
            let token = msg.pointer("/params/_meta/progressToken").cloned();
            let progress = token.map(|token| {
                let rewritten = format!("{client}:{upstream}");
                self.progress.insert(rewritten.clone(), (client, token));
                forwarded["params"]["_meta"]["progressToken"] = json!(rewritten);
                rewritten
            });
            self.pending.insert(upstream, Pending { client, id, progress });
            return vec![Out::Server(forwarded)];
        }
        if is_notification(&msg) {
            return match method(&msg) {
                "notifications/initialized" => Vec::new(),
                "notifications/cancelled" => {
                    let wanted = msg.pointer("/params/requestId").cloned().unwrap_or_default();
                    let Some(upstream) = self
                        .pending
                        .iter()
                        .find(|(_, pending)| pending.client == client && pending.id == wanted)
                        .map(|(upstream, _)| *upstream)
                    else {
                        return Vec::new();
                    };
                    let mut forwarded = msg.clone();
                    forwarded["params"]["requestId"] = json!(upstream);
                    vec![Out::Server(forwarded)]
                }
                _ if self.shared => Vec::new(),
                _ if !self.ready() => {
                    self.queue(client, msg);
                    Vec::new()
                }
                _ => vec![Out::Server(msg)],
            };
        }
        let Some(key) = msg.get("id").and_then(Value::as_str).map(String::from) else {
            return Vec::new();
        };
        match self.asks.remove(&key) {
            Some((owner, server_id)) if owner == client => vec![Out::Server(with_id(&msg, server_id))],
            Some(other) => {
                self.asks.insert(key, other);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    pub fn on_server(&mut self, msg: Value) -> Vec<Out> {
        if is_request(&msg) {
            let id = msg["id"].clone();
            match method(&msg) {
                "ping" => return vec![Out::Server(result(id, json!({})))],
                "roots/list" if self.shared => return vec![Out::Server(result(id, json!({ "roots": [] })))],
                _ => {}
            }
            let Some(target) = self.target() else {
                return vec![Out::Server(error(id, NO_CLIENT, "no client can answer this request"))];
            };
            let key = format!("hub-{}", self.next_ask);
            self.next_ask += 1;
            self.asks.insert(key.clone(), (target, id));
            return vec![Out::Client(target, with_id(&msg, json!(key)))];
        }
        if is_notification(&msg) {
            return match method(&msg) {
                "notifications/progress" => {
                    let token = msg.pointer("/params/progressToken").and_then(Value::as_str).unwrap_or_default();
                    match self.progress.get(token) {
                        Some((client, original)) => {
                            let mut forwarded = msg.clone();
                            forwarded["params"]["progressToken"] = original.clone();
                            vec![Out::Client(*client, forwarded)]
                        }
                        None => Vec::new(),
                    }
                }
                "notifications/cancelled" => {
                    let wanted = msg.pointer("/params/requestId").cloned().unwrap_or_default();
                    let Some(key) = self.asks.iter().find(|(_, (_, id))| *id == wanted).map(|(key, _)| key.clone()) else {
                        return Vec::new();
                    };
                    let (client, _) = self.asks.remove(&key).expect("found ask");
                    let mut forwarded = msg.clone();
                    forwarded["params"]["requestId"] = json!(key);
                    vec![Out::Client(client, forwarded)]
                }
                _ if self.ready() => self.clients.keys().map(|client| Out::Client(*client, msg.clone())).collect(),
                _ => Vec::new(),
            };
        }
        let Some(upstream) = msg.get("id").and_then(Value::as_u64) else {
            return Vec::new();
        };
        if upstream == INIT_ID {
            return self.initialized(msg);
        }
        let Some(pending) = self.pending.remove(&upstream) else {
            return Vec::new();
        };
        if let Some(token) = pending.progress {
            self.progress.remove(&token);
        }
        vec![Out::Client(pending.client, with_id(&msg, pending.id))]
    }

    pub fn detach(&mut self, client: ClientId) -> Vec<Out> {
        self.clients.remove(&client);
        let mut out = Vec::new();
        let gone: Vec<u64> = self.pending.iter().filter(|(_, p)| p.client == client).map(|(id, _)| *id).collect();
        for upstream in gone {
            if let Some(pending) = self.pending.remove(&upstream) {
                if let Some(token) = pending.progress {
                    self.progress.remove(&token);
                }
            }
            out.push(Out::Server(json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": upstream, "reason": "client disconnected" },
            })));
        }
        let asks: Vec<String> = self.asks.iter().filter(|(_, (owner, _))| *owner == client).map(|(key, _)| key.clone()).collect();
        for key in asks {
            if let Some((_, server_id)) = self.asks.remove(&key) {
                out.push(Out::Server(error(server_id, GONE, "client disconnected")));
            }
        }
        if let Init::Waiting(queued) = &mut self.init {
            queued.retain(|(owner, _)| *owner != client);
        }
        out
    }

    pub fn fail_all(&mut self, reason: &str) -> Vec<Out> {
        let mut out: Vec<Out> = self
            .pending
            .drain()
            .map(|(_, pending)| Out::Client(pending.client, error(pending.id, GONE, reason)))
            .collect();
        if let Init::Waiting(queued) = std::mem::replace(&mut self.init, Init::Idle) {
            out.extend(queued.into_iter().filter(|(_, msg)| is_request(msg)).map(|(client, msg)| Out::Client(client, error(msg["id"].clone(), GONE, reason))));
        }
        self.init = Init::Idle;
        self.progress.clear();
        self.asks.clear();
        out
    }

    fn queue(&mut self, client: ClientId, msg: Value) {
        if let Init::Waiting(queued) = &mut self.init {
            queued.push((client, msg));
        }
    }

    fn initialized(&mut self, msg: Value) -> Vec<Out> {
        let Init::Waiting(queued) = std::mem::replace(&mut self.init, Init::Idle) else {
            return Vec::new();
        };
        let Some(init) = msg.get("result").cloned() else {
            let message = msg.pointer("/error/message").and_then(Value::as_str).unwrap_or("MCP server failed to start").to_string();
            return queued
                .into_iter()
                .filter(|(_, msg)| is_request(msg))
                .map(|(client, msg)| Out::Client(client, error(msg["id"].clone(), GONE, &message)))
                .collect();
        };
        self.init = Init::Ready(init);
        let mut out = vec![Out::Server(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))];
        for (client, msg) in queued {
            out.extend(self.on_client(client, msg));
        }
        out
    }

    fn target(&self) -> Option<ClientId> {
        self.pending
            .values()
            .map(|pending| pending.client)
            .filter_map(|client| self.clients.get(&client).map(|seq| (*seq, client)))
            .max()
            .map(|(_, client)| client)
            .or_else(|| (self.clients.len() == 1).then(|| *self.clients.keys().next().expect("one client")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_req(id: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": { "roots": {} }, "clientInfo": { "name": "claude-code" } } })
    }

    fn ready(shared: bool, clients: &[ClientId]) -> Mux {
        let mut mux = Mux::new(shared);
        for client in clients {
            mux.attach(*client, &init_req(json!(0))["params"]);
        }
        mux.on_server(json!({ "jsonrpc": "2.0", "id": 0, "result": { "protocolVersion": "2025-06-18", "capabilities": { "tools": {} }, "serverInfo": { "name": "s" } } }));
        mux
    }

    fn server_msgs(out: &[Out]) -> Vec<Value> {
        out.iter().filter_map(|o| if let Out::Server(v) = o { Some(v.clone()) } else { None }).collect()
    }

    #[test]
    fn initializes_once_and_answers_every_client_from_cache() {
        let mut mux = Mux::new(true);
        let out = mux.on_client(1, init_req(json!(0)));
        let sent = server_msgs(&out);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["method"], "initialize");
        assert_eq!(sent[0]["params"]["capabilities"], json!({}), "shared servers never see client capabilities");
        assert!(mux.on_client(2, init_req(json!("a"))).is_empty());
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "id": 0, "result": { "serverInfo": { "name": "s" } } }));
        assert_eq!(out[0], Out::Server(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })));
        assert!(out.contains(&Out::Client(1, json!({ "jsonrpc": "2.0", "id": 0, "result": { "serverInfo": { "name": "s" } } }))));
        assert!(out.contains(&Out::Client(2, json!({ "jsonrpc": "2.0", "id": "a", "result": { "serverInfo": { "name": "s" } } }))));
        let out = mux.on_client(3, init_req(json!(7)));
        assert_eq!(out, vec![Out::Client(3, json!({ "jsonrpc": "2.0", "id": 7, "result": { "serverInfo": { "name": "s" } } }))]);
        assert!(mux.on_client(3, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_empty());
    }

    #[test]
    fn unshared_servers_keep_the_client_capabilities() {
        let mut mux = Mux::new(false);
        let sent = server_msgs(&mux.on_client(1, init_req(json!(0))));
        assert_eq!(sent[0]["params"]["capabilities"], json!({ "roots": {} }));
    }

    #[test]
    fn rewrites_colliding_ids_and_routes_replies_back() {
        let mut mux = ready(true, &[1, 2]);
        let a = server_msgs(&mux.on_client(1, json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "x" } })));
        let b = server_msgs(&mux.on_client(2, json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "y" } })));
        let (ida, idb) = (a[0]["id"].clone(), b[0]["id"].clone());
        assert_ne!(ida, idb);
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "id": idb, "result": { "content": "y" } }));
        assert_eq!(out, vec![Out::Client(2, json!({ "jsonrpc": "2.0", "id": 5, "result": { "content": "y" } }))]);
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "id": ida, "error": { "code": 1, "message": "no" } }));
        assert_eq!(out, vec![Out::Client(1, json!({ "jsonrpc": "2.0", "id": 5, "error": { "code": 1, "message": "no" } }))]);
        assert!(mux.on_server(json!({ "jsonrpc": "2.0", "id": ida, "result": {} })).is_empty(), "a reply is delivered once");
    }

    #[test]
    fn cancellation_and_progress_follow_the_rewritten_ids() {
        let mut mux = ready(true, &[1, 2]);
        let sent = server_msgs(&mux.on_client(1, json!({ "jsonrpc": "2.0", "id": "r", "method": "tools/call", "params": { "_meta": { "progressToken": 9 } } })));
        let upstream = sent[0]["id"].clone();
        let token = sent[0]["params"]["_meta"]["progressToken"].clone();
        assert_ne!(token, json!(9));
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": token, "progress": 1 } }));
        assert_eq!(out, vec![Out::Client(1, json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": 9, "progress": 1 } }))]);
        assert!(mux.on_client(2, json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": "r" } })).is_empty(), "another client cannot cancel it");
        let out = mux.on_client(1, json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": "r" } }));
        assert_eq!(server_msgs(&out)[0]["params"]["requestId"], upstream);
    }

    #[test]
    fn server_requests_go_to_the_active_client_and_answers_come_back() {
        let mut mux = ready(true, &[1, 2]);
        mux.on_client(2, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call" }));
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "id": 44, "method": "sampling/createMessage", "params": {} }));
        let Out::Client(target, ask) = &out[0] else { panic!("{out:?}") };
        assert_eq!(*target, 2);
        let key = ask["id"].clone();
        assert!(mux.on_client(1, json!({ "jsonrpc": "2.0", "id": key, "result": {} })).is_empty(), "only the asked client answers");
        let out = mux.on_client(2, json!({ "jsonrpc": "2.0", "id": key, "result": { "ok": true } }));
        assert_eq!(out, vec![Out::Server(json!({ "jsonrpc": "2.0", "id": 44, "result": { "ok": true } }))]);
        assert_eq!(server_msgs(&mux.on_server(json!({ "jsonrpc": "2.0", "id": 45, "method": "ping" })))[0]["result"], json!({}));
        assert_eq!(server_msgs(&mux.on_server(json!({ "jsonrpc": "2.0", "id": 46, "method": "roots/list" })))[0]["result"], json!({ "roots": [] }));
    }

    #[test]
    fn notifications_broadcast_and_departures_cancel_their_requests() {
        let mut mux = ready(true, &[1, 2]);
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
        assert_eq!(out.len(), 2);
        assert!(mux.on_client(1, json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed" })).is_empty(), "shared servers ignore per-client notifications");
        let upstream = server_msgs(&mux.on_client(1, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call" })))[0]["id"].clone();
        let out = mux.detach(1);
        assert_eq!(server_msgs(&out)[0]["params"]["requestId"], upstream);
        assert!(mux.on_server(json!({ "jsonrpc": "2.0", "id": upstream, "result": {} })).is_empty());
        assert_eq!(mux.on_server(json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": {} })).len(), 1);
    }

    #[test]
    fn a_crash_fails_every_waiting_request() {
        let mut mux = ready(true, &[1, 2]);
        mux.on_client(1, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call" }));
        mux.on_client(2, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call" }));
        let out = mux.fail_all("restarted");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|o| matches!(o, Out::Client(_, v) if v["error"]["message"] == "restarted" && v["id"] == 3)));
    }

    #[test]
    fn a_failed_start_answers_every_later_request() {
        let mut mux = Mux::new(true);
        mux.on_client(1, init_req(json!(0)));
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "id": 0, "error": { "code": -32603, "message": "bad params" } }));
        assert_eq!(out, vec![Out::Client(1, error(json!(0), GONE, "bad params"))]);
        let out = mux.on_client(1, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
        assert!(matches!(&out[..], [Out::Client(1, reply)] if reply["error"].is_object()), "{out:?}");
    }

    #[test]
    fn requests_before_the_server_is_ready_wait_for_it() {
        let mut mux = Mux::new(true);
        mux.attach(1, &json!({}));
        assert!(mux.on_client(1, json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" })).is_empty());
        let out = mux.on_server(json!({ "jsonrpc": "2.0", "id": 0, "result": {} }));
        let sent = server_msgs(&out);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["method"], "tools/list");
        assert_ne!(sent[1]["id"], json!(0));
    }
}
