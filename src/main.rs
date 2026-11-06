mod api;
mod apply;
mod core;
mod geosite;
#[cfg(test)]
mod integration_tests;
mod links;
mod storage;
mod terminal;
mod tray;
mod tui;

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
    about = "Manage Mihomo sources and config. No subcommand opens the terminal dashboard."
)]
struct Cli {
    /// Directory containing settings, routing rules, and saved sources
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    /// Open the dashboard in a new configured or system default terminal
    #[arg(long)]
    window: bool,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Inspect or update the managed Mihomo core
    Core {
        #[command(subcommand)]
        command: core::Action,
    },
    /// Run the desktop tray or synchronize its systemd user service
    Tray {
        #[command(subcommand)]
        command: tray::Action,
    },
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
    List {
        /// Show cached node display names below subscription entries
        #[arg(long)]
        full: bool,
    },
    /// Check delays for all entries returned by the Mihomo API, fastest first
    Ping,
    /// Show the current mode, or change it without rewriting local config
    Mode {
        #[arg(value_enum)]
        mode: Option<api::Mode>,
    },
    /// Print the generated current.yaml without changing it
    Dump,
    /// Print the external controller URL and secret
    Controller,
    /// Select a proxy in PROXY; omit the name to show its current selection
    Switch { proxyname: Option<String> },
    /// Start the system Mihomo service through sudo
    Start,
    /// Stop the system Mihomo service through sudo
    Stop,
    /// Restart the system Mihomo service through sudo
    Restart,
    /// Remove a source and its cache; run update afterward to apply to Mihomo
    Delete {
        /// Exact source name
        #[arg(value_parser = parse_source_target)]
        target: Target,
    },
    /// Rename a saved source; omit the new name to enter it at a prompt
    Rename {
        /// Exact source name, using the same selector as delete
        #[arg(value_parser = parse_source_target)]
        target: Target,
        /// New unique source name
        #[arg(value_parser = parse_source_name)]
        new_name: Option<String>,
    },
    /// Install config into /var/lib/minohomo and restart minohomo.service (root)
    Apply {
        /// Generated Mihomo config to install
        #[arg(long)]
        config: PathBuf,
        /// Optional local geosite database to install alongside the config
        #[arg(long)]
        geosite: Option<PathBuf>,
        /// Version URL used if neither a runtime core nor a bundled core is available
        #[arg(long, default_value = core::VERSION_URL)]
        core_version_url: String,
        /// Archive URL used if neither a runtime core nor a bundled core is available
        #[arg(long, default_value = core::DOWNLOAD_URL)]
        core_download_url: String,
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
fn prompt_source_name(
    input: &mut impl std::io::BufRead,
    output: &mut impl std::io::Write,
) -> Result<String> {
    write!(output, "New source name: ")?;
    output.flush()?;
    let mut name = String::new();
    ensure!(
        input
            .read_line(&mut name)
            .context("cannot read new source name")?
            > 0,
        "no new name received; source was not renamed"
    );
    parse_source_name(&name).map_err(anyhow::Error::msg)
}
fn rename_source(state: &mut State, index: usize, new_name: &str) -> Result<String> {
    let new_name = parse_source_name(new_name).map_err(anyhow::Error::msg)?;
    ensure!(
        !state
            .sources
            .iter()
            .enumerate()
            .any(|(i, source)| i != index && source.name.as_deref() == Some(&new_name)),
        "source name '{new_name}' is already in use"
    );
    let old_name = state.sources[index]
        .name
        .clone()
        .context("source has no name")?;
    state.sources[index].name = Some(new_name);
    Ok(old_name)
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
    if cli.window {
        ensure!(
            cli.command.is_none(),
            "--window cannot be combined with a subcommand"
        );
        let root = cli.config_dir.map(Ok).unwrap_or_else(default_config_dir)?;
        if !root.join("config.yaml").exists() {
            // Initialize first use, then release the lock before launching the TUI.
            drop(Store::open(&root)?);
        }
        return terminal::open_dashboard(&root);
    }
    let Some(command) = cli.command else {
        let root = cli.config_dir.map(Ok).unwrap_or_else(default_config_dir)?;
        return tui::run(&root);
    };
    run_action(cli.config_dir, command)
}
fn run_action(config_dir: Option<PathBuf>, command: Action) -> Result<()> {
    // The privileged installer never loads user settings or initializes root's config.
    if let Action::Apply {
        config,
        geosite,
        core_version_url,
        core_download_url,
    } = &command
    {
        return apply::run(
            config,
            geosite.as_deref(),
            core_version_url,
            core_download_url,
        );
    }
    if let Action::Core { command } = &command {
        match command {
            core::Action::Version => {
                println!("{}", core::version()?);
                return Ok(());
            }
            core::Action::Update {
                version_url,
                download_url,
            } if unsafe { libc::geteuid() } == 0 => {
                return core::update(
                    version_url.as_deref().unwrap_or(core::VERSION_URL),
                    download_url.as_deref().unwrap_or(core::DOWNLOAD_URL),
                );
            }
            _ => {}
        }
    }
    if matches!(command, Action::Start | Action::Stop | Action::Restart) {
        let action = if matches!(command, Action::Start) {
            "start"
        } else if matches!(command, Action::Stop) {
            "stop"
        } else {
            "restart"
        };
        if unsafe { libc::geteuid() } == 0 {
            return apply::service(action);
        }
        let status = sudo_self()?
            .arg(action)
            .status()
            .context("cannot run sudo service command")?;
        ensure!(status.success(), "Mihomo {action} failed");
        return Ok(());
    }
    let config_dir = config_dir.map(Ok).unwrap_or_else(default_config_dir)?;
    if let Action::Tray { command } = &command {
        return tray::run(&config_dir, command);
    }
    let store = Store::open(&config_dir)?;
    // Never make ordinary CLI use depend on a desktop or a user bus.
    if unsafe { libc::geteuid() } != 0
        && config_dir == default_config_dir()?
        && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        && let Err(error) = tray::sync(&config_dir)
    {
        eprintln!("warning: cannot synchronize tray service: {error:#}");
    }
    if let Action::Core {
        command: core::Action::Update {
            version_url,
            download_url,
        },
    } = &command
    {
        let settings = store.settings()?;
        return core::update(
            version_url.as_deref().unwrap_or(&settings.core_version_url),
            download_url
                .as_deref()
                .unwrap_or(&settings.core_download_url),
        );
    }
    if let Action::Geosite { command } = &command {
        return geosite::run(&store, command);
    }
    match &command {
        Action::Ping => return api::Controller::new(&store.settings()?)?.ping(),
        Action::Mode { mode } => {
            let controller = api::Controller::new(&store.settings()?)?;
            if let Some(mode) = mode {
                return controller.mode(*mode);
            }
            println!("Mihomo mode: {}", controller.current_mode()?);
            return Ok(());
        }
        Action::Dump => {
            return dump_config(&store.settings()?.output, &mut std::io::stdout().lock());
        }
        Action::Controller => {
            print!("{}", api::Controller::new(&store.settings()?)?.info());
            return Ok(());
        }
        Action::Switch { proxyname } => {
            let settings = store.settings()?;
            let controller = api::Controller::new(&settings)?;
            if let Some(name) = proxyname {
                let state = store.state()?;
                let generated = named_proxies(&settings, &state)?;
                let name = resolve_switch(name, &state, &generated)?;
                return controller.switch(&name);
            }
            return controller.selection();
        }
        _ => {}
    }
    let mut state = store.state()?;
    match command {
        Action::Tray { .. }
        | Action::Core { .. }
        | Action::Geosite { .. }
        | Action::Apply { .. }
        | Action::Ping
        | Action::Mode { .. }
        | Action::Dump
        | Action::Controller
        | Action::Switch { .. }
        | Action::Start
        | Action::Stop
        | Action::Restart => unreachable!(),
        Action::Init => println!("Config files ready in {}", store.root.display()),
        Action::List { full } => print!("{}", format_list(&state, full)?),
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
        Action::Rename { target, new_name } => {
            let index = resolve_source(&target, &state)?;
            let new_name = match new_name {
                Some(name) => name,
                None => {
                    prompt_source_name(&mut std::io::stdin().lock(), &mut std::io::stderr().lock())?
                }
            };
            let old_name = rename_source(&mut state, index, &new_name)?;
            if old_name == new_name {
                println!("Source is already named '{new_name}'.");
            } else {
                store.save(&state)?;
                println!(
                    "Renamed '{old_name}' to '{new_name}'. Cache and timestamps are unchanged."
                );
                println!(
                    "Active Mihomo config is unchanged. Run 'minohomo update --no-sync' to apply generated subscription name changes."
                );
            }
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
fn dump_config(path: &std::path::Path, output: &mut impl std::io::Write) -> Result<()> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("cannot open {}; run minohomo update first", path.display()))?;
    std::io::copy(&mut file, output).context("cannot write generated config")?;
    Ok(())
}
fn format_list(state: &State, full: bool) -> Result<String> {
    use std::fmt::Write;
    let mut output = String::new();
    if state.sources.is_empty() {
        writeln!(
            output,
            "No sources. Add one with: minohomo add 'URL_OR_PROXY_LINK'"
        )?;
    }
    for source in &state.sources {
        let updated = source
            .updated_at
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_else(|| "never".into());
        let kind = if source.kind == SourceKind::Subscription {
            "subscription"
        } else {
            "proxy"
        };
        writeln!(
            output,
            "{} [{}] {}\n   Last update: {} | Cached proxies: {}",
            source.name.as_deref().context("source has no name")?,
            kind,
            source.link,
            updated,
            source.proxies.len()
        )?;
        if full && source.kind == SourceKind::Subscription {
            for proxy in &source.proxies {
                writeln!(
                    output,
                    "      {}",
                    proxy
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("proxy")
                )?;
            }
        }
    }
    Ok(output)
}
fn resolve_switch(input: &str, state: &State, generated: &[(u64, links::Proxy)]) -> Result<String> {
    let source = state
        .sources
        .iter()
        .find(|s| s.name.as_deref() == Some(input));
    let named = |proxy: &links::Proxy| proxy["name"].as_str().unwrap().to_owned();
    let mut matches: Vec<_> = generated
        .iter()
        .filter(|(id, proxy)| {
            if let Some(source) = source {
                return source.id == *id;
            }
            proxy.get("name").and_then(serde_json::Value::as_str) == Some(input)
        })
        .map(|(_, proxy)| named(proxy))
        .collect();
    if source.is_none() && matches.is_empty() {
        // list --full exposes cached display names, not generated subscription prefixes.
        matches = generated
            .iter()
            .zip(state.sources.iter().flat_map(|source| &source.proxies))
            .filter(|(_, original)| {
                original.get("name").and_then(serde_json::Value::as_str) == Some(input)
            })
            .map(|((_, proxy), _)| named(proxy))
            .collect();
    }
    ensure!(
        !matches.is_empty(),
        "no cached proxy for '{input}'; use list --full and update first"
    );
    ensure!(
        matches.len() == 1,
        "'{input}' matches several proxies; use the full generated name: {}",
        matches.join(", ")
    );
    Ok(matches[0].clone())
}
fn sudo_self() -> Result<Command> {
    let executable = std::env::current_exe().context("cannot locate minohomo executable")?;
    let command = if unsafe { libc::geteuid() } == 0 {
        Command::new(&executable)
    } else {
        let mut command = Command::new("sudo");
        command.arg("--").arg(&executable);
        command
    };
    // Do not pass --config-dir into the privileged subcommand.
    Ok(command)
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
fn named_proxies(settings: &Settings, state: &State) -> Result<Vec<(u64, links::Proxy)>> {
    let out = &settings.mihomo;
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
            proxies.push((source.id, p));
        }
    }
    Ok(proxies)
}
fn generate(settings: &Settings, policy: &Policy, state: &State) -> Result<Value> {
    let mut out = settings.mihomo.clone();
    let proxies = named_proxies(settings, state)?
        .into_iter()
        .map(|(_, proxy)| serde_yaml::to_value(proxy))
        .collect::<std::result::Result<Vec<_>, _>>()?;
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
    core::ensure_local(&settings)?;
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
    let user_agent = reqwest::header::HeaderValue::from_str(&settings.subscription_user_agent)
        .context("invalid subscription-user-agent")?;
    Ok(Client::builder()
        .timeout(Duration::from_secs(settings.request_timeout_seconds))
        .user_agent(user_agent)
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
        let mut command = sudo_self()?;
        command
            .arg("apply")
            .arg("--config")
            .arg(&settings.output)
            .arg("--core-version-url")
            .arg(&settings.core_version_url)
            .arg("--core-download-url")
            .arg(&settings.core_download_url);
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
        "{saved} saved locally, but apply/reload failed; retry with 'minohomo update --no-sync' and check 'systemctl status minohomo.service'"
    );
    println!("Mihomo apply command succeeded.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    #[test]
    fn subscription_user_agent_rejects_header_injection() {
        let mut settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        settings.subscription_user_agent = "CustomClient/2.0".into();
        assert!(http_client(&settings).is_ok());
        settings.subscription_user_agent = "CustomClient/2.0\r\nX-Injected: value".into();
        assert!(
            http_client(&settings)
                .unwrap_err()
                .to_string()
                .contains("invalid subscription-user-agent")
        );
    }

    #[test]
    fn clap_definition() {
        Cli::command().debug_assert();
    }
    #[test]
    fn window_dashboard_syntax() {
        let cli = Cli::try_parse_from(["minohomo", "--window"]).unwrap();
        assert!(cli.window);
        assert!(cli.command.is_none());
        for argv in [
            ["minohomo", "--window", "--config-dir", "/custom"],
            ["minohomo", "--config-dir", "/custom", "--window"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert_eq!(cli.config_dir, Some(PathBuf::from("/custom")));
            assert!(cli.window);
            assert!(cli.command.is_none());
        }
        assert!(Cli::try_parse_from(["minohomo", "open"]).is_err());
        let cli = Cli::try_parse_from(["minohomo", "--window", "init"]).unwrap();
        assert!(
            run(cli)
                .unwrap_err()
                .to_string()
                .contains("--window cannot be combined with a subcommand")
        );
    }

    #[test]
    fn no_subcommand_selects_dashboard() {
        assert!(Cli::try_parse_from(["minohomo"]).unwrap().command.is_none());
        let cli = Cli::try_parse_from(["minohomo", "--config-dir", "/custom"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.config_dir, Some(PathBuf::from("/custom")));
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
    fn core_syntax() {
        for args in [
            vec!["minohomo", "core", "version"],
            vec!["minohomo", "core", "update"],
            vec![
                "minohomo",
                "core",
                "update",
                "--version-url",
                "https://mirror.test/version.txt",
                "--download-url",
                "https://mirror.test/{artifact}-{version}.{ext}",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(Cli::try_parse_from(["minohomo", "core"]).is_err());
        assert!(Cli::try_parse_from(["minohomo", "core", "wrong"]).is_err());
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
    fn full_list_only_expands_subscriptions_with_indented_display_names() {
        let mut subscription = source(1);
        subscription.kind = SourceKind::Subscription;
        subscription.name = Some("Sub".into());
        subscription.proxies =
            vec![links::parse_link("trojan://pwd@host.test:443#Node%20One").unwrap()];
        let mut direct = source(2);
        direct.proxies =
            vec![links::parse_link("trojan://pwd@host.test:443#Direct%20Node").unwrap()];
        let state = State {
            next_id: 2,
            sources: vec![subscription, direct],
        };
        let plain = format_list(&state, false).unwrap();
        let expanded = format_list(&state, true).unwrap();
        assert_eq!(expanded.replace("      Node One\n", ""), plain);
        assert!(!expanded.contains("      Direct Node"));
        assert!(expanded.contains("Cached proxies: 1\n      Node One\n"));
    }
    #[test]
    fn switch_resolves_source_names_and_generated_subscription_names() {
        let settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let mut subscription = source(1);
        subscription.kind = SourceKind::Subscription;
        subscription.name = Some("Sub".into());
        subscription.proxies = vec![
            links::parse_link("trojan://pwd@host.test:443#One").unwrap(),
            links::parse_link("trojan://pwd@host.test:443#Two").unwrap(),
        ];
        let mut direct = source(2);
        direct.name = Some("Personal".into());
        let state = State {
            next_id: 2,
            sources: vec![subscription, direct],
        };
        let generated = named_proxies(&settings, &state).unwrap();
        assert_eq!(
            resolve_switch("Personal", &state, &generated).unwrap(),
            "same"
        );
        assert_eq!(
            resolve_switch("Sub: One", &state, &generated).unwrap(),
            "Sub: One"
        );
        assert_eq!(
            resolve_switch("One", &state, &generated).unwrap(),
            "Sub: One"
        );
        assert!(
            resolve_switch("Sub", &state, &generated)
                .unwrap_err()
                .to_string()
                .contains("several proxies")
        );
        assert!(resolve_switch("unknown", &state, &generated).is_err());
    }
    #[test]
    fn switch_handles_ambiguous_display_names_and_collision_suffixes() {
        let settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        let mut first = source(1);
        first.kind = SourceKind::Subscription;
        first.name = Some("A".into());
        let mut second = source(2);
        second.kind = SourceKind::Subscription;
        second.name = Some("B".into());
        let state = State {
            next_id: 4,
            sources: vec![first, second, source(3), source(4)],
        };
        let generated = named_proxies(&settings, &state).unwrap();
        // Exact generated names take precedence over bare display-name matching.
        assert_eq!(
            resolve_switch("A: same", &state, &generated).unwrap(),
            "A: same"
        );
        assert_eq!(
            resolve_switch("source4", &state, &generated).unwrap(),
            "same (1)"
        );
        let mut only_subs = state;
        only_subs.sources.truncate(2);
        let generated = named_proxies(&settings, &only_subs).unwrap();
        assert!(
            resolve_switch("same", &only_subs, &generated)
                .unwrap_err()
                .to_string()
                .contains("matches several proxies")
        );
    }
    #[test]
    fn dump_preserves_generated_config_bytes_and_reports_missing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("current.yaml");
        let mut output = Vec::new();
        assert!(dump_config(&path, &mut output).is_err());
        assert!(output.is_empty());
        let body = b"# generated\r\nmode: rule\nsecret: test-secret";
        std::fs::write(&path, body).unwrap();
        dump_config(&path, &mut output).unwrap();
        assert_eq!(output, body);
    }
    #[test]
    fn runtime_command_syntax() {
        for arguments in [
            vec!["minohomo", "list", "--full"],
            vec!["minohomo", "ping"],
            vec!["minohomo", "mode"],
            vec!["minohomo", "dump"],
            vec!["minohomo", "controller"],
            vec!["minohomo", "mode", "rule"],
            vec!["minohomo", "mode", "direct"],
            vec!["minohomo", "switch"],
            vec!["minohomo", "switch", "Personal"],
            vec!["minohomo", "start"],
            vec!["minohomo", "stop"],
            vec!["minohomo", "restart"],
            vec!["minohomo", "tray", "enable"],
            vec!["minohomo", "tray", "disable"],
            vec!["minohomo", "tray", "run"],
            vec!["minohomo", "tray", "sync"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_ok());
        }
        assert!(Cli::try_parse_from(["minohomo", "mode", "global"]).is_ok());
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
                matches!(cli.command, Some(Action::Update { target: Some(Target::Name(value)), .. }) if value == name)
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
        assert!(matches!(
            cli.command,
            Some(Action::Update { target: None, .. })
        ));
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
        assert_eq!(generated["ipv6"], false);
        assert_eq!(generated["dns"]["ipv6"], false);
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
    #[test]
    fn rename_syntax_uses_delete_selectors_and_optional_new_name() {
        for name in ["Personal", "1", "all", "_"] {
            let cli = Cli::try_parse_from(["minohomo", "rename", name]).unwrap();
            assert!(
                matches!(cli.command, Some(Action::Rename {target: Target::Name(target), new_name: None}) if target == name)
            );
        }
        assert!(Cli::try_parse_from(["minohomo", "rename"]).is_err());
        assert!(Cli::try_parse_from(["minohomo", "rename", "Old", " "]).is_err());
        let cli = Cli::try_parse_from(["minohomo", "rename", "--", "--old", "--new"]).unwrap();
        assert!(
            matches!(cli.command, Some(Action::Rename {target: Target::Name(target), new_name: Some(name)}) if target == "--old" && name == "--new")
        );
    }
    #[test]
    fn rename_preserves_everything_except_source_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = source(1);
        first.kind = SourceKind::Subscription;
        first.name = Some("1".into());
        let original = State {
            next_id: 2,
            sources: vec![first, source(2)],
        };
        {
            let store = Store::open(dir.path()).unwrap();
            store.save(&original).unwrap();
        }
        let config = std::fs::read(dir.path().join("config.yaml")).unwrap();
        std::fs::write(
            dir.path().join("current.yaml"),
            "unchanged generated config",
        )
        .unwrap();
        run_action(
            Some(dir.path().into()),
            Action::Rename {
                target: Target::Name("1".into()),
                new_name: Some("New source".into()),
            },
        )
        .unwrap();
        let store = Store::open(dir.path()).unwrap();
        let renamed = store.state().unwrap();
        let mut expected = serde_json::to_value(original).unwrap();
        expected["sources"][0]["name"] = "New source".into();
        assert_eq!(serde_json::to_value(renamed).unwrap(), expected);
        assert_eq!(
            std::fs::read(dir.path().join("config.yaml")).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("current.yaml")).unwrap(),
            "unchanged generated config"
        );
    }
    #[test]
    fn failed_or_noop_rename_does_not_write_saved_sources() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            store
                .save(&State {
                    next_id: 2,
                    sources: vec![source(1), source(2)],
                })
                .unwrap();
        }
        let path = dir.path().join("sources.yaml");
        let before = std::fs::read(&path).unwrap();
        for (target, new_name) in [
            ("source1", "source2"),
            ("missing", "New"),
            ("source1", " "),
            ("source1", "bad\nname"),
        ] {
            assert!(
                run_action(
                    Some(dir.path().into()),
                    Action::Rename {
                        target: Target::Name(target.into()),
                        new_name: Some(new_name.into())
                    }
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        run_action(
            Some(dir.path().into()),
            Action::Rename {
                target: Target::Name("source1".into()),
                new_name: Some("source1".into()),
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn rename_prompt_validates_input_and_handles_eof() {
        let mut prompt = Vec::new();
        let name = prompt_source_name(
            &mut std::io::Cursor::new(b"  New source  \r\n"),
            &mut prompt,
        )
        .unwrap();
        assert_eq!(name, "New source");
        assert_eq!(prompt, b"New source name: ");
        for input in ["", "\n", " \n", "bad\0name\n"] {
            assert!(prompt_source_name(&mut std::io::Cursor::new(input), &mut Vec::new()).is_err());
        }
    }
}
