//! The roles a failed PostgreSQL restore names (K35, D-108). A backup made
//! on another server gives its objects to that server's roles (`ALTER …
//! OWNER TO`) and grants rights to them (`GRANT`, `REVOKE`, `ALTER DEFAULT
//! PRIVILEGES`, `SET SESSION AUTHORIZATION` around a grant by another
//! role); a server without those roles refuses each of these statements.
//!
//! Read from the SQL the tool printed, never from the words around it:
//! pg_restore and psql speak the user's language and the server its own
//! (`lc_messages`), but the SQL is never translated. pg_restore prints the
//! failed command after a translated label (`Command was:`, `La commande
//! était :`), a whole block of statements for an ACL entry; psql, which
//! Kerno runs without `--echo-errors`, prints the line of the script the
//! statement ended on (`psql:<stdin>:29: ERROR: …`), read back from the
//! file by the caller. Since a block names several roles, a role counts
//! only when the server's message just above names it too, whatever its
//! language: the name itself is never translated.

use crate::sql::{LexRules, TokenKind, lex};

/// A role that a failed statement names and the server's message names
/// too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedRole {
    /// The role's name as the server knows it (unquoted).
    pub name: String,
    /// Named as an owner (`ALTER … OWNER TO`); otherwise in privileges.
    pub owner: bool,
}

/// The roles the failed statements of a restore's `log` name (pg_restore's
/// or psql's output, line by line). `script_line(n)` gives line `n` (from
/// 1) of the SQL script psql ran, for psql's `psql:<file>:n:` lines.
pub fn refused_roles<S: AsRef<str>>(
    log: &[S],
    mut script_line: impl FnMut(u64) -> Option<String>,
) -> Vec<RefusedRole> {
    let lines: Vec<&str> = log.iter().map(AsRef::as_ref).collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let Some(first) = statement_in(lines[at]) else {
            at += 1;
            continue;
        };
        let error = error_above(&lines, at);
        add(&mut found, first, &error);
        // The other statements of the same command, one per line.
        at += 1;
        while at < lines.len() && is_statement(lines[at].trim()) {
            add(&mut found, lines[at].trim(), &error);
            at += 1;
        }
    }
    let psql = lines
        .iter()
        .rev()
        .filter(|line| statement_in(line).is_none())
        .find_map(|line| psql_line(line).map(|number| (number, *line)));
    if let Some((number, error)) = psql
        && let Some(statement) = script_line(number)
    {
        add(&mut found, statement.trim(), error);
    }
    found
}

/// Adds the roles `statement` names that `error` names too.
fn add(found: &mut Vec<RefusedRole>, statement: &str, error: &str) {
    for (name, owner) in roles_in(statement) {
        if !named_in(error, &name) {
            continue;
        }
        match found.iter_mut().find(|role| role.name == name) {
            Some(role) => role.owner |= owner,
            None => found.push(RefusedRole { name, owner }),
        }
    }
}

/// Whether `text` starts one of the statements that name roles, as
/// pg_dump writes them.
fn is_statement(text: &str) -> bool {
    ["ALTER ", "GRANT ", "REVOKE ", "SET SESSION AUTHORIZATION "]
        .iter()
        .any(|start| text.starts_with(start))
}

/// The statement a line prints after a label ending in a colon, in any
/// language: `Command was: ALTER …`, `La commande était : ALTER …`,
/// `psql:<stdin>:3: STATEMENT:  GRANT …`.
fn statement_in(line: &str) -> Option<&str> {
    line.match_indices(':')
        .map(|(at, _)| line[at + 1..].trim_start())
        .find(|tail| is_statement(tail))
}

/// The server's message above line `at`: the lines up to the tool's own
/// line that reports it (`pg_restore: error: … ERROR: …`, `psql:…: ERROR:
/// …`), the message's DETAIL and HINT lines between.
fn error_above(lines: &[&str], at: usize) -> String {
    let mut error = Vec::new();
    for line in lines[..at].iter().rev().take(6) {
        error.push(*line);
        if line.starts_with("pg_restore:") || line.starts_with("psql:") {
            break;
        }
    }
    error.join("\n")
}

/// The script line a psql message points at: `psql:<file>:29: …` gives 29.
fn psql_line(line: &str) -> Option<u64> {
    let mut fields = line.strip_prefix("psql:")?.split(':');
    fields.next()?;
    fields.next()?.parse().ok()
}

/// Whether `name` stands in `text` as a whole name, not inside a longer
/// one (`kerno` is not named by `kerno_reader`).
fn named_in(text: &str, name: &str) -> bool {
    let part_of_name = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(part_of_name)
            && !text[at + name.len()..]
                .chars()
                .next()
                .is_some_and(part_of_name)
    })
}

