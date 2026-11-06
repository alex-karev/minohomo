use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

const CLOSED_LIMIT: usize = 1000;

#[derive(Clone)]
pub(super) struct Connection {
    pub id: String,
    pub data: Value,
    pub open: bool,
    pub closed_at: Option<String>,
    pub download_speed: f64,
    pub upload_speed: f64,
    sampled: Instant,
}
impl Connection {
    pub fn text(&self, field: &str) -> String {
        display(&self.data[field])
    }
    pub fn meta(&self, field: &str) -> String {
        display(&self.data["metadata"][field])
    }
    pub fn bytes(&self, field: &str) -> u64 {
        self.data[field].as_u64().unwrap_or(0)
    }
    pub fn destination(&self) -> String {
        let host = self.data["metadata"]["host"]
            .as_str()
            .filter(|s| !s.is_empty());
        endpoint(
            host.unwrap_or_else(|| {
                self.data["metadata"]["destinationIP"]
                    .as_str()
                    .unwrap_or("-")
            }),
            &self.meta("destinationPort"),
        )
    }
    pub fn chains(&self) -> String {
        self.data["chains"]
            .as_array()
            .map(|chains| chains.iter().map(display).collect::<Vec<_>>().join(" → "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".into())
    }
    pub fn details(&self) -> String {
        let mut lines = vec![
            "Connection details".into(),
            format!("Status: {}", if self.open { "Open" } else { "Closed" }),
            format!("ID: {}", self.id),
            format!("Start: {}", self.text("start")),
            format!(
                "Closed observed: {}",
                self.closed_at.as_deref().unwrap_or("-")
            ),
            format!("Rule: {}", self.text("rule")),
            format!("Rule payload: {}", self.text("rulePayload")),
            format!("Chains: {}", self.chains()),
            String::new(),
            "Traffic (last observed)".into(),
            format!(
                "Download: {} ({} B)",
                traffic(self.bytes("download") as f64),
                self.bytes("download")
            ),
            format!(
                "Upload: {} ({} B)",
                traffic(self.bytes("upload") as f64),
                self.bytes("upload")
            ),
            format!("DL speed: {}/s", traffic(self.download_speed)),
            format!("UL speed: {}/s", traffic(self.upload_speed)),
            String::new(),
            "Metadata".into(),
        ];
        for (label, field) in [
            ("Network", "network"),
            ("Type", "type"),
            ("Host", "host"),
            ("Sniff host", "sniffHost"),
            ("DNS mode", "dnsMode"),
        ] {
            lines.push(format!("{label}: {}", self.meta(field)));
        }
        lines.extend([
            String::new(),
            "Source & destination".into(),
            format!(
                "Source: {}",
                endpoint(&self.meta("sourceIP"), &self.meta("sourcePort"))
            ),
            format!("Destination: {}", self.destination()),
            format!("Destination IP: {}", self.meta("destinationIP")),
            format!("Remote destination: {}", self.text("remoteDestination")),
            format!("Inbound name: {}", self.meta("inboundName")),
            format!(
                "Inbound address: {}",
                endpoint(&self.meta("inboundIP"), &self.meta("inboundPort"))
            ),
            format!("Inbound user: {}", self.meta("inboundUser")),
            String::new(),
            "Process".into(),
            format!("Process name: {}", self.meta("process")),
            format!("Process path: {}", self.meta("processPath")),
            format!("UID: {}", self.meta("uid")),
        ]);
        lines.join("\n")
    }
}
fn display(value: &Value) -> String {
    match value {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => "-".into(),
    }
}
fn endpoint(host: &str, port: &str) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
pub(super) fn traffic(mut bytes: f64) -> String {
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if bytes < 1024.0 || unit == "TiB" {
            return if unit == "B" {
                format!("{bytes:.0} {unit}")
            } else {
                format!("{bytes:.1} {unit}")
            };
        }
        bytes /= 1024.0;
    }
    unreachable!()
}

#[derive(Default)]
pub(super) struct History {
    pub entries: Vec<Connection>,
    pub error: Option<String>,
    pub updated: Option<String>,
}
impl History {
    pub fn update(&mut self, snapshot: Vec<Value>, now: Instant) {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut positions: HashMap<String, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.clone(), i))
            .collect();
        let mut seen = HashSet::new();
        let had_error = self.error.is_some();
        for data in snapshot {
            let Some(id) = data["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(&index) = positions.get(&id) {
                let entry = &mut self.entries[index];
                let elapsed = now.duration_since(entry.sampled).as_secs_f64();
                if entry.open && !had_error && elapsed > 0.0 {
                    entry.download_speed = data["download"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_sub(entry.bytes("download"))
                        as f64
                        / elapsed;
                    entry.upload_speed = data["upload"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_sub(entry.bytes("upload"))
                        as f64
                        / elapsed;
                } else {
                    entry.download_speed = 0.0;
                    entry.upload_speed = 0.0;
                }
                entry.data = data;
                entry.open = true;
                entry.closed_at = None;
                entry.sampled = now;
            } else {
                positions.insert(id.clone(), self.entries.len());
                self.entries.push(Connection {
                    id,
                    data,
                    open: true,
                    closed_at: None,
                    download_speed: 0.0,
                    upload_speed: 0.0,
                    sampled: now,
                });
            }
        }
        for entry in &mut self.entries {
            if entry.open && !seen.contains(&entry.id) {
                entry.open = false;
                entry.closed_at = Some(timestamp.clone());
                entry.download_speed = 0.0;
                entry.upload_speed = 0.0;
            }
        }
        // Stable order between snapshots. Newest start first, with UUID as tie-breaker.
        self.entries.sort_by(|a, b| {
            b.data["start"]
                .as_str()
                .cmp(&a.data["start"].as_str())
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut closed = 0;
        self.entries.retain(|entry| {
            if entry.open {
                true
            } else {
                closed += 1;
                closed <= CLOSED_LIMIT
            }
        });
        self.updated = Some(timestamp);
        self.error = None;
    }
}

pub(super) fn copy(text: &str) -> std::io::Result<()> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::io::Write;
    // OSC 52 keeps Wayland/X11 dependencies out of the TUI. Only base64 reaches the terminal.
    let mut stdout = std::io::stdout().lock();
    write!(stdout, "\x1b]52;c;{}\x07", STANDARD.encode(text))?;
    stdout.flush()
}
