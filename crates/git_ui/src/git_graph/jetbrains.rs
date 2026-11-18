use super::*;
use crate::{
    RenameBranchModal, git_panel_settings::GitPanelSettings,
    repository_selector::RepositorySelector,
};
use anyhow::anyhow;
use askpass::AskPassDelegate;
use collections::HashSet;
use editor::EditorEvent;
use git::{
    normalize_branch_name,
    repository::{
        Branch, CommitSummary, FetchOptions, LogOrder, LogSource, PushOptions, Remote, RepoPath,
        Upstream, UpstreamTracking, UpstreamTrackingStatus,
    },
};
use git_ui_core::askpass_modal::AskPassModal;
use gpui::{
    App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, MouseButton, MouseDownEvent, ScrollHandle, SharedString, Subscription, Task,
    WeakEntity, Window,
};
use menu::{Cancel, Confirm};
use project::git_store::{CommitDataState, GitStore, Repository, RepositoryId};
use std::collections::BTreeMap;
use time::{Duration as TimeDuration, OffsetDateTime};
use ui::{
    ContextMenu, DiffStat, Disclosure, Divider, IconButton, Label, ListItem, ListItemSpacing,
    PopoverMenu, PopoverMenuHandle, Tooltip,
};
use workspace::{
    ModalView, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::DetachAndPromptErr,
};

const DEFAULT_DOCK_HEIGHT: Pixels = px(320.);
const DEFAULT_BRANCH_PANE_WIDTH: Pixels = px(220.);
const DEFAULT_DETAILS_PANE_RATIO: f32 = 0.66;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateFilter {
    #[default]
    All,
    Today,
    Yesterday,
    Last7Days,
    Last30Days,
}

impl DateFilter {
    fn label(self) -> &'static str {
        match self {
            DateFilter::All => "Date",
            DateFilter::Today => "Today",
            DateFilter::Yesterday => "Yesterday",
            DateFilter::Last7Days => "Last 7 Days",
            DateFilter::Last30Days => "Last 30 Days",
        }
    }

    fn matches(self, commit_timestamp: i64) -> bool {
        if self == DateFilter::All {
            return true;
        }
        let now = OffsetDateTime::now_utc();
        let Ok(commit_dt) = OffsetDateTime::from_unix_timestamp(commit_timestamp) else {
            return false;
        };
        match self {
            DateFilter::All => true,
            DateFilter::Today => commit_dt.date() == now.date(),
            DateFilter::Yesterday => commit_dt.date() == (now - TimeDuration::days(1)).date(),
            DateFilter::Last7Days => {
                commit_timestamp >= (now - TimeDuration::days(7)).unix_timestamp()
            }
            DateFilter::Last30Days => {
                commit_timestamp >= (now - TimeDuration::days(30)).unix_timestamp()
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UserFilter {
    #[default]
    All,
    Me,
    Author(SharedString),
}

impl UserFilter {
    fn label(&self) -> SharedString {
        match self {
            UserFilter::All => "User".into(),
            UserFilter::Me => "User: me".into(),
            UserFilter::Author(name) => format!("User: {name}").into(),
        }
    }
}

pub(super) struct BranchContextMenuState {
    position: Point<Pixels>,
    menu: Entity<ContextMenu>,
    _subscription: Subscription,
}

pub(crate) struct JetBrainsGraphState {
    pub(crate) follow_active_repo: bool,
    pub(crate) show_branches_pane: bool,
    pub(crate) branch_filter_editor: Entity<Editor>,
    pub(crate) branches: Vec<Branch>,
    pub(crate) _branches_task: Option<Task<()>>,
    pub(crate) collapsed_sections: HashSet<&'static str>,
    pub(crate) expanded_branch_folders: HashSet<String>,
    pub(crate) branches_scroll_handle: ScrollHandle,
    pub(super) branch_context_menu: Option<BranchContextMenuState>,
    pub(crate) user_filter: UserFilter,
    pub(crate) date_filter: DateFilter,
    pub(crate) current_user_email: Option<String>,
    pub(crate) _user_email_task: Option<Task<()>>,
    pub(crate) filtered_commit_indices: Option<Vec<usize>>,
    pub(crate) branch_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(crate) user_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(crate) date_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(crate) paths_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(crate) sort_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(super) details_split_state: Entity<SplitState>,
}

impl JetBrainsGraphState {
    fn new(follow_active_repo: bool, window: &mut Window, cx: &mut Context<GitGraph>) -> Self {
        let branch_filter_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Filter branches…", window, cx);
            editor
        });
        cx.subscribe(&branch_filter_editor, |_, _, event, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                cx.notify();
            }
        })
        .detach();

        let details_split_state = cx.new(|_| SplitState {
            left_ratio: DEFAULT_DETAILS_PANE_RATIO,
            visible_left_ratio: DEFAULT_DETAILS_PANE_RATIO,
        });

        Self {
            follow_active_repo,
            show_branches_pane: true,
            branch_filter_editor,
            branches: Vec::new(),
            _branches_task: None,
            collapsed_sections: HashSet::default(),
            expanded_branch_folders: HashSet::default(),
            branches_scroll_handle: ScrollHandle::new(),
            branch_context_menu: None,
            user_filter: UserFilter::All,
            date_filter: DateFilter::All,
            current_user_email: None,
            _user_email_task: None,
            filtered_commit_indices: None,
            branch_menu_handle: PopoverMenuHandle::default(),
            user_menu_handle: PopoverMenuHandle::default(),
            date_menu_handle: PopoverMenuHandle::default(),
            paths_menu_handle: PopoverMenuHandle::default(),
            sort_menu_handle: PopoverMenuHandle::default(),
            details_split_state,
        }
    }

    fn is_client_filtered(&self, search_query: &str) -> bool {
        !search_query.is_empty()
            || self.user_filter != UserFilter::All
            || self.date_filter != DateFilter::All
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockTab {
    Log,
    History(usize),
}

struct HistoryTab {
    id: usize,
    repo_id: RepositoryId,
    repo_path: RepoPath,
    title: SharedString,
    graph: Entity<GitGraph>,
}

pub struct JetBrainsGitDock {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    log_graph: Option<Entity<GitGraph>>,
    history_tabs: Vec<HistoryTab>,
    active_tab: DockTab,
    next_history_tab_id: usize,
    _subscriptions: Vec<Subscription>,
}

pub fn register(workspace: &mut Workspace, window: Option<&mut Window>, cx: &mut Context<Workspace>) {
    workspace.register_action(|workspace, _: &super::ToggleFocus, window, cx| {
        workspace.toggle_panel_focus::<JetBrainsGitDock>(window, cx);
    });

    if let Some(window) = window {
        let git_store = workspace.project().read(cx).git_store().clone();
        let dock = cx.new(|cx| JetBrainsGitDock::new(workspace.weak_handle(), git_store, window, cx));
        workspace.add_panel(dock, window, cx);
    }
}

impl JetBrainsGitDock {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        git_store: Entity<GitStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let mut subscriptions = Vec::new();

        subscriptions.push(cx.subscribe_in(
            &git_store,
            window,
            |this, git_store, event, window, cx| {
                if matches!(
                    event,
                    GitStoreEvent::RepositoryAdded | GitStoreEvent::ActiveRepositoryChanged(_)
                ) {
                    this.ensure_log_graph(&git_store, window, cx);
                    cx.notify();
                }
            },
        ));

        let mut this = Self {
            workspace,
            focus_handle,
            log_graph: None,
            history_tabs: Vec::new(),
            active_tab: DockTab::Log,
            next_history_tab_id: 1,
            _subscriptions: subscriptions,
        };

        this.ensure_log_graph(&git_store, window, cx);

        this
    }

    fn ensure_log_graph(
        &mut self,
        git_store: &Entity<GitStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<GitGraph>> {
        if let Some(existing) = &self.log_graph {
            return Some(existing.clone());
        }
        let repo = git_store
            .read(cx)
            .active_repository()
            .or_else(|| git_store.read(cx).repositories().values().next().cloned())?;
        let repo_id = repo.read(cx).id;
        let graph = cx.new(|cx| {
            GitGraph::new_jetbrains(
                repo_id,
                git_store.clone(),
                self.workspace.clone(),
                Some(LogSource::All),
                true,
                window,
                cx,
            )
        });
        self.log_graph = Some(graph.clone());
        Some(graph)
    }

    pub fn open_graph(
        &mut self,
        repo_id: RepositoryId,
        git_store: Entity<GitStore>,
        log_source: LogSource,
        sha: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {

        if let LogSource::Path(repo_path) = log_source {
            if let Some(existing) = self
                .history_tabs
                .iter()
                .find(|tab| tab.repo_id == repo_id && tab.repo_path == repo_path)
            {
                let tab_id = existing.id;
                let graph = existing.graph.clone();
                self.active_tab = DockTab::History(tab_id);
                if let Some(sha) = sha {
                    cx.defer(move |cx| {
                        graph.update(cx, |graph, cx| {
                            graph.select_commit_by_sha(sha.as_str(), cx);
                        });
                    });
                }
                cx.notify();
                return;
            }

            let file_name = repo_path
                .file_name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| repo_path.as_unix_str().to_string());
            let title: SharedString = format!("History: {file_name}").into();
            let tab_id = self.next_history_tab_id;
            self.next_history_tab_id += 1;

            let graph = cx.new(|cx| {
                GitGraph::new_jetbrains(
                    repo_id,
                    git_store,
                    self.workspace.clone(),
                    Some(LogSource::Path(repo_path.clone())),
                    false,
                    window,
                    cx,
                )
            });

            self.history_tabs.push(HistoryTab {
                id: tab_id,
                repo_id,
                repo_path,
                title,
                graph: graph.clone(),
            });
            self.active_tab = DockTab::History(tab_id);

            if let Some(sha) = sha {
                cx.defer(move |cx| {
                    graph.update(cx, |graph, cx| {
                        graph.select_commit_by_sha(sha.as_str(), cx);
                    });
                });
            }
            cx.notify();
            return;
        }

        let log_graph = if let Some(existing) = &self.log_graph {
            existing.clone()
        } else {
            let graph = cx.new(|cx| {
                GitGraph::new_jetbrains(
                    repo_id,
                    git_store,
                    self.workspace.clone(),
                    Some(log_source.clone()),
                    true,
                    window,
                    cx,
                )
            });
            self.log_graph = Some(graph.clone());
            graph
        };

        log_graph.update(cx, |graph, cx| {
            if graph.repo_id != repo_id {
                graph.set_repo_id(repo_id, cx);
                graph.reload_jetbrains_repo_metadata(cx);
            }
            if graph.log_source != log_source {
                graph.set_jetbrains_log_source(log_source, cx);
            }
            if let Some(sha) = sha {
                graph.select_commit_by_sha(sha.as_str(), cx);
            }
        });
        self.active_tab = DockTab::Log;
        cx.notify();
    }

    fn close_history_tab(&mut self, tab_id: usize, cx: &mut Context<Self>) {
        let Some(index) = self.history_tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        self.history_tabs.remove(index);
        if self.active_tab == DockTab::History(tab_id) {
            self.active_tab = DockTab::Log;
        }
        cx.notify();
    }

    fn active_graph(&self) -> Option<Entity<GitGraph>> {
        match self.active_tab {
            DockTab::Log => self.log_graph.clone(),
            DockTab::History(tab_id) => self
                .history_tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .map(|tab| tab.graph.clone()),
        }
    }

    fn render_header_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let log_active = self.active_tab == DockTab::Log;
        let workspace = self.workspace.clone();

        h_flex()
            .id("jetbrains-git-dock-header")
            .w_full()
            .h(px(28.))
            .px_2()
            .gap_1()
            .items_center()
            .bg(cx.theme().colors().tab_bar_background)
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .gap_1()
                    .pr_2()
                    .items_center()
                    .child(Icon::new(IconName::GitGraph).size(IconSize::XSmall).color(Color::Muted))
                    .child(
                        Label::new("Git")
                            .size(LabelSize::Small)
                            .weight(FontWeight::SEMIBOLD),
                    ),
            )
            .child(
                div()
                    .id("jetbrains-git-tab-log")
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(log_active, |this| {
                        this.bg(cx.theme().colors().tab_active_background)
                            .border_b_2()
                            .border_color(cx.theme().colors().border_focused)
                    })
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_tab = DockTab::Log;
                        cx.notify();
                    }))
                    .child(
                        Label::new("Log")
                            .size(LabelSize::Small)
                            .when(!log_active, |label| label.color(Color::Muted)),
                    ),
            )
            .children(self.history_tabs.iter().map(|tab| {
                let tab_id = tab.id;
                let is_active = self.active_tab == DockTab::History(tab_id);
                h_flex()
                    .id(ElementId::NamedInteger("jetbrains-git-history-tab".into(), tab_id as u64))
                    .px_2()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_active, |this| {
                        this.bg(cx.theme().colors().tab_active_background)
                            .border_b_2()
                            .border_color(cx.theme().colors().border_focused)
                    })
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_tab = DockTab::History(tab_id);
                        cx.notify();
                    }))
                    .child(
                        Label::new(tab.title.clone())
                            .size(LabelSize::Small)
                            .when(!is_active, |label| label.color(Color::Muted)),
                    )
                    .child(
                        IconButton::new(
                            ElementId::NamedInteger("close-history-tab".into(), tab_id as u64),
                            IconName::Close,
                        )
                        .icon_size(IconSize::XSmall)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_history_tab(tab_id, cx);
                        })),
                    )
            }))
            .child(div().flex_1())
            .child(
                IconButton::new("jetbrains-git-dock-hide", IconName::Dash)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::for_action_title(
                        "Hide",
                        &workspace::ToggleBottomDock,
                    ))
                    .on_click(move |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.toggle_dock(DockPosition::Bottom, window, cx);
                            })
                            .ok();
                    }),
            )
    }
}

