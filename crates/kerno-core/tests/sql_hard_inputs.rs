//! Hard inputs for completion, statement splitting and the formatter, from
//! the audit of the DBeaver critique (docs/research/dbeaver-critique.md,
//! rows C8, C9, C10; triage in docs/COMPETITOR-ISSUES.md). Each test pins a
//! case Kerno gets right: the ones it got wrong at the audit became items of
//! docs/plans/M12.md, and each lands here once fixed (the C8 probes of K11).

use kerno_core::schema::RelationKind;
use kerno_core::sql::{
    Catalog, CatalogColumn, CatalogRelation, CatalogSchema, Comma, FormatOptions, Indent,
    KeywordCase, LexRules, TokenKind, complete, format_cleanly, format_with, lex, split_statements,
    statement_at,
};

// ---------------------------------------------------------------- C8

fn rel(name: &str, columns: &[&str]) -> CatalogRelation {
    CatalogRelation {
        name: name.into(),
        kind: RelationKind::Table,
        columns: columns
            .iter()
            .map(|c| CatalogColumn {
                name: (*c).into(),
                data_type: "integer".into(),
            })
            .collect(),
        foreign_keys: Vec::new(),
    }
}

fn catalog() -> Catalog {
    Catalog {
        schemas: vec![CatalogSchema {
            name: "shop".into(),
            relations: vec![
                rel("t", &["id", "name"]),
                rel("u", &["uid", "ref"]),
                rel("orders", &["id", "customer_id", "total"]),
                rel("MyTable", &["Id", "Name"]),
            ],
            routines: Vec::new(),
        }],
        search_path: vec!["shop".into()],
    }
}

