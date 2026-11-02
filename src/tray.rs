use crate::{
    api::{Controller, Mode},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use ksni::{blocking::TrayMethods, menu::*};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::Duration,
};

const SERVICE: &str = "minohomo-tray.service";
const WATCHER: &str = "minohomo-tray-config.path";

#[derive(Subcommand)]
pub enum Action {
    /// Run the tray in the foreground (normally started by systemd)
    Run,
    /// Enable or disable the default profile's tray service from config.yaml
    Sync,
    /// Enable the tray in config.yaml and synchronize the user service
    Enable,
    /// Disable the tray in config.yaml and synchronize the user service
    Disable,
}

pub fn run(root: &Path, action: &Action) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "run the tray as your desktop user, not root"
    );
    match action {
        Action::Sync => sync(root),
        Action::Enable | Action::Disable => {
            let enabled = matches!(action, Action::Enable);
            {
                let store = Store::open(root)?;
                store.settings()?;
                let path = root.join("config.yaml");
                let body = std::fs::read_to_string(&path)?;
                crate::storage::atomic_write(
                    &path,
                    rewrite_preference(&body, enabled)?.as_bytes(),
                )?;
            }
            println!(
                "Tray preference saved: {}",
                if enabled { "enabled" } else { "disabled" }
            );
            if root == crate::default_config_dir()? {
                sync(root).context("preference saved, but tray service synchronization failed")?;
            } else {
                println!(
                    "Custom profile: automatic service management uses the default profile only."
                );
            }
            Ok(())
        }
        Action::Run => {
            // Initialize once, then release the CLI lock before the long-running loop.
            let settings = if root.join("config.yaml").exists() {
                Store::load_settings(root)?
            } else {
                Store::open(root)?.settings()?
            };
            if !settings.tray_enabled {
                return Ok(());
            }
            serve(root)
        }
    }
}

/// Preserve comments in normal block YAML. Validate the edit against the parsed
/// mapping before writing; unusual YAML syntax falls back to serialization.
fn rewrite_preference(body: &str, enabled: bool) -> Result<String> {
    let mut expected: serde_yaml::Value = serde_yaml::from_str(body)?;
    expected
        .as_mapping_mut()
        .context("config.yaml must be a mapping")?
        .insert(crate::storage::key("tray-enabled"), enabled.into());
    let mut replaced = false;
    let mut edited = String::new();
    for line in body.split_inclusive('\n') {
        if !replaced
            && ["tray-enabled:", "'tray-enabled':", "\"tray-enabled\":"]
                .iter()
                .any(|key| line.starts_with(key))
        {
            let comment = line
                .find('#')
                .map(|index| format!(" {}", line[index..].trim_end()))
                .unwrap_or_default();
            edited.push_str(&format!("tray-enabled: {enabled}{comment}\n"));
            replaced = true;
        } else {
            edited.push_str(line);
        }
    }
    if !replaced {
        edited.push_str(&format!("\ntray-enabled: {enabled}\n"));
    }
    if serde_yaml::from_str::<serde_yaml::Value>(&edited).is_ok_and(|value| value == expected) {
        Ok(edited)
    } else {
        Ok(serde_yaml::to_string(&expected)?)
    }
}

fn user_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("/usr/bin/systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("cannot run systemctl --user")?;
    ensure!(
        output.status.success(),
        "systemctl --user {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn user_unit_state(query: &str, unit: &str) -> bool {
    Command::new("/usr/bin/systemctl")
        .args(["--user", "--quiet", query, unit])
        .status()
        .is_ok_and(|status| status.success())
}

/// The path unit remains enabled even when the icon is disabled, so editing the
/// preference back to true can start the tray without another CLI invocation.
pub fn sync(root: &Path) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "tray services belong to the desktop user, not root"
    );
    ensure!(
        root == crate::default_config_dir()?,
        "automatic tray service management uses ~/.config/minohomo; use tray run for custom profiles"
    );
    let enabled = Store::load_settings(root)?.tray_enabled;
    sync_units(enabled, user_unit_state, user_systemctl)
}

