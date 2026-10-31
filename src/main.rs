mod apply;
mod geosite;
#[cfg(test)]
mod integration_tests;
mod links;
mod storage;

use anyhow::{Context, Result, ensure};
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
    about = "Manage proxy sources and regenerate Mihomo config."
)]
struct Cli {
    /// Directory containing settings, routing rules, and saved sources
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
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
        /// Unique source name for delete/update (default: random six-character hex)
        #[arg(long, value_parser = parse_source_name)]
        name: Option<String>,
    },
    /// Show source names, cached proxy counts, and last update times
    List,
    /// Remove a source and its cache; run update afterward to apply to Mihomo
    Delete {
        /// Exact source name
        #[arg(value_parser = parse_source_target)]
        target: Target,
    },
    /// Install a generated config into /etc/mihomo and restart the system service (root)
    Apply {
        /// Generated Mihomo config to install
        #[arg(long)]
        config: PathBuf,
        /// Optional local geosite database to install alongside the config
        #[arg(long)]
        geosite: Option<PathBuf>,
    },
    /// Fetch sources, regenerate config, validate it, and apply it through sudo
    Update {
        /// Exact source name; omit to update every source
        #[arg(value_parser = parse_source_target)]
        target: Option<Target>,
        /// Regenerate from cached proxies without fetching any sources
        #[arg(long)]
        no_sync: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    All,
    Name(String),
}
fn parse_source_target(s: &str) -> std::result::Result<Target, String> {
    parse_source_name(s).map(Target::Name)
}
fn resolve_source(target: &Target, state: &State) -> Result<usize> {
    let Target::Name(name) = target else {
        anyhow::bail!("select one source by name");
    };
    let matches: Vec<_> = state
        .sources
        .iter()
        .enumerate()
        .filter(|(_, source)| source.name.as_deref() == Some(name.as_str()))
        .map(|(i, _)| i)
        .collect();
    ensure!(
        !matches.is_empty(),
        "no source named '{name}' (use minohomo list)"
    );
    ensure!(
        matches.len() == 1,
        "multiple sources named '{name}'; give each source a unique name in sources.yaml"
    );
    Ok(matches[0])
}
fn parse_source_name(value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err("name must be nonempty and contain no control characters".into());
    }
    Ok(value.to_owned())
}
fn main() -> ExitCode {
    let cli = Cli::parse();
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
fn default_config_dir() -> Result<PathBuf> {
    let home =
        std::env::var_os("HOME").context("HOME is not set; use --config-dir /absolute/path")?;
    config_dir_for_home(std::path::Path::new(&home))
}
fn config_dir_for_home(home: &std::path::Path) -> Result<PathBuf> {
    ensure!(
        home.is_absolute(),
        "HOME must be an absolute path; use --config-dir /absolute/path"
    );
    Ok(home.join(".config/minohomo"))
}
fn run(cli: Cli) -> Result<()> {
    // The privileged installer never loads user settings or initializes root's config.
    if let Action::Apply { config, geosite } = &cli.command {
        return apply::run(config, geosite.as_deref());
    }
    let config_dir = cli.config_dir.map(Ok).unwrap_or_else(default_config_dir)?;
    let store = Store::open(&config_dir)?;
    if let Action::Geosite { command } = &cli.command {
        return geosite::run(&store, command);
    }
    let mut state = store.state()?;
    match cli.command {
        Action::Geosite { .. } | Action::Apply { .. } => unreachable!(),
        Action::Init => println!("Config files ready in {}", store.root.display()),
        Action::List => {
            if state.sources.is_empty() {
                println!("No sources. Add one with: minohomo add 'URL_OR_PROXY_LINK'");
            }
            for s in &state.sources {
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
                    "{} [{}] {}\n   Last update: {} | Cached proxies: {}",
                    s.name.as_deref().context("source has no name")?,
                    kind,
                    s.link,
                    updated,
                    s.proxies.len()
                );
            }
        }
        Action::Add { links, proxy, name } => {
            ensure!(
                name.is_none() || links.len() == 1,
                "--name requires exactly one link; each source needs a unique name"
            );
            if let Some(name) = &name {
                ensure!(
                    !state.sources.iter().any(|s| s.name.as_ref() == Some(name)),
                    "source name '{name}' is already in use"
                );
            }
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
                let source_name = match &name {
                    Some(name) => name.clone(),
                    None => storage::random_source_name(&state)?,
                };
                println!("Source name: {source_name}");
                state.sources.push(Source {
                    id: state.next_id,
                    kind,
                    link,
                    name: Some(source_name),
                    added_at: Utc::now(),
                    updated_at: None,
                    proxies: vec![],
                });
            }
            store.save(&state)?;
            println!("Saved. Run 'minohomo update' to fetch sources and apply the config.");
        }
        Action::Delete { target } => {
            let index = resolve_source(&target, &state)?;
            let removed = state.sources.remove(index);
            store.save(&state)?;
            println!(
                "Deleted source '{}'. Run 'minohomo update --no-sync' to apply.",
                removed.name.as_deref().unwrap()
            );
            if state.sources.is_empty() {
                println!(
                    "No sources remain. The active Mihomo config is unchanged until you add and update a new source."
                );
            }
        }
        Action::Update { target, no_sync } => {
            update(&store, &mut state, target.unwrap_or(Target::All), no_sync)?
        }
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
            let stem = match source.kind {
                SourceKind::Subscription => format!(
                    "{}: {label}",
                    source
                        .name
                        .as_deref()
                        .context("subscription has no source name")?
                ),
                SourceKind::Proxy => label.to_owned(),
            };
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
fn update(store: &Store, state: &mut State, target: Target, no_sync: bool) -> Result<()> {
    ensure!(
        !state.sources.is_empty(),
        "no sources saved; use minohomo add first"
    );
    ensure!(
        !no_sync || target == Target::All,
        "--no-sync cannot be combined with a source name"
    );
    let selected = if target == Target::All {
        None
    } else {
        Some(resolve_source(&target, state)?)
    };
    let settings = store.settings()?;
    let policy = store.policy()?;
    let client = if no_sync {
        None
    } else {
        Some(http_client(&settings)?)
    };
    for (i, source) in state.sources.iter_mut().enumerate() {
        if !no_sync && (selected.is_none() || selected == Some(i)) {
            println!("Updating source {}...", source.name.as_deref().unwrap());
            source.proxies = fetch(client.as_ref().unwrap(), source).with_context(|| {
                format!(
                    "source '{}' update failed; no config or source state was changed",
                    source.name.as_deref().unwrap()
                )
            })?;
            source.updated_at = Some(Utc::now());
            println!("  {} proxies", source.proxies.len());
        } else if source.updated_at.is_none() {
            println!(
                "Source {} has never been updated; omitted from this generation.",
                source.name.as_deref().unwrap()
            );
        }
    }
    let config = generate(&settings, &policy, state)?;
    let body = format!(
        "# Generated by minohomo. Edit {} and {} instead.\n{}",
        store.root.join("config.yaml").display(),
        store.root.join("rules.yaml").display(),
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
    // Preserve the previous config, including its credentials, as an owner-only backup.
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
        .user_agent("V2rayNG/1.8.12")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}
fn reload(settings: &Settings, saved: &str) -> Result<()> {
    let mut command = if let Some(program) = settings.reload_command.first() {
        // Explicit custom deployment command, retained for existing installations.
        let mut command = Command::new(program);
        command.args(&settings.reload_command[1..]);
        command
    } else {
        let executable = std::env::current_exe().context("cannot locate minohomo executable")?;
        let mut command = if unsafe { libc::geteuid() } == 0 {
            Command::new(executable)
        } else {
            let mut command = Command::new("sudo");
            command.arg("--").arg(executable);
            command
        };
        command.arg("apply").arg("--config").arg(&settings.output);
        let database = settings
            .output
            .parent()
            .context("output has no directory")?
            .join("GeoSite.dat");
        if database.exists() {
            command.arg("--geosite").arg(database);
        }
        command
    };
    let status = command.status().with_context(|| {
        format!("{saved} saved locally, but apply command could not run; retry with 'minohomo update --no-sync'")
    })?;
    ensure!(
        status.success(),
        "{saved} saved locally, but apply/reload failed; retry with 'minohomo update --no-sync' and check 'systemctl status mihomo.service'"
    );
    println!("Mihomo apply command succeeded.");
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
    fn user_config_directory() {
        assert_eq!(
            config_dir_for_home(std::path::Path::new("/home/test")).unwrap(),
            PathBuf::from("/home/test/.config/minohomo")
        );
        assert!(config_dir_for_home(std::path::Path::new("")).is_err());
        assert!(config_dir_for_home(std::path::Path::new("relative")).is_err());
        assert!(
            Cli::try_parse_from(["minohomo", "init"])
                .unwrap()
                .config_dir
                .is_none()
        );
        assert_eq!(
            Cli::try_parse_from(["minohomo", "--config-dir", "/custom", "init"])
                .unwrap()
                .config_dir,
            Some(PathBuf::from("/custom"))
        );
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
    fn apply_syntax() {
        assert!(
            Cli::try_parse_from([
                "minohomo",
                "apply",
                "--config",
                "/tmp/current.yaml",
                "--geosite",
                "/tmp/GeoSite.dat"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["minohomo", "apply"]).is_err());
    }
    #[test]
    fn update_syntax() {
        for argv in [
            vec!["minohomo", "update"],
            vec!["minohomo", "update", "all"],
            vec!["minohomo", "update", "_"],
            vec!["minohomo", "update", "2"],
            vec!["minohomo", "update", "FooBar"],
            vec!["minohomo", "update", "--no-sync"],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok());
        }
        assert!(Cli::try_parse_from(["minohomo", "update", "0"]).is_ok());
        assert!(Cli::try_parse_from(["minohomo", "update", " "]).is_err());
    }
    fn source(id: u64) -> Source {
        Source {
            id,
            kind: SourceKind::Proxy,
            link: "trojan://pwd@host.test:443#same".into(),
            name: Some(format!("source{id}")),
            added_at: Utc::now(),
            updated_at: Some(Utc::now()),
            proxies: vec![links::parse_link("trojan://pwd@host.test:443#same").unwrap()],
        }
    }
    #[test]
    fn name_selection_rejects_missing_or_ambiguous_sources() {
        let mut first = source(1);
        first.name = Some("FooBar".into());
        let mut state = State {
            next_id: 2,
            sources: vec![first.clone(), source(2)],
        };
        assert_eq!(
            resolve_source(&Target::Name("FooBar".into()), &state).unwrap(),
            0
        );
        assert!(resolve_source(&Target::Name("foobar".into()), &state).is_err());
        assert!(resolve_source(&Target::Name("missing".into()), &state).is_err());
        state.sources.push(first);
        assert!(
            resolve_source(&Target::Name("FooBar".into()), &state)
                .unwrap_err()
                .to_string()
                .contains("multiple sources")
        );
        assert!(resolve_source(&Target::Name("1".into()), &state).is_err());
    }
    #[test]
    fn delete_accepts_names_including_numeric_names() {
        for selector in ["FooBar", "1"] {
            let dir = tempfile::tempdir().unwrap();
            {
                let store = Store::open(dir.path()).unwrap();
                let mut named = source(1);
                named.name = Some(selector.into());
                store
                    .save(&State {
                        next_id: 2,
                        sources: vec![named, source(2)],
                    })
                    .unwrap();
            }
            let cli = Cli::try_parse_from([
                "minohomo",
                "--config-dir",
                dir.path().to_str().unwrap(),
                "delete",
                selector,
            ])
            .unwrap();
            run(cli).unwrap();
            let store = Store::open(dir.path()).unwrap();
            let state = store.state().unwrap();
            assert_eq!(state.sources.len(), 1);
            assert_eq!(state.sources[0].id, 2);
        }
    }
    #[test]
    fn source_names_and_node_display_names_are_separate() {
        let settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let policy: Policy = serde_yaml::from_str(include_str!("../defaults/rules.yaml")).unwrap();
        let mut named = source(1);
        named.name = Some("FooBar".into());
        named.kind = SourceKind::Subscription;
        named.proxies = vec![
            links::parse_link("trojan://pwd@host.test:443#Proxy1").unwrap(),
            links::parse_link("trojan://pwd@host.test:443#Proxy2").unwrap(),
            links::parse_link("trojan://pwd@host.test:443#Proxy1").unwrap(),
        ];
        let mut direct = source(2);
        direct.name = Some("Personal".into());
        let state = State {
            next_id: 3,
            sources: vec![named, direct, source(3)],
        };
        let generated = generate(&settings, &policy, &state).unwrap();
        let names: Vec<_> = generated["proxies"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "FooBar: Proxy1",
                "FooBar: Proxy2",
                "FooBar: Proxy1 (3)",
                "same",
                "same (1)"
            ]
        );
        assert_eq!(state.sources[0].proxies[0]["name"], "Proxy1");
    }
    #[test]
    fn add_name_is_unique_and_persisted_for_subscriptions_and_proxies() {
        let dir = tempfile::tempdir().unwrap();
        for (link, name) in [
            ("https://host.test/sub", "FooBar"),
            ("trojan://pwd@host.test:443#Proxy1", "Personal"),
        ] {
            run(Cli::try_parse_from([
                "minohomo",
                "--config-dir",
                dir.path().to_str().unwrap(),
                "add",
                link,
                "--name",
                name,
            ])
            .unwrap())
            .unwrap();
        }
        let duplicate = Cli::try_parse_from([
            "minohomo",
            "--config-dir",
            dir.path().to_str().unwrap(),
            "add",
            "https://other.test/sub",
            "--name",
            "FooBar",
        ])
        .unwrap();
        assert!(run(duplicate).is_err());
        let store = Store::open(dir.path()).unwrap();
        let state = store.state().unwrap();
        assert_eq!(state.sources.len(), 2);
        assert_eq!(state.sources[0].name.as_deref(), Some("FooBar"));
        assert_eq!(state.sources[1].name.as_deref(), Some("Personal"));
        for invalid in ["", "   ", "bad\nname"] {
            assert!(
                Cli::try_parse_from([
                    "minohomo",
                    "add",
                    "https://host.test/sub",
                    "--name",
                    invalid
                ])
                .is_err()
            );
        }
    }
    #[test]
    fn unnamed_add_assigns_unique_hex_names() {
        let dir = tempfile::tempdir().unwrap();
        run(Cli::try_parse_from([
            "minohomo",
            "--config-dir",
            dir.path().to_str().unwrap(),
            "add",
            "https://host.test/sub",
            "trojan://pwd@host.test:443#Proxy1",
        ])
        .unwrap())
        .unwrap();
        let store = Store::open(dir.path()).unwrap();
        let state = store.state().unwrap();
        for source in &state.sources {
            let name = source.name.as_ref().unwrap();
            assert_eq!(name.len(), 6);
            assert!(
                name.bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
        assert_ne!(state.sources[0].name, state.sources[1].name);
        assert_eq!(
            store.state().unwrap().sources[0].name,
            state.sources[0].name
        );
    }
    #[test]
    fn every_explicit_target_is_a_name() {
        for name in ["0", "1", "123456", "all", "_", "FooBar"] {
            let cli = Cli::try_parse_from(["minohomo", "update", name]).unwrap();
            assert!(
                matches!(cli.command, Action::Update { target: Some(Target::Name(value)), .. } if value == name)
            );
            let mut named = source(9);
            named.name = Some(name.into());
            let state = State {
                next_id: 9,
                sources: vec![source(1), named],
            };
            assert_eq!(
                resolve_source(&Target::Name(name.into()), &state).unwrap(),
                1
            );
        }
        let cli = Cli::try_parse_from(["minohomo", "update"]).unwrap();
        assert!(matches!(cli.command, Action::Update { target: None, .. }));
    }
    #[test]
    fn named_bulk_add_is_rejected_without_saving() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "minohomo",
            "--config-dir",
            dir.path().to_str().unwrap(),
            "add",
            "https://a.test/sub",
            "https://b.test/sub",
            "--name",
            "Shared",
        ])
        .unwrap();
        assert!(run(cli).is_err());
        assert!(
            Store::open(dir.path())
                .unwrap()
                .state()
                .unwrap()
                .sources
                .is_empty()
        );
    }
    #[test]
    fn legacy_sources_get_persistent_unique_names_without_losing_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut unnamed = source(1);
        unnamed.name = None;
        let mut named = source(2);
        named.name = Some("KeepMe".into());
        let mut duplicate = source(3);
        duplicate.name = Some("KeepMe".into());
        store
            .save(&State {
                next_id: 3,
                sources: vec![unnamed.clone(), named, duplicate],
            })
            .unwrap();
        let migrated = store.state().unwrap();
        let names: Vec<_> = migrated
            .sources
            .iter()
            .map(|s| s.name.clone().unwrap())
            .collect();
        assert_eq!(names[1], "KeepMe");
        assert_eq!(names[0].len(), 6);
        assert_eq!(names[2].len(), 6);
        assert_ne!(names[0], names[2]);
        assert_eq!(migrated.sources[0].proxies, unnamed.proxies);
        assert_eq!(migrated.sources[0].updated_at, unnamed.updated_at);
        assert_eq!(
            store
                .state()
                .unwrap()
                .sources
                .iter()
                .map(|s| s.name.clone().unwrap())
                .collect::<Vec<_>>(),
            names
        );
    }
    #[test]
    fn old_sources_without_name_remain_compatible() {
        let mut value = serde_json::to_value(source(1)).unwrap();
        value.as_object_mut().unwrap().remove("name");
        let restored: Source = serde_json::from_value(value).unwrap();
        assert!(restored.name.is_none());
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