impl EventEmitter<PanelEvent> for JetBrainsGitDock {}

impl Focusable for JetBrainsGitDock {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let Some(graph) = self.active_graph() {
            return graph.read(cx).focus_handle.clone();
        }
        self.focus_handle.clone()
    }
}

impl Render for JetBrainsGitDock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.log_graph.is_none() {
            if let Some(workspace) = self.workspace.upgrade() {
                let git_store = workspace.read(cx).project().read(cx).git_store().clone();
                self.ensure_log_graph(&git_store, window, cx);
            }
        }

        v_flex()
            .id("jetbrains-git-dock")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_header_tabs(cx))
            .child(div().flex_1().min_h_0().w_full().map(|container| {
                if let Some(graph) = self.active_graph() {
                    container.child(graph)
                } else {
                    container.child(
                        h_flex()
                            .size_full()
                            .justify_center()
                            .items_center()
                            .child(Label::new("No Git repository open").color(Color::Muted)),
                    )
                }
            }))
    }
}

impl Panel for JetBrainsGitDock {
    fn persistent_name() -> &'static str {
        "JetBrainsGitDock"
    }

    fn panel_key() -> &'static str {
        "JetBrainsGitDock"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Bottom
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Bottom
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        DEFAULT_DOCK_HEIGHT
    }

    fn icon(&self, _: &Window, cx: &App) -> Option<ui::IconName> {
        let settings = GitPanelSettings::get_global(cx);
        (settings.layout == settings::GitPanelLayout::Jetbrains && settings.button)
            .then_some(ui::IconName::GitGraph)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Git")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(super::ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        4
    }
}

struct CreateBranchFromRefModal {
    base_ref: SharedString,
    editor: Entity<Editor>,
    repo: Entity<Repository>,
}

impl CreateBranchFromRefModal {
    fn new(
        base_ref: SharedString,
        repo: Entity<Repository>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("New branch name…", window, cx);
            editor
        });
        Self {
            base_ref,
            editor,
            repo,
        }
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let new_name = normalize_branch_name(&self.editor.read(cx).text(cx));
        if new_name.is_empty() {
            cx.emit(DismissEvent);
            return;
        }
        let repo = self.repo.clone();
        let base_ref = self.base_ref.to_string();
        cx.spawn(async move |_, cx| {
            match repo
                .update(cx, |repo, _| repo.create_branch(new_name, Some(base_ref)))
                .await
            {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(anyhow!("Operation was canceled")),
            }
        })
        .detach_and_prompt_err("Failed to create branch", window, cx, |_, _, _| None);
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for CreateBranchFromRefModal {}
impl ModalView for CreateBranchFromRefModal {}
impl Focusable for CreateBranchFromRefModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for CreateBranchFromRefModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("CreateBranchFromRefModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(28.))
            .p_3()
            .gap_2()
            .child(
                Label::new(format!("New Branch from {}", self.base_ref))
                    .size(LabelSize::Default)
                    .weight(FontWeight::SEMIBOLD),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().editor_background)
                    .child(self.editor.clone()),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-create-branch", "Cancel").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.cancel(&Cancel, window, cx);
                            },
                        )),
                    )
                    .child(
                        Button::new("confirm-create-branch", "Create")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm(&Confirm, window, cx);
                            })),
                    ),
            )
    }
}

struct PushCommitsModal {
    branch_name: SharedString,
    upstream: Option<Upstream>,
    remote_name: Option<SharedString>,
    remote_branch_name: SharedString,
    force: bool,
    is_new_branch: bool,
    loading_commits: bool,
    commits: Vec<CommitSummary>,
    error: Option<SharedString>,
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    _load_task: Task<()>,
}

impl PushCommitsModal {
    fn new(
        branch_name: SharedString,
        upstream: Option<Upstream>,
        force: bool,
        repository: Entity<Repository>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let is_new_branch = match upstream.as_ref() {
            Some(Upstream {
                tracking: UpstreamTracking::Tracked(_),
                ..
            }) => false,
            _ => true,
        };
        let remote_name = upstream
            .as_ref()
            .and_then(|u| u.remote_name().map(SharedString::new));
        let remote_branch_name: SharedString = upstream
            .as_ref()
            .filter(|u| matches!(u.tracking, UpstreamTracking::Tracked(_)))
            .and_then(|u| u.branch_name())
            .map(SharedString::new)
            .unwrap_or_else(|| branch_name.clone());
        let tracked_upstream_ref = upstream
            .as_ref()
            .filter(|u| matches!(u.tracking, UpstreamTracking::Tracked(_)))
            .map(|u| u.ref_name.to_string());

        let load_task = cx.spawn_in(window, {
            let repository = repository.clone();
            let branch_name = branch_name.to_string();
            let initial_remote = remote_name.clone();
            async move |this, cx| {
                let resolved_remote = match initial_remote {
                    Some(remote) => Ok(remote),
                    None => {
                        let remotes_res = repository
                            .update(cx, |repo, _| repo.get_remotes(Some(branch_name.clone()), true))
                            .await;
                        match remotes_res {
                            Ok(Ok(remotes)) => remotes
                                .into_iter()
                                .next()
                                .map(|r| r.name)
                                .ok_or_else(|| "No remote available to push to".to_string()),
                            Ok(Err(err)) => Err(err.to_string()),
                            Err(_) => Err("Failed to load remotes".to_string()),
                        }
                    }
                };

                let commits_res = repository
                    .update(cx, |repo, _| {
                        repo.outgoing_commits(branch_name, tracked_upstream_ref)
                    })
                    .await;

                this.update(cx, |this, cx| {
                    this.loading_commits = false;
                    match resolved_remote {
                        Ok(remote) => this.remote_name = Some(remote),
                        Err(err) => this.error = Some(err.into()),
                    }
                    if let Ok(Ok(commits)) = commits_res {
                        this.commits = commits;
                    }
                    cx.notify();
                })
                .ok();
            }
        });

        Self {
            branch_name,
            upstream,
            remote_name,
            remote_branch_name,
            force,
            is_new_branch,
            loading_commits: true,
            commits: Vec::new(),
            error: None,
            repository,
            workspace,
            focus_handle,
            _load_task: load_task,
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.remote_name.is_none() {
            return;
        }
        push_local_branch(
            self.repository.clone(),
            self.workspace.clone(),
            self.branch_name.clone(),
            self.upstream.clone(),
            self.force,
            window,
            cx,
        );
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for PushCommitsModal {}
impl ModalView for PushCommitsModal {}
impl Focusable for PushCommitsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PushCommitsModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = if self.force {
            "Force Push Commits"
        } else {
            "Push Commits"
        };
        let target_label = match &self.remote_name {
            Some(remote) => format!("{}:{}", remote, self.remote_branch_name),
            None => self.remote_branch_name.to_string(),
        };

        v_flex()
            .key_context("PushCommitsModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(34.))
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Label::new(title)
                            .size(LabelSize::Default)
                            .weight(FontWeight::SEMIBOLD),
                    )
                    .when(self.is_new_branch, |this| {
                        this.child(
                            Label::new("New branch")
                                .size(LabelSize::XSmall)
                                .color(Color::Accent),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .child(
                        Label::new(self.branch_name.clone())
                            .size(LabelSize::Small)
                            .weight(FontWeight::MEDIUM),
                    )
                    .child(
                        Icon::new(IconName::ArrowRight)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(target_label)
                            .size(LabelSize::Small)
                            .weight(FontWeight::MEDIUM),
                    ),
            )
            .child(
                div()
                    .id("push-commits-list")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().editor_background)
                    .p_2()
                    .child(if self.loading_commits {
                        Label::new("Loading outgoing commits…")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .into_any_element()
                    } else if let Some(error) = &self.error {
                        Label::new(error.clone())
                            .size(LabelSize::Small)
                            .color(Color::Error)
                            .into_any_element()
                    } else if self.commits.is_empty() {
                        Label::new("No outgoing commits")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .into_any_element()
                    } else {
                        v_flex()
                            .gap_1()
                            .children(self.commits.iter().map(|commit| {
                                let short_sha: SharedString =
                                    commit.sha.chars().take(7).collect::<String>().into();
                                h_flex()
                                    .gap_2()
                                    .justify_between()
                                    .child(
                                        h_flex()
                                            .gap_1p5()
                                            .min_w_0()
                                            .flex_1()
                                            .child(
                                                Label::new(short_sha)
                                                    .size(LabelSize::XSmall)
                                                    .color(Color::Muted),
                                            )
                                            .child(
                                                Label::new(commit.subject.clone())
                                                    .size(LabelSize::Small)
                                                    .truncate(),
                                            ),
                                    )
                                    .child(
                                        Label::new(commit.author_name.clone())
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted)
                                            .flex_shrink_0(),
                                    )
                            }))
                            .into_any_element()
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-push-commits", "Cancel").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.cancel(&Cancel, window, cx);
                            },
                        )),
                    )
                    .child(
                        Button::new(
                            "confirm-push-commits",
                            if self.force { "Force Push" } else { "Push" },
                        )
                        .style(ButtonStyle::Filled)
                        .color(if self.force {
                            Color::Warning
                        } else {
                            Color::Default
                        })
                        .disabled(self.remote_name.is_none())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm(&Confirm, window, cx);
                        })),
                    ),
            )
    }
}

enum BranchTreeNode {
    Folder {
        name: SharedString,
        full_prefix: String,
        children: BTreeMap<String, BranchTreeNode>,
    },
    Leaf {
        display_name: SharedString,
        branch: Branch,
    },
}

fn insert_branch_into_tree(
    root: &mut BTreeMap<String, BranchTreeNode>,
    parts: &[&str],
    prefix: String,
    branch: Branch,
) {
    let Some((&head, rest)) = parts.split_first() else {
        return;
    };
    if rest.is_empty() {
        root.insert(
            format!("leaf:{head}"),
            BranchTreeNode::Leaf {
                display_name: head.to_string().into(),
                branch,
            },
        );
    } else {
        let next_prefix = if prefix.is_empty() {
            head.to_string()
        } else {
            format!("{prefix}/{head}")
        };
        let entry = root
            .entry(format!("dir:{head}"))
            .or_insert_with(|| BranchTreeNode::Folder {
                name: head.to_string().into(),
                full_prefix: next_prefix.clone(),
                children: BTreeMap::new(),
            });
        if let BranchTreeNode::Folder { children, .. } = entry {
            insert_branch_into_tree(children, rest, next_prefix, branch);
        }
    }
}

impl GitGraph {
    pub(crate) fn new_jetbrains(
        repo_id: RepositoryId,
        git_store: Entity<GitStore>,
        workspace: WeakEntity<Workspace>,
        log_source: Option<LogSource>,
        follow_active_repo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut graph = Self::new(repo_id, git_store, workspace, log_source, window, cx);

        if graph.is_path_history() {
            // Hide Commit column (index 3) in JetBrains path history mode.
            graph.column_visibility = TableRow::from_vec(vec![false, false, false, true], 4);
        } else {
            // Hide Refs (0) and Commit (5) columns so default visible columns are [Graph, Description, Date, Author].
            graph.column_visibility = TableRow::from_vec(
                vec![true, false, false, false, false, true],
                GRAPH_LAYOUT_TABLE_OFFSET + TABLE_COLUMN_COUNT,
            );
        }

        let jb_state = JetBrainsGraphState::new(follow_active_repo, window, cx);
        graph.jetbrains = Some(jb_state);

        // Subscribe to search editor edits so JetBrains search filters rows live.
        let search_editor = graph.search_state.editor.clone();
        cx.subscribe(&search_editor, |this, _, event, cx| {
            if matches!(event, EditorEvent::BufferEdited) && this.jetbrains.is_some() {
                this.recompute_jetbrains_filter(cx);
                cx.notify();
            }
        })
        .detach();

        graph.reload_jetbrains_repo_metadata(cx);
        graph
    }

    pub(crate) fn reload_jetbrains_repo_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(repository) = self.get_repository(cx) else {
            return;
        };

        let work_dir = repository.read(cx).work_directory_abs_path.clone();
        let git_store = self.git_store.clone();
        let email_task = cx.spawn(async move |this, cx| {
            let result = git_store
                .read_with(cx, |store, cx| {
                    store.git_config(work_dir, vec!["--get".into(), "user.email".into()], cx)
                })
                .await;
            if let Ok(email) = result {
                let email = email.trim().to_string();
                this.update(cx, |this, cx| {
                    if let Some(jb) = &mut this.jetbrains {
                        jb.current_user_email = (!email.is_empty()).then_some(email);
                        if jb.user_filter == UserFilter::Me {
                            this.recompute_jetbrains_filter(cx);
                        }
                        cx.notify();
                    }
                })
                .ok();
            }
        });

        let branches_rx = repository.update(cx, |repo, _| repo.branches());
        let branches_task = cx.spawn(async move |this, cx| {
            if let Ok(Ok(scan_result)) = branches_rx.await {
                this.update(cx, |this, cx| {
                    if let Some(jb) = &mut this.jetbrains {
                        jb.branches = scan_result.branches;
                        cx.notify();
                    }
                })
                .ok();
            }
        });

