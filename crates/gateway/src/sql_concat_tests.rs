//! The raw-SQL concatenation floor pattern: a match never starts at a closing quote, a real
//! one-line or multi-line query still fires, and a template literal has its own form.

use super::*;

fn sql(text: &str) -> Vec<usize> {
    content_match_lines("SEC-NO-RAW-SQL-CONCAT-1", text)
}

#[test]
fn sql_concat_never_starts_at_a_closing_quote() {
    // Quiet: prose and an unrelated template after an ordinary string.
    let prose = "log(\"ready\");\n// select the rows to update from the cache where needed\nconst label = `${name}`;\n";
    assert!(sql(prose).is_empty());
    // Quiet: a parameterized call, then unrelated text.
    let bound = "db.query(\"DELETE FROM jobs WHERE id = $1\", [id]);\n// then update the counter and set the label\nconst note = `${count} left`;\n";
    assert!(sql(bound).is_empty());
    // Fires: one-line concatenation.
    assert_eq!(
        sql("const q = \"SELECT * FROM users WHERE name = '\" + name + \"'\";"),
        vec![1]
    );
    // Fires: one-line concatenation after another string on the same line.
    assert_eq!(
        sql("let a = \"x\"; let q = \"SELECT * FROM t WHERE id = \" + id;"),
        vec![1]
    );
    // Fires: a multi-line string with a named format argument.
    let multi = "let q = format!(\n    \"SELECT id\n     FROM users\n     WHERE name = '{name}'\",\n    name = name\n);";
    assert_eq!(sql(multi), vec![2]);
    // Fires: a real one-line match on the line after an ordinary string.
    let inside = "log(\"ready\");\nlet q = \"UPDATE users SET name = '\" + name + \"'\";\n";
    assert_eq!(sql(inside), vec![2]);
}

#[test]
fn a_rejected_span_does_not_swallow_a_real_match_inside_it() {
    // The first match starts at the closing quote of "ready" and runs to the `{count}`; a real
    // query opens inside that span.
    let text = "log(\"ready\");\nlet q = format!(\"DELETE FROM jobs WHERE id = {id}\", id = id);\n";
    assert_eq!(sql(text), vec![2]);
}

#[test]
fn sql_in_a_template_literal_fires_on_its_own_line() {
    let real = "log(\"ready\");\nconst q = `select * from orders where id = ${id}`;\n";
    assert_eq!(sql(real), vec![2]);
    assert_eq!(
        sql("const q = `select * from orders where id = ${id}`;"),
        vec![1]
    );
    assert!(sql("const q = `select * from orders where id = $1`;").is_empty());
    assert!(sql("const msg = `Loaded ${n} rows`;").is_empty());
    // A multi-line template reports the line of its first interpolation.
    let multi = "const q = `\n  select id\n  from orders\n  where owner = ${owner}\n`;";
    assert_eq!(sql(multi), vec![4]);
}

#[test]
fn the_gate_arm_reads_the_same_matches() {
    let prose = "log(\"ready\");\n// select the rows to update from the cache where needed\nconst label = `${name}`;\n";
    assert!(arm_sec_no_raw_sql_concat_1("src/a.ts", prose).is_ok());
    let real = "log(\"ready\");\nconst q = `select * from orders where id = ${id}`;\n";
    assert!(arm_sec_no_raw_sql_concat_1("src/a.ts", real).is_err());
}

#[test]
fn single_quoted_sql_after_an_earlier_double_quote_fires() {
    let fstring = "log(\"ready\")\nq = f'SELECT * FROM users WHERE name = {name}'\n";
    assert_eq!(sql(fstring), vec![2]);
    let format = "log(\"ready\")\nq = 'SELECT * FROM users WHERE name = {}'.format(name)\n";
    assert_eq!(sql(format), vec![2]);
    let concat = "log(\"ready\")\nq = 'SELECT * FROM users WHERE name = ' + name\n";
    assert_eq!(sql(concat), vec![2]);
    // Quiet: a parameterized single-quoted query, and prose with an apostrophe.
    assert!(sql("log(\"ready\")\nq = 'SELECT * FROM users WHERE name = %s'\n").is_empty());
    assert!(sql("log(\"ready\")\n// don't update it\nconst a = `${x}`;\n").is_empty());
}

#[test]
fn a_verb_on_the_quote_line_keeps_a_multi_line_match_whose_quote_does_not_open() {
    // The quote before SELECT follows one stray quote on its line, so it does not "open" a
    // string; the verb right after it is what keeps the match.
    let text = "it\"s \"SELECT id\n FROM t\n WHERE n = '{n}'\"";
    assert_eq!(sql(text), vec![1]);
}

#[test]
fn rust_lifetimes_and_prose_apostrophes_do_not_fire_the_single_quote_form() {
    let lifetimes = "impl<'a> Insert for Batch<T> where T: Row {}\n";
    assert!(
        content_match_lines_in_file("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", lifetimes).is_empty()
    );
    // The form is skipped for a Rust file even when the text would otherwise match.
    let rust_one_line = "let q = 'SELECT * FROM t WHERE id = {id}';\n";
    assert!(
        content_match_lines_in_file("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", rust_one_line)
            .is_empty()
    );
    // A prose apostrophe on one line, then code with the words on later lines.
    let prose = "// don't cache this\nlet x = update(set, {id});\n";
    assert!(sql(prose).is_empty());
    let prose2 = "# don't cache this\nupdate_rows()\nset_flag()\nwhere_clause = {id}\n";
    assert!(sql(prose2).is_empty());
    // Python forms still report line 2.
    assert_eq!(
        sql("log(\"ready\")\nq = f'SELECT * FROM users WHERE name = {name}'\n"),
        vec![2]
    );
    assert_eq!(
        content_match_lines_in_file(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/a.py",
            "log(\"ready\")\nq = 'SELECT * FROM users WHERE name = {}'.format(name)\n"
        ),
        vec![2]
    );
    assert_eq!(
        sql("log(\"ready\")\nq = 'SELECT * FROM users WHERE name = ' + name\n"),
        vec![2]
    );
}
