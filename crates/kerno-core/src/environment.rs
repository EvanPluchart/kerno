//! Environments: what kind of system a connection points at, with its
//! colour and its rules. The three built-ins (local, staging, production)
//! can be edited but not deleted; the user adds their own.
//!
//! Ported from DBeaver's connection types
//! (`plugins/org.jkiss.dbeaver.model/src/org/jkiss/dbeaver/model/connection/DBPConnectionType.java`,
//! Apache-2.0): the same idea of predefined, non-deletable types with a
//! colour for each theme and per-type defaults (auto-commit, confirm
//! execution, confirm data changes, close idle transactions). Kerno's names,
//! colours and the second confirmation on writes are its own.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// Id of the built-in local environment, the default of every connection.
pub const LOCAL: &str = "local";
/// Id of the built-in staging environment.
pub const STAGING: &str = "staging";
/// Id of the built-in production environment.
pub const PRODUCTION: &str = "production";

/// How much of the window's chrome takes an environment's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tint {
    /// None: the chrome keeps the theme's colours.
    #[default]
    None,
    /// A wash of the colour.
    Soft,
    /// The colour at full strength: it cannot be missed.
    Strong,
}

impl Tint {
    pub const ALL: [Tint; 3] = [Self::None, Self::Soft, Self::Strong];

    /// Name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Soft => "Soft",
            Self::Strong => "Strong",
        }
    }
}

/// What an environment asks of the connections in it. A connection's own
/// settings (its commit mode, its read-only flag) win over these defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvironmentRules {
    /// Query tabs start in auto-commit (otherwise in manual commit).
    pub auto_commit: bool,
    /// Every statement run from an editor is shown and confirmed first.
    pub confirm_execute: bool,
    /// Pending data edits are shown and confirmed before they are applied.
    /// Kerno always does this (CLAUDE.md §1), whatever the value: the rule
    /// is kept for DBeaver's connection types and for exported files.
    pub confirm_edits: bool,
    /// Every statement that writes asks a second time (CLAUDE.md §1:
    /// production asks twice before any write).
    pub confirm_writes_twice: bool,
    /// New connections of this environment start read-only.
    pub read_only: bool,
    /// A transaction left open and idle this many seconds in a manual-commit
    /// tab, after it changed something, is rolled back.
    pub close_idle_transactions: Option<u32>,
}

impl Default for EnvironmentRules {
    /// Local's rules: auto-commit, nothing asked beyond what Kerno always
    /// asks.
    fn default() -> Self {
        Self {
            auto_commit: true,
            confirm_execute: false,
            confirm_edits: true,
            confirm_writes_twice: false,
            read_only: false,
            close_idle_transactions: None,
        }
    }
}

/// An environment: a name, a colour for each theme, how much of the chrome
/// it tints, and its rules. Colours are `#RRGGBB`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentDef {
    /// What connections store; never changes once created.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color_light: Option<String>,
    #[serde(default)]
    pub color_dark: Option<String>,
    #[serde(default)]
    pub tint: Tint,
    #[serde(default)]
    pub rules: EnvironmentRules,
}

impl EnvironmentDef {
    /// One of the three Kerno ships: editable, never deleted. Decided by
    /// the id, so no stored value can make production deletable.
    pub fn is_builtin(&self) -> bool {
        is_builtin(&self.id)
    }

    /// The colour for a light or dark theme, when it is a valid `#RRGGBB`.
    pub fn color(&self, dark: bool) -> Option<u32> {
        let hex = if dark {
            &self.color_dark
        } else {
            &self.color_light
        };
        hex.as_deref().and_then(parse_color)
    }

    /// Why the environment cannot be deleted while `users` connections are
    /// in it; `None` when it can.
    pub fn deletion_refused(&self, users: usize) -> Option<String> {
        if self.is_builtin() {
            return Some("A built-in environment can be edited, not deleted.".into());
        }
        match users {
            0 => None,
            1 => Some("1 connection uses it: move it to another environment first.".into()),
            users => Some(format!(
                "{users} connections use it: move them to another environment first."
            )),
        }
    }
}

