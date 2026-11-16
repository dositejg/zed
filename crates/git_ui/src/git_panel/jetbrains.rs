use super::*;
use crate::stash_picker::{self, StashList};

const MAX_RECENT_COMMIT_MESSAGES_SCANNED: usize = 100;
const MAX_RECENT_COMMIT_MESSAGES_SHOWN: usize = 20;
// Aligns child rows under the parent header's label (past 12px chevron + 6px gap + 16px checkbox + 6px gap).
pub(super) const JETBRAINS_CHILD_INDENT: f32 = 40.0;
// Aligns nested tree rows under their parent directory's checkbox (past 14px chevron + 6px gap).
pub(super) const JETBRAINS_TREE_INDENT: f32 = 20.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecentCommitMessage {
    pub(super) subject: SharedString,
    pub(super) message: String,
}

#[derive(Default)]
pub(super) struct JetBrainsState {
    pub(super) stash_list: Option<Entity<StashList>>,
    pub(super) group_by_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(super) commit_history_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(super) commit_options_menu_handle: PopoverMenuHandle<ContextMenu>,
    pub(super) push_after_commit: bool,
    pub(super) recent_commit_messages: Option<Vec<RecentCommitMessage>>,
    pub(super) recent_commit_messages_task: Option<Task<()>>,
    #[cfg(test)]
    pub(super) push_triggered_count: usize,
    #[cfg(test)]
    pub(super) skip_actual_push_in_tests: bool,
}

pub(super) fn messages_by_author<'a>(
    details: impl IntoIterator<Item = &'a CommitDetails>,
    author_email: Option<&str>,
) -> Vec<RecentCommitMessage> {
    let Some(author_email) = author_email
        .map(str::trim)
        .filter(|email| !email.is_empty())
    else {
        return Vec::new();
    };

    details
        .into_iter()
        .filter(|commit| commit.author_email.trim().eq_ignore_ascii_case(author_email))
        .filter_map(|commit| {
            let message = commit.message.trim_end().to_string();
            let subject = message
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())?
                .to_string();
            Some(RecentCommitMessage {
                subject: subject.into(),
                message,
            })
        })
        .take(MAX_RECENT_COMMIT_MESSAGES_SHOWN)
        .collect()
}

impl GitPanel {
    pub(super) fn is_jetbrains_layout(&self, cx: &App) -> bool {
        GitPanelSettings::get_global(cx).layout == settings::GitPanelLayout::Jetbrains
    }

