use std::ops::Range;

use gpui::{
    AppContext as _, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Styled,
    Subscription, rems,
};
use multi_buffer::{Anchor, MultiBufferOffset, MultiBufferSnapshot, ToOffset as _};
use project::{Project, bookmark_store::BookmarkStore};
use rope::Point;
use text::Bias;
use ui::{
    ActiveTheme, Context, InteractiveElement, IntoElement, ParentElement, StyledExt, Window, div,
};
use util::ResultExt as _;
use workspace::{DismissDecision, ModalView, Workspace, searchable::Direction};

use crate::display_map::DisplayRow;
use crate::{
    EditBookmark, Editor, GoToNextBookmark, GoToPreviousBookmark, MultibufferSelectionMode,
    SelectionEffects, ToggleBookmark, ToggleNamedBookmark, ViewBookmarks, actions::SelectAll,
    scroll::Autoscroll,
};

#[derive(Clone)]
struct BookmarkTarget {
    buffer: Entity<language::Buffer>,
    anchor: text::Anchor,
}

#[derive(Clone, Copy)]
enum BookmarkNamePromptMode {
    Add,
    Edit,
}

pub struct BookmarkNamePrompt {
    name_editor: Entity<Editor>,
    active_editor: Entity<Editor>,
    targets: Vec<BookmarkTarget>,
    mode: BookmarkNamePromptMode,
    _subscriptions: Vec<Subscription>,
}

impl ModalView for BookmarkNamePrompt {
    fn on_before_dismiss(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> DismissDecision {
        DismissDecision::Dismiss(true)
    }
}

impl Focusable for BookmarkNamePrompt {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for BookmarkNamePrompt {}

impl BookmarkNamePrompt {
    fn new(
        active_editor: Entity<Editor>,
        targets: Vec<BookmarkTarget>,
        initial_name: String,
        select_initial_name: bool,
        mode: BookmarkNamePromptMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Bookmark name", window, cx);
            editor.set_text(initial_name.clone(), window, cx);
            if select_initial_name {
                editor.select_all(&SelectAll, window, cx);
            }
            editor
        });
        let name_editor_change = cx.subscribe_in(&name_editor, window, Self::on_name_editor_event);

        Self {
            name_editor,
            active_editor,
            targets,
            mode,
            _subscriptions: vec![name_editor_change],
        }
    }

    fn on_name_editor_event(
        &mut self,
        _: &Entity<Editor>,
        event: &crate::EditorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let crate::EditorEvent::Blurred = event {
            cx.emit(DismissEvent);
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_editor.read(cx).text(cx).trim().to_string();
        let targets = self.targets.clone();
        let mode = self.mode;
        self.active_editor.update(cx, |editor, cx| {
            if let Some(bookmark_store) = editor.bookmark_store.clone() {
                bookmark_store.update(cx, |store, cx| {
                    for target in targets {
                        match mode {
                            BookmarkNamePromptMode::Add => {
                                store.toggle_bookmark(
                                    target.buffer,
                                    target.anchor,
                                    name.clone(),
                                    cx,
                                );
                            }
                            BookmarkNamePromptMode::Edit => {
                                store.edit_bookmark(target.buffer, target.anchor, name.clone(), cx);
                            }
                        }
                    }
                });
            }
            editor.focus_handle(cx).focus(window, cx);
            cx.notify();
        });

        cx.emit(DismissEvent);
    }
}

impl Render for BookmarkNamePrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(rems(34.))
            .elevation_2(cx)
            .key_context("BookmarkNamePrompt")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .child(
                div()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .px_2()
                    .py_1()
                    .child(self.name_editor.clone()),
            )
    }
}

impl Editor {
    fn bookmark_exists_for_target(
        bookmark_store: &Entity<BookmarkStore>,
        target: &BookmarkTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        let buffer_snapshot = target.buffer.read(cx).snapshot();
        bookmark_store
            .update(cx, |bookmark_store, cx| {
                bookmark_store.bookmark_for_buffer_row(
                    target.buffer.clone(),
                    target.anchor,
                    &buffer_snapshot,
                    cx,
                )
            })
            .is_some()
    }

    fn prompt_for_bookmark_name_or_toggle(
        &mut self,
        targets: Vec<BookmarkTarget>,
        prompt_for_name: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt_opened = prompt_for_name
            && self.open_bookmark_name_prompt(
                targets.clone(),
                String::new(),
                false,
                BookmarkNamePromptMode::Add,
                window,
                cx,
            );

        if !prompt_opened {
            self.toggle_bookmarks(targets, String::new(), cx);
        }
    }

    fn toggle_bookmark_target(
        &mut self,
        target: BookmarkTarget,
        prompt_for_name: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bookmark_store) = self.bookmark_store.clone() else {
            return;
        };

