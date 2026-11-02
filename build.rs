use std::{
    env,
    error::Error,
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/mihomo-version");
    println!("cargo:rerun-if-changed=packaging/mihomo-sha256sums");
    if env::var_os("CARGO_FEATURE_BUNDLED_CORE").is_none() {
        return Ok(());
    }

    assert_eq!(
        env::var("CARGO_CFG_TARGET_OS")?,
        "linux",
        "core bundles require Linux"
    );
    let arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let arch = match arch.as_str() {
        // Package builds must run on baseline x86-64 CPUs, not just the build host.
        "x86_64" => "amd64-v1",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "armv7",
        "riscv64" => "riscv64",
        "s390x" => "s390x",
        "powerpc64" if env::var("CARGO_CFG_TARGET_ENDIAN")? == "little" => "ppc64le",
        "loongarch64" => "loong64-abi2",
        _ => panic!("unsupported bundled core architecture: {arch}"),
    };
    let version = include_str!("packaging/mihomo-version").trim();
    let name = format!("mihomo-linux-{arch}-{version}.gz");
    let digest = include_str!("packaging/mihomo-sha256sums")
        .lines()
        .find_map(|line| {
            let (digest, filename) = line.split_once("  ")?;
            (filename == name).then_some(digest)
        })
        .ok_or("bundled core has no pinned checksum")?;
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is missing")?);
    // Shared by debug/release builds and different build-script fingerprints.
    let bundle_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("manifest dir is missing")?)
            .join("target/bundled-core");
    let target = env::var("TARGET")?;
    let cache_dir = bundle_dir
        .join("cache")
        .join(target)
        .join(format!("{version}-{digest}"));
    fs::create_dir_all(&cache_dir)?;
    let cached = cache_dir.join("mihomo");
    if !cached.is_file() {
        let archive = out.join("mihomo.gz");
        let url = format!("https://github.com/MetaCubeX/mihomo/releases/download/{version}/{name}");
        println!("cargo:warning=Downloading bundled Mihomo {version} for {arch}");
        let status = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--retry",
                "2",
                "--connect-timeout",
                "15",
                "--max-time",
                "180",
                "--max-filesize",
                "134217728",
                "--output",
            ])
            .arg(&archive)
            .arg(url)
            .status()?;
        assert!(status.success(), "cannot download bundled Mihomo core");
        let mut check = Command::new("sha256sum")
            .args(["--check", "--status"])
            .current_dir(&out)
            .stdin(Stdio::piped())
            .spawn()?;
        writeln!(
            check.stdin.take().ok_or("checksum stdin is missing")?,
            "{digest}  mihomo.gz"
        )?;
        assert!(check.wait()?.success(), "bundled Mihomo checksum mismatch");
        let unpacked = cache_dir.join(format!("mihomo.{}.part", std::process::id()));
        let status = Command::new("gzip")
            .args(["--decompress", "--stdout"])
            .arg(&archive)
            .stdout(File::create(&unpacked)?)
            .status()?;
        assert!(status.success(), "cannot unpack bundled Mihomo core");
        fs::set_permissions(&unpacked, fs::Permissions::from_mode(0o755))?;
        fs::rename(&unpacked, &cached)?;
        fs::remove_file(&archive)?;
    }

    // cargo-deb treats target/release/<name> as a Cargo binary target.
    // Keep its generated asset outside that special prefix.
    let package_asset = bundle_dir.join("mihomo");
    let profile_asset = out
        .ancestors()
        .nth(3)
        .ok_or("invalid OUT_DIR")?
        .join("mihomo");
    for asset in [package_asset, profile_asset] {
        println!("cargo:rerun-if-changed={}", asset.display());
        stage(&cached, &asset)?;
    }
    Ok(())
}

fn stage(source: &std::path::Path, destination: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let bytes = fs::read(source)?;
    // Do not touch an unchanged output: that would keep Cargo's build script dirty.
    if fs::read(destination).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let staged = destination.with_extension(format!("{}.part", std::process::id()));
    fs::write(&staged, bytes)?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    fs::rename(staged, destination)?;
    Ok(())
}
