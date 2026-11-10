//! Ask: one list for commands and files, filtered as you type. The query, filtering and
//! selection live here in the server; a frontend draws an input and a list.

use spur_core::CommandId;
use spur_rpc::{ListRow, RowKind, Span};

use crate::fuzzy::{fuzzy, mark};

/// Most commands and files shown at once.
const MAX_COMMANDS: usize = 7;
const MAX_FILES: usize = 9;

/// What a row runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    Command(CommandId),
    /// Path relative to the workspace root.
    File(String),
}

/// A command offered by Ask.
#[derive(Clone, Debug)]
pub(crate) struct CommandEntry {
    pub id: CommandId,
    pub title: String,
    /// Its key, already formatted, or empty.
    pub hint: String,
}

/// `editor.move_word_next` becomes `Move word next`.
pub(crate) fn title(id: &CommandId) -> String {
    let last = id.as_str().rsplit('.').next().unwrap_or_default();
    let mut s = last.replace('_', " ");
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

/// One client's Ask state.
#[derive(Debug, Default)]
pub(crate) struct Ask {
    pub query: String,
    commands: Vec<CommandEntry>,
    files: Vec<String>,
    pub rows: Vec<ListRow>,
    picks: Vec<Option<Pick>>,
    pub selected: Option<usize>,
}

impl Ask {
    /// Starts a fresh Ask over these commands and files. `featured` commands come first,
    /// in that order; the rest follow by title.
    pub(crate) fn open(
        &mut self,
        mut commands: Vec<CommandEntry>,
        files: Vec<String>,
        featured: &[CommandId],
    ) {
        let rank = |c: &CommandEntry| {
            featured
                .iter()
                .position(|f| *f == c.id)
                .unwrap_or(usize::MAX)
        };
        commands.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.id.cmp(&b.id))
        });
        self.commands = commands;
        self.files = files;
        self.query.clear();
        self.filter();
    }

    /// Forgets the lists (they are rebuilt on the next open).
    pub(crate) fn close(&mut self) {
        *self = Self::default();
    }

    /// Appends typed text to the query.
    pub(crate) fn insert(&mut self, text: &str) {
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.filter();
    }

    /// Deletes the last char of the query.
    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.filter();
    }

    fn filter(&mut self) {
        self.rows.clear();
        self.picks.clear();
        let q = self.query.clone();

        let mut cmds: Vec<(i32, &CommandEntry, Vec<usize>)> = self
            .commands
            .iter()
            .filter_map(|c| {
                // Match the title; fall back to the id so `editor.undo` also finds it.
                fuzzy(&q, &c.title)
                    .map(|(s, at)| (s, c, at))
                    .or_else(|| fuzzy(&q, c.id.as_str()).map(|(s, _)| (s - 20, c, Vec::new())))
            })
            .collect();
        cmds.sort_by_key(|(s, ..)| std::cmp::Reverse(*s));
        let mut files: Vec<(i32, &String, Vec<usize>)> = self
            .files
            .iter()
            .filter_map(|f| fuzzy(&q, f).map(|(s, at)| (s, f, at)))
            .collect();
        files.sort_by_key(|(s, ..)| std::cmp::Reverse(*s));

        let header = |title: &str| ListRow {
            kind: RowKind::Header,
            spans: vec![Span::plain(title)],
            ..ListRow::default()
        };
        if !cmds.is_empty() {
            self.rows.push(header("Commands"));
            self.picks.push(None);
        }
        for (_, c, at) in cmds.into_iter().take(MAX_COMMANDS) {
            self.rows.push(ListRow {
                kind: RowKind::Item,
                indent: 0,
                icon: "◇".into(),
                spans: mark(&c.title, &at),
                detail: c.id.to_string(),
                hint: c.hint.clone(),
            });
            self.picks.push(Some(Pick::Command(c.id.clone())));
        }
        if !files.is_empty() {
            self.rows.push(header("Files"));
            self.picks.push(None);
        }
        for (_, f, at) in files.into_iter().take(MAX_FILES) {
            self.rows.push(ListRow {
                kind: RowKind::Item,
                indent: 0,
                icon: "·".into(),
                spans: mark(f, &at),
                detail: String::new(),
                hint: String::new(),
            });
            self.picks.push(Some(Pick::File(f.clone())));
        }
        // With a query, the best row overall wins; without one, the first row.
        self.selected = self.picks.iter().position(Option::is_some);
    }

    /// Moves the selection over items, skipping headers, stopping at the ends.
    pub(crate) fn move_by(&mut self, delta: i32) {
        let Some(mut at) = self.selected else { return };
        for _ in 0..delta.unsigned_abs() {
            let next = if delta < 0 {
                (0..at).rev().find(|&i| self.picks[i].is_some())
            } else {
                (at + 1..self.picks.len()).find(|&i| self.picks[i].is_some())
            };
            match next {
                Some(n) => at = n,
                None => break,
            }
        }
        self.selected = Some(at);
    }

    /// Selects the row at `index` if it is an item.
    pub(crate) fn select(&mut self, index: usize) -> bool {
        let ok = self.picks.get(index).is_some_and(Option::is_some);
        if ok {
            self.selected = Some(index);
        }
        ok
    }

    /// What the selected row runs.
    pub(crate) fn pick(&self) -> Option<Pick> {
        self.picks.get(self.selected?)?.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Ask {
        let cmd = |id: &str, hint: &str| {
            let id = CommandId::new(id).unwrap();
            CommandEntry {
                title: title(&id),
                id,
                hint: hint.into(),
            }
        };
        let mut a = Ask::default();
        a.open(
            vec![cmd("editor.undo", "ctrl z"), cmd("buffer.save", "ctrl s")],
            vec!["src/main.rs".into(), "README.md".into()],
            &[],
        );
        a
    }

    #[test]
    fn filters_and_moves_over_items_only() {
        let mut a = ask();
        assert_eq!(a.rows.len(), 6, "two headers, two commands, two files");
        assert_eq!(a.rows[1].spans[0].text, "Save", "sorted by title");
        assert_eq!(
            a.pick(),
            Some(Pick::Command(CommandId::new("buffer.save").unwrap()))
        );
        a.move_by(2);
        assert_eq!(
            a.pick(),
            Some(Pick::File("src/main.rs".into())),
            "skips the header"
        );
        a.move_by(50);
        assert_eq!(a.pick(), Some(Pick::File("README.md".into())));
        a.move_by(-50);
        assert_eq!(a.selected, Some(1));
        assert!(!a.select(0), "headers cannot be selected");
        assert!(a.select(5));

        a.insert("mai\n");
        assert_eq!(a.query, "mai", "control chars are dropped");
        assert_eq!(a.pick(), Some(Pick::File("src/main.rs".into())));
        assert_eq!(a.rows[0].kind, RowKind::Header);
        a.insert("zzz");
        assert!(a.rows.is_empty());
        assert_eq!(a.pick(), None);
        a.move_by(1);
        for _ in 0..10 {
            a.backspace();
        }
        assert_eq!(a.rows.len(), 6);
        a.insert("editor.un");
        assert_eq!(
            a.pick(),
            Some(Pick::Command(CommandId::new("editor.undo").unwrap()))
        );
        let featured = [CommandId::new("editor.undo").unwrap()];
        let commands = std::mem::take(&mut a.commands);
        a.open(commands, vec![], &featured);
        assert_eq!(a.rows[1].spans[0].text, "Undo", "featured commands lead");
    }
}