        if let Some(jb) = &mut self.jetbrains {
            jb._user_email_task = Some(email_task);
            jb._branches_task = Some(branches_task);
        }
    }

    pub(super) fn on_jetbrains_active_repository_changed(
        &mut self,
        new_repo_id: RepositoryId,
        cx: &mut Context<Self>,
    ) {
        self.set_repo_id(new_repo_id, cx);
        self.fetch_initial_graph_data(cx);
        self.reload_jetbrains_repo_metadata(cx);
    }

    pub(crate) fn set_jetbrains_log_source(
        &mut self,
        log_source: LogSource,
        cx: &mut Context<Self>,
    ) {
        if self.log_source == log_source {
            return;
        }
        self.log_source = log_source;
        self.pending_select_sha = None;
        self.invalidate_state(cx);
        self.fetch_initial_graph_data(cx);
    }

    pub(crate) fn set_jetbrains_log_order(&mut self, log_order: LogOrder, cx: &mut Context<Self>) {
        if self.log_order == log_order {
            return;
        }
        self.log_order = log_order;
        self.pending_select_sha = None;
        self.invalidate_state(cx);
        self.fetch_initial_graph_data(cx);
    }

    pub(super) fn on_jetbrains_commits_updated(&mut self, cx: &mut Context<Self>) {
        self.recompute_jetbrains_filter(cx);
        if self.selected_entry_idx.is_none() && self.jetbrains_visible_commit_count(cx) > 0 {
            let first_idx = self.jetbrains_commit_index(0);
            if first_idx < self.graph_data.commits.len() {
                self.select_entry(first_idx, ScrollStrategy::Top, cx);
            }
        }
    }

    pub(super) fn jetbrains_commit_index(&self, visible_row_idx: usize) -> usize {
        if let Some(jb) = &self.jetbrains
            && let Some(filtered) = &jb.filtered_commit_indices
        {
            return filtered.get(visible_row_idx).copied().unwrap_or(visible_row_idx);
        }
        visible_row_idx
    }

    fn jetbrains_visible_commit_count(&self, cx: &App) -> usize {
        if let Some(jb) = &self.jetbrains
            && let Some(filtered) = &jb.filtered_commit_indices
        {
            return filtered.len();
        }
        let _ = cx;
        self.graph_data.commits.len()
    }

    fn is_jetbrains_client_filtered(&self, cx: &App) -> bool {
        let Some(jb) = &self.jetbrains else {
            return false;
        };
        let query = self.search_state.editor.read(cx).text(cx);
        jb.is_client_filtered(query.trim())
    }

    pub(crate) fn recompute_jetbrains_filter(&mut self, cx: &mut Context<Self>) {
        let Some(jb) = &self.jetbrains else {
            return;
        };
        let query = self.search_state.editor.read(cx).text(cx);
        let query_trimmed = query.trim().to_string();
        if !jb.is_client_filtered(&query_trimmed) {
            if let Some(jb) = &mut self.jetbrains {
                jb.filtered_commit_indices = None;
            }
            return;
        }

        let Some(repository) = self.get_repository(cx) else {
            return;
        };

        let case_sensitive = self.search_state.case_sensitive;
        let query_lower = query_trimmed.to_lowercase();
        let user_filter = jb.user_filter.clone();
        let date_filter = jb.date_filter;
        let current_user_email = jb.current_user_email.clone();

        let mut matches = Vec::new();
        let total = self.graph_data.commits.len();
        for idx in 0..total {
            let commit = &self.graph_data.commits[idx];
            let sha_str = commit.data.sha.to_string();
            let data = repository.update(cx, |repo, cx| {
                repo.fetch_commit_data(commit.data.sha, false, cx).clone()
            });

            let (subject, author_name, author_email, timestamp) = match data {
                CommitDataState::Loaded(data) => (
                    data.subject.clone(),
                    data.author_name.clone(),
                    data.author_email.clone(),
                    Some(data.commit_timestamp),
                ),
                CommitDataState::Loading(_) => {
                    // Only allow pure SHA match while loading
                    if !query_trimmed.is_empty()
                        && user_filter == UserFilter::All
                        && date_filter == DateFilter::All
                        && sha_str.to_lowercase().starts_with(&query_lower)
                    {
                        matches.push(idx);
                    }
                    continue;
                }
            };

            if let Some(ts) = timestamp
                && !date_filter.matches(ts)
            {
                continue;
            }

            match &user_filter {
                UserFilter::All => {}
                UserFilter::Me => {
                    let Some(me_email) = current_user_email.as_deref() else {
                        continue;
                    };
                    if !author_email.trim().eq_ignore_ascii_case(me_email) {
                        continue;
                    }
                }
                UserFilter::Author(target) => {
                    if author_name.as_ref() != target.as_ref()
                        && author_email.as_ref() != target.as_ref()
                    {
                        continue;
                    }
                }
            }

            if !query_trimmed.is_empty() {
                let text_matches = if case_sensitive {
                    subject.contains(&query_trimmed)
                        || author_name.contains(&query_trimmed)
                        || author_email.contains(&query_trimmed)
                        || sha_str.starts_with(&query_trimmed)
                } else {
                    subject.to_lowercase().contains(&query_lower)
                        || author_name.to_lowercase().contains(&query_lower)
                        || author_email.to_lowercase().contains(&query_lower)
                        || sha_str.to_lowercase().starts_with(&query_lower)
                };
                if !text_matches {
                    continue;
                }
            }

            matches.push(idx);
        }

        if let Some(jb) = &mut self.jetbrains {
            jb.filtered_commit_indices = Some(matches);
        }
    }

    fn deploy_jetbrains_branch_context_menu(
        &mut self,
        position: Point<Pixels>,
        ref_name: SharedString,
        is_branch: bool,
        is_local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repository) = self.get_repository(cx) else {
            return;
        };
        let current_branch = repository.read(cx).branch.clone();
        let current_branch_name = current_branch
            .as_ref()
            .map(|branch| SharedString::from(branch.name().to_string()));
        let is_head = is_branch
            && is_local
            && current_branch_name
                .as_ref()
                .is_some_and(|head| head == &ref_name);
        let target_branch_upstream = if is_branch && is_local {
            if is_head {
                current_branch.as_ref().and_then(|b| b.upstream.clone())
            } else {
                self.jetbrains.as_ref().and_then(|jb| {
                    jb.branches
                        .iter()
                        .find(|b| !b.is_remote() && b.name() == ref_name.as_ref())
                        .and_then(|b| b.upstream.clone())
                })
            }
        } else {
            None
        };
        let workspace = self.workspace.clone();
        let ref_for_menu = ref_name;

        let menu = ContextMenu::build(window, cx, move |mut menu, _window, _cx| {
            if !is_head {
                let checkout_repo = repository.clone();
                let checkout_ref = ref_for_menu.clone();
                menu = menu.entry(
                    format!("Checkout {}", checkout_ref),
                    None,
                    move |window, cx| {
                        let repo = checkout_repo.clone();
                        let target = checkout_ref.to_string();
                        cx.spawn(async move |cx| {
                            match repo.update(cx, |repo, _| repo.change_branch(target)).await {
                                Ok(Ok(_)) => Ok(()),
                                Ok(Err(error)) => Err(error),
                                Err(_) => Err(anyhow!("Operation was canceled")),
                            }
                        })
                        .detach_and_prompt_err("Failed to checkout", window, cx, |_, _, _| None);
                    },
                );
            }

            let new_branch_repo = repository.clone();
            let new_branch_ref = ref_for_menu.clone();
            let new_branch_workspace = workspace.clone();
            menu = menu.entry(
                format!("New Branch from {}…", new_branch_ref),
                None,
                move |window, cx| {
                    let repo = new_branch_repo.clone();
                    let base = new_branch_ref.clone();
                    new_branch_workspace
                        .update(cx, |workspace, cx| {
                            workspace.toggle_modal(window, cx, |window, cx| {
                                CreateBranchFromRefModal::new(base, repo, window, cx)
                            });
                        })
                        .ok();
                },
            );

            if is_head {
                if let Some(upstream) = current_branch.as_ref().and_then(|b| b.upstream.as_ref()) {
                    if let Some(remote_name) = upstream.remote_name().map(str::to_owned) {
                        let remote_branch = upstream.branch_name().map(SharedString::from);
                        for (label, rebase) in
                            [("Pull", false), ("Pull Using Rebase", true)]
                        {
                            let pull_repo = repository.clone();
                            let pull_workspace = workspace.clone();
                            let pull_remote: SharedString = remote_name.clone().into();
                            let pull_branch = remote_branch.clone();
                            menu = menu.entry(label, None, move |window, cx| {
                                pull_branch_into_current(
                                    pull_repo.clone(),
                                    pull_workspace.clone(),
                                    pull_branch.clone(),
                                    pull_remote.clone(),
                                    rebase,
                                    window,
                                    cx,
                                );
                            });
                        }
                    }
                }
            }

            if is_branch && is_local {
                let push_repo = repository.clone();
                let push_workspace = workspace.clone();
                let push_branch = ref_for_menu.clone();
                let push_upstream = target_branch_upstream.clone();
                menu = menu.entry("Push…", None, move |window, cx| {
                    let repo = push_repo.clone();
                    let workspace = push_workspace.clone();
                    let branch = push_branch.clone();
                    let upstream = push_upstream.clone();
                    push_workspace
                        .update(cx, |ws, cx| {
                            ws.toggle_modal(window, cx, |window, cx| {
                                PushCommitsModal::new(
                                    branch,
                                    upstream,
                                    false,
                                    repo,
                                    workspace,
                                    window,
                                    cx,
                                )
                            });
                        })
                        .ok();
                });

                if target_branch_upstream.is_some() {
                    let force_push_repo = repository.clone();
                    let force_push_workspace = workspace.clone();
                    let force_push_branch = ref_for_menu.clone();
                    let force_push_upstream = target_branch_upstream.clone();
                    menu = menu.entry("Force Push…", None, move |window, cx| {
                        let repo = force_push_repo.clone();
                        let workspace = force_push_workspace.clone();
                        let branch = force_push_branch.clone();
                        let upstream = force_push_upstream.clone();
                        force_push_workspace
                            .update(cx, |ws, cx| {
                                ws.toggle_modal(window, cx, |window, cx| {
                                    PushCommitsModal::new(
                                        branch,
                                        upstream,
                                        true,
                                        repo,
                                        workspace,
                                        window,
                                        cx,
                                    )
                                });
                            })
                            .ok();
                    });
                }
            }

            if !is_head {
                if let Some(current_branch) = &current_branch_name {
                    if is_branch && !is_local {
                        if let Some((remote_name, remote_branch)) = ref_for_menu.split_once('/') {
                            for (label, rebase) in [
                                (format!("Pull into '{}' Using Merge", current_branch), false),
                                (format!("Pull into '{}' Using Rebase", current_branch), true),
                            ] {
                                let pull_repo = repository.clone();
                                let pull_workspace = workspace.clone();
                                let pull_remote: SharedString = remote_name.to_string().into();
                                let pull_branch =
                                    Some(SharedString::from(remote_branch.to_string()));
                                menu = menu.entry(label, None, move |window, cx| {
                                    pull_branch_into_current(
                                        pull_repo.clone(),
                                        pull_workspace.clone(),
                                        pull_branch.clone(),
                                        pull_remote.clone(),
                                        rebase,
                                        window,
                                        cx,
                                    );
                                });
                            }

                            let fetch_repo = repository.clone();
                            let fetch_workspace = workspace.clone();
                            let fetch_remote_name = remote_name.to_string();
                            menu = menu.entry(
                                format!("Fetch {}", remote_name),
                                None,
                                move |window, cx| {
                                    fetch_remote(
                                        fetch_repo.clone(),
                                        fetch_workspace.clone(),
                                        Some(fetch_remote_name.clone()),
                                        window,
                                        cx,
                                    );
                                },
                            );
                        }
                    }

                    let merge_repo = repository.clone();
                    let merge_ref = ref_for_menu.clone();
                    menu = menu.entry(
                        format!("Merge '{}' into '{}'", ref_for_menu, current_branch),
                        None,
                        move |window, cx| {
                            let repo = merge_repo.clone();
                            let target = merge_ref.to_string();
                            cx.spawn(async move |cx| {
                                match repo.update(cx, |repo, _| repo.merge(target)).await {
                                    Ok(Ok(_)) => Ok(()),
                                    Ok(Err(error)) => Err(error),
                                    Err(_) => Err(anyhow!("Operation was canceled")),
                                }
                            })
                            .detach_and_prompt_err("Failed to merge", window, cx, |_, _, _| None);
                        },
                    );

                    let rebase_repo = repository.clone();
                    let rebase_ref = ref_for_menu.clone();
                    menu = menu.entry(
                        format!("Rebase '{}' onto '{}'", current_branch, ref_for_menu),
                        None,
                        move |window, cx| {
                            let repo = rebase_repo.clone();
                            let target = rebase_ref.to_string();
                            cx.spawn(async move |cx| {
                                match repo.update(cx, |repo, _| repo.rebase(target)).await {
                                    Ok(Ok(_)) => Ok(()),
                                    Ok(Err(error)) => Err(error),
                                    Err(_) => Err(anyhow!("Operation was canceled")),
                                }
                            })
                            .detach_and_prompt_err("Failed to rebase", window, cx, |_, _, _| None);
                        },
                    );

                    if is_branch {
                        let checkout_rebase_repo = repository.clone();
                        let checkout_rebase_ref = ref_for_menu.clone();
                        let checkout_rebase_current = current_branch.clone();
                        menu = menu.entry(
                            format!("Checkout and Rebase onto '{}'", current_branch),
                            None,
                            move |window, cx| {
                                let repo = checkout_rebase_repo.clone();
                                let target = checkout_rebase_ref.to_string();
                                let onto = checkout_rebase_current.to_string();
                                cx.spawn(async move |cx| {
                                    match repo
                                        .update(cx, |repo, _| repo.change_branch(target))
                                        .await
                                    {
                                        Ok(Ok(())) => {}
                                        Ok(Err(error)) => return Err(error),
                                        Err(_) => return Err(anyhow!("Operation was canceled")),
                                    }
                                    match repo.update(cx, |repo, _| repo.rebase(onto)).await {
                                        Ok(Ok(())) => Ok(()),
                                        Ok(Err(error)) => Err(error),
                                        Err(_) => Err(anyhow!("Operation was canceled")),
                                    }
                                })
                                .detach_and_prompt_err(
                                    "Failed to checkout and rebase",
                                    window,
                                    cx,
                                    |_, _, _| None,
                                );
                            },
                        );
                    }
                }
            }

            if is_branch && is_local {
                let rename_repo = repository.clone();
                let rename_ref = ref_for_menu.clone();
                let rename_workspace = workspace.clone();
                menu = menu.entry("Rename Branch…", None, move |window, cx| {
                    let repo = rename_repo.clone();
                    let branch = rename_ref.to_string();
                    rename_workspace
                        .update(cx, |workspace, cx| {
                            workspace.toggle_modal(window, cx, |window, cx| {
                                RenameBranchModal::new(branch, repo, window, cx)
                            });
                        })
                        .ok();
                });
            }

            if is_branch && !is_head {
                let delete_repo = repository.clone();
                let delete_ref = ref_for_menu.clone();
                menu = menu.entry("Delete Branch", None, move |window, cx| {
                    let repo = delete_repo.clone();
                    let branch = delete_ref.to_string();
                    cx.spawn(async move |cx| {
                        match repo
                            .update(cx, |repo, _| repo.delete_branch(!is_local, branch, false))
                            .await
                        {
                            Ok(Ok(_)) => Ok(()),
                            Ok(Err(error)) => Err(error),
                            Err(_) => Err(anyhow!("Operation was canceled")),
                        }
                    })
                    .detach_and_prompt_err("Failed to delete branch", window, cx, |_, _, _| None);
                });
            }

            let copy_ref = ref_for_menu;
            menu.separator().entry("Copy Name", None, move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_ref.to_string()));
            })
        });

        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            if let Some(jb) = &mut this.jetbrains {
                jb.branch_context_menu = None;
            }
            cx.notify();
        });

        if let Some(jb) = &mut self.jetbrains {
            jb.branch_context_menu = Some(BranchContextMenuState {
                position,
                menu,
                _subscription: subscription,
            });
        }
        cx.notify();
    }

    fn deploy_jetbrains_remote_context_menu(
        &mut self,
        position: Point<Pixels>,
        remote_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repository) = self.get_repository(cx) else {
            return;
        };
        let workspace = self.workspace.clone();
        let label = match &remote_name {
            Some(name) => format!("Fetch {}", name),
            None => "Fetch All".to_string(),
        };

        let menu = ContextMenu::build(window, cx, move |menu, _window, _cx| {
            menu.entry(label, None, move |window, cx| {
                fetch_remote(
                    repository.clone(),
                    workspace.clone(),
                    remote_name.clone(),
                    window,
                    cx,
                );
            })
        });

        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            if let Some(jb) = &mut this.jetbrains {
                jb.branch_context_menu = None;
            }
            cx.notify();
        });

        if let Some(jb) = &mut self.jetbrains {
            jb.branch_context_menu = Some(BranchContextMenuState {
                position,
                menu,
                _subscription: subscription,
            });
        }
        cx.notify();
    }

    fn toggle_jetbrains_branch_filter(
        &mut self,
        target: SharedString,
        cx: &mut Context<Self>,
    ) {
        if self.log_source == LogSource::Branch(target.clone()) {
            self.set_jetbrains_log_source(LogSource::All, cx);
        } else {
            self.set_jetbrains_log_source(LogSource::Branch(target), cx);
        }
    }

    fn render_jetbrains_branches_pane(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(jb) = &self.jetbrains else {
            return Empty.into_any_element();
        };

        let filter_text = jb.branch_filter_editor.read(cx).text(cx).trim().to_lowercase();
        let branches = jb.branches.clone();
        let collapsed_sections = jb.collapsed_sections.clone();
        let expanded_folders = jb.expanded_branch_folders.clone();
        let branch_filter_editor = jb.branch_filter_editor.clone();
        let branches_scroll_handle = jb.branches_scroll_handle.clone();

        let head_branch = self
            .get_repository(cx)
            .and_then(|repo| repo.read(cx).branch.clone());
        let head_branch_name = head_branch
            .as_ref()
            .map(|b| SharedString::from(b.name().to_string()));
        let head_tracking_status = head_branch.as_ref().and_then(|b| b.tracking_status());

        let mut local_tree: BTreeMap<String, BranchTreeNode> = BTreeMap::new();
        let mut remote_tree: BTreeMap<String, BranchTreeNode> = BTreeMap::new();

        for branch in branches {
            let name = branch.name().to_string();
            if !filter_text.is_empty() && !name.to_lowercase().contains(&filter_text) {
                continue;
            }
            let parts: Vec<&str> = name.split('/').collect();
            if branch.is_remote() {
                insert_branch_into_tree(&mut remote_tree, &parts, String::new(), branch);
            } else {
                insert_branch_into_tree(&mut local_tree, &parts, String::new(), branch);
            }
        }

        let mut tags_set = IndexSet::default();
        for commit in &self.graph_data.commits {
            for tag in commit.data.tag_names() {
                if filter_text.is_empty() || tag.to_lowercase().contains(&filter_text) {
                    tags_set.insert(tag);
                }
            }
        }
        let tags: Vec<SharedString> = tags_set
            .into_iter()
            .map(|tag| SharedString::from(tag.to_string()))
            .collect();

        let all_selected = self.log_source == LogSource::All;
        let head_selected = head_branch_name
            .as_ref()
            .is_some_and(|head| self.log_source == LogSource::Branch(head.clone()));

        v_flex()
            .id("jetbrains-branches-pane")
            .w(DEFAULT_BRANCH_PANE_WIDTH)
            .min_w(px(160.))
            .h_full()
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .child(
                div()
                    .px_2()
                    .py_1p5()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(branch_filter_editor),
            )
            .child(
                v_flex()
                    .id("jetbrains-branches-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&branches_scroll_handle)
                    .p_1()
                    .gap_0p5()
                    .child(
                        ListItem::new("jb-branch-all")
                            .spacing(ListItemSpacing::Sparse)
                            .toggle_state(all_selected)
                            .start_slot(Icon::new(IconName::GitBranch).size(IconSize::XSmall))
                            .child(Label::new("All").size(LabelSize::Small))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_jetbrains_log_source(LogSource::All, cx);
                            })),
                    )
                    .when_some(head_branch_name, |this, head| {
                        let head_for_click = head.clone();
                        let head_for_menu = head.clone();
                        let tracking_indicator =
                            head_tracking_status.and_then(render_tracking_status_indicator);
                        this.child(
                            div()
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.deploy_jetbrains_branch_context_menu(
                                            event.position,
                                            head_for_menu.clone(),
                                            true,
                                            true,
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                                .child(
                                    ListItem::new("jb-branch-head")
                                        .spacing(ListItemSpacing::Sparse)
                                        .toggle_state(head_selected)
                                        .start_slot(
                                            Icon::new(IconName::Check).size(IconSize::XSmall),
                                        )
                                        .when_some(tracking_indicator, |this, indicator| {
                                            this.end_slot(indicator)
                                        })
                                        .child(
                                            Label::new(format!("HEAD ({head})"))
                                                .size(LabelSize::Small)
                                                .truncate(),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_jetbrains_branch_filter(
                                                head_for_click.clone(),
                                                cx,
                                            );
                                        })),
                                ),
                        )
                    })
                    .child(self.render_branch_section(
                        "Local",
                        &local_tree,
                        true,
                        &collapsed_sections,
                        &expanded_folders,
                        cx,
                    ))
                    .child(self.render_branch_section(
                        "Remote",
                        &remote_tree,
                        false,
                        &collapsed_sections,
                        &expanded_folders,
                        cx,
                    ))
                    .child(self.render_tags_section(&tags, &collapsed_sections, cx)),
            )
            .into_any_element()
    }

    fn render_branch_section(
        &self,
        section_name: &'static str,
        tree: &BTreeMap<String, BranchTreeNode>,
        is_local: bool,
        collapsed_sections: &HashSet<&'static str>,
        expanded_folders: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_collapsed = collapsed_sections.contains(section_name);
        v_flex()
            .w_full()
            .child(
                div()
                    .when(!is_local, |this| {
                        this.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.deploy_jetbrains_remote_context_menu(
                                    event.position,
                                    None,
                                    window,
                                    cx,
                                );
                            }),
                        )
                    })
                    .child(
                        ListItem::new(SharedString::from(format!("jb-section-{section_name}")))
                            .spacing(ListItemSpacing::Sparse)
                            .start_slot(
                                Disclosure::new(
                                    SharedString::from(format!("jb-disc-{section_name}")),
                                    !is_collapsed,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(jb) = &mut this.jetbrains {
                                        if !jb.collapsed_sections.insert(section_name) {
                                            jb.collapsed_sections.remove(section_name);
                                        }
                                        cx.notify();
                                    }
                                })),
                            )
                            .child(
                                Label::new(section_name)
                                    .size(LabelSize::Small)
                                    .weight(FontWeight::SEMIBOLD)
                                    .color(Color::Muted),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(jb) = &mut this.jetbrains {
                                    if !jb.collapsed_sections.insert(section_name) {
                                        jb.collapsed_sections.remove(section_name);
                                    }
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .when(!is_collapsed, |this| {
                this.children(self.render_branch_tree_nodes(
                    section_name,
                    tree,
                    1,
                    is_local,
                    expanded_folders,
                    cx,
                ))
            })
    }

    fn render_branch_tree_nodes(
        &self,
        section_prefix: &str,
        tree: &BTreeMap<String, BranchTreeNode>,
        depth: usize,
        is_local: bool,
        expanded_folders: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut elements = Vec::new();
        for node in tree.values() {
            match node {
                BranchTreeNode::Folder {
                    name,
                    full_prefix,
                    children,
                } => {
                    let folder_key = format!("{section_prefix}:{full_prefix}");
                    let is_expanded = expanded_folders.contains(&folder_key);
                    let folder_key_for_click = folder_key.clone();
                    let remote_for_menu = (!is_local && depth == 1).then(|| name.clone());
                    elements.push(
                        div()
                            .when_some(remote_for_menu, |this, remote_name| {
                                this.on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.deploy_jetbrains_remote_context_menu(
                                            event.position,
                                            Some(remote_name.to_string()),
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                            })
                            .child(
                                ListItem::new(SharedString::from(format!("jb-folder-{folder_key}")))
                                    .spacing(ListItemSpacing::Sparse)
                                    .indent_level(depth)
                                    .indent_step_size(px(14.))
                                    .start_slot(Disclosure::new(
                                        SharedString::from(format!("jb-folder-disc-{folder_key}")),
                                        is_expanded,
                                    ))
                                    .child(
                                        Label::new(name.clone())
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(jb) = &mut this.jetbrains {
                                            if !jb
                                                .expanded_branch_folders
                                                .insert(folder_key_for_click.clone())
                                            {
                                                jb.expanded_branch_folders
                                                    .remove(&folder_key_for_click);
                                            }
                                            cx.notify();
                                        }
                                    })),
                            )
                            .into_any_element(),
                    );
                    if is_expanded {
                        elements.extend(self.render_branch_tree_nodes(
                            section_prefix,
                            children,
                            depth + 1,
                            is_local,
                            expanded_folders,
                            cx,
                        ));
                    }
                }
                BranchTreeNode::Leaf {
                    display_name,
                    branch,
                } => {
                    let full_name: SharedString = branch.name().to_string().into();
                    let is_selected = self.log_source == LogSource::Branch(full_name.clone());
                    let is_head = branch.is_head;
                    let click_name = full_name.clone();
                    let menu_name = full_name.clone();
                    elements.push(
                        div()
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.deploy_jetbrains_branch_context_menu(
                                        event.position,
                                        menu_name.clone(),
                                        true,
                                        is_local,
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .child(
                                ListItem::new(SharedString::from(format!(
                                    "jb-branch-{section_prefix}-{full_name}"
                                )))
                                .spacing(ListItemSpacing::Sparse)
                                .indent_level(depth)
                                .indent_step_size(px(14.))
                                .toggle_state(is_selected)
                                .start_slot(
                                    Icon::new(if is_head {
                                        IconName::Check
                                    } else {
                                        IconName::GitBranch
                                    })
                                    .size(IconSize::XSmall)
                                    .color(if is_head { Color::Accent } else { Color::Muted }),
                                )
                                .when_some(
                                    branch
                                        .tracking_status()
                                        .and_then(render_tracking_status_indicator),
                                    |this, indicator| this.end_slot(indicator),
                                )
                                .child(Label::new(display_name.clone()).size(LabelSize::Small).truncate())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_jetbrains_branch_filter(click_name.clone(), cx);
                                })),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        elements
    }

    fn render_tags_section(
        &self,
        tags: &[SharedString],
        collapsed_sections: &HashSet<&'static str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_collapsed = collapsed_sections.contains("Tags");
        v_flex()
            .w_full()
            .child(
                ListItem::new("jb-section-Tags")
                    .spacing(ListItemSpacing::Sparse)
                    .start_slot(Disclosure::new("jb-disc-Tags", !is_collapsed))
                    .child(
                        Label::new("Tags")
                            .size(LabelSize::Small)
                            .weight(FontWeight::SEMIBOLD)
                            .color(Color::Muted),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(jb) = &mut this.jetbrains {
                            if !jb.collapsed_sections.insert("Tags") {
                                jb.collapsed_sections.remove("Tags");
                            }
                            cx.notify();
                        }
                    })),
            )
            .when(!is_collapsed, |this| {
                this.children(tags.iter().map(|tag| {
                    let is_selected = self.log_source == LogSource::Branch(tag.clone());
                    let click_tag = tag.clone();
                    let menu_tag = tag.clone();
                    div()
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.deploy_jetbrains_branch_context_menu(
                                    event.position,
                                    menu_tag.clone(),
                                    false,
                                    false,
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .child(
                            ListItem::new(SharedString::from(format!("jb-tag-{tag}")))
                                .spacing(ListItemSpacing::Sparse)
                                .indent_level(1)
                                .indent_step_size(px(14.))
                                .toggle_state(is_selected)
                                .start_slot(
                                    Icon::new(IconName::Hash)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .child(Label::new(tag.clone()).size(LabelSize::Small).truncate())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_jetbrains_branch_filter(click_tag.clone(), cx);
                                })),
                        )
                }))
            })
    }

    fn render_jetbrains_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(jb) = &self.jetbrains else {
            return Empty.into_any_element();
        };

        let is_path_history = self.is_path_history();
        let multi_repo = self.git_store.read(cx).repositories().len() > 1;
        let active_repo_name: SharedString = self
            .get_repository(cx)
            .and_then(|repo| {
                repo.read(cx)
                    .work_directory_abs_path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "Repository".to_string())
            .into();

        let project = self
            .workspace
            .read_with(cx, |workspace, _| workspace.project().clone())
            .ok();

        let branch_label: SharedString = match &self.log_source {
            LogSource::All => "Branch: All".into(),
            LogSource::Branch(name) => format!("Branch: {name}").into(),
            LogSource::Sha(sha) => format!("Sha: {}", sha.display_short()).into(),
            LogSource::Path(_) => "Branch: All".into(),
        };
        let user_label = jb.user_filter.label();
        let date_label = jb.date_filter.label();
        let paths_label: SharedString = match &self.log_source {
            LogSource::Path(path) => format!("Path: {}", path.as_unix_str()).into(),
            _ => "Paths".into(),
        };
        let sort_label = match self.log_order {
            LogOrder::DateOrder => "DateOrder",
            LogOrder::TopoOrder => "TopoOrder",
            LogOrder::AuthorDateOrder => "AuthorDateOrder",
            LogOrder::ReverseChronological => "Reverse",
        };

        let weak_self = cx.weak_entity();
        let branch_menu_handle = jb.branch_menu_handle.clone();
        let user_menu_handle = jb.user_menu_handle.clone();
        let date_menu_handle = jb.date_menu_handle.clone();
        let paths_menu_handle = jb.paths_menu_handle.clone();
        let sort_menu_handle = jb.sort_menu_handle.clone();
        let show_branches_pane = jb.show_branches_pane;
        let case_sensitive = self.search_state.case_sensitive;

        h_flex()
            .id("jetbrains-git-toolbar")
            .w_full()
            .px_2()
            .py_1()
            .gap_1p5()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .when(multi_repo && project.is_some() && !is_path_history, |this| {
                let project = project.clone();
                this.child(
                    PopoverMenu::new("jb-graph-repo-selector")
                        .menu(move |window, cx| {
                            project.clone().map(|project| {
                                cx.new(|cx| RepositorySelector::new(project, rems(20.), window, cx))
                            })
                        })
                        .trigger(
                            Button::new("jb-graph-repo-btn", active_repo_name)
                                .style(ButtonStyle::Subtle)
                                .label_size(LabelSize::Small)
                                .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                        ),
                )
            })
            .child(
                h_flex()
                    .w(px(220.))
                    .px_1p5()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().editor_background)
                    .child(
                        Icon::new(IconName::MagnifyingGlass)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(div().flex_1().min_w_0().child(self.search_state.editor.clone()))
                    .child(
                        IconButton::new("jb-search-case-sensitive", IconName::CaseSensitive)
                            .icon_size(IconSize::XSmall)
                            .toggle_state(case_sensitive)
                            .tooltip(Tooltip::text("Match Case"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.search_state.case_sensitive = !this.search_state.case_sensitive;
                                this.recompute_jetbrains_filter(cx);
                                cx.notify();
                            })),
                    ),
            )
            .when(!is_path_history, |this| {
                let weak_for_branch = weak_self.clone();
                let weak_for_paths = weak_self.clone();
                this.child(
                    PopoverMenu::new("jb-branch-filter-menu")
                        .with_handle(branch_menu_handle)
                        .menu(move |window, cx| {
                            let weak = weak_for_branch.clone();
                            Some(ContextMenu::build(window, cx, move |mut menu, _, cx| {
                                let weak_all = weak.clone();
                                menu = menu.entry("All", None, move |_, cx| {
                                    weak_all
                                        .update(cx, |this, cx| {
                                            this.set_jetbrains_log_source(LogSource::All, cx);
                                        })
                                        .ok();
                                });
                                if let Some(graph) = weak.upgrade() {
                                    let head = graph.read(cx).get_repository(cx).and_then(|r| {
                                        r.read(cx).branch.as_ref().map(|b| b.name().to_string())
                                    });
                                    if let Some(head) = head {
                                        let weak_head = weak.clone();
                                        menu = menu.entry(
                                            format!("HEAD ({head})"),
                                            None,
                                            move |_, cx| {
                                                weak_head
                                                    .update(cx, |this, cx| {
                                                        this.set_jetbrains_log_source(
                                                            LogSource::Branch(head.clone().into()),
                                                            cx,
                                                        );
                                                    })
                                                    .ok();
                                            },
                                        );
                                    }
                                    let branches = graph
                                        .read(cx)
                                        .jetbrains
                                        .as_ref()
                                        .map(|jb| jb.branches.clone())
                                        .unwrap_or_default();
                                    if !branches.is_empty() {
                                        menu = menu.separator();
                                        for branch in branches.into_iter().take(40) {
                                            let name: SharedString = branch.name().to_string().into();
                                            let weak_branch = weak.clone();
                                            menu = menu.entry(
                                                name.clone(),
                                                None,
                                                move |_, cx| {
                                                    weak_branch
                                                        .update(cx, |this, cx| {
                                                            this.set_jetbrains_log_source(
                                                                LogSource::Branch(name.clone()),
                                                                cx,
                                                            );
                                                        })
                                                        .ok();
                                                },
                                            );
                                        }
                                    }
                                }
                                menu
                            }))
                        })
                        .trigger(
                            Button::new("jb-branch-filter-btn", branch_label)
                                .style(ButtonStyle::Subtle)
                                .label_size(LabelSize::Small)
                                .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                        ),
                )
                .child(
                    PopoverMenu::new("jb-paths-filter-menu")
                        .with_handle(paths_menu_handle)
                        .menu(move |window, cx| {
                            let weak = weak_for_paths.clone();
                            Some(ContextMenu::build(window, cx, move |mut menu, _, cx| {
                                let weak_all = weak.clone();
                                menu = menu.entry("All", None, move |_, cx| {
                                    weak_all
                                        .update(cx, |this, cx| {
                                            this.set_jetbrains_log_source(LogSource::All, cx);
                                        })
                                        .ok();
                                });
                                if let Some(graph) = weak.upgrade()
                                    && let Some(workspace) = graph.read(cx).workspace.upgrade()
                                {
                                    let folders: Vec<String> = workspace
                                        .read(cx)
                                        .project()
                                        .read(cx)
                                        .worktrees(cx)
                                        .flat_map(|wt| {
                                            wt.read(cx)
                                                .child_entries(util::rel_path::RelPath::empty())
                                                .filter(|entry| entry.is_dir())
                                                .map(|entry| entry.path.as_unix_str().to_string())
                                                .collect::<Vec<_>>()
                                        })
                                        .take(30)
                                        .collect();
                                    if !folders.is_empty() {
                                        menu = menu.separator();
                                        for folder in folders {
                                            let weak_path = weak.clone();
                                            menu = menu.entry(
                                                folder.clone(),
                                                None,
                                                move |_, cx| {
                                                    weak_path
                                                        .update(cx, |this, cx| {
                                                            if let Ok(repo_path) =
                                                                RepoPath::new(&folder)
                                                            {
                                                                this.set_jetbrains_log_source(
                                                                    LogSource::Path(repo_path),
                                                                    cx,
                                                                );
                                                            }
                                                        })
                                                        .ok();
                                                },
                                            );
                                        }
                                    }
                                }
                                menu
                            }))
                        })
                        .trigger(
                            Button::new("jb-paths-filter-btn", paths_label)
                                .style(ButtonStyle::Subtle)
                                .label_size(LabelSize::Small)
                                .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                        ),
                )
            })
            .child({
                let weak_for_user = weak_self.clone();
                PopoverMenu::new("jb-user-filter-menu")
                    .with_handle(user_menu_handle)
                    .menu(move |window, cx| {
                        let weak = weak_for_user.clone();
                        Some(ContextMenu::build(window, cx, move |mut menu, _, cx| {
                            let weak_all = weak.clone();
                            menu = menu.entry("All", None, move |_, cx| {
                                weak_all
                                    .update(cx, |this, cx| {
                                        if let Some(jb) = &mut this.jetbrains {
                                            jb.user_filter = UserFilter::All;
                                        }
                                        this.recompute_jetbrains_filter(cx);
                                        cx.notify();
                                    })
                                    .ok();
                            });
                            let weak_me = weak.clone();
                            menu = menu.entry("me", None, move |_, cx| {
                                weak_me
                                    .update(cx, |this, cx| {
                                        if let Some(jb) = &mut this.jetbrains {
                                            jb.user_filter = UserFilter::Me;
                                        }
                                        this.recompute_jetbrains_filter(cx);
                                        cx.notify();
                                    })
                                    .ok();
                            });
                            if let Some(graph) = weak.upgrade() {
                                let authors = graph.update(cx, |graph, cx| {
                                    graph.collect_loaded_authors(cx)
                                });
                                if !authors.is_empty() {
                                    menu = menu.separator();
                                    for author in authors.into_iter().take(25) {
                                        let weak_author = weak.clone();
                                        menu = menu.entry(author.clone(), None, move |_, cx| {
                                            weak_author
                                                .update(cx, |this, cx| {
                                                    if let Some(jb) = &mut this.jetbrains {
                                                        jb.user_filter =
                                                            UserFilter::Author(author.clone());
                                                    }
                                                    this.recompute_jetbrains_filter(cx);
                                                    cx.notify();
                                                })
                                                .ok();
                                        });
                                    }
                                }
                            }
                            menu
                        }))
                    })
                    .trigger(
                        Button::new("jb-user-filter-btn", user_label)
                            .style(ButtonStyle::Subtle)
                            .label_size(LabelSize::Small)
                            .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                    )
            })
            .child({
                let weak_for_date = weak_self.clone();
                PopoverMenu::new("jb-date-filter-menu")
                    .with_handle(date_menu_handle)
                    .menu(move |window, cx| {
                        let weak = weak_for_date.clone();
                        Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                            for (label, filter) in [
                                ("All", DateFilter::All),
                                ("Today", DateFilter::Today),
                                ("Yesterday", DateFilter::Yesterday),
                                ("Last 7 Days", DateFilter::Last7Days),
                                ("Last 30 Days", DateFilter::Last30Days),
                            ] {
                                let weak_item = weak.clone();
                                menu = menu.entry(label, None, move |_, cx| {
                                    weak_item
                                        .update(cx, |this, cx| {
                                            if let Some(jb) = &mut this.jetbrains {
                                                jb.date_filter = filter;
                                            }
                                            this.recompute_jetbrains_filter(cx);
                                            cx.notify();
                                        })
                                        .ok();
                                });
                            }
                            menu
                        }))
                    })
                    .trigger(
                        Button::new("jb-date-filter-btn", date_label)
                            .style(ButtonStyle::Subtle)
                            .label_size(LabelSize::Small)
                            .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                    )
            })
            .child({
                let weak_for_sort = weak_self;
                PopoverMenu::new("jb-sort-order-menu")
                    .with_handle(sort_menu_handle)
                    .menu(move |window, cx| {
                        let weak = weak_for_sort.clone();
                        Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                            for (label, order) in [
                                ("Date Order", LogOrder::DateOrder),
                                ("Topo Order", LogOrder::TopoOrder),
                                ("Author Date Order", LogOrder::AuthorDateOrder),
                                ("Reverse Chronological", LogOrder::ReverseChronological),
                            ] {
                                let weak_item = weak.clone();
                                menu = menu.entry(label, None, move |_, cx| {
                                    weak_item
                                        .update(cx, |this, cx| {
                                            this.set_jetbrains_log_order(order, cx);
                                        })
                                        .ok();
                                });
                            }
                            menu
                        }))
                    })
                    .trigger(
                        Button::new("jb-sort-order-btn", sort_label)
                            .style(ButtonStyle::Subtle)
                            .label_size(LabelSize::Small)
                            .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall)),
                    )
            })
            .child(div().flex_1())
            .child(
                IconButton::new("jb-refresh-graph", IconName::RotateCw)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Refresh"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.invalidate_state(cx);
                        this.fetch_initial_graph_data(cx);
                        this.reload_jetbrains_repo_metadata(cx);
                    })),
            )
            .child(
                IconButton::new("jb-go-to-ref", IconName::ArrowUpRight)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Go to Hash/Branch/Tag"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.search_state
                            .editor
                            .update(cx, |editor, cx| editor.focus_handle(cx).focus(window, cx));
                    })),
            )
            .when(!is_path_history, |this| {
                this.child(
                    IconButton::new(
                        "jb-toggle-branches",
                        if show_branches_pane {
                            IconName::ThreadsSidebarLeftOpen
                        } else {
                            IconName::ThreadsSidebarLeftClosed
                        },
                    )
                    .icon_size(IconSize::Small)
                    .toggle_state(show_branches_pane)
                    .tooltip(Tooltip::text("Toggle Branches Pane"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(jb) = &mut this.jetbrains {
                            jb.show_branches_pane = !jb.show_branches_pane;
                            cx.notify();
                        }
                    })),
                )
            })
            .child(
                IconButton::new("jb-columns-menu", IconName::Settings)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Columns"))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.deploy_header_context_menu(event.position(), window, cx);
                    })),
            )
            .into_any_element()
    }

    fn collect_loaded_authors(&self, cx: &mut App) -> Vec<SharedString> {
        let Some(repository) = self.get_repository(cx) else {
            return Vec::new();
        };
        let mut authors = IndexSet::default();
        let shas: Vec<Oid> = self
            .graph_data
            .commits
            .iter()
            .take(200)
            .map(|c| c.data.sha)
            .collect();
        repository.update(cx, |repo, cx| {
            for sha in shas {
                if let CommitDataState::Loaded(data) = repo.fetch_commit_data(sha, false, cx) {
                    if !data.author_name.is_empty() {
                        authors.insert(data.author_name.clone());
                    }
                }
            }
        });
        authors.into_iter().collect()
    }

    fn render_jetbrains_right_pane(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(selected_idx) = self.selected_entry_idx else {
            return v_flex()
                .id("jetbrains-details-placeholder")
                .size_full()
                .items_center()
                .justify_center()
                .bg(cx.theme().colors().editor_background)
                .child(Label::new("Select a commit to view details").color(Color::Muted))
                .into_any_element();
        };

        let Some(commit_entry) = self.graph_data.commits.get(selected_idx) else {
            return v_flex()
                .id("jetbrains-details-placeholder")
                .size_full()
                .items_center()
                .justify_center()
                .bg(cx.theme().colors().editor_background)
                .child(Label::new("Select a commit to view details").color(Color::Muted))
                .into_any_element();
        };

        let Some(repository) = self.get_repository(cx) else {
            return Empty.into_any_element();
        };

        let data = repository.update(cx, |repository, cx| {
            repository
                .fetch_commit_data(commit_entry.data.sha, false, cx)
                .clone()
        });

        let full_sha: SharedString = commit_entry.data.sha.to_string().into();
        let short_sha: SharedString = full_sha.chars().take(7).collect::<String>().into();
        let mut all_refs = commit_entry.data.ref_names.clone();
        if let Some((branches_sha, containing_branches)) = self.selected_commit_branches.as_ref()
            && *branches_sha == commit_entry.data.sha
        {
            for branch in containing_branches {
                let already_present = all_refs.iter().any(|existing| {
                    existing.as_ref() == branch.as_ref()
                        || Self::ref_name_from_decoration(existing.as_ref()).as_deref()
                            == Some(branch.as_ref())
                });
                if !already_present {
                    all_refs.push(branch.clone());
                }
            }
        }
        const MAX_VISIBLE_COMMIT_BRANCHES: usize = 8;
        let hidden_refs = if all_refs.len() > MAX_VISIBLE_COMMIT_BRANCHES {
            all_refs.split_off(MAX_VISIBLE_COMMIT_BRANCHES)
        } else {
            Vec::new()
        };
        let head_branch_name: Option<SharedString> = repository
            .read(cx)
            .branch
            .as_ref()
            .map(|branch| SharedString::from(branch.name().to_string()));
        let accent_colors = cx.theme().accents();
        let accent_color = accent_colors
            .0
            .get(commit_entry.color_idx)
            .copied()
            .unwrap_or_else(|| accent_colors.0.first().copied().unwrap_or_default());

        let (author_name, commit_timestamp) = match &data {
            CommitDataState::Loaded(data) => {
                (data.author_name.clone(), Some(data.commit_timestamp))
            }
            CommitDataState::Loading(_) => ("Loading…".into(), None),
        };
        let date_string = commit_timestamp
            .map(|timestamp| format_timestamp(timestamp, GitDateStyle::Absolute, cx))
            .unwrap_or_default();

        let remote = repository.update(cx, |repo, cx| {
            let remote_url = repo.default_remote_url()?;
            let provider_registry = GitHostingProviderRegistry::default_global(cx);
            let (provider, parsed) = parse_git_remote_url(provider_registry, &remote_url)?;
            Some(GitRemote {
                host: provider,
                owner: parsed.owner.into(),
                repo: parsed.repo.into(),
            })
        });

        let changed_files_count = self
            .selected_commit_diff
            .as_ref()
            .map(|diff| diff.files.len())
            .unwrap_or(0);
        let (total_lines_added, total_lines_removed) =
            self.selected_commit_diff_stats.unwrap_or((0, 0));

        let changed_file_entries: Vec<ChangedFileEntry> = self
            .selected_commit_diff
            .as_ref()
            .map(|diff| {
                let mut files = diff.files.iter().collect::<Vec<_>>();
                if !self.changed_files_view_mode.is_tree() {
                    files.sort_by_key(|file| file.status());
                }
                files
                    .into_iter()
                    .map(|file| ChangedFileEntry::from_commit_file(file, cx))
                    .collect()
            })
            .unwrap_or_default();
        let changed_file_entries = Rc::new(changed_file_entries);
        let is_tree_view = self.changed_files_view_mode.is_tree();
        let tree_entries: Rc<Vec<ChangedFileTreeEntry>> = if is_tree_view {
            Rc::new(build_changed_file_tree_entries(
                changed_file_entries.as_ref().clone(),
                &self.changed_files_expanded_dirs,
            ))
        } else {
            Rc::default()
        };

        let all_dir_paths: Vec<RepoPath> = tree_entries
            .iter()
            .filter_map(|entry| match entry {
                ChangedFileTreeEntry::Directory(dir) => Some(dir.path.clone()),
                ChangedFileTreeEntry::File(_) => None,
            })
            .collect();

        let copied_state: Entity<CopiedState> =
            window.use_keyed_state("jb-sha-copy", cx, CopiedState::new);
        let is_copied = copied_state.read(cx).is_copied();

        let message_expanded =
            !self.commit_details_message_collapsed && self.selected_commit_message.is_some();

        v_flex()
            .id("jetbrains-right-pane")
            .size_full()
            .min_w_0()
            .bg(cx.theme().colors().editor_background)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .gap_1()
                            .w_full()
                            .justify_between()
                            .border_b_1()
                            .border_color(cx.theme().colors().border)
                            .child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        Label::new(format!(
                                            "{} Changed {}",
                                            changed_files_count,
                                            if changed_files_count == 1 {
                                                "File"
                                            } else {
                                                "Files"
                                            }
                                        ))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                    )
                                    .child(
                                        IconButton::new(
                                            "jb-toggle-changed-files-view",
                                            IconName::ListTree,
                                        )
                                        .icon_size(IconSize::XSmall)
                                        .toggle_state(is_tree_view)
                                        .tooltip(Tooltip::text(if is_tree_view {
                                            "Show Flat View"
                                        } else {
                                            "Show Tree View"
                                        }))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.changed_files_view_mode =
                                                this.changed_files_view_mode.toggled();
                                            this.changed_files_scroll_handle
                                                .scroll_to_item(0, ScrollStrategy::Top);
                                            cx.notify();
                                        })),
                                    )
                                    .when(is_tree_view, |this| {
                                        let dirs_for_expand = all_dir_paths.clone();
                                        this.child(
                                            IconButton::new(
                                                "jb-expand-all-dirs",
                                                IconName::ChevronDown,
                                            )
                                            .icon_size(IconSize::XSmall)
                                            .tooltip(Tooltip::text("Expand All"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                for dir in &dirs_for_expand {
                                                    this.changed_files_expanded_dirs
                                                        .insert(dir.clone(), true);
                                                }
                                                cx.notify();
                                            })),
                                        )
                                        .child(
                                            IconButton::new(
                                                "jb-collapse-all-dirs",
                                                IconName::ChevronRight,
                                            )
                                            .icon_size(IconSize::XSmall)
                                            .tooltip(Tooltip::text("Collapse All"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.changed_files_expanded_dirs.clear();
                                                cx.notify();
                                            })),
                                        )
                                    })
                                    .child(
                                        IconButton::new(
                                            "jb-open-commit-diff",
                                            IconName::FileDiff,
                                        )
                                        .icon_size(IconSize::XSmall)
                                        .tooltip(Tooltip::text("Open Commit Diff"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_selected_commit_view(window, cx);
                                        })),
                                    ),
                            )
                            .child(DiffStat::new(
                                "jb-commit-diff-stat",
                                total_lines_added,
                                total_lines_removed,
                            )),
                    )
                    .child(
                        div()
                            .id("jb-changed-files-container")
                            .flex_1()
                            .min_h_0()
                            .child({
                                let flat_entries = changed_file_entries;
                                let entry_count = if is_tree_view {
                                    tree_entries.len()
                                } else {
                                    flat_entries.len()
                                };
                                let commit_sha = full_sha.clone();
                                let repository = repository.downgrade();
                                let workspace = self.workspace.clone();
                                let git_graph = cx.weak_entity();
                                uniform_list(
                                    "jb-changed-files-list",
                                    entry_count,
                                    move |range, _window, cx| {
                                        range
                                            .map(|ix| {
                                                if is_tree_view {
                                                    match &tree_entries[ix] {
                                                        ChangedFileTreeEntry::Directory(entry) => {
                                                            entry.render(ix, git_graph.clone(), cx)
                                                        }
                                                        ChangedFileTreeEntry::File(entry) => {
                                                            entry.entry.render(
                                                                ix,
                                                                entry.depth,
                                                                None,
                                                                commit_sha.clone(),
                                                                repository.clone(),
                                                                workspace.clone(),
                                                                cx,
                                                            )
                                                        }
                                                    }
                                                } else {
                                                    let directory_label = (!flat_entries[ix]
                                                        .dir_path
                                                        .is_empty())
                                                    .then(|| flat_entries[ix].dir_path.clone());
                                                    flat_entries[ix].render(
                                                        ix,
                                                        0,
                                                        directory_label,
                                                        commit_sha.clone(),
                                                        repository.clone(),
                                                        workspace.clone(),
                                                        cx,
                                                    )
                                                }
                                            })
                                            .collect()
                                    },
                                )
                                .track_scroll(&self.changed_files_scroll_handle)
                                .size_full()
                            }),
                    ),
            )
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .when(message_expanded, |this| this.flex_1().min_h_0())
                    .when(!message_expanded, |this| this.flex_none())
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.render_commit_message(true, window, cx))
                    .child(Divider::horizontal())
                    .child(
                        h_flex()
                            .flex_none()
                            .w_full()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .gap_1p5()
                            .flex_wrap()
                            .items_center()
                            .child({
                                let copy_sha = full_sha.clone();
                                Button::new("jb-sha-copy-btn", short_sha)
                                    .style(ButtonStyle::Subtle)
                                    .label_size(LabelSize::Small)
                                    .start_icon(
                                        Icon::new(if is_copied {
                                            IconName::Check
                                        } else {
                                            IconName::Hash
                                        })
                                        .size(IconSize::XSmall)
                                        .color(if is_copied {
                                            Color::Success
                                        } else {
                                            Color::Muted
                                        }),
                                    )
                                    .tooltip(Tooltip::text("Copy Commit SHA"))
                                    .on_click(move |_, _, cx| {
                                        copied_state.update(cx, |state, _| state.mark_copied());
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_sha.to_string(),
                                        ));
                                    })
                            })
                            .child(
                                Label::new(author_name)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .when(!date_string.is_empty(), |this| {
                                this.child(
                                    Label::new(date_string)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                            .children(all_refs.iter().map(|name| {
                                let is_head =
                                    Self::is_head_ref(name.as_ref(), &head_branch_name);
                                self.render_ref_chip(
                                    name.clone(),
                                    accent_color,
                                    is_head,
                                    selected_idx,
                                    cx.theme().colors().editor_background,
                                    cx,
                                )
                            }))
                            .when(!hidden_refs.is_empty(), |this| {
                                this.child(self.render_ref_count_chip(
                                    hidden_refs,
                                    accent_color,
                                    selected_idx,
                                    cx,
                                ))
                            })
                            .when_some(remote, |this, remote| {
                                let provider_name = remote.host.name();
                                let icon = ui::git_hosting_provider_icon(provider_name.as_str());
                                let parsed_remote = ParsedGitRemote {
                                    owner: remote.owner.as_ref().into(),
                                    repo: remote.repo.as_ref().into(),
                                };
                                let params = BuildCommitPermalinkParams {
                                    sha: full_sha.as_ref(),
                                };
                                let url = remote
                                    .host
                                    .build_commit_permalink(&parsed_remote, params)
                                    .to_string();
                                this.child(
                                    IconButton::new("jb-view-on-provider", icon)
                                        .icon_size(IconSize::XSmall)
                                        .tooltip(Tooltip::text(format!("View on {provider_name}")))
                                        .on_click(move |_, _, cx| {
                                            cx.open_url(&url);
                                        }),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_jetbrains(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_path_history = self.is_path_history();
        let show_branches = !is_path_history
            && self
                .jetbrains
                .as_ref()
                .is_some_and(|jb| jb.show_branches_pane);
        let is_client_filtered = self.is_jetbrains_client_filtered(cx);
        let visible_commit_count = self.jetbrains_visible_commit_count(cx);
        let (_, is_loading) = self.commit_count_and_loading_state(cx);
        let error = self.get_repository(cx).and_then(|repo| {
            repo.read(cx)
                .get_graph_data(self.log_source.clone(), self.log_order)
                .and_then(|data| data.error.clone())
        });

        let center_table = if visible_commit_count == 0 {
            let message = if let Some(error) = &error {
                format!("Error loading: {}", error)
            } else if is_loading {
                "Loading".to_string()
            } else {
                "No commits found".to_string()
            };
            h_flex()
                .size_full()
                .gap_1()
                .justify_center()
                .child(Label::new(message).color(Color::Muted).size(LabelSize::Default))
                .when(is_loading && error.is_none(), |this| {
                    this.child(self.render_loading_spinner(cx))
                })
                .into_any_element()
        } else {
            let header_resize_info =
                HeaderResizeInfo::from_redistributable(&self.column_widths, cx);
            let column_filter = self.column_visibility.clone();
            let graph_visible = !is_path_history && !is_client_filtered && self.is_graph_column_visible();
            let table_offset = self.table_column_offset();
            let table_filter = column_filter
                .as_slice()
                .get(table_offset..table_offset + TABLE_COLUMN_COUNT)
                .map(|slice| TableRow::from_vec(slice.to_vec(), TABLE_COLUMN_COUNT))
                .unwrap_or_else(|| TableRow::from_element(false, TABLE_COLUMN_COUNT));
            let header_widths = redistribute_hidden_widths(
                &self.column_widths.read(cx).widths_to_render(),
                Some(&column_filter),
            );
            let header_context = TableRenderContext::for_column_widths(Some(header_widths), true)
                .with_column_filter(Some(column_filter));

            let [_, graph_fraction, description_fraction, date_fraction, author_fraction, commit_fraction] =
                self.preview_column_fractions(window, cx);
            let table_fraction =
                description_fraction + date_fraction + author_fraction + commit_fraction;
            let table_width_config = self.table_column_width_config(window, cx);
            let table_collapsed = table_fraction <= f32::EPSILON;
            let graph_content_width = self.graph_canvas_content_width();

            let row_height = Self::row_height(window, cx);
            let selected_entry_idx = self.selected_entry_idx;
            let hovered_entry_idx = self.hovered_entry_idx;
            let context_menu_target_index = self
                .context_menu
                .as_ref()
                .and_then(|menu| menu.target_entry_index);
            let weak_self = cx.weak_entity();
            let focus_handle = self.focus_handle.clone();
            let table_focus_handle = self.table_interaction_state.read(cx).focus_handle.clone();
            let filtered_indices = self
                .jetbrains
                .as_ref()
                .and_then(|jb| jb.filtered_commit_indices.clone());

            let graph_canvas = div()
                .id("jb-graph-canvas")
                .size_full()
                .overflow_hidden()
                .child(div().size_full().child(self.render_graph_canvas(window, cx)));

            let commits_table = Table::new(4)
                .interactable(&self.table_interaction_state)
                .hide_row_borders()
                .hide_row_hover()
                .width_config(table_width_config)
                .column_filter(table_filter)
                .map_row(move |(row_idx, row), window, cx| {
                    let index = filtered_indices
                        .as_ref()
                        .and_then(|f| f.get(row_idx).copied())
                        .unwrap_or(row_idx);
                    let is_selected = selected_entry_idx == Some(index);
                    let is_hovered = hovered_entry_idx == Some(index);
                    let is_context_menu_target = context_menu_target_index == Some(index);
                    let table_focus_handle = table_focus_handle.clone();
                    let is_focused =
                        focus_handle.is_focused(window) || table_focus_handle.is_focused(window);
                    let weak = weak_self.clone();
                    let weak_for_hover = weak.clone();
                    let weak_for_context_menu = weak.clone();

                    let hover_bg = cx.theme().colors().element_hover.opacity(0.6);
                    let selected_bg = if is_focused {
                        cx.theme().colors().element_selected
                    } else {
                        cx.theme().colors().element_hover
                    };

                    row.h(row_height)
                        .cursor_pointer()
                        .when(is_selected || is_context_menu_target, |row| {
                            row.bg(selected_bg)
                        })
                        .when(
                            is_hovered && !is_selected && !is_context_menu_target,
                            |row| row.bg(hover_bg),
                        )
                        .on_hover(move |&is_hovered, _, cx| {
                            weak_for_hover
                                .update(cx, |this, cx| {
                                    if is_hovered {
                                        if this.hovered_entry_idx != Some(index) {
                                            this.hovered_entry_idx = Some(index);
                                            cx.notify();
                                        }
                                    } else if this.hovered_entry_idx == Some(index) {
                                        this.hovered_entry_idx = None;
                                        cx.notify();
                                    }
                                })
                                .ok();
                        })
                        .on_click(move |event, window, cx| {
                            weak.update(cx, |this, cx| {
                                this.handle_entry_click(
                                    index,
                                    event,
                                    ScrollStrategy::Center,
                                    Some(&table_focus_handle),
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                        })
                        .on_mouse_down(
                            MouseButton::Right,
                            move |event: &MouseDownEvent, window, cx| {
                                weak_for_context_menu
                                    .update(cx, |this, cx| {
                                        this.handle_entry_secondary_mouse_down(
                                            index, event, window, cx,
                                        );
                                    })
                                    .ok();
                            },
                        )
                        .into_any_element()
                })
                .uniform_list(
                    "jb-git-graph-commits",
                    visible_commit_count,
                    cx.processor(Self::render_table_rows),
                );

            v_flex()
                .flex_1()
                .min_w_0()
                .size_full()
                .child(
                    div()
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                this.deploy_header_context_menu(event.position, window, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .child(render_table_header(
                            if !is_path_history {
                                TableRow::from_vec(
                                    vec![
                                        Label::new("Refs").color(Color::Muted).into_any_element(),
                                        Label::new("Graph").color(Color::Muted).into_any_element(),
                                        Label::new("Description")
                                            .color(Color::Muted)
                                            .into_any_element(),
                                        Label::new("Date").color(Color::Muted).into_any_element(),
                                        Label::new("Author").color(Color::Muted).into_any_element(),
                                        Label::new("Commit").color(Color::Muted).into_any_element(),
                                    ],
                                    GRAPH_LAYOUT_TABLE_OFFSET + TABLE_COLUMN_COUNT,
                                )
                            } else {
                                TableRow::from_vec(
                                    vec![
                                        Label::new("Description")
                                            .color(Color::Muted)
                                            .into_any_element(),
                                        Label::new("Date").color(Color::Muted).into_any_element(),
                                        Label::new("Author").color(Color::Muted).into_any_element(),
                                        Label::new("Commit").color(Color::Muted).into_any_element(),
                                    ],
                                    4,
                                )
                            },
                            header_context,
                            Some(header_resize_info),
                            Some(self.column_widths.entity_id()),
                            cx,
                        )),
                )
                .child(bind_redistributable_columns(
                    div()
                        .relative()
                        .flex_1()
                        .w_full()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .size_full()
                                .when(graph_visible, |this| {
                                    this.child(
                                        self.with_graph_row_interactions(
                                            div().id("jb-graph-column"),
                                            cx,
                                        )
                                        .map(|this| {
                                            if table_collapsed {
                                                this.w(graph_content_width)
                                            } else {
                                                this.w(DefiniteLength::Fraction(graph_fraction))
                                            }
                                        })
                                        .relative()
                                        .h_full()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(graph_canvas),
                                    )
                                })
                                .child(
                                    div()
                                        .map(|this| {
                                            if table_collapsed {
                                                this.flex_1()
                                            } else {
                                                this.w(DefiniteLength::Fraction(table_fraction))
                                            }
                                        })
                                        .h_full()
                                        .min_w_0()
                                        .child(commits_table),
                                ),
                        )
                        .child(render_redistributable_columns_resize_handles(
                            &self.column_widths,
                            Some(&self.column_visibility),
                            window,
                            cx,
                        )),
                    self.column_widths.clone(),
                    Some(self.column_visibility.clone()),
                ))
                .into_any_element()
        };

        let branch_context_menu = self
            .jetbrains
            .as_ref()
            .and_then(|jb| jb.branch_context_menu.as_ref())
            .map(|menu| (menu.position, menu.menu.clone()));

        let right_ratio = self
            .jetbrains
            .as_ref()
            .map(|jb| jb.details_split_state.read(cx).right_ratio())
            .unwrap_or(1.0 - DEFAULT_DETAILS_PANE_RATIO);

        div()
            .key_context("GitGraph")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .on_action(cx.listener(|this, _: &OpenCommitView, window, cx| {
                this.open_selected_commit_view(window, cx);
            }))
            .on_action(cx.listener(Self::copy_selected_commit_sha))
            .on_action(cx.listener(Self::copy_selected_commit_tag))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search_state
                    .editor
                    .update(cx, |editor, cx| editor.focus_handle(cx).focus(window, cx));
            }))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::toggle_changed_files_view))
            .child(
                h_flex()
                    .size_full()
                    .when(show_branches, |this| {
                        this.child(self.render_jetbrains_branches_pane(cx))
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_jetbrains_toolbar(cx))
                            .child(div().flex_1().min_h_0().w_full().child(center_table)),
                    )
                    .child(
                        div()
                            .h_full()
                            .w(px(1.))
                            .bg(cx.theme().colors().border),
                    )
                    .child(
                        div()
                            .h_full()
                            .min_w(px(260.))
                            .flex_basis(DefiniteLength::Fraction(right_ratio))
                            .child(self.render_jetbrains_right_pane(window, cx)),
                    ),
            )
            .children(self.context_menu.as_ref().map(|context_menu| {
                deferred(
                    anchored()
                        .position(context_menu.position)
                        .anchor(Anchor::TopLeft)
                        .child(context_menu.menu.clone()),
                )
                .with_priority(1)
            }))
            .children(branch_context_menu.map(|(position, menu)| {
                deferred(
                    anchored()
                        .position(position)
                        .anchor(Anchor::TopLeft)
                        .child(menu),
                )
                .with_priority(1)
            }))
            .into_any_element()
    }
}

fn make_askpass_delegate(
    operation: SharedString,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> AskPassDelegate {
    let window_handle = window.window_handle();
    AskPassDelegate::new_with_cancellation(&mut cx.to_async(), move |prompt, tx, cancellation, cx| {
        window_handle
            .update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.toggle_modal(window, cx, |window, cx| {
                            AskPassModal::new(
                                operation.clone(),
                                prompt.into(),
                                tx,
                                cancellation,
                                window,
                                cx,
                            )
                        });
                    })
                    .ok();
            })
            .ok();
    })
}

fn pull_branch_into_current(
    repo: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    branch: Option<SharedString>,
    remote: SharedString,
    rebase: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let operation: SharedString = if rebase {
        format!("git pull --rebase {}", remote).into()
    } else {
        format!("git pull {}", remote).into()
    };
    let askpass = make_askpass_delegate(operation, workspace, window, cx);
    cx.spawn(async move |cx| {
        match repo
            .update(cx, |repo, cx| repo.pull(branch, remote, rebase, askpass, cx))
            .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("Operation was canceled")),
        }
    })
    .detach_and_prompt_err("Failed to pull", window, cx, |_, _, _| None);
}

