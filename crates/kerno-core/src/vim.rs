//! Vim mode for the SQL editor (CLAUDE.md §4 J19; DBeaver #37897, #8219,
//! Beekeeper #317): a state machine over the text and the cursor that
//! turns keys into [`Effect`]s, which the app applies to its editor. Pure:
//! no UI, no clipboard, no file; the app passes what it needs in a
//! [`Buffer`] and does what the effects say.
//!
//! Offsets are byte offsets on character boundaries; `h` and `l` move by
//! `char`. Motions scan from the cursor to the next line break or word
//! boundary, never the whole text, so a key costs the same in a 10 MB
//! script as in a short one (`/`, `n`, `N` search the text, `%` lexes a
//! window around the cursor).

use std::ops::Range;

use crate::sql::{LexRules, matching_bracket};

/// What the keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Keys are commands; the cursor sits on a character.
    #[default]
    Normal,
    /// Keys type text (the editor handles them); `Esc` goes back to Normal.
    Insert,
    /// Motions extend a selection of characters.
    Visual,
    /// Motions extend a selection of whole lines.
    VisualLine,
}

impl Mode {
    /// The mode as the status bar shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "VISUAL LINE",
        }
    }
}

/// A key the app gives to vim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character typed, shift included (`G`, `$`, `%`).
    Char(char),
    /// A character with Control held (`⌃r`).
    Ctrl(char),
    Escape,
    Enter,
    Backspace,
}

/// What vim reads of the editor for one key.
#[derive(Debug, Clone, Copy)]
pub struct Buffer<'a> {
    pub text: &'a str,
    /// The editor's cursor (its selection's head).
    pub cursor: usize,
    /// The system clipboard's text: the unnamed register.
    pub clipboard: Option<&'a str>,
    /// How the engine lexes SQL, for `%`.
    pub rules: LexRules,
}

/// What the app does to its editor, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Put the cursor at this offset, without a selection.
    Move(usize),
    /// Select `range`; the cursor at its start when `reversed`.
    Select {
        range: Range<usize>,
        reversed: bool,
    },
    /// Replace `range` with `text`, as one undoable edit.
    Replace {
        range: Range<usize>,
        text: String,
    },
    Undo,
    Redo,
    /// Write `text` to the system clipboard.
    Yank(String),
    /// Open the one-line command field for `:` or `/`.
    Prompt(char),
    /// `:w` (and `:wq`, which closes the tab once written).
    Write {
        then_close: bool,
    },
    /// `:q`; `force` (`:q!`) forgets unsaved text first.
    Quit {
        force: bool,
    },
    /// Tell the user what went wrong (`/` found nothing, unknown `:`).
    Error(String),
}

/// The last change, for `.`: the keys of its command and, when it
/// entered Insert mode, the text typed there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    keys: Vec<Key>,
    inserted: Option<String>,
}

/// Where Insert mode began, to read back what was typed at `Esc`.
#[derive(Debug, Clone)]
struct InsertStart {
    keys: Vec<Key>,
    at: usize,
    /// The text's length when Insert mode began.
    len: usize,
}

/// The state of vim in one editor.
#[derive(Debug, Default)]
pub struct Vim {
    mode: Mode,
    /// Keys of the command being typed.
    pending: Vec<Key>,
    /// The selection's fixed end and moving end, in the Visual modes.
    anchor: usize,
    head: usize,
    /// Where the editor's cursor is after the last effects: elsewhere at
    /// the next key, the mouse or an arrow moved it.
    expected: Option<usize>,
    /// The column `j` and `k` keep, in characters (`usize::MAX`: the end).
    column: Option<usize>,
    /// The last text yanked or deleted, and whether it is whole lines.
    register: Option<(String, bool)>,
    last_change: Option<Change>,
    insert: Option<InsertStart>,
    search: Option<String>,
    /// While `.` replays a change that typed text: that text.
    replay: Option<String>,
}

// The commands, as parsed from the keys --------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Down,
    Up,
    WordForward,
    WordBack,
    WordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    /// `gg`: the first line, or line `count`.
    FirstLine,
    /// `G`: the last line, or line `count`.
    LastLine,
    Find(char),
    Till(char),
    Bracket,
    /// Enter: the first non-blank of the next line.
    NextLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Motion(Motion),
    /// `dd`, `cc`, `yy`.
    Lines,
    /// `iw` (inner) and `aw`.
    Word {
        inner: bool,
    },
    /// The Visual selection.
    Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Move(Motion),
    Op(Op, Target),
    /// `i a I A o O`.
    Insert(char),
    Paste {
        before: bool,
    },
    Undo,
    Redo,
    Repeat,
    Visual(Mode),
    Search {
        forward: bool,
    },
    Prompt(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Command {
    count: Option<usize>,
    action: Action,
}

enum Parse<T> {
    More,
    Bad,
    Done(T),
}

/// A count at the start of `keys` (a leading `0` is a motion) and how
/// many keys it took.
fn read_count(keys: &[Key]) -> (Option<usize>, usize) {
    let mut count: Option<usize> = None;
    let mut used = 0;
    for key in keys {
        match key {
            Key::Char(digit @ '0'..='9') if count.is_some() || *digit != '0' => {
                let value = digit.to_digit(10).unwrap_or(0) as usize;
                count = Some(count.unwrap_or(0).saturating_mul(10).saturating_add(value));
                used += 1;
            }
            _ => break,
        }
    }
    (count, used)
}

fn parse_motion(keys: &[Key]) -> Parse<Motion> {
    let Some(key) = keys.first() else {
        return Parse::More;
    };
    let motion = match key {
        Key::Enter => Motion::NextLine,
        Key::Backspace => Motion::Left,
        Key::Char('h') => Motion::Left,
        Key::Char('l' | ' ') => Motion::Right,
        Key::Char('j') => Motion::Down,
        Key::Char('k') => Motion::Up,
        Key::Char('w') => Motion::WordForward,
        Key::Char('b') => Motion::WordBack,
        Key::Char('e') => Motion::WordEnd,
        Key::Char('0') => Motion::LineStart,
        Key::Char('^') => Motion::FirstNonBlank,
        Key::Char('$') => Motion::LineEnd,
        Key::Char('G') => Motion::LastLine,
        Key::Char('%') => Motion::Bracket,
        Key::Char(prefix @ ('g' | 'f' | 't')) => {
            return match (prefix, keys.get(1)) {
                (_, None) => Parse::More,
                ('g', Some(Key::Char('g'))) => Parse::Done(Motion::FirstLine),
                ('f', Some(Key::Char(c))) => Parse::Done(Motion::Find(*c)),
                ('t', Some(Key::Char(c))) => Parse::Done(Motion::Till(*c)),
                _ => Parse::Bad,
            };
        }
        _ => return Parse::Bad,
    };
    Parse::Done(motion)
}

fn parse(keys: &[Key], visual: bool) -> Parse<Command> {
    let (count, used) = read_count(keys);
    let rest = &keys[used..];
    let done = |action| Parse::Done(Command { count, action });
    let Some(key) = rest.first() else {
        return Parse::More;
    };
    let op = match key {
        Key::Char('d') => Some(Op::Delete),
        Key::Char('c') => Some(Op::Change),
        Key::Char('y') => Some(Op::Yank),
        _ => None,
    };
    if let Some(op) = op {
        if visual {
            return done(Action::Op(op, Target::Selection));
        }
        let (second, used) = read_count(&rest[1..]);
        let count = match (count, second) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
        };
        let target = &rest[1 + used..];
        let target = match target.first() {
            None => return Parse::More,
            Some(same) if same == key => Target::Lines,
            Some(Key::Char(object @ ('i' | 'a'))) => match target.get(1) {
                None => return Parse::More,
                Some(Key::Char('w')) => Target::Word {
                    inner: *object == 'i',
                },
                Some(_) => return Parse::Bad,
            },
            Some(_) => match parse_motion(target) {
                Parse::More => return Parse::More,
                Parse::Bad => return Parse::Bad,
                Parse::Done(motion) => Target::Motion(motion),
            },
        };
        return Parse::Done(Command {
            count,
            action: Action::Op(op, target),
        });
    }
    match key {
        Key::Char('x') if visual => done(Action::Op(Op::Delete, Target::Selection)),
        Key::Char('x') => done(Action::Op(Op::Delete, Target::Motion(Motion::Right))),
        Key::Char('p') => done(Action::Paste { before: false }),
        Key::Char('P') => done(Action::Paste { before: true }),
        Key::Char('u') if !visual => done(Action::Undo),
        Key::Ctrl('r') if !visual => done(Action::Redo),
        Key::Char('.') if !visual => done(Action::Repeat),
        Key::Char(c @ ('i' | 'a' | 'I' | 'A' | 'o' | 'O')) if !visual => done(Action::Insert(*c)),
        Key::Char('v') => done(Action::Visual(Mode::Visual)),
        Key::Char('V') => done(Action::Visual(Mode::VisualLine)),
        Key::Char('n') => done(Action::Search { forward: true }),
        Key::Char('N') => done(Action::Search { forward: false }),
        Key::Char('/') => done(Action::Prompt('/')),
        Key::Char(':') if !visual => done(Action::Prompt(':')),
        _ => match parse_motion(rest) {
            Parse::More => Parse::More,
            Parse::Bad => Parse::Bad,
            Parse::Done(motion) => done(Action::Move(motion)),
        },
    }
}