        if Self::bookmark_exists_for_target(&bookmark_store, &target, cx) {
            self.toggle_bookmarks(vec![target], String::new(), cx);
        } else {
            self.prompt_for_bookmark_name_or_toggle(vec![target], prompt_for_name, window, cx);
        }

        cx.notify();
    }

    pub fn set_show_bookmarks(&mut self, show_bookmarks: bool, cx: &mut Context<Self>) {
        self.show_bookmarks = Some(show_bookmarks);
        cx.notify();
    }

    pub fn toggle_bookmark(
        &mut self,
        _: &ToggleBookmark,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_bookmarks_in_editor(false, window, cx);
    }

    pub fn toggle_named_bookmark(
        &mut self,
        _: &ToggleNamedBookmark,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_bookmarks_in_editor(true, window, cx);
    }

    fn toggle_bookmarks_in_editor(
        &mut self,
        prompt_for_name: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bookmark_store) = self.bookmark_store.clone() else {
            return;
        };
        let Some(project) = self.project().cloned() else {
            return;
        };

        let snapshot = self.snapshot(window, cx);
        let multi_buffer_snapshot = snapshot.buffer_snapshot();

        let mut selections = self.selections.all::<Point>(&snapshot.display_snapshot);
        selections.sort_by_key(|s| s.head());
        selections.dedup_by_key(|s| s.head().row);

        let mut bookmarks_to_add = Vec::new();

        for selection in &selections {
            let head = selection.head();
            let multibuffer_anchor = multi_buffer_snapshot.anchor_before(Point::new(head.row, 0));

            if let Some((buffer_anchor, _)) =
                multi_buffer_snapshot.anchor_to_buffer_anchor(multibuffer_anchor)
            {
                let buffer_id = buffer_anchor.buffer_id;
                if let Some(buffer) = project.read(cx).buffer_for_id(buffer_id, cx) {
                    let target = BookmarkTarget {
                        buffer,
                        anchor: buffer_anchor,
                    };

                    if Self::bookmark_exists_for_target(&bookmark_store, &target, cx) {
                        self.toggle_bookmarks(vec![target], String::new(), cx);
                    } else {
                        bookmarks_to_add.push(target);
                    }
                }
            }
        }

        if !bookmarks_to_add.is_empty() {
            self.prompt_for_bookmark_name_or_toggle(bookmarks_to_add, prompt_for_name, window, cx);
        }

        cx.notify();
    }

    pub fn toggle_bookmark_at_row(
        &mut self,
        row: DisplayRow,
        prompt_for_name: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_snapshot = self.display_snapshot(cx);
        let point = display_snapshot.display_point_to_point(row.as_display_point(), Bias::Left);
        let buffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let anchor = buffer_snapshot.anchor_before(point);

        self.toggle_bookmark_at_anchor(anchor, prompt_for_name, window, cx);
    }

    pub fn toggle_bookmark_at_anchor(
        &mut self,
        anchor: Anchor,
        prompt_for_name: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let Some((position, _)) = buffer_snapshot.anchor_to_buffer_anchor(anchor) else {
            return;
        };
        let Some(buffer) = self.buffer.read(cx).buffer(position.buffer_id) else {
            return;
        };

        self.toggle_bookmark_target(
            BookmarkTarget {
                buffer,
                anchor: position,
            },
            prompt_for_name,
            window,
            cx,
        );
    }

    pub fn edit_bookmark(&mut self, _: &EditBookmark, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(window, cx);
        let editor_buffer_snapshot = snapshot.buffer_snapshot();
        let selection = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        let multibuffer_anchor = editor_buffer_snapshot.anchor_before(Point::new(selection.row, 0));
        self.edit_bookmark_at_anchor(multibuffer_anchor, window, cx);
    }

    pub fn edit_bookmark_at_anchor(
        &mut self,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bookmark_store) = self.bookmark_store.clone() else {
            return;
        };
        let Some(project) = self.project() else {
            return;
        };

        let editor_buffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let Some((buffer_anchor, _)) = editor_buffer_snapshot.anchor_to_buffer_anchor(anchor)
        else {
            return;
        };
        let Some(buffer) = project.read(cx).buffer_for_id(buffer_anchor.buffer_id, cx) else {
            return;
        };
        let buffer_snapshot = buffer.read(cx).snapshot();
        let Some(bookmark) = bookmark_store.update(cx, |store, cx| {
            store.bookmark_for_buffer_row(buffer.clone(), buffer_anchor, &buffer_snapshot, cx)
        }) else {
            return;
        };

        self.open_bookmark_name_prompt(
            vec![BookmarkTarget {
                buffer,
                anchor: buffer_anchor,
            }],
            bookmark.name().to_string(),
            true,
            BookmarkNamePromptMode::Edit,
            window,
            cx,
        );
    }

    fn open_bookmark_name_prompt(
        &mut self,
        targets: Vec<BookmarkTarget>,
        initial_name: String,
        select_initial_name: bool,
        mode: BookmarkNamePromptMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(workspace) = self.workspace() else {
            return false;
        };
        let active_editor = cx.entity();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                BookmarkNamePrompt::new(
                    active_editor.clone(),
                    targets.clone(),
                    initial_name.clone(),
                    select_initial_name,
                    mode,
                    window,
                    cx,
                )
            });
        });
        true
    }

    fn toggle_bookmarks(
        &mut self,
        targets: Vec<BookmarkTarget>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(bookmark_store) = self.bookmark_store.clone() {
            bookmark_store.update(cx, |store, cx| {
                for target in targets {
                    store.toggle_bookmark(target.buffer, target.anchor, name.clone(), cx);
                }
            });
        }
    }

    pub fn go_to_next_bookmark(
        &mut self,
        _: &GoToNextBookmark,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_bookmark_impl(Direction::Next, window, cx);
    }

    pub fn go_to_previous_bookmark(
        &mut self,
        _: &GoToPreviousBookmark,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_bookmark_impl(Direction::Prev, window, cx);
    }

    fn go_to_bookmark_impl(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = &self.project else {
            return;
        };
        let Some(bookmark_store) = &self.bookmark_store else {
            return;
        };

        let selection = self
            .selections
            .newest::<MultiBufferOffset>(&self.display_snapshot(cx));
        let multi_buffer_snapshot = self.buffer.read(cx).snapshot(cx);

        let mut all_bookmarks = Self::bookmarks_in_range(
            MultiBufferOffset(0)..multi_buffer_snapshot.len(),
            &multi_buffer_snapshot,
            project,
            bookmark_store,
            cx,
        );
        all_bookmarks.sort_by_key(|a| a.to_offset(&multi_buffer_snapshot));

        let anchor = match direction {
            Direction::Next => all_bookmarks
                .iter()
                .find(|anchor| anchor.to_offset(&multi_buffer_snapshot) > selection.head())
                .or_else(|| all_bookmarks.first()),
            Direction::Prev => all_bookmarks
                .iter()
                .rfind(|anchor| anchor.to_offset(&multi_buffer_snapshot) < selection.head())
                .or_else(|| all_bookmarks.last()),
        }
        .cloned();

        if let Some(anchor) = anchor {
            self.unfold_ranges(&[anchor..anchor], true, false, cx);
            self.change_selections(
                SelectionEffects::scroll(Autoscroll::center()),
                window,
                cx,
                |s| {
                    s.select_anchor_ranges([anchor..anchor]);
                },
            );
        }
    }

    pub fn view_bookmarks(
        workspace: &mut Workspace,
        _: &ViewBookmarks,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let bookmark_store = workspace.project().read(cx).bookmark_store();
        cx.spawn_in(window, async move |workspace, cx| {
            let Some(locations) = BookmarkStore::all_bookmark_locations(bookmark_store, cx)
                .await
                .log_err()
            else {
                return;
            };

            workspace
                .update_in(cx, |workspace, window, cx| {
                    Editor::open_locations_in_multibuffer(
                        workspace,
                        locations,
                        "Bookmarks".into(),
                        false,
                        false,
                        MultibufferSelectionMode::First,
                        window,
                        cx,
                    );
                })
                .log_err();
        })
        .detach();
    }

    fn bookmarks_in_range(
        range: Range<MultiBufferOffset>,
        multi_buffer_snapshot: &MultiBufferSnapshot,
        project: &Entity<Project>,
        bookmark_store: &Entity<BookmarkStore>,
        cx: &mut Context<Self>,
    ) -> Vec<Anchor> {
        multi_buffer_snapshot
            .range_to_buffer_ranges(range)
            .into_iter()
            .flat_map(|(buffer_snapshot, buffer_range, _excerpt_range)| {
                let Some(buffer) = project
                    .read(cx)
                    .buffer_for_id(buffer_snapshot.remote_id(), cx)
                else {
                    return Vec::new();
                };
                bookmark_store
                    .update(cx, |store, cx| {
                        store.bookmarks_for_buffer(
                            buffer,
                            buffer_snapshot.anchor_before(buffer_range.start)
                                ..buffer_snapshot.anchor_after(buffer_range.end),
                            &buffer_snapshot,
                            cx,
                        )
                    })
                    .into_iter()
                    .filter_map(|bookmark| {
                        multi_buffer_snapshot.anchor_in_buffer(bookmark.anchor())
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}
