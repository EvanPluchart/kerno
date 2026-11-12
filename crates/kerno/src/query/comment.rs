//! Toggling SQL line comments on the lines of a selection.

use std::ops::Range;

/// The byte range of the whole lines that `selection` touches in `text`.
pub fn lines_of(text: &str, selection: Range<usize>) -> Range<usize> {
    let start = text[..selection.start]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    // A selection ending at the start of a line does not include that line.
    let last = if selection.end > selection.start && text[..selection.end].ends_with('\n') {
        selection.end - 1
    } else {
        selection.end
    };
    let end = text[last..]
        .find('\n')
        .map_or(text.len(), |newline| last + newline);
    start..end.max(start)
}

/// Comments the lines of `block` out with `-- `, or back in when every
/// line that holds something is already a comment. Blank lines are left
/// alone; the marker goes at the shallowest indentation of the block, so
/// the lines stay aligned.
pub fn toggle(block: &str) -> String {
    let lines: Vec<&str> = block.split('\n').collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let filled = || lines.iter().copied().filter(|line| !line.trim().is_empty());
    let commented = filled().all(|line| line.trim_start().starts_with("--"));
    let shallowest = filled().map(indent).min().unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            if commented {
                let at = indent(line);
                let rest = &line[at + 2..];
                format!("{}{}", &line[..at], rest.strip_prefix(' ').unwrap_or(rest))
            } else {
                format!("{}-- {}", &line[..shallowest], &line[shallowest..])
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_commented_out_and_back_in() {
        assert_eq!(toggle("select 1"), "-- select 1");
        assert_eq!(toggle("-- select 1"), "select 1");
        assert_eq!(toggle("--select 1"), "select 1");
        // The marker lines up at the shallowest indentation; blank lines stay blank.
        let block = "  select a,\n         b\n\n  from t";
        let out = "  -- select a,\n  --        b\n\n  -- from t";
        assert_eq!(toggle(block), out);
        assert_eq!(toggle(out), block);
        // A block that is only partly commented is commented out as a whole.
        assert_eq!(toggle("-- a\nb"), "-- -- a\n-- b");
        assert_eq!(toggle("-- -- a\n-- b"), "-- a\nb");
        assert_eq!(toggle(""), "");
        assert_eq!(toggle("\n\n"), "\n\n");
    }

    #[test]
    fn a_selection_is_widened_to_whole_lines() {
        let text = "one\ntwo\nthree";
        assert_eq!(&text[lines_of(text, 5..5)], "two");
        assert_eq!(&text[lines_of(text, 1..6)], "one\ntwo");
        // Ending right after a line break does not take the next line.
        assert_eq!(&text[lines_of(text, 0..4)], "one");
        assert_eq!(&text[lines_of(text, 0..0)], "one");
        assert_eq!(&text[lines_of(text, 13..13)], "three");
        assert_eq!(&text[lines_of("", 0..0)], "");
    }
}
