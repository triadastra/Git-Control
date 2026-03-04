use std::env;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use anyhow::Result;
use eframe::{
    egui::{self, Color32, Key, RichText, Stroke, Ui},
    App, CreationContext,
};

use crate::ai_agent::{
    AiSuggestion, LocalConflictAgent, OpenAiConfig, RemoteOpenAiAgent, ResolutionStrategy,
};
use crate::git_service::{
    BranchEntry, CommitEntry, ConflictEntry, FileChange, GitService, RepoSnapshot, RepoSummary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkTab {
    Changes,
    History,
    Branches,
    Sync,
    Conflicts,
    Recovery,
}

impl WorkTab {
    const ALL: [WorkTab; 6] = [
        WorkTab::Changes,
        WorkTab::History,
        WorkTab::Branches,
        WorkTab::Sync,
        WorkTab::Conflicts,
        WorkTab::Recovery,
    ];

    fn title(self) -> &'static str {
        match self {
            WorkTab::Changes => "Changes",
            WorkTab::History => "History Graph",
            WorkTab::Branches => "Branch Lab",
            WorkTab::Sync => "Sync",
            WorkTab::Conflicts => "Conflict Studio",
            WorkTab::Recovery => "Recovery Center",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiProvider {
    LocalHeuristic,
    OpenAi,
}

impl AiProvider {
    fn title(self) -> &'static str {
        match self {
            AiProvider::LocalHeuristic => "Local Heuristic",
            AiProvider::OpenAi => "OpenAI",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteAction {
    RefreshRepositories,
    ReloadSelectedRepo,
    StageAll,
    UnstageAll,
    Commit,
    Fetch,
    PullRebase,
    Push,
    OpenTab(WorkTab),
}

impl PaletteAction {
    fn label(self) -> &'static str {
        match self {
            PaletteAction::RefreshRepositories => "Refresh Repositories",
            PaletteAction::ReloadSelectedRepo => "Reload Selected Repo",
            PaletteAction::StageAll => "Stage All",
            PaletteAction::UnstageAll => "Unstage All",
            PaletteAction::Commit => "Create Commit",
            PaletteAction::Fetch => "Fetch All Remotes",
            PaletteAction::PullRebase => "Pull --rebase",
            PaletteAction::Push => "Push Branch",
            PaletteAction::OpenTab(tab) => match tab {
                WorkTab::Changes => "Open Changes",
                WorkTab::History => "Open History Graph",
                WorkTab::Branches => "Open Branch Lab",
                WorkTab::Sync => "Open Sync",
                WorkTab::Conflicts => "Open Conflict Studio",
                WorkTab::Recovery => "Open Recovery Center",
            },
        }
    }

    fn shortcut(self) -> &'static str {
        match self {
            PaletteAction::RefreshRepositories => "Cmd/Ctrl+R",
            PaletteAction::Commit => "Cmd/Ctrl+Enter",
            PaletteAction::OpenTab(_) => "Cmd/Ctrl+K",
            PaletteAction::ReloadSelectedRepo
            | PaletteAction::StageAll
            | PaletteAction::UnstageAll
            | PaletteAction::Fetch
            | PaletteAction::PullRebase
            | PaletteAction::Push => "Action",
        }
    }
}

fn palette_actions() -> &'static [PaletteAction] {
    &[
        PaletteAction::RefreshRepositories,
        PaletteAction::ReloadSelectedRepo,
        PaletteAction::StageAll,
        PaletteAction::UnstageAll,
        PaletteAction::Commit,
        PaletteAction::Fetch,
        PaletteAction::PullRebase,
        PaletteAction::Push,
        PaletteAction::OpenTab(WorkTab::Changes),
        PaletteAction::OpenTab(WorkTab::History),
        PaletteAction::OpenTab(WorkTab::Branches),
        PaletteAction::OpenTab(WorkTab::Sync),
        PaletteAction::OpenTab(WorkTab::Conflicts),
        PaletteAction::OpenTab(WorkTab::Recovery),
    ]
}

pub struct GitControlApp {
    repo_root_input: String,
    manual_repo_input: String,
    repositories: Vec<RepoSummary>,
    selected_repo: Option<usize>,
    snapshot: Option<RepoSnapshot>,

    active_tab: WorkTab,
    commit_message: String,
    branch_name_input: String,
    changes_filter: String,

    selected_change: Option<usize>,
    selected_commit: Option<usize>,
    selected_branch: Option<usize>,
    selected_conflict: Option<usize>,
    selected_recovery: Option<usize>,

    ai_agent: LocalConflictAgent,
    ai_strategy: ResolutionStrategy,
    ai_provider: AiProvider,
    ai_suggestion: Option<AiSuggestion>,
    ai_edited_text: String,
    ai_request_rx: Option<Receiver<Result<AiSuggestion>>>,
    ai_request_started_at: Option<Instant>,

    openai_api_key_input: String,
    openai_model_input: String,
    openai_base_url_input: String,

    pending_reset_to: Option<String>,
    sync_output: String,
    command_palette_open: bool,
    command_palette_query: String,

    status_line: String,
    status_is_error: bool,
}

impl GitControlApp {
    pub fn new(cc: &CreationContext<'_>) -> Self {
        configure_theme(&cc.egui_ctx);

        let default_root = default_repo_root();
        let mut app = Self {
            repo_root_input: default_root.display().to_string(),
            manual_repo_input: String::new(),
            repositories: Vec::new(),
            selected_repo: None,
            snapshot: None,
            active_tab: WorkTab::Changes,
            commit_message: String::new(),
            branch_name_input: String::new(),
            changes_filter: String::new(),
            selected_change: None,
            selected_commit: None,
            selected_branch: None,
            selected_conflict: None,
            selected_recovery: None,
            ai_agent: LocalConflictAgent,
            ai_strategy: ResolutionStrategy::SmartBlend,
            ai_provider: AiProvider::LocalHeuristic,
            ai_suggestion: None,
            ai_edited_text: String::new(),
            ai_request_rx: None,
            ai_request_started_at: None,
            openai_api_key_input: env::var("OPENAI_API_KEY").unwrap_or_default(),
            openai_model_input: "gpt-4.1-mini".to_owned(),
            openai_base_url_input: "https://api.openai.com/v1".to_owned(),
            pending_reset_to: None,
            sync_output: String::new(),
            command_palette_open: false,
            command_palette_query: String::new(),
            status_line: "Ready".to_owned(),
            status_is_error: false,
        };

        app.refresh_repositories();
        app
    }

    fn refresh_repositories(&mut self) {
        let root = PathBuf::from(self.repo_root_input.trim());
        if !root.exists() {
            self.set_error(format!("Path does not exist: {}", root.display()));
            return;
        }

        let repo_paths = GitService::discover_repositories(&root, 6);
        let mut summaries = Vec::new();

        for path in repo_paths {
            match GitService::load_summary(&path) {
                Ok(summary) => summaries.push(summary),
                Err(err) => {
                    // Log but continue - don't fail the whole scan for one bad repo
                    eprintln!(
                        "Warning: failed to load repo at {}: {err:#}",
                        path.display()
                    );
                }
            }
        }

        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        self.repositories = summaries;

        if self.repositories.is_empty() {
            self.selected_repo = None;
            self.snapshot = None;
            self.set_status(format!("No git repositories found in {}", root.display()));
            return;
        }

        if self
            .selected_repo
            .map(|idx| idx >= self.repositories.len())
            .unwrap_or(true)
        {
            self.selected_repo = Some(0);
        }

        self.refresh_selected_repo_snapshot();
        self.set_status(format!(
            "Found {} repositor{} in {}",
            self.repositories.len(),
            if self.repositories.len() == 1 {
                "y"
            } else {
                "ies"
            },
            root.display()
        ));
    }

    fn refresh_selected_repo_snapshot(&mut self) {
        let Some(path) = self.selected_repo_path() else {
            self.snapshot = None;
            return;
        };

        match GitService::load_snapshot(&path) {
            Ok(snapshot) => {
                if let Some(selected_idx) = self.selected_repo {
                    if let Some(summary) = self.repositories.get_mut(selected_idx) {
                        *summary = snapshot.summary.clone();
                    }
                }
                self.snapshot = Some(snapshot);
                self.clear_selection_context();
            }
            Err(err) => {
                self.snapshot = None;
                self.set_error(format!("Failed to load repository: {err:#}"));
            }
        }
    }

    fn clear_selection_context(&mut self) {
        self.selected_change = None;
        self.selected_commit = None;
        self.selected_branch = None;
        self.selected_conflict = None;
        self.selected_recovery = None;
        self.ai_suggestion = None;
        self.ai_edited_text.clear();
        self.pending_reset_to = None;
    }

    fn selected_repo_path(&self) -> Option<PathBuf> {
        let idx = self.selected_repo?;
        self.repositories.get(idx).map(|r| r.path.clone())
    }

    fn snapshot_cloned(&self) -> Option<RepoSnapshot> {
        self.snapshot.clone()
    }

    fn add_repository(&mut self, folder: &Path) {
        let repo_root = match GitService::resolve_existing_repo(folder) {
            Ok(path) => path,
            Err(err) => {
                self.set_error(format!("No existing repository found: {err:#}"));
                return;
            }
        };

        match GitService::load_summary(&repo_root) {
            Ok(summary) => {
                self.repositories.push(summary);
                self.repositories.sort_by(|a, b| a.name.cmp(&b.name));
                let selected = self
                    .repositories
                    .iter()
                    .position(|r| r.path == repo_root)
                    .unwrap_or(0);
                self.selected_repo = Some(selected);
                self.refresh_selected_repo_snapshot();
                self.set_status(format!("Added repository: {}", repo_root.display()));
            }
            Err(err) => {
                self.set_error(format!("Not a valid git repository: {err:#}"));
            }
        }
    }

    fn run_repo_action<T, F>(&mut self, label: &str, mut action: F)
    where
        F: FnMut(&Path) -> Result<T>,
    {
        let Some(path) = self.selected_repo_path() else {
            self.set_error("No repository selected".to_owned());
            return;
        };

        match action(&path) {
            Ok(_) => {
                self.refresh_selected_repo_snapshot();
                self.set_status(label.to_owned());
            }
            Err(err) => {
                self.set_error(format!("{label} failed: {err:#}"));
            }
        }
    }

    fn run_repo_action_with_output<F>(&mut self, label: &str, mut action: F)
    where
        F: FnMut(&Path) -> Result<String>,
    {
        let Some(path) = self.selected_repo_path() else {
            self.set_error("No repository selected".to_owned());
            return;
        };

        match action(&path) {
            Ok(output) => {
                self.refresh_selected_repo_snapshot();
                self.sync_output = output;
                self.set_status(label.to_owned());
            }
            Err(err) => {
                self.set_error(format!("{label} failed: {err:#}"));
            }
        }
    }

    fn commit(&mut self) {
        let message = self.commit_message.trim().to_owned();
        if message.is_empty() {
            self.set_error("Commit message cannot be empty".to_owned());
            return;
        }

        let Some(path) = self.selected_repo_path() else {
            self.set_error("No repository selected".to_owned());
            return;
        };

        match GitService::commit(&path, &message) {
            Ok(commit_id) => {
                self.commit_message.clear();
                self.refresh_selected_repo_snapshot();
                self.set_status(format!("Committed {commit_id}"));
            }
            Err(err) => self.set_error(format!("Commit failed: {err:#}")),
        }
    }

    fn create_branch(&mut self) {
        let name = self.branch_name_input.trim().to_owned();
        if name.is_empty() {
            self.set_error("Branch name cannot be empty".to_owned());
            return;
        }

        self.run_repo_action("Branch created and checked out", |path| {
            GitService::create_branch(path, &name, true)
        });
        self.branch_name_input.clear();
    }

    fn apply_ai_resolution(&mut self) {
        let Some(snapshot) = self.snapshot_cloned() else {
            self.set_error("No repository loaded".to_owned());
            return;
        };

        let Some(idx) = self.selected_conflict else {
            self.set_error("Select a conflict file first".to_owned());
            return;
        };

        let Some(conflict) = snapshot.conflicts.get(idx) else {
            self.set_error("Conflict selection is out of range".to_owned());
            return;
        };

        let Some(_suggestion) = self.ai_suggestion.as_ref() else {
            self.set_error("Generate an AI suggestion first".to_owned());
            return;
        };

        if contains_conflict_markers(&self.ai_edited_text) {
            self.set_error("Resolved text still contains conflict markers".to_owned());
            return;
        }

        let rel_file = conflict.path.clone();
        let text = self.ai_edited_text.clone();
        self.run_repo_action("Applied AI resolution and staged file", |path| {
            GitService::apply_resolution(path, &rel_file, &text)
        });
    }

    fn request_ai_suggestion(&mut self) {
        let Some(snapshot) = self.snapshot_cloned() else {
            self.set_error("No repository loaded".to_owned());
            return;
        };
        let Some(idx) = self.selected_conflict else {
            self.set_error("Select a conflict file first".to_owned());
            return;
        };
        let Some(conflict) = snapshot.conflicts.get(idx).cloned() else {
            self.set_error("Conflict selection is out of range".to_owned());
            return;
        };

        self.ai_suggestion = None;

        match self.ai_provider {
            AiProvider::LocalHeuristic => {
                self.ai_suggestion =
                    Some(self.ai_agent.suggest(&conflict.content, self.ai_strategy));
                if let Some(suggestion) = self.ai_suggestion.as_ref() {
                    self.ai_edited_text = suggestion.resolved_text.clone();
                }
                self.set_status("Generated local AI suggestion");
            }
            AiProvider::OpenAi => {
                if self.openai_api_key_input.trim().is_empty() {
                    self.set_error("Set OpenAI API key before remote suggestion".to_owned());
                    return;
                }

                if self.ai_request_rx.is_some() {
                    self.set_status("AI request already in progress");
                    return;
                }

                let config = OpenAiConfig {
                    base_url: self.openai_base_url_input.trim().to_owned(),
                    model: self.openai_model_input.trim().to_owned(),
                    api_key: self.openai_api_key_input.trim().to_owned(),
                };

                let strategy = self.ai_strategy;
                let (tx, rx) = mpsc::channel();
                self.ai_request_rx = Some(rx);
                self.ai_request_started_at = Some(Instant::now());
                self.set_status("Generating remote AI suggestion...");

                thread::spawn(move || {
                    let agent = RemoteOpenAiAgent;
                    let result = agent.suggest(
                        &conflict.content,
                        strategy,
                        &conflict.ours_label,
                        &conflict.theirs_label,
                        &config,
                    );
                    let _ = tx.send(result);
                });
            }
        }
    }

    fn poll_ai_request(&mut self) {
        let Some(rx) = self.ai_request_rx.as_ref() else {
            return;
        };

        match rx.try_recv() {
            Ok(result) => {
                self.ai_request_rx = None;
                self.ai_request_started_at = None;

                match result {
                    Ok(suggestion) => {
                        self.ai_edited_text = suggestion.resolved_text.clone();
                        self.ai_suggestion = Some(suggestion);
                        self.set_status("Remote AI suggestion ready");
                    }
                    Err(err) => {
                        self.set_error(format!("Remote AI failed: {err:#}"));
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.ai_request_rx = None;
                self.ai_request_started_at = None;
                self.set_error("Remote AI request disconnected".to_owned());
            }
        }
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status_line = message.into();
        self.status_is_error = false;
    }

    fn set_error(&mut self, message: impl Into<String>) {
        self.status_line = message.into();
        self.status_is_error = true;
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let open_palette = ctx.input(|i| i.modifiers.command && i.key_pressed(Key::K));
        if open_palette {
            self.command_palette_open = true;
        }

        let refresh = ctx.input(|i| i.modifiers.command && i.key_pressed(Key::R));
        if refresh {
            self.refresh_repositories();
            self.set_status("Refreshed repositories");
        }

        let commit = ctx.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
        if commit {
            self.commit();
        }
    }

    fn perform_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::RefreshRepositories => self.refresh_repositories(),
            PaletteAction::ReloadSelectedRepo => self.refresh_selected_repo_snapshot(),
            PaletteAction::StageAll => {
                self.run_repo_action("Staged all changes", GitService::stage_all);
            }
            PaletteAction::UnstageAll => {
                self.run_repo_action("Unstaged all changes", GitService::unstage_all);
            }
            PaletteAction::Commit => self.commit(),
            PaletteAction::Fetch => {
                self.run_repo_action_with_output("Fetch completed", GitService::fetch)
            }
            PaletteAction::PullRebase => {
                self.run_repo_action_with_output(
                    "Pull with rebase completed",
                    GitService::pull_rebase,
                );
            }
            PaletteAction::Push => {
                self.run_repo_action_with_output("Push completed", GitService::push);
            }
            PaletteAction::OpenTab(tab) => self.active_tab = tab,
        }
    }

    fn show_command_palette(&mut self, ctx: &egui::Context) {
        if !self.command_palette_open {
            return;
        }

        let actions = palette_actions();
        let query = self.command_palette_query.trim().to_lowercase();
        let filtered: Vec<PaletteAction> = actions
            .iter()
            .copied()
            .filter(|a| {
                query.is_empty()
                    || a.label().to_lowercase().contains(&query)
                    || a.shortcut().to_lowercase().contains(&query)
            })
            .collect();

        let mut open = self.command_palette_open;
        let mut selected_action: Option<PaletteAction> = None;

        egui::Window::new("Command Palette")
            .default_width(640.0)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.command_palette_query)
                        .hint_text("Type action name or shortcut"),
                );
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for action in filtered {
                            let label = format!("{}    {}", action.label(), action.shortcut());
                            if ui.button(label).clicked() {
                                selected_action = Some(action);
                            }
                        }
                    });
            });

        self.command_palette_open = open;
        if let Some(action) = selected_action {
            self.perform_palette_action(action);
            self.command_palette_open = false;
        }
    }

    fn show_command_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("command_bar")
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(250, 250, 250))
                    .stroke(Stroke::new(0.5, Color32::from_rgb(218, 218, 220)))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    traffic_light(ui, Color32::from_rgb(255, 95, 87));
                    traffic_light(ui, Color32::from_rgb(255, 189, 46));
                    traffic_light(ui, Color32::from_rgb(40, 201, 64));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Git Control").size(16.0).strong());
                    ui.label(
                        RichText::new("Local Git Workbench")
                            .size(12.0)
                            .color(Color32::from_rgb(128, 128, 132)),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if toolbar_button(ui, "Refresh", false).clicked() {
                        self.refresh_repositories();
                    }

                    if toolbar_button(ui, "Reload", false).clicked() {
                        self.refresh_selected_repo_snapshot();
                        self.set_status("Reloaded selected repository");
                    }

                    if toolbar_button(ui, "Stage All", false).clicked() {
                        self.run_repo_action("Staged all changes", GitService::stage_all);
                    }

                    if toolbar_button(ui, "Unstage All", false).clicked() {
                        self.run_repo_action("Unstaged all changes", GitService::unstage_all);
                    }

                    if toolbar_button(ui, "Commit", true).clicked() {
                        self.commit();
                    }

                    if toolbar_button(ui, "Conflicts", false).clicked() {
                        self.active_tab = WorkTab::Conflicts;
                    }

                    if toolbar_button(ui, "Actions", false).clicked() {
                        self.command_palette_open = true;
                    }

                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Cmd/Ctrl+K  Cmd/Ctrl+R  Cmd/Ctrl+Enter")
                            .color(Color32::from_rgb(150, 150, 150))
                            .size(11.0),
                    );
                });
            });
    }

    fn show_left_rail(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("repositories")
            .resizable(true)
            .default_width(280.0)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(245, 245, 247))
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Repositories").strong().size(14.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} found", self.repositories.len()))
                                .size(11.0)
                                .color(Color32::from_rgb(100, 100, 105)),
                        );
                    });
                });
                
                ui.add_space(8.0);

                // Scan section
                egui::Frame::none()
                    .fill(Color32::from_rgb(255, 255, 255))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(10.0))
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(230, 230, 235)))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Scan folder for git repos")
                                .size(11.0)
                                .color(Color32::from_rgb(100, 100, 105)),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.repo_root_input)
                                    .hint_text("~/Desktop")
                                    .desired_width(170.0),
                            );
                            if ui.button("🔍 Scan").clicked() {
                                self.refresh_repositories();
                            }
                        });
                    });

                ui.add_space(8.0);

                // Add repo section
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.manual_repo_input)
                            .hint_text("Or add repo path manually")
                            .desired_width(180.0),
                    );
                    if ui.button("Add").clicked() {
                        let path = PathBuf::from(self.manual_repo_input.trim());
                        if path.exists() {
                            self.add_repository(&path);
                        } else {
                            self.set_error(format!("Path does not exist: {}", path.display()));
                        }
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut next_selection = self.selected_repo;
                    for (idx, repo) in self.repositories.iter().enumerate() {
                        let selected = self.selected_repo == Some(idx);
                        let response = egui::Frame::none()
                            .fill(if selected {
                                Color32::from_rgb(225, 240, 255)
                            } else {
                                Color32::from_rgb(252, 252, 253)
                            })
                            .stroke(Stroke::new(
                                0.5,
                                if selected {
                                    Color32::from_rgb(64, 156, 255)
                                } else {
                                    Color32::from_rgb(224, 224, 226)
                                },
                            ))
                            .rounding(egui::Rounding::same(6.0))
                            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let text_color = if selected {
                                        Color32::from_rgb(18, 86, 163)
                                    } else {
                                        Color32::from_rgb(29, 29, 31)
                                    };
                                    ui.label(
                                        RichText::new(&repo.name)
                                            .strong()
                                            .color(text_color)
                                            .size(13.0),
                                    );
                                    ui.label(
                                        RichText::new(&format!("({})", repo.current_branch))
                                            .color(if selected {
                                                Color32::from_rgb(66, 114, 170)
                                            } else {
                                                Color32::from_rgb(140, 140, 140)
                                            })
                                            .size(11.0),
                                    );
                                });

                                ui.horizontal_wrapped(|ui| {
                                    let chip_color = if selected {
                                        Color32::from_rgb(205, 227, 248)
                                    } else {
                                        Color32::from_rgb(220, 220, 220)
                                    };
                                    let text_color = if selected {
                                        Color32::from_rgb(47, 87, 135)
                                    } else {
                                        Color32::from_rgb(80, 80, 80)
                                    };
                                    status_chip_flat(
                                        ui,
                                        &format!("S:{}", repo.staged_count),
                                        chip_color,
                                        text_color,
                                    );
                                    status_chip_flat(
                                        ui,
                                        &format!("U:{}", repo.unstaged_count),
                                        chip_color,
                                        text_color,
                                    );
                                    status_chip_flat(
                                        ui,
                                        &format!("C:{}", repo.conflict_count),
                                        chip_color,
                                        text_color,
                                    );
                                    status_chip_flat(
                                        ui,
                                        &format!("↑{}↓{}", repo.ahead, repo.behind),
                                        chip_color,
                                        text_color,
                                    );
                                });
                            })
                            .response;

                        if response.clicked() {
                            next_selection = Some(idx);
                        }
                        ui.add_space(4.0);
                    }

                    if next_selection != self.selected_repo {
                        self.selected_repo = next_selection;
                        self.refresh_selected_repo_snapshot();
                    }
                });
            });
    }

    fn show_right_inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(320.0)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(250, 250, 252))
                    .inner_margin(egui::Margin::same(12.0)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new("Inspector").strong().size(14.0));
                ui.label(
                    RichText::new("Context and guidance for the active selection")
                        .size(11.0)
                        .color(Color32::from_rgb(128, 128, 132)),
                );
                ui.add_space(8.0);

                let Some(snapshot) = self.snapshot.as_ref() else {
                    ui.label(
                        RichText::new("Select a repository to inspect")
                            .color(Color32::from_rgb(140, 140, 140))
                            .size(12.0),
                    );
                    return;
                };

                ui.label(RichText::new(&snapshot.summary.name).strong().size(15.0));
                ui.label(
                    RichText::new(snapshot.summary.path.display().to_string())
                        .color(Color32::from_rgb(120, 120, 120))
                        .size(10.0),
                );
                ui.add_space(8.0);
                risk_panel(ui, snapshot);
                ui.add_space(8.0);
                ui.separator();

                match self.active_tab {
                    WorkTab::Changes => {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Selected Change").strong().size(12.0));
                        if let Some(idx) = self.selected_change {
                            if let Some(change) = snapshot.changes.get(idx) {
                                render_change_info(ui, change);
                            } else {
                                ui.label("No change selected");
                            }
                        } else {
                            ui.label(
                                RichText::new("Pick a file in Changes")
                                    .color(Color32::from_rgb(140, 140, 140))
                                    .size(12.0),
                            );
                        }
                    }
                    WorkTab::History => {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Selected Commit").strong().size(12.0));
                        if let Some(idx) = self.selected_commit {
                            if let Some(commit) = snapshot.commits.get(idx) {
                                render_commit_info(ui, commit);
                            } else {
                                ui.label("No commit selected");
                            }
                        } else {
                            ui.label(
                                RichText::new("Pick a commit in History")
                                    .color(Color32::from_rgb(140, 140, 140))
                                    .size(12.0),
                            );
                        }
                    }
                    WorkTab::Branches => {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Selected Branch").strong().size(12.0));
                        if let Some(idx) = self.selected_branch {
                            if let Some(branch) = snapshot.branches.get(idx) {
                                render_branch_info(ui, branch);
                            } else {
                                ui.label("No branch selected");
                            }
                        } else {
                            ui.label(
                                RichText::new("Pick a branch in Branch Lab")
                                    .color(Color32::from_rgb(140, 140, 140))
                                    .size(12.0),
                            );
                        }
                    }
                    WorkTab::Sync => {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Sync State").strong().size(12.0));
                        ui.add_space(6.0);
                        ui.label(format!("↑ Ahead: {}", snapshot.summary.ahead));
                        ui.label(format!("↓ Behind: {}", snapshot.summary.behind));
                        ui.label(format!("⑂ Branch: {}", snapshot.summary.current_branch));
                    }
                    WorkTab::Conflicts => {
                        ui.add_space(8.0);
                        ui.label(RichText::new("AI Agent").strong().size(12.0));
                        ui.add_space(6.0);
                        ui.label(format!("Provider: {}", self.ai_provider.title()));
                        ui.label(format!("Strategy: {:?}", self.ai_strategy));
                        if let Some(started) = self.ai_request_started_at {
                            let secs = started.elapsed().as_secs_f32();
                            ui.label(
                                RichText::new(format!("⏳ {:.1}s", secs))
                                    .color(Color32::from_rgb(100, 100, 100))
                                    .size(11.0),
                            );
                        }
                        if let Some(suggestion) = self.ai_suggestion.as_ref() {
                            ui.add_space(6.0);
                            ui.separator();
                            ui.add_space(6.0);
                            ui.label(RichText::new(&suggestion.title).strong().size(12.0));
                            ui.label(
                                RichText::new(&suggestion.explanation)
                                    .color(Color32::from_rgb(100, 100, 100))
                                    .size(11.0),
                            );
                        }
                    }
                    WorkTab::Recovery => {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Recovery Actions").strong().size(12.0));
                        if self.pending_reset_to.is_some() {
                            ui.add_space(6.0);
                            egui::Frame::none()
                                .fill(Color32::from_rgb(255, 235, 235))
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::same(8.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("⚠ Reset armed")
                                            .color(Color32::from_rgb(180, 40, 40))
                                            .size(12.0),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "Click confirm in Recovery Center to execute",
                                        )
                                        .color(Color32::from_rgb(140, 60, 60))
                                        .size(10.0),
                                    );
                                });
                        } else {
                            ui.label(
                                RichText::new(
                                    "Select an entry and arm reset to restore previous state",
                                )
                                .color(Color32::from_rgb(140, 140, 140))
                                .size(11.0),
                            );
                        }
                    }
                }
            });
    }

    fn show_main_workbench(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(4.0);
                for tab in WorkTab::ALL {
                    if segmented_tab(ui, self.active_tab == tab, tab.title()).clicked() {
                        self.active_tab = tab;
                    }
                }
            });

            ui.add_space(4.0);
            ui.separator();
            ui.add_space(8.0);

            match self.active_tab {
                WorkTab::Changes => self.render_changes_tab(ui),
                WorkTab::History => self.render_history_tab(ui),
                WorkTab::Branches => self.render_branches_tab(ui),
                WorkTab::Sync => self.render_sync_tab(ui),
                WorkTab::Conflicts => self.render_conflicts_tab(ui),
                WorkTab::Recovery => self.render_recovery_tab(ui),
            }
        });
    }

    fn render_changes_tab(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot_cloned() else {
            ui.label("Select a repository to view changes.");
            return;
        };

        ui.heading("Guided Changes Workflow");
        ui.label(&snapshot.summary.next_step);

        ui.horizontal(|ui| {
            status_chip(
                ui,
                &format!("Staged {}", snapshot.summary.staged_count),
                Color32::from_rgb(52, 168, 83),
            );
            status_chip(
                ui,
                &format!("Unstaged {}", snapshot.summary.unstaged_count),
                Color32::from_rgb(251, 140, 0),
            );
            status_chip(
                ui,
                &format!("Untracked {}", snapshot.summary.untracked_count),
                Color32::from_rgb(117, 117, 117),
            );
            status_chip(
                ui,
                &format!("Conflicts {}", snapshot.summary.conflict_count),
                Color32::from_rgb(234, 67, 53),
            );
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if snapshot.summary.conflict_count > 0 && ui.button("Open Conflict Studio").clicked() {
                self.active_tab = WorkTab::Conflicts;
            }
            if snapshot.summary.behind > 0 && ui.button("Open Sync").clicked() {
                self.active_tab = WorkTab::Sync;
            }
            if snapshot.summary.staged_count > 0 && ui.button("Commit Now").clicked() {
                self.commit();
            }
            if ui.button("Stage All").clicked() {
                self.run_repo_action("Staged all changes", GitService::stage_all);
            }
            if ui.button("Unstage All").clicked() {
                self.run_repo_action("Unstaged all changes", GitService::unstage_all);
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Commit Message").strong());
        ui.add(
            egui::TextEdit::multiline(&mut self.commit_message)
                .desired_rows(3)
                .hint_text("feat(ui): improve conflict resolution flow"),
        );
        ui.horizontal(|ui| {
            if ui.button("Template: feat").clicked() {
                self.commit_message = "feat: ".to_owned();
            }
            if ui.button("Template: fix").clicked() {
                self.commit_message = "fix: ".to_owned();
            }
            if ui.button("Template: chore").clicked() {
                self.commit_message = "chore: ".to_owned();
            }
            if ui.button("Create Commit").clicked() {
                self.commit();
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Changed Files").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.changes_filter)
                    .hint_text("Filter by file path"),
            );
        });

        if snapshot.changes.is_empty() {
            ui.label("Working tree is clean.");
            return;
        }

        let filter = self.changes_filter.trim().to_lowercase();
        let visible_changes: Vec<(usize, FileChange)> = snapshot
            .changes
            .iter()
            .enumerate()
            .filter(|(_, c)| filter.is_empty() || c.path.to_lowercase().contains(&filter))
            .map(|(i, c)| (i, c.clone()))
            .collect();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, change) in &visible_changes {
                ui.horizontal(|ui| {
                    let selected = self.selected_change == Some(*idx);
                    if ui
                        .selectable_label(selected, format!("{} ({})", change.path, change.kind))
                        .clicked()
                    {
                        self.selected_change = Some(*idx);
                    }

                    if change.unstaged && ui.button("Stage").clicked() {
                        let rel_path = change.path.clone();
                        self.run_repo_action("Staged file", |repo_path| {
                            GitService::stage_path(repo_path, &rel_path)
                        });
                    }

                    if change.staged && ui.button("Unstage").clicked() {
                        let rel_path = change.path.clone();
                        self.run_repo_action("Unstaged file", |repo_path| {
                            GitService::unstage_path(repo_path, &rel_path)
                        });
                    }

                    if change.kind == "conflicted" && ui.button("Resolve").clicked() {
                        self.active_tab = WorkTab::Conflicts;
                        self.selected_conflict = snapshot
                            .conflicts
                            .iter()
                            .position(|conflict| conflict.path == change.path);
                    }
                });

                ui.horizontal(|ui| {
                    if change.staged {
                        status_chip(ui, "staged", Color32::from_rgb(52, 168, 83));
                    }
                    if change.unstaged {
                        status_chip(ui, "unstaged", Color32::from_rgb(251, 140, 0));
                    }
                });

                ui.add_space(4.0);
            }
        });
    }

    fn render_history_tab(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot_cloned() else {
            ui.label("Select a repository to view history.");
            return;
        };

        ui.label(RichText::new("History Graph").strong().size(16.0));
        ui.label(
            RichText::new("Flowchart view of branches, merges, and author actions.")
                .size(12.0)
                .color(Color32::from_rgb(120, 120, 125)),
        );
        ui.add_space(10.0);

        if snapshot.commits.is_empty() {
            ui.label("No commits found.");
            return;
        }

        let graph_rows = build_commit_graph_rows(&snapshot.commits);

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, (commit, graph)) in snapshot.commits.iter().zip(graph_rows.iter()).enumerate()
            {
                let selected = self.selected_commit == Some(idx);
                let is_head = idx == 0;

                let frame_response = egui::Frame::none()
                    .fill(if selected {
                        Color32::from_rgb(230, 242, 255)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(Stroke::new(
                        0.5,
                        if selected {
                            Color32::from_rgb(150, 196, 252)
                        } else {
                            Color32::from_rgb(230, 230, 232)
                        },
                    ))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0));

                let inner = frame_response.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let lane_count = graph.lane_count.max(1);
                        let graph_width = 24.0 + lane_count as f32 * 14.0;
                        let row_height = 58.0;
                        let (graph_rect, _) = ui.allocate_exact_size(
                            egui::vec2(graph_width, row_height),
                            egui::Sense::hover(),
                        );

                        let painter = ui.painter();
                        for lane in 0..lane_count {
                            let x = graph_rect.left() + 12.0 + lane as f32 * 14.0;
                            let color = graph_lane_color(lane).gamma_multiply(0.45);
                            painter.line_segment(
                                [
                                    egui::pos2(x, graph_rect.top() + 5.0),
                                    egui::pos2(x, graph_rect.bottom() - 5.0),
                                ],
                                Stroke::new(1.4, color),
                            );
                        }

                        let node_x = graph_rect.left() + 12.0 + graph.lane as f32 * 14.0;
                        let node_y = graph_rect.center().y;
                        let node_color = if selected || is_head {
                            Color32::from_rgb(0, 122, 255)
                        } else {
                            graph_lane_color(graph.lane)
                        };
                        painter.circle_filled(
                            egui::pos2(node_x, node_y),
                            if is_head { 6.5 } else { 5.0 },
                            node_color,
                        );
                        painter.circle_stroke(
                            egui::pos2(node_x, node_y),
                            if is_head { 6.5 } else { 5.0 },
                            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 190)),
                        );

                        if graph.parent_count > 1 {
                            let target_lane = graph.lane + graph.parent_count.saturating_sub(1);
                            if target_lane < lane_count {
                                let target_x = graph_rect.left() + 12.0 + target_lane as f32 * 14.0;
                                painter.line_segment(
                                    [egui::pos2(node_x, node_y), egui::pos2(target_x, node_y)],
                                    Stroke::new(1.2, Color32::from_rgb(140, 140, 145)),
                                );
                            }
                        }

                        ui.add_space(4.0);

                        ui.vertical(|ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&commit.id)
                                        .monospace()
                                        .size(11.0)
                                        .color(Color32::from_rgb(100, 100, 105)),
                                );
                                if is_head {
                                    egui::Frame::none()
                                        .fill(Color32::from_rgb(0, 122, 255))
                                        .rounding(egui::Rounding::same(4.0))
                                        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new("HEAD")
                                                    .size(9.0)
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            );
                                        });
                                }

                                for branch in commit.branch_labels.iter().take(4) {
                                    status_chip_flat(
                                        ui,
                                        branch,
                                        Color32::from_rgb(233, 239, 246),
                                        Color32::from_rgb(47, 87, 135),
                                    );
                                }

                                if graph.parent_count > 1 {
                                    status_chip_flat(
                                        ui,
                                        "merge",
                                        Color32::from_rgb(246, 234, 214),
                                        Color32::from_rgb(131, 92, 33),
                                    );
                                }
                            });

                            ui.label(RichText::new(&commit.summary).size(13.0).color(
                                if selected {
                                    Color32::from_rgb(0, 60, 120)
                                } else {
                                    Color32::from_rgb(40, 40, 45)
                                },
                            ));

                            ui.label(
                                RichText::new(format!(
                                    "{} • {} • {}",
                                    commit_action_label(&commit.summary),
                                    commit.author,
                                    commit.timestamp
                                ))
                                .size(10.0)
                                .color(Color32::from_rgb(140, 140, 145)),
                            );
                        });
                    });
                });

                let response = ui.interact(
                    inner.response.rect,
                    ui.next_auto_id().with(idx),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    self.selected_commit = Some(idx);
                }

                ui.add_space(2.0);
            }
        });
    }

    fn render_branches_tab(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot_cloned() else {
            ui.label("Select a repository to manage branches.");
            return;
        };

        ui.heading("Branch Lab");
        ui.label("Create/switch branches with immediate feedback.");

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.branch_name_input)
                    .hint_text("feature/conflict-assistant"),
            );
            if ui.button("Create + Checkout").clicked() {
                self.create_branch();
            }
        });

        ui.separator();

        if snapshot.branches.is_empty() {
            ui.label("No local branches found.");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, branch) in snapshot.branches.iter().enumerate() {
                ui.horizontal(|ui| {
                    let selected = self.selected_branch == Some(idx);
                    let mut title = branch.name.clone();
                    if branch.is_head {
                        title.push_str("  (current)");
                    }
                    if ui.selectable_label(selected, title).clicked() {
                        self.selected_branch = Some(idx);
                    }

                    if !branch.is_head && ui.button("Checkout").clicked() {
                        let name = branch.name.clone();
                        self.run_repo_action("Switched branch", |path| {
                            GitService::checkout_branch(path, &name)
                        });
                    }
                });

                if let Some(upstream) = &branch.upstream {
                    ui.small(format!("Upstream: {upstream}"));
                }
                ui.add_space(6.0);
            }
        });
    }

    fn render_sync_tab(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot_cloned() else {
            ui.label("Select a repository to view sync status.");
            return;
        };

        ui.heading("Sync");
        ui.label("Quick sync guidance for current branch.");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            status_chip(
                ui,
                &format!("↑ Ahead {}", snapshot.summary.ahead),
                Color32::from_rgb(66, 133, 244),
            );
            status_chip(
                ui,
                &format!("↓ Behind {}", snapshot.summary.behind),
                Color32::from_rgb(251, 140, 0),
            );
            status_chip(
                ui,
                &format!("⑂ {}", snapshot.summary.current_branch),
                Color32::from_rgb(100, 100, 100),
            );
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Fetch").clicked() {
                self.run_repo_action_with_output("Fetch completed", GitService::fetch);
            }
            if ui.button("Pull --rebase").clicked() {
                self.run_repo_action_with_output(
                    "Pull with rebase completed",
                    GitService::pull_rebase,
                );
            }
            if ui.button("Push").clicked() {
                self.run_repo_action_with_output("Push completed", GitService::push);
            }
        });

        ui.add_space(8.0);
        if snapshot.summary.behind > 0 {
            ui.label("Recommended: pull with rebase before additional commits.");
            if ui.button("Copy `git pull --rebase` command").clicked() {
                ui.output_mut(|output| {
                    output.copied_text = "git pull --rebase".to_owned();
                });
                self.set_status("Copied pull command");
            }
        }

        if snapshot.summary.ahead > 0 {
            ui.label("Recommended: push branch to publish local commits.");
            if ui.button("Copy `git push` command").clicked() {
                ui.output_mut(|output| {
                    output.copied_text = "git push".to_owned();
                });
                self.set_status("Copied push command");
            }
        }

        if snapshot.summary.ahead == 0 && snapshot.summary.behind == 0 {
            ui.label("Branch is in sync with upstream.");
        }

        ui.separator();
        ui.label(RichText::new("Last Sync Output").strong());
        if self.sync_output.is_empty() {
            ui.small("No sync command has been run yet.");
        } else {
            let mut output = self.sync_output.clone();
            ui.add(
                egui::TextEdit::multiline(&mut output)
                    .desired_rows(8)
                    .interactive(false),
            );
        }
    }

    fn render_conflicts_tab(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot_cloned() else {
            ui.label("Select a repository to resolve conflicts.");
            return;
        };

        ui.heading("Conflict Studio");
        ui.label("AI-assisted resolution with local and remote agents.");

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Provider");
            ui.selectable_value(
                &mut self.ai_provider,
                AiProvider::LocalHeuristic,
                AiProvider::LocalHeuristic.title(),
            );
            ui.selectable_value(
                &mut self.ai_provider,
                AiProvider::OpenAi,
                AiProvider::OpenAi.title(),
            );
        });

        ui.horizontal(|ui| {
            ui.label("Strategy");
            ui.selectable_value(
                &mut self.ai_strategy,
                ResolutionStrategy::SmartBlend,
                "Smart Blend",
            );
            ui.selectable_value(
                &mut self.ai_strategy,
                ResolutionStrategy::KeepOurs,
                "Keep Ours",
            );
            ui.selectable_value(
                &mut self.ai_strategy,
                ResolutionStrategy::KeepTheirs,
                "Keep Theirs",
            );
        });

        if self.ai_provider == AiProvider::OpenAi {
            ui.group(|ui| {
                ui.label(RichText::new("OpenAI Settings").strong());
                ui.horizontal(|ui| {
                    ui.label("Base URL");
                    ui.text_edit_singleline(&mut self.openai_base_url_input);
                });
                ui.horizontal(|ui| {
                    ui.label("Model");
                    ui.text_edit_singleline(&mut self.openai_model_input);
                });
                ui.horizontal(|ui| {
                    ui.label("API Key");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.openai_api_key_input)
                            .password(true)
                            .hint_text("sk-..."),
                    );
                });
                ui.small("Tip: OPENAI_API_KEY env var pre-fills the key field on launch.");
            });
        }

        if snapshot.conflicts.is_empty() {
            ui.add_space(8.0);
            ui.label("No merge conflicts detected.");
            return;
        }

        let conflicts = snapshot.conflicts.clone();
        if self.selected_conflict.is_none() {
            self.selected_conflict = Some(0);
        }

        ui.columns(2, |columns| {
            columns[0].label(RichText::new("Conflicted Files").strong());
            egui::ScrollArea::vertical().show(&mut columns[0], |ui| {
                for (idx, conflict) in conflicts.iter().enumerate() {
                    let selected = self.selected_conflict == Some(idx);
                    if ui.selectable_label(selected, &conflict.path).clicked() {
                        self.selected_conflict = Some(idx);
                        self.ai_suggestion = None;
                        self.ai_edited_text.clear();
                    }
                }
            });

            columns[1].label(RichText::new("Conflict Content").strong());
            if let Some(idx) = self.selected_conflict {
                if let Some(conflict) = conflicts.get(idx) {
                    conflict_detail_ui(
                        &mut columns[1],
                        conflict,
                        self.ai_suggestion.as_ref(),
                        &mut self.ai_edited_text,
                        self.ai_strategy,
                    );

                    columns[1].horizontal(|ui| {
                        let waiting = self.ai_request_rx.is_some();
                        if ui
                            .add_enabled(!waiting, egui::Button::new("Generate AI Suggestion"))
                            .clicked()
                        {
                            self.request_ai_suggestion();
                        }

                        if waiting {
                            ui.label("Request in progress...");
                        }
                    });

                    if self.ai_suggestion.is_some()
                        && columns[1].button("Apply Suggestion and Stage").clicked()
                    {
                        self.apply_ai_resolution();
                    }

                    if self.ai_suggestion.is_some()
                        && columns[1]
                            .button("Reset Edited Text to Suggestion")
                            .clicked()
                    {
                        if let Some(suggestion) = self.ai_suggestion.as_ref() {
                            self.ai_edited_text = suggestion.resolved_text.clone();
                        }
                    }

                    if contains_conflict_markers(&self.ai_edited_text) {
                        columns[1].label(
                            RichText::new("⚠ Text still contains conflict markers")
                                .color(Color32::from_rgb(180, 40, 40))
                                .size(12.0),
                        );
                    }
                }
            }
        });
    }

    fn render_recovery_tab(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot_cloned() else {
            ui.label("Select a repository to access recovery timeline.");
            return;
        };

        ui.heading("Recovery Center");
        ui.label("Human-readable reflog entries for rapid rollback.");

        if snapshot.recovery.is_empty() {
            ui.add_space(8.0);
            ui.label("No reflog entries available yet.");
            return;
        }

        let entries = snapshot.recovery.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, entry) in entries.iter().enumerate() {
                let selected = self.selected_recovery == Some(idx);
                let label = format!("{}  {}", entry.to_id_short, entry.message);
                if ui.selectable_label(selected, label).clicked() {
                    self.selected_recovery = Some(idx);
                }
                ui.small(format!(
                    "{}  {} -> {}",
                    entry.timestamp, entry.from_id_short, entry.to_id_short
                ));
                ui.add_space(6.0);
            }
        });

        ui.separator();

        if let Some(selected_idx) = self.selected_recovery {
            if let Some(entry) = entries.get(selected_idx) {
                ui.label(format!("Selected: {}", entry.message));
                if self
                    .pending_reset_to
                    .as_ref()
                    .map(|oid| oid == &entry.to_id)
                    .unwrap_or(false)
                {
                    if ui
                        .button(
                            RichText::new("Confirm Reset")
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .clicked()
                    {
                        let target = entry.to_id.clone();
                        self.run_repo_action("Reset repository to selected reflog entry", |path| {
                            GitService::mixed_reset_to(path, &target)
                        });
                        self.pending_reset_to = None;
                    }

                    if ui.button("Cancel").clicked() {
                        self.pending_reset_to = None;
                        self.set_status("Canceled recovery reset");
                    }
                } else if ui.button("Arm Reset").clicked() {
                    self.pending_reset_to = Some(entry.to_id.clone());
                    self.set_status("Reset armed. Click confirm to execute");
                }
            }
        } else {
            ui.label("Select an entry to arm a recovery reset.");
        }
    }

    fn show_footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_footer")
            .resizable(false)
            .frame(egui::Frame::none().fill(Color32::from_rgb(240, 240, 240)))
            .show(ctx, |ui| {
                ui.add_space(1.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if self.status_is_error {
                        ui.label(
                            RichText::new(&self.status_line)
                                .color(Color32::from_rgb(180, 40, 40))
                                .size(12.0),
                        );
                    } else {
                        ui.label(
                            RichText::new(&self.status_line)
                                .color(Color32::from_rgb(100, 100, 100))
                                .size(12.0),
                        );
                    }
                });
                ui.add_space(1.0);
            });
    }
}

