use crate::Settings;
use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use reqwest::blocking::Client;
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const DIRECTORY: &str = "/var/lib/minohomo";
pub const BINARY: &str = "/var/lib/minohomo/mihomo";
pub const BUNDLED_BINARY: &str = "/usr/share/minohomo/mihomo";
pub const SERVICE: &str = "minohomo.service";
pub const VERSION_URL: &str =
    "https://github.com/MetaCubeX/mihomo/releases/latest/download/version.txt";
pub const DOWNLOAD_URL: &str =
    "https://github.com/MetaCubeX/mihomo/releases/download/{version}/{artifact}-{version}.{ext}";
const UNIT: &str = "/usr/lib/systemd/system/minohomo.service";
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_BINARY: u64 = 256 * 1024 * 1024;

#[derive(Subcommand)]
pub enum Action {
    /// Show the installed managed core version, or "Not installed"
    Version,
    /// Download and install the latest core; restart the service only if running
    Update {
        /// Override core-version-url from config.yaml
        #[arg(long)]
        version_url: Option<String>,
        /// Archive URL template with {version}, {artifact}, and {ext}
        #[arg(long)]
        download_url: Option<String>,
    },
}

pub fn default_binary() -> PathBuf {
    BINARY.into()
}
pub fn default_version_url() -> String {
    VERSION_URL.into()
}
pub fn default_download_url() -> String {
    DOWNLOAD_URL.into()
}

pub fn version() -> Result<String> {
    version_at(Path::new(BINARY))
}
fn version_at(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok("Not installed".into());
    }
    let output = Command::new(path)
        .arg("-v")
        .output()
        .with_context(|| format!("cannot read core version from {}", path.display()))?;
    ensure!(
        output.status.success(),
        "core -v failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_version_output(&String::from_utf8(output.stdout).context("invalid core version output")?)
}
fn parse_version_output(output: &str) -> Result<String> {
    ensure!(
        output.to_ascii_lowercase().contains("mihomo"),
        "downloaded executable did not identify itself as Mihomo"
    );
    output
        .split_whitespace()
        .find(|word| {
            word.starts_with('v') && word.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
        })
        .map(str::to_owned)
        .context("Mihomo did not report a version")
}
fn release_version(body: &str) -> Result<String> {
    let version = body.trim();
    ensure!(
        version.len() <= 100
            && version.starts_with('v')
            && version.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
            && version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c)),
        "invalid version.txt; expected a release tag such as v1.19.33"
    );
    Ok(version.into())
}

fn artifact(os: &str, arch: &str, amd64_v2: bool) -> Result<(String, &'static str)> {
    let os = match os {
        "macos" => "darwin",
        "linux" | "windows" | "freebsd" => os,
        _ => bail!("unsupported core OS: {os}"),
    };
    let arch = match arch {
        "x86_64" => {
            if amd64_v2 {
                "amd64-v2"
            } else {
                "amd64-v1"
            }
        }
        "x86" => "386",
        "aarch64" => "arm64",
        "arm" if os == "linux" => "armv7",
        "riscv64" | "s390x" if os == "linux" => arch,
        "powerpc64" if os == "linux" && cfg!(target_endian = "little") => "ppc64le",
        "loongarch64" if os == "linux" => "loong64-abi2",
        _ => bail!("unsupported core architecture: {os}/{arch}"),
    };
    Ok((
        format!("mihomo-{os}-{arch}"),
        if os == "windows" { "zip" } else { "gz" },
    ))
}
fn supports_amd64_v2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // Go's amd64 v2 requires these in addition to the x86-64 baseline.
        let leaf = std::arch::x86_64::__cpuid(1);
        let extended = std::arch::x86_64::__cpuid(0x80000001);
        let required = (1 << 0) | (1 << 9) | (1 << 13) | (1 << 19) | (1 << 20) | (1 << 23);
        leaf.ecx & required == required && extended.ecx & 1 != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
