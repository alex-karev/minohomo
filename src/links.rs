use anyhow::{Context, Result, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use url::Url;

pub type Proxy = Map<String, Value>;

pub fn decode64(input: &str) -> Result<Vec<u8>> {
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(&compact) {
            return Ok(bytes);
        }
    }
    bail!("invalid base64 payload")
}
fn text64(s: &str) -> Result<String> {
    Ok(String::from_utf8(decode64(s)?)?)
}
fn decoded(s: &str) -> Result<String> {
    Ok(percent_decode_str(s).decode_utf8()?.into_owned())
}
fn put(p: &mut Proxy, key: &str, val: impl Into<Value>) {
    p.insert(key.into(), val.into());
}
fn truth(s: &str) -> Result<bool> {
    match s {
        "1" | "true" => Ok(true),
        "0" | "false" | "" => Ok(false),
        _ => bail!("expected boolean, found {s}"),
    }
}

fn endpoint(url: &Url, kind: &str) -> Result<Proxy> {
    let host = url
        .host_str()
        .context("proxy link has no server")?
        .trim_matches(['[', ']']);
    let port = url
        .port()
        .or_else(|| match url.scheme() {
            "https" => Some(443),
            "http" => Some(80),
            "hy2" | "hysteria2" | "trojan" => Some(443),
            _ => None,
        })
        .context("proxy link requires an explicit port")?;
    ensure!(port > 0, "proxy port cannot be zero");
    let name = url
        .fragment()
        .map(decoded)
        .transpose()?
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{kind}-{host}:{port}"));
    Ok(
        json!({"name":name, "type":kind, "server":host, "port":port})
            .as_object()
            .unwrap()
            .clone(),
    )
}

fn transport(p: &mut Proxy, q: &HashMap<String, String>, url: &Url) -> Result<()> {
    let network = q
        .get("type")
        .or_else(|| q.get("network"))
        .map(String::as_str)
        .unwrap_or("tcp");
    let path = q.get("path").cloned().unwrap_or_else(|| "/".into());
    match network {
        "tcp" | "raw" | "" => {}
        "ws" => {
            put(p, "network", "ws");
            let mut opts = json!({"path":path}).as_object().unwrap().clone();
            if let Some(host) = q.get("host") {
                opts.insert("headers".into(), json!({"Host":host}));
            }
            put(p, "ws-opts", opts);
        }
        "grpc" => {
            put(p, "network", "grpc");
            put(
                p,
                "grpc-opts",
                json!({"grpc-service-name":q.get("serviceName").or_else(||q.get("path")).cloned().unwrap_or_default()}),
            );
        }
        "http" | "h2" => {
            put(p, "network", network);
            let mut opts =
                json!({"path": if network == "h2" { json!(path) } else { json!([path]) }});
            if let Some(host) = q.get("host") {
                if network == "h2" {
                    opts["host"] = json!([host]);
                } else {
                    opts["headers"] = json!({"Host":[host]});
                }
            }
            put(
                p,
                if network == "h2" {
                    "h2-opts"
                } else {
                    "http-opts"
                },
                opts,
            );
        }
        _ => bail!(
            "unsupported transport '{network}'; import a Clash YAML subscription for this node"
        ),
    }
    let security = q.get("security").map(String::as_str).unwrap_or("");
    ensure!(
        ["", "none", "tls", "reality"].contains(&security),
        "unsupported link security '{security}'"
    );
    if security == "tls" || security == "reality" {
        put(p, "tls", true);
    }
    if let Some(sni) = q
        .get("sni")
        .or_else(|| q.get("peer"))
        .or_else(|| q.get("servername"))
    {
        put(
            p,
            if url.scheme() == "trojan" {
                "sni"
            } else {
                "servername"
            },
            sni.clone(),
        );
    }
    if let Some(fp) = q.get("fp") {
        put(p, "client-fingerprint", fp.clone());
    }
    if let Some(alpn) = q.get("alpn") {
        put(p, "alpn", json!(alpn.split(',').collect::<Vec<_>>()));
    }
    if let Some(insecure) = q.get("allowInsecure").or_else(|| q.get("insecure")) {
        put(p, "skip-cert-verify", truth(insecure)?);
    }
    if security == "reality" {
        let key = q.get("pbk").context("Reality link is missing pbk")?;
        let mut reality =
            json!({"public-key":key,"short-id":q.get("sid").cloned().unwrap_or_default()})
                .as_object()
                .unwrap()
                .clone();
        if let Some(support) = q.get("support-x25519mlkem768") {
            put(
                &mut reality,
                "support-x25519mlkem768",
                truth(support).context("invalid support-x25519mlkem768 value")?,
            );
        }
        // Mihomo's REALITY adapter has no SpiderX option. Xray's spx is not
        // needed for an authenticated tunnel; do not invent a Mihomo YAML key.
        put(p, "reality-opts", reality);
    }
    // Refuse features which would otherwise be silently lost during conversion.
    for key in ["ech", "echConfig", "packetEncoding"] {
        if q.get(key).is_some_and(|v| !v.is_empty()) {
            bail!("unsupported link parameter '{key}'; use Clash YAML");
        }
    }
    if q.get("headerType")
        .is_some_and(|v| v != "none" && !v.is_empty())
    {
        bail!("TCP header obfuscation is unsupported; use Clash YAML")
    }
    Ok(())
}