impl App for GitControlApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_ai_request();
        self.handle_shortcuts(ctx);
        self.show_command_bar(ctx);
        self.show_left_rail(ctx);
        self.show_right_inspector(ctx);
        self.show_main_workbench(ctx);
        self.show_command_palette(ctx);
        self.show_footer(ctx);
    }
}

fn default_repo_root() -> PathBuf {
    if let Ok(home) = env::var("HOME") {
        let desktop = PathBuf::from(&home).join("Desktop");
        if desktop.exists() {
            return desktop;
        }
        return PathBuf::from(home);
    }

    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn configure_theme(ctx: &egui::Context) {
    // macOS Sonoma-inspired light theme
    let mut visuals = egui::Visuals::light();

    // Soft, warm backgrounds like macOS
    visuals.window_fill = Color32::from_rgb(248, 248, 250);
    visuals.panel_fill = Color32::from_rgb(252, 252, 254);
    visuals.faint_bg_color = Color32::from_rgb(242, 242, 245);
    visuals.extreme_bg_color = Color32::from_rgb(255, 255, 255);

    // Text - SF Pro style
    visuals.override_text_color = Some(Color32::from_rgb(30, 30, 32));

    // Window styling
    visuals.window_rounding = egui::Rounding::same(12.0);
    visuals.popup_shadow = egui::epaint::Shadow {
        extrusion: 12.0,
        color: Color32::from_rgba_unmultiplied(0, 0, 0, 15),
    };

    // Button styling - macOS style
    visuals.widgets.active.bg_fill = Color32::from_rgb(0, 122, 255);
    visuals.widgets.active.fg_stroke.color = Color32::WHITE;
    visuals.widgets.active.bg_stroke = Stroke::new(0.0, Color32::TRANSPARENT);
    visuals.widgets.active.rounding = egui::Rounding::same(8.0);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0, 122, 255);
    visuals.widgets.hovered.fg_stroke.color = Color32::WHITE;
    visuals.widgets.hovered.bg_stroke = Stroke::new(0.0, Color32::TRANSPARENT);
    visuals.widgets.hovered.rounding = egui::Rounding::same(8.0);

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(255, 255, 255);
    visuals.widgets.inactive.fg_stroke.color = Color32::from_rgb(30, 30, 32);
    visuals.widgets.inactive.bg_stroke = Stroke::new(0.5, Color32::from_rgb(200, 200, 205));
    visuals.widgets.inactive.rounding = egui::Rounding::same(8.0);

    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(245, 245, 248);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(0.5, Color32::from_rgb(220, 220, 225));
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(8.0);

    // Open - macOS blue
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(0, 122, 255, 50);
    visuals.selection.stroke = Stroke::new(1.5, Color32::from_rgb(0, 122, 255));

    // Links
    visuals.hyperlink_color = Color32::from_rgb(0, 100, 220);

    // Wrapping
    visuals.text_cursor = Stroke::new(1.5, Color32::from_rgb(0, 122, 255));

    ctx.set_visuals(visuals);

    // Custom style tweaks
    let mut style = (*ctx.style()).clone();
    style.visuals.button_frame = true;
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.window_margin = egui::Margin::same(12.0);
    style.spacing.interact_size = egui::vec2(0.0, 28.0);
    style.spacing.combo_width = 200.0;

    // SF Pro-inspired text styles
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            egui::FontId::new(24.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Name("Heading2".into()),
            egui::FontId::new(17.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Name("Heading3".into()),
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
        ),
    ]
    .into();
    ctx.set_style(style);
}