// Text helpers: each scans from `at` to a line break or a word boundary ----

fn char_at(text: &str, at: usize) -> Option<char> {
    text[at..].chars().next()
}

fn next(text: &str, at: usize) -> usize {
    char_at(text, at).map_or(at, |c| at + c.len_utf8())
}

fn prev(text: &str, at: usize) -> usize {
    text[..at]
        .chars()
        .next_back()
        .map_or(at, |c| at - c.len_utf8())
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

/// The last character of the line of `at` (its start when empty): where
/// Normal mode's cursor may go at most.
fn last_char(text: &str, at: usize) -> usize {
    let (start, end) = (line_start(text, at), line_end(text, at));
    if end > start { prev(text, end) } else { start }
}

fn clamp(text: &str, at: usize) -> usize {
    at.min(last_char(text, at))
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// The first character of the line of `at` that is not a space or a tab.
fn first_non_blank(text: &str, at: usize) -> usize {
    let start = line_start(text, at);
    let end = line_end(text, start);
    start
        + text[start..end]
            .find(|c| !is_blank(c))
            .unwrap_or(end - start)
}

/// The leading spaces and tabs of the line of `at`.
fn indent(text: &str, at: usize) -> &str {
    let start = line_start(text, at);
    &text[start..first_non_blank(text, at)]
}

fn column(text: &str, at: usize) -> usize {
    text[line_start(text, at)..at].chars().count()
}

/// The offset of character `column` of the line starting at `start`, or
/// its end.
fn at_column(text: &str, start: usize, column: usize) -> usize {
    let end = line_end(text, start);
    text[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(i, _)| start + i)
}

/// The start of line `line` (from 0), or of the last line.
fn line_at(text: &str, line: usize) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => break,
        }
    }
    start
}

/// Word classes: blanks, keyword characters, other signs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Sign,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Sign
    }
}

fn class_at(text: &str, at: usize) -> Class {
    char_at(text, at).map_or(Class::Blank, class)
}

/// `w`: the start of the next word; an empty line is a word.
fn word_forward(text: &str, at: usize) -> usize {
    let mut i = at;
    let start = class_at(text, i);
    if start != Class::Blank {
        while i < text.len() && class_at(text, i) == start {
            i = next(text, i);
        }
    }
    while let Some(c) = char_at(text, i) {
        if c == '\n' {
            i = next(text, i);
            if char_at(text, i) == Some('\n') {
                return i;
            }
        } else if c.is_whitespace() {
            i = next(text, i);
        } else {
            break;
        }
    }
    i
}

/// `e`: the end of this word, or of the next one when already at an end.
fn word_end(text: &str, at: usize) -> usize {
    let mut i = next(text, at);
    while char_at(text, i).is_some_and(char::is_whitespace) {
        i = next(text, i);
    }
    if i >= text.len() {
        return prev(text, text.len());
    }
    let word = class_at(text, i);
    while next(text, i) < text.len() && class_at(text, next(text, i)) == word {
        i = next(text, i);
    }
    i
}

/// `b`: the start of this word, or of the previous one when at a start.
fn word_back(text: &str, at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    let mut i = prev(text, at);
    while i > 0 && class_at(text, i) == Class::Blank {
        i = prev(text, i);
    }
    let word = class_at(text, i);
    while i > 0 && word != Class::Blank && class_at(text, prev(text, i)) == word {
        i = prev(text, i);
    }
    i
}

/// `iw` / `aw`: the run of one class around `at` on its line; `aw` takes
/// the blanks after it too (before it when there are none after).
fn word_object(text: &str, at: usize, inner: bool) -> Option<Range<usize>> {
    let c = char_at(text, at).filter(|c| *c != '\n')?;
    let same = |i: usize| char_at(text, i).is_some_and(|d| d != '\n' && class(d) == class(c));
    let (mut start, mut end) = (at, next(text, at));
    while start > 0 && same(prev(text, start)) {
        start = prev(text, start);
    }
    while end < text.len() && same(end) {
        end = next(text, end);
    }
    if !inner && class(c) != Class::Blank {
        let blank = |i: usize| char_at(text, i).is_some_and(is_blank);
        let after = end;
        while end < text.len() && blank(end) {
            end = next(text, end);
        }
        if end == after {
            while start > 0 && blank(prev(text, start)) {
                start = prev(text, start);
            }
        }
    }
    Some(start..end)
}

/// The bracket on the line of `at`, at or after it, and the offset of
/// its match: lexed in a window around it, as the editor's bracket marks
/// are.
fn bracket_match(text: &str, at: usize, rules: LexRules) -> Option<usize> {
    const WINDOW: usize = 64 * 1024;
    let end = line_end(text, at);
    let bracket = at + text[at..end].find(['(', ')', '[', ']', '{', '}'])?;
    let mut from = bracket.saturating_sub(WINDOW);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let from = line_start(text, from);
    let mut to = (bracket + WINDOW).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }
    let (at, other) = matching_bracket(&text[from..to], bracket + 1 - from, rules)?;
    (at.start + from == bracket).then_some(other.start + from)
}

/// Where a motion goes and how an operator takes it.
#[derive(Debug, Clone, Copy)]
struct Jump {
    to: usize,
    linewise: bool,
    /// The character at `to` is part of the range.
    inclusive: bool,
}