fn ss(link: &str) -> Result<Proxy> {
    let body = link.strip_prefix("ss://").unwrap();
    // Legacy SS links encode the entire method:password@host:port endpoint.
    let (before_fragment, fragment) = body.split_once('#').unwrap_or((body, ""));
    let expanded;
    let url = if !before_fragment.contains('@') {
        expanded = format!(
            "ss://{}#{}",
            text64(before_fragment.trim_end_matches('/'))?,
            fragment
        );
        Url::parse(&expanded)?
    } else {
        Url::parse(link)?
    };
    let mut p = endpoint(&url, "ss")?;
    let (cipher, password) = if let Some(password) = url.password() {
        (decoded(url.username())?, decoded(password)?)
    } else {
        let credentials = text64(&decoded(url.username())?)?;
        let (method, password) = credentials
            .split_once(':')
            .context("SS credentials require method:password")?;
        (method.into(), password.into())
    };
    put(&mut p, "cipher", cipher);
    put(&mut p, "password", password);
    put(&mut p, "udp", true);
    if let Some((_, plugin)) = url.query_pairs().find(|(k, _)| k == "plugin") {
        let mut parts = plugin.split(';');
        let name = parts.next().unwrap_or_default();
        let mut opts = Proxy::new();
        for item in parts {
            let (key, value) = item
                .split_once('=')
                .map(|(k, v)| (k, json!(v)))
                .unwrap_or((item, json!(true)));
            opts.insert(key.into(), value);
        }
        match name {
            "obfs-local" | "simple-obfs" => {
                put(&mut p, "plugin", "obfs");
                if let Some(v) = opts.remove("obfs") {
                    opts.insert("mode".into(), v);
                }
                if let Some(v) = opts.remove("obfs-host") {
                    opts.insert("host".into(), v);
                }
            }
            "v2ray-plugin" => {
                put(&mut p, "plugin", name);
            }
            _ => bail!("unsupported Shadowsocks plugin '{name}'"),
        }
        put(&mut p, "plugin-opts", opts);
    }
    Ok(p)
}

fn vmess(link: &str) -> Result<Proxy> {
    let data: Value = serde_json::from_slice(&decode64(link.trim_start_matches("vmess://"))?)
        .context("VMess links must contain base64-encoded JSON")?;
    let field = |key: &str| -> String {
        data.get(key)
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default()
    };
    let server = field("add");
    ensure!(!server.is_empty(), "VMess link has no server");
    let port: u16 = field("port").parse().context("invalid VMess port")?;
    ensure!(port > 0, "proxy port cannot be zero");
    let id = field("id");
    ensure!(!id.is_empty(), "VMess link has no UUID");
    let mut p = json!({"name":field("ps"),"type":"vmess","server":server,"port":port,"uuid":id,
        "alterId":field("aid").parse::<u32>().unwrap_or(0),"cipher":if field("scy").is_empty(){"auto".into()}else{field("scy")},"udp":true})
        .as_object().unwrap().clone();
    if field("ps").is_empty() {
        put(&mut p, "name", format!("vmess-{server}:{port}"));
    }
    let mut q = HashMap::new();
    for (key, target) in [
        ("net", "type"),
        ("host", "host"),
        ("path", "path"),
        ("sni", "sni"),
        ("fp", "fp"),
        ("alpn", "alpn"),
    ] {
        let val = field(key);
        if !val.is_empty() {
            q.insert(target.into(), val);
        }
    }
    let tls = field("tls");
    if !tls.is_empty() && tls != "none" {
        q.insert("security".into(), tls);
    }
    if !field("type").is_empty() {
        q.insert("headerType".into(), field("type"));
    }
    let dummy = Url::parse("vmess://localhost:443")?;
    transport(&mut p, &q, &dummy)?;
    Ok(p)
}

