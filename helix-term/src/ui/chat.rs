use helix_core::{find_workspace, movement::Direction, Position, Transaction};
use helix_event::request_redraw;
use helix_view::{
    chat::{
        self,
        chat_state::{ChatState, Message},
    },
    graphics::{CursorKind, Modifier, Rect},
    theme::Style,
    Editor, ViewId,
};
use tui::{
    buffer::Buffer as Surface,
    widgets::{Block, BorderType, Widget},
};

use crate::{
    compositor::{self, Component, Compositor, Context, Event, EventResult},
    ctrl,
    job::Callback,
    key, shift,
    ui::{overlay::Overlay, Popup},
};

use crate::ui::markdown::Markdown;
use std::{path::Path, sync::Arc};

use super::{completers, Prompt, PromptEvent};

/// A popup like the Picker ui element but for a AI/copilot chat
///
/// Has the input bar on the bottom and displays a chat history in the main area
pub struct Chat {
    state: Option<ChatState>,
    is_quick: bool,
    view_id: ViewId,

    cursor: u32,
    completion_height: u16,
    prompt: Prompt,
}

impl Chat {
    pub fn new_global(ed: &mut Editor) -> Self {
        let (view, document) = current!(ed);
        let current_context = chat::context::Context::document(document.id());
        let prompt = Prompt::new(
            "".into(),
            None,
            completers::none,
            |_editor: &mut Context, _pattern: &str, _event: PromptEvent| {},
        );
        let mut chat = Self {
            state: None,
            prompt,
            cursor: 0,
            completion_height: 0,
            is_quick: false,
            view_id: view.id,
        };
        let state = chat.state_mut(ed);
        state.context.clear();
        state.context.push(current_context);
        state.context.push(chat::context::Context::Selection);

        chat
    }

    /// A new quick chat
    ///
    /// Quick chats:
    /// - Do not have any persistent history, when you close them they are gone
    /// - Include the current selection in the context
    /// - directly applies the edit.
    pub fn new_quick(ed: &mut Editor) -> Self {
        let (view, doc) = current!(ed);
        let current_context = chat::context::Context::document(doc.id());
        let prompt = Prompt::new(
            "".into(),
            None,
            completers::none,
            |_editor: &mut Context, _pattern: &str, _event: PromptEvent| {},
        );
        let mut chat = Self {
            state: Some(ChatState::default()),
            prompt,
            cursor: 0,
            completion_height: 0,
            is_quick: true,
            view_id: view.id,
        };
        let state = chat.state_mut(ed);
        state.context.push(current_context);
        state.context.push(chat::context::Context::Selection);

        chat
    }
    fn state_mut<'a>(&'a mut self, editor: &'a mut Editor) -> &'a mut ChatState {
        match &mut self.state {
            Some(state) => state,
            None => editor.chat_state_mut(),
        }
    }

    fn state<'a>(&'a self, editor: &'a Editor) -> &'a ChatState {
        match &self.state {
            Some(state) => state,
            None => editor.chat_state(),
        }
    }

    fn submit_text(&mut self, cx: &mut Context) {
        let text = self.prompt.line().to_owned();
        self.prompt.set_line("".to_owned(), cx.editor);
        if text.starts_with("/") {
            match text.as_str() {
                "/clear" => self.state_mut(cx.editor).history.clear(),
                _ => cx.editor.set_error("unknown cmd"),
            }
            return;
        }

        let is_quick = self.is_quick;
        let state = self.state_mut(cx.editor);
        state.history.push(Message {
            content: text.to_owned(),
            role: "user".to_owned(),
        });
        let (send, recv) = tokio::sync::mpsc::channel(1024);
        state.in_progress = Some(chat::chat_state::InProgressState {
            channel: recv,
            ticks: 0,
        });
        let prompt = if is_quick {
            chat::config::prompts::quick_copilot_instructions()
        } else {
            chat::config::prompts::copilot_instructions()
        };
        let state = self.state(&cx.editor);
        let lines = match chat::prompt::format_prompt(
            &cx.editor,
            &prompt,
            &state.history,
            &state.context,
        ) {
            Ok(lines) => lines,
            Err(err) => {
                cx.editor
                    .set_error(format!("could not render prompt {err:?}"));
                return;
            }
        };
        let state = self.state_mut(cx.editor);
        state.history.push(Message {
            content: "".to_owned(),
            role: "system".to_owned(),
        });
        let client = state.client.clone();
        cx.jobs.callback(async move {
            log::info!("start callback");
            client
                .send_chat(&lines, |text| async {
                    if send.send(text).await.is_err() {
                        return false;
                    }
                    request_redraw();
                    true
                })
                .await;

            Ok(Callback::EditorCompositor(Box::new(
                move |editor, composor| {
                    let chat_window = if is_quick {
                        let Some(layer) = composor.find_id::<Popup<Chat>>("aichat") else {
                            log::error!("no chat window found");
                            return;
                        };
                        layer.contents_mut()
                    } else {
                        let Some(layer) = composor.find_id::<Overlay<Chat>>("aichat") else {
                            log::error!("no chat window found");
                            return;
                        };
                        &mut layer.content
                    };

                    // make sure to empty the buffer
                    chat_window.state_mut(editor).fetch_inprogress();
                    chat_window.state_mut(editor).in_progress = None;
                    editor.set_status("finished ai response");

                    if chat_window.is_quick {
                        chat_window.apply_last_change(editor);
                    }
                },
            )))
        })
    }