fn push_local_branch(
    repo: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    branch_name: SharedString,
    upstream: Option<Upstream>,
    force: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let options = if force {
        Some(PushOptions::Force)
    } else {
        match upstream.as_ref() {
            Some(Upstream {
                tracking: UpstreamTracking::Gone,
                ..
            })
            | None => Some(PushOptions::SetUpstream),
            _ => None,
        }
    };
    let configured_remote = upstream
        .as_ref()
        .and_then(|u| u.remote_name().map(SharedString::new));
    let remote_branch: SharedString = upstream
        .as_ref()
        .filter(|u| matches!(u.tracking, UpstreamTracking::Tracked(_)))
        .and_then(|u| u.branch_name())
        .map(SharedString::new)
        .unwrap_or_else(|| branch_name.clone());
    let askpass_label: SharedString = match &configured_remote {
        Some(remote) => format!("git push {}", remote).into(),
        None => format!("git push {}", branch_name).into(),
    };
    let askpass = make_askpass_delegate(askpass_label, workspace, window, cx);

    cx.spawn(async move |cx| {
        let remote = match configured_remote {
            Some(remote) => remote,
            None => {
                let branch = branch_name.to_string();
                let remotes = repo
                    .update(cx, |repo, _| repo.get_remotes(Some(branch), true))
                    .await??;
                remotes
                    .into_iter()
                    .next()
                    .map(|remote| remote.name)
                    .ok_or_else(|| anyhow!("No remote available to push to"))?
            }
        };

        match repo
            .update(cx, |repo, cx| {
                repo.push(branch_name, remote_branch, remote, options, askpass, cx)
            })
            .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("Operation was canceled")),
        }
    })
    .detach_and_prompt_err("Failed to push", window, cx, |_, _, _| None);
}

