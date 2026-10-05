//! The raw-SQL guard: what `query-raw` and `execute-raw` will carry.
//!
//! Raw SQL used to be judged by its first keyword. Two things follow from that
//! and both were reachable. A statement can be a read at the front and a write
//! inside — `WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x` begins with
//! `WITH`, so it read as read-only — and the first keyword is only the first
//! keyword if you skip comments the way the server does. PostgreSQL nests block
//! comments; the old scanner stopped at the first `*/`, so in
//! `/* /* */ SELECT 1 */ DROP TABLE t` it saw `SELECT` where the server saw
//! `DROP TABLE`.
//!
//! So the statement is parsed, with the PostgreSQL dialect, and judged as a
//! tree. Anything that does not parse is refused: an unparseable statement is
//! one whose meaning this module cannot vouch for, and the server would be the
//! only other thing that decides.
//!
//! This is one of two controls on `query-raw` and the weaker of them. The
//! stronger one is the transaction it runs in, which the database opens
//! `READ ONLY`, so a write the parser somehow blessed still cannot land. Raw
//! SQL remains a declared trust grant: a plugin that holds `raw_sql = true` can
//! read any table this guard does not protect, and the per-plugin database role
//! stays the stronger answer after 1.0.

use std::ops::ControlFlow;

use sqlparser::ast::{Expr, ObjectName, Query, Select, Statement, Visit, Visitor};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::plugin::db_policy::is_protected_table;

/// Why a raw-SQL statement was refused. Rendered into the host log; the ABI
/// code the caller sees is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawSqlReject {
    /// The statement does not parse as PostgreSQL.
    Unparseable(String),
    /// Not exactly one statement (including a stray semicolon).
    NotOneStatement,
    /// `query-raw` was given something that is not a read.
    NotAQuery,
    /// `execute-raw` was given something that is not one INSERT, UPDATE or DELETE.
    NotARowChange,
    /// A data-modifying statement inside a `query-raw` tree (a writable CTE).
    WriteInsideQuery,
    /// `SELECT ... INTO`, which creates a table.
    SelectInto,
    /// A row-locking clause (`FOR UPDATE` and its relatives).
    RowLock,
    /// The statement names a protected table.
    ProtectedTable(String),
    /// The statement calls a function on the denylist.
    DeniedFunction(String),
}

impl std::fmt::Display for RawSqlReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparseable(e) => write!(f, "raw SQL does not parse: {e}"),
            Self::NotOneStatement => write!(f, "raw SQL must be exactly one statement"),
            Self::NotAQuery => write!(f, "query-raw takes a read, and this is not one"),
            Self::NotARowChange => {
                write!(f, "execute-raw takes one INSERT, UPDATE or DELETE")
            }
            Self::WriteInsideQuery => {
                write!(f, "query-raw statement contains a data-modifying statement")
            }
            Self::SelectInto => write!(f, "SELECT ... INTO creates a table"),
            Self::RowLock => write!(f, "row-locking clauses are not allowed in query-raw"),
            Self::ProtectedTable(t) => write!(f, "raw SQL names the protected table {t}"),
            Self::DeniedFunction(n) => write!(f, "raw SQL calls the denied function {n}"),
        }
    }
}

/// Parse to exactly one statement, or say why not.
fn parse_one(sql: &str) -> Result<Statement, RawSqlReject> {
    let mut statements = Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|e| RawSqlReject::Unparseable(e.to_string()))?;
    if statements.len() != 1 {
        return Err(RawSqlReject::NotOneStatement);
    }
    Ok(statements.remove(0))
}

/// What `query-raw` will carry: one read, with no write anywhere inside it.
pub fn check_query_raw(sql: &str) -> Result<(), RawSqlReject> {
    let statement = parse_one(sql)?;
    if !matches!(statement, Statement::Query(_)) {
        return Err(RawSqlReject::NotAQuery);
    }
    walk(&statement, Mode::Read)
}

/// What `execute-raw` will carry: one INSERT, UPDATE or DELETE and nothing else.
pub fn check_execute_raw(sql: &str) -> Result<(), RawSqlReject> {
    let statement = parse_one(sql)?;
    match statement {
        Statement::Insert(_) | Statement::Update { .. } | Statement::Delete(_) => {}
        _ => return Err(RawSqlReject::NotARowChange),
    }
    walk(&statement, Mode::Write)
}

