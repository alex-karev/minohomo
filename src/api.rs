use crate::storage::{self, Settings, key};
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use reqwest::{Method, blocking::Client};
use serde_json::{Value, json};
use std::{
    io::Read,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use url::Url;

const DELAY_TIMEOUT_MS: u64 = 5000;
const DELAY_URL: &str = "https://www.gstatic.com/generate_204";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Mode {
    Rule,
    Direct,
    Global,
}
impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Direct => "direct",
            Self::Global => "global",
        }
    }
}

pub struct Controller {
    client: Client,
    base: Url,
    secret: String,
}
impl Controller {
    pub fn new(settings: &Settings) -> Result<Self> {
        // Prefer the last generated controller settings over unapplied edits.
        let mihomo = if settings.output.exists() {
            storage::read_yaml::<serde_yaml::Mapping>(&settings.output)?
        } else {
            settings.mihomo.clone()
        };
        let address = mihomo
            .get(key("external-controller"))
            .and_then(serde_yaml::Value::as_str)
            .filter(|s| !s.is_empty())
            .context("external-controller is not configured")?;
        let mut base = Url::parse(&if address.contains("://") {
            address.to_owned()
        } else {
            format!("http://{address}")
        })
        .context("invalid external-controller address")?;
        ensure!(
            ["http", "https"].contains(&base.scheme()) && base.host_str().is_some(),
            "external-controller must use HTTP(S)"
        );
        ensure!(
            base.username().is_empty() && base.password().is_none(),
            "external-controller must not contain credentials"
        );
        if matches!(base.host_str(), Some("0.0.0.0" | "[::]" | "::")) {
            base.set_host(Some("127.0.0.1"))?;
        }
        base.set_path("/");
        base.set_query(None);
        base.set_fragment(None);
        let secret = mihomo
            .get(key("secret"))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(settings.request_timeout_seconds.max(6)))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            base,
            secret,
        })
    }
    fn endpoint(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid controller base URL"))?
            .clear()
            .extend(segments.iter().copied());
        Ok(url)
    }
    fn request(&self, method: Method, url: Url, body: Option<Value>) -> Result<Value> {
        let mut request = self.client.request(method, url);
        if !self.secret.is_empty() {
            request = request.bearer_auth(&self.secret);
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&body)?);
        }
        let response = request
            .send()
            .map_err(|e| e.without_url())
            .context("Mihomo API request failed; is the service running?")?;
        ensure!(
            response.status().is_success(),
            "Mihomo API returned HTTP {}",
            response.status()
        );
        let mut bytes = Vec::new();
        response
            .take(crate::MAX_BODY + 1)
            .read_to_end(&mut bytes)
            .context("cannot read Mihomo API response")?;
        ensure!(
            bytes.len() as u64 <= crate::MAX_BODY,
            "Mihomo API response exceeds 16 MiB"
        );
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&bytes).context("Mihomo API returned invalid JSON")
        }
    }
    fn proxies(&self) -> Result<Value> {
        self.request(Method::GET, self.endpoint(&["proxies"])?, None)
    }
    pub fn info(&self) -> String {
        format!(
            "{}\nsecret: {}\n",
            self.base.as_str().trim_end_matches('/'),
            self.secret
        )
    }
    pub fn current_mode(&self) -> Result<String> {
        let config = self.request(Method::GET, self.endpoint(&["configs"])?, None)?;
        config["mode"]
            .as_str()
            .map(str::to_owned)
            .context("Mihomo API response has no mode")
    }
    pub fn mode(&self, mode: Mode) -> Result<()> {
        self.request(
            Method::PATCH,
            self.endpoint(&["configs"])?,
            Some(json!({"mode":mode.as_str()})),
        )?;
        println!("Mihomo mode: {}", mode.as_str());
        Ok(())
    }
    pub fn selector_group(&self, group: &str) -> Result<Value> {
        self.request(Method::GET, self.endpoint(&["proxies", group])?, None)
    }
    fn selector(&self) -> Result<Value> {
        self.selector_group("PROXY")
    }
    pub fn for_tray(settings: &Settings) -> Result<Self> {
        let mut controller = Self::new(settings)?;
        controller.client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(controller)
    }
    pub fn selection(&self) -> Result<()> {
        let selector = self.selector()?;
        println!("PROXY: {}", selector["now"].as_str().unwrap_or("unknown"));
        if let Some(names) = selector["all"].as_array() {
            for name in names.iter().filter_map(Value::as_str) {
                println!("   {name}");
            }
        }
        Ok(())
    }
    pub fn switch(&self, name: &str) -> Result<()> {
        self.switch_group("PROXY", name)
    }
    pub fn switch_group(&self, group: &str, name: &str) -> Result<()> {
        let selector = self.selector_group(group)?;
        ensure!(
            selector["all"]
                .as_array()
                .is_some_and(|names| names.iter().any(|n| n.as_str() == Some(name))),
            "'{name}' is not available in the running {group} group; apply the config with update --no-sync first"
        );
        self.request(
            Method::PUT,
            self.endpoint(&["proxies", group])?,
            Some(json!({"name":name})),
        )?;
        println!("{group}: {name}");
        Ok(())
    }
    fn delay(&self, name: &str) -> Result<u64> {
        let mut url = self.endpoint(&["proxies", name, "delay"])?;
        url.query_pairs_mut()
            .append_pair("timeout", &DELAY_TIMEOUT_MS.to_string())
            .append_pair("url", DELAY_URL);
        let result = self.request(Method::GET, url, None)?;
        result["delay"]
            .as_u64()
            .filter(|delay| *delay > 0)
            .context("delay check failed")
    }
    fn delays(&self) -> Result<Vec<(String, Option<u64>)>> {
        let proxies = self.proxies()?;
        let names: Vec<_> = proxies["proxies"]
            .as_object()
            .context("Mihomo API response has no proxies map")?
            .keys()
            .cloned()
            .collect();
        let next = AtomicUsize::new(0);
        let mut results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..names.len().min(8))
                .map(|_| {
                    scope.spawn(|| {
                        let mut results = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(name) = names.get(index) else {
                                break;
                            };
                            results.push((name.clone(), self.delay(name).ok()));
                        }
                        results
                    })
                })
                .collect();
            let mut results = Vec::new();
            for worker in workers {
                results.extend(
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("delay worker failed"))?,
                );
            }
            Ok::<_, anyhow::Error>(results)
        })?;
        results.sort_by(|a, b| {
            a.1.unwrap_or(u64::MAX)
                .cmp(&b.1.unwrap_or(u64::MAX))
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(results)
    }
    pub fn ping(&self) -> Result<()> {
        for (name, delay) in self.delays()? {
            match delay {
                Some(delay) => println!("{delay:>6} ms  {name}"),
                None => println!("   failed  {name}"),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, thread::JoinHandle};

    fn mock(
        count: usize,
        handler: impl Fn(&str, &str) -> (u16, Value) + Send + 'static,
    ) -> (Controller, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..count {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let header_end;
                loop {
                    let mut buffer = [0; 1024];
                    let read = socket.read(&mut buffer).unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = end + 4;
                        break;
                    }
                }
                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                assert!(
                    headers
                        .to_lowercase()
                        .contains("authorization: bearer test-secret\r\n")
                );
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut buffer = [0; 1024];
                    let read = socket.read(&mut buffer).unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let body = std::str::from_utf8(&bytes[header_end..header_end + length]).unwrap();
                let (status, response) = handler(headers.lines().next().unwrap(), body);
                let body = if status == 204 {
                    String::new()
                } else {
                    response.to_string()
                };
                write!(socket, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let controller = Controller {
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            base: Url::parse(&format!("http://{address}/")).unwrap(),
            secret: "test-secret".into(),
        };
        (controller, worker)
    }

    #[test]
    fn mode_and_switch_use_authenticated_api_requests() {
        let (controller, worker) = mock(3, |request, body| match request {
            "PATCH /configs HTTP/1.1" => {
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"mode":"direct"})
                );
                (204, Value::Null)
            }
            "GET /proxies/PROXY HTTP/1.1" => {
                (200, json!({"now":"Other", "all":["Sub: Node / One"]}))
            }
            "PUT /proxies/PROXY HTTP/1.1" => {
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"name":"Sub: Node / One"})
                );
                (204, Value::Null)
            }
            _ => panic!("unexpected request: {request}"),
        });
        controller.mode(Mode::Direct).unwrap();
        controller.switch("Sub: Node / One").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn global_mode_and_selector_use_authenticated_api() {
        let (controller, worker) = mock(3, |request, body| match request {
            "PATCH /configs HTTP/1.1" => {
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"mode":"global"})
                );
                (204, Value::Null)
            }
            "GET /proxies/GLOBAL HTTP/1.1" => {
                (200, json!({"now":"DIRECT", "all":["DIRECT", "Node / One"]}))
            }
            "PUT /proxies/GLOBAL HTTP/1.1" => {
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap(),
                    json!({"name":"Node / One"})
                );
                (204, Value::Null)
            }
            _ => panic!("unexpected request: {request}"),
        });
        controller.mode(Mode::Global).unwrap();
        controller.switch_group("GLOBAL", "Node / One").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn delays_are_sorted_use_api_names_and_put_failures_last() {
        let (controller, worker) = mock(5, |request, _| {
            let target = request.split_whitespace().nth(1).unwrap();
            let url = Url::parse(&format!("http://localhost{target}")).unwrap();
            if url.path() == "/proxies" {
                return (
                    200,
                    json!({"proxies":{"Fast":{},"Group":{},"Slow/name x":{},"Broken":{}}}),
                );
            }
            let path = percent_encoding::percent_decode_str(url.path())
                .decode_utf8()
                .unwrap();
            assert!(
                url.query_pairs()
                    .any(|(k, v)| k == "timeout" && v == "5000")
            );
            assert!(url.query_pairs().any(|(k, v)| k == "url" && v == DELAY_URL));
            match path.as_ref() {
                "/proxies/Fast/delay" => (200, json!({"delay":10})),
                "/proxies/Group/delay" => (200, json!({"delay":20})),
                "/proxies/Slow/name x/delay" => (200, json!({"delay":100})),
                "/proxies/Broken/delay" => (504, json!({"message":"timeout"})),
                _ => panic!("unexpected request: {request}"),
            }
        });
        assert_eq!(
            controller.delays().unwrap(),
            vec![
                ("Fast".into(), Some(10)),
                ("Group".into(), Some(20)),
                ("Slow/name x".into(), Some(100)),
                ("Broken".into(), None)
            ]
        );
        worker.join().unwrap();
    }

    #[test]
    fn current_mode_queries_running_config_without_changing_it() {
        let (controller, worker) = mock(1, |request, body| {
            assert_eq!(request, "GET /configs HTTP/1.1");
            assert!(body.is_empty());
            (200, json!({"mode":"global"}))
        });
        assert_eq!(controller.current_mode().unwrap(), "global");
        worker.join().unwrap();
    }
    #[test]
    fn controller_info_is_two_plain_lines_without_network_requests() {
        let controller = Controller {
            client: Client::builder().no_proxy().build().unwrap(),
            base: Url::parse("http://localhost:9090/").unwrap(),
            secret: "example-secret".into(),
        };
        assert_eq!(
            controller.info(),
            "http://localhost:9090\nsecret: example-secret\n"
        );
    }
    #[test]
    fn auth_errors_are_reported_without_secret() {
        let (controller, worker) = mock(1, |_, _| (401, json!({"message":"Unauthorized"})));
        let error = controller.mode(Mode::Rule).unwrap_err().to_string();
        assert!(error.contains("401"));
        assert!(!error.contains("test-secret"));
        worker.join().unwrap();
    }

    #[test]
    fn generated_controller_settings_override_unapplied_edits() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        settings.output = temp.path().join("current.yaml");
        std::fs::write(
            &settings.output,
            "external-controller: 0.0.0.0:12345\nsecret: saved-secret\n",
        )
        .unwrap();
        let controller = Controller::new(&settings).unwrap();
        assert_eq!(controller.base.as_str(), "http://127.0.0.1:12345/");
        assert_eq!(controller.secret, "saved-secret");
    }
}