fn sync_units(
    enabled: bool,
    state: impl Fn(&str, &str) -> bool,
    mut action: impl FnMut(&[&str]) -> Result<()>,
) -> Result<()> {
    if !state("is-enabled", WATCHER) || !state("is-active", WATCHER) {
        action(&["enable", "--now", WATCHER])?;
    }
    if enabled {
        if !state("is-enabled", SERVICE) || !state("is-active", SERVICE) {
            action(&["enable", "--now", SERVICE])?;
        }
    } else if state("is-enabled", SERVICE) || state("is-active", SERVICE) {
        action(&["disable", "--now", SERVICE])?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum Request {
    Mode(Mode),
    Proxy { group: String, name: String },
    Service(&'static str),
    OpenDashboard,
}

#[derive(Default)]
struct Snapshot {
    service: String,
    mode: String,
    group: String,
    current: String,
    proxies: Vec<String>,
    api_error: Option<String>,
}

pub(crate) fn service_state() -> String {
    match Command::new("/usr/bin/systemctl")
        .args([
            "show",
            "minohomo.service",
            "--property=ActiveState",
            "--value",
        ])
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        _ => "unknown".into(),
    }
}

fn snapshot(root: &Path) -> Snapshot {
    let mut status = Snapshot {
        service: service_state(),
        ..Default::default()
    };
    let result = (|| -> Result<()> {
        let controller = Controller::for_tray(&Store::load_settings(root)?)?;
        status.mode = controller.current_mode()?;
        status.group = if status.mode.eq_ignore_ascii_case("global") {
            "GLOBAL"
        } else {
            "PROXY"
        }
        .into();
        let selector = controller.selector_group(&status.group)?;
        status.current = selector["now"].as_str().unwrap_or("unknown").into();
        status.proxies = selector["all"]
            .as_array()
            .context("proxy group has no choices")?
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        Ok(())
    })();
    if let Err(error) = result {
        status.api_error = Some(format!("{error:#}"));
    }
    status
}

fn execute(root: &Path, request: Request) -> Result<()> {
    match request {
        Request::OpenDashboard => crate::terminal::open_dashboard(root),
        Request::Mode(mode) => Controller::for_tray(&Store::load_settings(root)?)?.mode(mode),
        Request::Proxy { group, name } => {
            Controller::for_tray(&Store::load_settings(root)?)?.switch_group(&group, &name)
        }
        Request::Service(action) => {
            // systemctl uses the desktop polkit agent. Never launch terminal sudo
            // from a user service, nor grant passwordless blanket root access.
            ensure!(
                ["start", "stop", "restart"].contains(&action),
                "unsupported service action"
            );
            let output =
                if cfg!(feature = "bundled-core") && !Path::new(crate::core::BINARY).is_file() {
                    // First use also needs to copy the packaged core. Let polkit
                    // authorize the same root CLI path used by terminal controls.
                    Command::new("/usr/bin/pkexec")
                        .arg(std::env::current_exe().context("cannot locate minohomo executable")?)
                        .arg(action)
                        .output()
                } else {
                    Command::new("/usr/bin/systemctl")
                        .args(["--system", action, "minohomo.service"])
                        .output()
                }
                .context(
                    "cannot control minohomo.service; first-use tray setup requires polkit/pkexec",
                )?;
            ensure!(
                output.status.success(),
                "Mihomo {action} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            Ok(())
        }
    }
}

struct Tray {
    status: Snapshot,
    error: Option<String>,
    tx: mpsc::Sender<Request>,
    icon: ksni::Icon,
}

fn label(text: String) -> ksni::MenuItem<Tray> {
    StandardItem {
        label: text,
        enabled: false,
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "minohomo".into()
    }
    fn title(&self) -> String {
        format!(
            "Mihomo: {} | {} | {}",
            self.status.service, self.status.mode, self.status.current
        )
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Minohomo".into(),
            description: self.title(),
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let ready = self.status.api_error.is_none();
        let modes = [Mode::Rule, Mode::Global, Mode::Direct];
        let names = ["rule", "global", "direct"];
        let mut items = vec![
            label(format!(
                "Mihomo: {}",
                match self.status.service.as_str() {
                    "active" => "Enabled (running)",
                    "inactive" => "Disabled (stopped)",
                    other => other,
                }
            )),
            label(format!(
                "Mode: {}",
                if self.status.mode.is_empty() {
                    "unknown"
                } else {
                    &self.status.mode
                }
            )),
            label(format!(
                "{}: {}",
                if self.status.group.is_empty() {
                    "Proxy"
                } else {
                    &self.status.group
                },
                if self.status.current.is_empty() {
                    "unknown"
                } else {
                    &self.status.current
                }
            )),
            SubMenu {
                label: "Mode".into(),
                enabled: ready,
                submenu: vec![
                    RadioGroup {
                        selected: names
                            .iter()
                            .position(|name| self.status.mode.eq_ignore_ascii_case(name))
                            .unwrap_or(usize::MAX),
                        options: names
                            .iter()
                            .map(|name| RadioItem {
                                label: (*name).into(),
                                ..Default::default()
                            })
                            .collect(),
                        select: Box::new(move |tray: &mut Self, index| {
                            if let Some(mode) = modes.get(index) {
                                let _ = tray.tx.send(Request::Mode(*mode));
                            }
                        }),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Proxy".into(),
                enabled: ready && !self.status.proxies.is_empty(),
                submenu: vec![
                    RadioGroup {
                        selected: self
                            .status
                            .proxies
                            .iter()
                            .position(|name| name == &self.status.current)
                            .unwrap_or(usize::MAX),
                        options: self
                            .status
                            .proxies
                            .iter()
                            .map(|name| RadioItem {
                                label: name.clone(),
                                ..Default::default()
                            })
                            .collect(),
                        select: Box::new(|tray: &mut Self, index| {
                            if let Some(name) = tray.status.proxies.get(index) {
                                let _ = tray.tx.send(Request::Proxy {
                                    group: tray.status.group.clone(),
                                    name: name.clone(),
                                });
                            }
                        }),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
        ];
        for (text, action) in [
            ("Enable Mihomo", "start"),
            ("Disable Mihomo", "stop"),
            ("Restart Mihomo", "restart"),
        ] {
            items.push(
                StandardItem {
                    label: text.into(),
                    activate: Box::new(move |tray: &mut Self| {
                        let _ = tray.tx.send(Request::Service(action));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            StandardItem {
                label: "Open dashboard".into(),
                icon_name: "utilities-terminal".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Request::OpenDashboard);
                }),
                ..Default::default()
            }
            .into(),
        );
        if let Some(error) = &self.status.api_error {
            items.push(label(format!("API unavailable: {error}")));
        }
        if let Some(error) = &self.error {
            items.push(label(format!("Last action failed: {error}")));
        }
        items
    }
}

fn icon() -> Result<ksni::Icon> {
    let mut decoder = png::Decoder::new(Cursor::new(include_bytes!("assets/tray.png")));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer)?;
    ensure!(
        info.color_type == png::ColorType::Rgba,
        "tray icon must be RGBA"
    );
    buffer.truncate(info.buffer_size());
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.rotate_right(1);
    }
    Ok(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: buffer,
    })
}

fn serve(root: &Path) -> Result<()> {
    let root: PathBuf = root.into();
    let (tx, rx) = mpsc::channel();
    let handle = Tray {
        status: Snapshot::default(),
        error: None,
        tx,
        icon: icon()?,
    }
    .assume_sni_available(true)
    .spawn()
    .context("cannot start StatusNotifier tray on the session bus")?;
    let mut error = None;
    loop {
        // No persistent Store lock, and no network or service commands in D-Bus callbacks.
        if !Store::load_settings(&root)?.tray_enabled {
            handle.shutdown().wait();
            return Ok(());
        }
        let status = snapshot(&root);
        handle.update(|tray| {
            tray.status = status;
            tray.error = error.clone();
        });
        if handle.is_closed() {
            return Ok(());
        }
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(request) => {
                error = execute(&root, request)
                    .err()
                    .map(|error| format!("{error:#}"));
                if let Some(error) = &error {
                    eprintln!("tray action failed: {error}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_icon_is_argb() {
        let icon = icon().unwrap();
        let mut reader = png::Decoder::new(Cursor::new(include_bytes!("assets/tray.png")))
            .read_info()
            .unwrap();
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).unwrap();
        assert_eq!(
            (icon.width, icon.height),
            (info.width as i32, info.height as i32)
        );
        assert_eq!(icon.data.len(), info.buffer_size());
        for (argb, rgba) in icon
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(rgba.as_chunks::<4>().0)
        {
            assert_eq!(*argb, [rgba[3], rgba[0], rgba[1], rgba[2]]);
        }
    }
    #[test]
    fn preference_defaults_on_and_accepts_off() {
        let config = include_str!("../defaults/config.yaml");
        assert!(
            serde_yaml::from_str::<crate::Settings>(&config.replace("tray-enabled: true\n", ""))
                .unwrap()
                .tray_enabled
        );
        assert!(
            !serde_yaml::from_str::<crate::Settings>(
                &config.replace("tray-enabled: true", "tray-enabled: false")
            )
            .unwrap()
            .tray_enabled
        );
    }
    #[test]
    fn offline_menu_keeps_service_controls() {
        let (tx, _rx) = mpsc::channel();
        let tray = Tray {
            status: Snapshot {
                service: "inactive".into(),
                api_error: Some("offline".into()),
                ..Default::default()
            },
            error: None,
            tx,
            icon: icon().unwrap(),
        };
        assert_eq!(ksni::Tray::menu(&tray).len(), 11);
    }
    #[test]
    fn synchronization_enables_watcher_and_tray() {
        let mut actions = Vec::new();
        sync_units(
            true,
            |_, _| false,
            |args| {
                actions.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            actions,
            vec![
                vec!["enable", "--now", WATCHER],
                vec!["enable", "--now", SERVICE]
            ]
        );
    }
    #[test]
    fn disabling_tray_keeps_config_watcher() {
        let mut actions = Vec::new();
        sync_units(
            false,
            |_, _| true,
            |args| {
                actions.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actions, vec![vec!["disable", "--now", SERVICE]]);
    }
    #[test]
    fn synchronized_services_are_not_restarted() {
        sync_units(true, |_, _| true, |_| panic!("already synchronized")).unwrap();
    }
    #[test]
    fn failed_systemctl_is_reported() {
        assert!(sync_units(true, |_, _| false, |_| anyhow::bail!("no user bus")).is_err());
    }
    #[test]
    fn menu_callbacks_queue_mode_proxy_and_restart() {
        let (tx, rx) = mpsc::channel();
        let mut tray = Tray {
            status: Snapshot {
                service: "active".into(),
                mode: "global".into(),
                group: "GLOBAL".into(),
                current: "Node / One".into(),
                proxies: vec!["Node / One".into()],
                ..Default::default()
            },
            error: None,
            tx,
            icon: icon().unwrap(),
        };
        let mut menu = ksni::Tray::menu(&tray);
        if let ksni::MenuItem::SubMenu(submenu) = &mut menu[3] {
            if let ksni::MenuItem::RadioGroup(group) = &mut submenu.submenu[0] {
                assert_eq!(group.selected, 1);
                (group.select)(&mut tray, 2);
            } else {
                panic!("missing mode choices");
            }
        } else {
            panic!("missing mode menu");
        }
        assert!(matches!(
            rx.try_recv().unwrap(),
            Request::Mode(Mode::Direct)
        ));
        if let ksni::MenuItem::SubMenu(submenu) = &mut menu[4] {
            if let ksni::MenuItem::RadioGroup(group) = &mut submenu.submenu[0] {
                (group.select)(&mut tray, 0);
            } else {
                panic!("missing proxy choices");
            }
        } else {
            panic!("missing proxy menu");
        }
        assert!(
            matches!(rx.try_recv().unwrap(), Request::Proxy { group, name } if group == "GLOBAL" && name == "Node / One")
        );
        if let ksni::MenuItem::Standard(item) = &mut menu[8] {
            (item.activate)(&mut tray);
        } else {
            panic!("missing restart action");
        }
        assert!(matches!(
            rx.try_recv().unwrap(),
            Request::Service("restart")
        ));
    }
    #[test]
    fn preference_edits_preserve_comments_and_other_fields() {
        let body = "# settings\ntray-enabled: true # desktop\noutput: current.yaml\n";
        let changed = rewrite_preference(body, false).unwrap();
        assert!(changed.contains("# settings"));
        assert!(changed.contains("tray-enabled: false # desktop"));
        for body in [
            "{output: current.yaml, tray-enabled: true}",
            "---\noutput: current.yaml\n...\n",
            "output: current.yaml\n",
        ] {
            let result = rewrite_preference(body, false).unwrap();
            let value: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
            assert_eq!(value["tray-enabled"], false);
            assert_eq!(value["output"], "current.yaml");
        }
    }
}