/// Whether `id` names a built-in environment.
pub fn is_builtin(id: &str) -> bool {
    matches!(id, LOCAL | STAGING | PRODUCTION)
}

/// `#RRGGBB` as `0xRRGGBB`; `None` for anything else.
pub fn parse_color(hex: &str) -> Option<u32> {
    let digits = hex.trim().strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// The three environments Kerno ships, in their defaults. Production keeps
/// CLAUDE.md §1 true with no setup: manual commit and a second confirmation
/// on every write; like DBeaver's production type it also confirms every
/// execution and ends idle transactions after 10 minutes.
pub fn builtin_environments() -> Vec<EnvironmentDef> {
    let color = |hex: &str| Some(hex.to_string());
    vec![
        EnvironmentDef {
            id: LOCAL.into(),
            name: "Local".into(),
            color_light: None,
            color_dark: None,
            tint: Tint::None,
            rules: EnvironmentRules::default(),
        },
        EnvironmentDef {
            id: STAGING.into(),
            name: "Staging".into(),
            // Kerno's amber (the "yellow" connection tag).
            color_light: color("#9A6B00"),
            color_dark: color("#E6C04A"),
            tint: Tint::Soft,
            rules: EnvironmentRules::default(),
        },
        EnvironmentDef {
            id: PRODUCTION.into(),
            name: "Production".into(),
            // Kerno's red (the "red" connection tag).
            color_light: color("#C62F3D"),
            color_dark: color("#F26D78"),
            tint: Tint::Strong,
            rules: EnvironmentRules {
                auto_commit: false,
                confirm_execute: true,
                confirm_edits: true,
                confirm_writes_twice: true,
                read_only: false,
                close_idle_transactions: Some(600),
            },
        },
    ]
}

static LOCAL_FALLBACK: LazyLock<EnvironmentDef> = LazyLock::new(|| {
    builtin_environments()
        .into_iter()
        .find(|def| def.id == LOCAL)
        .expect("local is built in")
});

/// The environment `id` names in `environments`; an unknown id (a file from
/// another machine, an environment deleted elsewhere) gets local's.
pub fn environment<'a>(environments: &'a [EnvironmentDef], id: &str) -> &'a EnvironmentDef {
    environments
        .iter()
        .find(|def| def.id == id)
        .or_else(|| environments.iter().find(|def| def.id == LOCAL))
        .unwrap_or(&LOCAL_FALLBACK)
}

/// `environments` with each built-in that is missing put back (the user's
/// edits of the others kept), and later duplicates of an id dropped.
pub fn with_builtins(environments: Vec<EnvironmentDef>) -> Vec<EnvironmentDef> {
    let mut merged: Vec<EnvironmentDef> = Vec::with_capacity(environments.len() + 3);
    for builtin in builtin_environments() {
        let kept = environments.iter().find(|def| def.id == builtin.id);
        merged.push(kept.cloned().unwrap_or(builtin));
    }
    for def in environments {
        if !merged.iter().any(|kept| kept.id == def.id) {
            merged.push(def);
        }
    }
    merged
}

