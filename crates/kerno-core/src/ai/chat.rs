//! A conversation with the assistant (K51, D-120): the messages of a request built from the
//! context, the turns so far and the new message; the reader of an answer that arrives as it is
//! written (server-sent events); the parts of an answer (prose and code blocks). No GPUI: the
//! panel, and any other place that asks the assistant (K52, K53), use these.

use std::io::Read;

use serde_json::{Value, json};

use super::{AiError, Context, strip_reasoning};

/// Earlier turns sent with a message at most, and their characters at most: beyond, the oldest
/// exchanges are left out of what is sent (never summarised), and the thread says so.
pub const MAX_TURNS: usize = 20;
pub const MAX_TURN_CHARS: usize = 40_000;
/// Characters of the editor's query attached to a message; beyond, it is cut and says so.
pub const MAX_QUERY: usize = 20_000;

/// Who wrote a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One message of a conversation, as it was sent (a user's, attachments included) or answered
/// (the model's, without its reasoning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

impl Turn {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
        }
    }
}

/// How many of the oldest of `earlier` a send leaves out: it keeps the newest turns, at most
/// [`MAX_TURNS`] of them and [`MAX_TURN_CHARS`] characters, and never starts with an answer.
pub fn left_out(earlier: &[Turn]) -> usize {
    let (mut kept, mut chars) = (0, 0);
    for turn in earlier.iter().rev() {
        let length = turn.text.chars().count();
        if kept == MAX_TURNS || chars + length > MAX_TURN_CHARS {
            break;
        }
        kept += 1;
        chars += length;
    }
    let mut start = earlier.len() - kept;
    while earlier
        .get(start)
        .is_some_and(|turn| turn.role == Role::Assistant)
    {
        start += 1;
    }
    start
}

/// `message` with the editor's query after it, cut at [`MAX_QUERY`] characters (said in it).
pub fn with_query(message: &str, sql: &str) -> String {
    let sql = sql.trim();
    let shown = if sql.chars().count() > MAX_QUERY {
        let cut: String = sql.chars().take(MAX_QUERY).collect();
        format!("{cut}\n-- (cut here: the query is longer)")
    } else {
        sql.to_string()
    };
    let message = message.trim();
    let lead = if message.is_empty() {
        String::new()
    } else {
        format!("{message}\n\n")
    };
    format!("{lead}The query in my editor:\n```sql\n{shown}\n```")
}

/// The system message: who the model is, its rules, and what it is told of the database, once
/// per request.
fn system(context: &Context) -> String {
    let engine = if context.engine.is_empty() {
        "SQL"
    } else {
        &context.engine
    };
    format!(
        "You are the assistant of Kerno, a database client, for the user's {engine} database. \
         Answer in plain text. When you write SQL, write it for {engine}, using only the tables \
         and columns described below, each statement in a ```sql block. Kerno never runs what \
         you write: the user reads it and decides.\n\n{}",
        context.describe()
    )
}

/// The request body of a conversation, an OpenAI-compatible chat completion that asks for the
/// answer as it is written: the system message with the context, the earlier turns the budget
/// keeps ([`left_out`]), then `message`. The only way a body is made ([`super::build_payload`]
/// is this with no earlier turn).
pub fn build_chat(context: &Context, earlier: &[Turn], message: &str, model: &str) -> Value {
    let mut messages = vec![json!({"role": "system", "content": system(context)})];
    for turn in &earlier[left_out(earlier)..] {
        messages.push(json!({"role": turn.role.name(), "content": turn.text}));
    }
    messages.push(json!({"role": "user", "content": message}));
    json!({"model": model, "stream": true, "messages": messages})
}

/// A server-sent events reader: fed bytes as they come, it gives the `data` of each complete
/// event. Lines may be cut anywhere between two reads.
#[derive(Default)]
pub struct Events {
    line: Vec<u8>,
    data: Option<String>,
}

impl Events {
    /// The data of the events `bytes` complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut done = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                done.extend(self.data.take());
                continue;
            }
            let line = String::from_utf8_lossy(&line);
            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
            if field == "data" {
                let value = value.strip_prefix(' ').unwrap_or(value);
                match &mut self.data {
                    Some(data) => {
                        data.push('\n');
                        data.push_str(value);
                    }
                    None => self.data = Some(value.to_string()),
                }
            }
        }
        done
    }

    /// The last event, when the stream ended without the blank line after it.
    pub fn finish(&mut self) -> Option<String> {
        if !self.line.is_empty() {
            let rest = std::mem::take(&mut self.line);
            let mut events = self.feed(&rest);
            events.extend(self.feed(b"\n\n"));
            return events.pop();
        }
        self.data.take()
    }
}

