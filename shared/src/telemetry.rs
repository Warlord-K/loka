use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

const BUCKETS: [f64; 16] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 1800.0, 3600.0];
const LOG_BATCH: usize = 500;
const LOG_QUEUE: usize = 20_000;
const QUIET_TARGETS: [&str; 6] = ["hyper", "reqwest", "h2", "rustls", "tower", "sqlx::query"];

static METRICS_ON: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Counter,
    Gauge,
    Histogram,
}

enum Series {
    Value(f64),
    Histogram { bounds: &'static [f64], counts: Vec<u64>, sum: f64, count: u64 },
}

#[derive(Default)]
struct Registry {
    kinds: BTreeMap<&'static str, Kind>,
    series: BTreeMap<(&'static str, String), Series>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

pub fn metrics_on() -> bool {
    METRICS_ON.load(Ordering::Relaxed)
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn label_text(labels: &[(&str, &str)]) -> String {
    labels.iter().map(|(name, value)| format!("{name}=\"{}\"", escape(value))).collect::<Vec<_>>().join(",")
}

fn update(name: &'static str, kind: Kind, labels: &[(&str, &str)], bounds: &'static [f64], apply: impl FnOnce(&mut Series)) {
    if !metrics_on() {
        return;
    }
    let Ok(mut registry) = registry().lock() else { return };
    registry.kinds.entry(name).or_insert(kind);
    let series = registry.series.entry((name, label_text(labels))).or_insert_with(|| match kind {
        Kind::Histogram => Series::Histogram { bounds, counts: vec![0; bounds.len()], sum: 0.0, count: 0 },
        _ => Series::Value(0.0),
    });
    apply(series);
}

pub fn count(name: &'static str, labels: &[(&str, &str)], by: f64) {
    update(name, Kind::Counter, labels, &[], |series| {
        if let Series::Value(total) = series {
            *total += by;
        }
    });
}

pub fn gauge(name: &'static str, labels: &[(&str, &str)], value: f64) {
    update(name, Kind::Gauge, labels, &[], |series| {
        if let Series::Value(current) = series {
            *current = value;
        }
    });
}

pub fn total(name: &'static str, labels: &[(&str, &str)], value: f64) {
    update(name, Kind::Counter, labels, &[], |series| {
        if let Series::Value(current) = series {
            *current = value;
        }
    });
}

pub fn add(name: &'static str, labels: &[(&str, &str)], delta: f64) {
    update(name, Kind::Gauge, labels, &[], |series| {
        if let Series::Value(current) = series {
            *current += delta;
        }
    });
}

pub fn observe(name: &'static str, labels: &[(&str, &str)], seconds: f64) {
    observe_in(name, labels, seconds, &BUCKETS);
}

pub fn observe_in(name: &'static str, labels: &[(&str, &str)], value: f64, buckets: &'static [f64]) {
    update(name, Kind::Histogram, labels, buckets, |series| {
        if let Series::Histogram { bounds, counts, sum, count } = series {
            if let Some(slot) = bounds.iter().position(|bound| value <= *bound) {
                counts[slot] += 1;
            }
            *sum += value;
            *count += 1;
        }
    });
}

fn with_labels(name: &str, labels: &str) -> String {
    if labels.is_empty() {
        name.to_string()
    } else {
        format!("{name}{{{labels}}}")
    }
}

pub fn render() -> String {
    let Ok(registry) = registry().lock() else { return String::new() };
    let mut out = String::new();
    let mut typed = None;
    for ((name, labels), series) in &registry.series {
        if typed != Some(*name) {
            let kind = match registry.kinds.get(name) {
                Some(Kind::Counter) => "counter",
                Some(Kind::Histogram) => "histogram",
                _ => "gauge",
            };
            let _ = writeln!(out, "# TYPE {name} {kind}");
            typed = Some(*name);
        }
        match series {
            Series::Value(value) => {
                let _ = writeln!(out, "{} {value}", with_labels(name, labels));
            }
            Series::Histogram { bounds, counts, sum, count } => {
                let sep = if labels.is_empty() { "" } else { "," };
                let mut running = 0;
                for (bound, hits) in bounds.iter().zip(counts) {
                    running += hits;
                    let _ = writeln!(out, "{name}_bucket{{{labels}{sep}le=\"{bound}\"}} {running}");
                }
                let _ = writeln!(out, "{name}_bucket{{{labels}{sep}le=\"+Inf\"}} {count}");
                let _ = writeln!(out, "{} {sum}", with_labels(&format!("{name}_sum"), labels));
                let _ = writeln!(out, "{} {count}", with_labels(&format!("{name}_count"), labels));
            }
        }
    }
    out
}

pub fn host() -> String {
    let mut buffer = [0u8; 256];
    let found = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
    let name = if found { String::from_utf8_lossy(&buffer[..end]).to_string() } else { String::new() };
    name.split('.').next().filter(|part| !part.is_empty()).unwrap_or("unknown").to_string()
}

fn base_url(var: &str) -> Option<String> {
    std::env::var(var).ok().map(|url| url.trim().trim_end_matches('/').to_string()).filter(|url| !url.is_empty())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap_or_default()
}

fn sample_process() {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
        let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
        total("process_cpu_seconds_total", &[], seconds(usage.ru_utime) + seconds(usage.ru_stime));
    }
    if let Some(bytes) = resident_bytes() {
        gauge("process_resident_memory_bytes", &[], bytes as f64);
    }
}

#[cfg(target_os = "macos")]
fn resident_bytes() -> Option<u64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(std::process::id() as libc::c_int, libc::PROC_PIDTASKINFO, 0, (&mut info as *mut libc::proc_taskinfo).cast(), size)
    };
    (read == size).then_some(info.pti_resident_size)
}

#[cfg(not(target_os = "macos"))]
fn resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * page.max(0) as u64)
}

