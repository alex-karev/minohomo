use crate::{
    api::{Controller, Mode},
    storage::{self, State, Store},
};
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

pub(super) enum Job {
    Refresh,
    Ping,
    Ip,
    Mode(Mode),
    Proxy { group: String, name: String },
}
pub(super) enum Message {
    Status(Result<Status, String>),
    Connections(Result<Vec<Value>, String>, Instant),
    Ping(Result<Vec<(String, Option<u64>)>, String>),
    Ip(Result<String, String>),
    Action(Result<String, String>),
}
#[derive(Default)]
pub(super) struct Status {
    pub sources: Vec<storage::Source>,
    pub tray_enabled: bool,
    pub service: String,
    pub core: String,
    pub mode: String,
    pub group: String,
    pub current: String,
    pub choices: Vec<String>,
    pub geosite: String,
    pub api_error: Option<String>,
}

fn status(root: &Path) -> Result<Status> {
    let settings = Store::load_settings(root)?;
    let state: State = storage::read_yaml(&root.join("sources.yaml"))?;
    let database = settings
        .output
        .parent()
        .context("output has no directory")?
        .join("GeoSite.dat");
    let geosite = match std::fs::metadata(database) {
        Ok(meta) => format!("{} KiB", meta.len() / 1024),
        Err(_) => "not downloaded".into(),
    };
    let mut status = Status {
        sources: state.sources,
        tray_enabled: settings.tray_enabled,
        service: crate::tray::service_state(),
        core: crate::core::version().unwrap_or_else(|_| "unavailable".into()),
        geosite,
        ..Default::default()
    };
    let result = (|| -> Result<()> {
        let controller = Controller::for_tray(&settings)?;
        status.mode = controller.current_mode()?;
        status.group = if status.mode.eq_ignore_ascii_case("global") {
            "GLOBAL"
        } else {
            "PROXY"
        }
        .into();
        let selector = controller.selector_group(&status.group)?;
        status.current = selector["now"].as_str().unwrap_or("unknown").into();
        status.choices = selector["all"]
            .as_array()
            .context("proxy group has no choices")?
            .iter()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect();
        Ok(())
    })();
    status.api_error = result.err().map(|error| format!("{error:#}"));
    Ok(status)
}
fn result<T>(value: Result<T>) -> Result<T, String> {
    value.map_err(|error| format!("{error:#}"))
}

pub(super) fn spawn(root: PathBuf) -> (Sender<Job>, Receiver<Message>) {
    let (jobs, rx) = mpsc::channel();
    // Bound queued snapshots while foreground sudo/editor handoffs pause rendering.
    let (tx, messages) = mpsc::sync_channel(4);
    // Independent polling: a slow ping/IP lookup must not stall connection updates.
    let connection_tx = tx.clone();
    let connection_root = root.clone();
    std::thread::spawn(move || {
        loop {
            let snapshot = result((|| {
                Controller::for_tray(&Store::load_settings(&connection_root)?)?.connections()
            })());
            if connection_tx
                .send(Message::Connections(snapshot, Instant::now()))
                .is_err()
            {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    std::thread::spawn(move || {
        loop {
            if tx.send(Message::Status(result(status(&root)))).is_err() {
                return;
            }
            let job = match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(job) => job,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let message = match job {
                Job::Refresh => continue,
                Job::Ping => Message::Ping(result((|| {
                    Controller::new(&Store::load_settings(&root)?)?.delays()
                })())),
                Job::Ip => Message::Ip(result(public_ip())),
                Job::Mode(mode) => Message::Action(result((|| {
                    Controller::for_tray(&Store::load_settings(&root)?)?.set_mode(mode)?;
                    Ok("Mode changed. Runtime only; edit config.yaml to persist it.".into())
                })())),
                Job::Proxy { group, name } => Message::Action(result((|| {
                    Controller::for_tray(&Store::load_settings(&root)?)?
                        .set_selection(&group, &name)?;
                    Ok(format!("{group}: {name}"))
                })())),
            };
            if tx.send(message).is_err() {
                return;
            }
        }
    });
    (jobs, messages)
}

fn public_ip() -> Result<String> {
    // Deliberately on demand. Follow the user's outbound routing and proxy env,
    // unlike controller requests, which must bypass HTTP proxies.
    let response = Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?
        .get("https://ipwho.is/")
        .send()
        .map_err(|error| error.without_url())?
        .error_for_status()
        .map_err(|error| error.without_url())?;
    let mut bytes = Vec::new();
    response.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "IP lookup response exceeds 64 KiB");
    parse_ip(&serde_json::from_slice(&bytes)?)
}
fn parse_ip(value: &Value) -> Result<String> {
    ensure!(
        value["success"].as_bool() == Some(true),
        "IP lookup failed: {}",
        value["message"]
            .as_str()
            .unwrap_or("provider returned no result")
    );
    let ip = value["ip"].as_str().context("IP lookup returned no IP")?;
    ip.parse::<std::net::IpAddr>()
        .context("IP lookup returned an invalid IP")?;
    let location = ["city", "region", "country"]
        .iter()
        .filter_map(|key| value[*key].as_str())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "IP: {ip}\nLocation: {location}\nISP: {}\nSource: ipwho.is | {}\nObserved outbound route, not a guarantee of Mihomo routing.",
        value["connection"]["isp"].as_str().unwrap_or("unknown"),
        chrono::Local::now().format("%H:%M:%S")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn parses_ip_location_and_provider_failures() {
        let text = parse_ip(&json!({"success":true,"ip":"203.0.113.1", "city":"Example", "country":"Test", "connection":{"isp":"ISP"}})).unwrap();
        assert!(text.contains("203.0.113.1"));
        assert!(text.contains("Example, Test"));
        assert!(
            parse_ip(&json!({"success":false,"message":"rate limit"}))
                .unwrap_err()
                .to_string()
                .contains("rate limit")
        );
        assert!(parse_ip(&json!({"success":true,"ip":"bad"})).is_err());
    }
    #[test]
    fn status_uses_live_selector_without_holding_source_lock() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let config = dir.path().join("config.yaml");
        let body = std::fs::read_to_string(&config)
            .unwrap()
            .replace("127.0.0.1:9090", &address.to_string());
        std::fs::write(config, body).unwrap();
        let expected_secret = Store::load_settings(dir.path()).unwrap().mihomo
            [crate::storage::key("secret")]
        .as_str()
        .unwrap()
        .to_owned();
        let server = std::thread::spawn(move || {
            for target in ["/configs", "/proxies/GLOBAL"] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).unwrap(), 1);
                    bytes.extend(byte);
                }
                let request = String::from_utf8(bytes).unwrap();
                assert!(request.starts_with(&format!("GET {target} HTTP/1.1\r\n")));
                assert!(
                    request
                        .to_lowercase()
                        .contains(&format!("authorization: bearer {expected_secret}"))
                );
                let body = if target == "/configs" {
                    r#"{"mode":"global"}"#
                } else {
                    r#"{"now":"Node / One","all":["DIRECT","Node / One"]}"#
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        // Holding Store here proves the dashboard reader does not need its lock.
        let dashboard = status(dir.path()).unwrap();
        assert_eq!(dashboard.mode, "global");
        assert_eq!(dashboard.current, "Node / One");
        assert_eq!(dashboard.group, "GLOBAL");
        assert_eq!(dashboard.choices, vec!["DIRECT", "Node / One"]);
        assert!(dashboard.api_error.is_none());
        assert!(store.settings().is_ok());
        server.join().unwrap();
    }
}
