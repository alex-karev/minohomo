mod input;
mod ui;
mod worker;

use crate::{api::Mode, storage::Store};
use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use input::Input;
use ratatui::{
    DefaultTerminal,
    widgets::{ListState, TableState},
};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
};
use worker::{Job, Message, Status};

#[derive(Clone, Copy)]
enum Operation {
    Add,
    Remove,
    Rename,
    UpdateSelected,
    UpdateAll,
    ApplyCache,
    CoreUpdate,
    Geosite,
    GeositeOnly,
    Mode,
    Proxy,
    Ping,
    Ip,
    Config,
    Rules,
    Tray,
    Start,
    Stop,
    Restart,
    Refresh,
}
const ACTIONS: &[(Operation, &str, &str)] = &[
    (
        Operation::Add,
        "a  Add subscription / proxy",
        "Save one URL or proxy link. No fetch until Update.",
    ),
    (
        Operation::Remove,
        "d  Remove selected source",
        "Confirm before removing the source and its cache.",
    ),
    (
        Operation::Rename,
        "n  Rename selected source",
        "Change its saved source name. Cached nodes and timestamps stay unchanged.",
    ),
    (
        Operation::UpdateSelected,
        "u  Update selected source",
        "Fetch selected source, regenerate config, apply through sudo.",
    ),
    (
        Operation::UpdateAll,
        "U  Update all sources",
        "Fetch all sources, validate, apply through sudo.",
    ),
    (
        Operation::ApplyCache,
        "    Apply cached sources",
        "Regenerate after rule/config edits or source removal. No subscription fetch.",
    ),
    (
        Operation::CoreUpdate,
        "    Update Mihomo core",
        "Download latest core, install through sudo, restart minohomo.service if running.",
    ),
    (
        Operation::Geosite,
        "g  Update geosite + reload",
        "Download and validate geosite, then apply through sudo.",
    ),
    (
        Operation::GeositeOnly,
        "    Download geosite only",
        "Download and validate without reload. Useful before first Update.",
    ),
    (
        Operation::Mode,
        "m  Change mode",
        "Rule / Global / Direct. Runtime change only.",
    ),
    (
        Operation::Proxy,
        "x  Select current proxy",
        "Choose from the running selector. Type to filter.",
    ),
    (
        Operation::Ping,
        "p  Ping all proxies",
        "Same delay test as minohomo ping, sorted fastest first.",
    ),
    (
        Operation::Ip,
        "i  Look up public IP + location",
        "Contact https://ipwho.is on demand. The provider sees your outbound IP.",
    ),
    (
        Operation::Config,
        "c  Edit config.yaml",
        "Open with $VISUAL, $EDITOR, or sensible-editor.",
    ),
    (
        Operation::Rules,
        "r  Edit rules.yaml",
        "Apply cached sources afterward to deploy changes.",
    ),
    (
        Operation::Tray,
        "t  Toggle tray",
        "Save tray-enabled in config and synchronize its user service.",
    ),
    (
        Operation::Start,
        "    Start Mihomo",
        "Start the system service through sudo. Does not enable it at boot.",
    ),
    (
        Operation::Stop,
        "    Stop Mihomo",
        "Stop the system service through sudo.",
    ),
    (
        Operation::Restart,
        "    Restart Mihomo",
        "Restart the system service through sudo.",
    ),
    (
        Operation::Refresh,
        "F5 Refresh dashboard",
        "Read saved sources and query the running Mihomo API.",
    ),
];