fn pg_quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn my_quote(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// Labels offered at the `|` of `sql`.
fn labels(sql: &str, rules: LexRules) -> Vec<String> {
    let cursor = sql.find('|').expect("cursor");
    let text = sql.replacen('|', "", 1);
    let quote: &dyn Fn(&str) -> String = if rules == LexRules::MYSQL {
        &my_quote
    } else {
        &pg_quote
    };
    complete(&text, cursor, &catalog(), rules, quote)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect()
}

#[track_caller]
fn offers(sql: &str, rules: LexRules, expected: &[&str]) {
    let got = labels(sql, rules);
    let first: Vec<&str> = got
        .iter()
        .take(expected.len())
        .map(String::as_str)
        .collect();
    assert_eq!(first, expected, "input: {sql}\nall offered: {got:?}");
}

const PG: LexRules = LexRules::POSTGRES;
const MY: LexRules = LexRules::MYSQL;

#[test]
fn c8_update_from_pg() {
    offers(
        "UPDATE t SET name = 'a' FROM u WHERE u.|",
        PG,
        &["uid", "ref"],
    );
}
#[test]
fn c8_delete_using_pg() {
    offers("DELETE FROM t USING u WHERE u.|", PG, &["uid", "ref"]);
}
#[test]
fn c8_mysql_backticks_alias_before_from_my() {
    offers(
        "SELECT `o`.| FROM `orders` `o`",
        MY,
        &["id", "customer_id", "total"],
    );
}
#[test]
fn c8_quoted_mixed_case_table_pg() {
    offers("SELECT m.| FROM \"MyTable\" m", PG, &["Id", "Name"]);
}
#[test]
fn c8_schema_qualified_alias_select_list_pg() {
    offers(
        "SELECT o.| FROM shop.orders o",
        PG,
        &["id", "customer_id", "total"],
    );
}
#[test]
fn c8_schema_qualified_alias_select_list_my() {
    offers(
        "SELECT o.| FROM shop.orders o",
        MY,
        &["id", "customer_id", "total"],
    );
}

// ---------------------------------------------------------------- C9

fn texts(source: &str, rules: LexRules) -> Vec<String> {
    split_statements(source, rules)
        .iter()
        .map(|s| s.text(source).to_string())
        .collect()
}

#[track_caller]
fn splits(source: &str, rules: LexRules, expected: &[&str]) {
    assert_eq!(texts(source, rules), expected, "input: {source}");
}

#[test]
fn c9_do_block_pg() {
    splits(
        "DO $$ BEGIN RAISE NOTICE 'a;b'; END $$; SELECT 1;",
        PG,
        &["DO $$ BEGIN RAISE NOTICE 'a;b'; END $$", "SELECT 1"],
    );
}
#[test]
fn c9_tagged_dollar_quote_pg() {
    splits(
        "CREATE FUNCTION f() RETURNS int AS $fn$ BEGIN RETURN 1; END; $fn$ LANGUAGE plpgsql; SELECT 1;",
        PG,
        &[
            "CREATE FUNCTION f() RETURNS int AS $fn$ BEGIN RETURN 1; END; $fn$ LANGUAGE plpgsql",
            "SELECT 1",
        ],
    );
}
#[test]
fn c9_double_dollar_inside_tagged_body_pg() {
    splits(
        "CREATE FUNCTION f() RETURNS text AS $fn$ SELECT $$a;b$$; $fn$ LANGUAGE sql; SELECT 2;",
        PG,
        &[
            "CREATE FUNCTION f() RETURNS text AS $fn$ SELECT $$a;b$$; $fn$ LANGUAGE sql",
            "SELECT 2",
        ],
    );
}
#[test]
fn c9_mysql_procedure_with_delimiter_my() {
    splits(
        "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\nDELIMITER ;\nSELECT 3;",
        MY,
        &[
            "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
            "SELECT 3",
        ],
    );
}
#[test]
fn c9_begin_atomic_pg() {
    // Failed at the audit: four statements (K12).
    splits(
        "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END; SELECT 3;",
        PG,
        &[
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END",
            "SELECT 3",
        ],
    );
}
#[test]
fn c9_mysql_procedure_without_delimiter_my() {
    // Failed at the audit: four statements (K12).
    splits(
        "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END; SELECT 3;",
        MY,
        &[
            "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
            "SELECT 3",
        ],
    );
}
#[test]
fn c9_bare_begin_is_a_transaction() {
    splits(
        "BEGIN; SELECT 1; COMMIT;",
        PG,
        &["BEGIN", "SELECT 1", "COMMIT"],
    );
    splits(
        "BEGIN; SELECT 1; COMMIT;",
        MY,
        &["BEGIN", "SELECT 1", "COMMIT"],
    );
}
#[test]
fn c9_semicolon_in_line_comment_pg() {
    splits("SELECT 1 -- a;b\n; SELECT 2", PG, &["SELECT 1", "SELECT 2"]);
}
#[test]
fn c9_semicolon_in_block_comment_pg() {
    splits(
        "SELECT /* a;b */ 1; SELECT 2",
        PG,
        &["SELECT /* a;b */ 1", "SELECT 2"],
    );
}
#[test]
fn c9_semicolon_in_string_pg() {
    splits("SELECT 'a;b'; SELECT 2", PG, &["SELECT 'a;b'", "SELECT 2"]);
}
#[test]
fn c9_semicolon_in_string_my() {
    splits("SELECT 'a;b'; SELECT 2", MY, &["SELECT 'a;b'", "SELECT 2"]);
}
#[test]
fn c9_semicolon_in_e_string_pg() {
    splits(
        "SELECT E'it\\'s;'; SELECT 2",
        PG,
        &["SELECT E'it\\'s;'", "SELECT 2"],
    );
}
#[test]
fn c9_semicolon_in_backslash_string_my() {
    splits(
        "SELECT 'it\\'s;'; SELECT 2",
        MY,
        &["SELECT 'it\\'s;'", "SELECT 2"],
    );
}
#[test]
fn c9_semicolon_in_double_quoted_string_my() {
    splits(
        "SELECT \"a;b\"; SELECT 2",
        MY,
        &["SELECT \"a;b\"", "SELECT 2"],
    );
}
#[test]
fn c9_semicolon_in_backtick_my() {
    splits(
        "SELECT `back;tick` FROM t; SELECT 2",
        MY,
        &["SELECT `back;tick` FROM t", "SELECT 2"],
    );
}
#[test]
fn c9_semicolon_in_hash_comment_my() {
    splits("SELECT 1 # a;b\n; SELECT 2", MY, &["SELECT 1", "SELECT 2"]);
}

fn at(source: &str, cursor: usize, rules: LexRules) -> Option<String> {
    let spans = split_statements(source, rules);
    statement_at(&spans, cursor).map(|s| s.text(source).to_string())
}

#[test]
fn c9_at_cursor_blank_line_between_pg() {
    // Kerno's documented rule: on a blank line, the statement that follows.
    let source = "SELECT 1;\n\nSELECT 2;";
    let cursor = source.find("\n\n").unwrap() + 1;
    assert_eq!(at(source, cursor, PG).as_deref(), Some("SELECT 2"));
}
#[test]
fn c9_at_cursor_end_after_last_semicolon_pg() {
    let source = "SELECT 1;\nSELECT 2;\n\n";
    assert_eq!(at(source, source.len(), PG).as_deref(), Some("SELECT 2"));
}
#[test]
fn c9_at_cursor_inside_do_body_pg() {
    let source = "SELECT 0;\nDO $$ BEGIN RAISE NOTICE 'a;b'; END $$;\nSELECT 1;";
    let cursor = source.find("RAISE").unwrap() + 2;
    assert_eq!(
        at(source, cursor, PG).as_deref(),
        Some("DO $$ BEGIN RAISE NOTICE 'a;b'; END $$")
    );
}

#[test]
fn c9_at_cursor_inside_begin_atomic_pg() {
    // Failed at the audit: `SELECT 2` alone (K12).
    let source = "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END; SELECT 3;";
    let cursor = source.find("SELECT 2").unwrap() + 2;
    assert_eq!(
        at(source, cursor, PG).as_deref(),
        Some("CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END")
    );
}
#[test]
fn c9_at_cursor_inside_mysql_procedure_my() {
    // Failed at the audit: `SELECT 2` alone (K12).
    let source = "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END; SELECT 3;";
    let cursor = source.find("SELECT 2").unwrap() + 2;
    assert_eq!(
        at(source, cursor, MY).as_deref(),
        Some("CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END")
    );
}

// ---------------------------------------------------------------- C10

const DEFAULT: FormatOptions = FormatOptions {
    keyword_case: KeywordCase::Upper,
    indent: Indent::Spaces(4),
    comma: Comma::Trailing,
    line_width: 0,
};
/// A non-default J7 set, keyword case kept so every token must match exactly.
const KEEP: FormatOptions = FormatOptions {
    keyword_case: KeywordCase::Keep,
    indent: Indent::Spaces(2),
    comma: Comma::Leading,
    line_width: 80,
};

/// Non-whitespace tokens; words lower-cased unless `exact`.
fn tokens(sql: &str, rules: LexRules, exact: bool) -> Vec<(TokenKind, String)> {
    lex(sql, rules)
        .into_iter()
        .filter(|t| t.kind != TokenKind::Whitespace)
        .map(|t| {
            let text = &sql[t.range];
            let text = if t.kind == TokenKind::Word && !exact {
                text.to_ascii_lowercase()
            } else {
                text.to_string()
            };
            (t.kind, text)
        })
        .collect()
}

#[track_caller]
fn keeps_tokens(input: &str, rules: LexRules) {
    let out = format_with(input, rules, &DEFAULT);
    assert_eq!(
        tokens(input, rules, false),
        tokens(&out, rules, false),
        "default\n{out}"
    );
    let out = format_with(input, rules, &KEEP);
    assert_eq!(
        tokens(input, rules, true),
        tokens(&out, rules, true),
        "keep-case\n{out}"
    );
}

#[track_caller]
fn lays_out(input: &str, rules: LexRules) {
    for options in [DEFAULT, KEEP] {
        assert!(
            format_cleanly(input, rules, &options).is_some(),
            "kept as written (safety net) with {options:?}: {input}"
        );
    }
}

#[track_caller]
fn idempotent(input: &str, rules: LexRules) {
    for options in [DEFAULT, KEEP] {
        let once = format_with(input, rules, &options);
        let twice = format_with(&once, rules, &options);
        let thrice = format_with(&twice, rules, &options);
        assert_eq!(
            twice, once,
            "{options:?}\n--- once ---\n{once}\n--- twice ---\n{twice}"
        );
        assert_eq!(thrice, twice, "{options:?}");
    }
}

const COMMENTS: &str =
    "select a, -- first\n  b /* second */\nfrom t -- tail\nwhere x = 1 /* why */ and y = 2";
const DOLLAR_BODY: &str = "$fn$\nBEGIN\n  -- keep   me\n  RETURN   1 ;  /* x */\nEND;\n$fn$";
const HARD: &str = "with ranked as (select o.id, o.customer_id, row_number() over (partition by o.customer_id order by o.placed_at desc) as rn, case when o.total > 100 then 'big' when o.total > 10 then 'mid' else 'small' end as size from shop.orders o where o.customer_id in (select c.id from shop.customers c where c.is_active)) select * from ranked where rn = 1";
const MYSQL_TEXT: &str = "select `o`.`id`, `o`.`total` # the total\nfrom `shop`.`orders` `o` where `o`.`status` = \"paid\" # only paid\norder by `o`.`id`";
const INSERT_SELECT: &str = "insert into shop.totals (customer_id, n, amount) select o.customer_id, count(*), sum(o.total) from shop.orders o join shop.customers c on c.id = o.customer_id group by o.customer_id having sum(o.total) > 10 order by 3 desc; update shop.orders set total = 0 where id = 1;";

fn dollar_function() -> String {
    format!("create function f() returns int language plpgsql as {DOLLAR_BODY};")
}

#[test]
fn c10_comments_tokens_kept_pg() {
    keeps_tokens(COMMENTS, PG);
}
#[test]
fn c10_comments_trailing_line_comment_stays_on_its_line_pg() {
    let out = format_with(COMMENTS, PG, &DEFAULT);
    let line = out.lines().find(|l| l.contains("-- first")).unwrap();
    assert!(line.contains("a,"), "`-- first` left `a,`:\n{out}");
    let line = out.lines().find(|l| l.contains("-- tail")).unwrap();
    assert!(line.contains("FROM t"), "`-- tail` left `FROM t`:\n{out}");
}
#[test]
fn c10_comments_laid_out_pg() {
    lays_out(COMMENTS, PG);
}
#[test]
fn c10_dollar_body_byte_identical_pg() {
    let input = dollar_function();
    for options in [DEFAULT, KEEP] {
        let out = format_with(&input, PG, &options);
        assert!(out.contains(DOLLAR_BODY), "{options:?}\n{out}");
    }
}
#[test]
fn c10_hard_query_tokens_kept_pg() {
    keeps_tokens(HARD, PG);
}
#[test]
fn c10_hard_query_laid_out_pg() {
    lays_out(HARD, PG);
}
#[test]
fn c10_mysql_backticks_and_hash_tokens_kept_my() {
    keeps_tokens(MYSQL_TEXT, MY);
}
#[test]
fn c10_mysql_backticks_and_hash_laid_out_my() {
    lays_out(MYSQL_TEXT, MY);
}
#[test]
fn c10_idempotent_comments_pg() {
    idempotent(COMMENTS, PG);
}
#[test]
fn c10_idempotent_dollar_function_pg() {
    idempotent(&dollar_function(), PG);
}
#[test]
fn c10_idempotent_hard_query_pg() {
    idempotent(HARD, PG);
}
#[test]
fn c10_idempotent_mysql_my() {
    idempotent(MYSQL_TEXT, MY);
}
#[test]
fn c10_idempotent_insert_select_script_pg() {
    idempotent(INSERT_SELECT, PG);
}

// The unaliased UPDATE … FROM and DELETE … USING above can pass through the
// fallback that resolves a bare table name; with an alias only the scope can.
#[test]
fn c8_update_from_aliased_pg() {
    offers(
        "UPDATE t SET name = 'a' FROM u x WHERE x.|",
        PG,
        &["uid", "ref"],
    );
}

// K11: the audit's failing C8 probes, now answered (docs/plans/M12.md).
#[test]
fn c8_cte_columns() {
    for rules in [PG, MY] {
        offers(
            "WITH x AS (SELECT id, name FROM t) SELECT x.| FROM x",
            rules,
            &["id", "name"],
        );
        // Cut at the cursor.
        offers(
            "WITH x AS (SELECT id, name FROM t) SELECT x.|",
            rules,
            &["id", "name"],
        );
    }
}
#[test]
fn c8_cte_column_list() {
    for rules in [PG, MY] {
        offers(
            "WITH x(a, b) AS (SELECT id, name FROM t) SELECT x.| FROM x",
            rules,
            &["a", "b"],
        );
    }
}
#[test]
fn c8_cte_on_a_cte() {
    for rules in [PG, MY] {
        offers(
            "WITH x AS (SELECT id, name FROM t), y AS (SELECT name FROM x) SELECT y.| FROM y",
            rules,
            &["name"],
        );
    }
}
#[test]
fn c8_subquery_alias() {
    for rules in [PG, MY] {
        offers("SELECT s.| FROM (SELECT id AS k FROM t) s", rules, &["k"]);
    }
}
#[test]
fn c8_join_lateral_pg() {
    offers(
        "SELECT l.| FROM t JOIN LATERAL (SELECT uid AS lid FROM u WHERE u.uid = t.id) l ON true",
        PG,
        &["lid"],
    );
}
#[test]
fn c8_delete_using_aliased() {
    for rules in [PG, MY] {
        offers("DELETE FROM t USING u x WHERE x.|", rules, &["uid", "ref"]);
    }
}
