//! Engine maintenance tools (vacuum, analyze, check, optimize, …) and their
//! options, as DBeaver offers them under Tools ▸. Each dialect writes the
//! statement of a tool for an object ([`crate::Dialect::maintenance_sql`])
//! and says which tools apply to which object
//! ([`crate::Dialect::maintenance_tools`]).

/// A maintenance tool with its options. The engines and object kinds each
/// one applies to are the dialect's to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Maintenance {
    /// PostgreSQL `VACUUM (VERBOSE, …)` of a table or of the database.
    Vacuum(VacuumOptions),
    /// `ANALYZE`: PostgreSQL (`VERBOSE`, table or database), MySQL table.
    Analyze,
    /// PostgreSQL `REFRESH MATERIALIZED VIEW`.
    RefreshMaterializedView(RefreshOptions),
    /// `TRUNCATE TABLE` with PostgreSQL's options; MySQL takes none.
    Truncate(TruncateOptions),
    /// PostgreSQL `ALTER TABLE … ENABLE TRIGGER`.
    EnableTrigger,
    /// PostgreSQL `ALTER TABLE … DISABLE TRIGGER`.
    DisableTrigger,
    /// MySQL `CHECK TABLE`, with at most one option.
    Check(Option<CheckOption>),
    /// MySQL `OPTIMIZE TABLE`.
    Optimize,
    /// MySQL `REPAIR TABLE`.
    Repair(RepairOptions),
}

impl Maintenance {
    /// Every tool at its default options, in menu order.
    pub fn all() -> Vec<Maintenance> {
        vec![
            Maintenance::Vacuum(VacuumOptions::default()),
            Maintenance::Analyze,
            Maintenance::RefreshMaterializedView(RefreshOptions::default()),
            Maintenance::Truncate(TruncateOptions::default()),
            Maintenance::EnableTrigger,
            Maintenance::DisableTrigger,
            Maintenance::Check(None),
            Maintenance::Optimize,
            Maintenance::Repair(RepairOptions::default()),
        ]
    }
}

/// Options of `VACUUM`, all off by default (the server's defaults).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VacuumOptions {
    /// `FULL`: rewrites the table, under an exclusive lock.
    pub full: bool,
    /// `FREEZE`.
    pub freeze: bool,
    /// `ANALYZE`: updates the planner statistics too.
    pub analyze: bool,
    /// `DISABLE_PAGE_SKIPPING` (not with `FULL`).
    pub disable_page_skipping: bool,
    /// `SKIP_LOCKED`.
    pub skip_locked: bool,
    /// `INDEX_CLEANUP`: forces index vacuuming (the server's default is AUTO).
    pub index_cleanup: bool,
}

/// Options of `REFRESH MATERIALIZED VIEW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshOptions {
    /// `CONCURRENTLY`: readers are not blocked; the view needs a unique
    /// index and data (not with `with_data` off).
    pub concurrently: bool,
    /// `WITH DATA`; off, `WITH NO DATA` leaves the view unreadable until
    /// the next refresh.
    pub with_data: bool,
}

impl Default for RefreshOptions {
    fn default() -> Self {
        RefreshOptions {
            concurrently: false,
            with_data: true,
        }
    }
}

/// Options of PostgreSQL's `TRUNCATE`, all off by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TruncateOptions {
    /// `ONLY`: not the tables that inherit from this one.
    pub only: bool,
    /// `RESTART IDENTITY`: restarts the sequences the columns own.
    pub restart_identity: bool,
    /// `CASCADE`: also empties the tables that reference this one.
    pub cascade: bool,
}

/// The option of MySQL's `CHECK TABLE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOption {
    /// `FOR UPGRADE`.
    ForUpgrade,
    /// `QUICK`.
    Quick,
    /// `FAST`.
    Fast,
    /// `MEDIUM`.
    Medium,
    /// `EXTENDED`.
    Extended,
    /// `CHANGED`.
    Changed,
}

impl CheckOption {
    /// Every option, in DBeaver's order.
    pub const ALL: [CheckOption; 6] = [
        CheckOption::ForUpgrade,
        CheckOption::Quick,
        CheckOption::Fast,
        CheckOption::Medium,
        CheckOption::Extended,
        CheckOption::Changed,
    ];

    /// The keywords of the option.
    pub fn sql(self) -> &'static str {
        match self {
            CheckOption::ForUpgrade => "FOR UPGRADE",
            CheckOption::Quick => "QUICK",
            CheckOption::Fast => "FAST",
            CheckOption::Medium => "MEDIUM",
            CheckOption::Extended => "EXTENDED",
            CheckOption::Changed => "CHANGED",
        }
    }
}

/// Options of MySQL's `REPAIR TABLE`, all off by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairOptions {
    /// `QUICK`.
    pub quick: bool,
    /// `EXTENDED`.
    pub extended: bool,
    /// `USE_FRM`.
    pub use_frm: bool,
}

/// The statement of a tool for one object, and how the client runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceSql {
    /// The statement.
    pub sql: String,
    /// Whether the server answers with rows to show (MySQL: `Table`, `Op`,
    /// `Msg_type`, `Msg_text`). Otherwise its report, if any, comes as
    /// notices (PostgreSQL `VERBOSE`).
    pub returns_rows: bool,
    /// Whether it must run outside a transaction, on an auto-commit
    /// session: PostgreSQL refuses `VACUUM` in a transaction block, and
    /// MySQL commits the open transaction before each of its tools.
    pub autocommit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dialect;
    use crate::dialect::test_support::AnsiDialect;
    use crate::schema::{ObjectRef, RelationKind, TableRef};

    #[test]
    fn defaults_are_the_servers_and_refresh_keeps_the_data() {
        assert_eq!(Maintenance::all().len(), 9);
        assert!(RefreshOptions::default().with_data);
        assert!(!RefreshOptions::default().concurrently);
        assert_eq!(CheckOption::ALL.map(CheckOption::sql)[0], "FOR UPGRADE");
    }

    #[test]
    fn a_dialect_without_tools_offers_none() {
        let table = ObjectRef::Relation(TableRef::new("s", "t"), RelationKind::Table);
        assert!(AnsiDialect.maintenance_tools(&table).is_empty());
        assert!(matches!(
            AnsiDialect.maintenance_sql(&Maintenance::Analyze, &table),
            Err(crate::Error::Unsupported(_))
        ));
    }
}