/// What an event of a streamed completion holds.
enum Chunk {
    Text(String),
    Reasoning,
    Done,
}

fn chunk(data: &str) -> Result<Chunk, AiError> {
    if data.trim() == "[DONE]" {
        return Ok(Chunk::Done);
    }
    let value: Value =
        serde_json::from_str(data).map_err(|error| AiError::Answer(error.to_string()))?;
    if let Some(error) = value.get("error") {
        let message = (error.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        return Err(AiError::Answer(format!("the endpoint said: {message}")));
    }
    let delta = value.pointer("/choices/0/delta");
    if let Some(text) = delta
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return Ok(Chunk::Text(text.to_string()));
    }
    let reasoned = ["reasoning_content", "reasoning"].iter().any(|field| {
        (delta
            .and_then(|delta| delta.get(*field))
            .and_then(Value::as_str))
        .is_some_and(|text| !text.is_empty())
    });
    Ok(if reasoned {
        Chunk::Reasoning
    } else {
        Chunk::Text(String::new())
    })
}

/// Reads a streamed completion from `reader` to its end (or `[DONE]`), telling `on_text` each
/// piece of the answer as it comes (reasoning fields are not read); returns the whole answer
/// without its reasoning. A read error is given back for the caller to name (a stop, the
/// network); nothing of it is lost: `on_text` has seen what came before.
pub fn read_stream(
    mut reader: impl Read,
    on_text: &mut dyn FnMut(&str),
) -> Result<Result<String, AiError>, std::io::Error> {
    let mut events = Events::default();
    let (mut text, mut reasoned) = (String::new(), false);
    let mut buffer = [0; 8192];
    let mut take =
        |data: String, text: &mut String, reasoned: &mut bool| -> Option<Result<(), AiError>> {
            match chunk(&data) {
                Ok(Chunk::Text(piece)) => {
                    if !piece.is_empty() {
                        text.push_str(&piece);
                        on_text(&piece);
                    }
                    None
                }
                Ok(Chunk::Reasoning) => {
                    *reasoned = true;
                    None
                }
                Ok(Chunk::Done) => Some(Ok(())),
                Err(error) => Some(Err(error)),
            }
        };
    'read: loop {
        let read = reader.read(&mut buffer)?;
        let datas = if read == 0 {
            events.finish().into_iter().collect()
        } else {
            events.feed(&buffer[..read])
        };
        for data in datas {
            match take(data, &mut text, &mut reasoned) {
                None => {}
                Some(Ok(())) => break 'read,
                Some(Err(error)) => return Ok(Err(error)),
            }
        }
        if read == 0 {
            break;
        }
    }
    Ok(finished(&text, reasoned))
}

/// The answer of a whole streamed text: its reasoning left out, an empty one said in words.
fn finished(text: &str, reasoned: bool) -> Result<String, AiError> {
    let answer = strip_reasoning(text);
    if answer.is_empty() {
        return Err(AiError::Answer(if reasoned || text.contains("<think>") {
            "the model gave its reasoning but no answer (it may have run out of tokens): ask \
             again, or choose a model that thinks less."
                .into()
        } else {
            "the model's answer is empty.".into()
        }));
    }
    Ok(answer)
}

/// A part of an answer as the panel draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Prose, as written (never markup run, never a link followed).
    Text(String),
    /// A fenced block: its language (lower case, may be empty), its code, and whether its fence
    /// was closed (an answer still arriving, or cut, may leave it open).
    Code {
        language: String,
        code: String,
        closed: bool,
    },
}

impl Segment {
    /// SQL a user may insert: a closed ```sql block, or a closed one with no language.
    pub fn sql(&self) -> Option<&str> {
        match self {
            Self::Code {
                language,
                code,
                closed: true,
            } if language.is_empty() || language == "sql" => Some(code),
            _ => None,
        }
    }
}

