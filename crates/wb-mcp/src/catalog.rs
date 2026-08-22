//! Dynamic MCP tool and Skill catalogs backed by daemon plugin state.

use super::{DaemonClient, EVENTS_RESOURCE_URI};
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const POLL_MS: u64 = 3_000;

pub(super) struct JsonWriter<W> {
    inner: Arc<Mutex<W>>,
}

impl<W> Clone for JsonWriter<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<W: Write> JsonWriter<W> {
    pub(super) fn new(writer: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }

    pub(super) fn send(&self, message: &serde_json::Value) -> Result<(), String> {
        let mut writer = self
            .inner
            .lock()
            .map_err(|_| "MCP stdout lock poisoned".to_string())?;
        writeln!(writer, "{message}").map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())
    }
}

#[cfg(test)]
impl JsonWriter<Vec<u8>> {
    pub(super) fn bytes(&self) -> Vec<u8> {
        self.inner.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    tools: serde_json::Value,
    skills: serde_json::Value,
}

impl Snapshot {
    fn load(client: &mut DaemonClient) -> Result<Self, String> {
        // Refresh development plugins too; install/remove already refresh the pool.
        client.call("plugin.list", serde_json::json!({}))?;
        Ok(Self {
            tools: client.call("cmd.tools", serde_json::json!({"include_annotations":true}))?,
            skills: client.call("skill.list", serde_json::json!({}))?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Changes {
    tools: bool,
    resources: bool,
    prompts: bool,
}

fn changes(previous: &Snapshot, current: &Snapshot) -> Changes {
    Changes {
        tools: previous.tools != current.tools,
        resources: previous.skills != current.skills,
        prompts: previous.skills != current.skills,
    }
}

pub(super) struct MonitorSeed {
    cursor: u64,
    snapshot: Snapshot,
}

impl MonitorSeed {
    pub(super) fn load(client: &mut DaemonClient) -> Result<Self, String> {
        // Snapshot first, then cursor. A change in between is caught by the
        // monitor's immediate refresh; a later change remains after cursor.
        let snapshot = Snapshot::load(client)?;
        Ok(Self {
            cursor: latest_cursor(client)?,
            snapshot,
        })
    }
}

fn latest_cursor(client: &mut DaemonClient) -> Result<u64, String> {
    let events = client.call("audit.tail", serde_json::json!({"limit":1}))?;
    Ok(events
        .as_array()
        .and_then(|items| items.first())
        .and_then(|event| event.get("id"))
        .and_then(|id| id.as_u64())
        .unwrap_or(0))
}

fn relevant_event(event: &serde_json::Value) -> bool {
    let action = event.get("action").and_then(|value| value.as_str());
    let status = event
        .pointer("/detail/status")
        .and_then(|value| value.as_str());
    status == Some("ok")
        && matches!(
            action,
            Some(
                "plugin.reload"
                    | "plugin.install"
                    | "plugin.market.install"
                    | "plugin.market.update"
                    | "plugin.approve"
                    | "plugin.revoke"
                    | "plugin.remove"
            )
        )
}

fn send_notifications<W: Write>(output: &JsonWriter<W>, changed: Changes) -> Result<(), String> {
    if changed.tools {
        output.send(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"notifications/tools/list_changed"
        }))?;
    }
    if changed.resources {
        output.send(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"notifications/resources/list_changed"
        }))?;
    }
    if changed.prompts {
        output.send(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"notifications/prompts/list_changed"
        }))?;
    }
    Ok(())
}

fn send_resource_updated<W: Write>(
    output: &JsonWriter<W>,
    subscriptions: &Arc<Mutex<HashSet<String>>>,
    has_events: bool,
) -> Result<(), String> {
    if !has_events {
        return Ok(());
    }
    let subscribed = subscriptions
        .lock()
        .map_err(|_| "resource subscription lock poisoned".to_string())?
        .contains(EVENTS_RESOURCE_URI);
    if subscribed {
        output.send(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"notifications/resources/updated",
            "params":{"uri":EVENTS_RESOURCE_URI}
        }))?;
    }
    Ok(())
}

pub(super) fn start<W: Write + Send + 'static>(
    output: JsonWriter<W>,
    initialized: Arc<AtomicBool>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    seed: Option<MonitorSeed>,
) {
    std::thread::spawn(move || monitor(output, initialized, subscriptions, seed));
}