#[derive(Default)]
struct AddForm {
    name: Input,
    link: Input,
    direct: bool,
    field: usize,
    error: String,
}
struct RenameForm {
    old_name: String,
    name: Input,
    error: String,
}
struct Choice {
    title: String,
    options: Vec<String>,
    values: Vec<ChoiceValue>,
    filter: Input,
    state: ListState,
}
#[derive(Clone)]
enum ChoiceValue {
    Mode(Mode),
    Proxy(String, String),
    Service(&'static str),
}
enum Modal {
    Add(AddForm),
    Rename(RenameForm),
    Confirm {
        description: String,
        args: Vec<String>,
    },
    Choice(Choice),
    Help {
        scroll: u16,
    },
}
enum Foreground {
    Cli(Vec<String>),
    Editor(&'static str),
}

struct App {
    root: PathBuf,
    tab: usize,
    status: Status,
    sources: TableState,
    actions: ListState,
    pings: Vec<(String, Option<u64>)>,
    ping_state: TableState,
    ip: String,
    modal: Option<Modal>,
    notice: String,
    failed: bool,
    busy: Option<&'static str>,
    tick: Instant,
    jobs: Sender<Job>,
    messages: Receiver<Message>,
}
impl App {
    fn new(root: PathBuf, notice: String) -> Self {
        let (jobs, messages) = worker::spawn(root.clone());
        Self::with_channels(root, notice, jobs, messages)
    }
    fn with_channels(
        root: PathBuf,
        notice: String,
        jobs: Sender<Job>,
        messages: Receiver<Message>,
    ) -> Self {
        Self {
            root, tab: 0, status: Status::default(), sources: TableState::default().with_selected(0),
            actions: ListState::default().with_selected(Some(0)), pings: vec![], ping_state: TableState::default().with_selected(0),
            ip: "Not requested. Press i to contact ipwho.is.\nThe provider sees your outbound IP. Location is approximate.".into(),
            modal: None, notice, failed: false, busy: None, tick: Instant::now(), jobs, messages,
        }
    }
    #[cfg(test)]
    fn test_app() -> Self {
        let (jobs, _rx) = std::sync::mpsc::channel();
        let (_tx, messages) = std::sync::mpsc::channel();
        Self::with_channels(
            PathBuf::from("/test/profile"),
            "Ready".into(),
            jobs,
            messages,
        )
    }
    fn notify(&mut self, text: impl Into<String>, failed: bool) {
        self.notice = text.into();
        self.failed = failed;
    }
    fn send(&mut self, job: Job, label: &'static str) {
        if self.busy.is_some() {
            self.notify(
                "An action is already running. You can still browse or quit.",
                false,
            );
            return;
        }
        if self.jobs.send(job).is_ok() {
            self.busy = Some(label);
        } else {
            self.notify("Background worker stopped. Reopen minohomo.", true);
        }
    }
    fn receive(&mut self) {
        while let Ok(message) = self.messages.try_recv() {
            match message {
                Message::Status(Ok(status)) => {
                    let selected_id = self
                        .sources
                        .selected()
                        .and_then(|i| self.status.sources.get(i))
                        .map(|source| source.id);
                    let index = selected_id
                        .and_then(|id| status.sources.iter().position(|source| source.id == id))
                        .unwrap_or_else(|| {
                            self.sources
                                .selected()
                                .unwrap_or(0)
                                .min(status.sources.len().saturating_sub(1))
                        });
                    self.sources.select(if status.sources.is_empty() {
                        None
                    } else {
                        Some(index)
                    });
                    self.status = status;
                    if self.notice.starts_with("Refresh failed:") {
                        self.notify("Dashboard refreshed.", false);
                    }
                }
                Message::Status(Err(error)) => {
                    self.notify(format!("Refresh failed: {error}"), true)
                }
                Message::Ping(result) => {
                    self.busy = None;
                    match result {
                        Ok(pings) => {
                            self.notify(
                                format!(
                                    "Ping complete: {} entries. Failed checks sort last.",
                                    pings.len()
                                ),
                                false,
                            );
                            self.pings = pings;
                            self.ping_state.select(Some(0));
                        }
                        Err(error) => self.notify(error, true),
                    }
                }
                Message::Ip(result) => {
                    self.busy = None;
                    match result {
                        Ok(ip) => {
                            self.ip = ip;
                            self.notify("IP lookup complete. Location is approximate.", false);
                        }
                        Err(error) => self.notify(format!("IP lookup failed: {error}"), true),
                    }
                }
                Message::Action(result) => {
                    self.busy = None;
                    match result {
                        Ok(text) => self.notify(text, false),
                        Err(error) => self.notify(error, true),
                    }
                }
            }
        }
    }
    fn source_name(&self) -> Option<String> {
        self.sources
            .selected()
            .and_then(|i| self.status.sources.get(i))
            .and_then(|source| source.name.clone())
    }
    fn operate(&mut self, operation: Operation) -> Option<Foreground> {
        if self.busy.is_some() {
            self.notify(
                "Wait for the current action to finish, or quit with q.",
                false,
            );
            return None;
        }
        match operation {
            Operation::Add => self.modal = Some(Modal::Add(AddForm::default())),
            Operation::Rename => {
                if let Some(old_name) = self.source_name() {
                    let mut name = Input::default();
                    name.insert(&old_name);
                    self.modal = Some(Modal::Rename(RenameForm {
                        old_name,
                        name,
                        error: String::new(),
                    }));
                } else {
                    self.notify("Select a source first. Press a to add one.", true);
                }
            }
            Operation::Remove => {
                if let Some(name) = self.source_name() {
                    self.modal = Some(Modal::Confirm {
                        description: format!(
                            "Remove source '{name}' and its cache?\nThe active Mihomo config stays unchanged. Apply cached sources afterward."
                        ),
                        args: vec!["delete".into(), "--".into(), name],
                    });
                } else {
                    self.notify("Select a source first. Press a to add one.", true);
                }
            }
            Operation::UpdateSelected => {
                if let Some(name) = self.source_name() {
                    return Some(Foreground::Cli(vec!["update".into(), "--".into(), name]));
                }
                self.notify("Select a source first. Press a to add one.", true);
            }
            Operation::UpdateAll => return Some(Foreground::Cli(vec!["update".into()])),
            Operation::ApplyCache => {
                return Some(Foreground::Cli(vec!["update".into(), "--no-sync".into()]));
            }
            Operation::CoreUpdate => {
                return Some(Foreground::Cli(vec!["core".into(), "update".into()]));
            }
            Operation::Geosite | Operation::GeositeOnly => {
                let mut args = vec!["geosite".into(), "update".into()];
                if matches!(operation, Operation::GeositeOnly) {
                    args.push("--no-reload".into());
                }
                return Some(Foreground::Cli(args));
            }
            Operation::Mode => self.choose(
                "Change mode (runtime only)",
                vec!["Rule".into(), "Global".into(), "Direct".into()],
                vec![
                    ChoiceValue::Mode(Mode::Rule),
                    ChoiceValue::Mode(Mode::Global),
                    ChoiceValue::Mode(Mode::Direct),
                ],
            ),
            Operation::Proxy => {
                if self.status.choices.is_empty() {
                    self.notify(
                        "No running selector available. Start Mihomo and check the API.",
                        true,
                    );
                } else {
                    let values = self
                        .status
                        .choices
                        .iter()
                        .map(|name| ChoiceValue::Proxy(self.status.group.clone(), name.clone()))
                        .collect();
                    self.choose(
                        &format!("Select proxy in {}", self.status.group),
                        self.status.choices.clone(),
                        values,
                    );
                }
            }
            Operation::Ping => {
                self.tab = 1;
                self.send(Job::Ping, "Testing proxy delays");
            }
            Operation::Ip => {
                self.tab = 1;
                self.send(Job::Ip, "Looking up IP via ipwho.is");
            }
            Operation::Config => return Some(Foreground::Editor("config.yaml")),
            Operation::Rules => return Some(Foreground::Editor("rules.yaml")),
            Operation::Tray => {
                return Some(Foreground::Cli(vec![
                    "tray".into(),
                    if self.status.tray_enabled {
                        "disable"
                    } else {
                        "enable"
                    }
                    .into(),
                ]));
            }
            Operation::Start => return Some(Foreground::Cli(vec!["start".into()])),
            Operation::Stop => {
                self.modal = Some(Modal::Confirm {
                    description: "Stop Mihomo? Active connections through it will be interrupted."
                        .into(),
                    args: vec!["stop".into()],
                })
            }
            Operation::Restart => {
                self.modal = Some(Modal::Confirm {
                    description:
                        "Restart Mihomo? Active connections through it will be interrupted.".into(),
                    args: vec!["restart".into()],
                })
            }
            Operation::Refresh => {
                let _ = self.jobs.send(Job::Refresh);
                self.notify("Refreshing dashboard...", false);
            }
        }
        None
    }
    fn choose(&mut self, title: &str, options: Vec<String>, values: Vec<ChoiceValue>) {
        let selected = values
            .iter()
            .position(|value| match value {
                ChoiceValue::Mode(mode) => self.status.mode.eq_ignore_ascii_case(match mode {
                    Mode::Rule => "rule",
                    Mode::Global => "global",
                    Mode::Direct => "direct",
                }),
                ChoiceValue::Proxy(_, name) => name == &self.status.current,
                ChoiceValue::Service(_) => false,
            })
            .unwrap_or(0);
        self.modal = Some(Modal::Choice(Choice {
            title: title.into(),
            options,
            values,
            filter: Input::default(),
            state: ListState::default().with_selected(Some(selected)),
        }));
    }
    fn key(&mut self, key: KeyEvent) -> (bool, Option<Foreground>) {
        if key.kind == KeyEventKind::Release {
            return (false, None);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return (true, None);
        }
        if let Some(modal) = self.modal.take() {
            return (false, self.modal_key(modal, key));
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return (false, None);
        }
        let mut operation = None;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return (true, None),
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab = (self.tab + if key.code == KeyCode::Tab { 1 } else { 2 }) % 3
            }
            KeyCode::Char('1') => self.tab = 0,
            KeyCode::Char('2') => self.tab = 1,
            KeyCode::Char('3') => self.tab = 2,
            KeyCode::Char('?') | KeyCode::F(1) => self.modal = Some(Modal::Help { scroll: 0 }),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home => self.move_selection(-isize::MAX),
            KeyCode::End => self.move_selection(isize::MAX),
            KeyCode::Enter if self.tab == 2 => {
                operation = Some(ACTIONS[self.actions.selected().unwrap_or(0)].0)
            }
            KeyCode::Char('a') => operation = Some(Operation::Add),
            KeyCode::Char('d') | KeyCode::Delete => operation = Some(Operation::Remove),
            KeyCode::Char('n') if self.tab == 0 => operation = Some(Operation::Rename),
            KeyCode::Char('u') => operation = Some(Operation::UpdateSelected),
            KeyCode::Char('U') => operation = Some(Operation::UpdateAll),
            KeyCode::Char('g') => operation = Some(Operation::Geosite),
            KeyCode::Char('m') => operation = Some(Operation::Mode),
            KeyCode::Char('x') => operation = Some(Operation::Proxy),
            KeyCode::Char('p') => operation = Some(Operation::Ping),
            KeyCode::Char('i') => operation = Some(Operation::Ip),
            KeyCode::Char('c') => operation = Some(Operation::Config),
            KeyCode::Char('r') => operation = Some(Operation::Rules),
            KeyCode::Char('t') => operation = Some(Operation::Tray),
            KeyCode::F(5) => operation = Some(Operation::Refresh),
            KeyCode::Char('s') => self.choose(
                "Mihomo system service",
                vec!["Start".into(), "Stop".into(), "Restart".into()],
                vec![
                    ChoiceValue::Service("start"),
                    ChoiceValue::Service("stop"),
                    ChoiceValue::Service("restart"),
                ],
            ),
            _ => {}
        }
        (
            false,
            operation.and_then(|operation| self.operate(operation)),
        )
    }
    fn move_selection(&mut self, delta: isize) {
        match self.tab {
            0 => self.sources.select(move_index(
                self.sources.selected(),
                self.status.sources.len(),
                delta,
            )),
            1 => self.ping_state.select(move_index(
                self.ping_state.selected(),
                self.pings.len(),
                delta,
            )),
            _ => self
                .actions
                .select(move_index(self.actions.selected(), ACTIONS.len(), delta)),
        }
    }
    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent) -> Option<Foreground> {
        if key.code == KeyCode::Esc {
            return None;
        }
        match &mut modal {
            Modal::Help { scroll } => {
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        *scroll = scroll.saturating_add(1).min(24)
                    }
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => *scroll = scroll.saturating_add(8).min(24),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(8),
                    _ => {}
                }
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    return None;
                }
            }
            Modal::Confirm { args, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    return Some(Foreground::Cli(args.clone()));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter => return None,
                _ => {}
            },
            Modal::Add(form) => match key.code {
                KeyCode::Tab => form.field = (form.field + 1) % 3,
                KeyCode::BackTab => form.field = (form.field + 2) % 3,
                KeyCode::Enter => match add_args(form) {
                    Ok(args) => return Some(Foreground::Cli(args)),
                    Err(error) => form.error = format!("{error:#}"),
                },
                KeyCode::Char(' ') if form.field == 2 => form.direct = !form.direct,
                _ => match form.field {
                    0 => form.name.key(key),
                    1 => form.link.key(key),
                    _ => {}
                },
            },
            Modal::Rename(form) => {
                if key.code == KeyCode::Enter {
                    match crate::parse_source_name(&form.name.text) {
                        Err(error) => form.error = error,
                        Ok(name) => {
                            if self.status.sources.iter().any(|source| {
                                source.name.as_deref() == Some(name.as_str())
                                    && source.name.as_deref() != Some(form.old_name.as_str())
                            }) {
                                form.error = format!("Source name '{name}' is already in use.");
                            } else {
                                return Some(Foreground::Cli(vec![
                                    "rename".into(),
                                    "--".into(),
                                    form.old_name.clone(),
                                    name,
                                ]));
                            }
                        }
                    }
                } else {
                    form.name.key(key);
                }
            }
            Modal::Choice(choice) => {
                let visible = choice.visible();
                match key.code {
                    KeyCode::Down => {
                        choice
                            .state
                            .select(move_index(choice.state.selected(), visible.len(), 1))
                    }
                    KeyCode::Up => {
                        choice
                            .state
                            .select(move_index(choice.state.selected(), visible.len(), -1))
                    }
                    KeyCode::PageDown => {
                        choice
                            .state
                            .select(move_index(choice.state.selected(), visible.len(), 10))
                    }
                    KeyCode::PageUp => {
                        choice
                            .state
                            .select(move_index(choice.state.selected(), visible.len(), -10))
                    }
                    KeyCode::Enter => {
                        if let Some(value) = choice
                            .state
                            .selected()
                            .and_then(|i| visible.get(i))
                            .and_then(|i| choice.values.get(*i))
                            .cloned()
                        {
                            match value {
                                ChoiceValue::Mode(mode) => {
                                    self.send(Job::Mode(mode), "Changing mode")
                                }
                                ChoiceValue::Proxy(group, name) => {
                                    self.send(Job::Proxy { group, name }, "Switching proxy")
                                }
                                ChoiceValue::Service(action) => {
                                    return self.operate(match action {
                                        "start" => Operation::Start,
                                        "stop" => Operation::Stop,
                                        _ => Operation::Restart,
                                    });
                                }
                            }
                            return None;
                        }
                    }
                    _ => {
                        choice.filter.key(key);
                        choice.state.select(Some(0));
                    }
                }
            }
        }
        self.modal = Some(modal);
        None
    }
}
impl Choice {
    fn visible(&self) -> Vec<usize> {
        let query = self.filter.text.to_lowercase();
        self.options
            .iter()
            .enumerate()
            .filter(|(_, name)| name.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }
}
fn move_index(current: Option<usize>, count: usize, delta: isize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(
        current
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(count - 1),
    )
}
fn add_args(form: &AddForm) -> Result<Vec<String>> {
    let link = form.link.text.trim();
    ensure!(!link.is_empty(), "Enter a subscription URL or proxy link.");
    crate::classify(link, form.direct)?;
    let mut args = vec!["add".into(), link.into()];
    if form.direct {
        args.push("--proxy".into());
    }
    if !form.name.text.trim().is_empty() {
        let name = crate::parse_source_name(&form.name.text).map_err(anyhow::Error::msg)?;
        args.push(format!("--name={name}"));
    }
    Ok(args)
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste, crossterm::cursor::Show);
        ratatui::restore();
    }
}
fn suspend() -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    Ok(())
}
fn resume(terminal: &mut DefaultTerminal) -> Result<()> {
    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        crossterm::cursor::Hide
    )?;
    terminal.clear()?;
    Ok(())
}
fn editor_command(path: &Path) -> Result<Command> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "sensible-editor".into());
    build_editor(&editor, path)
}
fn build_editor(editor: &str, path: &Path) -> Result<Command> {
    let words = shell_words::split(editor).context("invalid quoting in VISUAL/EDITOR")?;
    let program = words.first().context("editor command is empty")?;
    let mut command = Command::new(program);
    command.args(&words[1..]).arg(path);
    Ok(command)
}
fn foreground(terminal: &mut DefaultTerminal, root: &Path, action: Foreground) -> Result<()> {
    suspend()?;
    let is_editor = matches!(action, Foreground::Editor(_));
    let result = (|| -> Result<()> {
        let mut command = match action {
            Foreground::Cli(args) => {
                println!("Minohomo action. Sudo may request your password.\n");
                let mut command = Command::new(std::env::current_exe()?);
                command.arg("--config-dir").arg(root).args(args);
                command
            }
            Foreground::Editor(file) => editor_command(&root.join(file))?,
        };
        let status = command.status().context("cannot launch command/editor")?;
        ensure!(status.success(), "command exited with {status}");
        Ok(())
    })();
    if let Err(error) = &result {
        eprintln!("\nerror: {error:#}");
    }
    if !is_editor || result.is_err() {
        print!("\nPress Enter to return to the dashboard...");
        let _ = io::stdout().flush();
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }
    resume(terminal)?;
    result
}