/// Which path is asking. A write inside the tree is the defect on the read path
/// and the whole point on the write path, and nothing else differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Read,
    Write,
}

/// Walk the parsed statement for protected tables, denied functions and — on
/// the read path — anything that writes.
fn walk(statement: &Statement, mode: Mode) -> Result<(), RawSqlReject> {
    let mut guard = Guard {
        mode,
        depth: 0,
        _private: (),
    };
    match statement.visit(&mut guard) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(reject) => Err(reject),
    }
}

struct Guard {
    mode: Mode,
    /// How many statements deep the walk is. The outermost statement is the one
    /// the caller sent and has already been matched; a statement found below it
    /// is a data-modifying CTE or nothing good.
    depth: usize,
    _private: (),
}

impl Visitor for Guard {
    type Break = RawSqlReject;

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        self.depth += 1;
        // The outermost statement was matched by the caller.
        if self.depth == 1 {
            return ControlFlow::Continue(());
        }
        // Anything nested is a statement smuggled into a query position: a
        // writable CTE on the read path, and on the write path a second
        // statement where exactly one was allowed.
        match statement {
            Statement::Query(_) => ControlFlow::Continue(()),
            _ => ControlFlow::Break(match self.mode {
                Mode::Read => RawSqlReject::WriteInsideQuery,
                Mode::Write => RawSqlReject::NotARowChange,
            }),
        }
    }

    fn post_visit_statement(&mut self, _statement: &Statement) -> ControlFlow<Self::Break> {
        self.depth = self.depth.saturating_sub(1);
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        // `FOR UPDATE` takes a row lock, which is a write in every sense that
        // matters here and is refused by a read-only transaction anyway.
        if self.mode == Mode::Read && !query.locks.is_empty() {
            return ControlFlow::Break(RawSqlReject::RowLock);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if select.into.is_some() {
            return ControlFlow::Break(RawSqlReject::SelectInto);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        let Some(name) = last_ident(relation) else {
            return ControlFlow::Continue(());
        };
        if is_protected_table(&name) {
            return ControlFlow::Break(RawSqlReject::ProtectedTable(name));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::Function(function) = expr
            && let Some(name) = last_ident(&function.name)
            && is_denied_function(&name)
        {
            return ControlFlow::Break(RawSqlReject::DeniedFunction(name));
        }
        ControlFlow::Continue(())
    }
}

/// The bare name an `ObjectName` ends in, lowercased and unquoted — the schema
/// qualifier dropped, because `public.users` and `users` are the same table.
fn last_ident(name: &ObjectName) -> Option<String> {
    name.0
        .last()?
        .as_ident()
        .map(|ident| ident.value.to_ascii_lowercase())
}

/// Functions a plugin may not call through raw SQL: the ones that change the
/// session the kernel's own queries then run in, reach the server's files, run
/// SQL text of their own, or take a table by name and read it for you.
///
/// `pg_sleep` is deliberately absent — a plugin is allowed to be slow, and the
/// queue worker's timeout test depends on it.
fn is_denied_function(name: &str) -> bool {
    // `set_config('search_path', ..., false)` outlives the transaction on a
    // pooled connection, which is the whole trick.
    if name == "set_config" {
        return true;
    }

    // Session-level advisory locks are held past the call and can wedge the
    // pool. The `_xact_` variants end with the transaction, so they are fine.
    if (name.starts_with("pg_advisory") || name.starts_with("pg_try_advisory"))
        && !name.contains("_xact_")
    {
        return true;
    }

    // Outbound SQL (`dblink`), large objects (`lo_*`, which read and write
    // server files), and the file and log readers.
    if name.starts_with("dblink") || name.starts_with("lo_") || name.starts_with("pg_ls_") {
        return true;
    }

    // The `*_to_xml` families take a table or a query by name and return its
    // contents, which walks straight around the relation check above.
    if name.ends_with("_to_xml")
        || name.ends_with("_to_xmlschema")
        || name.ends_with("_to_xml_and_xmlschema")
    {
        return true;
    }

    matches!(
        name,
        "pg_read_file"
            | "pg_read_binary_file"
            | "pg_stat_file"
            | "pg_terminate_backend"
            | "pg_cancel_backend"
            | "pg_reload_conf"
            | "pg_rotate_logfile"
    )
}