fn ssr(link: &str) -> Result<Proxy> {
    let decoded = text64(link.trim_start_matches("ssr://"))?;
    let (endpoint, query) = decoded.split_once("/?").unwrap_or((&decoded, ""));
    let fields: Vec<_> = endpoint.rsplitn(6, ':').collect();
    ensure!(
        fields.len() == 6,
        "SSR link requires server:port:protocol:method:obfs:password"
    );
    let port: u16 = fields[4].parse().context("invalid SSR port")?;
    let server = fields[5].trim_matches(['[', ']']);
    let q: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let name = q
        .get("remarks")
        .map(|s| text64(s))
        .transpose()?
        .unwrap_or_else(|| format!("ssr-{server}:{port}"));
    let mut p = json!({"name": name, "type": "ssr", "server": server, "port": port,
        "protocol": fields[3], "cipher": fields[2], "obfs": fields[1], "password": text64(fields[0])?, "udp": true})
        .as_object().unwrap().clone();
    for (input, output) in [
        ("obfsparam", "obfs-param"),
        ("protoparam", "protocol-param"),
    ] {
        if let Some(value) = q.get(input) {
            put(&mut p, output, text64(value)?);
        }
    }
    Ok(p)
}

pub fn parse_link(link: &str) -> Result<Proxy> {
    let link = link.trim();
    if link.starts_with("ss://") {
        return ss(link);
    }
    if link.starts_with("vmess://") {
        return vmess(link);
    }
    if link.starts_with("ssr://") {
        return ssr(link);
    }
    let url = Url::parse(link).context("invalid proxy URL")?;
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let kind = match url.scheme() {
        "vless" | "trojan" | "tuic" | "hysteria" => url.scheme(),
        "hy2" | "hysteria2" => "hysteria2",
        "socks" | "socks5" | "socks5h" => "socks5",
        "http" | "https" => "http",
        scheme => bail!("unsupported proxy scheme '{scheme}'"),
    };
    let mut p = endpoint(&url, kind)?;
    let username = decoded(url.username())?;
    let password = url.password().map(decoded).transpose()?.unwrap_or_default();
    match kind {
        "vless" => {
            ensure!(!username.is_empty(), "VLESS link has no UUID");
            put(&mut p, "uuid", username);
            if let Some(flow) = q.get("flow") {
                put(&mut p, "flow", flow.clone());
            }
            transport(&mut p, &q, &url)?;
        }
        "trojan" => {
            ensure!(!username.is_empty(), "Trojan link has no password");
            put(&mut p, "password", username);
            transport(&mut p, &q, &url)?;
        }
        "hysteria2" => {
            let auth = if password.is_empty() {
                username
            } else {
                format!("{username}:{password}")
            };
            ensure!(!auth.is_empty(), "Hysteria2 link has no password");
            put(&mut p, "password", auth);
            if let Some(sni) = q.get("sni") {
                put(&mut p, "sni", sni.clone());
            }
            if let Some(v) = q.get("insecure") {
                put(&mut p, "skip-cert-verify", truth(v)?);
            }
            if let Some(obfs) = q.get("obfs") {
                put(&mut p, "obfs", obfs.clone());
            }
            if let Some(pwd) = q.get("obfs-password") {
                put(&mut p, "obfs-password", pwd.clone());
            }
            if let Some(ports) = q.get("mport") {
                put(&mut p, "ports", ports.clone());
            }
        }
        "hysteria" => {
            if let Some(auth) = q.get("auth") {
                put(&mut p, "auth-str", auth.clone());
            }
            if let Some(sni) = q.get("peer").or_else(|| q.get("sni")) {
                put(&mut p, "sni", sni.clone());
            }
            if let Some(v) = q.get("insecure") {
                put(&mut p, "skip-cert-verify", truth(v)?);
            }
            for (input, output) in [
                ("upmbps", "up"),
                ("downmbps", "down"),
                ("obfsParam", "obfs"),
            ] {
                if let Some(v) = q.get(input) {
                    put(&mut p, output, v.clone());
                }
            }
            if let Some(proto) = q.get("protocol") {
                put(&mut p, "protocol", proto.clone());
            }
        }
        "tuic" => {
            ensure!(
                !username.is_empty() && !password.is_empty(),
                "TUIC v5 link requires UUID and password"
            );
            put(&mut p, "uuid", username);
            put(&mut p, "password", password);
            for (input, output) in [
                ("sni", "sni"),
                ("congestion_control", "congestion-controller"),
                ("udp_relay_mode", "udp-relay-mode"),
            ] {
                if let Some(v) = q.get(input) {
                    put(&mut p, output, v.clone());
                }
            }
            if let Some(alpn) = q.get("alpn") {
                put(&mut p, "alpn", json!(alpn.split(',').collect::<Vec<_>>()));
            }
            if let Some(v) = q.get("allow_insecure").or_else(|| q.get("insecure")) {
                put(&mut p, "skip-cert-verify", truth(v)?);
            }
        }
        "socks5" | "http" => {
            if !username.is_empty() {
                put(&mut p, "username", username);
            }
            if !password.is_empty() {
                put(&mut p, "password", password);
            }
            if url.scheme() == "https" {
                put(&mut p, "tls", true);
            }
        }
        _ => unreachable!(),
    }
    if kind != "http" {
        put(&mut p, "udp", true);
    }
    Ok(p)
}