pub fn run(root: &Path) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the dashboard needs an interactive terminal; use minohomo --help for CLI commands"
    );
    {
        let store = Store::open(root)?;
        store.settings()?;
        store.state()?;
    }
    let mut notice = String::new();
    if unsafe { libc::geteuid() } != 0
        && root == crate::default_config_dir()?
        && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        && let Err(error) = crate::tray::sync(root)
    {
        notice = format!("Tray sync: {error:#}");
    }
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        old_hook(info);
    }));
    let _guard = TerminalGuard;
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let mut app = App::new(root.into(), notice);
    loop {
        app.receive();
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                let (quit, action) = app.key(key);
                if quit {
                    return Ok(());
                }
                if let Some(action) = action {
                    match foreground(&mut terminal, root, action) {
                        Ok(()) => app.notify("Action finished. Add/remove/rename/edit changes need Update or Apply cached sources to deploy.", false),
                        Err(error) => app.notify(format!("Action failed: {error:#}"), true),
                    }
                    let _ = app.jobs.send(Job::Refresh);
                }
            }
            Event::Paste(text) => match app.modal.as_mut() {
                Some(Modal::Add(form)) => match form.field {
                    0 => form.name.insert(&text),
                    1 => form.link.insert(&text),
                    _ => {}
                },
                Some(Modal::Rename(form)) => form.name.insert(&text),
                Some(Modal::Choice(choice)) => {
                    choice.filter.insert(&text);
                    choice.state.select(Some(0));
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_handles_empty_lists_and_large_jumps() {
        assert_eq!(move_index(None, 0, 1), None);
        assert_eq!(move_index(Some(2), 3, isize::MAX), Some(2));
        assert_eq!(move_index(Some(2), 3, -isize::MAX), Some(0));
    }
    #[test]
    fn add_validates_and_builds_safe_arguments() {
        let mut form = AddForm::default();
        assert!(add_args(&form).is_err());
        form.name.insert("My source");
        form.link.insert("https://example.test/sub?token=secret");
        assert_eq!(
            add_args(&form).unwrap(),
            vec![
                "add",
                "https://example.test/sub?token=secret",
                "--name=My source"
            ]
        );
        form.link.text = "file:///etc/passwd".into();
        assert!(add_args(&form).is_err());
        form.link.text = "https://user:pass@proxy.example:443".into();
        form.direct = true;
        assert!(add_args(&form).unwrap().contains(&"--proxy".into()));
    }
    #[test]
    fn editor_arguments_do_not_use_a_shell() {
        let command =
            build_editor("editor --wait 'two words'", Path::new("/tmp/a file.yaml")).unwrap();
        assert_eq!(command.get_program(), "editor");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec!["--wait", "two words", "/tmp/a file.yaml"]
        );
        assert!(build_editor("'broken", Path::new("/tmp/config.yaml")).is_err());
    }
    #[test]
    fn confirmation_requires_explicit_yes() {
        let mut app = App::test_app();
        app.modal = Some(Modal::Confirm {
            description: "remove?".into(),
            args: vec!["delete".into(), "Test".into()],
        });
        assert!(app.key(KeyCode::Enter.into()).1.is_none());
        assert!(app.modal.is_none());
        app.modal = Some(Modal::Confirm {
            description: "remove?".into(),
            args: vec!["delete".into(), "Test".into()],
        });
        assert!(matches!(
            app.key(KeyCode::Char('y').into()).1,
            Some(Foreground::Cli(_))
        ));
    }
    #[test]
    fn proxy_filter_queues_exact_api_name_without_blocking() {
        let (jobs, requests) = std::sync::mpsc::channel();
        let (responses, messages) = std::sync::mpsc::channel();
        let mut app = App::with_channels(
            PathBuf::from("/test/profile"),
            "Ready".into(),
            jobs,
            messages,
        );
        app.status.group = "GLOBAL".into();
        app.status.current = "Other".into();
        app.status.choices = vec!["Node / One".into(), "Other".into()];
        app.key(KeyCode::Char('x').into());
        if let Some(Modal::Choice(choice)) = &app.modal {
            assert_eq!(choice.state.selected(), Some(1));
        } else {
            panic!("missing proxy picker");
        }
        for c in "Node /".chars() {
            app.key(KeyCode::Char(c).into());
        }
        app.key(KeyCode::Enter.into());
        assert!(
            matches!(requests.try_recv().unwrap(), Job::Proxy {group, name} if group == "GLOBAL" && name == "Node / One")
        );
        assert!(app.busy.is_some());
        assert!(app.modal.is_none());
        app.key(KeyCode::Char('a').into());
        assert!(
            app.modal.is_none(),
            "must not start a second action while busy"
        );
        responses
            .send(Message::Action(Err("Unauthorized".into())))
            .unwrap();
        app.receive();
        assert!(app.busy.is_none());
        assert!(app.failed);
        assert_eq!(app.notice, "Unauthorized");
    }
    #[test]
    fn source_selection_survives_poll_reordering_and_option_like_names() {
        fn source(name: &str) -> crate::storage::Source {
            crate::storage::Source {
                id: if name == "Other" { 1 } else { 2 },
                kind: crate::storage::SourceKind::Proxy,
                link: "trojan://secret@host.test:443#node".into(),
                name: Some(name.into()),
                added_at: chrono::Utc::now(),
                updated_at: None,
                proxies: vec![],
            }
        }
        let mut app = App::test_app();
        app.status.sources = vec![source("Other"), source("--name")];
        app.sources.select(Some(1));
        let (tx, rx) = std::sync::mpsc::channel();
        app.messages = rx;
        tx.send(Message::Status(Ok(Status {
            sources: vec![source("--name"), source("Other")],
            ..Default::default()
        })))
        .unwrap();
        app.receive();
        assert_eq!(app.sources.selected(), Some(0));
        match app.operate(Operation::UpdateSelected) {
            Some(Foreground::Cli(args)) => assert_eq!(args, vec!["update", "--", "--name"]),
            _ => panic!("missing update command"),
        }
        app.key(KeyCode::Char('d').into());
        match app.key(KeyCode::Char('y').into()).1 {
            Some(Foreground::Cli(args)) => assert_eq!(args, vec!["delete", "--", "--name"]),
            _ => panic!("missing delete command"),
        }
    }
    #[test]
    fn rename_shortcut_is_sources_only_and_validates_dialog() {
        let mut app = App::test_app();
        app.status.sources = ["Old", "Taken"]
            .iter()
            .enumerate()
            .map(|(i, name)| crate::storage::Source {
                id: i as u64 + 1,
                kind: crate::storage::SourceKind::Proxy,
                link: "trojan://secret@host.test:443#node".into(),
                name: Some((*name).into()),
                added_at: chrono::Utc::now(),
                updated_at: None,
                proxies: vec![],
            })
            .collect();
        app.tab = 1;
        app.key(KeyCode::Char('n').into());
        assert!(app.modal.is_none());
        app.tab = 0;
        app.key(KeyCode::Char('n').into());
        if let Some(Modal::Rename(form)) = &app.modal {
            assert_eq!(form.name.text, "Old");
        } else {
            panic!("missing rename form");
        }
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(app.key(KeyCode::Enter.into()).1.is_none());
        if let Some(Modal::Rename(form)) = &app.modal {
            assert!(!form.error.is_empty());
        }
        for c in "Taken".chars() {
            app.key(KeyCode::Char(c).into());
        }
        assert!(app.key(KeyCode::Enter.into()).1.is_none());
        if let Some(Modal::Rename(form)) = &app.modal {
            assert!(form.error.contains("already in use"));
        }
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in "--New name".chars() {
            app.key(KeyCode::Char(c).into());
        }
        match app.key(KeyCode::Enter.into()).1 {
            Some(Foreground::Cli(args)) => {
                assert_eq!(args, vec!["rename", "--", "Old", "--New name"])
            }
            _ => panic!("missing rename command"),
        }
        assert!(app.modal.is_none());
        app.key(KeyCode::Char('n').into());
        app.key(KeyCode::Esc.into());
        assert!(app.modal.is_none());
        app.status.sources.clear();
        app.key(KeyCode::Char('n').into());
        assert!(app.failed);
        assert!(app.modal.is_none());
    }
    #[test]
    fn selection_tracks_source_id_after_rename() {
        let mut app = App::test_app();
        let source = crate::storage::Source {
            id: 42,
            kind: crate::storage::SourceKind::Proxy,
            link: "trojan://secret@host.test:443#node".into(),
            name: Some("Old".into()),
            added_at: chrono::Utc::now(),
            updated_at: None,
            proxies: vec![],
        };
        app.status.sources = vec![source.clone()];
        let (tx, rx) = std::sync::mpsc::channel();
        app.messages = rx;
        let mut renamed = source.clone();
        renamed.name = Some("New".into());
        let mut other = source;
        other.id = 43;
        other.name = Some("Other".into());
        tx.send(Message::Status(Ok(Status {
            sources: vec![other, renamed],
            ..Default::default()
        })))
        .unwrap();
        app.receive();
        assert_eq!(app.sources.selected(), Some(1));
        assert_eq!(app.source_name().as_deref(), Some("New"));
    }
}
