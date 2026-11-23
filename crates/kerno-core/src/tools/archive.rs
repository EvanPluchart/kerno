//! What a PostgreSQL archive needs from the server it is restored into
//! (K39, D-112): the extensions it creates and the roles that own its
//! objects, read from `pg_restore --list` before the run; and the
//! extensions a failed restore could not create, read from its output.
//!
//! `pg_restore --list` prints a few `;` comment lines, then one line per
//! entry of the archive's table of contents: `<id>; <tableoid> <oid> <desc>
//! <schema or -> <name> <owner>`, the owner empty (the line then ends in a
//! space) for entries without one, such as an extension. Names are printed
//! bare, except in a comment's name (`COMMENT - EXTENSION "uuid-ossp"`),
//! which pg_dump quotes as SQL does. A list with lines commented out is
//! what pg_restore's `--use-list` takes to leave those entries out.

use std::collections::BTreeSet;

use super::roles::{failed_statements, named_in};
use crate::sql::{LexRules, TokenKind, lex};

/// The part of an entry line after its id and catalog ids: `EXTENSION -
/// pg_trgm `, `TABLE public t kerno`. `None` for a comment line.
fn entry(line: &str) -> Option<&str> {
    let (id, rest) = line.split_once("; ")?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut fields = rest.splitn(3, ' ');
    let numeric =
        |field: Option<&str>| field.is_some_and(|f| f.bytes().all(|b| b.is_ascii_digit()));
    if !numeric(fields.next()) || !numeric(fields.next()) {
        return None;
    }
    fields.next()
}

/// The database the archive was dumped from (`;     dbname: shop`).
pub fn list_database(list: &str) -> Option<String> {
    list.lines()
        .take_while(|line| line.starts_with(';'))
        .find_map(|line| {
            line.trim_start_matches(';')
                .trim()
                .strip_prefix("dbname:")
                .map(|name| name.trim().to_string())
        })
}