fn status_chip(ui: &mut Ui, label: &str, color: Color32) {
    egui::Frame::none()
        .fill(color)
        .inner_margin(egui::Margin::symmetric(10.0, 5.0))
        .rounding(egui::Rounding::same(12.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(label)
                    .color(Color32::WHITE)
                    .size(11.0)
                    .strong(),
            );
        });
}

fn status_chip_flat(ui: &mut Ui, label: &str, bg_color: Color32, text_color: Color32) {
    egui::Frame::none()
        .fill(bg_color)
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .rounding(egui::Rounding::same(6.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).color(text_color).size(10.0));
        });
}

fn traffic_light(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn toolbar_button(ui: &mut Ui, label: &str, primary: bool) -> egui::Response {
    let fill = if primary {
        Color32::from_rgb(0, 122, 255)
    } else {
        Color32::from_rgb(244, 244, 246)
    };
    let text_color = if primary {
        Color32::WHITE
    } else {
        Color32::from_rgb(44, 44, 48)
    };
    let button = egui::Button::new(RichText::new(label).color(text_color))
        .fill(fill)
        .stroke(Stroke::new(
            0.5,
            if primary {
                Color32::from_rgb(0, 108, 230)
            } else {
                Color32::from_rgb(210, 210, 214)
            },
        ))
        .rounding(egui::Rounding::same(7.0));
    ui.add(button)
}

fn segmented_tab(ui: &mut Ui, active: bool, label: &str) -> egui::Response {
    let button = egui::Button::new(
        RichText::new(label)
            .size(12.0)
            .color(if active {
                Color32::WHITE
            } else {
                Color32::from_rgb(84, 84, 88)
            })
            .strong(),
    )
    .fill(if active {
        Color32::from_rgb(0, 122, 255)
    } else {
        Color32::from_rgb(241, 241, 243)
    })
    .stroke(Stroke::new(
        0.5,
        if active {
            Color32::from_rgb(0, 108, 230)
        } else {
            Color32::from_rgb(210, 210, 214)
        },
    ))
    .rounding(egui::Rounding::same(7.0))
    .min_size(egui::vec2(110.0, 28.0));
    ui.add(button)
}

#[derive(Debug, Clone, Copy)]
struct CommitGraphRow {
    lane: usize,
    lane_count: usize,
    parent_count: usize,
}

fn build_commit_graph_rows(commits: &[CommitEntry]) -> Vec<CommitGraphRow> {
    let mut rows = Vec::with_capacity(commits.len());
    let mut lanes: Vec<String> = Vec::new();

    for commit in commits {
        let lane = if let Some(found) = lanes.iter().position(|id| id == &commit.oid) {
            found
        } else {
            lanes.push(commit.oid.clone());
            lanes.len() - 1
        };
        let lane_count = lanes.len();
        let parent_count = commit.parents.len();

        if commit.parents.is_empty() {
            lanes.remove(lane);
        } else {
            lanes[lane] = commit.parents[0].clone();
            for (offset, parent) in commit.parents.iter().skip(1).enumerate() {
                lanes.insert(lane + 1 + offset, parent.clone());
            }
        }

        rows.push(CommitGraphRow {
            lane,
            lane_count,
            parent_count,
        });
    }

    rows
}

fn graph_lane_color(lane: usize) -> Color32 {
    const PALETTE: [Color32; 8] = [
        Color32::from_rgb(0, 122, 255),
        Color32::from_rgb(175, 82, 222),
        Color32::from_rgb(255, 149, 0),
        Color32::from_rgb(52, 199, 89),
        Color32::from_rgb(255, 59, 48),
        Color32::from_rgb(90, 200, 250),
        Color32::from_rgb(88, 86, 214),
        Color32::from_rgb(255, 204, 0),
    ];
    PALETTE[lane % PALETTE.len()]
}

fn commit_action_label(summary: &str) -> &'static str {
    let first = summary
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(':')
        .to_ascii_lowercase();

    if first.starts_with("merge") {
        "merge"
    } else if first.starts_with("revert") {
        "revert"
    } else if first.starts_with("fix") {
        "fix"
    } else if first.starts_with("feat") {
        "feature"
    } else if first.starts_with("refactor") {
        "refactor"
    } else if first.starts_with("docs") {
        "docs"
    } else if first.starts_with("test") {
        "test"
    } else if first.starts_with("chore") {
        "chore"
    } else if first.starts_with("ci") {
        "ci"
    } else if first.starts_with("perf") {
        "perf"
    } else {
        "commit"
    }
}

