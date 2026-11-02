use crate::storage::Store;
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn executable(program: &str) -> Option<PathBuf> {
    let candidates = if program.contains('/') {
        vec![PathBuf::from(program)]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(program))
            .collect()
    };
    candidates
        .into_iter()
        .find(|path| {
            path.is_file()
                && path
                    .metadata()
                    .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
        })
        .and_then(|path| std::path::absolute(path).ok())
}

fn terminal_command(configured: Option<&str>) -> Result<Vec<OsString>> {
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    let mut words = if let Some(configured) = configured {
        shell_words::split(configured).context("invalid quoting in default_terminal")?
    } else if executable("xdg-terminal-exec").is_some() {
        vec!["xdg-terminal-exec".into()]
    } else if executable("x-terminal-emulator").is_some() {
        vec!["x-terminal-emulator".into()]
    } else if let Ok(terminal) = std::env::var("TERMINAL") {
        shell_words::split(&terminal).context("invalid quoting in TERMINAL")?
    } else {
        bail!("no system default terminal found; set default_terminal in config.yaml");
    };
    let program = words
        .first()
        .context("default_terminal must name an executable")?;
    let path = executable(program).with_context(|| {
        format!("terminal executable '{program}' was not found or is not executable")
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !words
        .last()
        .is_some_and(|arg| ["--", "-e", "-x", "--execute", "--command"].contains(&arg.as_str()))
    {
        match name {
            "xdg-terminal-exec" | "gnome-terminal" | "kgx" | "ptyxis" | "kitty" | "foot"
            | "footclient" => words.push("--".into()),
            "xfce4-terminal" => words.push("--execute".into()),
            "terminator" => words.push("-x".into()),
            "wezterm" => {
                if !words.iter().any(|arg| arg == "start") {
                    words.push("start".into());
                }
                words.push("--".into());
            }
            _ => words.push("-e".into()),
        }
    }
    let mut args = vec![path.into_os_string()];
    args.extend(words.into_iter().skip(1).map(OsString::from));
    Ok(args)
}

pub fn open_dashboard(root: &Path) -> Result<()> {
    // Reload on each click, so editing the terminal preference needs no tray restart.
    let settings = Store::load_settings(root)?;
    let mut args = terminal_command(settings.default_terminal.as_deref())?;
    args.extend([
        std::env::current_exe()
            .context("cannot locate minohomo executable")?
            .into_os_string(),
        OsString::from("--config-dir"),
        root.as_os_str().to_owned(),
    ]);
    // A separate transient user service keeps the terminal/TUI alive if the user
    // disables the tray from that TUI. Do not leave it in the tray service cgroup.
    let mut command = Command::new("/usr/bin/systemd-run");
    command.args([
        "--user",
        "--collect",
        "--quiet",
        "--property=Type=exec",
        "--description=Minohomo terminal dashboard",
    ]);
    for key in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "XDG_CURRENT_DESKTOP",
        "DESKTOP_SESSION",
        "PATH",
    ] {
        if let Some(value) = std::env::var_os(key) {
            let mut assignment = OsString::from(format!("{key}="));
            assignment.push(value);
            command.arg("--setenv").arg(assignment);
        }
    }
    let output = command
        .arg("--")
        .args(args)
        .output()
        .context("cannot launch terminal through systemd-run --user")?;
    ensure!(
        output.status.success(),
        "cannot open dashboard: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