/// The extensions the archive creates, in its order.
pub fn list_extensions(list: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in list.lines() {
        if let Some(name) = entry(line).and_then(|rest| rest.strip_prefix("EXTENSION - ")) {
            let name = name.trim_end().to_string();
            if !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

/// The extension an entry line is about (its `EXTENSION` entry or the
/// comment on it), if any.
fn extension_of(line: &str) -> Option<String> {
    let rest = entry(line)?;
    if let Some(name) = rest.strip_prefix("EXTENSION - ") {
        return Some(name.trim_end().to_string());
    }
    let quoted = rest.strip_prefix("COMMENT - EXTENSION ")?.trim_end();
    Some(
        match quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
            Some(inner) => inner.replace("\"\"", "\""),
            None => quoted.to_string(),
        },
    )
}

/// The list with the entries of `extensions` (each one's `EXTENSION` entry
/// and the comment on it) commented out, for `pg_restore --use-list`.
pub fn list_without(list: &str, extensions: &[String]) -> String {
    let mut out = String::with_capacity(list.len() + 64);
    for line in list.lines() {
        if extension_of(line).is_some_and(|name| extensions.contains(&name)) {
            out.push(';');
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The owners the archive names that are not among `roles` (the server's
/// roles), sorted. An owner is the last field of an entry; it is matched
/// against the whole end of the line, so a role whose name holds a space
/// is found when the server has it.
// ponytail: a missing role whose name holds a space is named by its last
// word only (the list prints names bare); read the archive's SQL if it
// matters.
pub fn missing_owners(list: &str, roles: &[String]) -> Vec<String> {
    let mut missing = BTreeSet::new();
    for rest in list.lines().filter_map(entry) {
        if rest.ends_with(' ') || roles.iter().any(|role| rest.ends_with(&format!(" {role}"))) {
            continue;
        }
        if let Some(owner) = rest.rsplit(' ').next().filter(|owner| !owner.is_empty()) {
            missing.insert(owner.to_string());
        }
    }
    missing.into_iter().collect()
}

/// The extensions whose `CREATE EXTENSION` failed in a restore's `log`, the
/// server's message naming them (in any language: the name is never
/// translated). `script_line` as for [`super::refused_roles`].
pub fn refused_extensions<S: AsRef<str>>(
    log: &[S],
    script_line: impl FnMut(u64) -> Option<String>,
) -> Vec<String> {
    let mut found = Vec::new();
    for (statement, error) in failed_statements(log, script_line) {
        if let Some(name) = created_extension(&statement)
            && named_in(&error, &name)
            && !found.contains(&name)
        {
            found.push(name);
        }
    }
    found
}

/// The extension `CREATE EXTENSION [IF NOT EXISTS] name …` creates.
fn created_extension(statement: &str) -> Option<String> {
    let tokens: Vec<(TokenKind, &str)> = lex(statement, LexRules::POSTGRES)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| (token.kind, &statement[token.range]))
        .collect();
    let word = |at: usize, word: &str| matches!(tokens.get(at), Some((TokenKind::Word, text)) if text.eq_ignore_ascii_case(word));
    if !(word(0, "CREATE") && word(1, "EXTENSION")) {
        return None;
    }
    let at = if word(2, "IF") && word(3, "NOT") && word(4, "EXISTS") {
        5
    } else {
        2
    };
    match tokens.get(at)? {
        (TokenKind::Word, text) => Some(text.to_lowercase()),
        (TokenKind::QuotedIdent, text) if text.len() >= 2 && text.ends_with('"') => {
            Some(text[1..text.len() - 1].replace("\"\"", "\""))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pg_restore --list` (pg_restore 18.7) of a custom archive made by
    /// pg_dump 17 from a scratch database with two extensions, one whose
    /// name needs quotes. An entry without an owner ends in a space.
    const LIST: &str = concat!(
        "\
;
; Archive created at 2026-11-23 15:23:06 CET
;     dbname: kerno_test_k39_probe
;     TOC Entries: 11
;     Compression: gzip
;     Dump Version: 1.16-0
;     Format: CUSTOM
;     Integer: 4 bytes
;     Offset: 8 bytes
;     Dumped from database version: 17.11 (Debian 17.11-1.pgdg13+2)
;     Dumped by pg_dump version: 17.12 (Homebrew)
;
;
; Selected TOC Entries:
;
",
        "2; 3079 540982 EXTENSION - pg_trgm \n",
        "3485; 0 0 COMMENT - EXTENSION pg_trgm \n",
        "3; 3079 541063 EXTENSION - uuid-ossp \n",
        "3486; 0 0 COMMENT - EXTENSION \"uuid-ossp\" \n",
        "219; 1259 541074 TABLE public t kerno\n",
        "3478; 0 541074 TABLE DATA public t kerno\n",
        "3332; 1259 541079 INDEX public t_s_idx kerno\n",
    );

    #[test]
    fn the_list_names_its_database_extensions_and_owners() {
        assert_eq!(list_database(LIST).as_deref(), Some("kerno_test_k39_probe"));
        assert_eq!(list_extensions(LIST), ["pg_trgm", "uuid-ossp"]);
        assert_eq!(
            missing_owners(LIST, &["kerno".into()]),
            Vec::<String>::new()
        );
        assert_eq!(missing_owners(LIST, &["postgres".into()]), ["kerno"]);
        // A role whose name holds a space, which the server has.
        let spaced = "5; 2615 2200 SCHEMA - archive Rôle Été\n6; 1259 1 TABLE archive t kerno\n";
        assert_eq!(missing_owners(spaced, &["Rôle Été".into()]), ["kerno"]);
        assert_eq!(missing_owners("", &[]), Vec::<String>::new());
    }

    #[test]
    fn leaving_an_extension_out_comments_its_entries() {
        let without = list_without(LIST, &["uuid-ossp".into()]);
        let changed: Vec<&str> = without
            .lines()
            .filter(|line| line.starts_with(";3"))
            .collect();
        assert_eq!(
            changed,
            [
                ";3; 3079 541063 EXTENSION - uuid-ossp ",
                ";3486; 0 0 COMMENT - EXTENSION \"uuid-ossp\" "
            ]
        );
        assert!(without.contains("\n2; 3079 540982 EXTENSION - pg_trgm \n"));
        assert_eq!(without.lines().count(), LIST.lines().count());
        assert_eq!(list_extensions(&without), ["pg_trgm"]);
        assert_eq!(list_without(LIST, &[]), LIST);
    }

    /// The report of 2026-11-23, verbatim: pg_restore 18 in French, the
    /// server in English.
    const FRENCH: &str = "\
pg_restore: création de EXTENSION « pg_trgm »
pg_restore: création de COMMENT « EXTENSION pg_trgm »
pg_restore: création de EXTENSION « pgaudit »
pg_restore: pendant le traitement de la TOC (« PROCESSING TOC ») :
pg_restore: de l'entrée TOC 2 ; 3079 16390 EXTENSION pgaudit (no owner)
pg_restore: erreur : could not execute query: ERROR:  extension \"pgaudit\" is not available
ASTUCE : The extension must first be installed on the system where PostgreSQL is running.
La commande était : CREATE EXTENSION IF NOT EXISTS pgaudit WITH SCHEMA public;";

    #[test]
    fn a_failed_extension_is_read_in_any_language() {
        let found = |log: &str| refused_extensions(&log.lines().collect::<Vec<_>>(), |_| None);
        assert_eq!(found(FRENCH), ["pgaudit"]);
        let english = "\
pg_restore: creating EXTENSION \"pgaudit\"
pg_restore: while PROCESSING TOC:
pg_restore: from TOC entry 2; 3079 16390 EXTENSION pgaudit (no owner)
pg_restore: error: could not execute query: ERROR:  extension \"pgaudit\" is not available
HINT:  The extension must first be installed on the system where PostgreSQL is running.
Command was: CREATE EXTENSION IF NOT EXISTS pgaudit WITH SCHEMA public;";
        assert_eq!(found(english), ["pgaudit"]);
        // A name that needs quotes, a French server.
        let quoted = "\
pg_restore: erreur : could not execute query: ERREUR:  l'extension « Ext-Été » n'est pas disponible
La commande était : CREATE EXTENSION IF NOT EXISTS \"Ext-Été\" WITH SCHEMA public;";
        assert_eq!(found(quoted), ["Ext-Été"]);
        // A failure that does not name the extension: not one.
        let other = "\
pg_restore: error: could not execute query: ERROR:  schema \"x\" does not exist
Command was: CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA x;";
        assert_eq!(found(other), Vec::<String>::new());
        // psql points at the script's line.
        let log = ["psql:<stdin>:7: ERROR:  extension \"pgaudit\" is not available"];
        assert_eq!(
            refused_extensions(&log, |n| (n == 7)
                .then(|| "CREATE EXTENSION pgaudit;".into())),
            ["pgaudit"]
        );
    }
}
