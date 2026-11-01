//! The Textport panel: an interactive Python console over the open project
//! (FR-P5, `docs/textport.md`).
//!
//! Python runs on the textport's worker ([`lsr_py::textport`]), never on
//! this thread: Enter only queues the text, and output arrives on later
//! frames. Parameter changes the console asks for are applied here, once per
//! frame, as undoable edits.

use std::path::Path;
use std::time::Duration;

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, Ui};
use lsr_gpu::engine::Engine;
use lsr_py::textport::{Event, Stream, Textport};

/// Lines kept in the output; older ones are dropped.
const MAX_LINES: usize = 5000;
/// Entries kept in the input history.
const MAX_HISTORY: usize = 500;
const ERROR: Color32 = Color32::from_rgb(230, 80, 80);

/// What a line of output is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// What the user typed, with its prompt.
    Input,
    Out,
    Err,
    /// An expression's value.
    Result,
    /// A message from the panel itself.
    Note,
}

/// The output, as lines.
#[derive(Debug, Default)]
pub struct Log {
    lines: Vec<(Kind, String)>,
    /// The last line has no newline yet: more of the same kind continues it.
    open: bool,
}

impl Log {
    /// Appends `text`, splitting it into lines.
    pub fn push(&mut self, kind: Kind, text: &str) {
        for (i, part) in text.split('\n').enumerate() {
            let continues = i == 0 && self.open && self.lines.last().is_some_and(|l| l.0 == kind);
            if continues {
                if let Some(last) = self.lines.last_mut() {
                    last.1.push_str(part);
                }
            } else {
                self.lines.push((kind, part.to_owned()));
            }
        }
        // "a\n" splits into "a" and "": the empty part is where the next
        // write starts, not a line.
        self.open = !text.ends_with('\n');
        if !self.open {
            self.lines.pop();
        }
        if self.lines.len() > MAX_LINES {
            self.lines.drain(..self.lines.len() - MAX_LINES);
        }
    }

    /// Echoes an entry: `>>> ` before its first line (`... ` when it
    /// continues a statement) and `... ` before the others.
    pub fn echo(&mut self, text: &str, continues: bool) {
        self.open = false;
        for (i, line) in text.split('\n').enumerate() {
            let prompt = if i == 0 && !continues { ">>> " } else { "... " };
            self.lines.push((Kind::Input, format!("{prompt}{line}")));
        }
    }

    pub fn note(&mut self, text: &str) {
        self.open = false;
        self.push(Kind::Note, &format!("{text}\n"));
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.open = false;
    }

    pub fn lines(&self) -> &[(Kind, String)] {
        &self.lines
    }
}

/// Earlier entries, browsed with Up and Down.
#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    /// The entry shown while browsing.
    at: Option<usize>,
    /// What was typed before browsing started.
    draft: String,
}

impl History {
    pub fn add(&mut self, text: &str) {
        self.at = None;
        if text.trim().is_empty() || self.entries.last().is_some_and(|l| l == text) {
            return;
        }
        self.entries.push(text.to_owned());
        if self.entries.len() > MAX_HISTORY {
            self.entries.remove(0);
        }
    }

    /// Up (`older`) or Down: replaces `input` with the next entry. Down
    /// past the newest entry restores what was typed.
    pub fn step(&mut self, input: &mut String, older: bool) {
        let n = self.entries.len();
        let next = match (self.at, older) {
            (None, true) if n > 0 => {
                self.draft = std::mem::take(input);
                Some(n - 1)
            }
            (None, _) => return,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < n => Some(i + 1),
            (Some(_), false) => None,
        };
        self.at = next;
        *input = match next {
            Some(i) => self.entries[i].clone(),
            None => std::mem::take(&mut self.draft),
        };
    }
}

/// Which project the console belongs to: (projects opened so far, whether
/// its Python operators may run).
type ProjectKey = (u64, bool);