/// The parts of `answer`: prose and fenced code blocks (```lang), in order; empty prose left out.
pub fn segments(answer: &str) -> Vec<Segment> {
    let mut parts = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut code: Option<(String, Vec<&str>)> = None;
    let push_prose = |prose: &mut Vec<&str>, parts: &mut Vec<Segment>| {
        let text = prose.join("\n").trim().to_string();
        if !text.is_empty() {
            parts.push(Segment::Text(text));
        }
        prose.clear();
    };
    for line in answer.lines() {
        let fence = line.trim_start().strip_prefix("```");
        match (&mut code, fence) {
            (None, Some(language)) => {
                push_prose(&mut prose, &mut parts);
                code = Some((language.trim().to_lowercase(), Vec::new()));
            }
            (Some(_), Some(_)) => {
                let (language, lines) = code.take().unwrap_or_default();
                parts.push(Segment::Code {
                    language,
                    code: lines.join("\n"),
                    closed: true,
                });
            }
            (Some((_, lines)), None) => lines.push(line),
            (None, None) => prose.push(line),
        }
    }
    push_prose(&mut prose, &mut parts);
    if let Some((language, lines)) = code {
        parts.push(Segment::Code {
            language,
            code: lines.join("\n"),
            closed: false,
        });
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Table;

    fn context() -> Context {
        Context {
            engine: "PostgreSQL".into(),
            schemas: vec!["shop".into()],
            tables: vec![Table {
                schema: "shop".into(),
                name: "orders".into(),
                view: false,
                columns: vec![("id".into(), "bigint".into())],
                foreign_keys: Vec::new(),
                detail: None,
            }],
            ..Context::default()
        }
    }

    fn exchange(n: usize, size: usize) -> [Turn; 2] {
        [
            Turn::user(format!("q{n} {}", "x".repeat(size))),
            Turn::assistant(format!("a{n}")),
        ]
    }

    /// K51: a follow-up goes with the earlier turns in order, the schema once, in the system
    /// message only.
    #[test]
    fn a_follow_up_is_sent_with_the_turns_and_the_schema_once() {
        let earlier = [
            Turn::user("count the orders"),
            Turn::assistant("```sql\nSELECT count(*) FROM shop.orders;\n```"),
        ];
        let payload = build_chat(&context(), &earlier, "by customer", "m");
        assert_eq!(payload["stream"], true);
        let messages = payload["messages"].as_array().unwrap();
        let roles: Vec<&str> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(messages[1]["content"], "count the orders");
        assert_eq!(messages[3]["content"], "by customer");
        let body = String::from_utf8(crate::ai::body(&payload)).unwrap();
        assert_eq!(body.matches("shop.orders (table)").count(), 1, "{body}");
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("```sql block")
        );
    }

    /// K51: beyond the budget the oldest exchanges are left out, whole, and how many is said;
    /// the new message always goes.
    #[test]
    fn the_budget_leaves_out_the_oldest_exchanges_whole() {
        let many: Vec<Turn> = (0..15).flat_map(|n| exchange(n, 10)).collect();
        assert_eq!(left_out(&many), 30 - MAX_TURNS);
        let payload = build_chat(&context(), &many, "next", "m");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1 + MAX_TURNS + 1);
        assert!(messages[1]["content"].as_str().unwrap().starts_with("q5 "));
        // By characters: two exchanges of 25,000 do not fit in 40,000; the newest stays.
        let big: Vec<Turn> = (0..2).flat_map(|n| exchange(n, 25_000)).collect();
        assert_eq!(left_out(&big), 2);
        // An odd count never starts the history with an answer.
        let mut odd = many.clone();
        odd.remove(0);
        let start = left_out(&odd);
        assert_eq!(odd[start].role, Role::User);
        assert_eq!(left_out(&[]), 0);
        assert_eq!(left_out(&many[..4]), 0);
    }

    #[test]
    fn the_editor_query_is_attached_and_cut_when_long() {
        assert_eq!(
            with_query("add the date filter", " select 1 "),
            "add the date filter\n\nThe query in my editor:\n```sql\nselect 1\n```"
        );
        let long = "x".repeat(MAX_QUERY + 5);
        let message = with_query("", &long);
        assert!(message.starts_with("The query in my editor:\n```sql\nxxx"));
        assert!(message.ends_with("\n-- (cut here: the query is longer)\n```"));
        assert_eq!(message.matches('x').count(), MAX_QUERY);
    }

    /// Recorded events of an OpenAI-compatible server (llama.cpp's shape), fed cut anywhere.
    const RECORDED: &str = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
        : keep-alive\n\n\
        data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking ```sql DELETE```\"}}]}\n\n\
        data: {\"choices\":[{\"delta\":{\"content\":\"Here:\\n```sql\\nSELECT \"}}]}\r\n\r\n\
        data: {\"choices\":[{\"delta\":{\"content\":\"1;\\n```é\"}}]}\n\n\
        data: [DONE]\n\n\
        data: {\"choices\":[{\"delta\":{\"content\":\"after the end\"}}]}\n\n";

    /// Gives its bytes `size` at a time.
    struct Trickle<'a>(&'a [u8], usize);

    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let n = self.1.min(self.0.len()).min(buffer.len());
            buffer[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_stream_cut_anywhere_reads_the_same() {
        for size in [1, 2, 3, 7, 64, 8192] {
            let mut pieces = Vec::new();
            let answer = read_stream(Trickle(RECORDED.as_bytes(), size), &mut |piece| {
                pieces.push(piece.to_string())
            })
            .unwrap()
            .unwrap();
            assert_eq!(answer, "Here:\n```sql\nSELECT 1;\n```é", "{size}");
            assert_eq!(pieces, ["Here:\n```sql\nSELECT ", "1;\n```é"], "{size}");
        }
        // The last event without its blank line still counts.
        let mut events = Events::default();
        assert!(events.feed(b"data: a\ndata: b").is_empty());
        assert_eq!(events.finish().as_deref(), Some("a\nb"));
    }

    /// A read that fails in the middle (Stop closes the connection) gives the error back; what
    /// came before was told.
    #[test]
    fn a_stream_stopped_in_the_middle_says_so_after_what_came() {
        struct Broken<'a>(Trickle<'a>);
        impl Read for Broken<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                match self.0.read(buffer)? {
                    0 => Err(std::io::ErrorKind::ConnectionAborted.into()),
                    n => Ok(n),
                }
            }
        }
        let first = "data: {\"choices\":[{\"delta\":{\"content\":\"SELECT\"}}]}\n\n";
        let mut pieces = Vec::new();
        let read = read_stream(Broken(Trickle(first.as_bytes(), 5)), &mut |piece| {
            pieces.push(piece.to_string())
        });
        assert_eq!(
            read.unwrap_err().kind(),
            std::io::ErrorKind::ConnectionAborted
        );
        assert_eq!(pieces, ["SELECT"]);
    }

    #[test]
    fn an_error_event_and_an_answer_of_reasoning_only_are_said() {
        let error = "data: {\"error\":{\"message\":\"model not loaded\"}}\n\n";
        let said = read_stream(error.as_bytes(), &mut |_| {})
            .unwrap()
            .unwrap_err();
        assert!(said.to_string().contains("model not loaded"), "{said}");
        let thinking =
            "data: {\"choices\":[{\"delta\":{\"content\":\"<think>long\"}}]}\n\ndata: [DONE]\n\n";
        let said = read_stream(thinking.as_bytes(), &mut |_| {})
            .unwrap()
            .unwrap_err();
        assert!(
            said.to_string().contains("reasoning but no answer"),
            "{said}"
        );
    }

    #[test]
    fn an_answer_is_prose_and_code_blocks() {
        let parts =
            segments("Here:\n```sql\nSELECT 1;\n```\nThen:\n```python\nx\n```\n```SQL\nSELECT 2");
        assert_eq!(
            parts,
            [
                Segment::Text("Here:".into()),
                Segment::Code {
                    language: "sql".into(),
                    code: "SELECT 1;".into(),
                    closed: true
                },
                Segment::Text("Then:".into()),
                Segment::Code {
                    language: "python".into(),
                    code: "x".into(),
                    closed: true
                },
                Segment::Code {
                    language: "sql".into(),
                    code: "SELECT 2".into(),
                    closed: false
                },
            ]
        );
        let sql: Vec<&str> = parts.iter().filter_map(Segment::sql).collect();
        assert_eq!(sql, ["SELECT 1;"], "only closed SQL blocks are offered");
        assert_eq!(segments("plain"), [Segment::Text("plain".into())]);
        assert_eq!(
            segments("```\nSELECT 3\n```")[0].sql(),
            Some("SELECT 3"),
            "a block with no language"
        );
    }
}
