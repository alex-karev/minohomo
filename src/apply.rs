use crate::storage;
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    process::Command,
};

const MAX_CONFIG: u64 = 16 * 1024 * 1024;
const MAX_DATABASE: u64 = 64 * 1024 * 1024;

pub fn run(
    config: &Path,
    geosite: Option<&Path>,
    version_url: &str,
    download_url: &str,
) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "apply requires root; run it with sudo"
    );
    // Check input paths before installing the core.
    read_bounded(config, MAX_CONFIG)?;
    if let Some(path) = geosite {
        read_bounded(path, MAX_DATABASE)?;
    }
    crate::core::ensure_root(version_url, download_url)?;
    // Deliberately fixed destinations and commands. Never load user config.yaml as root.
    install(
        config,
        geosite,
        Path::new(crate::core::DIRECTORY),
        Path::new(crate::core::BINARY),
    )?;
    let status = Command::new("/usr/bin/systemctl")
        .args(["restart", "minohomo.service"])
        .status()
        .context("config installed, but cannot restart minohomo.service")?;
    ensure!(
        status.success(),
        "config installed, but minohomo.service restart failed; check 'systemctl status minohomo.service'"
    );
    println!("Installed /var/lib/minohomo/config.yaml and restarted minohomo.service.");
    Ok(())
}

pub fn service(action: &str) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "service control requires root; run it with sudo"
    );
    ensure!(
        ["start", "stop", "restart"].contains(&action),
        "unsupported service action"
    );
    crate::core::ensure_root(crate::core::VERSION_URL, crate::core::DOWNLOAD_URL)?;
    let status = Command::new("/usr/bin/systemctl")
        .args([action, "minohomo.service"])
        .status()
        .context("cannot run systemctl")?;
    ensure!(
        status.success(),
        "systemctl {action} minohomo.service failed"
    );
    println!("Mihomo service {action} succeeded.");
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "{} must be a regular file",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{} exceeds size limit",
        path.display()
    );
    Ok(bytes)
}

fn install(
    config: &Path,
    geosite: Option<&Path>,
    directory: &Path,
    validator: &Path,
) -> Result<()> {
    // Snapshot inputs once. Install exactly the bytes that passed validation.
    let config = read_bounded(config, MAX_CONFIG)?;
    let database = geosite
        .map(|path| read_bounded(path, MAX_DATABASE))
        .transpose()?;
    let _lock = crate::core::runtime_lock(directory)?;
    let work = tempfile::tempdir_in(directory)?;
    let candidate = work.path().join("config.yaml");
    storage::atomic_write(&candidate, &config)?;
    for name in ["GeoSite.dat", "GeoIP.dat", "Country.mmdb", "ASN.mmdb"] {
        if directory.join(name).is_file() {
            fs::copy(directory.join(name), work.path().join(name))?;
        }
    }
    if let Some(bytes) = &database {
        storage::atomic_write(&work.path().join("GeoSite.dat"), bytes)?;
    }
    let status = Command::new(validator)
        .arg("-t")
        .arg("-d")
        .arg(work.path())
        .arg("-f")
        .arg(&candidate)
        .status()
        .context("cannot run system Mihomo validator")?;
    ensure!(
        status.success(),
        "Mihomo rejected the system config; installed config and database are unchanged"
    );
    let output = directory.join("config.yaml");
    if output.exists() {
        storage::atomic_write(&directory.join("config.yaml.bak"), &fs::read(&output)?)?;
    }
    // Include data downloaded by validation, if the system had no copy yet.
    for name in ["GeoSite.dat", "GeoIP.dat", "Country.mmdb", "ASN.mmdb"] {
        let staged = work.path().join(name);
        let destination = directory.join(name);
        if staged.is_file()
            && (!destination.exists() || (name == "GeoSite.dat" && database.is_some()))
        {
            if destination.exists() {
                storage::atomic_write(
                    &directory.join(format!("{name}.bak")),
                    &fs::read(&destination)?,
                )?;
            }
            storage::atomic_write(&destination, &fs::read(staged)?)?;
        }
    }
    storage::atomic_write(&output, &config)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(exit: u8) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let validator = temp.path().join("validator");
        fs::write(&validator, format!("#!/bin/sh\nexit {exit}\n")).unwrap();
        fs::set_permissions(&validator, fs::Permissions::from_mode(0o700)).unwrap();
        let system = temp.path().join("system");
        fs::create_dir(&system).unwrap();
        fs::set_permissions(&system, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(system.join("config.yaml"), "old").unwrap();
        fs::write(system.join("GeoSite.dat"), "old data").unwrap();
        fs::write(temp.path().join("current.yaml"), "new").unwrap();
        fs::write(temp.path().join("GeoSite.dat"), "new data").unwrap();
        (temp, validator, system)
    }

    #[test]
    fn installs_validated_inputs_and_keeps_backups() {
        let (temp, validator, system) = fixture(0);
        install(
            &temp.path().join("current.yaml"),
            Some(&temp.path().join("GeoSite.dat")),
            &system,
            &validator,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(system.join("config.yaml")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(system.join("config.yaml.bak")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(system.join("GeoSite.dat")).unwrap(),
            "new data"
        );
        assert_eq!(
            fs::read_to_string(system.join("GeoSite.dat.bak")).unwrap(),
            "old data"
        );
        assert_eq!(
            fs::metadata(system.join("config.yaml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn validation_failure_preserves_system_files() {
        let (temp, validator, system) = fixture(1);
        assert!(
            install(
                &temp.path().join("current.yaml"),
                Some(&temp.path().join("GeoSite.dat")),
                &system,
                &validator
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(system.join("config.yaml")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(system.join("GeoSite.dat")).unwrap(),
            "old data"
        );
        assert!(!system.join("config.yaml.bak").exists());
    }

    #[test]
    fn missing_input_preserves_system_config() {
        let (temp, validator, system) = fixture(0);
        assert!(install(&temp.path().join("missing"), None, &system, &validator).is_err());
        assert_eq!(
            fs::read_to_string(system.join("config.yaml")).unwrap(),
            "old"
        );
    }
}
