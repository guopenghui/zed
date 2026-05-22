use editor::{Editor, SelectionEffects, scroll::Autoscroll};
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    App, Context, DismissEvent, Entity, ParentElement, Styled, Task, TaskExt, WeakEntity, Window,
    rems,
};
use picker::{Picker, PickerDelegate};
use project::{
    Project,
    bookmark_store::{BookmarkStore, ProjectBookmark},
};
use std::{ops::Range, sync::Arc};
use text::Point;
use ui::{
    Color, HighlightedLabel, LabelCommon, LabelSize, ListItem, ListItemSpacing, Toggleable, h_flex,
};
use util::ResultExt;
use workspace::Workspace;

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace.register_action(
                |workspace, _: &workspace::ToggleProjectBookmarks, window, cx| {
                    let project = workspace.project().clone();
                    let handle = cx.entity().downgrade();
                    workspace.toggle_modal(window, cx, move |window, cx| {
                        let delegate = ProjectBookmarksDelegate::new(handle, project);
                        Picker::uniform_list(delegate, window, cx).width(rems(34.))
                    })
                },
            );
        },
    )
    .detach();
}

pub type ProjectBookmarks = Entity<Picker<ProjectBookmarksDelegate>>;

#[derive(Clone, Debug)]
struct BookmarkMatch {
    bookmark: ProjectBookmark,
    name: String,
    display_path: String,
    filter_text: String,
    name_range: Range<usize>,
    path_range: Range<usize>,
    line_number: u32,
}

pub struct ProjectBookmarksDelegate {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    selected_match_index: usize,
    bookmarks: Vec<BookmarkMatch>,
    candidates: Vec<StringMatchCandidate>,
    matches: Vec<StringMatch>,
}

impl ProjectBookmarksDelegate {
    fn new(workspace: WeakEntity<Workspace>, project: Entity<Project>) -> Self {
        Self {
            workspace,
            project,
            selected_match_index: 0,
            bookmarks: Vec::new(),
            candidates: Vec::new(),
            matches: Vec::new(),
        }
    }

    fn set_bookmarks(&mut self, bookmarks: Vec<ProjectBookmark>, cx: &mut App) {
        let project = self.project.read(cx);
        self.bookmarks = bookmarks
            .into_iter()
            .map(|bookmark| {
                let project_path = project.project_path_for_absolute_path(&bookmark.abs_path, cx);
                let display_path = project_path
                    .as_ref()
                    .and_then(|path| project.short_full_path_for_project_path(path, cx))
                    .unwrap_or_else(|| bookmark.abs_path.to_string_lossy().to_string());
                let line_number = bookmark.row + 1;
                let normalized_path = display_path.replace('\\', "/");

                let name_range = 0..bookmark.name.len();
                let mut filter_text = bookmark.name.clone();
                if !filter_text.is_empty() {
                    filter_text.push(' ');
                }
                let path_start = filter_text.len();
                filter_text.push_str(&normalized_path);
                let path_range = path_start..filter_text.len();

                BookmarkMatch {
                    name: bookmark.name.clone(),
                    bookmark,
                    display_path,
                    filter_text,
                    name_range,
                    path_range,
                    line_number,
                }
            })
            .collect();
        self.candidates = self
            .bookmarks
            .iter()
            .enumerate()
            .map(|(ix, bookmark)| StringMatchCandidate::new(ix, &bookmark.filter_text))
            .collect();
    }

    fn filter(&mut self, query: &str, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        const MAX_MATCHES: usize = 100;
        self.matches = if query.is_empty() {
            self.candidates
                .iter()
                .take(MAX_MATCHES)
                .map(|candidate| StringMatch {
                    candidate_id: candidate.id,
                    score: 0.0,
                    positions: Vec::new(),
                    string: candidate.string.clone(),
                })
                .collect()
        } else {
            cx.foreground_executor().block_on(fuzzy::match_strings(
                &self.candidates,
                query,
                false,
                true,
                MAX_MATCHES,
                &Default::default(),
                cx.background_executor().clone(),
            ))
        };
        self.matches.sort_unstable_by(|left, right| {
            self.bookmarks[left.candidate_id]
                .display_path
                .cmp(&self.bookmarks[right.candidate_id].display_path)
                .then_with(|| {
                    self.bookmarks[left.candidate_id]
                        .line_number
                        .cmp(&self.bookmarks[right.candidate_id].line_number)
                })
        });
        self.set_selected_index(0, window, cx);
    }

    fn highlight_positions_for_range(
        string_match: &StringMatch,
        range: Range<usize>,
    ) -> Vec<usize> {
        string_match
            .positions
            .iter()
            .filter_map(|position| {
                if range.contains(position) {
                    Some(position - range.start)
                } else {
                    None
                }
            })
            .collect()
    }
}