fn risk_panel(ui: &mut Ui, snapshot: &RepoSnapshot) {
    let (icon, label, bg_color, fg_color, border_color) = if snapshot.summary.conflict_count > 0 {
        (
            "⚠",
            "Unresolved conflicts",
            Color32::from_rgb(255, 240, 240),
            Color32::from_rgb(200, 50, 50),
            Color32::from_rgb(255, 200, 200),
        )
    } else if snapshot.summary.unstaged_count > 0 || snapshot.summary.behind > 0 {
        (
            "●",
            "Pending changes",
            Color32::from_rgb(255, 250, 235),
            Color32::from_rgb(180, 120, 20),
            Color32::from_rgb(255, 230, 180),
        )
    } else {
        (
            "✓",
            "Clean working tree",
            Color32::from_rgb(235, 252, 240),
            Color32::from_rgb(40, 140, 70),
            Color32::from_rgb(200, 240, 210),
        )
    };

    egui::Frame::none()
        .fill(bg_color)
        .stroke(Stroke::new(1.0, border_color))
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(14.0, 10.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(14.0).color(fg_color));
                ui.label(RichText::new(label).strong().color(fg_color).size(13.0));
            });
        });
}

fn render_change_info(ui: &mut Ui, change: &FileChange) {
    ui.add_space(4.0);
    info_row(ui, "Path", &change.path);
    info_row(ui, "Type", &change.kind);
    info_row(ui, "Staged", yes_no(change.staged));
    info_row(ui, "Unstaged", yes_no(change.unstaged));
}