    fn apply_last_change(&mut self, editor: &mut Editor) {
        let new_texts = self.state(editor).get_last_code_changes();
        if new_texts.len() == 0 {
            editor.set_error("no ai code block found");
            return;
        }
        let (path, start_line, end_line, new_text) = &new_texts[0];
        let mut path = Path::new(path).to_owned();
        let root = find_workspace().0;
        if path.is_relative() {
            path = root.join(path)
        }
        let doc = editor.document_by_path_mut(path).unwrap();
        let end_line = (*end_line).min(doc.text().len_lines() - 1);
        let start = doc.text().line_to_char(*start_line);
        let end = doc.text().line_to_char(end_line) + doc.text().line(end_line).len_chars() - 1;

        let transaction = Transaction::change(
            doc.text(),
            [(start, end, Some(new_text.into()))].into_iter(),
        );
        doc.apply(&transaction, self.view_id);
    }

    fn render_picker(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let _text_style = cx.editor.theme.get("ui.text");
        let _selected = cx.editor.theme.get("ui.text.focus");
        let _highlight_style = cx.editor.theme.get("special").add_modifier(Modifier::BOLD);

        // -- Render the frame:
        // clear area
        let background = cx.editor.theme.get("ui.background");
        surface.clear_with(area, background);

        const BLOCK: Block<'_> = Block::bordered();

        // calculate the inner area inside the box
        let inner = BLOCK.inner(area);

        BLOCK.render(area, surface);

        // -- Render the input bar:
        let line_area = Rect {
            x: inner.x + 1,
            y: inner.y + inner.height - 2,
            width: inner.width - 2,
            height: 2,
        };

        if let Some(progress) = &self.state(&cx.editor).in_progress {
            let frames = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
            let frame = frames[progress.ticks % frames.len()];
            surface.set_string(line_area.x, line_area.y + 1, frame, Style::default());
        } else {
            // render the prompt first since it will clear its background
            self.prompt.render(line_area, surface, cx);
        }

        // -- Separator
        let sep_style = cx.editor.theme.get("ui.background.separator");
        let borders = BorderType::line_symbols(BorderType::Plain);
        for x in inner.left()..inner.right() {
            if let Some(cell) = surface.get_mut(x, inner.y + inner.height - 2) {
                cell.set_symbol(borders.horizontal).set_style(sep_style);
            }
        }

        // -- Render the contents:
        // subtract area of prompt from top
        let inner = inner.clip_bottom(2);
        let rows = inner.height as u32;
        let offset = self.cursor - (self.cursor % std::cmp::max(1, rows));
        let _cursor = self.cursor.saturating_sub(offset);
        let _end = offset.saturating_add(rows);
        // .min(snapshot.matched_item_count());

        self.render_history(inner, surface, cx)
    }