impl PickerDelegate for ProjectBookmarksDelegate {
    type ListItem = ListItem;

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search project bookmarks...".into()
    }

    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(bookmark) = self
            .matches
            .get(self.selected_match_index)
            .and_then(|mat| self.bookmarks.get(mat.candidate_id))
            .cloned()
        else {
            return;
        };

        let workspace = self.workspace.clone();
        let bookmark_store = self.project.read(cx).bookmark_store();
        cx.spawn_in(window, async move |_, cx| {
            let buffer = BookmarkStore::open_project_bookmark_buffer(
                bookmark_store,
                bookmark.bookmark.abs_path.clone(),
                cx,
            )
            .await?;
            workspace.update_in(cx, |workspace, window, cx| {
                let pane = if secondary {
                    workspace.adjacent_pane(window, cx)
                } else {
                    workspace.active_pane().clone()
                };

                let editor = workspace
                    .open_project_item::<Editor>(pane, buffer, true, true, true, true, window, cx);

                editor.update(cx, |editor, cx| {
                    let multibuffer_snapshot = editor.buffer().read(cx).snapshot(cx);
                    let Some(buffer_snapshot) = multibuffer_snapshot.as_singleton() else {
                        return;
                    };
                    let point = Point::new(bookmark.bookmark.row, 0);
                    if point > buffer_snapshot.max_point() {
                        return;
                    }
                    let text_anchor = buffer_snapshot.anchor_before(point);
                    let Some(anchor) = multibuffer_snapshot.anchor_in_buffer(text_anchor) else {
                        return;
                    };
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()),
                        window,
                        cx,
                        |s| s.select_ranges([anchor..anchor]),
                    );
                });
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_match_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_match_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let bookmark_store = self.project.read(cx).bookmark_store();
        let bookmarks = BookmarkStore::all_project_bookmarks(bookmark_store, cx)
            .log_err()
            .unwrap_or_default();
        self.set_bookmarks(bookmarks, cx);
        self.filter(&query, window, cx);
        Task::ready(())
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let string_match = self.matches.get(ix)?;
        let bookmark = self.bookmarks.get(string_match.candidate_id)?;
        let name_positions =
            Self::highlight_positions_for_range(string_match, bookmark.name_range.clone());
        let path_positions =
            Self::highlight_positions_for_range(string_match, bookmark.path_range.clone());
        let path = format!("{}:{}", bookmark.display_path, bookmark.line_number);
        let mut row = h_flex().gap_2().py_px();
        if !bookmark.name.is_empty() {
            row = row.child(HighlightedLabel::new(bookmark.name.clone(), name_positions));
        }

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    row.child(
                        HighlightedLabel::new(path, path_positions)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualContext};
    use project::FakeFs;
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use workspace::MultiWorkspace;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
        });
    }

    #[gpui::test]
    async fn test_project_bookmarks_filters_by_name(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/dir"),
            json!({
                "src": {
                    "alpha.rs": "unique body\nline\n",
                },
                "beta.rs": "alpha appears in text\nline\n",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/dir").as_ref()], cx).await;
        add_bookmark(&project, path!("/dir/src/alpha.rs"), 0, "first mark", cx).await;
        add_bookmark(&project, path!("/dir/beta.rs"), 0, "beta mark", cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());

        let picker = cx.new_window_entity(|window, cx| {
            Picker::uniform_list(
                ProjectBookmarksDelegate::new(workspace.downgrade(), project.clone()),
                window,
                cx,
            )
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches(String::new(), window, cx);
        });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.delegate.matches.len(), 2);
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("beta".to_string(), window, cx);
        });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.delegate.matches.len(), 1);
            let bookmark = &picker.delegate.bookmarks[picker.delegate.matches[0].candidate_id];
            assert!(bookmark.display_path.ends_with("beta.rs"));
            assert_eq!(bookmark.name, "beta mark");
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("beta.rs".to_string(), window, cx);
        });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.delegate.matches.len(), 1);
            let bookmark = &picker.delegate.bookmarks[picker.delegate.matches[0].candidate_id];
            assert!(bookmark.display_path.ends_with("beta.rs"));
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("src/alpha.rs".to_string(), window, cx);
        });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.delegate.matches.len(), 1);
            let bookmark = &picker.delegate.bookmarks[picker.delegate.matches[0].candidate_id];
            assert!(bookmark.display_path.ends_with("alpha.rs"));
        });
    }

    #[gpui::test]
    async fn test_project_bookmarks_confirm_opens_bookmark(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/dir"), json!({"alpha.rs": "row0\nrow1\nrow2\n"}))
            .await;
        let project = Project::test(fs, [path!("/dir").as_ref()], cx).await;
        add_bookmark(&project, path!("/dir/alpha.rs"), 2, "target", cx).await;

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        let picker = cx.new_window_entity(|window, cx| {
            Picker::uniform_list(
                ProjectBookmarksDelegate::new(workspace.downgrade(), project.clone()),
                window,
                cx,
            )
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches(String::new(), window, cx);
        });
        cx.run_until_parked();
        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.confirm(false, window, cx);
        });
        cx.run_until_parked();

        let editor = workspace
            .read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx))
            .expect("bookmark should open an editor");
        editor.update_in(cx, |editor, _window, cx| {
            let cursor = editor
                .selections
                .newest::<Point>(&editor.display_snapshot(cx))
                .head();
            assert_eq!(cursor.row, 2);
            assert_eq!(cursor.column, 0);
        });
    }

    async fn add_bookmark(
        project: &Entity<Project>,
        path: &str,
        row: u32,
        name: &str,
        cx: &mut TestAppContext,
    ) {
        let buffer = project
            .update(cx, |project, cx| project.open_local_buffer(path, cx))
            .await
            .unwrap();
        project.update(cx, |project, cx| {
            let snapshot = buffer.read(cx).snapshot();
            let anchor = snapshot.anchor_after(Point::new(row, 0));
            project.bookmark_store().update(cx, |store, cx| {
                store.toggle_bookmark(buffer, anchor, name.to_string(), cx);
            });
        });
    }
}
