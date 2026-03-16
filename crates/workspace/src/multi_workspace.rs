use anyhow::Result;
use feature_flags::{AgentV2FeatureFlag, FeatureFlagAppExt};
use gpui::{
    App, Context, Entity, EntityId, EventEmitter, Focusable, FontWeight, ManagedView, Pixels,
    Render, Subscription, Task, Tiling, Window, WindowId, actions, px,
};
use project::{DisableAiSettings, Project};
use settings::Settings;
use std::future::Future;
use std::path::PathBuf;
use ui::prelude::*;
use util::ResultExt;

use crate::workspace_settings::WorkspaceSettings;

pub const SIDEBAR_RESIZE_HANDLE_SIZE: Pixels = px(6.0);
const DEFAULT_SIDEBAR_WIDTH: Pixels = px(240.0);
#[allow(dead_code)]
const MIN_SIDEBAR_WIDTH: Pixels = px(150.0);
#[allow(dead_code)]
const MAX_SIDEBAR_WIDTH: Pixels = px(500.0);

use crate::{
    CloseIntent, CloseWindow, DockPosition, Event as WorkspaceEvent, Item, ModalView,
    Panel, Toast, Workspace, WorkspaceId, client_side_decorations,
    notifications::NotificationId, persistence::model::MultiWorkspaceId,
};

actions!(
    multi_workspace,
    [
        /// Creates a new workspace within the current window.
        NewWorkspaceInWindow,
        /// Switches to the next workspace within the current window.
        NextWorkspaceInWindow,
        /// Switches to the previous workspace within the current window.
        PreviousWorkspaceInWindow,
        /// Toggles the workspace switcher sidebar.
        ToggleWorkspaceSidebar,
        /// Moves focus to or from the workspace sidebar without closing it.
        FocusWorkspaceSidebar,
        /// Opens multiple folders, each as its own workspace in the current window.
        OpenFoldersAsWorkspaces,
    ]
);

pub enum MultiWorkspaceEvent {
    ActiveWorkspaceChanged,
    WorkspaceAdded(Entity<Workspace>),
    WorkspaceRemoved(EntityId),
}

#[derive(Clone)]
pub struct DraggedSidebar;

impl Render for DraggedSidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

pub struct MultiWorkspace {
    window_id: WindowId,
    workspaces: Vec<Entity<Workspace>>,
    database_id: Option<MultiWorkspaceId>,
    active_workspace_index: usize,
    sidebar_visible: bool,
    sidebar_width: Pixels,
    expanded_folders: Vec<bool>,
    pending_removal_tasks: Vec<Task<()>>,
    _serialize_task: Option<Task<()>>,
    _create_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MultiWorkspaceEvent> for MultiWorkspace {}

pub fn multi_workspace_enabled(cx: &App) -> bool {
    let agent_v2 =
        cx.has_flag::<AgentV2FeatureFlag>() && !DisableAiSettings::get_global(cx).disable_ai;
    let multi_folder = WorkspaceSettings::get_global(cx).multi_folder_workspaces_enabled;
    agent_v2 || multi_folder
}

impl MultiWorkspace {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let release_subscription = cx.on_release(|this: &mut MultiWorkspace, _cx| {
            if let Some(task) = this._serialize_task.take() {
                task.detach();
            }
            if let Some(task) = this._create_task.take() {
                task.detach();
            }
            for task in std::mem::take(&mut this.pending_removal_tasks) {
                task.detach();
            }
        });
        let quit_subscription = cx.on_app_quit(Self::app_will_quit);
        Self::subscribe_to_workspace(&workspace, cx);
        Self {
            window_id: window.window_handle().window_id(),
            database_id: None,
            workspaces: vec![workspace],
            active_workspace_index: 0,
            sidebar_visible: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            expanded_folders: vec![true],
            pending_removal_tasks: Vec::new(),
            _serialize_task: None,
            _create_task: None,
            _subscriptions: vec![release_subscription, quit_subscription],
        }
    }

    pub fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let workspaces = this.update(cx, |multi_workspace, _cx| {
                multi_workspace.workspaces().to_vec()
            })?;