fn fetch_remote(
    repo: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    remote_name: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let (fetch_options, operation): (FetchOptions, SharedString) = match remote_name {
        Some(name) => (
            FetchOptions::Remote(Remote {
                name: name.clone().into(),
            }),
            format!("git fetch {}", name).into(),
        ),
        None => (FetchOptions::All, "git fetch --all".into()),
    };
    let askpass = make_askpass_delegate(operation, workspace, window, cx);
    cx.spawn(async move |cx| {
        match repo
            .update(cx, |repo, cx| repo.fetch(fetch_options, Some(askpass), cx))
            .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("Operation was canceled")),
        }
    })
    .detach_and_prompt_err("Failed to fetch", window, cx, |_, _, _| None);
}

fn render_tracking_status_indicator(tracking: UpstreamTrackingStatus) -> Option<AnyElement> {
    if tracking.ahead == 0 && tracking.behind == 0 {
        return None;
    }
    Some(
        h_flex()
            .gap_1()
            .flex_shrink_0()
            .when(tracking.behind > 0, |this| {
                this.child(
                    h_flex()
                        .gap_0p5()
                        .child(
                            Icon::new(IconName::ArrowDown)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(tracking.behind.to_string())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
            })
            .when(tracking.ahead > 0, |this| {
                this.child(
                    h_flex()
                        .gap_0p5()
                        .child(
                            Icon::new(IconName::ArrowUp)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(tracking.ahead.to_string())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use git::repository::CommitData;
    use gpui::{TestAppContext, UpdateGlobal};
    use project::Project;
    use serde_json::json;
    use settings::{GitPanelLayout, SettingsStore};
    use std::path::Path;
    use util::path;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            language_model::init(cx);
            crate::init(cx);
        });
    }

    fn set_layout(layout: GitPanelLayout, cx: &mut TestAppContext) {
        let layout_str = match layout {
            GitPanelLayout::Zed => "zed",
            GitPanelLayout::Jetbrains => "jetbrains",
        };
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(&format!(r#"{{"git_panel": {{"layout": "{layout_str}"}}}}"#), cx)
                    .unwrap();
            });
        });
    }

    #[gpui::test]
    async fn test_jetbrains_git_dock_visibility_by_layout_setting(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(path!("/project"), json!({ ".git": {}, "a.txt": "hello" }))
            .await;
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let dock = workspace.read_with(cx, |workspace, cx| {
            workspace
                .panel::<JetBrainsGitDock>(cx)
                .expect("JetBrainsGitDock should be registered")
        });

        cx.update(|window, cx| {
            let dock_ref = dock.read(cx);
            assert_eq!(dock_ref.position(window, cx), DockPosition::Bottom);
            assert!(dock_ref.position_is_valid(DockPosition::Bottom));
            assert!(!dock_ref.position_is_valid(DockPosition::Left));
            assert_eq!(dock_ref.default_size(window, cx), px(320.));
            assert_eq!(dock_ref.icon_tooltip(window, cx), Some("Git"));
            // Default layout is Zed, so icon should be hidden.
            assert_eq!(dock_ref.icon(window, cx), None);
            assert_eq!(
                workspace::WorkspaceSettings::get_global(cx).bottom_dock_layout,
                settings::BottomDockLayout::Contained
            );
        });

        set_layout(GitPanelLayout::Jetbrains, cx);
        cx.update(|window, cx| {
            assert_eq!(dock.read(cx).icon(window, cx), Some(IconName::GitGraph));
            assert_eq!(
                workspace::WorkspaceSettings::get_global(cx).bottom_dock_layout,
                settings::BottomDockLayout::Full
            );
        });

        set_layout(GitPanelLayout::Zed, cx);
        cx.update(|window, cx| {
            assert_eq!(dock.read(cx).icon(window, cx), None);
            assert_eq!(
                workspace::WorkspaceSettings::get_global(cx).bottom_dock_layout,
                settings::BottomDockLayout::Contained
            );
        });
    }

    #[gpui::test]
    async fn test_jetbrains_open_and_file_history_routing(cx: &mut TestAppContext) {
        init_test(cx);
        set_layout(GitPanelLayout::Jetbrains, cx);

        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(path!("/project"), json!({ ".git": {}, "file.rs": "fn main() {}" }))
            .await;
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let git_store = project.read_with(cx, |p, _| p.git_store().clone());
        let repo = git_store
            .read_with(cx, |store, _| store.active_repository())
            .expect("active repository");
        let repo_id = repo.read_with(cx, |r, _| r.id);

        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                super::open_or_reuse_graph(
                    workspace,
                    repo_id,
                    git_store.clone(),
                    LogSource::All,
                    None,
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        let dock = workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items_of_type::<GitGraph>(cx).count(), 0);
            workspace.panel::<JetBrainsGitDock>(cx).expect("dock")
        });
        dock.read_with(cx, |dock, _| {
            assert_eq!(dock.active_tab, DockTab::Log);
            assert!(dock.history_tabs.is_empty());
        });

        // Opening FileHistory should open a closable History tab in JetBrainsGitDock.
        let repo_path = RepoPath::new("file.rs").expect("valid repo path");
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                super::open_or_reuse_graph(
                    workspace,
                    repo_id,
                    git_store.clone(),
                    LogSource::Path(repo_path.clone()),
                    None,
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        let tab_id = dock.read_with(cx, |dock, _| {
            assert_eq!(dock.history_tabs.len(), 1);
            assert_eq!(dock.history_tabs[0].title, "History: file.rs");
            assert_eq!(dock.active_tab, DockTab::History(dock.history_tabs[0].id));
            dock.history_tabs[0].id
        });

        // Closing the History tab returns to Log.
        dock.update(cx, |dock, cx| {
            dock.close_history_tab(tab_id, cx);
        });
        dock.read_with(cx, |dock, _| {
            assert!(dock.history_tabs.is_empty());
            assert_eq!(dock.active_tab, DockTab::Log);
        });

        // Clicking the hide button collapses the bottom dock.
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.bottom_dock().read(cx).is_open());
        });
        cx.refresh().unwrap();
        let hide_bounds = cx
            .debug_bounds("ICON-Dash")
            .expect("hide button should be rendered");
        cx.simulate_click(hide_bounds.center(), gpui::Modifiers::none());
        workspace.read_with(cx, |workspace, cx| {
            assert!(!workspace.bottom_dock().read(cx).is_open());
        });
    }

    #[gpui::test]
    async fn test_jetbrains_filtering_and_auto_select(cx: &mut TestAppContext) {
        init_test(cx);
        set_layout(GitPanelLayout::Jetbrains, cx);

        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(path!("/project"), json!({ ".git": {}, "a.txt": "hello" }))
            .await;
        let project = Project::test(fs.clone(), [Path::new(path!("/project"))], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let git_store = project.read_with(cx, |p, _| p.git_store().clone());
        let repo = git_store
            .read_with(cx, |store, _| store.active_repository())
            .expect("active repository");
        let repo_id = repo.read_with(cx, |r, _| r.id);

        let sha1: Oid = "1111111111111111111111111111111111111111"
            .parse()
            .expect("valid oid");
        let sha2: Oid = "2222222222222222222222222222222222222222"
            .parse()
            .expect("valid oid");

        let now = OffsetDateTime::now_utc().unix_timestamp();
        let old = now - TimeDuration::days(60).whole_seconds();

        fs.set_commit_data(
            Path::new(path!("/project/.git")),
            [
                (
                    CommitData {
                        sha: sha1,
                        parents: SmallVec::new(),
                        subject: "feat: add jetbrains dock".into(),
                        message: "feat: add jetbrains dock".into(),
                        author_name: "Alice".into(),
                        author_email: "alice@example.com".into(),
                        commit_timestamp: now,
                    },
                    false,
                ),
                (
                    CommitData {
                        sha: sha2,
                        parents: SmallVec::new(),
                        subject: "fix: old bug".into(),
                        message: "fix: old bug".into(),
                        author_name: "Bob".into(),
                        author_email: "bob@example.com".into(),
                        commit_timestamp: old,
                    },
                    false,
                ),
            ],
        );

        let graph = cx.update(|window, cx| {
            cx.new(|cx| {
                let mut graph = GitGraph::new_jetbrains(
                    repo_id,
                    git_store.clone(),
                    workspace.downgrade(),
                    Some(LogSource::All),
                    true,
                    window,
                    cx,
                );
                graph.graph_data.add_commits(&[
                    Arc::new(InitialGraphCommitData {
                        sha: sha1,
                        parents: SmallVec::new(),
                        ref_names: vec!["main".into()],
                    }),
                    Arc::new(InitialGraphCommitData {
                        sha: sha2,
                        parents: SmallVec::new(),
                        ref_names: Vec::new(),
                    }),
                ]);
                graph
            })
        });

        // Pre-populate commit_data on repo so filtering & auto-select can inspect them synchronously.
        repo.update(cx, |repo, cx| {
            repo.fetch_commit_data(sha1, false, cx);
            repo.fetch_commit_data(sha2, false, cx);
        });
        cx.run_until_parked();

        graph.update(cx, |graph, cx| {
            graph.on_jetbrains_commits_updated(cx);
            assert_eq!(graph.selected_entry_idx, Some(0));
            assert_eq!(graph.jetbrains_visible_commit_count(cx), 2);

            // Filter by User
            if let Some(jb) = &mut graph.jetbrains {
                jb.user_filter = UserFilter::Author("Alice".into());
            }
            graph.recompute_jetbrains_filter(cx);
            assert_eq!(graph.jetbrains_visible_commit_count(cx), 1);
            assert_eq!(graph.jetbrains_commit_index(0), 0);

            // Filter by Date (Last 7 Days)
            if let Some(jb) = &mut graph.jetbrains {
                jb.user_filter = UserFilter::All;
                jb.date_filter = DateFilter::Last7Days;
            }
            graph.recompute_jetbrains_filter(cx);
            assert_eq!(graph.jetbrains_visible_commit_count(cx), 1);
            assert_eq!(graph.jetbrains_commit_index(0), 0);
        });

        // Filter by search query
        cx.update(|window, cx| {
            graph.update(cx, |graph, cx| {
                if let Some(jb) = &mut graph.jetbrains {
                    jb.date_filter = DateFilter::All;
                }
                graph.search_state.editor.update(cx, |editor, cx| {
                    editor.set_text("old bug", window, cx);
                });
                graph.recompute_jetbrains_filter(cx);
                assert_eq!(graph.jetbrains_visible_commit_count(cx), 1);
                assert_eq!(graph.jetbrains_commit_index(0), 1);
            });
        });
    }

    #[gpui::test]
    async fn test_jetbrains_branch_context_menu_and_tracking_indicator(cx: &mut TestAppContext) {
        init_test(cx);
        set_layout(GitPanelLayout::Jetbrains, cx);

        assert!(
            render_tracking_status_indicator(UpstreamTrackingStatus {
                ahead: 0,
                behind: 0,
            })
            .is_none()
        );
        assert!(
            render_tracking_status_indicator(UpstreamTrackingStatus {
                ahead: 2,
                behind: 3,
            })
            .is_some()
        );

        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(path!("/project"), json!({ ".git": {}, "a.txt": "hello" }))
            .await;
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let git_store = project.read_with(cx, |p, _| p.git_store().clone());
        let repo = git_store
            .read_with(cx, |store, _| store.active_repository())
            .expect("active repository");
        let repo_id = repo.read_with(cx, |r, _| r.id);

        let graph = cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                cx.new(|cx| {
                    let mut graph = GitGraph::new_jetbrains(
                        repo_id,
                        git_store,
                        workspace.weak_handle(),
                        None,
                        true,
                        window,
                        cx,
                    );
                    if let Some(jb) = &mut graph.jetbrains {
                        jb.branches = vec![
                            Branch {
                                is_head: true,
                                ref_name: "refs/heads/main".into(),
                                upstream: Some(git::repository::Upstream {
                                    ref_name: "refs/remotes/origin/main".into(),
                                    tracking: git::repository::UpstreamTracking::Tracked(
                                        UpstreamTrackingStatus {
                                            ahead: 1,
                                            behind: 2,
                                        },
                                    ),
                                }),
                                most_recent_commit: None,
                            },
                            Branch {
                                is_head: false,
                                ref_name: "refs/heads/feature".into(),
                                upstream: Some(git::repository::Upstream {
                                    ref_name: "refs/remotes/origin/feature".into(),
                                    tracking: git::repository::UpstreamTracking::Tracked(
                                        UpstreamTrackingStatus {
                                            ahead: 3,
                                            behind: 1,
                                        },
                                    ),
                                }),
                                most_recent_commit: None,
                            },
                            Branch {
                                is_head: false,
                                ref_name: "refs/remotes/origin/other".into(),
                                upstream: None,
                                most_recent_commit: None,
                            },
                        ];
                    }
                    graph
                })
            })
        });

        cx.update(|window, cx| {
            graph.update(cx, |graph, cx| {
                graph.deploy_jetbrains_branch_context_menu(
                    Point::default(),
                    "origin/other".into(),
                    true,
                    false,
                    window,
                    cx,
                );
                assert!(
                    graph
                        .jetbrains
                        .as_ref()
                        .and_then(|jb| jb.branch_context_menu.as_ref())
                        .is_some()
                );

                graph.deploy_jetbrains_branch_context_menu(
                    Point::default(),
                    "feature".into(),
                    true,
                    true,
                    window,
                    cx,
                );
                assert!(
                    graph
                        .jetbrains
                        .as_ref()
                        .and_then(|jb| jb.branch_context_menu.as_ref())
                        .is_some()
                );

                graph.deploy_jetbrains_branch_context_menu(
                    Point::default(),
                    "main".into(),
                    true,
                    true,
                    window,
                    cx,
                );
                assert!(
                    graph
                        .jetbrains
                        .as_ref()
                        .and_then(|jb| jb.branch_context_menu.as_ref())
                        .is_some()
                );

                graph.deploy_jetbrains_remote_context_menu(
                    Point::default(),
                    Some("origin".to_string()),
                    window,
                    cx,
                );
                assert!(
                    graph
                        .jetbrains
                        .as_ref()
                        .and_then(|jb| jb.branch_context_menu.as_ref())
                        .is_some()
                );

                graph.deploy_jetbrains_remote_context_menu(
                    Point::default(),
                    None,
                    window,
                    cx,
                );
                assert!(
                    graph
                        .jetbrains
                        .as_ref()
                        .and_then(|jb| jb.branch_context_menu.as_ref())
                        .is_some()
                );
            });
        });
    }
}