impl Vim {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Back to Normal mode with nothing pending (the user clicked, or the
    /// command field closed).
    pub fn reset(&mut self) {
        self.mode = Mode::Normal;
        self.pending.clear();
        self.insert = None;
    }

    /// The character Normal mode's block covers, for the app to draw;
    /// `None` outside Normal mode and on an empty line.
    pub fn block(&self, text: &str, cursor: usize) -> Option<Range<usize>> {
        if self.mode != Mode::Normal || cursor > text.len() || !text.is_char_boundary(cursor) {
            return None;
        }
        let at = clamp(text, cursor);
        char_at(text, at)
            .filter(|c| *c != '\n')
            .map(|c| at..at + c.len_utf8())
    }

    /// Handles one key.
    pub fn key(&mut self, key: Key, buffer: &Buffer) -> Vec<Effect> {
        if self.mode == Mode::Insert {
            return match key {
                Key::Escape => self.leave_insert(buffer),
                _ => Vec::new(),
            };
        }
        if key == Key::Escape {
            self.pending.clear();
            if self.mode == Mode::Normal {
                return Vec::new();
            }
            let head = self.head(buffer);
            self.mode = Mode::Normal;
            return vec![self.go(clamp(buffer.text, head))];
        }
        self.pending.push(key);
        let visual = self.mode != Mode::Normal;
        match parse(&self.pending, visual) {
            Parse::More => Vec::new(),
            Parse::Bad => {
                self.pending.clear();
                Vec::new()
            }
            Parse::Done(command) => {
                let keys = std::mem::take(&mut self.pending);
                self.run(command, keys, buffer)
            }
        }
    }

    /// Runs what was typed in the command field: `/pattern`, or `:w`,
    /// `:q`, `:wq`, `:q!`.
    pub fn command(&mut self, kind: char, line: &str, buffer: &Buffer) -> Vec<Effect> {
        if kind == '/' {
            if !line.is_empty() {
                self.search = Some(line.to_string());
            }
            return self.search_next(true, 1, buffer);
        }
        match line.trim() {
            "w" => vec![Effect::Write { then_close: false }],
            "wq" | "x" => vec![Effect::Write { then_close: true }],
            "q" => vec![Effect::Quit { force: false }],
            "q!" => vec![Effect::Quit { force: true }],
            "" => Vec::new(),
            other => vec![Effect::Error(format!("Not an editor command: {other}"))],
        }
    }

    // Cursor bookkeeping -----------------------------------------------------

    /// Normal mode's cursor: the editor's, kept on a character; the column
    /// `j`/`k` keep is forgotten when something else moved the cursor.
    fn cursor(&mut self, buffer: &Buffer) -> usize {
        if self.expected != Some(buffer.cursor) {
            self.column = None;
        }
        clamp(buffer.text, buffer.cursor)
    }

    /// The Visual selection's moving end; the editor's cursor when an
    /// arrow or the mouse moved it since.
    fn head(&mut self, buffer: &Buffer) -> usize {
        if self.expected != Some(buffer.cursor) {
            self.column = None;
            self.head = clamp(buffer.text, buffer.cursor);
        }
        self.head
    }

    fn go(&mut self, to: usize) -> Effect {
        self.expected = Some(to);
        Effect::Move(to)
    }

    /// The Visual selection as the editor shows it.
    fn select(&mut self, text: &str) -> Effect {
        let (low, high) = (self.anchor.min(self.head), self.anchor.max(self.head));
        let range = if self.mode == Mode::VisualLine {
            line_start(text, low)..next(text, line_end(text, high))
        } else {
            low..next(text, high)
        };
        let reversed = self.head < self.anchor;
        self.expected = Some(if reversed { range.start } else { range.end });
        Effect::Select { range, reversed }
    }

    /// Moves Normal mode's cursor, or the Visual selection's head.
    fn jump(&mut self, to: usize, text: &str) -> Vec<Effect> {
        if self.mode == Mode::Normal {
            vec![self.go(clamp(text, to))]
        } else {
            self.head = clamp(text, to);
            vec![self.select(text)]
        }
    }

    // Commands ----------------------------------------------------------------

    fn run(&mut self, command: Command, keys: Vec<Key>, buffer: &Buffer) -> Vec<Effect> {
        let text = buffer.text;
        let at = if self.mode == Mode::Normal {
            self.cursor(buffer)
        } else {
            self.head(buffer)
        };
        let n = command.count.unwrap_or(1).max(1);
        match command.action {
            Action::Move(motion) => match self.motion(buffer, at, motion, command.count) {
                Some(jump) => self.jump(jump.to, text),
                None => Vec::new(),
            },
            Action::Op(op, Target::Selection) => {
                let (low, high) = (self.anchor.min(self.head), self.anchor.max(self.head));
                let linewise = self.mode == Mode::VisualLine;
                let range = if linewise {
                    lines(text, low, high)
                } else {
                    low..next(text, high)
                };
                self.mode = Mode::Normal;
                self.operate(op, range, linewise, at.min(low), Vec::new(), buffer)
            }
            Action::Op(op, Target::Lines) => {
                let last = line_at_offset(text, at, n - 1);
                let range = lines(text, at, last);
                self.operate(op, range, true, at, keys, buffer)
            }
            Action::Op(op, Target::Word { inner }) => match word_object(text, at, inner) {
                Some(range) => self.operate(op, range, false, at, keys, buffer),
                None => Vec::new(),
            },
            Action::Op(op, Target::Motion(motion)) => {
                // `cw` on a word changes to its end, as `ce` does from
                // inside it (vim's rule): the word under the cursor counts.
                let changes_word = op == Op::Change
                    && motion == Motion::WordForward
                    && class_at(text, at) != Class::Blank;
                let jump = if changes_word {
                    let mut to = at;
                    while class_at(text, next(text, to)) == class_at(text, at)
                        && next(text, to) < text.len()
                    {
                        to = next(text, to);
                    }
                    let more = command
                        .count
                        .map(|n| n.saturating_sub(1))
                        .filter(|n| *n > 0);
                    match more {
                        Some(more) => self.motion(buffer, to, Motion::WordEnd, Some(more)),
                        None => Some(Jump {
                            to,
                            linewise: false,
                            inclusive: true,
                        }),
                    }
                } else {
                    self.motion(buffer, at, motion, command.count)
                };
                let Some(jump) = jump else {
                    return Vec::new();
                };
                let (range, linewise) = if jump.linewise {
                    (lines(text, at.min(jump.to), at.max(jump.to)), true)
                } else {
                    let (low, high) = (at.min(jump.to), at.max(jump.to));
                    // The character at an inclusive end is taken, never a
                    // line break.
                    let mut high =
                        if jump.inclusive && char_at(text, high).is_some_and(|c| c != '\n') {
                            next(text, high)
                        } else {
                            high
                        };
                    // `dw` on the last word of a line stops at its end.
                    if motion == Motion::WordForward && text[low..high].contains('\n') {
                        high = line_end(text, low).max(low);
                    }
                    (low..high, false)
                };
                let cursor = if linewise {
                    at.min(jump.to)
                } else {
                    range.start
                };
                self.operate(op, range, linewise, cursor, keys, buffer)
            }
            Action::Insert(kind) => self.start_insert(kind, at, keys, buffer),
            Action::Paste { before } => self.paste(before, n, at, keys, buffer),
            Action::Undo => vec![Effect::Undo; n],
            Action::Redo => vec![Effect::Redo; n],
            Action::Repeat => self.repeat(command.count, buffer),
            Action::Visual(mode) => {
                if self.mode == mode {
                    self.mode = Mode::Normal;
                    vec![self.go(clamp(text, at))]
                } else {
                    if self.mode == Mode::Normal {
                        self.anchor = at;
                        self.head = at;
                    }
                    self.mode = mode;
                    vec![self.select(text)]
                }
            }
            Action::Search { forward } => self.search_next(forward, n, buffer),
            Action::Prompt(kind) => vec![Effect::Prompt(kind)],
        }
    }

    fn motion(
        &mut self,
        buffer: &Buffer,
        at: usize,
        motion: Motion,
        count: Option<usize>,
    ) -> Option<Jump> {
        let text = buffer.text;
        let n = count.unwrap_or(1).max(1);
        let jump = |to, linewise, inclusive| {
            Some(Jump {
                to,
                linewise,
                inclusive,
            })
        };
        if !matches!(motion, Motion::Down | Motion::Up) {
            self.column = None;
        }
        match motion {
            Motion::Left => {
                let start = line_start(text, at);
                let mut to = at;
                for _ in 0..n {
                    if to <= start {
                        break;
                    }
                    to = prev(text, to);
                }
                jump(to, false, false)
            }
            Motion::Right => {
                let end = line_end(text, at);
                let mut to = at;
                for _ in 0..n {
                    if to >= end {
                        break;
                    }
                    to = next(text, to);
                }
                jump(to, false, false)
            }
            Motion::Down | Motion::Up => {
                let column = *self.column.get_or_insert_with(|| column(text, at));
                let mut start = line_start(text, at);
                for _ in 0..n {
                    let moved = if motion == Motion::Down {
                        let end = line_end(text, start);
                        (end < text.len()).then(|| end + 1)
                    } else {
                        (start > 0).then(|| line_start(text, start - 1))
                    };
                    match moved {
                        Some(line) => start = line,
                        None => break,
                    }
                }
                jump(at_column(text, start, column), true, false)
            }
            Motion::WordForward => {
                let mut to = at;
                for _ in 0..n {
                    to = word_forward(text, to);
                }
                jump(to, false, false)
            }
            Motion::WordBack => {
                let mut to = at;
                for _ in 0..n {
                    to = word_back(text, to);
                }
                jump(to, false, false)
            }
            Motion::WordEnd => {
                let mut to = at;
                for _ in 0..n {
                    to = word_end(text, to);
                }
                jump(to, false, true)
            }
            Motion::LineStart => jump(line_start(text, at), false, false),
            Motion::FirstNonBlank => jump(first_non_blank(text, at), false, false),
            Motion::LineEnd => {
                self.column = Some(usize::MAX);
                let line = line_at_offset(text, at, n - 1);
                jump(last_char(text, line), false, true)
            }
            Motion::FirstLine | Motion::LastLine => {
                let start = match (motion, count) {
                    (_, Some(line)) => line_at(text, line.saturating_sub(1)),
                    (Motion::FirstLine, None) => 0,
                    _ => line_start(text, text.len()),
                };
                jump(first_non_blank(text, start), true, false)
            }
            Motion::Find(c) | Motion::Till(c) => {
                let end = line_end(text, at);
                let mut to = at;
                for _ in 0..n {
                    let from = next(text, to);
                    if from > end {
                        return None;
                    }
                    to = from + text[from..end].find(c)?;
                }
                if matches!(motion, Motion::Till(_)) {
                    to = prev(text, to);
                }
                jump(to, false, true)
            }
            Motion::Bracket => jump(bracket_match(text, at, buffer.rules)?, false, true),
            Motion::NextLine => {
                let line = line_at_offset(text, at, n);
                if line == line_start(text, at) {
                    return None;
                }
                jump(first_non_blank(text, line), true, false)
            }
        }
    }

    /// Applies an operator to `range`: characters, or with `linewise`
    /// the lines from its start's to its end's, without the last line
    /// break (see [`lines`]). `cursor` is where Normal mode's cursor goes
    /// after a yank.
    fn operate(
        &mut self,
        op: Op,
        range: Range<usize>,
        linewise: bool,
        cursor: usize,
        keys: Vec<Key>,
        buffer: &Buffer,
    ) -> Vec<Effect> {
        let text = buffer.text;
        if range.is_empty() && !linewise {
            self.mode = Mode::Normal;
            return Vec::new();
        }
        let mut yanked = text[range.clone()].to_string();
        if linewise {
            yanked.push('\n');
        }
        self.register = Some((yanked.clone(), linewise));
        let yank = Effect::Yank(yanked);
        match op {
            Op::Yank => {
                self.mode = Mode::Normal;
                vec![yank, self.go(clamp(text, cursor))]
            }
            Op::Delete => {
                self.mode = Mode::Normal;
                if !keys.is_empty() {
                    self.last_change = Some(Change {
                        keys,
                        inserted: None,
                    });
                }
                // Lines go with a line break: the one after them, else
                // (the last lines) the one before.
                let range = if !linewise {
                    range
                } else if range.end < text.len() {
                    range.start..range.end + 1
                } else if range.start > 0 {
                    range.start - 1..range.end
                } else {
                    range
                };
                let to = cursor_after_delete(text, &range, linewise);
                vec![
                    yank,
                    Effect::Replace {
                        range,
                        text: String::new(),
                    },
                    self.go(to),
                ]
            }
            Op::Change => {
                // Whole lines leave one empty line to type on.
                let mut effects = vec![yank];
                effects.extend(self.insert_at(keys, range, "", "", buffer));
                effects
            }
        }
    }

    /// Enters Insert mode at `range` replaced by `before` + `after`, with
    /// the cursor between; while `.` replays a change, what was typed goes
    /// between and Normal mode stays.
    fn insert_at(
        &mut self,
        keys: Vec<Key>,
        range: Range<usize>,
        before: &str,
        after: &str,
        buffer: &Buffer,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(typed) = self.replay.clone() {
            let text = format!("{before}{typed}{after}");
            let end = range.start + before.len() + typed.len();
            let local = end - range.start;
            // Esc steps back onto the last character typed.
            let to = match text[..local].chars().next_back() {
                Some(c) if c != '\n' => end - c.len_utf8(),
                _ => end,
            };
            if !(range.is_empty() && text.is_empty()) {
                effects.push(Effect::Replace { range, text });
            }
            self.mode = Mode::Normal;
            effects.push(self.go(to));
            return effects;
        }
        let new_len = buffer.text.len() - range.len() + before.len() + after.len();
        let at = range.start + before.len();
        if !(range.is_empty() && before.is_empty() && after.is_empty()) {
            effects.push(Effect::Replace {
                range,
                text: format!("{before}{after}"),
            });
        }
        self.mode = Mode::Insert;
        self.insert = Some(InsertStart {
            keys,
            at,
            len: new_len,
        });
        effects.push(self.go(at));
        effects
    }

    fn start_insert(
        &mut self,
        kind: char,
        at: usize,
        keys: Vec<Key>,
        buffer: &Buffer,
    ) -> Vec<Effect> {
        let text = buffer.text;
        let empty_line = char_at(text, at).is_none_or(|c| c == '\n');
        match kind {
            'i' => self.insert_at(keys, at..at, "", "", buffer),
            'a' => {
                let to = if empty_line { at } else { next(text, at) };
                self.insert_at(keys, to..to, "", "", buffer)
            }
            'I' => {
                let to = first_non_blank(text, at);
                self.insert_at(keys, to..to, "", "", buffer)
            }
            'A' => {
                let to = line_end(text, at);
                self.insert_at(keys, to..to, "", "", buffer)
            }
            'o' => {
                let to = line_end(text, at);
                let before = format!("\n{}", indent(text, at));
                self.insert_at(keys, to..to, &before, "", buffer)
            }
            _ => {
                let to = line_start(text, at);
                let indent = indent(text, at).to_string();
                self.insert_at(keys, to..to, &indent, "\n", buffer)
            }
        }
    }

    /// `Esc` in Insert mode: what was typed since it began is kept for
    /// `.` when it is one run of text before the cursor.
    fn leave_insert(&mut self, buffer: &Buffer) -> Vec<Effect> {
        let text = buffer.text;
        self.mode = Mode::Normal;
        if let Some(start) = self.insert.take() {
            let cursor = buffer.cursor;
            let inserted = (cursor >= start.at
                && cursor <= text.len()
                && text.len() == start.len + (cursor - start.at)
                && text.is_char_boundary(start.at)
                && text.is_char_boundary(cursor))
            .then(|| text[start.at..cursor].to_string());
            self.last_change = Some(Change {
                keys: start.keys,
                inserted,
            });
        }
        let cursor = buffer.cursor.min(text.len());
        let to = if cursor > line_start(text, cursor) {
            prev(text, cursor)
        } else {
            cursor
        };
        vec![self.go(to)]
    }

    fn paste(
        &mut self,
        before: bool,
        n: usize,
        at: usize,
        keys: Vec<Key>,
        buffer: &Buffer,
    ) -> Vec<Effect> {
        let text = buffer.text;
        let Some(pasted) = buffer.clipboard.filter(|pasted| !pasted.is_empty()) else {
            return Vec::new();
        };
        if self.mode != Mode::Normal {
            return Vec::new();
        }
        // The clipboard is whole lines when it still holds what vim yanked
        // as lines; anything copied elsewhere pastes as characters.
        let linewise = self
            .register
            .as_ref()
            .is_some_and(|(yanked, lines)| *lines && yanked == pasted);
        self.last_change = Some(Change {
            keys,
            inserted: None,
        });
        if linewise {
            let body = pasted.repeat(n);
            let lead = body.len() - body.trim_start_matches(is_blank).len();
            let end = line_end(text, at);
            let (range, insert, to) = if before {
                let start = line_start(text, at);
                (start..start, body, start + lead)
            } else if end < text.len() {
                (end + 1..end + 1, body, end + 1 + lead)
            } else {
                // The last line has no line break to paste after.
                let body = format!("\n{}", body.strip_suffix('\n').unwrap_or(&body));
                (end..end, body, end + 1 + lead)
            };
            vec![
                Effect::Replace {
                    range,
                    text: insert,
                },
                self.go(to),
            ]
        } else {
            let empty_line = char_at(text, at).is_none_or(|c| c == '\n');
            let start = if before || empty_line {
                at
            } else {
                next(text, at)
            };
            let body = pasted.repeat(n);
            let to = start + prev(&body, body.len());
            vec![
                Effect::Replace {
                    range: start..start,
                    text: body,
                },
                self.go(to),
            ]
        }
    }

    /// `.`: the last change again, at the cursor; a count replaces its own.
    fn repeat(&mut self, count: Option<usize>, buffer: &Buffer) -> Vec<Effect> {
        let Some(change) = self.last_change.clone() else {
            return Vec::new();
        };
        let mut keys = change.keys.clone();
        if let Some(count) = count {
            let (_, used) = read_count(&keys);
            keys.drain(..used);
            let digits: Vec<Key> = count.to_string().chars().map(Key::Char).collect();
            keys.splice(0..0, digits);
        }
        self.replay = Some(change.inserted.clone().unwrap_or_default());
        self.pending.clear();
        let mut effects = Vec::new();
        for key in keys {
            effects.extend(self.key(key, buffer));
        }
        self.replay = None;
        self.pending.clear();
        self.last_change = Some(change);
        effects
    }

    fn search_next(&mut self, forward: bool, n: usize, buffer: &Buffer) -> Vec<Effect> {
        let Some(pattern) = self.search.clone() else {
            return vec![Effect::Error("No previous search".into())];
        };
        let text = buffer.text;
        let mut at = if self.mode == Mode::Normal {
            buffer.cursor.min(text.len())
        } else {
            self.head(buffer)
        };
        for _ in 0..n {
            let found = if forward {
                let from = next(text, at);
                text[from..]
                    .find(&pattern)
                    .map(|i| from + i)
                    .or_else(|| text.find(&pattern))
            } else {
                text[..at].rfind(&pattern).or_else(|| text.rfind(&pattern))
            };
            match found {
                Some(found) => at = found,
                None => return vec![Effect::Error(format!("Pattern not found: {pattern}"))],
            }
        }
        if self.mode == Mode::Normal {
            self.column = None;
            vec![self.go(at)]
        } else {
            self.jump(at, text)
        }
    }
}

/// The start of the line `down` lines below the one of `at` (or the
/// last line).
fn line_at_offset(text: &str, at: usize, down: usize) -> usize {
    let mut start = line_start(text, at);
    for _ in 0..down {
        let end = line_end(text, start);
        if end >= text.len() {
            break;
        }
        start = end + 1;
    }
    start
}

/// The lines from the one of `first` to the one of `last`, without the
/// line break that ends the last.
fn lines(text: &str, first: usize, last: usize) -> Range<usize> {
    line_start(text, first)..line_end(text, last)
}

/// Where Normal mode's cursor is once `range` is deleted, in the new text.
fn cursor_after_delete(text: &str, range: &Range<usize>, linewise: bool) -> usize {
    if linewise {
        if range.end < text.len() {
            // The line now at the start is the one that followed.
            range.start + (first_non_blank(text, range.end) - range.end)
        } else if range.start > 0 {
            first_non_blank(text, line_start(text, range.start))
        } else {
            0
        }
    } else {
        let rest = &text[range.end..line_end(text, range.end)];
        if !rest.is_empty() || range.start == line_start(text, range.start) {
            range.start
        } else {
            prev(text, range.start)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An editor as the app drives it: effects applied in order, typing in
    /// Insert mode goes to the text, undo restores whole edits.
    struct Editor {
        text: String,
        cursor: usize,
        selection: Option<Range<usize>>,
        clipboard: Option<String>,
        undo: Vec<(String, usize)>,
        redo: Vec<(String, usize)>,
        vim: Vim,
        effects: Vec<Effect>,
    }

    /// An editor holding `marked`, the cursor at its `|`.
    fn editor(marked: &str) -> Editor {
        let cursor = marked.find('|').expect("a cursor mark");
        Editor {
            text: marked.replacen('|', "", 1),
            cursor,
            selection: None,
            clipboard: None,
            undo: Vec::new(),
            redo: Vec::new(),
            vim: Vim::new(),
            effects: Vec::new(),
        }
    }

    fn parse_keys(keys: &str) -> Vec<Key> {
        let mut out = Vec::new();
        let mut rest = keys;
        while let Some(c) = rest.chars().next() {
            let named = [
                ("<Esc>", Key::Escape),
                ("<CR>", Key::Enter),
                ("<BS>", Key::Backspace),
                ("<C-r>", Key::Ctrl('r')),
            ]
            .into_iter()
            .find(|(name, _)| rest.starts_with(name));
            match named {
                Some((name, key)) => {
                    out.push(key);
                    rest = &rest[name.len()..];
                }
                None => {
                    out.push(Key::Char(c));
                    rest = &rest[c.len_utf8()..];
                }
            }
        }
        out
    }

    impl Editor {
        fn apply(&mut self, effects: Vec<Effect>) {
            for effect in effects {
                match &effect {
                    Effect::Move(at) => {
                        self.cursor = *at;
                        self.selection = None;
                    }
                    Effect::Select { range, reversed } => {
                        self.cursor = if *reversed { range.start } else { range.end };
                        self.selection = Some(range.clone());
                    }
                    Effect::Replace { range, text } => {
                        self.undo.push((self.text.clone(), self.cursor));
                        self.redo.clear();
                        self.text.replace_range(range.clone(), text);
                        self.cursor = range.start + text.len();
                        self.selection = None;
                    }
                    Effect::Undo => {
                        if let Some((text, cursor)) = self.undo.pop() {
                            self.redo
                                .push((std::mem::replace(&mut self.text, text), self.cursor));
                            self.cursor = cursor;
                        }
                    }
                    Effect::Redo => {
                        if let Some((text, cursor)) = self.redo.pop() {
                            self.undo
                                .push((std::mem::replace(&mut self.text, text), self.cursor));
                            self.cursor = cursor;
                        }
                    }
                    Effect::Yank(text) => self.clipboard = Some(text.clone()),
                    _ => {}
                }
                self.effects.push(effect);
            }
        }

        /// Presses keys; in Insert mode, what is not a named key is typed.
        fn keys(&mut self, keys: &str) -> &mut Self {
            for key in parse_keys(keys) {
                match (self.vim.mode(), key) {
                    (Mode::Insert, Key::Char(c)) => {
                        // Typing coalesces into one undo step, as the kit's.
                        let at = self.cursor;
                        self.text.insert(at, c);
                        self.cursor = at + c.len_utf8();
                    }
                    _ => {
                        let buffer = Buffer {
                            text: &self.text,
                            cursor: self.cursor,
                            clipboard: self.clipboard.as_deref(),
                            rules: LexRules::POSTGRES,
                        };
                        let effects = self.vim.key(key, &buffer);
                        self.apply(effects);
                    }
                }
            }
            self
        }

        fn command(&mut self, kind: char, line: &str) -> &mut Self {
            let buffer = Buffer {
                text: &self.text,
                cursor: self.cursor,
                clipboard: self.clipboard.as_deref(),
                rules: LexRules::POSTGRES,
            };
            let effects = self.vim.command(kind, line, &buffer);
            self.apply(effects);
            self
        }

        /// The text with `|` at the cursor.
        fn shown(&self) -> String {
            let mut shown = self.text.clone();
            shown.insert(self.cursor, '|');
            shown
        }

        fn selected(&self) -> Option<&str> {
            self.selection.clone().map(|range| &self.text[range])
        }
    }

    fn after(marked: &str, keys: &str) -> String {
        editor(marked).keys(keys).shown()
    }

    // Motions -----------------------------------------------------------------

    #[test]
    fn h_moves_left_and_stops_at_the_line_start() {
        assert_eq!(after("ab|c", "h"), "a|bc");
        assert_eq!(after("ab\n|cd", "h"), "ab\n|cd");
        assert_eq!(after("abc|d", "2h"), "a|bcd");
        assert_eq!(after("abc|d", "9h"), "|abcd");
        assert_eq!(after("abc|d", "<BS>"), "ab|cd");
    }

    #[test]
    fn l_moves_right_and_stays_on_the_last_character() {
        assert_eq!(after("|abc", "l"), "a|bc");
        assert_eq!(after("ab|c\nd", "l"), "ab|c\nd");
        assert_eq!(after("|abcd", "3l"), "abc|d");
        assert_eq!(after("|abcd", "9l"), "abc|d");
    }

    #[test]
    fn j_moves_down_keeping_the_column() {
        assert_eq!(after("a|bc\ndef", "j"), "abc\nd|ef");
        assert_eq!(after("ab|c\nd\nefg", "j"), "abc\n|d\nefg");
        // The column is kept across a shorter line.
        assert_eq!(after("ab|c\nd\nefg", "jj"), "abc\nd\nef|g");
        assert_eq!(after("|a\nb", "j"), "a\n|b");
        assert_eq!(after("a\n|b", "j"), "a\n|b");
    }

    #[test]
    fn k_moves_up_keeping_the_column() {
        assert_eq!(after("abc\nd|ef", "k"), "a|bc\ndef");
        assert_eq!(after("abc\nd\nef|g", "kk"), "ab|c\nd\nefg");
        assert_eq!(after("|abc", "k"), "|abc");
    }

    #[test]
    fn a_count_repeats_a_motion() {
        assert_eq!(after("|1\n2\n3\n4\n5", "3j"), "1\n2\n3\n|4\n5");
        assert_eq!(after("1\n2\n3\n4\n|5", "2k"), "1\n2\n|3\n4\n5");
        assert_eq!(after("|a b c d", "2w"), "a b |c d");
        assert_eq!(
            after("|1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12", "11j"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n|12"
        );
    }

    #[test]
    fn w_goes_to_the_next_word() {
        assert_eq!(after("|select a.id from t", "w"), "select |a.id from t");
        assert_eq!(after("select |a.id from t", "w"), "select a|.id from t");
        assert_eq!(after("select a|.id from t", "w"), "select a.|id from t");
        assert_eq!(after("|one\n  two", "w"), "one\n  |two");
        // An empty line is a word.
        assert_eq!(after("|one\n\ntwo", "w"), "one\n|\ntwo");
    }

    #[test]
    fn b_goes_back_to_the_start_of_a_word() {
        assert_eq!(after("select a.i|d", "b"), "select a.|id");
        assert_eq!(after("select a.|id", "b"), "select a|.id");
        assert_eq!(after("select |a", "b"), "|select a");
        assert_eq!(after("one\n  |two", "b"), "|one\n  two");
        assert_eq!(after("|one", "b"), "|one");
    }

    #[test]
    fn e_goes_to_the_end_of_a_word() {
        assert_eq!(after("|select a", "e"), "selec|t a");
        assert_eq!(after("selec|t abc", "e"), "select ab|c");
        assert_eq!(after("|a.id", "e"), "a|.id");
        assert_eq!(after("ab|c", "e"), "ab|c");
    }

    #[test]
    fn zero_goes_to_the_line_start() {
        assert_eq!(after("x\n  ab|c", "0"), "x\n|  abc");
    }

    #[test]
    fn caret_goes_to_the_first_non_blank() {
        assert_eq!(after("x\n \tab|c", "^"), "x\n \t|abc");
        assert_eq!(after("x\n|  abc", "^"), "x\n  |abc");
    }

    #[test]
    fn dollar_goes_to_the_last_character_and_sticks_to_line_ends() {
        assert_eq!(after("|abc\nd", "$"), "ab|c\nd");
        assert_eq!(after("|abc\nde\nfghi", "2$"), "abc\nd|e\nfghi");
        // `$` then `j` stays at the end of each line.
        assert_eq!(after("|abc\nde\nfghi", "$jj"), "abc\nde\nfgh|i");
    }

    #[test]
    fn gg_goes_to_the_first_line_or_to_line_count() {
        assert_eq!(after("a\n  b\nc|c", "gg"), "|a\n  b\ncc");
        assert_eq!(after("a\n  b\nc|c", "2gg"), "a\n  |b\ncc");
    }

    #[test]
    fn capital_g_goes_to_the_last_line_or_to_line_count() {
        assert_eq!(after("|a\nb\n  c", "G"), "a\nb\n  |c");
        assert_eq!(after("|a\nb\n  c", "2G"), "a\n|b\n  c");
        assert_eq!(after("|a\nb", "9G"), "a\n|b");
    }

    #[test]
    fn f_goes_to_a_character_on_the_line() {
        assert_eq!(after("|a, b, c", "f,"), "a|, b, c");
        assert_eq!(after("|a, b, c", "2f,"), "a, b|, c");
        assert_eq!(after("|a, b\n,", "2f,"), "|a, b\n,");
        assert_eq!(after("|", "fx"), "|");
    }

    #[test]
    fn t_goes_before_a_character_on_the_line() {
        assert_eq!(after("|count(id)", "t("), "coun|t(id)");
        assert_eq!(after("|a)", "t)"), "|a)");
        assert_eq!(after("|abc", "tz"), "|abc");
    }

    #[test]
    fn percent_goes_to_the_matching_bracket() {
        assert_eq!(after("select |(a, (b))", "%"), "select (a, (b)|)");
        assert_eq!(after("select (a, (b)|)", "%"), "select |(a, (b))");
        // The next bracket on the line when the cursor is not on one.
        assert_eq!(after("|f(x) + 1", "%"), "f(x|) + 1");
        // Brackets in strings are text.
        assert_eq!(after("|(')') ", "%"), "(')'|) ");
        assert_eq!(after("|abc", "%"), "|abc");
    }

    #[test]
    fn enter_goes_to_the_first_non_blank_of_the_next_line() {
        assert_eq!(after("a|b\n  cd", "<CR>"), "ab\n  |cd");
    }

    #[test]
    fn the_mouse_or_an_arrow_moving_the_cursor_forgets_the_column() {
        let mut ed = editor("ab|c\nd\nefg");
        ed.keys("j");
        assert_eq!(ed.shown(), "abc\n|d\nefg");
        // An arrow puts the cursor elsewhere: `j` starts from there.
        ed.cursor = 0;
        ed.keys("jj");
        assert_eq!(ed.shown(), "abc\nd\n|efg");
        // A cursor on a line break (End, a click past the end) is put back
        // on the last character.
        let mut ed = editor("abc|\ndef");
        ed.keys("x");
        assert_eq!(ed.shown(), "a|b\ndef");
    }

    // Operators ---------------------------------------------------------------

    #[test]
    fn d_with_a_motion_deletes_what_it_covers() {
        assert_eq!(after("|one two", "dw"), "|two");
        // The last word of a line: up to the line break, not past it.
        assert_eq!(after("one |two\nx", "dw"), "one| \nx");
        assert_eq!(after("a|bc def", "d$"), "|a");
        assert_eq!(after("abc |def", "d0"), "|def");
        assert_eq!(after("|a(b)c", "dfb"), "|)c");
        assert_eq!(after("|a(b)c", "dt)"), "|)c");
        assert_eq!(after("x |(a) y", "d%"), "x | y");
        assert_eq!(after("|one two three", "d2w"), "|three");
        assert_eq!(after("|one two three", "2dw"), "|three");
        assert_eq!(after("abc |def", "db"), "|def");
        assert_eq!(after("|abc def", "de"), "| def");
        // Nothing to delete: the clipboard keeps what it holds.
        let mut ed = editor("|abc");
        ed.clipboard = Some("kept".into());
        ed.keys("dfz");
        assert_eq!(
            (ed.shown().as_str(), ed.clipboard.as_deref()),
            ("|abc", Some("kept"))
        );
    }

    #[test]
    fn d_with_a_line_motion_deletes_whole_lines() {
        let mut ed = editor("a\nb|b\nc\nd");
        ed.keys("dj");
        assert_eq!(ed.shown(), "a\n|d");
        assert_eq!(ed.clipboard.as_deref(), Some("bb\nc\n"));
        assert_eq!(after("a\nb\n|c", "dk"), "|a");
        assert_eq!(after("a\n|b\nc", "dG"), "|a");
        assert_eq!(after("a\n|b\nc", "dgg"), "|c");
    }

    #[test]
    fn dd_deletes_lines_and_yanks_them_as_lines() {
        let mut ed = editor("one\n  t|wo\nthree");
        ed.keys("dd");
        assert_eq!(ed.shown(), "one\n|three");
        assert_eq!(ed.clipboard.as_deref(), Some("  two\n"));
        assert_eq!(after("a\nb\n|c", "dd"), "a\n|b");
        assert_eq!(after("|only", "dd"), "|");
        assert_eq!(after("|a\nb\nc\nd", "2dd"), "|c\nd");
        assert_eq!(after("a\n|\nb", "dd"), "a\n|b");
    }

    #[test]
    fn c_with_a_motion_deletes_and_types() {
        assert_eq!(after("|one two", "cwnew<Esc>"), "ne|w two");
        assert_eq!(after("|a two", "cwx<Esc>"), "|x two");
        assert_eq!(after("se|lect a", "c$x<Esc>"), "se|x");
        assert_eq!(after("|one two three", "c2wx<Esc>"), "|x three");
        assert_eq!(after("|ab cd", "ceX<Esc>"), "|X cd");
    }

    #[test]
    fn ciw_changes_the_word_under_the_cursor() {
        let mut ed = editor("select na|me from t");
        ed.keys("ciwtitle<Esc>");
        assert_eq!(ed.shown(), "select titl|e from t");
        assert_eq!(ed.vim.mode(), Mode::Normal);
        assert_eq!(ed.clipboard.as_deref(), Some("name"));
        assert_eq!(after("a fo|o b", "daw"), "a |b");
        assert_eq!(after("a fo|o", "daw"), "|a");
    }

    #[test]
    fn cc_changes_whole_lines_keeping_one_to_type_on() {
        assert_eq!(after("a\n  b|b\nc", "ccx<Esc>"), "a\n|x\nc");
        assert_eq!(after("a\n|b\nc", "2ccx<Esc>"), "a\n|x");
        let mut ed = editor("a\n|b");
        ed.keys("cc");
        assert_eq!((ed.shown().as_str(), ed.vim.mode()), ("a\n|", Mode::Insert));
        assert_eq!(ed.clipboard.as_deref(), Some("b\n"));
    }

    #[test]
    fn y_with_a_motion_yanks_without_changing_the_text() {
        let mut ed = editor("a|bc def");
        ed.keys("yw");
        assert_eq!(ed.shown(), "a|bc def");
        assert_eq!(ed.clipboard.as_deref(), Some("bc "));
        let mut ed = editor("abc d|ef");
        ed.keys("yb");
        assert_eq!(
            (ed.shown().as_str(), ed.clipboard.as_deref()),
            ("abc |def", Some("d"))
        );
        let mut ed = editor("a\nb|b\nc");
        ed.keys("yk");
        assert_eq!(
            (ed.shown().as_str(), ed.clipboard.as_deref()),
            ("|a\nbb\nc", Some("a\nbb\n"))
        );
    }

    #[test]
    fn yy_yanks_lines_and_p_puts_them_below() {
        let mut ed = editor("o|ne\ntwo");
        ed.keys("yyp");
        assert_eq!(ed.shown(), "one\n|one\ntwo");
        assert_eq!(after("|a\nb\nc", "2yyGp"), "a\nb\nc\n|a\nb");
        assert_eq!(after("a\n|b", "yyp"), "a\nb\n|b");
    }

    #[test]
    fn x_deletes_the_characters_under_the_cursor() {
        assert_eq!(after("a|bc", "x"), "a|c");
        assert_eq!(after("ab|c", "x"), "a|b");
        assert_eq!(after("a|bcde", "3x"), "a|e");
        assert_eq!(after("a|bc\nd", "9x"), "|a\nd");
        assert_eq!(after("a\n|\nb", "x"), "a\n|\nb");
    }

    #[test]
    fn p_puts_characters_after_the_cursor() {
        let mut ed = editor("|ab");
        ed.clipboard = Some("XY".into());
        ed.keys("p");
        assert_eq!(ed.shown(), "aX|Yb");
        let mut ed = editor("|ab");
        ed.clipboard = Some("X".into());
        ed.keys("3p");
        assert_eq!(ed.shown(), "aXX|Xb");
    }

    #[test]
    fn capital_p_puts_before_the_cursor() {
        let mut ed = editor("a|b");
        ed.clipboard = Some("XY".into());
        ed.keys("P");
        assert_eq!(ed.shown(), "aX|Yb");
        assert_eq!(after("a\n|b", "yykP"), "|b\na\nb");
    }

    #[test]
    fn ddp_swaps_two_lines() {
        assert_eq!(after("|one\ntwo\nthree", "ddp"), "two\n|one\nthree");
    }

    #[test]
    fn the_register_is_linewise_only_while_the_clipboard_holds_vims_lines() {
        // Copied elsewhere (⌘C), the same text pastes as characters.
        let mut ed = editor("a|b");
        ed.clipboard = Some("x\n".into());
        ed.keys("p");
        assert_eq!(ed.shown(), "abx|\n");
        let mut ed = editor("|one\ntwo");
        ed.keys("yy");
        assert_eq!(ed.clipboard.as_deref(), Some("one\n"));
        ed.clipboard = Some("zz".into());
        ed.keys("j0p");
        assert_eq!(ed.shown(), "one\ntz|zwo");
        // A characterwise yank pastes as characters even with a line break.
        let mut ed = editor("|ab\ncd");
        ed.keys("vjy$p");
        assert_eq!(ed.clipboard.as_deref(), Some("ab\nc"));
        assert_eq!(ed.shown(), "abab\n|c\ncd");
    }

    #[test]
    fn u_undoes_and_ctrl_r_redoes_one_command_each() {
        let mut ed = editor("|a\nb\nc");
        ed.keys("dd");
        ed.keys("3x");
        assert_eq!(ed.text, "\nc");
        ed.keys("u");
        assert_eq!(ed.text, "b\nc");
        ed.keys("u");
        assert_eq!(ed.text, "a\nb\nc");
        ed.keys("<C-r>");
        assert_eq!(ed.text, "b\nc");
        ed.keys("2u");
        assert_eq!(ed.effects.iter().filter(|e| **e == Effect::Undo).count(), 4);
    }

    #[test]
    fn dot_repeats_the_last_change() {
        assert_eq!(after("|a b c d", "dw."), "|c d");
        assert_eq!(after("|abcdef", "2x."), "|ef");
        assert_eq!(after("|abcdef", "x3."), "|ef");
        assert_eq!(after("|a\nb\nc\nd", "dd."), "|c\nd");
        // A change that typed text types it again.
        assert_eq!(after("|one two", "cwX<Esc>w."), "X |X");
        assert_eq!(after("|a\nb", "ciwZZ<Esc>j0."), "ZZ\nZ|Z");
        assert_eq!(after("|a\nb", "A;<Esc>j."), "a;\nb|;");
        assert_eq!(after("|a", "oz<Esc>."), "a\nz\n|z");
        // Moving and yanking are no changes.
        assert_eq!(after("|abc", "xlyl."), "|b");
    }

    #[test]
    fn i_and_a_insert_before_and_after_the_cursor() {
        assert_eq!(after("a|c", "ib<Esc>"), "a|bc");
        assert_eq!(after("a|c", "ab<Esc>"), "ac|b");
        assert_eq!(after("|", "ax<Esc>"), "|x");
        let mut ed = editor("a|b");
        ed.keys("i");
        assert_eq!(ed.vim.mode(), Mode::Insert);
        ed.keys("<Esc>");
        assert_eq!((ed.shown().as_str(), ed.vim.mode()), ("|ab", Mode::Normal));
    }

    #[test]
    fn capital_i_and_a_insert_at_the_line_ends() {
        assert_eq!(after("x\n  a|b\ny", "I-<Esc>"), "x\n  |-ab\ny");
        assert_eq!(after("x\n  a|b\ny", "A;<Esc>"), "x\n  ab|;\ny");
    }

    #[test]
    fn o_and_capital_o_open_a_line_with_the_same_indent() {
        assert_eq!(after("  a|b\nc", "ox<Esc>"), "  ab\n  |x\nc");
        assert_eq!(after("a\n  b|c", "Ox<Esc>"), "a\n  |x\n  bc");
        let mut ed = editor("|a");
        ed.keys("o");
        assert_eq!((ed.shown().as_str(), ed.vim.mode()), ("a\n|", Mode::Insert));
    }

    #[test]
    fn v_selects_characters_and_operators_take_the_selection() {
        let mut ed = editor("a|bcd");
        ed.keys("vl");
        assert_eq!((ed.vim.mode(), ed.selected()), (Mode::Visual, Some("bc")));
        ed.keys("d");
        assert_eq!((ed.shown().as_str(), ed.vim.mode()), ("a|d", Mode::Normal));
        assert_eq!(ed.clipboard.as_deref(), Some("bc"));
        // Backwards, then yank.
        let mut ed = editor("abc|d");
        ed.keys("vhhy");
        assert_eq!(
            (ed.shown().as_str(), ed.clipboard.as_deref()),
            ("a|bcd", Some("bcd"))
        );
        // Change.
        assert_eq!(after("a|bcd", "vlcX<Esc>"), "a|Xd");
        // `x` deletes, `v` again leaves, `Esc` too.
        assert_eq!(after("a|bcd", "vlx"), "a|d");
        let mut ed = editor("a|bcd");
        ed.keys("vlv");
        assert_eq!((ed.vim.mode(), ed.selected()), (Mode::Normal, None));
        // Esc leaves the cursor on the selection's head.
        ed.keys("vl<Esc>");
        assert_eq!(
            (ed.shown().as_str(), ed.vim.mode()),
            ("abc|d", Mode::Normal)
        );
    }

    #[test]
    fn capital_v_selects_lines() {
        let mut ed = editor("a\nb|b\nc\nd");
        ed.keys("Vj");
        assert_eq!(
            (ed.vim.mode(), ed.selected()),
            (Mode::VisualLine, Some("bb\nc\n"))
        );
        ed.keys("d");
        assert_eq!(ed.shown(), "a\n|d");
        assert_eq!(ed.clipboard.as_deref(), Some("bb\nc\n"));
        assert_eq!(after("a\n|b\nc", "Vyjp"), "a\nb\nc\n|b");
        // From Visual to Visual line and back.
        let mut ed = editor("|ab\ncd");
        ed.keys("vV");
        assert_eq!(ed.vim.mode(), Mode::VisualLine);
        ed.keys("v");
        assert_eq!((ed.vim.mode(), ed.selected()), (Mode::Visual, Some("a")));
    }

    #[test]
    fn an_arrow_in_visual_mode_moves_the_selections_head() {
        let mut ed = editor("|abcd");
        ed.keys("v");
        // An arrow collapsed the editor's selection to offset 2.
        ed.cursor = 2;
        ed.selection = None;
        ed.keys("l");
        assert_eq!(ed.selected(), Some("abcd"));
    }

    #[test]
    fn escape_cancels_a_pending_command() {
        assert_eq!(after("|abc", "d<Esc>x"), "|bc");
        assert_eq!(after("|abc", "2<Esc>x"), "|bc");
        assert_eq!(after("|abc", "dz"), "|abc");
        let mut ed = editor("|abc");
        ed.keys("<Esc>");
        assert!(ed.effects.is_empty());
    }

    #[test]
    fn slash_searches_and_n_and_capital_n_repeat() {
        let mut ed = editor("|select id from t where id = 1");
        ed.keys("/");
        assert_eq!(ed.effects, vec![Effect::Prompt('/')]);
        ed.command('/', "id");
        assert_eq!(ed.shown(), "select |id from t where id = 1");
        ed.keys("n");
        assert_eq!(ed.shown(), "select id from t where |id = 1");
        // It wraps around.
        ed.keys("n");
        assert_eq!(ed.shown(), "select |id from t where id = 1");
        ed.keys("N");
        assert_eq!(ed.shown(), "select id from t where |id = 1");
        ed.command('/', "nothing");
        assert_eq!(
            ed.effects.last(),
            Some(&Effect::Error("Pattern not found: nothing".into()))
        );
        // An empty pattern searches the last one again.
        ed.command('/', "");
        assert_eq!(
            ed.effects.last(),
            Some(&Effect::Error("Pattern not found: nothing".into()))
        );
        assert_eq!(
            editor("|a").keys("n").effects,
            vec![Effect::Error("No previous search".into())]
        );
    }

    #[test]
    fn colon_commands_write_and_close() {
        let mut ed = editor("|a");
        ed.keys(":");
        assert_eq!(ed.effects, vec![Effect::Prompt(':')]);
        let run = |line: &str| editor("|a").command(':', line).effects.clone();
        assert_eq!(run("w"), vec![Effect::Write { then_close: false }]);
        assert_eq!(run(" wq "), vec![Effect::Write { then_close: true }]);
        assert_eq!(run("x"), vec![Effect::Write { then_close: true }]);
        assert_eq!(run("q"), vec![Effect::Quit { force: false }]);
        assert_eq!(run("q!"), vec![Effect::Quit { force: true }]);
        assert_eq!(
            run("s/a/b/"),
            vec![Effect::Error("Not an editor command: s/a/b/".into())]
        );
        assert_eq!(run(""), vec![]);
    }

    #[test]
    fn multi_byte_text_moves_and_edits_by_character() {
        assert_eq!(after("é|ü😀", "l"), "éü|😀");
        assert_eq!(after("éü|😀", "h"), "é|ü😀");
        assert_eq!(after("é|ü😀a", "x"), "é|😀a");
        assert_eq!(after("é|😀a", "x"), "é|a");
        assert_eq!(after("|日本 語", "w"), "日本 |語");
        assert_eq!(after("|日本 語", "e"), "日|本 語");
        assert_eq!(after("naïve |café", "ciwthé<Esc>"), "naïve th|é");
        assert_eq!(after("ab|c\n日本語", "j"), "abc\n日本|語");
        assert_eq!(after("|(é)", "%"), "(é|)");
        assert_eq!(after("|aé, b", "f,"), "aé|, b");
        let mut ed = editor("|é\n😀");
        ed.keys("yyjp");
        assert_eq!(ed.shown(), "é\n😀\n|é");
        assert_eq!(editor("a|é").vim.block("aé", 1), Some(1..3));
    }

    #[test]
    fn the_block_covers_the_character_under_the_cursor_in_normal_mode() {
        let mut ed = editor("ab|c\n\nd");
        assert_eq!(ed.vim.block(&ed.text, ed.cursor), Some(2..3));
        // On a line break, the last character of the line.
        assert_eq!(ed.vim.block(&ed.text, 3), Some(2..3));
        assert_eq!(ed.vim.block(&ed.text, 4), None);
        ed.keys("i");
        assert_eq!(ed.vim.block(&ed.text, ed.cursor), None);
    }

    #[test]
    fn motions_scan_locally_in_a_large_script() {
        // 10 MB on one line, then a short line: `j`, `$`, `w` and `x` stay
        // cheap (no rescan per key; this would take seconds otherwise).
        let mut text = "x".repeat(10 * 1024 * 1024);
        text.insert_str(0, "|ab cd\n");
        let mut ed = editor(&text);
        let started = std::time::Instant::now();
        for _ in 0..2_000 {
            ed.keys("wbex");
            ed.keys("u");
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }
}