/// An id for a new environment named `name`, unused in `environments`.
pub fn unused_id(environments: &[EnvironmentDef], name: &str) -> String {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if slug.is_empty() {
        "environment".to_string()
    } else {
        slug
    };
    let taken = |id: &str| is_builtin(id) || environments.iter().any(|def| def.id == id);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("an unused number")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(environments: &[EnvironmentDef]) -> Vec<&str> {
        environments.iter().map(|def| def.id.as_str()).collect()
    }

    #[test]
    fn the_three_built_ins_are_there_and_cannot_be_deleted() {
        let builtins = builtin_environments();
        assert_eq!(ids(&builtins), [LOCAL, STAGING, PRODUCTION]);
        for def in &builtins {
            assert!(def.is_builtin());
            assert!(def.deletion_refused(0).is_some(), "{}", def.id);
        }
        let mut own = builtins[1].clone();
        own.id = "qa".into();
        assert!(!own.is_builtin());
        assert_eq!(own.deletion_refused(0), None);
        assert!(
            own.deletion_refused(1)
                .unwrap()
                .starts_with("1 connection uses")
        );
        assert!(
            own.deletion_refused(3)
                .unwrap()
                .starts_with("3 connections use")
        );
    }

    /// CLAUDE.md §1 holds with no setup: production opens in manual commit
    /// and asks twice before any write; local and staging ask nothing more.
    #[test]
    fn production_keeps_claude_md_section_1_by_default() {
        let builtins = builtin_environments();
        let production = environment(&builtins, PRODUCTION);
        assert!(!production.rules.auto_commit);
        assert!(production.rules.confirm_writes_twice);
        assert!(production.rules.confirm_execute);
        assert_eq!(production.rules.close_idle_transactions, Some(600));
        assert_eq!(production.tint, Tint::Strong);
        for id in [LOCAL, STAGING] {
            let rules = &environment(&builtins, id).rules;
            assert!(rules.auto_commit && !rules.confirm_writes_twice && !rules.confirm_execute);
            assert!(!rules.read_only && rules.close_idle_transactions.is_none());
        }
        // Every built-in confirms edits: Kerno always does.
        assert!(builtins.iter().all(|def| def.rules.confirm_edits));
        assert_eq!(environment(&builtins, LOCAL).tint, Tint::None);
        assert_eq!(environment(&builtins, STAGING).tint, Tint::Soft);
    }

    #[test]
    fn an_unknown_id_gets_local_rules() {
        let builtins = builtin_environments();
        assert_eq!(environment(&builtins, "gone").id, LOCAL);
        // Even when the list itself lost local.
        assert_eq!(environment(&[], PRODUCTION).id, LOCAL);
    }

    #[test]
    fn missing_built_ins_are_merged_back_and_edits_kept() {
        let mut edited = builtin_environments().remove(2);
        edited.name = "Live".into();
        edited.rules.confirm_execute = false;
        let own = EnvironmentDef {
            id: "qa".into(),
            name: "QA".into(),
            color_light: Some("#1D6FB8".into()),
            color_dark: None,
            tint: Tint::Soft,
            rules: EnvironmentRules::default(),
        };
        let merged = with_builtins(vec![own.clone(), edited.clone(), own.clone()]);
        assert_eq!(ids(&merged), [LOCAL, STAGING, PRODUCTION, "qa"]);
        assert_eq!(merged[2], edited);
        assert_eq!(merged[3], own);
    }

    #[test]
    fn colours_are_read_per_theme_and_only_as_rrggbb() {
        let builtins = builtin_environments();
        let production = environment(&builtins, PRODUCTION);
        assert_eq!(production.color(false), Some(0xC62F3D));
        assert_eq!(production.color(true), Some(0xF26D78));
        assert_eq!(environment(&builtins, LOCAL).color(false), None);
        assert_eq!(parse_color(" #a1b2c3 "), Some(0xA1B2C3));
        for wrong in [
            "",
            "#",
            "a1b2c3",
            "#a1b2c",
            "#a1b2c3d4",
            "#ggg000",
            "#+12345",
        ] {
            assert_eq!(parse_color(wrong), None, "{wrong}");
        }
    }

    #[test]
    fn new_ids_are_slugs_never_taken() {
        let mut environments = builtin_environments();
        assert_eq!(unused_id(&environments, "QA (Europe)"), "qa-europe");
        assert_eq!(unused_id(&environments, "Production"), "production-2");
        assert_eq!(unused_id(&environments, "  "), "environment");
        environments.push(EnvironmentDef {
            id: "qa".into(),
            ..environments[0].clone()
        });
        assert_eq!(unused_id(&environments, "qa"), "qa-2");
    }

    #[test]
    fn stored_environments_round_trip_and_tolerate_missing_fields() {
        let builtins = builtin_environments();
        let json = serde_json::to_string(&builtins).unwrap();
        let back: Vec<EnvironmentDef> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, builtins);
        let sparse: EnvironmentDef =
            serde_json::from_str(r#"{"id":"qa","name":"QA","rules":{"read_only":true}}"#).unwrap();
        assert_eq!(sparse.tint, Tint::None);
        assert!(sparse.rules.read_only && sparse.rules.auto_commit);
    }
}