/// The roles a statement names, each with whether it is named as an
/// owner: `ALTER … OWNER TO r`; `GRANT … TO r`, `REVOKE … FROM r`, `…
/// GRANTED BY r`, `ALTER DEFAULT PRIVILEGES FOR ROLE r …`, `SET SESSION
/// AUTHORIZATION r`. `PUBLIC` and `CURRENT_USER` are not roles of a server.
fn roles_in(statement: &str) -> Vec<(String, bool)> {
    let tokens: Vec<(TokenKind, &str)> = lex(statement, LexRules::POSTGRES)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| (token.kind, &statement[token.range]))
        .collect();
    let word = |at: usize, word: &str| matches!(tokens.get(at), Some((TokenKind::Word, text)) if text.eq_ignore_ascii_case(word));
    let mut roles = Vec::new();
    // The comma-separated names from `at` on.
    let names = |mut at: usize, owner: bool, roles: &mut Vec<(String, bool)>| {
        loop {
            if word(at, "GROUP") {
                at += 1;
            }
            let name = match tokens.get(at) {
                Some((TokenKind::Word, text)) => {
                    let special = ["PUBLIC", "CURRENT_USER", "SESSION_USER", "CURRENT_ROLE"]
                        .iter()
                        .any(|special| text.eq_ignore_ascii_case(special));
                    (!special).then(|| text.to_lowercase())
                }
                Some((TokenKind::QuotedIdent, text)) if text.len() >= 2 && text.ends_with('"') => {
                    Some(text[1..text.len() - 1].replace("\"\"", "\""))
                }
                _ => return,
            };
            roles.extend(name.map(|name| (name, owner)));
            if !matches!(tokens.get(at + 1), Some((TokenKind::Punct, ","))) {
                return;
            }
            at += 2;
        }
    };
    if word(0, "SET") && word(1, "SESSION") && word(2, "AUTHORIZATION") {
        names(3, false, &mut roles);
    } else if word(0, "ALTER") && !word(1, "DEFAULT") {
        if let Some(at) = (1..tokens.len()).find(|&at| word(at, "OWNER") && word(at + 1, "TO")) {
            names(at + 2, true, &mut roles);
        }
    } else if word(0, "ALTER") || word(0, "GRANT") || word(0, "REVOKE") {
        for at in 1..tokens.len() {
            // `TO r`, `FROM r`, `GRANTED BY r`, `FOR ROLE r`, `FOR USER r`.
            if word(at, "TO")
                || word(at, "FROM")
                || (word(at, "BY") && word(at - 1, "GRANTED"))
                || ((word(at, "ROLE") || word(at, "USER")) && word(at - 1, "FOR"))
            {
                names(at + 1, false, &mut roles);
            }
        }
    }
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    fn found(log: &str) -> Vec<RefusedRole> {
        refused_roles(&lines(log), |_| None)
    }

    fn role(name: &str, owner: bool) -> RefusedRole {
        RefusedRole {
            name: name.into(),
            owner,
        }
    }

    /// The first outside user's report of 2026-11-23, verbatim: pg_restore
    /// 18 in French, the server in English.
    const FRENCH: &str = "\
pg_restore: connexion à la base de données pour la restauration
pg_restore: exécution SELECT pg_catalog.set_config('search_path', '', false);
pg_restore: création de SCHEMA « archive »
pg_restore: pendant le traitement de la TOC (« PROCESSING TOC ») :
pg_restore: de l'entrée TOC 12 ; 2615 117450 SCHEMA archive app_owner
pg_restore: erreur : could not execute query: ERROR:  role \"app_owner\" does not exist
La commande était : ALTER SCHEMA archive OWNER TO app_owner;";

    const ENGLISH: &str = "\
pg_restore: connecting to database for restore
pg_restore: executing SELECT pg_catalog.set_config('search_path', '', false);
pg_restore: creating SCHEMA \"archive\"
pg_restore: while PROCESSING TOC:
pg_restore: from TOC entry 12; 2615 117450 SCHEMA archive app_owner
pg_restore: error: could not execute query: ERROR:  role \"app_owner\" does not exist
Command was: ALTER SCHEMA archive OWNER TO app_owner;";

    #[test]
    fn the_owner_case_is_read_in_either_language() {
        assert_eq!(found(FRENCH), [role("app_owner", true)]);
        assert_eq!(found(ENGLISH), [role("app_owner", true)]);
        // A French server too: only the name and the SQL are the same.
        let both = FRENCH.replace(
            "ERROR:  role \"app_owner\" does not exist",
            "ERREUR:  le rôle « app_owner » n'existe pas",
        );
        assert_eq!(found(&both), [role("app_owner", true)]);
    }

    /// An ACL entry is a block of statements: only the role the server's
    /// message names is the one refused (pg_restore 18, PostgreSQL 17).
    #[test]
    fn the_privilege_case_names_only_the_refused_role() {
        let log = "\
pg_restore: création de ACL « archive.TABLE t »
pg_restore: de l'entrée TOC 3426 ; 0 0 ACL TABLE t kerno
pg_restore: erreur : could not execute query: ERROR:  role \"kerno_reader\" does not exist
La commande était : GRANT INSERT ON TABLE archive.t TO kerno_reader;
GRANT ALL ON TABLE archive.t TO app_owner WITH GRANT OPTION;
SET SESSION AUTHORIZATION app_owner;
GRANT UPDATE ON TABLE archive.t TO kerno;
RESET SESSION AUTHORIZATION;


pg_restore: attention : erreurs ignorées lors de la restauration : 1";
        assert_eq!(found(log), [role("kerno_reader", false)]);
        // The grantor of a grant made by another role.
        let log = "\
pg_restore: error: could not execute query: ERROR:  role \"app_owner\" does not exist
Command was: SET SESSION AUTHORIZATION app_owner;
GRANT UPDATE ON TABLE archive.t TO kerno;";
        assert_eq!(found(log), [role("app_owner", false)]);
        let log = "\
pg_restore: error: could not execute query: ERROR:  role \"app_owner\" does not exist
Command was: REVOKE ALL ON SCHEMA archive FROM app_owner;";
        assert_eq!(found(log), [role("app_owner", false)]);
    }

    /// A name that needs quotes: the server prints it bare, the SQL quoted
    /// with its quote doubled; default privileges name two roles.
    #[test]
    fn a_quoted_role_name_is_unquoted() {
        let log = "\
pg_restore: creating DEFAULT ACL \"archive.DEFAULT PRIVILEGES FOR TABLES\"
pg_restore: from TOC entry 2044; 826 504593 DEFAULT ACL DEFAULT PRIVILEGES FOR TABLES app_owner
pg_restore: error: could not execute query: ERROR:  role \"Rôle \"Été\" does not exist
Command was: ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA archive GRANT SELECT ON TABLES TO \"Rôle \"\"Été\";";
        assert_eq!(found(log), [role("Rôle \"Été", false)]);
        let log = "\
pg_restore: error: could not execute query: ERROR:  role \"Rôle Été\" does not exist
Command was: ALTER TABLE public.t OWNER TO \"Rôle Été\";";
        assert_eq!(found(log), [role("Rôle Été", true)]);
        // Both roles of default privileges, when the message names both.
        assert_eq!(
            roles_in("ALTER DEFAULT PRIVILEGES FOR ROLE a, \"B\" REVOKE ALL ON TABLES FROM c;"),
            [
                ("a".into(), false),
                ("B".into(), false),
                ("c".into(), false)
            ]
        );
    }

    #[test]
    fn a_failure_not_about_a_role_names_none() {
        let log = "\
pg_restore: creating ACL \"archive.TABLE t\"
pg_restore: creating TABLE \"public.kept\"
pg_restore: error: could not execute query: ERROR:  relation \"kept\" already exists
Command was: CREATE TABLE public.kept (
    id integer
);";
        assert_eq!(found(log), []);
        // An owner statement refused for another reason that does not name
        // the role; a role inside a longer name; PUBLIC.
        let log = "\
pg_restore: error: could not execute query: ERROR:  schema \"archive\" does not exist
Command was: ALTER TABLE archive.t OWNER TO app_owner;
pg_restore: error: could not execute query: ERROR:  role \"kerno_reader\" does not exist
Command was: GRANT ALL ON TABLE archive.t TO kerno;
pg_restore: error: could not execute query: ERROR:  PUBLIC
Command was: REVOKE ALL ON SCHEMA public FROM PUBLIC;";
        assert_eq!(found(log), []);
        // pg_restore's verbose lines and psql's command tags.
        assert_eq!(
            found("GRANT\nALTER SCHEMA\npg_restore: creating ACL \"x\""),
            []
        );
    }

    /// psql does not print the failed statement: its line in the script is
    /// read back.
    #[test]
    fn psql_points_at_the_script_line() {
        let log = [
            "SET",
            "CREATE SCHEMA",
            "psql:<stdin>:29: ERREUR:  le rôle « app_owner » n'existe pas",
        ];
        let mut asked = Vec::new();
        let roles = refused_roles(&log, |line| {
            asked.push(line);
            Some("ALTER SCHEMA archive OWNER TO app_owner;\n".into())
        });
        assert_eq!(roles, [role("app_owner", true)]);
        assert_eq!(asked, [29]);
        // Another failure at that line names nothing.
        let log = ["psql:<stdin>:3: ERROR:  relation \"kept\" already exists"];
        assert_eq!(
            refused_roles(&log, |_| Some("CREATE TABLE kept (id int);".into())),
            []
        );
    }
}
