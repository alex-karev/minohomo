use crate::{
    http_client, reload,
    storage::{self, Settings, Store, key},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use prost::Message;
use serde_yaml::{Mapping, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
use url::Url;

const MAX_DATABASE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Show configured source, database path, size, and modification time
    Status,
    /// Download, validate, replace geosite data, then reload Mihomo
    Update {
        /// One-off source override; edit geosite-url in config.yaml for permanence
        #[arg(long)]
        url: Option<String>,
        /// Save the database without reloading Mihomo (useful before first start)
        #[arg(long)]
        no_reload: bool,
    },
}

// Only decode the fields needed to check the standard V2Ray geosite protobuf.
// Prost ignores unknown fields, preserving compatibility with newer datasets.
#[derive(Clone, Message)]
struct SiteList {
    #[prost(message, repeated, tag = "1")]
    entries: Vec<Site>,
}
#[derive(Clone, Message)]
struct Site {
    #[prost(string, tag = "1")]
    code: String,
    #[prost(message, repeated, tag = "2")]
    domains: Vec<Domain>,
}
#[derive(Clone, Message)]
struct Domain {
    #[prost(int32, tag = "1")]
    kind: i32,
    #[prost(string, tag = "2")]
    value: String,
}
fn inspect(bytes: &[u8]) -> Result<SiteList> {
    let list = SiteList::decode(bytes).context("not a valid geosite protobuf database")?;
    ensure!(
        !list.entries.is_empty(),
        "geosite database contains no categories"
    );
    ensure!(
        list.entries.iter().all(|s| !s.code.is_empty()),
        "geosite database contains an unnamed category"
    );
    ensure!(
        list.entries
            .iter()
            .flat_map(|s| &s.domains)
            .any(|d| !d.value.is_empty()),
        "geosite database contains no domains"
    );
    Ok(list)
}
fn path(settings: &Settings) -> Result<PathBuf> {
    Ok(settings
        .output
        .parent()
        .context("output has no directory")?
        .join("GeoSite.dat"))
}
pub fn run(store: &Store, action: &Action) -> Result<()> {
    let settings = store.settings()?;
    let database = path(&settings)?;
    match action {
        Action::Status => status(&settings, &database),
        Action::Update { url, no_reload } => {
            let source = url.as_deref().unwrap_or(&settings.geosite_url);
            let parsed = Url::parse(source).context("invalid geosite URL")?;
            ensure!(
                ["https", "http"].contains(&parsed.scheme()) && parsed.host_str().is_some(),
                "geosite URL must be HTTP(S)"
            );
            println!("Downloading geosite data...");
            let response = http_client(&settings)?
                .get(source)
                .send()
                .map_err(|e| e.without_url())
                .context("geosite download failed")?
                .error_for_status()
                .map_err(|e| e.without_url())
                .context("geosite server returned an error")?;
            if let Some(size) = response.content_length() {
                ensure!(size <= MAX_DATABASE, "geosite database exceeds 64 MiB");
            }
            let mut bytes = Vec::new();
            response.take(MAX_DATABASE + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= MAX_DATABASE,
                "geosite database exceeds 64 MiB"
            );
            install(store, &settings, &database, &bytes)?;
            println!("Updated {} ({} bytes).", database.display(), bytes.len());
            if url.is_some() {
                println!(
                    "Used the one-off URL override; geosite-url in config.yaml was not changed."
                );
            }
            if *no_reload {
                println!(
                    "Reload skipped. The new data takes effect on the next config reload or daemon start."
                );
                Ok(())
            } else {
                reload(&settings, "geosite database")
            }
        }
    }
}
fn status(settings: &Settings, database: &Path) -> Result<()> {
    println!("Configured URL: {}", settings.geosite_url);
    println!("File: {}", database.display());
    let metadata = match fs::metadata(database) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("Status: missing. Run 'minohomo geosite update'.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    ensure!(
        metadata.len() <= MAX_DATABASE,
        "database exceeds inspection limit (64 MiB)"
    );
    let modified: DateTime<Utc> = metadata.modified()?.into();
    println!("Size: {} bytes", metadata.len());
    println!(
        "Last modified: {}",
        modified.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    let list = inspect(&fs::read(database)?).context("installed geosite database is invalid")?;
    let domains: usize = list.entries.iter().map(|s| s.domains.len()).sum();
    println!(
        "Status: valid protobuf | Categories: {} | Domain entries: {}",
        list.entries.len(),
        domains
    );
    println!("This is the file's modification time, not the publisher's release date.");
    Ok(())
}
fn required_tags(store: &Store, settings: &Settings, fallback: &str) -> Result<BTreeSet<String>> {
    let mut tags = BTreeSet::new();
    for rule in store.policy()?.rules {
        for (position, _) in rule.to_ascii_uppercase().match_indices("GEOSITE,") {
            if let Some(tag) = rule[position + 8..].split([',', ')']).next() {
                tags.insert(tag.trim().to_owned());
            }
        }
    }
    if let Some(policy) = settings
        .mihomo
        .get(key("dns"))
        .and_then(|v| v.get("nameserver-policy"))
        .and_then(Value::as_mapping)
    {
        for name in policy.keys().filter_map(Value::as_str) {
            if let Some(names) = name.strip_prefix("geosite:") {
                tags.extend(names.split(',').map(str::to_owned));
            }
        }
    }
    if tags.is_empty() {
        tags.insert(fallback.to_owned());
    }
    Ok(tags)
}
fn install(store: &Store, settings: &Settings, database: &Path, bytes: &[u8]) -> Result<()> {
    let list = inspect(bytes)?;
    let tags = required_tags(store, settings, &list.entries[0].code)?;
    let work = tempfile::tempdir_in(&store.root)?;
    storage::atomic_write(&work.path().join("GeoSite.dat"), bytes)?;
    let mut candidate = Mapping::new();
    candidate.insert(key("geodata-mode"), true.into());
    candidate.insert(key("geo-auto-update"), false.into());
    // Do not let Mihomo replace a broken candidate with a network download.
    candidate.insert(
        key("geox-url"),
        serde_yaml::from_str::<Value>("{geosite: 'http://127.0.0.1:1/disabled'}")?,
    );
    let rules: Vec<String> = tags
        .iter()
        .map(|tag| format!("GEOSITE,{tag},DIRECT"))
        .chain(std::iter::once("MATCH,DIRECT".into()))
        .collect();
    candidate.insert(key("rules"), serde_yaml::to_value(rules)?);
    let config = work.path().join("validate.yaml");
    storage::atomic_write(&config, serde_yaml::to_string(&candidate)?.as_bytes())?;
    println!(
        "Validating geosite data and {} required categories with Mihomo...",
        tags.len()
    );
    let result = Command::new(&settings.mihomo_binary)
        .arg("-t")
        .arg("-d")
        .arg(work.path())
        .arg("-f")
        .arg(&config)
        .status()
        .context("cannot run Mihomo for geosite validation")?;
    ensure!(
        result.success(),
        "Mihomo rejected the geosite database or a required category is missing; existing database unchanged"
    );
    let staged = storage::staged_file(database, bytes)?;
    if database.exists() {
        storage::atomic_write(&database.with_extension("dat.bak"), &fs::read(database)?)?;
    }
    storage::commit(staged, database)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        SiteList {
            entries: vec![Site {
                code: "TEST".into(),
                domains: vec![Domain {
                    kind: 2,
                    value: "example.org".into(),
                }],
            }],
        }
        .encode_to_vec()
    }
    #[test]
    #[ignore = "requires an installed Mihomo and MINOHOMO_TEST_GEOSITE pointing to a real database"]
    fn real_database_passes_mihomo_validation_and_replacement() {
        let source = std::env::var("MINOHOMO_TEST_GEOSITE").expect("set MINOHOMO_TEST_GEOSITE");
        let bytes = fs::read(source).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let settings = store.settings().unwrap();
        let database = temp.path().join("GeoSite.dat");
        install(&store, &settings, &database, &bytes).unwrap();
        status(&settings, &database).unwrap();
        install(&store, &settings, &database, &bytes).unwrap();
        assert_eq!(fs::read(&database).unwrap(), bytes);
        assert_eq!(fs::read(database.with_extension("dat.bak")).unwrap(), bytes);
    }
    #[test]
    fn protobuf_validation() {
        assert_eq!(inspect(&fixture()).unwrap().entries[0].code, "TEST");
        for bad in [b"<html>error</html>".as_slice(), b"", b"random junk"] {
            assert!(inspect(bad).is_err());
        }
        assert!(inspect(&SiteList { entries: vec![] }.encode_to_vec()).is_err());
    }
    #[test]
    fn category_extraction_includes_dns_and_nested_rules() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let settings = store.settings().unwrap();
        fs::write(
            store.root.join("rules.yaml"),
            "rules:\n - AND,((GEOSITE,custom),(NETWORK,TCP)),DIRECT\n - MATCH,PROXY\n",
        )
        .unwrap();
        let tags = required_tags(&store, &settings, "fallback").unwrap();
        for tag in ["custom", "cn", "alibaba", "tencent"] {
            assert!(tags.contains(tag));
        }
    }
    #[test]
    fn alternate_url_and_path_are_configurable() {
        let mut settings: Settings =
            serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
        settings.geosite_url = "https://mirror.example/geosite.dat".into();
        settings.output = PathBuf::from("/custom/mihomo/config.yaml");
        assert_eq!(
            path(&settings).unwrap(),
            PathBuf::from("/custom/mihomo/GeoSite.dat")
        );
    }
    #[test]
    fn invalid_download_never_replaces_database() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let settings = store.settings().unwrap();
        let database = temp.path().join("GeoSite.dat");
        fs::write(&database, fixture()).unwrap();
        let before = fs::read(&database).unwrap();
        assert!(install(&store, &settings, &database, b"<html>error</html>").is_err());
        assert_eq!(fs::read(&database).unwrap(), before);
    }
}