/// The Textport panel's state.
#[derive(Debug, Default)]
pub struct TextportPanel {
    /// Started the first time the panel shows.
    host: Option<Textport>,
    failed: Option<String>,
    log: Log,
    input: String,
    history: History,
    events: Vec<Event>,
    project: Option<ProjectKey>,
    /// The panel's Close button was pressed (the app removes the tab).
    pub close: bool,
}

impl TextportPanel {
    /// Once per app frame, after cooking: follows project changes, applies
    /// the console's parameter changes, and takes its output. Returns
    /// whether an entry is running (keep frames coming).
    pub fn frame(
        &mut self,
        engine: &mut Engine,
        time: f64,
        opened: u64,
        dir: Option<&Path>,
        allowed: bool,
    ) -> bool {
        let Some(host) = &mut self.host else {
            return false;
        };
        let key = (opened, allowed);
        if self.project != Some(key) {
            match self.project {
                Some((o, _)) if o == opened => {
                    host.allow_ops();
                    self.log.note(
                        "The project's Python operators are allowed: the console starts afresh.",
                    );
                }
                previous => {
                    let ops = dir.map(|d| d.join(lsr_py::ops::OPS_DIR));
                    host.set_project(ops, allowed);
                    if previous.is_some() {
                        self.log
                            .note("Another project is open: the console starts afresh.");
                    }
                }
            }
            self.project = Some(key);
        }
        host.sync(engine, time);
        self.events.clear();
        host.poll(&mut self.events);
        for e in &self.events {
            match e {
                Event::Output { stream, text } => {
                    let kind = match stream {
                        Stream::Out => Kind::Out,
                        Stream::Err => Kind::Err,
                        Stream::Result => Kind::Result,
                    };
                    self.log.push(kind, text);
                }
                Event::Done { .. } | Event::Reset => {}
            }
        }
        host.is_busy()
    }

    pub fn ui(&mut self, ui: &mut Ui, engine: Option<&Engine>) {
        if self.host.is_none() && self.failed.is_none() {
            match Textport::start() {
                Ok(h) => {
                    self.host = Some(h);
                    self.log.note(&format!(
                        "Lasair {} Python console · Enter runs, Shift+Enter adds a line, \
                         Up/Down recall. Try lasair.nodes(); see docs/textport.md.",
                        lsr_core::VERSION
                    ));
                }
                Err(e) => self.failed = Some(e.to_string()),
            }
        }
        let Some(host) = &mut self.host else {
            let msg = self.failed.as_deref().unwrap_or_default();
            ui.colored_label(ERROR, format!("The Python console could not start: {msg}"));
            return;
        };
        let busy = host.is_busy();
        ui.horizontal(|ui| {
            if ui
                .button("Clear")
                .on_hover_text("Clear the output (the namespace stays)")
                .clicked()
            {
                self.log.clear();
            }
            if ui
                .button("Reset")
                .on_hover_text("Start a fresh namespace")
                .clicked()
            {
                host.reset();
                self.log.note("Fresh namespace.");
            }
            if ui
                .add_enabled(busy, egui::Button::new("Stop"))
                .on_hover_text("Raise KeyboardInterrupt in the running code")
                .clicked()
            {
                host.interrupt();
            }
            if busy {
                ui.spinner();
                ui.weak("running…");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Close")
                    .on_hover_text("Close the Textport")
                    .clicked()
                {
                    self.close = true;
                }
            });
        });
        ui.separator();