fn monitor<W: Write>(
    output: JsonWriter<W>,
    initialized: Arc<AtomicBool>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    seed: Option<MonitorSeed>,
) {
    while !initialized.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(10));
    }

    let (mut cursor, mut previous) = match seed {
        Some(seed) => (Some(seed.cursor), Some(seed.snapshot)),
        None => (None, None),
    };
    loop {
        let mut client = match DaemonClient::connect() {
            Ok(client) => client,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        if cursor.is_none() {
            cursor = latest_cursor(&mut client).ok();
        }
        if previous.is_none() {
            previous = Snapshot::load(&mut client).ok();
        }
        if let (Some(old), Ok(current)) = (previous.as_ref(), Snapshot::load(&mut client)) {
            let changed = changes(old, &current);
            previous = Some(current);
            if send_notifications(&output, changed).is_err() {
                return;
            }
        }
        let Some(mut after) = cursor else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };

        while let Ok(events) = client.call(
            "events.tail",
            serde_json::json!({"after":after,"limit":200,"wait_ms":POLL_MS}),
        ) {
            after = events
                .get("cursor")
                .and_then(|value| value.as_u64())
                .unwrap_or(after);
            cursor = Some(after);
            let has_events = events
                .get("events")
                .and_then(|value| value.as_array())
                .is_some_and(|items| !items.is_empty());
            if send_resource_updated(&output, &subscriptions, has_events).is_err() {
                return;
            }
            let relevant = events
                .get("events")
                .and_then(|value| value.as_array())
                .is_some_and(|items| items.iter().any(relevant_event));
            let timed_out = events
                .get("timed_out")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !relevant && !timed_out {
                continue;
            }
            let Ok(current) = Snapshot::load(&mut client) else {
                break;
            };
            if let Some(old) = previous.replace(current.clone()) {
                if send_notifications(&output, changes(&old, &current)).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_tools_and_skills_independently() {
        let baseline = Snapshot {
            tools: serde_json::json!([{"name":"search"}]),
            skills: serde_json::json!([]),
        };
        let tools = Snapshot {
            tools: serde_json::json!([{"name":"search"},{"name":"plugin_tool"}]),
            skills: serde_json::json!([]),
        };
        assert_eq!(
            changes(&baseline, &tools),
            Changes {
                tools: true,
                resources: false,
                prompts: false,
            }
        );
        let skills = Snapshot {
            tools: tools.tools.clone(),
            skills: serde_json::json!([{"plugin":"demo","id":"workflow"}]),
        };
        assert_eq!(
            changes(&tools, &skills),
            Changes {
                tools: false,
                resources: true,
                prompts: true,
            }
        );
    }

    #[test]
    fn filters_failed_and_unrelated_events() {
        assert!(relevant_event(&serde_json::json!({
            "action":"plugin.approve", "detail":{"status":"ok"}
        })));
        assert!(!relevant_event(&serde_json::json!({
            "action":"plugin.approve", "detail":{"status":"error"}
        })));
        assert!(!relevant_event(&serde_json::json!({
            "action":"todo.add", "detail":{"status":"ok"}
        })));
    }

    #[test]
    fn emits_standard_notifications_without_ids() {
        let output = JsonWriter::new(Vec::<u8>::new());
        send_notifications(
            &output,
            Changes {
                tools: true,
                resources: true,
                prompts: true,
            },
        )
        .unwrap();
        let bytes = output.bytes();
        let messages: Vec<serde_json::Value> = String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages[0]["method"], "notifications/tools/list_changed");
        assert_eq!(
            messages[1]["method"],
            "notifications/resources/list_changed"
        );
        assert_eq!(messages[2]["method"], "notifications/prompts/list_changed");
        assert!(messages.iter().all(|message| message.get("id").is_none()));
    }

    #[test]
    fn emits_event_updates_only_for_subscribed_sessions() {
        let subscriptions = Arc::new(Mutex::new(HashSet::new()));
        let unsubscribed = JsonWriter::new(Vec::<u8>::new());
        send_resource_updated(&unsubscribed, &subscriptions, true).unwrap();
        assert!(unsubscribed.bytes().is_empty());

        subscriptions.lock().unwrap().insert(EVENTS_RESOURCE_URI.into());
        let subscribed = JsonWriter::new(Vec::<u8>::new());
        send_resource_updated(&subscribed, &subscriptions, false).unwrap();
        assert!(subscribed.bytes().is_empty());
        send_resource_updated(&subscribed, &subscriptions, true).unwrap();
        let message: serde_json::Value = serde_json::from_slice(
            subscribed.bytes().strip_suffix(b"\n").unwrap()
        ).unwrap();
        assert_eq!(message["method"], "notifications/resources/updated");
        assert_eq!(message["params"]["uri"], EVENTS_RESOURCE_URI);
    }
}
