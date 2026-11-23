//! What a newer release brings, from its own notes (K43, D-115): read from the notes file of
//! its tag only when a newer version was found and checks are on. The notes are outside text:
//! read as plain text, control characters dropped, Markdown's marks removed, every part capped.

use std::ffi::OsString;

use super::Version;

/// Largest notes file read.
pub const NOTES_MAX_BYTES: u64 = 64 * 1024;

/// Most leads shown, longest summary and lead (in characters).
const MAX_LEADS: usize = 8;
const MAX_SUMMARY: usize = 400;
const MAX_LEAD: usize = 100;

/// The notes file of `version`'s tag: plain text that does not change once tagged.
pub fn notes_url(version: Version) -> String {
    format!(
        "https://raw.githubusercontent.com/EvanPluchart/kerno/v{version}/docs/release-notes/v{version}.md"
    )
}

/// `curl`'s arguments to read the notes of `version` to standard output: HTTPS only, no
/// redirect, 5 s at most, a size cap, an HTTP error as a failure, the user agent of the check.
pub fn notes_args(current: &str, version: Version) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--disable",
        "--silent",
        "--fail",
        "--max-time",
        "5",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--max-filesize",
    ]
    .map(OsString::from)
    .into();
    args.push(NOTES_MAX_BYTES.to_string().into());
    args.push("--user-agent".into());
    args.push(format!("Kerno/{current}").into());
    args.push("--".into());
    args.push(notes_url(version).into());
    args
}

/// What a release brings: the summary paragraph and the bold lead of each item of its "New"
/// and "Better" sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNew {
    pub summary: String,
    pub leads: Vec<String>,
    /// More items than `leads` holds.
    pub more: bool,
}

/// Reads a release's notes. `None` when they are not what a notes file holds (no summary
/// paragraph, no item under "New" or "Better").
pub fn whats_new(notes: &[u8]) -> Option<WhatsNew> {
    let cut = &notes[..notes.len().min(NOTES_MAX_BYTES as usize)];
    let text: String = String::from_utf8_lossy(cut)
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let summary: Vec<&str> = text
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .collect();
    if summary.first().is_some_and(|line| line.starts_with('#')) {
        return None;
    }
    let summary = plain(&summary.join(" "), MAX_SUMMARY);
    if summary.is_empty() {
        return None;
    }
    let mut leads = Vec::new();
    let mut section = "";
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            section = heading.trim();
            continue;
        }
        let Some(item) = line.strip_prefix("- ") else {
            continue;
        };
        if !matches!(section, "New" | "Better") {
            continue;
        }
        let lead = match item
            .strip_prefix("**")
            .and_then(|rest| rest.split_once("**"))
        {
            Some((lead, _)) => lead,
            // An item without a lead: its first sentence.
            None => item.split(". ").next().unwrap_or(item),
        };
        let lead = plain(lead, MAX_LEAD);
        if !lead.is_empty() {
            leads.push(lead);
        }
    }
    if leads.is_empty() {
        return None;
    }
    let more = leads.len() > MAX_LEADS;
    leads.truncate(MAX_LEADS);
    Some(WhatsNew {
        summary,
        leads,
        more,
    })
}

/// `text` without Markdown's marks (`**`, backticks, `[t](u)` read as `t`), its spaces
/// collapsed, cut to `max` characters with "…".
fn plain(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let link = after
            .split_once("](")
            .and_then(|(label, tail)| tail.split_once(')').map(|(_, tail)| (label, tail)));
        match link {
            Some((label, tail)) => {
                out.push_str(label);
                rest = tail;
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let out = out.replace("**", "").replace('`', "");
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > max {
        let cut: String = out.chars().take(max - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes(version: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../docs/release-notes/v{version}.md",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn the_real_notes_give_their_summary_and_leads() {
        let new = whats_new(&notes("0.3.1")).unwrap();
        assert!(
            new.summary
                .starts_with("Kerno 0.3.1 answers the first reports of people replacing DBeaver"),
            "{}",
            new.summary
        );
        assert_eq!(
            new.leads[..2],
            [
                "A restore that names a missing role says so, and runs again without it.",
                "A table's rows load as you scroll."
            ]
        );
        // "Better"'s item has no lead: its first sentence, its backticks gone.
        assert!(
            new.leads
                .last()
                .unwrap()
                .starts_with("The url crate is updated"),
            "{:?}",
            new.leads
        );
        assert!(!new.more);
        let older = whats_new(&notes("0.3.0")).unwrap();
        assert_eq!(older.leads[0], "SQLite.");
        assert!(older.leads.len() <= MAX_LEADS);
        assert!(older.leads.iter().all(|lead| !lead.contains("**")));
    }

    #[test]
    fn hostile_notes_are_cut_cleaned_or_refused() {
        // No sections, no items: not a notes file.
        assert_eq!(whats_new(b"just a page\n\nno items"), None);
        assert_eq!(whats_new(b"# Heading first\n\n## New\n- **x.** y"), None);
        assert_eq!(whats_new(b""), None);
        // Control characters, a link, very long parts, many items.
        let mut text = String::from("Sum\u{1b}mary \u{7}with [a link](javascript:x) ");
        text.push_str(&"long ".repeat(500));
        text.push_str("\n\n## New\n\n");
        for n in 0..30 {
            text.push_str(&format!("- **Lead {n}\u{0}{}.** body\n", "x".repeat(300)));
        }
        let new = whats_new(text.as_bytes()).unwrap();
        assert!(
            new.summary.starts_with("Summary with a link long"),
            "{}",
            new.summary
        );
        assert!(!new.summary.contains("javascript"));
        assert!(new.summary.chars().count() <= MAX_SUMMARY);
        assert_eq!(new.leads.len(), MAX_LEADS);
        assert!(new.more);
        assert!(new.leads.iter().all(|lead| lead.chars().count() <= MAX_LEAD
            && !lead.chars().any(char::is_control)));
        // A huge file is read up to the cap only.
        let huge = format!(
            "Summary.\n\n## New\n- **One.** a\n{}",
            "z".repeat(10_000_000)
        );
        assert_eq!(whats_new(huge.as_bytes()).unwrap().leads, ["One."]);
    }

    #[test]
    fn the_notes_request_has_fixed_arguments() {
        let args: Vec<String> = notes_args("0.3.1", Version::parse("0.3.2").unwrap())
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "--disable",
                "--silent",
                "--fail",
                "--max-time",
                "5",
                "--proto",
                "=https",
                "--max-redirs",
                "0",
                "--max-filesize",
                "65536",
                "--user-agent",
                "Kerno/0.3.1",
                "--",
                "https://raw.githubusercontent.com/EvanPluchart/kerno/v0.3.2/docs/release-notes/v0.3.2.md",
            ]
        );
    }
}
