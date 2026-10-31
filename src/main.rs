mod geosite;
#[cfg(test)]
mod integration_tests;
mod links;
mod storage;

use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use serde_yaml::Value;
use std::{
    collections::HashSet,
    io::Read,
    path::PathBuf,
    process::{Command, ExitCode},
    time::Duration,
};
use storage::{Policy, Settings, Source, SourceKind, State, Store, commit, key, staged_file};
use url::Url;

const MAX_BODY: u64 = 16 * 1024 * 1024;

#[derive(Parser)]
#[command(
    name = "minohomo",
    version,
    about = "Manage proxy sources and regenerate Mihomo config. Run as root."
)]
struct Cli {
    /// Directory containing settings, routing rules, and saved sources
    #[arg(long, global = true, default_value = "/etc/minohomo")]
    config_dir: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Inspect or manually update the geosite database
    Geosite {
        #[command(subcommand)]
        command: geosite::Action,
    },
    /// Create missing config files, without replacing existing files
    Init,
    /// Save subscription URLs or direct proxy links; does not fetch or reload
    Add {
        #[arg(required=true, num_args=1..)]
        links: Vec<String>,
        /// Treat HTTP(S) links as proxy endpoints rather than subscriptions
        #[arg(long)]
        proxy: bool,
    },
    /// Show one-based source indexes, cached proxy counts, and last update times
    List,
    /// Remove a source and its cache; run update afterward to apply to Mihomo
    Delete { index: usize },
    /// Fetch sources, regenerate config, validate it, and reload Mihomo
    Update {
        /// all, _, or a one-based source index; omitted means all
        #[arg(default_value="all", value_parser=parse_target)]
        target: Target,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    All,
    Index(usize),
}
fn parse_target(s: &str) -> std::result::Result<Target, String> {
    match s {
        "all" | "_" => Ok(Target::All),
        _ => s
            .parse::<usize>()
            .ok()
            .filter(|v| *v > 0)
            .map(Target::Index)
            .ok_or_else(|| "use all, _, or a positive source index".into()),
    }
}
fn main() -> ExitCode {
    let cli = Cli::parse();
    // Help and version work without root. All application commands require root.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("error: minohomo must run as root; use sudo minohomo ...");
        return ExitCode::FAILURE;
    }
    // Source caches and generated configs contain authentication secrets.
    unsafe {
        libc::umask(0o077);
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
fn run(cli: Cli) -> Result<()> {
    let store = Store::open(&cli.config_dir)?;
    if let Action::Geosite { command } = &cli.command {
        return geosite::run(&store, command);
    }
    let mut state = store.state()?;
    match cli.command {
        Action::Geosite { .. } => unreachable!(),
        Action::Init => println!("Config files ready in {}", store.root.display()),
        Action::List => {
            if state.sources.is_empty() {
                println!("No sources. Add one with: minohomo add 'URL_OR_PROXY_LINK'");
            }
            for (i, s) in state.sources.iter().enumerate() {
                let updated = s
                    .updated_at
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_else(|| "never".into());
                let kind = if s.kind == SourceKind::Subscription {
                    "subscription"
                } else {
                    "proxy"
                };
                println!(
                    "{}. [{}] {}\n   Last update: {} | Cached proxies: {}",
                    i + 1,
                    kind,
                    s.link,
                    updated,
                    s.proxies.len()
                );
            }
        }
        Action::Add { links, proxy } => {
            // Validate every argument before saving any of them.
            for link in links {
                ensure!(
                    !state.sources.iter().any(|s| s.link == link),
                    "source is already saved"
                );
                let kind = classify(&link, proxy)?;
                state.next_id = state
                    .next_id
                    .checked_add(1)
                    .context("source ID limit reached")?;
                state.sources.push(Source {
                    id: state.next_id,
                    kind,
                    link,
                    added_at: Utc::now(),
                    updated_at: None,
                    proxies: vec![],
                });
            }
            store.save(&state)?;
            println!("Saved. Run 'minohomo update' to fetch sources and apply the config.");
        }
        Action::Delete { index } => {
            ensure!(
                index > 0 && index <= state.sources.len(),
                "source index out of range (use minohomo list)"
            );
            state.sources.remove(index - 1);
            store.save(&state)?;
            println!(
                "Deleted source {index}. Indexes may have shifted. Run 'minohomo update' to apply."
            );
            if state.sources.is_empty() {
                println!(
                    "No sources remain. The active Mihomo config is unchanged until you add and update a new source."
                );
            }
        }
        Action::Update { target } => update(&store, &mut state, target)?,
    }
    Ok(())
}
fn classify(link: &str, force_proxy: bool) -> Result<SourceKind> {
    let url = Url::parse(link).context("invalid source URL")?;
    if ["http", "https"].contains(&url.scheme()) && !force_proxy {
        ensure!(url.host_str().is_some(), "subscription URL needs a host");
        ensure!(
            url.fragment().is_none(),
            "subscription URLs must not have fragments; use --proxy for HTTP proxy links"
        );
        return Ok(SourceKind::Subscription);
    }
    links::validate_proxies(vec![links::parse_link(link)?])?;
    Ok(SourceKind::Proxy)
}
fn fetch(client: &Client, source: &Source) -> Result<Vec<links::Proxy>> {
    if source.kind == SourceKind::Proxy {
        return links::validate_proxies(vec![links::parse_link(&source.link)?]);
    }
    // Avoid reqwest errors printing a private subscription URL.
    let response = client
        .get(&source.link)
        .send()
        .map_err(|e| e.without_url())
        .context("subscription request failed")?;
    let response = response
        .error_for_status()
        .map_err(|e| e.without_url())
        .context("subscription server returned an error")?;
    if let Some(size) = response.content_length() {
        ensure!(size <= MAX_BODY, "subscription exceeds 16 MiB");
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .context("cannot read subscription response")?;
    ensure!(
        bytes.len() as u64 <= MAX_BODY,
        "subscription exceeds 16 MiB"
    );
    let text = String::from_utf8(bytes).context("subscription is not UTF-8 text")?;
    links::parse_subscription(&text)
}
fn generate(settings: &Settings, policy: &Policy, state: &State) -> Result<Value> {
    let mut out = settings.mihomo.clone();
    let mut names = HashSet::new();
    if let Some(groups) = out.get(key("proxy-groups")).and_then(Value::as_sequence) {
        for group in groups {
            if let Some(name) = group.get("name").and_then(Value::as_str) {
                names.insert(name.to_string());
            }
        }
    }
    for reserved in [
        "DIRECT",
        "REJECT",
        "REJECT-DROP",
        "PASS",
        "COMPATIBLE",
        "GLOBAL",
    ] {
        names.insert(reserved.into());
    }
    let mut proxies = Vec::new();
    for source in &state.sources {
        for (index, original) in source.proxies.iter().enumerate() {
            let mut p = original.clone();
            p.extend(settings.proxy_overrides.clone());
            let label = p
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("proxy");
            let stem = format!("s{}: {label}", source.id);
            let mut name = stem.clone();
            let mut suffix = index + 1;
            while !names.insert(name.clone()) {
                name = format!("{stem} ({suffix})");
                suffix += 1;
            }
            p.insert("name".into(), name.into());
            proxies.push(serde_yaml::to_value(p)?);
        }
    }
    ensure!(
        !proxies.is_empty(),
        "no cached proxies; add and update at least one nonempty source"
    );
    // No HTTP proxy provider: subscription refreshes happen only through this CLI.
    out.remove(key("proxy-providers"));
    out.insert(key("proxies"), Value::Sequence(proxies));
    out.insert(key("rules"), serde_yaml::to_value(&policy.rules)?);
    let geox = out
        .entry(key("geox-url"))
        .or_insert_with(|| Value::Mapping(Default::default()));
    geox.as_mapping_mut()
        .context("mihomo.geox-url must be a mapping")?
        .insert(key("geosite"), settings.geosite_url.clone().into());
    if policy.rule_providers.is_empty() {
        out.remove(key("rule-providers"));
    } else {
        out.insert(
            key("rule-providers"),
            Value::Mapping(policy.rule_providers.clone()),
        );
    }
    Ok(Value::Mapping(out))
}
fn update(store: &Store, state: &mut State, target: Target) -> Result<()> {
    ensure!(
        !state.sources.is_empty(),
        "no sources saved; use minohomo add first"
    );
    if let Target::Index(index) = target {
        ensure!(
            index <= state.sources.len(),
            "source index out of range (use minohomo list)"
        );
    }
    let settings = store.settings()?;
    let policy = store.policy()?;
    let client = http_client(&settings)?;
    for (i, source) in state.sources.iter_mut().enumerate() {
        if target == Target::All || target == Target::Index(i + 1) {
            println!("Updating source {}...", i + 1);
            source.proxies = fetch(&client, source).with_context(|| {
                format!(
                    "source {} update failed; no config or source state was changed",
                    i + 1
                )
            })?;
            source.updated_at = Some(Utc::now());
            println!("  {} proxies", source.proxies.len());
        } else if source.updated_at.is_none() {
            println!(
                "Source {} has never been updated; omitted from this generation.",
                i + 1
            );
        }
    }
    let config = generate(&settings, &policy, state)?;
    let body = format!(
        "# Generated by minohomo. Edit /etc/minohomo/config.yaml and rules.yaml instead.\n{}",
        serde_yaml::to_string(&config)?
    );
    let candidate = staged_file(&settings.output, body.as_bytes())?;
    let parent = settings
        .output
        .parent()
        .context("output has no directory")?;
    println!("Validating with Mihomo...");
    // The daemon keeps reading the old config until validation succeeds.
    let status = Command::new(&settings.mihomo_binary)
        .arg("-t")
        .arg("-d")
        .arg(parent)
        .arg("-f")
        .arg(candidate.path())
        .status()
        .with_context(|| format!("cannot run {}", settings.mihomo_binary.display()))?;
    ensure!(
        status.success(),
        "Mihomo rejected the generated config; existing config and saved sources are unchanged"
    );
    let state_file = staged_file(
        &store.root.join("sources.yaml"),
        serde_yaml::to_string(state)?.as_bytes(),
    )?;
    // Preserve the previous config, including its credentials, as a root-only backup.
    if settings.output.exists() {
        let old = std::fs::read(&settings.output)?;
        storage::atomic_write(&settings.output.with_extension("yaml.bak"), &old)?;
    }
    commit(candidate, &settings.output)?;
    commit(state_file, &store.root.join("sources.yaml"))?;
    println!("Wrote {}", settings.output.display());
    reload(&settings, "config and source cache")
}
fn http_client(settings: &Settings) -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(settings.request_timeout_seconds))
        .user_agent("clash.meta/minohomo")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}
fn reload(settings: &Settings, saved: &str) -> Result<()> {
    let command = &settings.reload_command;
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .with_context(|| {
            format!("{saved} saved, but reload command could not run; reload Mihomo manually")
        })?;
    if !status.success() {
        bail!(
            "{saved} saved, but Mihomo reload failed; check 'systemctl status mihomo.service'. If inactive, start it with 'systemctl start mihomo.service'"
        );
    }
    println!("Mihomo reload command succeeded.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    #[test]
    fn clap_definition() {
        Cli::command().debug_assert();
    }
    #[test]
    fn geosite_syntax() {
        for argv in [
            vec!["minohomo", "geosite", "status"],
            vec!["minohomo", "geosite", "update"],
            vec![
                "minohomo",
                "geosite",
                "update",
                "--url",
                "https://mirror.example/geosite.dat",
                "--no-reload",
            ],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok());
        }
        assert!(Cli::try_parse_from(["minohomo", "geosite", "wrong"]).is_err());
    }
    #[test]
    fn update_syntax() {
        for argv in [
            vec!["minohomo", "update"],
            vec!["minohomo", "update", "all"],
            vec!["minohomo", "update", "_"],
            vec!["minohomo", "update", "2"],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok());
        }
        assert!(Cli::try_parse_from(["minohomo", "update", "0"]).is_err());
        assert!(Cli::try_parse_from(["minohomo", "update", "garbage"]).is_err());
    }
    fn source(id: u64) -> Source {
        Source {
            id,
            kind: SourceKind::Proxy,
            link: "trojan://pwd@host.test:443#same".into(),
            added_at: Utc::now(),
            updated_at: Some(Utc::now()),
            proxies: vec![links::parse_link("trojan://pwd@host.test:443#same").unwrap()],
        }
    }
    #[test]
    fn merge_and_policy() {
        let settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let policy: Policy = serde_yaml::from_str(include_str!("../defaults/rules.yaml")).unwrap();
        let mut a = source(1);
        a.proxies.push(a.proxies[0].clone());
        let state = State {
            next_id: 2,
            sources: vec![a, source(2)],
        };
        let generated = generate(&settings, &policy, &state).unwrap();
        let proxies = generated["proxies"].as_sequence().unwrap();
        let names: HashSet<_> = proxies
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 3);
        assert_eq!(generated["mixed-port"], 7897);
        assert_eq!(generated["tun"]["device"], "mihomo");
        assert_eq!(generated["tun"]["stack"], "gvisor");
        assert_eq!(generated["tun"]["auto-redirect"], false);
        assert_eq!(generated["sniffer"]["enable"], true);
        assert_eq!(generated["sniffer"]["parse-pure-ip"], true);
        assert_eq!(generated["sniffer"]["override-destination"], true);
        for protocol in ["HTTP", "TLS", "QUIC"] {
            assert_eq!(
                generated["sniffer"]["sniff"][protocol]["override-destination"], true,
                "sniffed hostnames must replace locally resolved destination IPs"
            );
        }
        assert_eq!(
            generated["rules"].as_sequence().unwrap().last().unwrap(),
            "MATCH,PROXY"
        );
        assert!(generated.get("proxy-providers").is_none());
        assert_eq!(
            generated["geox-url"]["geosite"].as_str().unwrap(),
            settings.geosite_url
        );
    }
    #[test]
    fn custom_geosite_url_is_written_to_mihomo_config() {
        let mut settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        settings.geosite_url = "https://mirror.example/custom.dat".into();
        let policy: Policy = serde_yaml::from_str(include_str!("../defaults/rules.yaml")).unwrap();
        let state = State {
            next_id: 1,
            sources: vec![source(1)],
        };
        let generated = generate(&settings, &policy, &state).unwrap();
        assert_eq!(
            generated["geox-url"]["geosite"],
            "https://mirror.example/custom.dat"
        );
    }
    #[test]
    fn proxy_overrides_apply_without_mutating_source_cache() {
        let mut settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let policy: Policy = serde_yaml::from_str(include_str!("../defaults/rules.yaml")).unwrap();
        let mut cached = source(1);
        cached.proxies[0].insert("client-fingerprint".into(), "firefox".into());
        let state = State {
            next_id: 1,
            sources: vec![cached],
        };
        let generated = generate(&settings, &policy, &state).unwrap();
        assert_eq!(generated["proxies"][0]["client-fingerprint"], "chrome");
        assert_eq!(state.sources[0].proxies[0]["client-fingerprint"], "firefox");
        settings.proxy_overrides.clear();
        let generated = generate(&settings, &policy, &state).unwrap();
        assert_eq!(generated["proxies"][0]["client-fingerprint"], "firefox");
    }
    #[test]
    fn older_settings_do_not_implicitly_override_source_proxies() {
        let mut value: Value =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        value
            .as_mapping_mut()
            .unwrap()
            .remove(key("proxy-overrides"));
        let settings: Settings = serde_yaml::from_value(value).unwrap();
        assert!(settings.proxy_overrides.is_empty());
    }
    #[test]
    fn default_dns_avoids_tun_bootstrap_and_tailscale_interface_loops() {
        let settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let dns = settings.mihomo.get(key("dns")).unwrap();
        for field in [
            "default-nameserver",
            "nameserver",
            "proxy-server-nameserver",
        ] {
            let servers = dns[field].as_sequence().unwrap();
            assert!(!servers.is_empty());
            for server in servers {
                let url = Url::parse(server.as_str().unwrap()).unwrap();
                assert_eq!(url.scheme(), "https");
                assert!(
                    url.host_str().unwrap().parse::<std::net::IpAddr>().is_ok(),
                    "DNS bootstrap must not require another DNS lookup"
                );
            }
        }
        assert_eq!(
            dns["nameserver-policy"]["+.ts.net"],
            "100.100.100.100#tailscale0"
        );
        assert!(
            dns["fake-ip-filter"]
                .as_sequence()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("+.ts.net"))
        );
        let china = dns["nameserver-policy"]["geosite:cn,alibaba,tencent"]
            .as_str()
            .unwrap();
        assert!(
            Url::parse(china)
                .unwrap()
                .host_str()
                .unwrap()
                .parse::<std::net::IpAddr>()
                .is_ok()
        );
    }
    #[test]
    fn editable_rules_replace_defaults() {
        let settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let policy: Policy =
            serde_yaml::from_str("rules:\n - DOMAIN,example.org,DIRECT\n - MATCH,REJECT\n")
                .unwrap();
        let generated = generate(
            &settings,
            &policy,
            &State {
                next_id: 1,
                sources: vec![source(1)],
            },
        )
        .unwrap();
        assert_eq!(generated["rules"].as_sequence().unwrap().len(), 2);
        assert_eq!(generated["rules"][1], "MATCH,REJECT");
    }
    #[test]
    fn classify_sources() {
        assert_eq!(
            classify("https://example.org/sub", false).unwrap(),
            SourceKind::Subscription
        );
        assert_eq!(
            classify("https://user:pass@host.test:443", true).unwrap(),
            SourceKind::Proxy
        );
        assert_eq!(
            classify("vless://uuid@host.test:443", false).unwrap(),
            SourceKind::Proxy
        );
        assert!(classify("file:///etc/shadow", false).is_err());
    }
}