pub fn parse_subscription(body: &str) -> Result<Vec<Proxy>> {
    // Only import proxies. Never trust a subscription's rules, groups or listeners.
    if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(body) {
        if let Some(proxies) = value.get("proxies") {
            let proxies: Vec<Proxy> =
                serde_yaml::from_value(proxies.clone()).context("invalid Clash proxies list")?;
            return validate_proxies(proxies);
        }
        if let Some(proxies) = value.as_sequence()
            && proxies.iter().all(|p| p.is_mapping())
        {
            return validate_proxies(serde_yaml::from_value(value)?);
        }
    }
    let decoded_body;
    let text = if body.trim().contains("://") {
        body
    } else {
        decoded_body =
            text64(body).context("expected Clash YAML, proxy links, or a base64 subscription")?;
        &decoded_body
    };
    let mut proxies = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        proxies.push(
            parse_link(line)
                .with_context(|| format!("cannot parse subscription line {}", index + 1))?,
        );
    }
    validate_proxies(proxies)
}

pub fn validate_proxies(proxies: Vec<Proxy>) -> Result<Vec<Proxy>> {
    ensure!(!proxies.is_empty(), "source contains no proxies");
    for (index, p) in proxies.iter().enumerate() {
        let kind = p
            .get("type")
            .and_then(Value::as_str)
            .context("proxy has no type")?;
        ensure!(
            !["direct", "reject", "pass", "compatible"]
                .contains(&kind.to_ascii_lowercase().as_str()),
            "source contains a non-proxy outbound at position {}",
            index + 1
        );
        ensure!(
            p.get("server")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty()),
            "proxy {} has no server",
            index + 1
        );
        ensure!(
            p.get("port")
                .and_then(Value::as_u64)
                .is_some_and(|v| v > 0 && v <= 65535),
            "proxy {} has an invalid port",
            index + 1
        );
        // Provider/group references are untrusted and cannot survive a proxies-only import.
        ensure!(
            !p.contains_key("dialer-proxy"),
            "dialer-proxy dependencies require manual configuration"
        );
    }
    Ok(proxies)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vless_reality_ws() {
        let p=parse_link("vless://abc@host.test:443?security=reality&pbk=pub&sid=01&sni=sni.test&type=ws&path=%2Fws&host=cdn.test#Node%20One").unwrap();
        assert_eq!(p["name"], "Node One");
        assert_eq!(p["uuid"], "abc");
        assert_eq!(p["reality-opts"]["public-key"], "pub");
        assert_eq!(p["ws-opts"]["path"], "/ws");
    }
    #[test]
    fn vless_reality_preserves_mlkem_from_direct_links_and_subscriptions() {
        for (input, expected) in [("true", true), ("1", true), ("false", false), ("0", false)] {
            let link = format!(
                "vless://uuid@host.test:443?security=reality&pbk=public&sid=71645f53&sni=shop.example.org&fp=firefox&spx=%2Ftest&support-x25519mlkem768={input}&type=tcp"
            );
            let p = parse_link(&link).unwrap();
            assert_eq!(p["reality-opts"]["support-x25519mlkem768"], expected);
            assert_eq!(p["reality-opts"]["short-id"], "71645f53");
            assert_eq!(p["servername"], "shop.example.org");
            assert_eq!(p["client-fingerprint"], "firefox");
            let subscription = STANDARD.encode(&link);
            let imported = parse_subscription(&subscription).unwrap();
            assert_eq!(
                imported[0]["reality-opts"]["support-x25519mlkem768"],
                expected
            );
        }
        assert!(parse_link("vless://uuid@host.test:443?security=reality&pbk=public&support-x25519mlkem768=invalid").is_err());
        let p = parse_link("vless://uuid@host.test:443?security=reality&pbk=public").unwrap();
        assert!(p["reality-opts"].get("support-x25519mlkem768").is_none());
    }
    #[test]
    fn ss_variants() {
        for link in [
            "ss://YWVzLTI1Ni1nY206cGFzcw@host.test:443#SS".to_string(),
            format!(
                "ss://{}#SS",
                STANDARD.encode("aes-256-gcm:pass@host.test:443")
            ),
            "ss://aes-256-gcm:pass@host.test:443#SS".into(),
        ] {
            let p = parse_link(&link).unwrap();
            assert_eq!(p["password"], "pass");
            assert_eq!(p["cipher"], "aes-256-gcm");
        }
    }
    #[test]
    fn ipv6_and_password_escaping() {
        let p = parse_link("trojan://p%40ss%3Aword@[2001:db8::1]:443?sni=host.test#T").unwrap();
        assert_eq!(p["server"], "2001:db8::1");
        assert_eq!(p["password"], "p@ss:word");
    }
    #[test]
    fn vmess_json() {
        let link=format!("vmess://{}",STANDARD.encode(r#"{"v":"2","ps":"VM","add":"host.test","port":"443","id":"uuid","aid":"0","net":"ws","path":"/test","tls":"tls"}"#));
        let p = parse_link(&link).unwrap();
        assert_eq!(p["type"], "vmess");
        assert_eq!(p["tls"], true);
        assert_eq!(p["ws-opts"]["path"], "/test");
    }
    #[test]
    fn ignores_upstream_policy() {
        let p=parse_subscription("proxies:\n- {name: node, type: ss, server: host.test, port: 443, cipher: aes-128-gcm, password: test}\nrules: [MATCH,DIRECT]\nproxy-groups: [{name: Evil}]\n").unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0]["name"], "node");
    }
    #[test]
    fn base64_subscription() {
        let body =
            STANDARD_NO_PAD.encode("trojan://pass@a.test:443#A\nvless://uuid@b.test:8443#B\n");
        assert_eq!(parse_subscription(&body).unwrap().len(), 2);
    }
    #[test]
    fn extra_protocols() {
        for (link, kind) in [
            (
                "hy2://secret@host.test:443?obfs=salamander&obfs-password=x",
                "hysteria2",
            ),
            (
                "tuic://uuid:pwd@host.test:443?congestion_control=bbr",
                "tuic",
            ),
            ("socks5://user:pass@localhost:1080", "socks5"),
            ("https://user:pass@host.test:8443", "http"),
        ] {
            assert_eq!(parse_link(link).unwrap()["type"], kind);
        }
    }
    #[test]
    fn refuses_empty_or_bad_sources() {
        for s in ["proxies: []", "", "<html>bad</html>", "ssr://not-supported"] {
            assert!(parse_subscription(s).is_err());
        }
        assert!(parse_link("vless://uuid@host.test:443?type=xhttp").is_err());
        assert!(parse_link("vless://uuid@host.test:443?security=reality").is_err());
    }
    #[test]
    fn ssr_link() {
        let endpoint = format!(
            "host.test:443:auth_sha1_v4:aes-256-cfb:tls1.2_ticket_auth:{}/?remarks={}",
            URL_SAFE_NO_PAD.encode("secret"),
            URL_SAFE_NO_PAD.encode("SSR node")
        );
        let p = parse_link(&format!("ssr://{}", URL_SAFE_NO_PAD.encode(endpoint))).unwrap();
        assert_eq!(p["name"], "SSR node");
        assert_eq!(p["password"], "secret");
        assert_eq!(p["type"], "ssr");
    }
    #[test]
    fn ss_plugin() {
        let p=parse_link("ss://YWVzLTI1Ni1nY206cGFzcw@host.test:443?plugin=obfs-local%3Bobfs%3Dhttp%3Bobfs-host%3Dcdn.test").unwrap();
        assert_eq!(p["plugin"], "obfs");
        assert_eq!(p["plugin-opts"]["mode"], "http");
    }
}
