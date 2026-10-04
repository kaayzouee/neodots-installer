// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::{
    config::{
        MachineConfig, PasswordState, home_directory_for_username, render_machine_config,
        validate_absolute_path, validate_hostname, validate_username_for_target,
    },
    target::TargetRoot,
    wallpaper::{WallpaperAsset, WallpaperSelection, validate_wallpaper_id},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Machine,
    Password,
    Wallpaper,
    Review,
    Confirm,
    Progress,
    Error,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MachineField {
    Username,
    Hostname,
    Personal,
    Persistence,
    PersistencePath,
}

impl MachineField {
    fn index(self) -> usize {
        match self {
            Self::Username => 0,
            Self::Hostname => 1,
            Self::Personal => 2,
            Self::Persistence => 3,
            Self::PersistencePath => 4,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Username,
            1 => Self::Hostname,
            2 => Self::Personal,
            3 => Self::Persistence,
            _ => Self::PersistencePath,
        }
    }
}

#[derive(Debug, Clone)]
struct MachineForm {
    machine: MachineConfig,
    field: MachineField,
    error: Option<String>,
}

impl MachineForm {
    fn new(machine: MachineConfig) -> Self {
        Self {
            machine,
            field: MachineField::Username,
            error: None,
        }
    }

    fn current_field_index(&self) -> usize {
        self.field.index()
    }

    fn last_field_index(&self) -> usize {
        if self.machine.persistence_enable {
            MachineField::PersistencePath.index()
        } else {
            MachineField::Persistence.index()
        }
    }

    fn move_up(&mut self) {
        let current = self.current_field_index();

        if current == 0 {
            self.field = MachineField::from_index(self.last_field_index());
            return;
        }

        self.field = MachineField::from_index(current - 1);
    }

    fn move_down(&mut self) {
        let current = self.current_field_index();

        if current >= self.last_field_index() {
            self.field = MachineField::Username;
            return;
        }

        self.field = MachineField::from_index(current + 1);
    }

    fn backspace(&mut self) {
        match self.field {
            MachineField::Username => {
                self.machine.username.pop();
            }

            MachineField::Hostname => {
                self.machine.hostname.pop();
            }

            MachineField::PersistencePath => {
                self.machine.persistence_path.pop();
            }

            MachineField::Personal | MachineField::Persistence => {}
        }
    }

    fn push_char(&mut self, character: char) {
        match self.field {
            MachineField::Username => {
                self.machine.username.push(character);
            }

            MachineField::Hostname => {
                self.machine.hostname.push(character);
            }

            MachineField::PersistencePath => {
                self.machine.persistence_path.push(character);
            }

            MachineField::Personal | MachineField::Persistence => {}
        }
    }

    fn toggle(&mut self) {
        match self.field {
            MachineField::Personal => {
                self.machine.personal_enable = !self.machine.personal_enable;
            }

            MachineField::Persistence => {
                self.machine.persistence_enable = !self.machine.persistence_enable;
            }

            MachineField::Username | MachineField::Hostname | MachineField::PersistencePath => {}
        }
    }

    fn force_toggle(&mut self, value: bool) {
        match self.field {
            MachineField::Personal => {
                self.machine.personal_enable = value;
            }

            MachineField::Persistence => {
                self.machine.persistence_enable = value;
            }

            MachineField::Username | MachineField::Hostname | MachineField::PersistencePath => {}
        }
    }
}

pub struct Tui {
    terminal: ratatui::DefaultTerminal,
    screen: Screen,
}

impl Tui {
    pub fn new(screen: Screen) -> Result<Self, String> {
        let terminal = ratatui::try_init()
            .map_err(|error| format!("failed to initialize terminal UI: {error}"))?;

        Ok(Self { terminal, screen })
    }

    fn set_screen(&mut self, screen: Screen) {
        self.screen = screen;
    }

