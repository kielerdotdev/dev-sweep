use super::*;
use crate::inventory::{Action, Event, Item};
use crossterm::{
    event::{self, Event as TerminalEvent, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Instant;
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
#[derive(Clone, Default)]
struct PlanReport {
    proposed: Vec<Item>,
    deferred: Vec<Item>,
    reviewed: usize,
    failed: usize,
}
impl PlanReport {
    fn chosen(&self) -> Vec<Item> {
        self.proposed
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect()
    }
}
enum Popup {
    Info(String),
    Ai(Vec<Item>),
    PlanRequest(Vec<Item>),
    Metadata { items: Vec<Item>, proposal: bool },
    NoCandidates(String),
    Proposal { report: PlanReport, deferred: bool },
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
    queued_plan: bool,
    providers: Vec<advice::Provider>,
    review_done: usize,
    review_total: usize,
    review_errors: usize,
    review_started: Option<Instant>,
    review_cancel: Option<Arc<AtomicBool>>,
    review_cancelled: bool,
    report: Option<PlanReport>,
    popup_cursor: usize,
    plan_confirmation: String,
    plan_error: String,
    scan_status: String,
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
            queued_plan: false,
            providers: Vec::new(),
            review_done: 0,
            review_total: 0,
            review_errors: 0,
            review_started: None,
            review_cancel: None,
            review_cancelled: false,
            report: None,
            popup_cursor: 0,
            plan_confirmation: String::new(),
            plan_error: String::new(),
            scan_status: "Discovering projects and worktrees".into(),
            status: "Press Enter or p to build a cleanup plan.".into(),
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
        let retained_worktrees: HashSet<_> = self
            .items
            .iter()
            .filter(|i| matches!(i.action, Action::Worktree(_)) && i.blocked.is_some())
            .filter_map(|i| i.scope.as_deref())
            .collect();
        let mut rows: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                (self.filter == 0
                    || FILTERS[self.filter] == item.group
                    || self.filter == 6 && item.selected)
                    && (self.filter != 0
                        || item.parent_worktree.is_none()
                        || item.selected
                        || item.parent_worktree.as_ref().is_some_and(|parent| {
                            item.blocked.is_none() && retained_worktrees.contains(parent.as_path())
                        }))
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
    fn begin_plan(&mut self, fresh: bool) {
        self.scroll = 0;
        self.popup_cursor = 0;
        self.plan_confirmation.clear();
        self.plan_error.clear();
        self.queued_plan = false;
        if self.planning {
            self.popup = Some(Popup::Info(format!("AI review is running: {} / {} complete.\n\nBrowse the inventory while reviews finish. Press Esc in the inventory to stop further requests. Nothing is selected or deleted.", self.review_done, self.review_total)));
        } else if self.executing > 0 || self.items.iter().any(|i| i.reviewing) {
            self.popup = Some(Popup::Info(
                "Wait for the current review or cleanup to finish. Nothing else was started."
                    .into(),
            ));
        } else if !fresh && self.report.is_some() {
            self.popup = Some(Popup::Proposal {
                report: self.report.clone().unwrap(),
                deferred: false,
            });
        } else if self.providers.is_empty() {
            self.popup = Some(Popup::Info(advice::SETUP.into()));
        } else {
            let items = self.proposal_candidates();
            if items.is_empty() {
                let mut reasons = std::collections::BTreeMap::new();
                for i in self.visible() {
                    if let Some(reason) = &self.items[i].blocked {
                        *reasons.entry(reason.clone()).or_insert(0usize) += 1;
                    }
                }
                let mut text = format!("No eligible cleanup candidates in this view yet.\n\n{} inspected items. Retained worktrees can still contain eligible caches; those appear as inspection finishes.\n\n", self.visible().len());
                for (reason, count) in reasons {
                    text.push_str(&format!("{count} retained: {reason}\n"));
                }
                text.push_str(if self.scanning { "\nThe scan is still running. Enter waits for the remaining inventory, then opens the metadata approval. No model requests start while waiting." } else { "\nThe scan is complete. Try All or Caches, or clear search with Esc. The AI cannot override age, Git, process or path protection checks. Native stores and Docker volumes require separate manual review. No files or selections changed." });
                self.popup = Some(Popup::NoCandidates(text));
            } else if items.len() > 512 {
                self.popup = Some(Popup::Info("More than 512 candidates. Use a category or search to reduce provider calls, then press p again.".into()));
            } else {
                self.popup = Some(Popup::PlanRequest(items));
            }
        }
    }
    fn review_progress(&self) -> String {
        format!(
            "{}: {} / {} complete | {} failed | {}s elapsed. No deletion approved.",
            if self.review_cancelled {
                "Stopping AI review; finishing in-flight requests"
            } else {
                "AI review"
            },
            self.review_done,
            self.review_total,
            self.review_errors,
            self.review_started
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0)
        )
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
            Event::Status(s) => self.scan_status = s,
            Event::Issue(s) => {
                self.scan_status = s.clone();
                self.issues.push(s);
            }
            Event::Done => {
                self.scanning = false;
                self.scan_status = format!(
                    "Inventory complete · {} inspection issues",
                    self.issues.len()
                );
                if self.queued_plan && self.popup.is_none() && self.executing == 0 {
                    self.queued_plan = false;
                    self.begin_plan(true);
                }
            }
            Event::ReviewProgress { key, reviews } => {
                self.review_done += 1;
                if reviews.iter().any(|r| r.failed) {
                    self.review_errors += 1;
                }
                if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
                    item.advice = reviews.iter().map(|r| r.text.clone()).collect();
                    item.reviewing = false;
                }
                if !self.planning && self.review_done >= self.review_total {
                    self.review_cancel = None;
                    self.status = "AI review finished. Results are in the details pane; p builds a cleanup plan.".into();
                }
            }
            Event::Proposal(results) => {
                self.planning = false;
                self.review_cancel = None;
                let mut report = PlanReport {
                    reviewed: results.len(),
                    ..PlanReport::default()
                };
                for (key, reviews) in results {
                    if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
                        item.reviewing = false;
                        item.advice = reviews.iter().map(|r| r.text.clone()).collect();
                        let failed = reviews.is_empty() || reviews.iter().any(|r| r.failed);
                        if failed {
                            report.failed += 1;
                        }
                        if item.blocked.is_none()
                            && !reviews.is_empty()
                            && reviews.iter().all(|r| r.regenerable)
                        {
                            let mut draft = item.clone();
                            draft.selected = true;
                            report.proposed.push(draft);
                        } else {
                            report.deferred.push(item.clone());
                        }
                    }
                }
                report
                    .proposed
                    .sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.key.cmp(&b.key)));
                self.status = format!("AI review complete: {} proposed, {} need review, {} failed. Enter opens the plan; n starts a new review.", report.proposed.len(), report.deferred.len(), report.failed);
                self.report = Some(report.clone());
                if !self.review_cancelled {
                    self.popup = Some(Popup::Proposal {
                        report,
                        deferred: false,
                    });
                }
                self.scroll = 0;
                self.popup_cursor = 0;
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
    run_with_providers(config, home, advice::configured())
}
fn run_with_providers(
    config: Config,
    home: PathBuf,
    providers: Vec<advice::Provider>,
) -> Result<()> {
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
    app.providers = providers;
    loop {
        while let Ok(event) = rx.try_recv() {
            app.accept(event);
        }
        if app.queued_plan
            && !app.scanning
            && app.popup.is_none()
            && app.executing == 0
            && app.review_cancel.is_none()
        {
            app.begin_plan(true);
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
                    if let Some(Popup::Proposal { report, .. }) = &app.popup {
                        app.report = Some(report.clone());
                    }
                    app.popup = None;
                    app.scroll = 0;
                }
                KeyCode::Down | KeyCode::Up
                    if matches!(app.popup, Some(Popup::Proposal { .. })) =>
                {
                    if let Some(Popup::Proposal { report, deferred }) = &app.popup {
                        let len = if *deferred {
                            report.deferred.len()
                        } else {
                            report.proposed.len()
                        };
                        app.popup_cursor = if key.code == KeyCode::Down {
                            (app.popup_cursor + 1).min(len.saturating_sub(1))
                        } else {
                            app.popup_cursor.saturating_sub(1)
                        };
                        app.scroll = 0;
                    }
                }
                KeyCode::Tab if matches!(app.popup, Some(Popup::Proposal { .. })) => {
                    if let Some(Popup::Proposal { deferred, .. }) = &mut app.popup {
                        *deferred = !*deferred;
                    }
                    app.popup_cursor = 0;
                    app.scroll = 0;
                }
                KeyCode::Char(' ')
                    if matches!(app.popup, Some(Popup::Proposal { .. }))
                        && app.plan_confirmation != "DELETE" =>
                {
                    if let Some(Popup::Proposal {
                        report,
                        deferred: false,
                    }) = &mut app.popup
                    {
                        if let Some(item) = report.proposed.get_mut(app.popup_cursor) {
                            item.selected = !item.selected;
                        }
                    }
                    app.plan_confirmation.clear();
                    app.plan_error.clear();
                }
                KeyCode::Char(c)
                    if matches!(app.popup, Some(Popup::Proposal { .. }))
                        && (!app.plan_confirmation.is_empty() || c == 'D') =>
                {
                    app.plan_confirmation.push(c);
                    app.plan_error.clear();
                }
                KeyCode::Backspace if matches!(app.popup, Some(Popup::Proposal { .. })) => {
                    app.plan_confirmation.pop();
                    app.plan_error.clear();
                }
                KeyCode::Char(' ') if matches!(app.popup, Some(Popup::Proposal { .. })) => {
                    if let Some(Popup::Proposal {
                        report,
                        deferred: false,
                    }) = &mut app.popup
                    {
                        if let Some(item) = report.proposed.get_mut(app.popup_cursor) {
                            item.selected = !item.selected;
                        }
                    }
                }
                KeyCode::Char('n') if matches!(app.popup, Some(Popup::Proposal { .. })) => {
                    app.begin_plan(true)
                }
                KeyCode::Char('m')
                    if matches!(
                        app.popup,
                        Some(Popup::Ai(_) | Popup::PlanRequest(_) | Popup::Metadata { .. })
                    ) =>
                {
                    app.popup = Some(match app.popup.take().unwrap() {
                        Popup::Ai(items) => Popup::Metadata {
                            items,
                            proposal: false,
                        },
                        Popup::PlanRequest(items) => Popup::Metadata {
                            items,
                            proposal: true,
                        },
                        Popup::Metadata {
                            items,
                            proposal: true,
                        } => Popup::PlanRequest(items),
                        Popup::Metadata {
                            items,
                            proposal: false,
                        } => Popup::Ai(items),
                        _ => unreachable!(),
                    });
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
                    popup @ (Popup::Ai(_) | Popup::PlanRequest(_) | Popup::Metadata { .. }) => {
                        let proposal = matches!(
                            popup,
                            Popup::PlanRequest(_) | Popup::Metadata { proposal: true, .. }
                        );
                        let items = match popup {
                            Popup::Ai(items)
                            | Popup::PlanRequest(items)
                            | Popup::Metadata { items, .. } => items,
                            _ => unreachable!(),
                        };
                        app.scroll = 0;
                        app.planning = proposal;
                        app.review_done = 0;
                        app.review_total = items.len();
                        app.review_errors = 0;
                        app.review_started = Some(Instant::now());
                        app.review_cancelled = false;
                        for item in &items {
                            if let Some(current) = app.items.iter_mut().find(|i| i.key == item.key)
                            {
                                current.reviewing = true;
                            }
                        }
                        app.review_cancel = Some(start_reviews(
                            items,
                            app.providers.clone(),
                            config.days,
                            tx.clone(),
                            proposal,
                        ));
                        app.status = "Decision reviews running (four workers). No selection or deletion has been approved.".into();
                    }
                    Popup::Proposal { report, deferred } => {
                        let chosen = report.chosen();
                        app.report = Some(report.clone());
                        if confirmed(&chosen, &app.plan_confirmation) {
                            app.adopt_proposal(&chosen);
                            begin_cleanup(&mut app, chosen, &config, &home, &tx);
                            app.plan_confirmation.clear();
                        } else {
                            app.plan_error = if chosen.is_empty() {
                                "No files checked. Nothing deleted.".into()
                            } else {
                                format!(
                                    "Nothing deleted. Enter the exact phrase {}.",
                                    phrase(&chosen)
                                )
                            };
                            app.status = if chosen.is_empty() {
                                "No files chosen. Tab shows review outcomes; n starts a new plan."
                                    .into()
                            } else {
                                format!("Nothing deleted. Type {} and press Enter to approve these exact files.", phrase(&chosen))
                            };
                            app.popup = Some(Popup::Proposal { report, deferred });
                        }
                    }
                    Popup::NoCandidates(_) => {
                        if app.scanning {
                            app.queued_plan = true;
                            app.status = "Waiting for remaining inventory. A metadata approval will open when the scan finishes.".into();
                        } else {
                            app.filter = 0;
                            app.query.clear();
                            app.cursor = 0;
                            app.status =
                                "Showing All. Age and protection checks remain in force.".into();
                        }
                        app.scroll = 0;
                    }
                    Popup::Delete(items, input) if confirmed(&items, &input) => {
                        begin_cleanup(&mut app, items, &config, &home, &tx);
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
                if let Some(cancel) = &app.review_cancel {
                    cancel.store(true, Ordering::Relaxed);
                    app.review_cancelled = true;
                    app.status = "Stopping further AI requests. Waiting for in-flight requests to finish; no deletion approved.".into();
                }
                app.queued_plan = false;
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
            KeyCode::Char('p') | KeyCode::Enter => app.begin_plan(false),
            KeyCode::Char('n') => app.begin_plan(true),
            KeyCode::Char('a') => {
                app.queued_plan = false;
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
                if app.providers.is_empty() {
                    app.popup = Some(Popup::Info(advice::SETUP.into()));
                } else if items.len() > 8 {
                    app.popup = Some(Popup::Info("Review up to eight items at a time. Reduce your selection to bound provider calls and cost.".into()));
                } else if !items.is_empty() {
                    app.popup = Some(Popup::Ai(items));
                }
                app.scroll = 0;
            }
            KeyCode::Char('r') => {
                app.queued_plan = false;
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
                app.popup = Some(Popup::Info(format!("Inspection issues:\n{}\n\nSpace selects a candidate. Tab changes category. / searches; Esc clears. s changes sorting. PgUp/PgDn scroll details.\n\na requests AI review of selected items, or the highlighted item. You approve metadata sharing first. p or Enter builds a plan or reopens the last plan. n starts a fresh plan. During scan, an empty plan can wait for remaining inventory. The review screen shows progress and errors. Space removes exceptions in the draft; Type the displayed DELETE phrase in the plan and press Enter to approve exact deletion. Models never approve or execute cleanup.\n\nr shows exact selections and consequences. Type the requested phrase and press Enter to delete. Esc cancels. Local checks are repeated first. Worktrees use Git without --force; volumes are confirmed individually.\n\nThe scheduled job produces reports only. Sizes can overlap across stores, areas and children. Unknown sizes are not zero. Some processes deny inspection; protect service paths with --exclude or .dev-sweep-keep.\n\nq exits. For privileged storage sizes, run dev-sweep audit --system outside the TUI.", if app.issues.is_empty() { "None reported so far".into() } else {app.issues.join("\n")})));
                app.scroll = 0;
            }
            _ => {}
        }
    }
    terminal.show_cursor()?;
    Ok(())
}
fn begin_cleanup(
    app: &mut App,
    items: Vec<Item>,
    config: &Config,
    home: &Path,
    tx: &mpsc::Sender<Event>,
) {
    app.scroll = 0;
    app.executing = items.len();
    app.status = "Approval received. Rechecking every item before removal.".into();
    app.report = None;
    let tx = tx.clone();
    let config = config.clone();
    let home = home.to_path_buf();
    std::thread::spawn(move || {
        for item in items {
            let result = inventory::execute(&item, &config, &home).map_err(|e| e.to_string());
            let _ = tx.send(Event::Finished(item.key, result));
        }
    });
}
fn start_reviews(
    items: Vec<Item>,
    providers: Vec<advice::Provider>,
    days: u64,
    tx: mpsc::Sender<Event>,
    proposal: bool,
) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let stopped = cancel.clone();
    std::thread::spawn(move || {
        let results = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for worker in 0..items.len().min(4) {
                let items = &items;
                let providers = &providers;
                let tx = &tx;
                let results = &results;
                let stopped = &stopped;
                scope.spawn(move || {
                    for item in items.iter().skip(worker).step_by(4) {
                        let mut reviews = Vec::new();
                        for p in providers {
                            if stopped.load(Ordering::Relaxed) { break; }
                            let review = advice::review(p.clone(), item.decision_evidence(days));
                            if review.terminal_failure { stopped.store(true, Ordering::Relaxed); }
                            reviews.push(review);
                        }
                        if reviews.len() != providers.len() || reviews.is_empty() {
                            reviews.push(advice::Review { text: "UNREVIEWED: Batch stopped or cancelled. No recommendation was inferred. Start a new plan after resolving the provider error.".into(), regenerable: false, failed: true, terminal_failure: false });
                        }
                        let _ = tx.send(Event::ReviewProgress { key: item.key.clone(), reviews: reviews.clone() });
                        if proposal { results.lock().unwrap().push((item.key.clone(), reviews)); }
                    }
                });
            }
        });
        if proposal {
            let _ = tx.send(Event::Proposal(results.into_inner().unwrap()));
        }
    });
    cancel
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
    } else if app.review_cancel.is_some() {
        "AI REVIEW"
    } else if app.queued_plan {
        "WAITING FOR SCAN"
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
                if app.review_cancel.is_some() {
                    app.review_progress()
                } else if app.queued_plan {
                    "Plan queued. Waiting for inventory, then metadata approval.".into()
                } else if app.report.is_some() {
                    "[ Enter / p ] Review last cleanup plan    [ n ] Build a new plan".into()
                } else {
                    format!(
                        "[ Enter / p ] Build cleanup plan    {} eligible candidates in this view",
                        app.proposal_candidates().len()
                    )
                },
                Style::new().fg(ACCENT).bold(),
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
            safe(&if app.review_cancel.is_some() {
                app.review_progress()
            } else if app.status.is_empty() {
                app.scan_status.clone()
            } else {
                app.status.clone()
            }),
            if app.query.is_empty() {
                String::new()
            } else {
                format!("  / {}", safe(&app.query))
            }
        )
    };
    frame.render_widget(
        Paragraph::new(format!("{status}\n{}", safe(&app.scan_status)))
            .wrap(Wrap { trim: false })
            .style(Style::new().fg(WARN)),
        chunks[3],
    );
    frame.render_widget(Paragraph::new("Enter / p cleanup plan   n new plan   ↑↓ browse   Tab category   / search\nSpace select   r review selections   a single AI review   ? help   q quit").style(Style::new().fg(MUTED)),chunks[4]);
    if let Some(popup) = &app.popup {
        let modal = Rect {
            x: area.x + area.width / 16,
            y: area.y + area.height / 12,
            width: area.width - area.width / 8,
            height: area.height - area.height / 6,
        };
        frame.render_widget(Clear, modal);
        if let Popup::Proposal { report, deferred } = popup {
            draw_proposal(frame, app, report, *deferred, modal);
            return;
        }
        let (title, text, input) = match popup {
            Popup::Info(s) => (" Help / setup ", s.clone(), None),
            Popup::Ai(items) | Popup::PlanRequest(items) | Popup::Metadata { items, .. } => {
                let providers = app
                    .providers
                    .iter()
                    .map(|p| p.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let bytes: u64 = items.iter().filter_map(|i| i.bytes).sum();
                let mut text = format!("Review {} cleanup candidates · {} estimated space\n\nModel: {}\n\nEnter starts {} provider requests, with up to four running at once. Requests may cost money. {}\n\nShared: paths, sizes, activity timestamps, local checks and removal consequences. File contents and keys are not sent.\n\nThis creates a draft only. You approve exact deletion separately. Press m to inspect the exact metadata.\n\nCANDIDATES\n", items.len(), human_size(bytes), providers, items.len()*app.providers.len(), if app.scanning { "The scan is still running; this reviews the current snapshot." } else { "Inventory is complete." });
                if matches!(popup, Popup::Metadata { .. }) {
                    text.push_str("\nEXACT METADATA\n");
                    for item in items {
                        text.push_str(
                            &serde_json::to_string_pretty(&item.decision_evidence(days)).unwrap(),
                        );
                        text.push_str("\n\n");
                    }
                } else {
                    for item in items {
                        text.push_str(&format!(
                            "{} · {}\n{}\n\n",
                            item.bytes
                                .map(human_size)
                                .unwrap_or_else(|| "Unknown size".into()),
                            item.label,
                            item.scope
                                .as_ref()
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_else(|| item.location.clone())
                        ));
                    }
                }
                (
                    if matches!(popup, Popup::Ai(_)) {
                        " Review with AI "
                    } else {
                        " Build cleanup plan "
                    },
                    text,
                    None,
                )
            }
            Popup::NoCandidates(text) => (" No eligible candidates yet ", text.clone(), None),
            Popup::Proposal { .. } => unreachable!(),
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
            None => match popup {
                Popup::Ai(_) | Popup::PlanRequest(_) | Popup::Metadata { .. } => "[ Enter ] Start AI review    [ m ] Metadata    [ Esc ] Cancel\nPgUp/PgDn scroll. No deletion is approved.".into(),
                Popup::NoCandidates(_) if app.scanning => "[ Enter ] Wait for remaining inventory    [ Esc ] Back\nNo model requests or deletion start while waiting.".into(),
                Popup::NoCandidates(_) => "[ Enter ] Show All    [ Esc ] Back\nLocal protections remain in force.".into(),
                _ => "[ Enter / Esc ] Back    PgUp/PgDn scroll.".into(),
            },
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::new().fg(ACCENT)),
            parts[1],
        );
    }
}
fn draw_proposal(frame: &mut Frame, app: &App, report: &PlanReport, deferred: bool, area: Rect) {
    frame.render_widget(
        panel(" Review cleanup plan ").border_style(Style::new().fg(ACCENT)),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let parts = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(4),
    ])
    .split(inner);
    let chosen = report.chosen();
    let bytes: u64 = chosen.iter().filter_map(|i| i.bytes).sum();
    let summary = vec![
        Line::styled(
            format!(
                "{} checked · {} estimated space · {} AI-reviewed",
                chosen.len(),
                human_size(bytes),
                report.reviewed
            ),
            Style::new().fg(ACCENT).bold(),
        ),
        Line::from(format!(
            "{} proposed · {} need review · {} failed",
            report.proposed.len(),
            report.deferred.len(),
            report.failed
        )),
        Line::styled(
            "Untick exceptions. Deletion is permanent.",
            Style::new().fg(WARN),
        ),
    ];
    frame.render_widget(Paragraph::new(summary), parts[0]);
    frame.render_widget(
        Paragraph::new(if deferred {
            "[Tab] Proposed files     > Needs review / errors"
        } else {
            "> Proposed files     [Tab] Needs review / errors"
        })
        .style(Style::new().fg(ACCENT)),
        parts[1],
    );
    let items = if deferred {
        &report.deferred
    } else {
        &report.proposed
    };
    if items.is_empty() {
        let error = report
            .deferred
            .iter()
            .flat_map(|i| &i.advice)
            .find(|s| s.contains("UNREVIEWED"));
        let text = if deferred {
            "Every reviewed item is in the proposed list. Tab returns to it.".into()
        } else {
            format!("No cleanup was recommended. No files or selections changed.\n\n{}\n\nTab shows the retained items and model outcomes. n starts a new review; Esc returns to inventory.", error.cloned().unwrap_or_else(|| "Models retained the candidates or requested investigation. Their recommendations cannot override local protections.".into()))
        };
        frame.render_widget(
            Paragraph::new(safe(&text)).wrap(Wrap { trim: false }),
            parts[2],
        );
    } else {
        let body = if inner.width >= 100 {
            Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(parts[2])
        } else {
            Layout::vertical([Constraint::Min(3), Constraint::Length(6)]).split(parts[2])
        };
        let rows: Vec<_> = items
            .iter()
            .map(|item| {
                ListItem::new(vec![
                    Line::styled(
                        format!(
                            "{}  {}  {}",
                            if deferred {
                                "[?]"
                            } else if item.selected {
                                "[x]"
                            } else {
                                "[ ]"
                            },
                            item.bytes
                                .map(human_size)
                                .unwrap_or_else(|| "Unknown".into()),
                            safe(&item.label)
                        ),
                        Style::new().fg(if deferred { WARN } else { INK }),
                    ),
                    Line::styled(
                        item.scope
                            .as_ref()
                            .map(|p| safe(&p.to_string_lossy()))
                            .unwrap_or_else(|| safe(&item.location)),
                        Style::new().fg(MUTED),
                    ),
                ])
            })
            .collect();
        let mut state =
            ListState::default().with_selected(Some(app.popup_cursor.min(items.len() - 1)));
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_symbol("› ")
                .highlight_style(Style::new().bg(Color::Rgb(35, 58, 73))),
            body[0],
            &mut state,
        );
        if let Some(item) = items.get(app.popup_cursor) {
            let detail = format!(
                "{}\n\n{}\n\n{}",
                item.location,
                item.consequence,
                item.advice.join("\n\n")
            );
            frame.render_widget(
                Paragraph::new(safe(&detail))
                    .wrap(Wrap { trim: false })
                    .scroll((app.scroll, 0))
                    .block(panel(" Consequences / AI review · PgUp/PgDn ")),
                body[1],
            );
        }
    }
    let footer = if chosen.is_empty() {
        "No files checked. [Tab] Review outcomes  [n] New plan  [Esc] Back".into()
    } else {
        format!("Approve deletion: type {}, then Enter\n> {}▏\n{}\n↑↓ inspect   Space untick   Tab outcomes   Esc cancel", phrase(&chosen), safe(&app.plan_confirmation), safe(&app.plan_error))
    };
    frame.render_widget(
        Paragraph::new(footer)
            .wrap(Wrap { trim: false })
            .style(Style::new().fg(ACCENT)),
        parts[3],
    );
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
    #[ignore = "invoked by tests/proposal_pty.py in an interactive PTY"]
    fn proposal_terminal_fixture() {
        let root = PathBuf::from(env::var("DEV_SWEEP_FIXTURE_ROOT").unwrap());
        let mut config = Config::defaults(&root);
        config.roots = vec![root.clone()];
        config.docker = false;
        config.global_caches = false;
        config.normalize(false).unwrap();
        let provider = advice::fixture_provider(env::var("DEV_SWEEP_FIXTURE_URL").unwrap());
        run_with_providers(config, root, vec![provider]).unwrap();
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
            failed: false,
            terminal_failure: false,
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
        let Some(Popup::Proposal { report, .. }) = app.popup.take() else {
            panic!("missing proposal")
        };
        let proposed = report.chosen();
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
    fn blocked_worktree_children_and_scan_feedback_remain_actionable() {
        let mut cache = item();
        cache.parent_worktree = Some("/tmp/project".into());
        let mut tree = item();
        tree.key = "tree".into();
        tree.action = Action::Worktree("/tmp/project".into());
        tree.scope = Some("/tmp/project".into());
        tree.parent_worktree = None;
        tree.blocked = Some("recent filesystem activity".into());
        let mut app = App::new();
        app.providers = vec![advice::fixture_provider("http://127.0.0.1:1".into())];
        app.items.push(tree);
        app.begin_plan(true);
        assert!(matches!(app.popup, Some(Popup::NoCandidates(_))));
        let status = app.status.clone();
        app.accept(Event::Status("Measuring another folder".into()));
        assert_eq!(app.status, status);
        assert!(matches!(app.popup, Some(Popup::NoCandidates(_))));
        app.popup = None;
        app.queued_plan = true;
        app.accept(Event::Item(Box::new(cache)));
        assert_eq!(app.proposal_candidates().len(), 1);
        assert_eq!(app.visible().len(), 2);
        app.accept(Event::Done);
        assert!(matches!(app.popup, Some(Popup::PlanRequest(_))));
        assert!(app.selected().is_empty());
        app.queued_plan = true;
        app.popup = Some(Popup::Delete(vec![item()], "DELETE ".into()));
        app.accept(Event::Done);
        assert!(matches!(app.popup,Some(Popup::Delete(_,ref input)) if input == "DELETE "));
    }
    #[test]
    fn individual_reviews_finish_and_plan_confirmation_errors_are_visible() {
        let mut app = App::new();
        app.items = vec![item()];
        app.review_total = 1;
        app.review_cancel = Some(Arc::new(AtomicBool::new(false)));
        app.accept(Event::ReviewProgress {
            key: "cache".into(),
            reviews: vec![advice::Review {
                text: "mock".into(),
                regenerable: true,
                failed: false,
                terminal_failure: false,
            }],
        });
        assert!(app.review_cancel.is_none());
        assert!(app.status.contains("finished"));
        let mut draft = item();
        draft.selected = true;
        let report = PlanReport {
            proposed: vec![draft],
            reviewed: 1,
            ..PlanReport::default()
        };
        app.popup = Some(Popup::Proposal {
            report,
            deferred: false,
        });
        app.plan_error = "Nothing deleted. Enter the exact phrase DELETE 1.".into();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        terminal.draw(|f| draw(f, &app, 30)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Nothing deleted."));
        assert!(text.contains("DELETE 1"));
    }
    #[test]
    fn terminal_provider_failure_stops_bulk_requests_but_reports_every_candidate() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let server = std::thread::spawn(move || {
            let mut count = 0;
            while !finished.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut data = [0; 4096];
                        let _ = stream.read(&mut data);
                        write!(stream,"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
                        count += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            count
        });
        let items = (0..20)
            .map(|n| {
                let mut i = item();
                i.key = format!("{n}");
                i
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        let _cancel = start_reviews(
            items,
            vec![advice::fixture_provider(format!("http://{address}"))],
            30,
            tx,
            true,
        );
        let mut progress = 0;
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::ReviewProgress { reviews, .. } => {
                    progress += 1;
                    assert!(reviews.iter().any(|r| r.failed));
                }
                Event::Proposal(results) => {
                    assert_eq!(results.len(), 20);
                    assert!(results
                        .iter()
                        .all(|(_, r)| r.iter().all(|r| !r.regenerable)));
                    break;
                }
                _ => panic!("Unexpected review event"),
            }
        }
        done.store(true, Ordering::Relaxed);
        let count = server.join().unwrap();
        assert!(
            (1..=4).contains(&count),
            "Failed provider received {count} requests"
        );
        assert_eq!(progress, 20);
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