fn render_commit_info(ui: &mut Ui, commit: &CommitEntry) {
    ui.add_space(4.0);
    info_row(ui, "Commit", &commit.id);
    info_row(ui, "Action", commit_action_label(&commit.summary));
    info_row(ui, "Summary", &commit.summary);
    info_row(ui, "Author", &commit.author);
    info_row(ui, "Time", &commit.timestamp);
    info_row(ui, "Parents", &commit.parents.len().to_string());
    if !commit.branch_labels.is_empty() {
        info_row(ui, "Branches", &commit.branch_labels.join(", "));
    }
}

fn render_branch_info(ui: &mut Ui, branch: &BranchEntry) {
    ui.add_space(4.0);
    info_row(ui, "Branch", &branch.name);
    info_row(ui, "Current", yes_no(branch.is_head));
    info_row(ui, "Upstream", branch.upstream.as_deref().unwrap_or("none"));
}

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{}:", label))
                .color(Color32::from_rgb(120, 120, 125))
                .size(12.0),
        );
        ui.label(
            RichText::new(value)
                .size(12.0)
                .color(Color32::from_rgb(50, 50, 55)),
        );
    });
}

fn conflict_detail_ui(
    ui: &mut Ui,
    conflict: &ConflictEntry,
    suggestion: Option<&AiSuggestion>,
    edited_text: &mut String,
    _strategy: ResolutionStrategy,
) {
    ui.label(RichText::new(&conflict.path).strong().size(14.0));
    ui.add_space(4.0);

    egui::Frame::none()
        .fill(Color32::from_rgb(245, 245, 248))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(10.0, 6.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "Current: {}  •  Incoming: {}",
                    conflict.ours_label, conflict.theirs_label
                ))
                .color(Color32::from_rgb(100, 100, 105))
                .size(11.0),
            );
        });

    ui.add_space(10.0);
    ui.label(
        RichText::new("Conflict Content")
            .strong()
            .size(12.0)
            .color(Color32::from_rgb(80, 80, 85)),
    );

    egui::Frame::none()
        .fill(Color32::from_rgb(255, 255, 255))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(230, 230, 235)))
        .show(ui, |ui| {
            let mut raw = conflict.content.clone();
            ui.add(
                egui::TextEdit::multiline(&mut raw)
                    .desired_rows(6)
                    .interactive(false)
                    .text_color(Color32::from_rgb(60, 60, 65)),
            );
        });

    if let Some(suggestion) = suggestion {
        ui.add_space(12.0);

        egui::Frame::none()
            .fill(Color32::from_rgb(240, 248, 255))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(12.0))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(200, 225, 255)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("✦")
                            .size(14.0)
                            .color(Color32::from_rgb(0, 122, 255)),
                    );
                    ui.label(
                        RichText::new(&suggestion.title)
                            .strong()
                            .size(13.0)
                            .color(Color32::from_rgb(0, 80, 160)),
                    );
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&suggestion.explanation)
                        .size(11.0)
                        .color(Color32::from_rgb(80, 100, 130)),
                );
            });

        ui.add_space(8.0);
        ui.label(
            RichText::new("Edit Resolution")
                .strong()
                .size(12.0)
                .color(Color32::from_rgb(80, 80, 85)),
        );

        if edited_text.is_empty() {
            *edited_text = suggestion.resolved_text.clone();
        }

        egui::Frame::none()
            .fill(Color32::from_rgb(250, 255, 250))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::same(12.0))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(210, 240, 220)))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(edited_text)
                        .desired_rows(8)
                        .hint_text("Edit the proposed resolution before applying"),
                );
            });
    }
}

fn yes_no(flag: bool) -> &'static str {
    if flag {
        "yes"
    } else {
        "no"
    }
}

fn contains_conflict_markers(content: &str) -> bool {
    content.contains("<<<<<<<") || content.contains("=======") || content.contains(">>>>>>>")
}