    pub(super) fn ensure_jetbrains_stash_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.jetbrains.stash_list.is_some() {
            return;
        }
        let repository = self.active_repository.clone();
        let workspace = self.workspace.clone();
        self.jetbrains.stash_list = Some(cx.new(|cx| {
            stash_picker::create_embedded(repository, workspace, rems(20.), true, window, cx)
        }));
    }

    pub(super) fn render_jetbrains_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_entries = !self.entries.is_empty();
        let has_write_access = self.has_write_access(cx);

        v_flex()
            .size_full()
            .when(!self.commit_editor_expanded, |this| {
                this.child(self.render_jetbrains_tab_bar(cx))
                    .children(self.render_jetbrains_repo_bar(window, cx))
            })
            .map(|this| match self.active_tab {
                GitPanelTab::Changes | GitPanelTab::History => this
                    .when(!self.commit_editor_expanded, |this| {
                        this.children(self.render_jetbrains_toolbar(window, cx))
                            .map(|this| {
                                if let Some(repo) = self.active_repository.clone()
                                    && has_entries
                                {
                                    this.child(self.render_entries(
                                        has_write_access,
                                        repo,
                                        window,
                                        cx,
                                    ))
                                } else {
                                    this.child(self.render_empty_state(cx).into_any_element())
                                }
                            })
                    })
                    .children(self.render_jetbrains_footer(window, cx)),
                GitPanelTab::Stash => this.child(self.render_jetbrains_stash_tab(window, cx)),
            })
    }

    pub(super) fn render_jetbrains_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_tab = match self.active_tab {
            GitPanelTab::History => GitPanelTab::Changes,
            other => other,
        };
        let changes_count = if self.is_branch_mode() {
            self.section_file_counts
                .get(&Section::Branch)
                .copied()
                .unwrap_or_default()
        } else {
            self.changes_count
        };
        let stash_count = self.stash_entries.entries.len();
        let focus_handle = self.focus_handle.clone();

        let render_tab = |id: &'static str,
                          tab: GitPanelTab,
                          label: &'static str,
                          count: usize,
                          tooltip_action: Option<Box<dyn Action>>,
                          plain_tooltip: Option<&'static str>| {
            let active = active_tab == tab;
            let focus_handle = focus_handle.clone();

            h_flex()
                .cursor_pointer()
                .id(id)
                .debug_selector(move || id.to_string())
                .h_full()
                .py_1()
                .gap_1()
                .flex_1()
                .justify_center()
                .hover(|s| s.bg(cx.theme().colors().element_hover))
                .border_b_1()
                .when(!active, |s| {
                    s.bg(cx.theme().colors().editor_background.opacity(0.6))
                        .border_color(cx.theme().colors().border.opacity(0.6))
                })
                .child(Label::new(label).when(!active, |this| this.color(Color::Muted)))
                .when(count > 0, |this| {
                    this.child(
                        Label::new(format!("({count})"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
                .tooltip(move |_window, cx| {
                    if let Some(action) = tooltip_action.as_ref() {
                        Tooltip::for_action_in(
                            format!("Toggle {label} Tab"),
                            action.as_ref(),
                            &focus_handle,
                            cx,
                        )
                    } else {
                        Tooltip::simple(plain_tooltip.unwrap_or(label), cx)
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_active_tab(tab, window, cx)
                }))
        };

        h_flex()
            .relative()
            .h(Tab::container_height(cx))
            .w_full()
            .child(render_tab(
                "jetbrains-commit-tab",
                GitPanelTab::Changes,
                "Commit",
                changes_count,
                Some(ActivateChangesTab.boxed_clone()),
                None,
            ))
            .child(
                Divider::vertical()
                    .color(ui::DividerColor::BorderFaded)
                    .h_full(),
            )
            .child(render_tab(
                "jetbrains-stash-tab",
                GitPanelTab::Stash,
                "Stash",
                stash_count,
                None,
                Some("Show Stashes"),
            ))
    }

    pub(super) fn render_jetbrains_repo_bar(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let active_repository = self.active_repository.as_ref()?;
        let project = self.project.clone();
        let single_repo = project.read(cx).git_store().read(cx).repositories().len() <= 1;
        let active_repo_name = repository_display_name(Some(active_repository), &project, cx);

        let repo_read = active_repository.read(cx);
        let branch = repo_read.branch.clone();
        let head_commit = repo_read.head_commit.clone();

        const MAX_SHORT_SHA_LEN: usize = 8;
        let branch_name = branch
            .as_ref()
            .map(|branch| branch.name().to_owned())
            .or_else(|| {
                head_commit.as_ref().map(|commit| {
                    commit
                        .sha
                        .chars()
                        .take(MAX_SHORT_SHA_LEN)
                        .collect::<String>()
                })
            })
            .unwrap_or_else(|| " (no branch)".to_owned());
        let show_separator = branch.is_some() || head_commit.is_some();

        let repo_selector = PopoverMenu::new("jetbrains-repository-switcher")
            .menu(move |window, cx| {
                Some(cx.new(|cx| RepositorySelector::new(project.clone(), rems(20.), window, cx)))
            })
            .trigger_with_tooltip(
                Button::new("jetbrains-repo-selector", active_repo_name)
                    .size(ButtonSize::None)
                    .label_size(LabelSize::Small)
                    .truncate(true),
                move |_, cx| {
                    if single_repo {
                        cx.new(|_| Empty).into()
                    } else {
                        Tooltip::simple("Switch Active Repository", cx)
                    }
                },
            )
            .anchor(Anchor::TopLeft)
            .offset(gpui::Point {
                x: px(0.0),
                y: px(2.0),
            });

        let workspace = self.workspace.clone();
        let repo_for_branch = Some(active_repository.clone());
        let branch_selector_button = Button::new("jetbrains-branch-selector", branch_name)
            .size(ButtonSize::None)
            .label_size(LabelSize::Small)
            .truncate(true)
            .on_click(|_, window, cx| {
                window.dispatch_action(zed_actions::git::Switch.boxed_clone(), cx);
            });

        let branch_selector = PopoverMenu::new("jetbrains-branch-popover")
            .menu(move |window, cx| {
                Some(branch_picker::popover(
                    workspace.clone(),
                    false,
                    repo_for_branch.clone(),
                    window,
                    cx,
                ))
            })
            .trigger_with_tooltip(
                branch_selector_button,
                Tooltip::for_action_title("Switch Branch", &zed_actions::git::Switch),
            )
            .anchor(Anchor::TopLeft)
            .offset(gpui::Point {
                x: px(0.0),
                y: px(2.0),
            });

        Some(
            h_flex()
                .w_full()
                .px_2()
                .py_1()
                .justify_between()
                .gap_1()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    h_flex()
                        .flex_1()
                        .overflow_hidden()
                        .gap_1()
                        .child(
                            Icon::new(IconName::GitBranch)
                                .size(IconSize::Small)
                                .color(if single_repo {
                                    Color::Disabled
                                } else {
                                    Color::Muted
                                }),
                        )
                        .when(!single_repo, |this| {
                            this.child(div().child(repo_selector).min_w_0()).when(
                                show_separator,
                                |this| {
                                    this.child(Label::new("/").size(LabelSize::Small).color(
                                        Color::Custom(cx.theme().colors().text_muted.opacity(0.4)),
                                    ))
                                },
                            )
                        })
                        .child(div().child(branch_selector).min_w_0()),
                )
                .children(self.render_remote_button(cx)),
        )
    }

    pub(super) fn render_jetbrains_toolbar(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if matches!(self.git_access, Some(GitAccess::No)) {
            return None;
        }
        self.active_repository.as_ref()?;

        let is_branch_mode = self.is_branch_mode();
        let has_write_access = self.has_write_access(cx);
        let is_read_only = self.project.read(cx).is_read_only(cx);
        let has_selection = self.selected_entry.is_some() || self.has_marks();
        let can_rollback = has_selection && !is_branch_mode && has_write_access && !is_read_only;
        let is_tree_mode = self.view_mode.tree_state().is_some();

        let focus_handle = self.focus_handle.clone();
        let has_tracked_changes = self.has_tracked_changes();
        let has_staged_changes = self.has_staged_changes();
        let has_new_changes = self.new_count > 0;
        let has_stash_items = !self.stash_entries.entries.is_empty();

        let stash_menu = PopoverMenu::new("jetbrains-stash-popover")
            .trigger_with_tooltip(
                IconButton::new("jetbrains-stash-menu-trigger", IconName::Archive)
                    .icon_size(IconSize::Small)
                    .disabled(!has_write_access || is_read_only),
                Tooltip::text("Stash"),
            )
            .menu({
                let focus_handle = focus_handle.clone();
                move |window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _, _| {
                        menu.context(focus_handle.clone())
                            .action_disabled_when(
                                !(has_new_changes || has_tracked_changes),
                                "Stash All",
                                StashAll.boxed_clone(),
                            )
                            .action_disabled_when(
                                !has_tracked_changes,
                                "Stash Tracked",
                                StashTracked.boxed_clone(),
                            )
                            .action_disabled_when(
                                !has_staged_changes,
                                "Stash Staged",
                                StashStaged.boxed_clone(),
                            )
                            .separator()
                            .action_disabled_when(
                                !has_stash_items,
                                "Stash Pop",
                                StashPop.boxed_clone(),
                            )
                    }))
                }
            })
            .anchor(Anchor::TopLeft);

        let group_by = GitPanelSettings::get_global(cx).group_by;
        let group_by_menu = PopoverMenu::new("jetbrains-group-by-menu")
            .with_handle(self.jetbrains.group_by_menu_handle.clone())
            .trigger_with_tooltip(
                IconButton::new("jetbrains-group-by-trigger", IconName::ListTree)
                    .icon_size(IconSize::Small)
                    .disabled(is_branch_mode),
                Tooltip::text("Group By"),
            )
            .menu({
                let focus_handle = focus_handle.clone();
                move |window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _, _| {
                        menu.context(focus_handle.clone())
                            .item(
                                ContextMenuEntry::new("Directory")
                                    .toggle(IconPosition::End, is_tree_mode)
                                    .handler(|window, cx| {
                                        window.dispatch_action(Box::new(ToggleTreeView), cx);
                                    }),
                            )
                            .separator()
                            .item(
                                ContextMenuEntry::new("None")
                                    .toggle(IconPosition::End, group_by == GitPanelGroupBy::None)
                                    .handler(|window, cx| {
                                        window.dispatch_action(Box::new(SetGroupByNone), cx);
                                    }),
                            )
                            .item(
                                ContextMenuEntry::new("Tracked & Untracked")
                                    .toggle(IconPosition::End, group_by == GitPanelGroupBy::Status)
                                    .handler(|window, cx| {
                                        window.dispatch_action(Box::new(SetGroupByStatus), cx);
                                    }),
                            )
                            .item(
                                ContextMenuEntry::new("Staged & Unstaged")
                                    .toggle(
                                        IconPosition::End,
                                        group_by == GitPanelGroupBy::Staging,
                                    )
                                    .handler(|window, cx| {
                                        window.dispatch_action(Box::new(SetGroupByStaging), cx);
                                    }),
                            )
                    }))
                }
            })
            .anchor(Anchor::TopRight);

        Some(
            h_flex()
                .min_h(Tab::container_height(cx))
                .w_full()
                .px_1p5()
                .flex_none()
                .justify_between()
                .gap_1()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    h_flex()
                        .gap_0p5()
                        .child(
                            IconButton::new("jetbrains-refresh", IconName::RotateCcw)
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::for_action_title("Fetch", &git::Fetch))
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(git::Fetch), cx);
                                }),
                        )
                        .child(
                            IconButton::new("jetbrains-rollback", IconName::Undo)
                                .icon_size(IconSize::Small)
                                .disabled(!can_rollback)
                                .tooltip({
                                    let focus_handle = focus_handle.clone();
                                    move |_window, cx| {
                                        Tooltip::for_action_in(
                                            "Rollback",
                                            &RestoreFile::default(),
                                            &focus_handle,
                                            cx,
                                        )
                                    }
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.revert_selected(
                                        &RestoreFile { skip_prompt: false },
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(stash_menu)
                        .child(
                            IconButton::new("jetbrains-show-diff", IconName::Diff)
                                .icon_size(IconSize::Small)
                                .tooltip(move |_window, cx| {
                                    if is_branch_mode {
                                        Tooltip::simple("Show Branch Diff", cx)
                                    } else {
                                        Tooltip::for_action_in(
                                            "Show Diff",
                                            &Diff,
                                            &focus_handle,
                                            cx,
                                        )
                                    }
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.is_branch_mode() {
                                        this.open_branch_diff(None, window, cx);
                                    } else if this.selected_entry.is_some() {
                                        this.open_diff(&menu::Confirm, window, cx);
                                    } else {
                                        cx.defer(|cx| {
                                            cx.dispatch_action(&Diff);
                                        });
                                    }
                                })),
                        )
                        .when(is_tree_mode, |this| {
                            this.child(
                                IconButton::new("jetbrains-expand-all", IconName::ExpandVertical)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Expand All"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.expand_all_jetbrains(window, cx);
                                    })),
                            )
                            .child(
                                IconButton::new("jetbrains-collapse-all", IconName::ListCollapse)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Collapse All"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.collapse_all_jetbrains(window, cx);
                                    })),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap_0p5()
                        .child(self.render_changes_mode_menu(cx))
                        .child(group_by_menu)
                        .child(self.render_view_options_menu("jetbrains_view_options_menu")),
                ),
        )
    }

    pub(super) fn render_jetbrains_stash_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_jetbrains_stash_list(window, cx);
        v_flex()
            .flex_1()
            .size_full()
            .overflow_hidden()
            .children(self.jetbrains.stash_list.clone())
    }

    pub(super) fn render_jetbrains_footer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let active_repository = self.active_repository.clone()?;
        let settings = ThemeSettings::get_global(cx);
        let panel_editor_style =
            git_commit_editor_style(settings.git_commit_buffer_font_size(cx), cx);
        let editor_focus_handle = self.commit_editor.focus_handle(cx);
        let has_head_commit = active_repository.read(cx).head_commit.is_some();
        let amend_pending = self.amend_pending();
        let editor_is_long = self.commit_editor.update(cx, |editor, cx| {
            editor.max_point(cx).row().0 >= MAX_PANEL_EDITOR_LINES as u32
        });

        let max_title_length = GitPanelSettings::get_global(cx).commit_title_max_length;
        let title_exceeds_limit = if max_title_length > 0 {
            self.commit_editor
                .read(cx)
                .text(cx)
                .lines()
                .next()
                .is_some_and(|title| commit_title_exceeds_limit(title, max_title_length))
        } else {
            false
        };

        let collapsed = self.commit_editor_collapsed;
        let (can_commit, commit_tooltip) = self.configure_commit_button(cx);
        let commit_title = self.commit_button_title();
        let can_push = self.can_push_and_pull(cx);
        let options = self.commit_options();

        let amend_checkbox = Checkbox::new("jetbrains-amend-checkbox", amend_pending.into())
            .label("Amend")
            .label_size(LabelSize::Small)
            .disabled(!has_head_commit || self.pending_commit.is_some())
            .on_click(cx.listener(|this, _, _, cx| {
                this.toggle_amend_pending(cx);
            }));

        let fill_editor_button = {
            let (icon, label) = if self.commit_editor_expanded {
                (IconName::Minimize, "Collapse Commit Editor")
            } else {
                (IconName::Maximize, "Expand Commit Editor")
            };
            let focus_handle = self.focus_handle.clone();

            IconButton::new("jetbrains-fill-commit-editor", icon)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(move |_window, cx| {
                    Tooltip::for_action_in(
                        label,
                        &git::ToggleFillCommitEditor,
                        &focus_handle,
                        cx,
                    )
                })
                .on_click(|_, window, cx| {
                    window.dispatch_action(git::ToggleFillCommitEditor.boxed_clone(), cx);
                })
        };

        let toggle_editor_button = {
            let (icon, label) = if collapsed {
                (IconName::ChevronUp, "Show Commit Editor")
            } else {
                (IconName::ChevronDown, "Hide Commit Editor")
            };
            let focus_handle = self.focus_handle.clone();

            IconButton::new("jetbrains-toggle-commit-editor", icon)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(move |_window, cx| {
                    Tooltip::for_action_in(label, &git::ToggleCommitEditor, &focus_handle, cx)
                })
                .on_click(|_, window, cx| {
                    window.dispatch_action(git::ToggleCommitEditor.boxed_clone(), cx);
                })
        };

        let header_row = h_flex()
            .w_full()
            .px_2()
            .py_1()
            .justify_between()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().colors().border_variant)
            .child(amend_checkbox)
            .child(
                h_flex()
                    .gap_0p5()
                    .child(self.render_jetbrains_commit_history_menu(cx))
                    .children(self.render_generate_commit_message_button(cx))
                    .child(self.render_jetbrains_commit_options_menu(cx))
                    .child(fill_editor_button)
                    .child(toggle_editor_button),
            );

        let commit_button = Button::new("jetbrains-commit-button", commit_title)
            .style(ButtonStyle::Filled)
            .layer(ElevationIndex::ModalSurface)
            .size(ButtonSize::Compact)
            .label_size(LabelSize::Small)
            .disabled(!can_commit || self.modal_open)
            .on_click(cx.listener(|this, _, window, cx| {
                telemetry::event!("Git Committed", source = "Git Panel");
                let options = this.commit_options();
                this.commit_changes(options, window, cx);
            }))
            .tooltip({
                let amend = options.amend;
                let signoff = options.signoff;
                let no_verify = options.no_verify;
                move |_window, cx| {
                    if can_commit {
                        Tooltip::with_meta_in(
                            commit_tooltip,
                            Some(&git::Commit),
                            format!(
                                "git commit{}{}{}",
                                if amend { " --amend" } else { "" },
                                if signoff { " --signoff" } else { "" },
                                if no_verify { " --no-verify" } else { "" }
                            ),
                            &editor_focus_handle,
                            cx,
                        )
                    } else {
                        Tooltip::simple(commit_tooltip, cx)
                    }
                }
            });

        let commit_and_push_button = Button::new("jetbrains-commit-and-push-button", "Commit and Push...")
            .style(ButtonStyle::Outlined)
            .layer(ElevationIndex::ModalSurface)
            .size(ButtonSize::Compact)
            .label_size(LabelSize::Small)
            .disabled(!can_commit || !can_push || self.modal_open)
            .on_click(cx.listener(|this, _, window, cx| {
                telemetry::event!("Git Committed", source = "Git Panel");
                this.jetbrains.push_after_commit = true;
                let options = this.commit_options();
                this.commit_changes(options, window, cx);
            }))
            .tooltip(move |_window, cx| {
                if !can_commit {
                    Tooltip::simple(commit_tooltip, cx)
                } else if !can_push {
                    Tooltip::simple("Cannot push via collab", cx)
                } else {
                    Tooltip::simple("Commit and push changes", cx)
                }
            });

        Some(
            v_flex()
                .w_full()
                .when(self.commit_editor_expanded, |this| this.flex_1().min_h_0())
                .child(header_row)
                .when(title_exceeds_limit, |this| {
                    this.child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .gap_1()
                            .border_t_1()
                            .border_color(cx.theme().status().warning_border)
                            .bg(cx.theme().status().warning_background.opacity(0.5))
                            .child(
                                Icon::new(IconName::Warning)
                                    .size(IconSize::XSmall)
                                    .color(Color::Warning),
                            )
                            .child(
                                Label::new(format!(
                                    "Commit message title exceeds {max_title_length}-character limit."
                                ))
                                .size(LabelSize::Small),
                            ),
                    )
                })
                .child(
                    panel_editor_container(window, cx)
                        .id("jetbrains-commit-editor-container")
                        .w_full()
                        .when(self.commit_editor_expanded, |this| this.flex_1().min_h_0())
                        .border_t_1()
                        .border_color(if title_exceeds_limit {
                            cx.theme().status().warning_border
                        } else {
                            cx.theme().colors().border
                        })
                        .when(!collapsed, |this| {
                            this.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                window.focus(&this.commit_editor.focus_handle(cx), cx);
                            }))
                            .child(
                                div()
                                    .pt_2()
                                    .px_2()
                                    .h_full()
                                    .w_full()
                                    .flex_grow_1()
                                    .cursor_text()
                                    .on_action(|&zed_actions::editor::MoveUp, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_action(|&zed_actions::editor::MoveDown, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .child(EditorElement::new(
                                        &self.commit_editor,
                                        panel_editor_style,
                                    )),
                            )
                        })
                        .child(
                            h_flex()
                                .id("jetbrains-commit-actions")
                                .w_full()
                                .p_1p5()
                                .gap_1p5()
                                .border_t_1()
                                .when(editor_is_long, |el| {
                                    el.border_color(cx.theme().colors().border_variant)
                                })
                                .on_hover(cx.listener(move |this, hovered, _, cx| {
                                    this.show_placeholders = *hovered
                                        && !this.has_staged_changes()
                                        && !this.has_unstaged_conflicts();
                                    cx.notify();
                                }))
                                .child(commit_button)
                                .child(commit_and_push_button),
                        ),
                )
                .when(self.amend_pending, |this| {
                    this.child(self.render_pending_amend(cx))
                })
                .when(!self.amend_pending, |this| {
                    this.children(self.render_previous_commit(window, cx))
                })
                .into_any_element(),
        )
    }

    fn render_jetbrains_commit_history_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let git_panel = cx.weak_entity();
        PopoverMenu::new("jetbrains-commit-history-menu")
            .with_handle(self.jetbrains.commit_history_menu_handle.clone())
            .trigger_with_tooltip(
                IconButton::new("jetbrains-commit-history-trigger", IconName::HistoryRerun)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted),
                Tooltip::text("Commit Message History"),
            )
            .menu(move |window, cx| {
                let Some(panel_entity) = git_panel.upgrade() else {
                    return None;
                };
                panel_entity.update(cx, |this, cx| {
                    if let Some(messages) = this.jetbrains.recent_commit_messages.clone() {
                        Some(Self::build_recent_commit_messages_menu(
                            &messages,
                            &panel_entity,
                            window,
                            cx,
                        ))
                    } else {
                        this.ensure_recent_commit_messages(window, cx);
                        Some(ContextMenu::build(window, cx, |menu, _, _| {
                            menu.item(
                                ContextMenuEntry::new("Loading recent commits...").disabled(true),
                            )
                        }))
                    }
                })
            })
            .anchor(Anchor::TopRight)
            .offset(gpui::Point {
                x: px(0.),
                y: px(2.),
            })
    }

    fn build_recent_commit_messages_menu(
        messages: &[RecentCommitMessage],
        git_panel: &Entity<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<ContextMenu> {
        ContextMenu::build(window, cx, |mut menu, _, _| {
            if messages.is_empty() {
                return menu.item(ContextMenuEntry::new("No recent commits by you").disabled(true));
            }

            for recent in messages {
                let message = recent.message.clone();
                let panel = git_panel.downgrade();
                menu = menu.item(ContextMenuEntry::new(recent.subject.clone()).handler(
                    move |window, cx| {
                        panel
                            .update(cx, |this, cx| {
                                this.commit_message_buffer(cx).update(cx, |buffer, cx| {
                                    let start = buffer.anchor_before(0);
                                    let end = buffer.anchor_after(buffer.len());
                                    buffer.edit([(start..end, message.clone())], None, cx);
                                });
                                window.focus(&this.commit_editor.focus_handle(cx), cx);
                                cx.notify();
                            })
                            .ok();
                    },
                ));
            }
            menu
        })
    }

    fn ensure_recent_commit_messages(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.jetbrains.recent_commit_messages.is_some()
            || self.jetbrains.recent_commit_messages_task.is_some()
        {
            return;
        }
        let Some(repo) = self.active_repository.clone() else {
            return;
        };
        if self.head_commit(cx).is_none() {
            self.jetbrains.recent_commit_messages = Some(Vec::new());
            return;
        }

        let cached_email = self
            .local_committer
            .as_ref()
            .and_then(|committer| committer.email.clone());

        self.jetbrains.recent_commit_messages_task = Some(cx.spawn_in(window, async move |this, cx| {
            let email = if let Some(email) = cached_email {
                Some(email)
            } else {
                let committer = get_git_committer(cx).await;
                let email = committer.email.clone();
                this.update(cx, |this, _| {
                    this.local_committer = Some(committer);
                })
                .ok();
                email
            };

            let messages = if email
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
            {
                Vec::new()
            } else {
                let mut details = Vec::new();
                let mut matched_count = 0usize;
                for index in 0..MAX_RECENT_COMMIT_MESSAGES_SCANNED {
                    let revision = if index == 0 {
                        "HEAD".to_string()
                    } else {
                        format!("HEAD~{index}")
                    };
                    let show_rx = repo.update(cx, |repo, _| repo.show(revision));
                    match show_rx.await {
                        Ok(Ok(detail)) => {
                            if !messages_by_author(std::iter::once(&detail), email.as_deref())
                                .is_empty()
                            {
                                matched_count += 1;
                            }
                            details.push(detail);
                            if matched_count >= MAX_RECENT_COMMIT_MESSAGES_SHOWN {
                                break;
                            }
                        }
                        Ok(Err(_)) | Err(_) => break,
                    }
                }
                messages_by_author(&details, email.as_deref())
            };

            this.update_in(cx, |this, window, cx| {
                this.jetbrains.recent_commit_messages = Some(messages.clone());
                this.jetbrains.recent_commit_messages_task = None;
                if this.jetbrains.commit_history_menu_handle.is_deployed() {
                    let git_panel = cx.entity();
                    this.jetbrains.commit_history_menu_handle.refresh_menu(
                        window,
                        cx,
                        Rc::new(move |window, cx| {
                            Some(Self::build_recent_commit_messages_menu(
                                &messages,
                                &git_panel,
                                window,
                                cx,
                            ))
                        }),
                    );
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_jetbrains_commit_options_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let git_panel = cx.entity();
        let editor_focus_handle = self.commit_editor.focus_handle(cx);
        let signoff = self.signoff_enabled;
        let skip_hooks = self.skip_hooks_enabled;
        let add_coauthors = self.add_coauthors;
        let has_coauthors = !self.potential_co_authors(cx).is_empty();

        PopoverMenu::new("jetbrains-commit-options-menu")
            .with_handle(self.jetbrains.commit_options_menu_handle.clone())
            .trigger_with_tooltip(
                IconButton::new("jetbrains-commit-options-trigger", IconName::Settings)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted),
                Tooltip::text("Commit Options"),
            )
            .menu(move |window, cx| {
                let git_panel = git_panel.downgrade();
                let editor_focus_handle = editor_focus_handle.clone();
                Some(ContextMenu::build(window, cx, |menu, _, _| {
                    menu.context(editor_focus_handle.clone())
                        .toggleable_entry(
                            "Signoff",
                            signoff,
                            IconPosition::Start,
                            Some(Box::new(Signoff)),
                            move |window, cx| window.dispatch_action(Box::new(Signoff), cx),
                        )
                        .item(
                            ContextMenuEntry::new("Skip Hooks")
                                .toggleable(IconPosition::Start, skip_hooks)
                                .action(Box::new(SkipHooks))
                                .handler(move |window, cx| {
                                    window.dispatch_action(Box::new(SkipHooks), cx)
                                })
                                .documentation_aside(DocumentationSide::Left, |_| {
                                    Label::new("git commit --no-verify").into_any_element()
                                }),
                        )
                        .when(has_coauthors, |menu| {
                            let git_panel = git_panel.clone();
                            menu.toggleable_entry(
                                "Add Co-authors",
                                add_coauthors,
                                IconPosition::Start,
                                Some(Box::new(ToggleFillCoAuthors)),
                                move |_, cx| {
                                    git_panel
                                        .update(cx, |this, cx| {
                                            this.add_coauthors = !this.add_coauthors;
                                            cx.notify();
                                        })
                                        .ok();
                                },
                            )
                        })
                        .separator()
                        .item(
                            ContextMenuEntry::new("Open Commit Modal")
                                .action(Box::new(git::ExpandCommitEditor))
                                .handler(move |window, cx| {
                                    window.dispatch_action(
                                        git::ExpandCommitEditor.boxed_clone(),
                                        cx,
                                    );
                                }),
                        )
                }))
            })
            .anchor(Anchor::TopRight)
            .offset(gpui::Point {
                x: px(0.),
                y: px(2.),
            })
    }

    fn expand_all_jetbrains(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.collapsed_sections.clear();
        if let Some(state) = self.view_mode.tree_state_mut() {
            for expanded in state.expanded_dirs.values_mut() {
                *expanded = true;
            }
            self.tree_expanded_dirs = state.expanded_dirs.clone();
        }
        self.update_visible_entries(window, cx);
    }

    fn collapse_all_jetbrains(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for entry in &self.entries {
            if let GitListEntry::Header(header) = entry {
                self.collapsed_sections.insert(header.header);
            }
        }
        if let Some(state) = self.view_mode.tree_state_mut() {
            for expanded in state.expanded_dirs.values_mut() {
                *expanded = false;
            }
            self.tree_expanded_dirs = state.expanded_dirs.clone();
        }
        self.update_visible_entries(window, cx);
    }

    pub(super) fn render_jetbrains_list_header(
        &self,
        ix: usize,
        header: &GitHeaderEntry,
        has_write_access: bool,
        _window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id: ElementId = ElementId::Name(format!("header_{}", ix).into());
        let checkbox_id: ElementId = ElementId::Name(format!("header_{}_checkbox", ix).into());
        let group_name: SharedString = format!("header_{}", ix).into();
        let section = header.header;
        let weak = cx.weak_entity();
        let checkbox_weak = weak.clone();
        let stage_intent = StageIntent::for_section(section);
        let toggle_state = stage_intent.checkbox_state(|| self.header_state(header.header));
        let is_collapsed = self.collapsed_sections.contains(&section);

        let all_conflicts_resolved = section == Section::Conflict
            && self.conflicted_count > 0
            && self.conflicted_staged_count == self.conflicted_count;

        let section_is_empty = !self
            .entries
            .get(ix + 1)
            .is_some_and(GitListEntry::is_selectable);
        let file_count = self
            .section_file_counts
            .get(&section)
            .copied()
            .unwrap_or_default();

        h_flex()
            .id(id)
            .group(group_name)
            .h(self.list_item_height())
            .w_full()
            .pl_2p5()
            .pr_1()
            .gap_1p5()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().colors().ghost_element_hover))
            .border_1()
            .border_r_2()
            .child(
                Icon::new(if is_collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .when(!section_is_empty && section != Section::Branch, |this| {
                let checkbox = Checkbox::new(checkbox_id, toggle_state)
                    .disabled(!has_write_access || all_conflicts_resolved)
                    .fill()
                    .elevation(ElevationIndex::Surface)
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        if !has_write_access || all_conflicts_resolved {
                            return;
                        }

                        checkbox_weak
                            .update(cx, |this, cx| {
                                this.toggle_staged_for_entry(
                                    &GitListEntry::Header(GitHeaderEntry { header: section }),
                                    stage_intent,
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                    });
                let tooltip_label = if all_conflicts_resolved {
                    Some("All conflicts marked as resolved")
                } else {
                    match stage_intent {
                        StageIntent::Stage => Some("Stage All"),
                        StageIntent::Unstage => Some("Unstage All"),
                        StageIntent::Toggle => None,
                    }
                };
                if let Some(label) = tooltip_label {
                    this.child(checkbox.tooltip(move |_window, cx| Tooltip::simple(label, cx)))
                } else {
                    this.child(checkbox)
                }
            })
            .child(Label::new(jetbrains_section_title(section)))
            .when(file_count > 0, |this| {
                this.child(
                    Label::new(jetbrains_file_count_label(file_count))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .on_click(move |_, window, cx| {
                weak.update(cx, |this, cx| {
                    this.toggle_section_collapsed(section, window, cx);
                })
                .ok();
            })
            .into_any_element()
    }

    pub(super) fn render_jetbrains_status_entry(
        &self,
        ix: usize,
        entry: &GitStatusEntry,
        depth: usize,
        has_write_access: bool,
        show_checkbox: bool,
        repo: &Repository,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let settings = GitPanelSettings::get_global(cx);
        let tree_view = settings.tree_view;
        let path_style = self.project.read(cx).path_style(cx);
        let git_path_style = ProjectSettings::get_global(cx).git.path_style;
        let display_name = entry.display_name(path_style);

        let selected = self.selected_entry == Some(ix);
        let marked = self.row_is_marked(ix);
        let status_style = settings.status_style;
        let status = entry.status;
        let file_icon = if settings.file_icons {
            FileIcons::get_icon(entry.repo_path.as_std_path(), cx)
        } else {
            None
        };

        let has_conflict = status.is_conflicted();
        let is_modified = status.is_modified();
        let is_deleted = status.is_deleted();
        let is_created = status.is_created();

        let label_color = if status_style == StatusStyle::LabelColor {
            if has_conflict {
                Color::VersionControlConflict
            } else if is_created {
                Color::VersionControlAdded
            } else if is_modified {
                Color::VersionControlModified
            } else if is_deleted {
                Color::Disabled
            } else {
                Color::VersionControlAdded
            }
        } else {
            Color::Default
        };

        let path_color = if status.is_deleted() {
            Color::Disabled
        } else {
            Color::Muted
        };

        let id: ElementId = ElementId::Name(format!("entry_{}_{}", display_name, ix).into());
        let checkbox_wrapper_id: ElementId =
            ElementId::Name(format!("entry_{}_{}_checkbox_wrapper", display_name, ix).into());
        let checkbox_id: ElementId =
            ElementId::Name(format!("entry_{}_{}_checkbox", display_name, ix).into());

        let stage_status = GitPanel::stage_status_for_entry(entry, repo);
        let stage_intent = self.stage_intent_for_entry_index(ix);
        let resolved_conflict = self.is_resolved_conflict(ix, cx);
        let toggle_state = stage_intent.checkbox_state(|| {
            if self.show_placeholders && !self.has_staged_changes() && !entry.status.is_created() {
                ToggleState::Selected
            } else {
                match stage_status {
                    StageStatus::Staged => ToggleState::Selected,
                    StageStatus::Unstaged => ToggleState::Unselected,
                    StageStatus::PartiallyStaged => ToggleState::Indeterminate,
                }
            }
        });

        let handle = cx.weak_entity();

        let selected_bg_alpha = 0.08;
        let marked_bg_alpha = 0.12;
        let state_opacity_step = 0.04;

        let info_color = cx.theme().status().info;

        let base_bg = match (selected, marked) {
            (true, true) => info_color.alpha(selected_bg_alpha + marked_bg_alpha),
            (true, false) => info_color.alpha(selected_bg_alpha),
            (false, true) => info_color.alpha(marked_bg_alpha),
            _ => cx.theme().colors().ghost_element_background,
        };

        let (hover_bg, active_bg) = if selected {
            (
                info_color.alpha(selected_bg_alpha + state_opacity_step),
                info_color.alpha(selected_bg_alpha + state_opacity_step * 2.0),
            )
        } else {
            (
                cx.theme().colors().ghost_element_hover,
                cx.theme().colors().ghost_element_active,
            )
        };

        let dragged_selection = self.dragged_selection(ix, entry, repo, cx);
        let dragged_icon = file_icon.clone();
        let dragged_filename: SharedString = display_name.clone().into();
        let id_for_diff_stat = id.clone();

        let row_content = h_flex()
            .min_w_0()
            .flex_1()
            .gap_1p5()
            .pl(px(JETBRAINS_CHILD_INDENT
                + if tree_view {
                    depth as f32 * JETBRAINS_TREE_INDENT
                } else {
                    0.0
                }))
            .when(show_checkbox, |this| {
                this.child(
                    div()
                        .id(checkbox_wrapper_id)
                        .flex_none()
                        .occlude()
                        .cursor_pointer()
                        .child(
                            Checkbox::new(checkbox_id, toggle_state)
                                .fill()
                                .elevation(ElevationIndex::Surface)
                                .disabled(!has_write_access || resolved_conflict)
                                .on_click_ext({
                                    let entry = entry.clone();
                                    let this = cx.weak_entity();
                                    move |_, click, window, cx| {
                                        this.update(cx, |this, cx| {
                                            if !has_write_access || resolved_conflict {
                                                return;
                                            }
                                            if click.modifiers().shift {
                                                this.stage_bulk(
                                                    ix,
                                                    stage_intent != StageIntent::Unstage,
                                                    cx,
                                                );
                                            } else {
                                                let list_entry = if GitPanelSettings::get_global(cx)
                                                    .tree_view
                                                {
                                                    GitListEntry::TreeStatus(GitTreeStatusEntry {
                                                        entry: entry.clone(),
                                                        depth,
                                                    })
                                                } else {
                                                    GitListEntry::Status(entry.clone())
                                                };
                                                this.toggle_staged_for_entry(
                                                    &list_entry,
                                                    stage_intent,
                                                    window,
                                                    cx,
                                                );
                                            }
                                            cx.stop_propagation();
                                        })
                                        .ok();
                                    }
                                })
                                .tooltip(move |_window, cx| {
                                    if resolved_conflict {
                                        Tooltip::simple("Conflict marked as resolved", cx)
                                    } else {
                                        let action = stage_intent.label(|| stage_status);
                                        Tooltip::for_action(action, &ToggleStaged, cx)
                                    }
                                }),
                        ),
                )
            })
            .when(settings.file_icons, |this| {
                this.child(
                    file_icon
                        .map(Icon::from_path)
                        .unwrap_or_else(|| Icon::new(IconName::File))
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
            })
            .when(status_style != StatusStyle::LabelColor, |el| {
                el.child(git_status_icon(status))
            })
            .map(|this| {
                if tree_view {
                    this.child(
                        self.entry_label(display_name, label_color)
                            .when(status.is_deleted(), Label::strikethrough)
                            .truncate(),
                    )
                } else {
                    this.child(self.path_formatted(
                        entry.parent_dir(path_style),
                        path_color,
                        display_name,
                        label_color,
                        path_style,
                        git_path_style,
                        status.is_deleted(),
                    ))
                }
            });

        h_flex()
            .id(id)
            .h(self.list_item_height())
            .w_full()
            .pl_2p5()
            .pr_1()
            .gap_1p5()
            .border_1()
            .border_r_2()
            .when(selected && self.focus_handle.is_focused(window), |el| {
                el.border_color(cx.theme().colors().panel_focused_border)
            })
            .bg(base_bg)
            .hover(|s| s.bg(hover_bg))
            .active(|s| s.bg(active_bg))
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .child(row_content),
            )
            .when(GitPanelSettings::get_global(cx).diff_stats, |el| {
                el.when_some(entry.diff_stat, move |this, stat| {
                    let id = format!("diff-stat-{}", id_for_diff_stat);
                    this.child(ui::DiffStat::new(
                        id,
                        stat.added as usize,
                        stat.deleted as usize,
                    ))
                })
            })
            .when_some(dragged_selection, |this, dragged_selection| {
                this.on_drag(dragged_selection, move |selection, click_offset, _, cx| {
                    cx.new(|_| DraggedGitEntryView {
                        icon: dragged_icon.clone(),
                        filename: dragged_filename.clone(),
                        entry_count: selection.items().count(),
                        click_offset,
                    })
                })
            })
            .on_click({
                cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.modifiers().shift {
                        let anchor_ix = this.selected_entry.unwrap_or(ix);
                        this.apply_range_gesture(anchor_ix, ix);
                        this.selected_entry = Some(ix);
                        cx.notify();
                    } else if event.modifiers().secondary(cx) {
                        this.toggle_mark(ix, cx);
                    } else {
                        this.clear_marks_and_select(ix, cx);
                        this.open_selected_entry_on_click(event.click_count() > 1, window, cx);
                    }
                })
            })
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, window, cx| {
                    if event.button != MouseButton::Right {
                        return;
                    }

                    let Some(this) = handle.upgrade() else {
                        return;
                    };
                    this.update(cx, |this, cx| {
                        this.deploy_entry_context_menu(event.position, ix, window, cx);
                    });
                    cx.stop_propagation();
                },
            )
            .into_any_element()
    }

    pub(super) fn render_jetbrains_directory_entry(
        &self,
        ix: usize,
        entry: &GitTreeDirEntry,
        has_write_access: bool,
        show_checkbox: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_entry == Some(ix);
        let marked = self.marked_directories.contains(&entry.key);
        let id: ElementId = ElementId::Name(format!("dir_{}_{}", entry.name, ix).into());
        let checkbox_id: ElementId =
            ElementId::Name(format!("dir_checkbox_{}_{}", entry.name, ix).into());
        let checkbox_wrapper_id: ElementId =
            ElementId::Name(format!("dir_checkbox_wrapper_{}_{}", entry.name, ix).into());

        let selected_bg_alpha = 0.08;
        let marked_bg_alpha = 0.12;
        let state_opacity_step = 0.04;

        let info_color = cx.theme().status().info;
        let colors = cx.theme().colors();

        let base_bg = match (selected, marked) {
            (true, true) => info_color.alpha(selected_bg_alpha + marked_bg_alpha),
            (true, false) => info_color.alpha(selected_bg_alpha),
            (false, true) => info_color.alpha(marked_bg_alpha),
            _ => colors.ghost_element_background,
        };

        let (hover_bg, active_bg) = if selected {
            (
                info_color.alpha(selected_bg_alpha + state_opacity_step),
                info_color.alpha(selected_bg_alpha + state_opacity_step * 2.0),
            )
        } else {
            (colors.ghost_element_hover, colors.ghost_element_active)
        };

        let settings = GitPanelSettings::get_global(cx);
        let folder_indicator = settings.folder_indicator;
        let folder_indicators = FileIcons::get_folder_indicators(
            folder_indicator,
            entry.expanded,
            entry.key.path.as_std_path(),
            cx,
        );
        let fallback_chevron = if entry.expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let fallback_folder_icon = if entry.expanded {
            IconName::FolderOpen
        } else {
            IconName::Folder
        };

        let stage_status = if let Some(repo) = &self.active_repository {
            self.stage_status_for_directory(entry, repo.read(cx))
        } else {
            util::debug_panic!(
                "Won't have entries to render without an active repository in Git Panel"
            );
            StageStatus::PartiallyStaged
        };

        let stage_intent = StageIntent::for_section(entry.key.section);
        let resolved_conflict = self.is_resolved_conflict(ix, cx);
        let toggle_state = stage_intent.checkbox_state(|| match stage_status {
            StageStatus::Staged => ToggleState::Selected,
            StageStatus::Unstaged => ToggleState::Unselected,
            StageStatus::PartiallyStaged => ToggleState::Indeterminate,
        });

        let file_count = match &self.view_mode {
            GitPanelViewMode::Tree(tree_state) => tree_state
                .directory_descendants
                .get(&entry.key)
                .map(|descendants| descendants.len())
                .unwrap_or_default(),
            GitPanelViewMode::Flat => 0,
        };

        let render_indicator = |themed: Option<SharedString>, fallback: IconName| {
            themed
                .map(Icon::from_path)
                .unwrap_or_else(|| Icon::new(fallback))
                .size(IconSize::Small)
                .color(Color::Muted)
        };

        let row_content = h_flex()
            .min_w_0()
            .flex_1()
            .gap_1p5()
            .pl(px(JETBRAINS_CHILD_INDENT + entry.depth as f32 * JETBRAINS_TREE_INDENT))
            .when(folder_indicator.shows_chevron(), |this| {
                this.child(render_indicator(
                    folder_indicators.chevron,
                    fallback_chevron,
                ))
            })
            .when(show_checkbox, |this| {
                this.child(
                    div()
                        .id(checkbox_wrapper_id)
                        .flex_none()
                        .occlude()
                        .cursor_pointer()
                        .child(
                            Checkbox::new(checkbox_id, toggle_state)
                                .disabled(!has_write_access || resolved_conflict)
                                .fill()
                                .elevation(ElevationIndex::Surface)
                                .on_click({
                                    let entry = entry.clone();
                                    let this = cx.weak_entity();
                                    move |_, window, cx| {
                                        this.update(cx, |this, cx| {
                                            if !has_write_access || resolved_conflict {
                                                return;
                                            }
                                            this.toggle_staged_for_entry(
                                                &GitListEntry::Directory(entry.clone()),
                                                stage_intent,
                                                window,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        })
                                        .ok();
                                    }
                                })
                                .tooltip(move |_window, cx| {
                                    if resolved_conflict {
                                        Tooltip::simple("Conflicts marked as resolved", cx)
                                    } else {
                                        let action = stage_intent.label(|| stage_status);
                                        Tooltip::simple(format!("{action} Folder"), cx)
                                    }
                                }),
                        ),
                )
            })
            .when(folder_indicator.shows_icon(), |this| {
                this.child(render_indicator(
                    folder_indicators.icon,
                    fallback_folder_icon,
                ))
            })
            .child(
                self.entry_label(entry.name.clone(), Color::Default)
                    .truncate(),
            )
            .when(file_count > 0, |this| {
                this.child(
                    Label::new(jetbrains_file_count_label(file_count))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            });

        h_flex()
            .id(id)
            .h(self.list_item_height())
            .min_w_0()
            .w_full()
            .pl_2p5()
            .pr_1()
            .gap_1p5()
            .border_1()
            .border_r_2()
            .when(selected && self.focus_handle.is_focused(window), |el| {
                el.border_color(cx.theme().colors().panel_focused_border)
            })
            .bg(base_bg)
            .hover(|s| s.bg(hover_bg))
            .active(|s| s.bg(active_bg))
            .child(row_content)
            .on_click({
                let key = entry.key.clone();
                cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.modifiers().shift {
                        let anchor_ix = this.selected_entry.unwrap_or(ix);
                        this.apply_range_gesture(anchor_ix, ix);
                        this.selected_entry = Some(ix);
                        cx.notify();
                    } else if event.modifiers().secondary(cx) {
                        this.toggle_mark(ix, cx);
                    } else {
                        this.clear_marks();
                        this.selected_entry = Some(ix);
                        this.toggle_directory(&key, window, cx);
                    }
                })
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.deploy_entry_context_menu(event.position, ix, window, cx);
                }),
            )
            .into_any_element()
    }
}

fn jetbrains_section_title(section: Section) -> &'static str {
    match section {
        Section::Conflict => "Merge Conflicts",
        Section::Tracked => "Changes",
        Section::New => "Unversioned Files",
        Section::Staged => "Staged",
        Section::Unstaged => "Unstaged",
        Section::Branch => "Branch Changes",
    }
}

fn jetbrains_file_count_label(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_panel::tests::{
        await_git_panel_entries, directory_index_for_repo_path, entry_index_for_repo_path,
        init_test, setup_git_panel_with_changes,
    };
    use git::{
        repository::{CommitDetails, repo_path},
        status::StatusCode,
    };
    use gpui::{TestAppContext, UpdateGlobal, VisualTestContext};
    use serde_json::json;
    use settings::{GitPanelLayout, SettingsStore};
    use util::path;

    fn stage_status_for_path(panel: &GitPanel, path: &str, cx: &App) -> Option<StageStatus> {
        let repo = panel.active_repository.as_ref()?.read(cx);
        let target = repo_path(path);
        panel.entries.iter().find_map(|entry| {
            entry
                .status_entry()
                .filter(|status_entry| status_entry.repo_path == target)
                .map(|status_entry| GitPanel::stage_status_for_entry(status_entry, repo))
        })
    }

    fn set_git_panel_layout(layout: GitPanelLayout, cx: &mut VisualTestContext) {
        let layout_str = match layout {
            GitPanelLayout::Zed => "zed",
            GitPanelLayout::Jetbrains => "jetbrains",
        };
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(&format!(r#"{{"git_panel": {{"layout": "{layout_str}"}}}}"#), cx)
                    .unwrap();
            });
        });
    }

    #[gpui::test]
    async fn test_runtime_layout_switch_zed_and_jetbrains(cx: &mut TestAppContext) {
        init_test(cx);
        let (_, _, _, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "file.rs": "modified\n",
            }),
            &[("file.rs", StatusCode::Modified)],
        )
        .await;

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(GitPanelSettings::get_global(cx).layout, GitPanelLayout::Zed);
            assert!(!panel.is_jetbrains_layout(cx));
        });

        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        r#"{"git_panel": {"layout": "jetbrains", "tree_view": true}}"#,
                        cx,
                    )
                    .unwrap();
            });
        });
        cx.run_until_parked();
        await_git_panel_entries(&panel, &mut cx).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            assert_eq!(
                GitPanelSettings::get_global(cx).layout,
                GitPanelLayout::Jetbrains
            );
            assert!(panel.is_jetbrains_layout(cx));
            panel.set_active_tab(GitPanelTab::Stash, window, cx);
            assert_eq!(panel.active_tab, GitPanelTab::Stash);
        });
        cx.run_until_parked();

        set_git_panel_layout(GitPanelLayout::Zed, &mut cx);
        cx.run_until_parked();
        await_git_panel_entries(&panel, &mut cx).await;

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(GitPanelSettings::get_global(cx).layout, GitPanelLayout::Zed);
            assert!(!panel.is_jetbrains_layout(cx));
            assert_eq!(panel.active_tab, GitPanelTab::Changes);
        });
    }

    #[gpui::test]
    async fn test_jetbrains_header_directory_and_file_stage_unstage(cx: &mut TestAppContext) {
        init_test(cx);
        let (_, _, _, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "src": {
                    "a.rs": "a\n",
                    "b.rs": "b\n",
                },
                "root.rs": "root\n",
            }),
            &[
                ("src/a.rs", StatusCode::Modified),
                ("src/b.rs", StatusCode::Modified),
                ("root.rs", StatusCode::Modified),
            ],
        )
        .await;

        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        r#"{"git_panel": {"layout": "jetbrains", "tree_view": true}}"#,
                        cx,
                    )
                    .unwrap();
            });
        });
        cx.run_until_parked();
        await_git_panel_entries(&panel, &mut cx).await;

        // 1. Header checkbox stages and unstages all tracked files
        panel.update_in(&mut cx, |panel, window, cx| {
            assert!(panel.is_jetbrains_layout(cx));
            let _ = panel.render_jetbrains_list_header(
                0,
                &GitHeaderEntry {
                    header: Section::Tracked,
                },
                true,
                window,
                cx,
            );
            panel.toggle_staged_for_entry(
                &GitListEntry::Header(GitHeaderEntry {
                    header: Section::Tracked,
                }),
                StageIntent::for_section(Section::Tracked),
                window,
                cx,
            );
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(panel.header_state(Section::Tracked), ToggleState::Selected);
            assert_eq!(
                stage_status_for_path(panel, "src/a.rs", cx),
                Some(StageStatus::Staged)
            );
            assert_eq!(
                stage_status_for_path(panel, "root.rs", cx),
                Some(StageStatus::Staged)
            );
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            panel.toggle_staged_for_entry(
                &GitListEntry::Header(GitHeaderEntry {
                    header: Section::Tracked,
                }),
                StageIntent::for_section(Section::Tracked),
                window,
                cx,
            );
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(
                panel.header_state(Section::Tracked),
                ToggleState::Unselected
            );
            assert_eq!(
                stage_status_for_path(panel, "src/a.rs", cx),
                Some(StageStatus::Unstaged)
            );
            assert_eq!(
                stage_status_for_path(panel, "root.rs", cx),
                Some(StageStatus::Unstaged)
            );
        });

        // 2. Directory checkbox stages and unstages only directory descendants
        panel.update_in(&mut cx, |panel, window, cx| {
            let dir_ix = directory_index_for_repo_path(panel, &repo_path("src"))
                .expect("src directory should be present in tree view");
            let dir_entry = panel.entries[dir_ix]
                .directory_entry()
                .expect("entry should be a directory")
                .clone();
            let _ =
                panel.render_jetbrains_directory_entry(dir_ix, &dir_entry, true, true, window, cx);
            panel.toggle_staged_for_entry(
                &GitListEntry::Directory(dir_entry.clone()),
                StageIntent::for_section(dir_entry.key.section),
                window,
                cx,
            );
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(
                stage_status_for_path(panel, "src/a.rs", cx),
                Some(StageStatus::Staged)
            );
            assert_eq!(
                stage_status_for_path(panel, "src/b.rs", cx),
                Some(StageStatus::Staged)
            );
            assert_eq!(
                stage_status_for_path(panel, "root.rs", cx),
                Some(StageStatus::Unstaged)
            );
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            let dir_ix = directory_index_for_repo_path(panel, &repo_path("src")).unwrap();
            let dir_entry = panel.entries[dir_ix].directory_entry().unwrap().clone();
            panel.toggle_staged_for_entry(
                &GitListEntry::Directory(dir_entry.clone()),
                StageIntent::for_section(dir_entry.key.section),
                window,
                cx,
            );
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(
                stage_status_for_path(panel, "src/a.rs", cx),
                Some(StageStatus::Unstaged)
            );
            assert_eq!(
                stage_status_for_path(panel, "src/b.rs", cx),
                Some(StageStatus::Unstaged)
            );
        });

        // 3. File checkbox stages and unstages a single file
        panel.update_in(&mut cx, |panel, window, cx| {
            let file_ix = entry_index_for_repo_path(panel, &repo_path("root.rs"))
                .expect("root.rs should be present");
            let status_entry = panel.entries[file_ix].status_entry().unwrap().clone();
            let repo = panel.active_repository.as_ref().unwrap().read(cx);
            let _ = panel.render_jetbrains_status_entry(
                file_ix,
                &status_entry,
                0,
                true,
                true,
                repo,
                window,
                cx,
            );
            let list_entry = panel.entries[file_ix].clone();
            let intent = panel.stage_intent_for_entry_index(file_ix);
            panel.toggle_staged_for_entry(&list_entry, intent, window, cx);
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(
                stage_status_for_path(panel, "root.rs", cx),
                Some(StageStatus::Staged)
            );
            assert_eq!(
                stage_status_for_path(panel, "src/a.rs", cx),
                Some(StageStatus::Unstaged)
            );
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            let file_ix = entry_index_for_repo_path(panel, &repo_path("root.rs")).unwrap();
            let list_entry = panel.entries[file_ix].clone();
            let intent = panel.stage_intent_for_entry_index(file_ix);
            panel.toggle_staged_for_entry(&list_entry, intent, window, cx);
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(
                stage_status_for_path(panel, "root.rs", cx),
                Some(StageStatus::Unstaged)
            );
        });
    }

    #[test]
    fn test_messages_by_author_filter_and_limit() {
        let make_commit = |sha: &str, email: &str, message: &str| CommitDetails {
            sha: sha.to_string().into(),
            message: message.to_string().into(),
            commit_timestamp: 0,
            author_email: email.to_string().into(),
            author_name: "Author".into(),
        };

        let mut details = vec![
            make_commit("0", "other@example.com", "Other author commit"),
            make_commit("1", "  ME@Example.COM ", "First subject\n\nBody line\n\n"),
            make_commit("2", "me@example.com", "   \n\n"),
        ];
        for index in 0..25 {
            details.push(make_commit(
                &format!("extra-{index}"),
                "me@example.com",
                &format!("Commit {index}"),
            ));
        }

        assert!(messages_by_author(&details, None).is_empty());
        assert!(messages_by_author(&details, Some("   ")).is_empty());

        let filtered = messages_by_author(&details, Some("me@example.com"));
        assert_eq!(filtered.len(), MAX_RECENT_COMMIT_MESSAGES_SHOWN);
        assert_eq!(filtered[0].subject.as_ref(), "First subject");
        assert_eq!(filtered[0].message, "First subject\n\nBody line");
        assert_eq!(filtered[1].subject.as_ref(), "Commit 0");
    }

    #[gpui::test]
    async fn test_jetbrains_amend_signoff_skip_hooks_and_message_history(cx: &mut TestAppContext) {
        init_test(cx);
        let (_, _, _, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "file.rs": "modified\n",
            }),
            &[("file.rs", StatusCode::Modified)],
        )
        .await;

        set_git_panel_layout(GitPanelLayout::Jetbrains, &mut cx);
        cx.run_until_parked();

        // Amend, Sign-off, Skip Hooks toggles
        panel.update_in(&mut cx, |panel, window, cx| {
            assert!(!panel.amend_pending());
            panel.toggle_amend_pending(cx);
            assert!(panel.amend_pending());
            assert!(panel.commit_options().amend);
            panel.toggle_amend_pending(cx);
            assert!(!panel.amend_pending());

            assert!(!panel.signoff_enabled);
            panel.toggle_signoff_enabled(&Signoff, window, cx);
            assert!(panel.signoff_enabled);
            assert!(panel.commit_options().signoff);
            panel.toggle_signoff_enabled(&Signoff, window, cx);
            assert!(!panel.signoff_enabled);

            assert!(!panel.skip_hooks_enabled());
            panel.toggle_skip_hooks(&SkipHooks, window, cx);
            assert!(panel.skip_hooks_enabled());
            assert!(panel.commit_options().no_verify);
            panel.toggle_skip_hooks(&SkipHooks, window, cx);
            assert!(!panel.skip_hooks_enabled());
        });
        cx.run_until_parked();

        // Message-history insertion replaces existing commit editor text
        panel.update_in(&mut cx, |panel, _, cx| {
            panel
                .commit_message_buffer(cx)
                .update(cx, |buffer, cx| buffer.set_text("existing draft", cx));
        });

        let messages = vec![
            RecentCommitMessage {
                subject: "feat: replace editor".into(),
                message: "feat: replace editor\n\nDetailed body".to_string(),
            },
            RecentCommitMessage {
                subject: "fix: second".into(),
                message: "fix: second".to_string(),
            },
        ];

        let menu = cx.update(|window, cx| {
            GitPanel::build_recent_commit_messages_menu(&messages, &panel, window, cx)
        });
        menu.update_in(&mut cx, |menu, window, cx| {
            menu.select_first(&menu::SelectFirst, window, cx);
            menu.confirm(&menu::Confirm, window, cx);
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, cx| {
            assert_eq!(
                panel.commit_editor.read(cx).text(cx),
                "feat: replace editor\n\nDetailed body"
            );
        });
    }

    #[gpui::test]
    async fn test_jetbrains_commit_and_push_success(cx: &mut TestAppContext) {
        init_test(cx);
        let (fs, _, _, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "file.rs": "modified\n",
            }),
            &[("file.rs", StatusCode::Modified)],
        )
        .await;

        fs.insert_branches(path!("/project/.git").as_ref(), &["main"]);
        fs.with_git_state(path!("/project/.git").as_ref(), false, |state| {
            state.current_branch_name = Some("main".to_string());
        })
        .expect("fake repository should exist");
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        r#"{"git_panel": {"layout": "jetbrains", "tree_view": true}}"#,
                        cx,
                    )
                    .unwrap();
            });
        });
        cx.run_until_parked();
        await_git_panel_entries(&panel, &mut cx).await;

        let commit_task = panel.update_in(&mut cx, |panel, window, cx| {
            panel
                .commit_message_buffer(cx)
                .update(cx, |buffer, cx| buffer.set_text("Commit and push", cx));
            panel.jetbrains.push_after_commit = true;
            panel.jetbrains.skip_actual_push_in_tests = true;
            let options = panel.commit_options();
            panel.commit_changes(options, window, cx);
            std::mem::replace(&mut panel.pending_commit, None)
        });

        commit_task
            .expect("commit task should be spawned")
            .await;
        cx.run_until_parked();

        panel.read_with(&cx, |panel, _| {
            assert!(!panel.jetbrains.push_after_commit);
            assert_eq!(panel.jetbrains.push_triggered_count, 1);
        });
        let commit_count = fs
            .with_git_state(path!("/project/.git").as_ref(), false, |state| {
                state.commit_history.len()
            })
            .expect("fake repository should exist");
        assert_eq!(commit_count, 1);
    }

    #[gpui::test]
    async fn test_jetbrains_commit_and_push_failure_clears_flag_without_push(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let (fs, _, _, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "file.rs": "modified\n",
            }),
            &[("file.rs", StatusCode::Modified)],
        )
        .await;

        fs.insert_branches(path!("/project/.git").as_ref(), &["main"]);
        fs.set_status_for_repo(
            path!("/project/.git").as_ref(),
            &[("file.rs", StatusCode::Modified.index())],
        );
        cx.run_until_parked();
        await_git_panel_entries(&panel, &mut cx).await;
        fs.with_git_state(path!("/project/.git").as_ref(), false, |state| {
            state.current_branch_name = Some("main".to_string());
            state.index_contents = state.head_contents.clone();
        })
        .expect("fake repository should exist");

        set_git_panel_layout(GitPanelLayout::Jetbrains, &mut cx);
        cx.run_until_parked();

        let commit_task = panel.update_in(&mut cx, |panel, window, cx| {
            panel
                .commit_message_buffer(cx)
                .update(cx, |buffer, cx| buffer.set_text("Failing commit", cx));
            panel.jetbrains.push_after_commit = true;
            let options = panel.commit_options();
            panel.commit_changes(options, window, cx);
            std::mem::replace(&mut panel.pending_commit, None)
        });

        commit_task
            .expect("commit task should be spawned")
            .await;

        panel.read_with(&cx, |panel, _| {
            assert!(!panel.jetbrains.push_after_commit);
            assert_eq!(panel.jetbrains.push_triggered_count, 0);
            assert!(panel.pending_remote_operation.is_none());
        });
        let commit_count = fs
            .with_git_state(path!("/project/.git").as_ref(), false, |state| {
                state.commit_history.len()
            })
            .expect("fake repository should exist");
        assert_eq!(commit_count, 0);

        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_jetbrains_tab_switching_and_stash_list_reset_on_repo_change(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let (fs, project, _, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "file.rs": "modified\n",
            }),
            &[("file.rs", StatusCode::Modified)],
        )
        .await;

        set_git_panel_layout(GitPanelLayout::Jetbrains, &mut cx);
        cx.run_until_parked();

        panel.update_in(&mut cx, |panel, window, cx| {
            assert_eq!(panel.active_tab, GitPanelTab::Changes);
            assert!(panel.jetbrains.stash_list.is_none());

            panel.set_active_tab(GitPanelTab::Stash, window, cx);
            assert_eq!(panel.active_tab, GitPanelTab::Stash);
            assert!(panel.jetbrains.stash_list.is_some());

            panel.set_active_tab(GitPanelTab::History, window, cx);
            assert_eq!(panel.active_tab, GitPanelTab::History);

            panel.set_active_tab(GitPanelTab::Changes, window, cx);
            assert_eq!(panel.active_tab, GitPanelTab::Changes);
        });

        // Switching to Stash creates stash_list; changing active repo resets it
        let initial_stash_id = panel.update_in(&mut cx, |panel, window, cx| {
            panel.set_active_tab(GitPanelTab::Stash, window, cx);
            panel
                .jetbrains
                .stash_list
                .as_ref()
                .expect("stash list should be created")
                .entity_id()
        });

        fs.insert_tree(
            path!("/project2"),
            json!({
                ".git": {},
                "other.rs": "other\n",
            }),
        )
        .await;
        project
            .update(&mut cx, |project, cx| {
                project.find_or_create_worktree(path!("/project2"), true, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();

        let (first_repo, second_repo) = project.read_with(&cx, |project, cx| {
            let repos = project.git_store().read(cx).repositories();
            let first = repos
                .values()
                .find(|repo| {
                    repo.read(cx).dot_git_abs_path.as_ref() == Path::new(path!("/project/.git"))
                })
                .cloned()
                .expect("first repository should be registered");
            let second = repos
                .values()
                .find(|repo| {
                    repo.read(cx).dot_git_abs_path.as_ref() == Path::new(path!("/project2/.git"))
                })
                .cloned()
                .expect("second repository should be registered");
            (first, second)
        });

        second_repo.update(&mut cx, |repo, cx| repo.set_as_active_repository(cx));
        cx.run_until_parked();

        panel.read_with(&cx, |panel, _| {
            let new_stash_id = panel
                .jetbrains
                .stash_list
                .as_ref()
                .expect("stash list should be recreated on Stash tab")
                .entity_id();
            assert_ne!(initial_stash_id, new_stash_id);
        });

        // When not on Stash tab, changing active repo clears stash_list to None
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.set_active_tab(GitPanelTab::Changes, window, cx);
        });
        first_repo.update(&mut cx, |repo, cx| repo.set_as_active_repository(cx));
        cx.run_until_parked();

        panel.read_with(&cx, |panel, _| {
            assert!(panel.jetbrains.stash_list.is_none());
        });
    }

    #[gpui::test]
    async fn test_jetbrains_tab_bar_only_renders_commit_and_stash_tabs(cx: &mut TestAppContext) {
        init_test(cx);
        let (_, _, workspace, panel, mut cx) = setup_git_panel_with_changes(
            cx,
            json!({
                ".git": {},
                "file.rs": "modified\n",
            }),
            &[("file.rs", StatusCode::Modified)],
        )
        .await;

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.add_panel(panel.clone(), window, cx);
            workspace.open_panel::<GitPanel>(window, cx);
        });
        cx.run_until_parked();

        // Simulate History tab being active in Zed layout before switching to JetBrains layout.
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.set_active_tab(GitPanelTab::History, window, cx);
            assert_eq!(panel.active_tab, GitPanelTab::History);
        });
        cx.run_until_parked();

        set_git_panel_layout(GitPanelLayout::Jetbrains, &mut cx);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        assert!(cx.debug_bounds("jetbrains-commit-tab").is_some());
        assert!(cx.debug_bounds("jetbrains-stash-tab").is_some());
        assert!(cx.debug_bounds("jetbrains-history-tab").is_none());
        // Falling back from History renders the Changes view (including the commit footer).
        assert!(cx.debug_bounds("ICON-HistoryRerun").is_some());
    }
}
