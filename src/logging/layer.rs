//! Unified redacting `tracing_subscriber::Layer` driving BOTH stdout (`text` or `json`) and
//! asynchronous bounded log storage (`logs.db` or ClickHouse) from a single redaction pass.

use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::{Context, Filter, Layer};

use crate::config::LogFormat;

use super::redact::{redact_field, redact_text};
use super::{LogRecord, level_to_num};

/// Single-pass tracing layer that redacts every event once, emits formatted output to stdout
/// according to `RUST_LOG` + `LOG_FORMAT`, and pushes matching records non-blockingly into the
/// bounded storage channel (`try_send`, incrementing `dropped` if full).
pub struct RedactingLayer {
    stdout_filter: EnvFilter,
    format: LogFormat,
    store_min_level_num: i32,
    tx: Option<mpsc::Sender<LogRecord>>,
    dropped: Arc<AtomicU64>,
}

impl RedactingLayer {
    pub fn new(
        stdout_filter: EnvFilter,
        format: LogFormat,
        store_min_level_num: i32,
        tx: Option<mpsc::Sender<LogRecord>>,
        dropped: Arc<AtomicU64>,
    ) -> Self {
        Self {
            stdout_filter,
            format,
            store_min_level_num,
            tx,
            dropped,
        }
    }
}

struct RedactingVisitor {
    message: String,
    fields: Map<String, Value>,
    feed_id: Option<i64>,
    task_id: Option<i64>,
    route_id: Option<i64>,
    channel_id: Option<i64>,
    destination_id: Option<i64>,
    plugin_id: Option<String>,
}

impl RedactingVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Map::new(),
            feed_id: None,
            task_id: None,
            route_id: None,
            channel_id: None,
            destination_id: None,
            plugin_id: None,
        }
    }

    fn record_i64_field(&mut self, name: &str, value: i64) {
        if super::redact::is_sensitive_field_name(name) {
            self.fields
                .insert(name.to_string(), Value::String("[redacted]".to_string()));
            return;
        }
        match name {
            "feed_id" => self.feed_id = Some(value),
            "task_id" => self.task_id = Some(value),
            "route_id" => self.route_id = Some(value),
            "channel_id" => self.channel_id = Some(value),
            "destination_id" => self.destination_id = Some(value),
            _ => {}
        }
        self.fields
            .insert(name.to_string(), Value::Number(value.into()));
    }

    fn record_str_field(&mut self, name: &str, raw: &str) {
        if name == "message" {
            self.message = redact_text(raw);
            return;
        }
        let redacted = redact_field(name, raw);
        if name == "plugin_id" && !redacted.is_empty() && redacted != "[redacted]" {
            self.plugin_id = Some(redacted.clone());
        }
        self.fields
            .insert(name.to_string(), Value::String(redacted));
    }
}

impl Visit for RedactingVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_i64_field(field.name(), value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        let name = field.name();
        if super::redact::is_sensitive_field_name(name) {
            self.fields
                .insert(name.to_string(), Value::String("[redacted]".to_string()));
            return;
        }
        if let Ok(signed) = i64::try_from(value) {
            match name {
                "feed_id" => self.feed_id = Some(signed),
                "task_id" => self.task_id = Some(signed),
                "route_id" => self.route_id = Some(signed),
                "channel_id" => self.channel_id = Some(signed),
                "destination_id" => self.destination_id = Some(signed),
                _ => {}
            }
        }
        self.fields
            .insert(name.to_string(), Value::Number(value.into()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        let name = field.name();
        if super::redact::is_sensitive_field_name(name) {
            self.fields
                .insert(name.to_string(), Value::String("[redacted]".to_string()));
            return;
        }
        self.fields.insert(name.to_string(), Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        let name = field.name();
        if super::redact::is_sensitive_field_name(name) {
            self.fields
                .insert(name.to_string(), Value::String("[redacted]".to_string()));
            return;
        }
        if let Some(num) = serde_json::Number::from_f64(value) {
            self.fields.insert(name.to_string(), Value::Number(num));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_str_field(field.name(), value);
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_str_field(field.name(), &value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record_str_field(field.name(), &format!("{value:?}"));
    }
}

impl<S> Layer<S> for RedactingLayer
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level_num = level_to_num(*meta.level());
        let emit_stdout = Filter::enabled(&self.stdout_filter, meta, &ctx);
        let emit_store = self.tx.is_some()
            && level_num >= self.store_min_level_num
            && (meta.target().starts_with("feedcourier")
                || meta.target().starts_with("tower_http"));

        if !emit_stdout && !emit_store {
            return;
        }

        let now = Utc::now();
        let ts_ms = now.timestamp_millis();
        let mut visitor = RedactingVisitor::new();
        event.record(&mut visitor);

        let level_str = level_label(*meta.level());
        let target = meta.target();

        if emit_stdout {
            let line = match self.format {
                LogFormat::Text => format_text_line(
                    &now.to_rfc3339_opts(SecondsFormat::Millis, true),
                    level_str,
                    target,
                    &visitor.message,
                    &visitor.fields,
                ),
                LogFormat::Json => format_json_line(
                    &now.to_rfc3339_opts(SecondsFormat::Millis, true),
                    level_str,
                    target,
                    &visitor.message,
                    &visitor.fields,
                ),
            };
            let mut handle = io::stdout().lock();
            let _ = writeln!(handle, "{line}");
        }

        if emit_store && let Some(ref tx) = self.tx {
            let fields_json =
                serde_json::to_string(&visitor.fields).unwrap_or_else(|_| "{}".to_string());
            let record = LogRecord {
                ts_ms,
                level: level_str.to_string(),
                level_num,
                target: target.to_string(),
                message: visitor.message,
                fields_json,
                feed_id: visitor.feed_id,
                task_id: visitor.task_id,
                route_id: visitor.route_id,
                channel_id: visitor.channel_id,
                destination_id: visitor.destination_id,
                plugin_id: visitor.plugin_id,
            };
            if tx.try_send(record).is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn level_label(level: Level) -> &'static str {
    match level {
        Level::TRACE => "TRACE",
        Level::DEBUG => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARN",
        Level::ERROR => "ERROR",
    }
}

fn format_text_line(
    ts: &str,
    level: &str,
    target: &str,
    message: &str,
    fields: &Map<String, Value>,
) -> String {
    let mut out = format!("{ts} {level:>5} {target}: {message}");
    for (k, v) in fields {
        out.push(' ');
        out.push_str(k);
        out.push('=');
        match v {
            Value::String(s) => {
                if s.contains(' ') || s.is_empty() {
                    out.push('"');
                    out.push_str(&s.replace('"', "\\\""));
                    out.push('"');
                } else {
                    out.push_str(s);
                }
            }
            other => out.push_str(&other.to_string()),
        }
    }
    out
}

fn format_json_line(
    ts: &str,
    level: &str,
    target: &str,
    message: &str,
    fields: &Map<String, Value>,
) -> String {
    let mut obj = Map::with_capacity(5);
    obj.insert("ts".to_string(), Value::String(ts.to_string()));
    obj.insert("level".to_string(), Value::String(level.to_string()));
    obj.insert("target".to_string(), Value::String(target.to_string()));
    obj.insert("message".to_string(), Value::String(message.to_string()));
    if !fields.is_empty() {
        obj.insert("fields".to_string(), Value::Object(fields.clone()));
    }
    serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
}