        let mono = egui::TextStyle::Monospace;
        let row = ui.text_style_height(&mono);
        let rows = self.input.lines().count().clamp(1, 8);
        let input_height = row * rows as f32 + 16.0;
        let lines = self.log.lines();
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - input_height).max(row * 2.0))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, row, lines.len(), |ui, range| {
                for (kind, text) in &lines[range] {
                    let text = RichText::new(text).monospace();
                    ui.label(match kind {
                        Kind::Input | Kind::Out => text,
                        Kind::Err => text.color(ERROR),
                        Kind::Result => text.color(ui.visuals().hyperlink_color),
                        Kind::Note => text.weak().italics(),
                    });
                }
            });

        let id = ui.make_persistent_id("textport_input");
        let focused = ui.memory(|m| m.has_focus(id));
        // Keys the editor would otherwise take: Up/Down recall entries
        // (single-line input only), plain Enter runs.
        if focused && !self.input.contains('\n') {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                self.history.step(&mut self.input, true);
            } else if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                self.history.step(&mut self.input, false);
            }
        }
        let run = focused
            && ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter));
        ui.horizontal(|ui| {
            ui.monospace(if host.wants_more() { "..." } else { ">>>" });
            let edit = egui::TextEdit::multiline(&mut self.input)
                .id(id)
                .code_editor()
                .desired_rows(rows)
                .desired_width(f32::INFINITY)
                .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter))
                .hint_text("Python: Enter runs, Shift+Enter adds a line");
            ui.add(edit);
        });
        if run {
            let text = std::mem::take(&mut self.input);
            self.log.echo(&text, host.wants_more());
            self.history.add(&text);
            if let Some(engine) = engine {
                // The entry reads the project as it is now.
                host.publish(engine);
            }
            host.submit(&text);
            ui.memory_mut(|m| m.request_focus(id));
        }
        if host.is_busy() {
            // Output arrives without input events: keep polling.
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(log: &Log) -> Vec<(Kind, &str)> {
        log.lines().iter().map(|(k, t)| (*k, t.as_str())).collect()
    }

    #[test]
    fn output_is_split_into_lines_and_partial_lines_continue() {
        let mut log = Log::default();
        log.push(Kind::Out, "a\nb");
        log.push(Kind::Out, "c\n");
        log.push(Kind::Err, "x\n");
        log.push(Kind::Result, "1\n");
        assert_eq!(
            texts(&log),
            [
                (Kind::Out, "a"),
                (Kind::Out, "bc"),
                (Kind::Err, "x"),
                (Kind::Result, "1")
            ]
        );
        // An unfinished line of another kind does not swallow the next.
        log.push(Kind::Out, "half");
        log.push(Kind::Err, "boom\n");
        assert_eq!(
            &texts(&log)[4..],
            [(Kind::Out, "half"), (Kind::Err, "boom")]
        );
    }

    #[test]
    fn entries_are_echoed_with_prompts() {
        let mut log = Log::default();
        log.echo("for i in range(2):\n    print(i)", false);
        log.echo("", true);
        assert_eq!(
            texts(&log),
            [
                (Kind::Input, ">>> for i in range(2):"),
                (Kind::Input, "...     print(i)"),
                (Kind::Input, "... ")
            ]
        );
    }

    #[test]
    fn the_log_keeps_the_newest_lines() {
        let mut log = Log::default();
        for i in 0..MAX_LINES + 10 {
            log.push(Kind::Out, &format!("{i}\n"));
        }
        assert_eq!(log.lines().len(), MAX_LINES);
        assert_eq!(log.lines()[0].1, "10");
    }

    #[test]
    fn history_recalls_entries_and_restores_the_draft() {
        let mut h = History::default();
        h.add("a = 1");
        h.add("a = 1");
        h.add("  ");
        h.add("b = 2");
        let mut input = "typing".to_owned();
        h.step(&mut input, true);
        assert_eq!(input, "b = 2");
        h.step(&mut input, true);
        assert_eq!(input, "a = 1");
        h.step(&mut input, true);
        assert_eq!(input, "a = 1");
        h.step(&mut input, false);
        assert_eq!(input, "b = 2");
        h.step(&mut input, false);
        assert_eq!(input, "typing");
        // Down with nothing recalled does nothing.
        h.step(&mut input, false);
        assert_eq!(input, "typing");
    }
}
