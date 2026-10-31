use crate::links::Proxy;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Settings {
    pub output: PathBuf,
    pub mihomo_binary: PathBuf,
    pub reload_command: Vec<String>,
    pub request_timeout_seconds: u64,
    #[serde(default = "default_geosite_url")]
    pub geosite_url: String,
    /// Top-level proxy fields applied after importing each source.
    #[serde(default)]
    pub proxy_overrides: Proxy,
    pub mihomo: Mapping,
}
fn default_geosite_url() -> String {
    "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/release/geosite.dat".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub rule_providers: Mapping,
    pub rules: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Subscription,
    Proxy,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: u64,
    pub kind: SourceKind,
    pub link: String,
    pub added_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub proxies: Vec<Proxy>,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub sources: Vec<Source>,
}

pub struct Store {
    pub root: PathBuf,
    _lock: File,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(
            root.is_absolute(),
            "config directory must be an absolute path"
        );
        fs::create_dir_all(root).with_context(|| format!("cannot create {}", root.display()))?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join(".lock"))?;
        use std::os::fd::AsRawFd;
        // Keep the descriptor alive for the duration of the command.
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        ensure!(rc == 0, "another minohomo command holds the config lock");
        let store = Self {
            root: root.to_owned(),
            _lock: lock,
        };
        store.seed("config.yaml", include_str!("../defaults/config.yaml"))?;
        store.seed("rules.yaml", include_str!("../defaults/rules.yaml"))?;
        if !store.root.join("sources.yaml").exists() {
            store.save(&State::default())?;
        }
        Ok(store)
    }
    fn seed(&self, name: &str, body: &str) -> Result<()> {
        let body = if body.contains("__MINOHOMO_API_SECRET__") {
            let mut random = [0u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut random)?;
            let secret: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            body.replace("__MINOHOMO_API_SECRET__", &secret)
        } else {
            body.to_owned()
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root.join(name))
        {
            Ok(mut file) => {
                file.write_all(body.as_bytes())?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
    pub fn state(&self) -> Result<State> {
        read_yaml(&self.root.join("sources.yaml"))
    }
    pub fn settings(&self) -> Result<Settings> {
        let s: Settings = read_yaml(&self.root.join("config.yaml"))?;
        ensure!(s.output.is_absolute(), "output must be an absolute path");
        ensure!(
            !s.reload_command.is_empty(),
            "reload-command cannot be empty"
        );
        ensure!(
            s.request_timeout_seconds > 0,
            "request-timeout-seconds must be positive"
        );
        ensure!(
            s.output != self.root.join("sources.yaml")
                && s.output != self.root.join("rules.yaml")
                && s.output != self.root.join("config.yaml"),
            "output must not overwrite minohomo's own files"
        );
        Ok(s)
    }
    pub fn policy(&self) -> Result<Policy> {
        let p: Policy = read_yaml(&self.root.join("rules.yaml"))?;
        ensure!(
            !p.rules.is_empty(),
            "rules.yaml must contain a nonempty rules list"
        );
        Ok(p)
    }
    pub fn save(&self, state: &State) -> Result<()> {
        atomic_write(
            &self.root.join("sources.yaml"),
            serde_yaml::to_string(state)?.as_bytes(),
        )
    }
}
pub fn read_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let mut body = String::new();
    File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?
        .read_to_string(&mut body)?;
    serde_yaml::from_str(&body).with_context(|| format!("invalid YAML in {}", path.display()))
}
pub fn staged_file(path: &Path, body: &[u8]) -> Result<NamedTempFile> {
    let parent = path.parent().context("file has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut file = NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(body)?;
    file.as_file().sync_all()?;
    Ok(file)
}
pub fn commit(file: NamedTempFile, path: &Path) -> Result<()> {
    file.persist(path).map_err(|e| e.error)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
pub fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    commit(staged_file(path, body)?, path)
}
pub fn key(s: &str) -> Value {
    Value::String(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_preserves_edits() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            assert!(s.state().unwrap().sources.is_empty());
            s.settings().unwrap();
            s.policy().unwrap();
        }
        fs::write(dir.path().join("rules.yaml"), "rules: [MATCH,REJECT]").unwrap();
        let _s = Store::open(dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("rules.yaml")).unwrap(),
            "rules: [MATCH,REJECT]"
        );
    }
    #[test]
    fn lock_prevents_concurrent_updates() {
        let dir = tempfile::tempdir().unwrap();
        let _s = Store::open(dir.path()).unwrap();
        assert!(Store::open(dir.path()).is_err());
    }
    #[test]
    fn private_atomic_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