            for workspace in workspaces {
                let should_continue = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.prepare_to_close(CloseIntent::CloseWindow, window, cx)
                    })?
                    .await?;
                if !should_continue {
                    return anyhow::Ok(());
                }
            }

            cx.update(|window, _cx| {
                window.remove_window();
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn subscribe_to_workspace(workspace: &Entity<Workspace>, cx: &mut Context<Self>) {
        cx.subscribe(workspace, |this, workspace, event, cx| {
            if let WorkspaceEvent::Activate = event {
                this.activate(workspace, cx);
            }
        })
        .detach();
    }

    pub fn workspace(&self) -> &Entity<Workspace> {
        &self.workspaces[self.active_workspace_index]
    }

    pub fn workspaces(&self) -> &[Entity<Workspace>] {
        &self.workspaces
    }

    pub fn active_workspace_index(&self) -> usize {
        self.active_workspace_index
    }

    pub fn sidebar_visible(&self) -> bool {
        self.sidebar_visible
    }

    pub fn activate(&mut self, workspace: Entity<Workspace>, cx: &mut Context<Self>) {
        if !multi_workspace_enabled(cx) {
            self.workspaces[0] = workspace;
            self.active_workspace_index = 0;
            cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
            cx.notify();
            return;
        }

        let old_index = self.active_workspace_index;
        let new_index = self.set_active_workspace(workspace, cx);
        if old_index != new_index {
            self.serialize(cx);
        }
    }

    fn set_active_workspace(
        &mut self,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> usize {
        let index = self.add_workspace(workspace, cx);
        let changed = self.active_workspace_index != index;
        self.active_workspace_index = index;
        if changed {
            cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        }
        cx.notify();
        index
    }

    /// Adds a workspace to this window without changing which workspace is active.
    /// Returns the index of the workspace (existing or newly inserted).
    pub fn add_workspace(&mut self, workspace: Entity<Workspace>, cx: &mut Context<Self>) -> usize {
        if let Some(index) = self.workspaces.iter().position(|w| *w == workspace) {
            index
        } else {
            Self::subscribe_to_workspace(&workspace, cx);
            self.workspaces.push(workspace.clone());
            self.expanded_folders.push(true);
            cx.emit(MultiWorkspaceEvent::WorkspaceAdded(workspace));
            cx.notify();
            self.workspaces.len() - 1
        }
    }

    pub fn database_id(&self) -> Option<MultiWorkspaceId> {
        self.database_id
    }

    pub fn set_database_id(&mut self, id: Option<MultiWorkspaceId>) {
        self.database_id = id;
    }

    pub fn activate_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        debug_assert!(
            index < self.workspaces.len(),
            "workspace index out of bounds"
        );
        let changed = self.active_workspace_index != index;
        self.active_workspace_index = index;
        self.serialize(cx);
        self.focus_active_workspace(window, cx);
        if changed {
            cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        }
        cx.notify();
    }

    pub fn activate_next_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces.len() > 1 {
            let next_index = (self.active_workspace_index + 1) % self.workspaces.len();
            self.activate_index(next_index, window, cx);
        }
    }

    pub fn activate_previous_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces.len() > 1 {
            let prev_index = if self.active_workspace_index == 0 {
                self.workspaces.len() - 1
            } else {
                self.active_workspace_index - 1
            };
            self.activate_index(prev_index, window, cx);
        }
    }

    fn serialize(&mut self, cx: &mut App) {
        let window_id = self.window_id;
        let state = crate::persistence::model::MultiWorkspaceState {
            active_workspace_id: self.workspace().read(cx).database_id(),
        };
        self._serialize_task = Some(cx.background_spawn(async move {
            crate::persistence::write_multi_workspace_state(window_id, state).await;
        }));
    }

    /// Returns the in-flight serialization task (if any) so the caller can
    /// await it. Used by the quit handler to ensure pending DB writes
    /// complete before the process exits.
    pub fn flush_serialization(&mut self) -> Task<()> {
        self._serialize_task.take().unwrap_or(Task::ready(()))
    }

    fn app_will_quit(&mut self, _cx: &mut Context<Self>) -> impl Future<Output = ()> + use<> {
        let mut tasks: Vec<Task<()>> = Vec::new();
        if let Some(task) = self._serialize_task.take() {
            tasks.push(task);
        }
        if let Some(task) = self._create_task.take() {
            tasks.push(task);
        }
        tasks.extend(std::mem::take(&mut self.pending_removal_tasks));

        async move {
            futures::future::join_all(tasks).await;
        }
    }

    pub fn focus_active_workspace(&self, window: &mut Window, cx: &mut App) {
        // If a dock panel is zoomed, focus it instead of the center pane.
        // Otherwise, focusing the center pane triggers dismiss_zoomed_items_to_reveal
        // which closes the zoomed dock.
        let focus_handle = {
            let workspace = self.workspace().read(cx);
            let mut target = None;
            for dock in workspace.all_docks() {
                let dock = dock.read(cx);
                if dock.is_open() {
                    if let Some(panel) = dock.active_panel() {
                        if panel.is_zoomed(window, cx) {
                            target = Some(panel.panel_focus_handle(cx));
                            break;
                        }
                    }
                }
            }
            target.unwrap_or_else(|| {
                let pane = workspace.active_pane().clone();
                pane.read(cx).focus_handle(cx)
            })
        };
        window.focus(&focus_handle, cx);
    }

    pub fn panel<T: Panel>(&self, cx: &App) -> Option<Entity<T>> {
        self.workspace().read(cx).panel::<T>(cx)
    }

    pub fn active_modal<V: ManagedView + 'static>(&self, cx: &App) -> Option<Entity<V>> {
        self.workspace().read(cx).active_modal::<V>(cx)
    }

    pub fn add_panel<T: Panel>(
        &mut self,
        panel: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace().update(cx, |workspace, cx| {
            workspace.add_panel(panel, window, cx);
        });
    }

    pub fn focus_panel<T: Panel>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<T>> {
        self.workspace()
            .update(cx, |workspace, cx| workspace.focus_panel::<T>(window, cx))
    }

    // used in a test
    pub fn toggle_modal<V: ModalView, B>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: B,
    ) where
        B: FnOnce(&mut Window, &mut gpui::Context<V>) -> V,
    {
        self.workspace().update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, build);
        });
    }

    pub fn toggle_dock(
        &mut self,
        dock_side: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace().update(cx, |workspace, cx| {
            workspace.toggle_dock(dock_side, window, cx);
        });
    }

    pub fn active_item_as<I: 'static>(&self, cx: &App) -> Option<Entity<I>> {
        self.workspace().read(cx).active_item_as::<I>(cx)
    }

    pub fn items_of_type<'a, T: Item>(
        &'a self,
        cx: &'a App,
    ) -> impl 'a + Iterator<Item = Entity<T>> {
        self.workspace().read(cx).items_of_type::<T>(cx)
    }

    pub fn active_workspace_database_id(&self, cx: &App) -> Option<WorkspaceId> {
        self.workspace().read(cx).database_id()
    }

    pub fn take_pending_removal_tasks(&mut self) -> Vec<Task<()>> {
        let mut tasks: Vec<Task<()>> = std::mem::take(&mut self.pending_removal_tasks)
            .into_iter()
            .filter(|task| !task.is_ready())
            .collect();
        if let Some(task) = self._create_task.take() {
            if !task.is_ready() {
                tasks.push(task);
            }
        }
        tasks
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_random_database_id(&mut self, cx: &mut Context<Self>) {
        self.workspace().update(cx, |workspace, _cx| {
            workspace.set_random_database_id();
        });
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = cx.new(|cx| Workspace::test_new(project, window, cx));
        Self::new(workspace, window, cx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_add_workspace(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Workspace> {
        let workspace = cx.new(|cx| Workspace::test_new(project, window, cx));
        self.activate(workspace.clone(), cx);
        workspace
    }

    pub fn create_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !multi_workspace_enabled(cx) {
            return;
        }
        let app_state = self.workspace().read(cx).app_state().clone();
        let project = Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags::default(),
            cx,
        );
        let new_workspace = cx.new(|cx| Workspace::new(None, project, app_state, window, cx));
        self.set_active_workspace(new_workspace.clone(), cx);
        self.focus_active_workspace(window, cx);

        let weak_workspace = new_workspace.downgrade();
        self._create_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = crate::persistence::DB.next_id().await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(workspace_id) => {
                    if let Some(workspace) = weak_workspace.upgrade() {
                        let session_id = workspace.read(cx).session_id();
                        let window_id = window.window_handle().window_id().as_u64();
                        workspace.update(cx, |workspace, _cx| {
                            workspace.set_database_id(workspace_id);
                        });
                        cx.background_spawn(async move {
                            crate::persistence::DB
                                .set_session_binding(workspace_id, session_id, Some(window_id))
                                .await
                                .log_err();
                        })
                        .detach();
                    } else {
                        cx.background_spawn(async move {
                            crate::persistence::DB
                                .delete_workspace_by_id(workspace_id)
                                .await
                                .log_err();
                        })
                        .detach();
                    }
                    this.serialize(cx);
                }
                Err(error) => {
                    log::error!("Failed to create workspace: {error:#}");
                    if let Some(index) = weak_workspace
                        .upgrade()
                        .and_then(|w| this.workspaces.iter().position(|ws| *ws == w))
                    {
                        this.remove_workspace(index, window, cx);
                    }
                    this.workspace().update(cx, |workspace, cx| {
                        let id = NotificationId::unique::<MultiWorkspace>();
                        workspace.show_toast(
                            Toast::new(id, format!("Failed to create workspace: {error}")),
                            cx,
                        );
                    });
                }
            })
            .log_err();
        }));
    }

    pub fn remove_workspace(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces.len() <= 1 || index >= self.workspaces.len() {
            return;
        }

        let removed_workspace = self.workspaces.remove(index);
        if index < self.expanded_folders.len() {
            self.expanded_folders.remove(index);
        }

        if self.active_workspace_index >= self.workspaces.len() {
            self.active_workspace_index = self.workspaces.len() - 1;
        } else if self.active_workspace_index > index {
            self.active_workspace_index -= 1;
        }

        if let Some(workspace_id) = removed_workspace.read(cx).database_id() {
            self.pending_removal_tasks.retain(|task| !task.is_ready());
            self.pending_removal_tasks
                .push(cx.background_spawn(async move {
                    crate::persistence::DB
                        .delete_workspace_by_id(workspace_id)
                        .await
                        .log_err();
                }));
        }

        self.serialize(cx);
        self.focus_active_workspace(window, cx);
        cx.emit(MultiWorkspaceEvent::WorkspaceRemoved(
            removed_workspace.entity_id(),
        ));
        cx.emit(MultiWorkspaceEvent::ActiveWorkspaceChanged);
        cx.notify();
    }

    pub fn open_project(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Workspace>>> {
        let workspace = self.workspace().clone();

        if multi_workspace_enabled(cx) {
            workspace.update(cx, |workspace, cx| {
                workspace.open_workspace_for_paths(true, paths, window, cx)
            })
        } else {
            cx.spawn_in(window, async move |_this, cx| {
                let should_continue = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.prepare_to_close(crate::CloseIntent::ReplaceWindow, window, cx)
                    })?
                    .await?;
                if should_continue {
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_workspace_for_paths(true, paths, window, cx)
                        })?
                        .await
                } else {
                    Ok(workspace)
                }
            })
        }
    }

    /// Opens each given folder path as its own workspace within this window,
    /// with a terminal auto-opened in each workspace's center pane.
    pub fn open_folders_as_workspaces(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app_state = self.workspace().read(cx).app_state().clone();
        let fs = app_state.fs.clone();

        cx.spawn_in(window, {
            async move |this, cx| {
                let mut canonical_paths = Vec::with_capacity(paths.len());
                for path in &paths {
                    let canonical = fs.canonicalize(path).await.unwrap_or_else(|_| path.clone());
                    canonical_paths.push(canonical);
                }

                for (folder_index, folder_path) in canonical_paths.into_iter().enumerate() {
                    let app_state = app_state.clone();
                    let folder_path_for_worktree = folder_path.clone();

                    this.update_in(cx, |this, window, cx| {
                        let project = Project::local(
                            app_state.client.clone(),
                            app_state.node_runtime.clone(),
                            app_state.user_store.clone(),
                            app_state.languages.clone(),
                            app_state.fs.clone(),
                            None,
                            project::LocalProjectFlags::default(),
                            cx,
                        );

                        project.update(cx, |project, cx| {
                            project.find_or_create_worktree(folder_path_for_worktree, true, cx)
                        }).detach_and_log_err(cx);

                        let new_workspace = cx.new(|cx| {
                            Workspace::new(None, project, app_state, window, cx)
                        });

                        if folder_index == 0 {
                            this.set_active_workspace(new_workspace.clone(), cx);
                        } else {
                            this.add_workspace(new_workspace.clone(), cx);
                        }

                        // Dispatch NewCenterTerminal on the new workspace to auto-open
                        // a terminal with the folder as cwd.
                        new_workspace.update(cx, |workspace, cx| {
                            let pane = workspace.active_pane().clone();
                            let focus = pane.read(cx).focus_handle(cx);
                            focus.dispatch_action(
                                &crate::NewCenterTerminal { local: false },
                                window,
                                cx,
                            );
                        });
                    })?;
                }

                this.update_in(cx, |this, window, cx| {
                    this.sidebar_visible = this.workspaces.len() > 1;
                    this.focus_active_workspace(window, cx);
                    cx.notify();
                })?;

                anyhow::Ok(())
            }
        })
        .detach_and_log_err(cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleWorkspaceSidebar, _window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    fn toggle_folder_expanded(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(expanded) = self.expanded_folders.get_mut(index) {
            *expanded = !*expanded;
            cx.notify();
        }
    }

    fn folder_name_for_workspace(workspace: &Workspace, cx: &App) -> SharedString {
        let project = workspace.project().read(cx);
        if let Some(worktree) = project.worktrees(cx).next() {
            worktree.read(cx).root_name_str().to_string().into()
        } else {
            "Untitled".into()
        }
    }

    fn branch_for_workspace(workspace: &Workspace, cx: &App) -> Option<SharedString> {
        let project = workspace.project().read(cx);
        let repository = project.active_repository(cx)?;
        let repo = repository.read(cx);
        repo.branch.as_ref().map(|branch| branch.name().to_string().into())
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let surface_bg = cx.theme().colors().surface_background;
        let border_color = cx.theme().colors().border;
        let text_muted = cx.theme().colors().text_muted;
        let selected_bg = cx.theme().colors().ghost_element_selected;
        let hover_bg = cx.theme().colors().ghost_element_hover;
        let text_color = cx.theme().colors().text;

        // Phase 1: collect all data while borrowing cx immutably
        struct FolderData {
            folder_name: SharedString,
            branch_name: Option<SharedString>,
            is_active: bool,
            is_expanded: bool,
            children: Vec<(SharedString, bool)>,
        }

        let mut folder_data = Vec::new();
        for (index, workspace) in self.workspaces.iter().enumerate() {
            let is_active = index == self.active_workspace_index;
            let is_expanded = self.expanded_folders.get(index).copied().unwrap_or(true);

            let folder_name = workspace.read_with(cx, |workspace, cx| {
                Self::folder_name_for_workspace(workspace, cx)
            });
            let branch_name = workspace.read_with(cx, |workspace, cx| {
                Self::branch_for_workspace(workspace, cx)
            });

            let children: Vec<(SharedString, bool)> = if is_expanded {
                workspace.read_with(cx, |workspace, cx| {
                    let project = workspace.project().read(cx);
                    let mut entries = Vec::new();
                    for worktree in project.worktrees(cx) {
                        let worktree = worktree.read(cx);
                        for entry in worktree.entries(false, 0) {
                            let depth = entry.path.components().count();
                            if depth <= 1 {
                                let name: SharedString = entry
                                    .path
                                    .file_name()
                                    .unwrap_or(worktree.root_name_str())
                                    .to_string()
                                    .into();
                                entries.push((name, entry.is_dir()));
                            }
                        }
                    }
                    entries
                })
            } else {
                Vec::new()
            };

            folder_data.push(FolderData {
                folder_name,
                branch_name,
                is_active,
                is_expanded,
                children,
            });
        }

        // Phase 2: build elements using cx.listener for interactivity
        let folder_entries: Vec<Div> = folder_data
            .into_iter()
            .enumerate()
            .map(|(index, data)| {
                let chevron: &str = if data.is_expanded { "v" } else { ">" };

                let header = div()
                    .id(ElementId::named_usize("folder-header", index))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(px(4.0))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(data.is_active, |this| this.bg(selected_bg))
                    .hover(|style| style.bg(hover_bg))
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        this.activate_index(index, window, cx);
                    }))
                    .child(
                        div()
                            .id(ElementId::named_usize("folder-chevron", index))
                            .text_xs()
                            .text_color(text_muted)
                            .mr_1()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.toggle_folder_expanded(index, cx);
                            }))
                            .child(chevron),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(text_color)
                            .child(data.folder_name),
                    )
                    .when_some(data.branch_name, |this, branch| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(text_muted)
                                .ml_auto()
                                .child(branch),
                        )
                    });

                let mut folder_div = div().flex().flex_col().child(header);
                if data.is_expanded && !data.children.is_empty() {
                    folder_div = folder_div.child(
                        div().flex().flex_col().pl(px(16.0)).children(
                            data.children.into_iter().enumerate().map(
                                move |(child_index, (name, is_dir))| {
                                    let entry_id = ElementId::named_usize(
                                        format!("entry-{index}"),
                                        child_index,
                                    );
                                    let prefix: SharedString =
                                        if is_dir { "📁 " } else { "📄 " }.into();
                                    div()
                                        .id(entry_id)
                                        .flex()
                                        .items_center()
                                        .px_2()
                                        .py(px(2.0))
                                        .text_sm()
                                        .text_color(text_color)
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(hover_bg))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .child(prefix)
                                                .child(name),
                                        )
                                },
                            ),
                        ),
                    );
                }
                folder_div
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .w(self.sidebar_width)
            .h_full()
            .bg(surface_bg)
            .border_r_1()
            .border_color(border_color)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(text_muted)
                            .child("WORKSPACES"),
                    ),
            )
            .child(
                div()
                    .id("workspace-sidebar-entries")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .children(folder_entries),
            )
    }
}

