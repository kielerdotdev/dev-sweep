use super::*;
use crate::inventory::{Action, Event, Item};
use crossterm::{
    event::{self, Event as TerminalEvent, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use std::sync::mpsc;
const INK: Color = Color::Rgb(220, 230, 238);
const MUTED: Color = Color::Rgb(142, 161, 176);
const ACCENT: Color = Color::Rgb(108, 211, 200);
const WARN: Color = Color::Rgb(242, 192, 111);
const BG: Color = Color::Rgb(15, 22, 30);
const PANEL: Color = Color::Rgb(21, 31, 42);
const FILTERS: [&str; 7] = [
    "All",
    "Caches",
    "Worktrees",
    "Docker",
    "Stores",
    "Retained",
    "Selected",
];
enum Popup {
    Info(String),
    Ai(Vec<Item>),
    PlanRequest(Vec<Item>),
    Proposal(Vec<Item>),
    Delete(Vec<Item>, String),
}
struct App {
    items: Vec<Item>,
    cursor: usize,
    filter: usize,
    query: String,
    searching: bool,
    scanning: bool,
    executing: usize,
    planning: bool,
    status: String,
    popup: Option<Popup>,
    scroll: u16,
    size_sort: bool,
    tick: usize,
    disk: Option<(u64, u64)>,
    issues: Vec<String>,
}
impl App {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            cursor: 0,
            filter: 0,
            query: String::new(),
            searching: false,
            scanning: true,
            executing: 0,
            planning: false,
            status: "Reading storage inventory".into(),
            popup: None,
            scroll: 0,
            size_sort: true,
            tick: 0,
            disk: None,
            issues: Vec::new(),
        }
    }
    fn visible(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        let mut rows: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                (self.filter == 0
                    || FILTERS[self.filter] == item.group
                    || self.filter == 6 && item.selected)
                    && (self.filter != 0 || item.parent_worktree.is_none() || item.selected)
                    && format!(
                        "{} {} {}",
                        item.label,
                        item.location,
                        item.included.join(" ")
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if self.size_sort {
            rows.sort_by(|&a, &b| {
                self.items[b]
                    .bytes
                    .cmp(&self.items[a].bytes)
                    .then(self.items[a].key.cmp(&self.items[b].key))
            });
        }
        rows
    }
    fn focused(&self) -> Option<usize> {
        self.visible().get(self.cursor).copied()
    }
    fn selected(&self) -> Vec<Item> {
        self.items.iter().filter(|i| i.selected).cloned().collect()
    }
    fn proposal_candidates(&self) -> Vec<Item> {
        let mut candidates: Vec<_> = self
            .visible()
            .into_iter()
            .map(|i| &self.items[i])
            .filter(|i| {
                i.blocked.is_none()
                    && !i.reviewing
                    && i.scope.is_some()
                    && !matches!(
                        i.action,
                        Action::Store(_) | Action::Volume { .. } | Action::Retained
                    )
            })
            .cloned()
            .collect();
        // Prefer the containing worktree over its generated children.
        candidates.sort_by_key(|i| i.scope.as_ref().unwrap().components().count());
        let mut result: Vec<Item> = Vec::new();
        for item in candidates {
            if !result.iter().any(|other| {
                item.scope
                    .as_ref()
                    .unwrap()
                    .starts_with(other.scope.as_ref().unwrap())
            }) {
                result.push(item);
            }
        }
        result
    }
    fn adopt_proposal(&mut self, proposed: &[Item]) {
        if proposed.is_empty() {
            self.status = "Empty proposal; existing selections kept.".into();
            return;
        }
        for item in &mut self.items {
            item.selected = false;
        }
        for draft in proposed {
            if let Some(item) = self
                .items
                .iter_mut()
                .find(|i| i.key == draft.key && i.blocked.is_none())
            {
                item.selected = true;
            }
        }
        self.filter = 6;
        self.query.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.status =
            "Proposal adopted. Space removes exceptions; r opens final deletion approval.".into();
    }
    fn toggle(&mut self) {
        let Some(index) = self.focused() else {
            return;
        };
        let item = &self.items[index];
        if item.selected {
            self.items[index].selected = false;
            return;
        }
        if let Some(reason) = &item.blocked {
            self.status = format!("Retained: {reason}");
            return;
        }
        if self.executing > 0 {
            return;
        }
        if self.items.iter().any(|i| {
            i.selected
                && (matches!(item.action, Action::Volume { .. })
                    || matches!(i.action, Action::Volume { .. }))
        }) {
            self.status =
                "Review and remove one volume at a time. Clear the other selections first.".into();
            return;
        }
        if let Some(scope) = &item.scope {
            if self.items.iter().any(|i| {
                i.selected
                    && i.scope
                        .as_ref()
                        .is_some_and(|p| scope.starts_with(p) || p.starts_with(scope))
            }) {
                self.status =
                    "Overlapping selections: select the checkout/store or its children separately."
                        .into();
                return;
            }
        }
        self.items[index].selected = true;
        self.status = "Selected for review. Nothing has been deleted.".into();
    }
    fn accept(&mut self, event: Event) {
        let focused = self.focused().map(|i| self.items[i].key.clone());
        match event {
            Event::Disk {
                available,
                used_percent,
            } => self.disk = Some((available, used_percent)),
            Event::Item(item) => self.items.push(*item),
            Event::Status(s) => self.status = s,
            Event::Issue(s) => {
                self.status = s.clone();
                self.issues.push(s);
            }
            Event::Done => {
                self.scanning = false;
                if self.issues.is_empty() {
                    self.status =
                        "Inventory ready. p builds a cleanup proposal; Space selects; r reviews deletion."
                            .into();
                } else {
                    self.status = format!(
                        "Inventory has {} inspection issues. Press ? for details.",
                        self.issues.len()
                    );
                }
            }
            Event::Advice(key, advice) => {
                if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
                    item.advice = advice;
                    item.reviewing = false;
                }
            }
            Event::Proposal(results) => {
                self.planning = false;
                let mut proposed = Vec::new();
                let total = results.len();
                for (key, reviews) in results {
                    if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
                        item.reviewing = false;
                        item.advice = reviews.iter().map(|r| r.text.clone()).collect();
                        if item.blocked.is_none()
                            && !reviews.is_empty()
                            && reviews.iter().all(|r| r.regenerable)
                        {
                            proposed.push(item.clone());
                        }
                    }
                }
                self.status = format!("Draft: {} proposed / {total} reviewed. Other items need investigation; inspect their advice.", proposed.len());
                self.popup = Some(Popup::Proposal(proposed));
                self.scroll = 0;
            }
            Event::Finished(key, result) => {
                self.executing = self.executing.saturating_sub(1);
                if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
                    item.selected = false;
                    match result {
                        Ok(message) => {
                            item.blocked = Some("Action completed; restart to rescan".into());
                            self.status = message;
                        }
                        Err(error) => {
                            item.blocked = Some(format!("Recheck refused: {error}"));
                            self.status = format!("Kept: {error}");
                        }
                    }
                }
            }
        }
        if let Some(key) = focused {
            if let Some(position) = self
                .visible()
                .iter()
                .position(|&i| self.items[i].key == key)
            {
                self.cursor = position;
            }
        }
        self.cursor = self.cursor.min(self.visible().len().saturating_sub(1));
    }
}
fn phrase(items: &[Item]) -> String {
    if let Some(Item {
        action: Action::Volume { name, .. },
        ..
    }) = items.first()
    {
        format!("DELETE VOLUME {name}")
    } else {
        format!("DELETE {}", items.len())
    }
}
fn confirmed(items: &[Item], input: &str) -> bool {
    !items.is_empty()
        && items.iter().all(|i| i.blocked.is_none())
        && input == phrase(items)
        && (!items
            .iter()
            .any(|i| matches!(i.action, Action::Volume { .. }))
            || items.len() == 1)
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
pub fn run(config: Config, home: PathBuf) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("The TUI needs an interactive terminal. Use scan or audit for reports.".into());
    }
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        previous(info);
    }));
    let (tx, rx) = mpsc::channel();
    let _scan = inventory::start(config.clone(), home.clone(), tx.clone());
    let mut app = App::new();
    loop {
        while let Ok(event) = rx.try_recv() {
            app.accept(event);
        }
        terminal.draw(|frame| draw(frame, &app, config.days))?;
        app.tick += 1;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let TerminalEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if terminal.size()?.width < 44 || terminal.size()?.height < 18 {
            if matches!(key.code, KeyCode::Char('q')) && app.executing == 0 {
                break;
            }
            if key.code != KeyCode::Char('c') || !key.modifiers.contains(KeyModifiers::CONTROL) {
                continue;
            }
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if app.executing > 0 {
                app.status =
                    "Cleanup is running; wait for the manager to finish before quitting.".into();
                continue;
            }
            break;
        }
        if app.popup.is_some() {
            match key.code {
                KeyCode::Esc => {
                    app.popup = None;
                    app.scroll = 0;
                }
                KeyCode::PageDown | KeyCode::Down => app.scroll = app.scroll.saturating_add(5),
                KeyCode::PageUp | KeyCode::Up => app.scroll = app.scroll.saturating_sub(5),
                KeyCode::Char(c) => {
                    if let Some(Popup::Delete(_, input)) = &mut app.popup {
                        input.push(c);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(Popup::Delete(_, input)) = &mut app.popup {
                        input.pop();
                    }
                }
                KeyCode::Enter => match app.popup.take().unwrap() {
                    popup @ (Popup::Ai(_) | Popup::PlanRequest(_)) => {
                        let proposal = matches!(popup, Popup::PlanRequest(_));
                        let items = match popup {
                            Popup::Ai(items) | Popup::PlanRequest(items) => items,
                            _ => unreachable!(),
                        };
                        app.scroll = 0;
                        app.planning = proposal;
                        for item in &items {
                            if let Some(current) = app.items.iter_mut().find(|i| i.key == item.key)
                            {
                                current.reviewing = true;
                            }
                        }
                        start_reviews(
                            items,
                            advice::configured(),
                            config.days,
                            tx.clone(),
                            proposal,
                        );
                        app.status = "Decision reviews running (four workers). No selection or deletion has been approved.".into();
                    }
                    Popup::Proposal(items) => app.adopt_proposal(&items),
                    Popup::Delete(items, input) if confirmed(&items, &input) => {
                        app.scroll = 0;
                        app.executing = items.len();
                        app.status = "Rechecking each selection before removal".into();
                        let tx = tx.clone();
                        let config = config.clone();
                        let home = home.clone();
                        std::thread::spawn(move || {
                            for item in items {
                                let result = inventory::execute(&item, &config, &home)
                                    .map_err(|e| e.to_string());
                                let _ = tx.send(Event::Finished(item.key, result));
                            }
                        });
                    }
                    Popup::Delete(items, input) => {
                        app.status = "Confirmation does not match. Nothing deleted.".into();
                        app.popup = Some(Popup::Delete(items, input));
                    }
                    Popup::Info(_) => app.scroll = 0,
                },
                _ => {}
            }
            continue;
        }
        if app.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => app.searching = false,
                KeyCode::Backspace => {
                    app.query.pop();
                    app.cursor = 0;
                }
                KeyCode::Char(c) => {
                    app.query.push(c);
                    app.cursor = 0;
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') => {
                if app.executing == 0 {
                    break;
                } else {
                    app.status = "Wait for cleanup to complete before quitting.".into();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.cursor = (app.cursor + 1).min(app.visible().len().saturating_sub(1));
                app.scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.cursor = app.cursor.saturating_sub(1);
                app.scroll = 0;
            }
            KeyCode::Home => {
                app.cursor = 0;
                app.scroll = 0;
            }
            KeyCode::End => {
                app.cursor = app.visible().len().saturating_sub(1);
                app.scroll = 0;
            }
            KeyCode::Tab => {
                app.filter = (app.filter + 1) % FILTERS.len();
                app.cursor = 0;
                app.scroll = 0;
            }
            KeyCode::BackTab => {
                app.filter = (app.filter + FILTERS.len() - 1) % FILTERS.len();
                app.cursor = 0;
                app.scroll = 0;
            }
            KeyCode::Char('/') => app.searching = true,
            KeyCode::Esc => {
                app.query.clear();
                app.cursor = 0;
            }
            KeyCode::Char('s') => {
                app.size_sort = !app.size_sort;
                app.cursor = 0;
            }
            KeyCode::Char(' ') => app.toggle(),
            KeyCode::PageDown => app.scroll = app.scroll.saturating_add(8),
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(8),
            KeyCode::Char('p') => {
                if app.planning || app.executing > 0 || app.items.iter().any(|i| i.reviewing) {
                    app.status = "Wait for pending reviews or cleanup to finish.".into();
                } else if advice::configured().is_empty() {
                    app.popup = Some(Popup::Info(advice::SETUP.into()));
                } else {
                    let items = app.proposal_candidates();
                    if items.is_empty() {
                        app.status = "No eligible path candidates in this view. Local blockers and persistent stores are excluded.".into();
                    } else if items.len() > 512 {
                        app.popup = Some(Popup::Info("More than 512 candidates. Narrow the category or search to bound provider costs.".into()));
                    } else {
                        app.popup = Some(Popup::PlanRequest(items));
                    }
                }
                app.scroll = 0;
            }
            KeyCode::Char('a') => {
                let mut items = app.selected();
                if items.is_empty() {
                    if let Some(i) = app.focused() {
                        items.push(app.items[i].clone());
                    }
                }
                if app.planning || app.executing > 0 || app.items.iter().any(|i| i.reviewing) {
                    app.status = "Wait for pending reviews or cleanup to finish.".into();
                    continue;
                }
                if advice::configured().is_empty() {
                    app.popup = Some(Popup::Info(advice::SETUP.into()));
                } else if items.len() > 8 {
                    app.popup = Some(Popup::Info("Review up to eight items at a time. Reduce your selection to bound provider calls and cost.".into()));
                } else if !items.is_empty() {
                    app.popup = Some(Popup::Ai(items));
                }
                app.scroll = 0;
            }
            KeyCode::Char('r') => {
                let items = app.selected();
                if app.planning || app.executing > 0 || items.iter().any(|i| i.reviewing) {
                    app.status = "Wait for the pending review or cleanup to finish.".into();
                } else if items.is_empty() {
                    app.status = "Select an item with Space first.".into();
                } else {
                    app.popup = Some(Popup::Delete(items, String::new()));
                    app.scroll = 0;
                }
            }
            KeyCode::Char('?') => {
                app.popup = Some(Popup::Info(format!("Inspection issues:\n{}\n\nSpace selects a candidate. Tab changes category. / searches; Esc clears. s changes sorting. PgUp/PgDn scroll details.\n\na requests AI review of selected items, or the highlighted item. You approve metadata sharing first. p reviews eligible items in the current view and builds a draft proposal. Enter adopts it; Space removes exceptions. Models never approve or execute cleanup.\n\nr shows exact selections and consequences. Type the requested phrase and press Enter to delete. Esc cancels. Local checks are repeated first. Worktrees use Git without --force; volumes are confirmed individually.\n\nThe scheduled job produces reports only. Sizes can overlap across stores, areas and children. Unknown sizes are not zero. Some processes deny inspection; protect service paths with --exclude or .dev-sweep-keep.\n\nq exits. For privileged storage sizes, run dev-sweep audit --system outside the TUI.", if app.issues.is_empty() { "None reported so far".into() } else {app.issues.join("\n")})));
                app.scroll = 0;
            }
            _ => {}
        }
    }
    terminal.show_cursor()?;
    Ok(())
}
fn start_reviews(
    items: Vec<Item>,
    providers: Vec<advice::Provider>,
    days: u64,
    tx: mpsc::Sender<Event>,
    proposal: bool,
) {
    std::thread::spawn(move || {
        let results = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for worker in 0..items.len().min(4) {
                let items = &items;
                let providers = &providers;
                let tx = &tx;
                let results = &results;
                scope.spawn(move || {
                    for item in items.iter().skip(worker).step_by(4) {
                        let reviews: Vec<_> = providers
                            .iter()
                            .map(|p| advice::review(p.clone(), item.decision_evidence(days)))
                            .collect();
                        if proposal {
                            results.lock().unwrap().push((item.key.clone(), reviews));
                        } else {
                            let _ = tx.send(Event::Advice(
                                item.key.clone(),
                                reviews.into_iter().map(|r| r.text).collect(),
                            ));
                        }
                    }
                });
            }
        });
        if proposal {
            let _ = tx.send(Event::Proposal(results.into_inner().unwrap()));
        }
    });
}
fn safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n'
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)
            {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .border_style(Style::new().fg(Color::Rgb(65, 89, 105)))
        .style(Style::new().bg(PANEL).fg(INK))
}
fn details(item: &Item) -> String {
    let location = match &item.scope {
        Some(path) if matches!(item.action, Action::Store(_)) => {
            format!("{}\nScope: {path:?}", item.location)
        }
        _ => item.location.clone(),
    };
    let activity = item
        .snapshot
        .as_ref()
        .and_then(|s| s.2.newest)
        .and_then(|time| SystemTime::now().duration_since(time).ok())
        .map(|age| {
            format!(
                "\nNewest observed activity: {} days ago",
                age.as_secs() / 86400
            )
        })
        .unwrap_or_default();
    let mut text = format!("{}\n{}\n\nLOCAL CHECKS\n{}{}\n\nSIZE\n{}\n{}\n\nWHAT DELETION DOES\n{}\n\nAI SAFETY REVIEW\n",item.label,location,item.blocked.as_deref().unwrap_or("Eligible for your review. Local checks found no blocker; this is not a guarantee of safety."),activity,item.bytes.map(human_size).unwrap_or_else(||"Unknown".into()),item.size_note,item.consequence);
    if !item.included.is_empty() {
        let included = format!(
            "INCLUDED WITH WORKTREE REMOVAL\n{}\n\n",
            item.included.join("\n")
        );
        text = text.replace(
            "AI SAFETY REVIEW\n",
            &format!("{included}AI SAFETY REVIEW\n"),
        );
    }
    if let Some(parent) = &item.parent_worktree {
        text = text.replace("WHAT DELETION DOES\n", &format!("PART OF WORKTREE\n{parent:?}\nRemoving that worktree includes this folder. Select this cache separately only to keep the checkout.\n\nWHAT DELETION DOES\n"));
    }
    if item.reviewing {
        text.push_str("Reviewing metadata...\n");
    } else if item.advice.is_empty() {
        text.push_str("Unreviewed. Press a to ask configured models.\n");
    } else {
        text.push_str(&item.advice.join("\n\n"));
        text.push_str("\n\nCompare model answers. Disagreement or low confidence warrants investigation. Advice cannot override local blockers.");
    }
    text.push_str("\n\nNothing is removed until you select it, review the selection, type the confirmation and press Enter.");
    safe(&text)
}
fn draw(frame: &mut Frame, app: &App, days: u64) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(BG).fg(INK)), area);
    if area.width < 44 || area.height < 18 {
        frame.render_widget(Paragraph::new("dev-sweep\nEnlarge this terminal to at least 44 x 18.\nq quits; no cleanup is triggered.").style(Style::new().fg(INK)),area);
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(3),
        Constraint::Length(2),
    ])
    .margin(1)
    .split(area);
    let selected = app.selected();
    let bytes: u64 = selected
        .iter()
        .filter(|i| {
            matches!(
                i.action,
                Action::Artifact(_) | Action::Global { .. } | Action::Worktree(_)
            )
        })
        .filter_map(|i| i.bytes)
        .sum();
    let state = if app.executing > 0 {
        "CLEANUP RUNNING"
    } else if app.scanning {
        "SCANNING"
    } else {
        "READY"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("dev-sweep", Style::new().fg(ACCENT).bold()),
                Span::raw("   storage review"),
                Span::styled(format!("    {state}"), Style::new().fg(MUTED)),
            ]),
            Line::from(format!(
                "{} items  |  {} selected  |  {} path estimate  |  {}d idle policy",
                app.items.len(),
                selected.len(),
                human_size(bytes),
                days
            )),
            Line::styled(
                "Daily scheduling is report-only. You approve every cleanup.",
                Style::new().fg(MUTED),
            ),
            Line::styled(
                app.disk
                    .map(|(free, used)| {
                        format!("Disk {used}% used   {} available", human_size(free))
                    })
                    .unwrap_or_else(|| "Reading disk capacity...".into()),
                Style::new().fg(WARN),
            ),
        ]),
        chunks[0],
    );
    let tabs = FILTERS
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Line::styled(
                *s,
                Style::new().fg(if i == app.filter { ACCENT } else { MUTED }),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Tabs::new(tabs)
            .select(app.filter)
            .divider("  ")
            .highlight_style(Style::new().fg(ACCENT).bold()),
        chunks[1],
    );
    let body = if area.width >= 100 {
        Layout::horizontal([Constraint::Percentage(54), Constraint::Percentage(46)])
            .split(chunks[2])
    } else {
        Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).split(chunks[2])
    };
    let visible = app.visible();
    let rows = visible
        .iter()
        .map(|&i| {
            let item = &app.items[i];
            let state = if item.selected {
                "[x]"
            } else if item.blocked.is_some() {
                "[-]"
            } else {
                "[ ]"
            };
            let size = item.bytes.map(human_size).unwrap_or_else(|| {
                if item.group == "Docker" {
                    "managed".into()
                } else {
                    "unknown".into()
                }
            });
            let color = if item.selected {
                ACCENT
            } else if item.blocked.is_some() {
                MUTED
            } else if matches!(item.action, Action::Volume { .. }) {
                WARN
            } else {
                INK
            };
            let path = match &item.scope {
                Some(p) => {
                    let displayed = if matches!(item.action, Action::Store(_)) {
                        p.as_path()
                    } else {
                        p.parent().unwrap_or(p)
                    };
                    if let Some(home) = env::var_os("HOME") {
                        match displayed.strip_prefix(Path::new(&home)) {
                            Ok(relative) => format!("~/{}", relative.to_string_lossy()),
                            Err(_) => format!("{displayed:?}"),
                        }
                    } else {
                        format!("{displayed:?}")
                    }
                }
                None => item.location.clone(),
            };
            ListItem::new(vec![
                Line::styled(
                    format!("{state}  {size:>10}  {}", safe(&item.label)),
                    Style::new().fg(color),
                ),
                Line::styled(format!("     {}", safe(&path)), Style::new().fg(MUTED)),
            ])
        })
        .collect::<Vec<_>>();
    let title = format!(
        " Inventory · {} · {} ",
        visible.len(),
        if app.size_sort {
            "largest first"
        } else {
            "discovery order"
        }
    );
    let list = List::new(rows)
        .block(panel(&title))
        .highlight_style(Style::new().bg(Color::Rgb(35, 58, 73)))
        .highlight_symbol("› ");
    let mut state = ListState::default().with_selected(if visible.is_empty() {
        None
    } else {
        Some(app.cursor)
    });
    frame.render_stateful_widget(list, body[0], &mut state);
    let detail = app.focused().map(|i|details(&app.items[i])).unwrap_or_else(||if app.scanning { "Discovering storage...\n\nInventory arrives as each area is measured. You can browse while scanning.".into() } else { "No matching items. Change the category or clear search with Esc.".into() });
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(panel(" Details · PgUp/PgDn ")),
        body[1],
    );
    let spinner = if app.scanning {
        ["◐", "◓", "◑", "◒"][(app.tick / 3) % 4]
    } else {
        "•"
    };
    let status = if app.searching {
        format!("Search: {}▏", app.query)
    } else {
        format!(
            "{spinner} {}{}",
            safe(&app.status),
            if app.query.is_empty() {
                String::new()
            } else {
                format!("  / {}", safe(&app.query))
            }
        )
    };
    frame.render_widget(
        Paragraph::new(status)
            .wrap(Wrap { trim: false })
            .style(Style::new().fg(WARN)),
        chunks[3],
    );
    frame.render_widget(Paragraph::new("↑↓ move   Space select   Tab category   / search   a AI review   p cleanup proposal\nr review deletion   s sort   ? help   q quit").style(Style::new().fg(MUTED)),chunks[4]);
    if let Some(popup) = &app.popup {
        let modal = Rect {
            x: area.x + area.width / 16,
            y: area.y + area.height / 12,
            width: area.width - area.width / 8,
            height: area.height - area.height / 6,
        };
        frame.render_widget(Clear, modal);
        let (title, text, input) = match popup {
            Popup::Info(s) => (" Help / setup ", s.clone(), None),
            Popup::Ai(items) | Popup::PlanRequest(items) => {
                let providers = advice::configured()
                    .into_iter()
                    .map(|p| p.label)
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut text = format!("Send cleanup metadata for {} item(s) to:\n{}\n\nIncludes paths/names, sizes, category, local blockers, planned action and consequences. File contents and API keys are not included in the prompt. Provider calls may incur charges.\n\nUp to four concurrent calls; one per item per provider. Long metadata is refused rather than truncated. No files or selections change. Enter approves metadata sharing only.\n\nMETADATA TO SEND\n",items.len(),providers);
                for item in items {
                    text.push_str(
                        &serde_json::to_string_pretty(&item.decision_evidence(days)).unwrap(),
                    );
                    text.push_str("\n\n");
                }
                (" Request AI safety advice ", text, None)
            }
            Popup::Proposal(items) => {
                let mut text = format!("{}\n\n{} item(s) proposed for your review. Enter REPLACES your current selections with this draft. Esc keeps existing selections. Nothing is deleted. Then remove exceptions with Space and press r for final approval.\n\nModel agreement is not proof of safety. Native stores and volumes require separate manual review.\n\n", app.status, items.len());
                for item in items {
                    text.push_str(&format!(
                        "{} · {}\n{}\n{}\n{}\n\n",
                        item.label,
                        item.bytes
                            .map(human_size)
                            .unwrap_or_else(|| "Unknown size".into()),
                        item.location,
                        item.consequence,
                        item.advice.join("\n")
                    ));
                }
                (
                    " Cleanup proposal · Enter adopts / Esc cancels ",
                    text,
                    None,
                )
            }
            Popup::Delete(items, input) => {
                let mut text = format!("Permanently remove {} selected item(s). There is no undo. Each action is rechecked immediately before execution.\n\n",items.len());
                for item in items {
                    let included = if item.included.is_empty() {
                        String::new()
                    } else {
                        format!("\nIncludes:\n{}", item.included.join("\n"))
                    };
                    text.push_str(&format!(
                        "{}\n{}\nScope: {}\n{}{}\nAI: {}\n\n",
                        item.label,
                        item.location,
                        item.scope
                            .as_ref()
                            .map(|p| format!("{p:?}"))
                            .unwrap_or_else(|| "Native Docker action".into()),
                        item.consequence,
                        included,
                        if item.advice.is_empty() {
                            "UNREVIEWED".into()
                        } else {
                            item.advice.join("\n")
                        }
                    ));
                }
                text.push_str(&format!(
                    "To approve these exact selections, type:\n{}",
                    phrase(items)
                ));
                (" Review deletion ", text, Some(input))
            }
        };
        frame.render_widget(
            panel(title).border_style(Style::new().fg(if input.is_some() { WARN } else { ACCENT })),
            modal,
        );
        let inner = modal.inner(Margin {
            horizontal: 2,
            vertical: 1,
        });
        let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
        frame.render_widget(
            Paragraph::new(safe(&text))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            parts[0],
        );
        let footer = match input {
            Some(input) => format!(
                "Type: {}\n> {}▏\nEnter approves. Esc cancels. PgUp/PgDn scroll.",
                if let Popup::Delete(items, _) = popup {
                    phrase(items)
                } else {
                    String::new()
                },
                safe(input)
            ),
            None => "Enter continues. Esc cancels. PgUp/PgDn scroll.".into(),
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::new().fg(ACCENT)),
            parts[1],
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn item() -> Item {
        Item {
            key: "cache".into(),
            label: "node_modules".into(),
            location: "/tmp/project/node_modules".into(),
            group: "Caches",
            bytes: Some(4096),
            size_note: "Allocated".into(),
            blocked: None,
            consequence: "Reinstall dependencies".into(),
            action: Action::Artifact("/tmp/project/node_modules".into()),
            scope: Some("/tmp/project/node_modules".into()),
            snapshot: None,
            selected: false,
            advice: Vec::new(),
            reviewing: false,
            parent_worktree: None,
            included: Vec::new(),
        }
    }
    #[test]
    fn approval_requires_exact_selection_and_local_permission() {
        let mut a = App::new();
        a.items.push(item());
        assert!(!confirmed(&a.selected(), "DELETE 0"));
        a.toggle();
        assert!(confirmed(&a.selected(), "DELETE 1"));
        assert!(!confirmed(&a.selected(), "yes"));
        assert!(!confirmed(&a.selected(), "DELETE 2"));
        a.items[0].blocked = Some("active".into());
        assert!(!confirmed(&a.selected(), "DELETE 1"));
        a.items[0].selected = false;
        a.toggle();
        assert!(a.selected().is_empty());
    }
    #[test]
    fn worktree_bundles_are_visible_searchable_and_cannot_overlap() {
        let mut cache = item();
        cache.parent_worktree = Some("/tmp/project".into());
        let mut tree = item();
        tree.key = "worktree".into();
        tree.label = "Worktree: project".into();
        tree.location = "/tmp/project".into();
        tree.group = "Worktrees";
        tree.bytes = Some(8192);
        tree.action = Action::Worktree("/tmp/project".into());
        tree.scope = Some("/tmp/project".into());
        tree.included = vec!["node_modules: /tmp/project/node_modules".into()];
        let mut app = App::new();
        app.items = vec![tree, cache];
        assert_eq!(app.visible(), [0]);
        app.query = "node_modules".into();
        assert_eq!(app.visible(), [0], "search should find bundled artifacts");
        app.filter = 1;
        assert_eq!(app.visible(), [1], "Caches permits separate cleanup");
        app.toggle();
        app.filter = 0;
        assert_eq!(app.visible(), [0, 1], "a selected child stays visible");
        app.toggle();
        assert_eq!(app.selected().len(), 1);
        assert!(app.status.contains("Overlapping"));
        app.filter = 1;
        app.toggle();
        app.filter = 0;
        app.toggle();
        let selected = app.selected();
        assert_eq!(selected.len(), 1);
        assert!(matches!(selected[0].action, Action::Worktree(_)));
        assert_eq!(
            selected[0].evidence(30)["included_generated_artifacts"][0],
            "node_modules: /tmp/project/node_modules"
        );
        assert!(confirmed(&selected, "DELETE 1"));
    }
    #[test]
    fn proposal_requires_adoption_and_excludes_failed_reviews_and_blockers() {
        let mut app = App::new();
        let good = item();
        let mut failed = item();
        failed.key = "failed".into();
        failed.scope = Some("/tmp/other/node_modules".into());
        let mut blocked = item();
        blocked.key = "blocked".into();
        blocked.blocked = Some("active".into());
        let mut store = item();
        store.key = "store".into();
        store.action = Action::Store("uv".into());
        app.items = vec![good, failed, blocked, store];
        assert_eq!(app.proposal_candidates().len(), 2);
        let review = |regenerable| advice::Review {
            text: "fixture".into(),
            regenerable,
        };
        app.accept(Event::Proposal(vec![
            (app.items[0].key.clone(), vec![review(true)]),
            ("failed".into(), vec![review(true), review(false)]),
            ("blocked".into(), vec![review(true)]),
        ]));
        assert!(
            app.selected().is_empty(),
            "model output cannot adopt selections"
        );
        let Some(Popup::Proposal(proposed)) = app.popup.take() else {
            panic!("missing proposal")
        };
        assert_eq!(proposed.len(), 1);
        app.adopt_proposal(&proposed);
        assert_eq!(app.selected().len(), 1);
        assert!(confirmed(&app.selected(), "DELETE 1"));
        app.adopt_proposal(&[]);
        assert_eq!(app.selected().len(), 1);
        app.items[0].blocked = Some("changed".into());
        app.adopt_proposal(&proposed);
        assert!(app.selected().is_empty());
    }
    #[test]
    fn terminal_layout_shows_risk_and_handles_small_screens() {
        let mut app = App::new();
        app.items.push(item());
        app.scanning = false;
        for (width, height) in [(120, 36), (80, 30), (44, 14), (20, 6)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &app, 30)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("dev-sweep"));
            if width >= 80 {
                assert!(text.contains("Space select"));
            }
        }
        app.popup = Some(Popup::Delete(vec![item()], String::new()));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        terminal.draw(|f| draw(f, &app, 30)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("UNREVIEWED"));
        assert!(text.contains("DELETE 1"));
    }
}