pub fn start(service: &str) {
    let Some(url) = base_url("WORLD_METRICS_URL") else { return };
    if tokio::runtime::Handle::try_current().is_err() || METRICS_ON.swap(true, Ordering::SeqCst) {
        return;
    }
    let every = std::env::var("WORLD_METRICS_INTERVAL_SECS").ok().and_then(|secs| secs.parse().ok()).unwrap_or(15u64).max(1);
    let target = format!(
        "{url}/api/v1/import/prometheus?extra_label=service={}&extra_label=host={}",
        urlencoding::encode(service),
        urlencoding::encode(&host())
    );
    tokio::spawn(async move {
        let client = client();
        let mut tick = tokio::time::interval(Duration::from_secs(every));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failing = false;
        loop {
            tick.tick().await;
            sample_process();
            let body = render();
            match client.post(&target).body(body).send().await.and_then(|response| response.error_for_status()) {
                Ok(_) => failing = false,
                Err(error) if !failing => {
                    failing = true;
                    tracing::warn!(%error, "Could not push metrics");
                }
                Err(_) => {}
            }
        }
    });
}

#[derive(Default)]
struct Fields(Map<String, Value>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), Value::String(format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), Value::String(value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }
}

pub struct LogShipper {
    sender: tokio::sync::mpsc::Sender<Value>,
    service: String,
    host: String,
}

pub fn log_shipper(service: &str) -> Option<LogShipper> {
    let url = base_url("WORLD_LOGS_URL")?;
    tokio::runtime::Handle::try_current().ok()?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Value>(LOG_QUEUE);
    let target = format!("{url}/insert/jsonline?_stream_fields=service,host&_msg_field=_msg&_time_field=_time");
    tokio::spawn(async move {
        let client = client();
        let mut batch = Vec::with_capacity(LOG_BATCH);
        let mut failing = false;
        while receiver.recv_many(&mut batch, LOG_BATCH).await > 0 {
            let body = batch.drain(..).map(|line| line.to_string()).collect::<Vec<_>>().join("\n");
            match client.post(&target).body(body).send().await.and_then(|response| response.error_for_status()) {
                Ok(_) => failing = false,
                Err(error) if !failing => {
                    failing = true;
                    eprintln!("Could not ship logs: {error}");
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    Some(LogShipper { sender, service: service.to_string(), host: host() })
}

impl<S> Layer<S> for LogShipper
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &tracing::span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        span.extensions_mut().insert(fields);
    }

    fn on_record(&self, id: &tracing::span::Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<Fields>() {
            values.record(fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let target = event.metadata().target();
        if QUIET_TARGETS.iter().any(|quiet| target.starts_with(quiet)) {
            return;
        }
        let mut line = Map::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<Fields>() {
                    for (key, value) in &fields.0 {
                        line.insert(key.clone(), value.clone());
                    }
                }
                line.insert("span".into(), Value::String(span.name().to_string()));
            }
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let message = fields.0.remove("message").unwrap_or(Value::String(String::new()));
        line.extend(fields.0);
        line.insert("_msg".into(), message);
        line.insert("_time".into(), Value::String(chrono::Utc::now().to_rfc3339()));
        line.insert("level".into(), Value::String(event.metadata().level().as_str().to_lowercase()));
        line.insert("target".into(), Value::String(target.to_string()));
        line.insert("service".into(), Value::String(self.service.clone()));
        line.insert("host".into(), Value::String(self.host.clone()));
        let _ = self.sender.try_send(Value::Object(line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_gauges_and_histograms() {
        METRICS_ON.store(true, Ordering::SeqCst);
        count("test_requests_total", &[("route", "/a\"b")], 2.0);
        count("test_requests_total", &[("route", "/a\"b")], 1.0);
        gauge("test_open", &[], 4.0);
        observe("test_seconds", &[("kind", "x")], 0.2);
        observe("test_seconds", &[("kind", "x")], 7200.0);
        let text = render();
        assert!(text.contains("# TYPE test_requests_total counter\ntest_requests_total{route=\"/a\\\"b\"} 3\n"));
        assert!(text.contains("\ntest_open 4\n"));
        assert!(text.contains("test_seconds_bucket{kind=\"x\",le=\"0.1\"} 0\n"));
        assert!(text.contains("test_seconds_bucket{kind=\"x\",le=\"0.25\"} 1\n"));
        assert!(text.contains("test_seconds_bucket{kind=\"x\",le=\"3600\"} 1\n"));
        assert!(text.contains("test_seconds_bucket{kind=\"x\",le=\"+Inf\"} 2\n"));
        assert!(text.contains("test_seconds_count{kind=\"x\"} 2\n"));
    }
}
