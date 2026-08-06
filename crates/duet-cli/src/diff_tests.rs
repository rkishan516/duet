//! What the diff says, and what it refuses to say at length.

use super::*;

#[test]
fn identical_text_produces_nothing() {
    // Emptiness is the caller's notion of "the same", so this is the property
    // the whole comparison rests on rather than a convenience.
    assert_eq!(render("a\nb\n", "a\nb\n"), "");
    assert_eq!(render("", ""), "");
}

#[test]
fn a_changed_line_is_shown_with_both_sides_and_its_number() {
    let rendered = render("a\nb\nc\n", "a\nB\nc\n");
    assert_eq!(rendered, "      1 | a\n-     2 | b\n+     2 | B\n");
}

#[test]
fn context_is_bounded_at_three_lines() {
    let committed = (1..=20).map(|n| format!("line {n}")).collect::<Vec<_>>();
    let mut generated = committed.clone();
    generated[14] = "changed".to_string();
    let rendered = render(&committed.join("\n"), &generated.join("\n"));
    let context = rendered.lines().filter(|l| l.starts_with(' ')).count();
    assert_eq!(context, CONTEXT_LINES, "{rendered}");
    assert!(rendered.contains("line 12"), "{rendered}");
    assert!(!rendered.contains("line 11"), "{rendered}");
}

#[test]
fn a_difference_in_the_first_line_needs_no_context() {
    let rendered = render("a\n", "b\n");
    assert_eq!(rendered, "-     1 | a\n+     1 | b\n");
}

#[test]
fn further_differences_are_counted_rather_than_printed() {
    // The bound that matters in CI: a header change moves every line, and a
    // full diff of two generated files is thousands of lines of log.
    let committed = (1..=100).map(|n| format!("{n}")).collect::<Vec<_>>();
    let generated = (1..=100).map(|n| format!("x{n}")).collect::<Vec<_>>();
    let rendered = render(&committed.join("\n"), &generated.join("\n"));
    assert!(rendered.lines().count() < 6, "{rendered}");
    assert!(rendered.contains("99 more line(s) differ"), "{rendered}");
}

#[test]
fn a_file_that_only_grew_says_so_at_the_end_of_the_shorter_side() {
    let rendered = render("a\nb\n", "a\nb\nc\n");
    assert!(rendered.contains("(end of file)"), "{rendered}");
    assert!(rendered.contains("+     3 | c"), "{rendered}");
    assert!(rendered.contains("1 line(s) would be added"), "{rendered}");
}

#[test]
fn a_file_that_only_shrank_says_so_too() {
    let rendered = render("a\nb\nc\n", "a\nb\n");
    assert!(rendered.contains("-     3 | c"), "{rendered}");
    assert!(rendered.contains("+     3 | (end of file)"), "{rendered}");
    assert!(
        rendered.contains("1 line(s) would be removed"),
        "{rendered}"
    );
}

#[test]
fn an_empty_file_against_a_generated_one_is_a_difference_at_line_one() {
    let rendered = render("", "a\n");
    assert!(rendered.contains("-     1 | (end of file)"), "{rendered}");
    assert!(rendered.contains("+     1 | a"), "{rendered}");
}

#[test]
fn a_long_line_is_cut_rather_than_echoed() {
    // Both sides come from a schema this tool does not control.
    let rendered = render(&"a".repeat(10_000), &"b".repeat(10_000));
    assert!(rendered.len() < 600, "{} bytes", rendered.len());
    assert!(rendered.contains('…'), "{rendered}");
}

#[test]
fn a_multibyte_line_is_cut_on_a_character_boundary() {
    let rendered = render(&"é".repeat(500), &"è".repeat(500));
    assert!(rendered.contains('…'));
}

#[test]
fn a_trailing_newline_difference_alone_is_reported_as_such() {
    // `lines()` discards the final terminator, so this is the one difference a
    // line-based diff could miss entirely. `render` compares the strings first,
    // which is why it does not — and pointing at a line would be useless here,
    // so it names the terminator instead.
    let rendered = render("a\nb", "a\nb\n");
    assert_eq!(
        rendered,
        "  every line matches; the file on disk does not end with a newline, \
         the generated text ends with a newline.\n"
    );
}

#[test]
fn an_extra_trailing_newline_is_a_line_difference_not_a_terminator_one() {
    // "a\n\n" has a second, empty line; that is a real line the file lacks.
    let rendered = render("a\n", "a\n\n");
    assert!(rendered.contains("-     2 | (end of file)"), "{rendered}");
}