    fn render_history(&self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let mut markdown = Markdown::new(
            self.state(cx.editor)
                .history
                .iter()
                .map(|item| {
                    let prefix = if item.role == "system" {
                        "=== system\n"
                    } else {
                        "=== user\n"
                    };
                    format!("{}{}", prefix, item.content)
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
            Arc::clone(&cx.editor.syn_loader),
        );

        let md_size = markdown.required_size((area.width, area.height));
        if let Some(size) = md_size {
            cx.scroll = Some(size.1.saturating_sub(area.height) as usize);
        }
        markdown.render(area, surface, cx);
    }

    fn prompt_handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        if self.state(&cx.editor).in_progress.is_some() {
            return EventResult::Consumed(None);
        }
        if let EventResult::Consumed(_) = self.prompt.handle_event(event, cx) {
            // self.handle_prompt_change(matches!(event, Event::Paste(_)));
        }
        EventResult::Consumed(None)
    }

    /// Move the cursor by a number of lines, either down (`Forward`) or up (`Backward`)
    pub fn move_by(&mut self, amount: u32, direction: Direction) {
        let len = self.state.as_ref().map_or(0, |state| state.history.len()) as u32;

        if len == 0 {
            // No results, can't move.
            return;
        }

        match direction {
            Direction::Forward => {
                self.cursor = self.cursor.saturating_add(amount) % len;
            }
            Direction::Backward => {
                self.cursor = self.cursor.saturating_add(len).saturating_sub(amount) % len;
            }
        }
    }

    /// Move the cursor down by exactly one page. After the last page comes the first page.
    pub fn page_up(&mut self) {
        self.move_by(self.completion_height as u32, Direction::Backward);
    }

    /// Move the cursor up by exactly one page. After the first page comes the last page.
    pub fn page_down(&mut self) {
        self.move_by(self.completion_height as u32, Direction::Forward);
    }

    /// Move the cursor to the first entry
    pub fn to_start(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the last entry
    pub fn to_end(&mut self) {
        self.cursor = self
            .state
            .as_ref()
            .map_or(0, |state| state.history.len().saturating_sub(1)) as u32;
    }
}

impl Component for Chat {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let state = self.state_mut(cx.editor);
        state.fetch_inprogress();
        self.render_picker(area, surface, cx)
    }

    fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        if self.state(editor).in_progress.is_some() {
            return (None, CursorKind::Hidden);
        }
        let block = Block::bordered();
        // calculate the inner area inside the box
        let inner = block.inner(area);
        let area = Rect {
            x: inner.x + 1,
            y: inner.y + inner.height - 2,
            width: inner.width,
            height: 2,
        };

        self.prompt.cursor(area, editor)
    }

    fn handle_event(&mut self, event: &Event, ctx: &mut Context) -> EventResult {
        let key_event = match event {
            Event::Key(event) => *event,
            Event::Paste(..) => return self.prompt_handle_event(event, ctx),
            Event::Resize(..) => return EventResult::Consumed(None),
            _ => return EventResult::Ignored(None),
        };

        let close_fn = |_picker: &mut Self| {
            // if the picker is very large don't store it as last_picker to avoid
            // excessive memory consumption
            let callback: compositor::Callback = Box::new(|compositor: &mut Compositor, _ctx| {
                // remove the layer
                compositor.pop();
            });
            EventResult::Consumed(Some(callback))
        };

        match key_event {
            ctrl!('y') => self.apply_last_change(ctx.editor),
            shift!(Tab) | key!(Up) | ctrl!('p') => {
                self.move_by(1, Direction::Backward);
            }
            key!(Tab) | key!(Down) | ctrl!('n') => {
                self.move_by(1, Direction::Forward);
            }
            key!(PageDown) | ctrl!('d') => {
                self.page_down();
            }
            key!(PageUp) | ctrl!('u') => {
                self.page_up();
            }
            key!(Home) => {
                self.to_start();
            }
            key!(End) => {
                self.to_end();
            }
            key!(Esc) | ctrl!('c') => return close_fn(self),
            // alt!(Enter) => {
            //     if let Some(option) = self.selection() {
            //         (self.callback_fn)(ctx, option, Action::Replace);
            //     }
            // }
            key!(Enter) => {
                // If the prompt has a history completion and is empty, use enter to accept
                // that completion
                if let Some(completion) = self
                    .prompt
                    .first_history_completion(ctx.editor)
                    .filter(|_| self.prompt.line().is_empty())
                {
                    let completion = completion.into_owned();
                    self.prompt.set_line(completion, ctx.editor);

                    // Inserting from the history register is a paste.
                    // self.handle_prompt_change(true);
                } else {
                    self.submit_text(ctx);
                    // return close_fn(self);
                }
            }
            // ctrl!('s') => {
            //     if let Some(option) = self.selection() {
            //         (self.callback_fn)(ctx, option, Action::HorizontalSplit);
            //     }
            //     return close_fn(self);
            // }
            // ctrl!('v') => {
            //     if let Some(option) = self.selection() {
            //         (self.callback_fn)(ctx, option, Action::VerticalSplit);
            //     }
            //     return close_fn(self);
            // }
            // ctrl!('t') => {
            //     self.toggle_preview();
            // }
            _ => {
                self.prompt_handle_event(event, ctx);
            }
        }

        EventResult::Consumed(None)
    }

    fn required_size(&mut self, (width, height): (u16, u16)) -> Option<(u16, u16)> {
        self.completion_height = height.saturating_sub(4);

        let width = u16::min(width, 80) as u16;
        Some((width, 3))
    }

    fn id(&self) -> Option<&'static str> {
        Some("aichat")
    }
}