    fn draw<F>(&mut self, renderer: F) -> Result<(), String>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal
            .draw(renderer)
            .map(|_| ())
            .map_err(|error| format!("failed to render terminal UI: {error}"))
    }

    fn wait_key(&self) -> Result<Event, String> {
        loop {
            if event::poll(Duration::from_millis(250))
                .map_err(|error| format!("failed to poll terminal input: {error}"))?
            {
                let event = event::read()
                    .map_err(|error| format!("failed to read terminal input: {error}"))?;

                if matches!(
                    event,
                    Event::Key(ref key) if key.kind == KeyEventKind::Press
                ) {
                    return Ok(event);
                }
            }
        }
    }

    fn footer(frame: &mut Frame, area: Rect, text: &str) {
        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::TOP))
            .style(Style::default().add_modifier(Modifier::DIM));

        frame.render_widget(paragraph, area);
    }

    fn shell<'a>(frame: &mut Frame, area: Rect, title: &str, body: Paragraph<'a>, footer: &str) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(area);

        let header = Paragraph::new("Neodots Installer")
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .alignment(Alignment::Center);

        frame.render_widget(header, chunks[0]);
        frame.render_widget(body, chunks[1]);
        Self::footer(frame, chunks[2], footer);
    }

    pub fn machine_config(
        &mut self,
        target: &TargetRoot,
        detected: &MachineConfig,
    ) -> Result<MachineConfig, String> {
        self.set_screen(Screen::Machine);

        let mut form = MachineForm::new(detected.clone());

        loop {
            let machine = form.machine.clone();
            let field = form.field;
            let error = form.error.clone();

            self.draw(|frame| {
                let field_items = vec![
                    ListItem::new(format!(
                        "{} Username: {}",
                        if field == MachineField::Username {
                            ">"
                        } else {
                            " "
                        },
                        machine.username
                    )),
                    ListItem::new(format!(
                        "{} Hostname: {}",
                        if field == MachineField::Hostname {
                            ">"
                        } else {
                            " "
                        },
                        machine.hostname
                    )),
                    ListItem::new(format!(
                        "{} Personal configuration: {}",
                        if field == MachineField::Personal {
                            ">"
                        } else {
                            " "
                        },
                        enabled_label(machine.personal_enable)
                    )),
                    ListItem::new(format!(
                        "{} Persistence: {}",
                        if field == MachineField::Persistence {
                            ">"
                        } else {
                            " "
                        },
                        enabled_label(machine.persistence_enable)
                    )),
                ];

                let mut all_items = field_items;

                if machine.persistence_enable {
                    all_items.push(ListItem::new(format!(
                        "{} Persistence path: {}",
                        if field == MachineField::PersistencePath {
                            ">"
                        } else {
                            " "
                        },
                        machine.persistence_path
                    )));
                }

                all_items.push(ListItem::new(format!(
                    "  Home directory: {}",
                    machine.home_directory
                )));

                let list = List::new(all_items).block(
                    Block::default()
                        .title("Configuration")
                        .borders(Borders::ALL),
                );

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(4)])
                    .split(frame.area());

                frame.render_widget(list, chunks[0]);

                let message = error
                    .as_deref()
                    .unwrap_or("Up/Down: field  Space: toggle  Enter: next  Esc: cancel");

                let help = Paragraph::new(message)
                    .block(Block::default().borders(Borders::ALL))
                    .wrap(Wrap { trim: true });

                frame.render_widget(help, chunks[1]);
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Esc => {
                        return Err("configuration selection cancelled".to_string());
                    }

                    KeyCode::Up => {
                        form.move_up();
                        form.error = None;
                    }

                    KeyCode::Down => {
                        form.move_down();
                        form.error = None;
                    }

                    KeyCode::Backspace => {
                        form.backspace();
                        form.error = None;
                    }

                    KeyCode::Char(' ') => {
                        form.toggle();
                        form.error = None;
                    }

                    KeyCode::Left
                        if matches!(
                            form.field,
                            MachineField::Personal | MachineField::Persistence
                        ) =>
                    {
                        form.force_toggle(false);
                        form.error = None;
                    }

                    KeyCode::Right
                        if matches!(
                            form.field,
                            MachineField::Personal | MachineField::Persistence
                        ) =>
                    {
                        form.force_toggle(true);
                        form.error = None;
                    }

                    KeyCode::Enter => {
                        let complete = validate_and_advance_machine_form(target, &mut form);

                        if complete {
                            return Ok(form.machine);
                        }
                    }

                    KeyCode::Char(character) => {
                        form.push_char(character);
                        form.error = None;
                    }

                    _ => {}
                }
            }
        }
    }

    pub fn password_state(&mut self, state: PasswordState) -> Result<(), String> {
        self.set_screen(Screen::Password);

        let state_text = state.as_str();

        loop {
            self.draw(|frame| {
                let (message, style) = match state {
                    PasswordState::PasswordProtected => (
                        "The primary account has a usable password. Installation may continue.",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),

                    PasswordState::Passwordless => (
                        "The primary account is passwordless. The installer will refuse to continue.",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),

                    PasswordState::Locked => (
                        "The primary account is locked. The installer will refuse to continue.",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                };

                let body = Paragraph::new(vec![
                    Line::from(format!("Detected state: {state_text}")),
                    Line::from(""),
                    Line::from(Span::styled(message, style)),
                ])
                .block(
                    Block::default()
                        .title("Password state")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true });

                Self::shell(
                    frame,
                    frame.area(),
                    "Password",
                    body,
                    "Enter: continue  Esc: cancel",
                );
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Enter => return Ok(()),
                    KeyCode::Esc => return Err("password-state screen cancelled".to_string()),
                    _ => {}
                }
            }
        }
    }

    pub fn wallpaper(&mut self) -> Result<WallpaperSelection, String> {
        self.set_screen(Screen::Wallpaper);

        let mut selected = 0usize;
        let mut specific = String::new();
        let mut error = None::<String>;

        loop {
            self.draw(|frame| {
                let options = ["None", "Random", "Specific"];

                let items = options
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let marker = if selected == index { ">" } else { " " };

                        ListItem::new(format!("{marker} {value}"))
                    })
                    .collect::<Vec<_>>();

                let list = List::new(items)
                    .block(Block::default().title("Wallpaper").borders(Borders::ALL));

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(5)])
                    .split(frame.area());

                frame.render_widget(list, chunks[0]);

                let detail = if selected == 2 {
                    let message = error
                        .as_deref()
                        .unwrap_or("Type a wallpaper ID, then press Enter.");

                    Paragraph::new(vec![
                        Line::from(format!("ID: {specific}")),
                        Line::from(""),
                        Line::from(message),
                    ])
                } else {
                    Paragraph::new(
                        error
                            .as_deref()
                            .unwrap_or("Up/Down: selection  Enter: confirm  Esc: cancel"),
                    )
                };

                let detail = detail
                    .block(Block::default().borders(Borders::ALL))
                    .wrap(Wrap { trim: true });

                frame.render_widget(detail, chunks[1]);
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Esc => {
                        return Err("wallpaper selection cancelled".to_string());
                    }

                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        error = None;
                    }

                    KeyCode::Down => {
                        selected = (selected + 1).min(2);
                        error = None;
                    }

                    KeyCode::Backspace if selected == 2 => {
                        specific.pop();
                        error = None;
                    }

                    KeyCode::Char(character) if selected == 2 => {
                        specific.push(character);
                        error = None;
                    }

                    KeyCode::Enter => match selected {
                        0 => return Ok(WallpaperSelection::None),
                        1 => return Ok(WallpaperSelection::Random),
                        2 => match validate_wallpaper_id(&specific) {
                            Ok(()) => {
                                return Ok(WallpaperSelection::Specific(specific));
                            }

                            Err(message) => {
                                error = Some(message);
                            }
                        },

                        _ => unreachable!(),
                    },

                    _ => {}
                }
            }
        }
    }

    pub fn review(
        &mut self,
        target: &TargetRoot,
        machine: &MachineConfig,
        wallpaper: Option<&WallpaperAsset>,
    ) -> Result<(), String> {
        self.set_screen(Screen::Review);

        let rendered = render_machine_config(machine);

        let wallpaper_text = match wallpaper {
            Some(asset) => format!(
                "Wallpaper\n  id: {}\n  filename: {}\n  format: {}\n  size: {} bytes\n  sha256: {}\n",
                asset.id, asset.filename, asset.format, asset.size, asset.sha256
            ),

            None => "Wallpaper\n  none\n".to_string(),
        };

        let mut text = String::new();

        text.push_str("Target\n");
        text.push_str(&format!("  root: {}\n", target.root().display()));
        text.push_str(&format!(
            "  machine.nix: {}\n\n",
            target.machine_config().display()
        ));
        text.push_str("Generated machine contract\n");
        text.push_str(&rendered);
        text.push('\n');
        text.push_str(&wallpaper_text);

        loop {
            self.draw(|frame| {
                let body = Paragraph::new(text.clone())
                    .block(Block::default().title("Review").borders(Borders::ALL))
                    .wrap(Wrap { trim: false });

                Self::shell(
                    frame,
                    frame.area(),
                    "Review",
                    body,
                    "Enter: continue  Esc: cancel",
                );
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Enter => return Ok(()),
                    KeyCode::Esc => return Err("review cancelled".to_string()),
                    _ => {}
                }
            }
        }
    }

    pub fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool, String> {
        self.set_screen(Screen::Confirm);

        let mut selected = default;

        loop {
            self.draw(|frame| {
                let yes_style = if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::DIM)
                };

                let no_style = if !selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::DIM)
                };

                let body = Paragraph::new(vec![
                    Line::from(prompt),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("[ Yes ]", yes_style),
                        Span::raw("    "),
                        Span::styled("[ No ]", no_style),
                    ]),
                ])
                .block(Block::default().title("Confirmation").borders(Borders::ALL))
                .alignment(Alignment::Center);

                Self::shell(
                    frame,
                    frame.area(),
                    "Confirmation",
                    body,
                    "Left/Right: select  Y/N: direct select  Enter: continue  Esc: cancel",
                );
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Left => selected = false,
                    KeyCode::Right => selected = true,
                    KeyCode::Char('y') | KeyCode::Char('Y') => selected = true,
                    KeyCode::Char('n') | KeyCode::Char('N') => selected = false,
                    KeyCode::Enter => return Ok(selected),
                    KeyCode::Esc => return Err("operation cancelled".to_string()),
                    _ => {}
                }
            }
        }
    }

    pub fn progress_gate(&mut self, title: &str, detail: &str) -> Result<(), String> {
        self.set_screen(Screen::Progress);

        let started = Instant::now();

        loop {
            self.draw(|frame| {
                let elapsed = started.elapsed().as_secs();

                let body = Paragraph::new(vec![
                    Line::from(Span::styled(
                        title,
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(detail),
                    Line::from(""),
                    Line::from(format!("Ready to start. Elapsed: {elapsed}s")),
                ])
                .block(Block::default().title("Progress").borders(Borders::ALL))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

                Self::shell(
                    frame,
                    frame.area(),
                    "Progress",
                    body,
                    "Enter: begin  Esc: cancel",
                );
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Enter => return Ok(()),
                    KeyCode::Esc => {
                        return Err("operation cancelled before execution".to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn error(&mut self, message: &str, recovery_hint: &str) -> Result<bool, String> {
        self.set_screen(Screen::Error);

        loop {
            self.draw(|frame| {
                let body = Paragraph::new(vec![
                    Line::from(Span::styled(
                        "Installation error",
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(message),
                    Line::from(""),
                    Line::from("Recovery guidance"),
                    Line::from(recovery_hint),
                ])
                .block(
                    Block::default()
                        .title("Error / Recovery")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true });

                Self::shell(
                    frame,
                    frame.area(),
                    "Error",
                    body,
                    "R: retry  Enter/Esc: abort",
                );
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Char('r') | KeyCode::Char('R') => return Ok(true),
                    KeyCode::Enter | KeyCode::Esc => return Ok(false),
                    _ => {}
                }
            }
        }
    }

    pub fn done(&mut self, message: &str) -> Result<(), String> {
        self.set_screen(Screen::Done);

        loop {
            self.draw(|frame| {
                let body = Paragraph::new(vec![
                    Line::from(Span::styled(
                        "Installation completed",
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(message),
                ])
                .block(Block::default().title("Done").borders(Borders::ALL))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

                Self::shell(frame, frame.area(), "Done", body, "Enter: exit installer");
            })?;

            if let Event::Key(key) = self.wait_key()? {
                match key.code {
                    KeyCode::Enter | KeyCode::Esc => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn enabled_label(value: bool) -> &'static str {
    if value { "enabled" } else { "disabled" }
}

fn validate_and_advance_machine_form(target: &TargetRoot, form: &mut MachineForm) -> bool {
    match form.field {
        MachineField::Username => {
            if let Err(error) = validate_username_for_target(target, &form.machine.username) {
                form.error = Some(error);
                return false;
            }

            form.machine.home_directory = home_directory_for_username(&form.machine.username);
            form.field = MachineField::Hostname;
            form.error = None;

            false
        }

        MachineField::Hostname => {
            if let Err(error) = validate_hostname(&form.machine.hostname) {
                form.error = Some(error);
                return false;
            }

            form.field = MachineField::Personal;
            form.error = None;

            false
        }

        MachineField::Personal => {
            form.field = MachineField::Persistence;
            form.error = None;

            false
        }

        MachineField::Persistence => {
            if form.machine.persistence_enable {
                form.field = MachineField::PersistencePath;
                form.error = None;

                false
            } else {
                form.error = None;

                true
            }
        }

        MachineField::PersistencePath => {
            if let Err(error) =
                validate_absolute_path(&form.machine.persistence_path, "persistence.path", true)
            {
                form.error = Some(error);

                return false;
            }

            form.error = None;

            true
        }
    }
}

pub fn run_with_progress<T, F>(title: &str, detail: &str, operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    let mut tui = Tui::new(Screen::Progress)?;

    tui.progress_gate(title, detail)?;
    drop(tui);

    operation()
}

pub fn select_machine_config(
    target: &TargetRoot,
    detected: &MachineConfig,
) -> Result<MachineConfig, String> {
    let mut tui = Tui::new(Screen::Machine)?;

    tui.machine_config(target, detected)
}

pub fn show_password_state(state: PasswordState) -> Result<(), String> {
    let mut tui = Tui::new(Screen::Password)?;

    tui.password_state(state)
}

pub fn select_wallpaper() -> Result<WallpaperSelection, String> {
    let mut tui = Tui::new(Screen::Wallpaper)?;

    tui.wallpaper()
}

pub fn show_review(
    target: &TargetRoot,
    machine: &MachineConfig,
    wallpaper: Option<&WallpaperAsset>,
) -> Result<(), String> {
    let mut tui = Tui::new(Screen::Review)?;

    tui.review(target, machine, wallpaper)
}

pub fn confirm(prompt: &str, default: bool) -> Result<bool, String> {
    let mut tui = Tui::new(Screen::Confirm)?;

    tui.confirm(prompt, default)
}

pub fn show_error(message: &str, recovery_hint: &str) -> Result<bool, String> {
    let mut tui = Tui::new(Screen::Error)?;

    tui.error(message, recovery_hint)
}

pub fn show_done(message: &str) -> Result<(), String> {
    let mut tui = Tui::new(Screen::Done)?;

    tui.done(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_field_indexes_are_stable() {
        assert_eq!(MachineField::Username.index(), 0);
        assert_eq!(MachineField::Hostname.index(), 1);
        assert_eq!(MachineField::Personal.index(), 2);
        assert_eq!(MachineField::Persistence.index(), 3);
        assert_eq!(MachineField::PersistencePath.index(), 4);
    }

    #[test]
    fn machine_form_exposes_persistence_path_only_when_enabled() {
        let mut machine = MachineConfig {
            system: "x86_64-linux".to_string(),
            username: "kay".to_string(),
            hostname: "nixos".to_string(),
            home_directory: "/home/kay".to_string(),
            personal_enable: false,
            persistence_enable: false,
            persistence_path: "/persist".to_string(),
        };

        let form = MachineForm::new(machine.clone());

        assert_eq!(form.last_field_index(), MachineField::Persistence.index());

        machine.persistence_enable = true;

        let form = MachineForm::new(machine);

        assert_eq!(
            form.last_field_index(),
            MachineField::PersistencePath.index()
        );
    }

    #[test]
    fn enabled_label_is_stable() {
        assert_eq!(enabled_label(true), "enabled");
        assert_eq!(enabled_label(false), "disabled");
    }
}