fn platform_artifact() -> Result<(String, &'static str)> {
    artifact(
        std::env::consts::OS,
        std::env::consts::ARCH,
        supports_amd64_v2(),
    )
}
fn archive_url(template: &str, version: &str, artifact: &str, ext: &str) -> Result<String> {
    let url = template
        .replace("{version}", version)
        .replace("{artifact}", artifact)
        .replace("{ext}", ext);
    ensure!(
        !url.contains(['{', '}']),
        "unknown placeholder in core-download-url"
    );
    check_url(&url)?;
    Ok(url)
}
fn check_url(url: &str) -> Result<()> {
    let url = url::Url::parse(url).context("invalid core download URL")?;
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
        "core downloads require HTTPS (HTTP is allowed only on loopback)"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "core URLs must not contain credentials"
    );
    Ok(())
}
fn response(client: &Client, url: &str) -> Result<reqwest::blocking::Response> {
    check_url(url)?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| error.without_url())?
        .error_for_status()
        .map_err(|error| error.without_url())?;
    check_url(response.url().as_str())?;
    Ok(response)
}
fn copy_bounded(input: impl Read, output: &mut impl Write, limit: u64) -> Result<()> {
    let count = std::io::copy(&mut input.take(limit + 1), output)?;
    ensure!(
        count > 0 && count <= limit,
        "empty file or download exceeds size limit"
    );
    Ok(())
}
fn unpack(archive: &mut File, ext: &str, output: &mut File) -> Result<()> {
    archive.rewind()?;
    match ext {
        "gz" => copy_bounded(flate2::read::GzDecoder::new(archive), output, MAX_BINARY),
        "zip" => {
            let mut archive = zip::ZipArchive::new(archive)?;
            // Read only the executable; never extract paths supplied by the archive.
            let mut found = None;
            for index in 0..archive.len() {
                let file = archive.by_index(index)?;
                let name = file.name().rsplit('/').next().unwrap_or("");
                let name = name.to_ascii_lowercase();
                if !file.is_dir()
                    && (name == "mihomo.exe"
                        || (name.starts_with("mihomo-") && name.ends_with(".exe")))
                {
                    ensure!(
                        found.is_none(),
                        "archive contains more than one core executable"
                    );
                    found = Some(index);
                }
            }
            let file = archive.by_index(found.context("archive has no Mihomo executable")?)?;
            copy_bounded(file, output, MAX_BINARY)
        }
        _ => bail!("unsupported core archive: {ext}"),
    }
}

pub fn ensure_local(settings: &Settings) -> Result<()> {
    // Retain explicit local-validator overrides for custom reload workflows.
    if settings.mihomo_binary != Path::new(BINARY) {
        ensure!(
            settings.mihomo_binary.is_file(),
            "custom Mihomo validator is missing: {}",
            settings.mihomo_binary.display()
        );
        return Ok(());
    }
    if !Path::new(BINARY).is_file() {
        println!("Managed core is not installed; installing it first...");
        ensure_root(&settings.core_version_url, &settings.core_download_url)?;
    }
    Ok(())
}
pub fn update(version_url: &str, download_url: &str) -> Result<()> {
    update_impl(version_url, download_url).with_context(|| manual_help(version_url, download_url))
}

pub fn ensure_root(version_url: &str, download_url: &str) -> Result<()> {
    if !Path::new(BINARY).is_file() {
        if cfg!(feature = "bundled-core") && Path::new(BUNDLED_BINARY).is_file() {
            install_bundled()?;
        } else {
            println!("No local Mihomo core is available; downloading it first...");
            update(version_url, download_url)?;
        }
    }
    ensure_service()
}

fn install_bundled() -> Result<()> {
    ensure!(
        Path::new(BUNDLED_BINARY).is_file(),
        "bundled Mihomo core disappeared from {BUNDLED_BINARY}; retry or run 'minohomo core update'"
    );
    let version = version_at(Path::new(BUNDLED_BINARY))
        .context("bundled Mihomo core cannot run; reinstall the matching architecture package or run 'minohomo core update'")?;
    privileged(
        "/usr/bin/install",
        &[
            OsStr::new("-D"),
            OsStr::new("-m"),
            OsStr::new("755"),
            OsStr::new("-o"),
            OsStr::new("root"),
            OsStr::new("-g"),
            OsStr::new("root"),
            OsStr::new("--"),
            OsStr::new(BUNDLED_BINARY),
            OsStr::new(BINARY),
        ],
    )
    .context("cannot copy bundled Mihomo core; retry with sudo authorization, reinstall the package, or run 'minohomo core update'")?;
    println!("Installed bundled Mihomo {version} at {BINARY}.");
    Ok(())
}