impl Render for MultiWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = theme::setup_ui_font(window, cx);
        let text_color = cx.theme().colors().text;
        let show_sidebar =
            self.sidebar_visible && self.workspaces.len() > 1 && multi_workspace_enabled(cx);

        let workspace = self.workspace().clone();
        let workspace_key_context = workspace.update(cx, |workspace, cx| workspace.key_context(cx));
        let root = workspace.update(cx, |workspace, cx| workspace.actions(h_flex(), window, cx));

        let content = if show_sidebar {
            div()
                .flex()
                .flex_1()
                .size_full()
                .overflow_hidden()
                .child(self.render_sidebar(cx))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .size_full()
                        .overflow_hidden()
                        .child(self.workspace().clone()),
                )
        } else {
            div()
                .flex()
                .flex_1()
                .size_full()
                .overflow_hidden()
                .child(self.workspace().clone())
        };

        client_side_decorations(
            root.key_context(workspace_key_context)
                .relative()
                .size_full()
                .font(ui_font)
                .text_color(text_color)
                .on_action(cx.listener(Self::close_window))
                .on_action(cx.listener(Self::toggle_sidebar))
                .on_action(
                    cx.listener(|this: &mut Self, _: &NewWorkspaceInWindow, window, cx| {
                        this.create_workspace(window, cx);
                    }),
                )
                .on_action(
                    cx.listener(|this: &mut Self, _: &NextWorkspaceInWindow, window, cx| {
                        this.activate_next_workspace(window, cx);
                    }),
                )
                .on_action(cx.listener(
                    |this: &mut Self, _: &PreviousWorkspaceInWindow, window, cx| {
                        this.activate_previous_workspace(window, cx);
                    },
                ))
                .child(content)
                .child(self.workspace().read(cx).modal_layer.clone()),
            window,
            cx,
            Tiling {
                left: false,
                ..Tiling::default()
            },
        )
    }
}