pub struct RuntimeLock(File);
impl Drop for RuntimeLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
pub fn runtime_lock(directory: &Path) -> Result<RuntimeLock> {
    fs::create_dir_all(directory)?;
    let meta = fs::symlink_metadata(directory)?;
    ensure!(
        meta.is_dir() && meta.mode() & 0o022 == 0,
        "runtime directory must be a real directory, not writable by group/others"
    );
    if unsafe { libc::geteuid() } == 0 {
        ensure!(meta.uid() == 0, "runtime directory must belong to root");
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join(".runtime.lock"))?;
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another minohomo command holds the runtime lock"
    );
    Ok(RuntimeLock(file))
}
fn privileged(program: &str, args: &[&OsStr]) -> Result<()> {
    let mut command = if unsafe { libc::geteuid() } == 0 {
        Command::new(program)
    } else {
        let mut command = Command::new("sudo");
        command.arg("--").arg(program);
        command
    };
    let status = command
        .args(args)
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    ensure!(status.success(), "{program} failed ({status})");
    Ok(())
}
fn move_binary(
    source: &Path,
    directory: &Path,
    mut run: impl FnMut(&str, &[&OsStr]) -> Result<()>,
) -> Result<()> {
    let destination = directory.join("mihomo");
    run(
        "/usr/bin/install",
        &[
            OsStr::new("-d"),
            OsStr::new("-m"),
            OsStr::new("755"),
            directory.as_os_str(),
        ],
    )?;
    run(
        "/usr/bin/mv",
        &[
            OsStr::new("--"),
            source.as_os_str(),
            destination.as_os_str(),
        ],
    )?;
    // mv preserves the source owner/mode. Do not leave a user-owned executable
    // behind for the root system service to execute.
    run(
        "/usr/bin/chown",
        &[OsStr::new("root:root"), destination.as_os_str()],
    )?;
    run(
        "/usr/bin/chmod",
        &[OsStr::new("755"), destination.as_os_str()],
    )
}
pub fn ensure_service() -> Result<()> {
    if !Path::new(UNIT).exists() {
        let mut unit = tempfile::NamedTempFile::new_in("/tmp")?;
        unit.write_all(include_bytes!("../packaging/minohomo.service"))?;
        unit.as_file().sync_all()?;
        privileged(
            "/usr/bin/install",
            &[
                OsStr::new("-D"),
                OsStr::new("-m"),
                OsStr::new("644"),
                unit.path().as_os_str(),
                OsStr::new(UNIT),
            ],
        )?;
        privileged("/usr/bin/systemctl", &[OsStr::new("daemon-reload")])?;
    }
    Ok(())
}
fn update_impl(version_url: &str, download_url: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("minohomo/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if let Err(error) = check_url(attempt.url().as_str()) {
                attempt.error(error.to_string())
            } else {
                attempt.follow()
            }
        }))
        .build()?;
    let mut body = String::new();
    response(&client, version_url)?
        .take(1025)
        .read_to_string(&mut body)?;
    ensure!(body.len() <= 1024, "version.txt exceeds 1 KiB");
    let version = release_version(&body)?;
    let (artifact, ext) = platform_artifact()?;
    let url = archive_url(download_url, &version, &artifact, ext)?;
    println!("Downloading {artifact}-{version}.{ext}...");
    // Force /tmp even if TMPDIR is set. Private temporary files are removed on
    // success and on every error path by TempDir's destructor.
    let work = tempfile::Builder::new()
        .prefix("minohomo-core-")
        .tempdir_in("/tmp")?;
    let mut archive = tempfile::tempfile_in(work.path())?;
    let response = response(&client, &url)?;
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= MAX_ARCHIVE),
        "core archive exceeds 128 MiB"
    );
    copy_bounded(response, &mut archive, MAX_ARCHIVE)?;
    let binary = work.path().join("mihomo");
    {
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&binary)?;
        unpack(&mut archive, ext, &mut output).context("cannot unpack core archive")?;
        output.sync_all()?;
    }
    let reported = version_at(&binary).context("downloaded core cannot run on this machine")?;
    ensure!(
        reported == version,
        "downloaded core reports {reported}, expected {version}"
    );
    move_binary(&binary, Path::new(DIRECTORY), privileged)?;
    ensure_service()?;
    println!("Installed Mihomo {version} at {BINARY}.");
    if Command::new("/usr/bin/systemctl")
        .args(["is-active", "--quiet", SERVICE])
        .status()?
        .success()
    {
        privileged(
            "/usr/bin/systemctl",
            &[OsStr::new("restart"), OsStr::new(SERVICE)],
        )
        .with_context(|| {
            format!("core installed, but restart failed; check systemctl status {SERVICE}")
        })?;
        println!("Restarted {SERVICE}.");
    }
    Ok(())
}
fn manual_help(version_url: &str, download_url: &str) -> String {
    let asset = platform_artifact()
        .map(|(name, ext)| format!("{name}-<VERSION>.{ext}"))
        .unwrap_or_else(|_| "the archive matching your OS/architecture".into());
    format!(
        "Core update failed. Manual recovery:\n\
        1. Read the release tag from {version_url}\n\
        2. Download {asset} from https://github.com/MetaCubeX/mihomo/releases\n\
           Custom archive template: {download_url}\n\
        3. Unpack the archive, then install the extracted executable:\n\
           sudo install -d -m 755 {DIRECTORY}\n\
           sudo mv -- /path/to/extracted/mihomo {BINARY}\n\
           sudo chown root:root {BINARY}\n\
           sudo chmod 755 {BINARY}\n\
        4. Run minohomo update --no-sync to validate and install config.yaml, geodata, and the service.\n\
           If already configured, sudo systemctl restart {SERVICE}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_release_artifacts_and_versions() {
        assert_eq!(
            artifact("linux", "x86_64", true).unwrap(),
            ("mihomo-linux-amd64-v2".into(), "gz")
        );
        assert_eq!(
            artifact("linux", "x86_64", false).unwrap().0,
            "mihomo-linux-amd64-v1"
        );
        assert_eq!(
            artifact("windows", "aarch64", false).unwrap(),
            ("mihomo-windows-arm64".into(), "zip")
        );
        assert_eq!(
            artifact("macos", "aarch64", false).unwrap().0,
            "mihomo-darwin-arm64"
        );
        assert!(artifact("unknown", "aarch64", false).is_err());
        assert!(artifact("linux", "unknown", false).is_err());
        assert_eq!(release_version("v1.19.33\n").unwrap(), "v1.19.33");
        for bad in ["", "latest", "v../../tmp", "v1\nx", "<html>"] {
            assert!(release_version(bad).is_err());
        }
        assert_eq!(
            parse_version_output("Mihomo Meta v1.19.33 linux amd64").unwrap(),
            "v1.19.33"
        );
        assert!(parse_version_output("not a core v1.0").is_err());
    }
    #[test]
    fn custom_urls_and_manual_recovery() {
        assert_eq!(
            archive_url(
                "https://mirror.test/{version}/{artifact}.{ext}",
                "v1.2.3",
                "mihomo-linux-arm64",
                "gz"
            )
            .unwrap(),
            "https://mirror.test/v1.2.3/mihomo-linux-arm64.gz"
        );
        assert!(archive_url("https://mirror.test/{bad}", "v1", "x", "gz").is_err());
        assert!(check_url("http://example.com/file").is_err());
        assert!(check_url("file:///tmp/core").is_err());
        assert!(manual_help(VERSION_URL, DOWNLOAD_URL).contains("sudo chmod 755"));
        assert!(manual_help(VERSION_URL, DOWNLOAD_URL).contains(BINARY));
    }
    #[test]
    fn unpacks_gzip_and_rejects_bad_or_oversized_input() {
        let mut archive = tempfile::tempfile().unwrap();
        {
            let mut gzip =
                flate2::write::GzEncoder::new(&mut archive, flate2::Compression::default());
            gzip.write_all(b"core executable").unwrap();
            gzip.finish().unwrap();
        }
        let mut output = tempfile::tempfile().unwrap();
        unpack(&mut archive, "gz", &mut output).unwrap();
        output.rewind().unwrap();
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"core executable");
        assert!(copy_bounded(&b"12345"[..], &mut Vec::new(), 4).is_err());
        assert!(copy_bounded(&b""[..], &mut Vec::new(), 4).is_err());
        let mut bad = tempfile::tempfile().unwrap();
        bad.write_all(b"not gzip").unwrap();
        assert!(unpack(&mut bad, "gz", &mut output).is_err());
    }
    #[test]
    fn reads_upstream_windows_zip_name_without_extracting_paths() {
        let mut archive = tempfile::tempfile().unwrap();
        {
            let mut zip = zip::ZipWriter::new(&mut archive);
            zip.start_file("readme.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"ignore this").unwrap();
            zip.start_file(
                "nested/mihomo-windows-arm64.exe",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"windows executable").unwrap();
            zip.finish().unwrap();
        }
        let mut output = tempfile::tempfile().unwrap();
        unpack(&mut archive, "zip", &mut output).unwrap();
        output.rewind().unwrap();
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"windows executable");
    }
    #[test]
    fn moves_core_with_sudo_compatible_commands() {
        let mut commands = Vec::new();
        move_binary(
            Path::new("/tmp/core file"),
            Path::new(DIRECTORY),
            |program, args| {
                commands.push((
                    program.to_owned(),
                    args.iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                ));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            commands
                .iter()
                .map(|(program, _)| program.as_str())
                .collect::<Vec<_>>(),
            [
                "/usr/bin/install",
                "/usr/bin/mv",
                "/usr/bin/chown",
                "/usr/bin/chmod"
            ]
        );
        assert_eq!(commands[1].1, ["--", "/tmp/core file", BINARY]);
        assert_eq!(commands[2].1, ["root:root", BINARY]);
        let mut attempts = 0;
        assert!(
            move_binary(Path::new("/tmp/core"), Path::new(DIRECTORY), |_, _| {
                attempts += 1;
                bail!("sudo cancelled")
            })
            .is_err()
        );
        assert_eq!(attempts, 1);
    }
    #[test]
    fn runtime_lock_releases_inherited_descriptors_and_missing_core_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let lock = runtime_lock(temp.path()).unwrap();
        assert!(runtime_lock(temp.path()).is_err());
        let inherited = lock.0.try_clone().unwrap();
        drop(lock);
        let _reopened = runtime_lock(temp.path()).unwrap();
        drop(inherited);
        assert_eq!(
            version_at(&temp.path().join("absent")).unwrap(),
            "Not installed"
        );
    }
}
