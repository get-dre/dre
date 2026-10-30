//! The query stage: each statement on its connection's session, in strict YAML order, its
//! result spooled to disk; or the results saved by an earlier run (`--from-results`,
//! `--retry-failed`); or, for `validate --live`, each statement checked against the database.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arrow::ipc::writer::FileWriter;
use dre_protocol::CAP_SESSIONS;
use dre_protocol::host::Execution;

use super::session::{Pool, PoolRunner, Session, masked_source_error, protected_sql};
use super::{BindingRun, Fail, Produced, Statement, thousands};
use crate::codes::Code;
use crate::engine::Level;
use crate::formats::{self, Format};
use crate::profiles::Role;
use crate::project::QueryEntry;
use crate::render::{Connections, QueryRunner};
use crate::sqlsplit::StatementKind;

impl BindingRun<'_> {
    /// Run the Binding's queries: each rendered just before it runs (a managed report), or
    /// every one rendered first.
    pub(super) fn query(&mut self, per_connection: &BTreeMap<String, usize>) -> Result<(), Fail> {
        if self.stages.interleave {
            return self.query_interleaved(per_connection);
        }
        let statements = self.render_all(per_connection)?;
        self.check_all_sessions(&statements, per_connection)?;
        let pool = self.pool.clone().expect("the pool is made before rendering");
        std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        for (i, st) in statements.iter().enumerate() {
            let session = pool.session(&st.connection)?;
            self.execute_statement(&session, i, st)?;
        }
        Ok(())
    }

    /// `validate --live`: render everything, then check it against the database.
    pub(super) fn check(&mut self, per_connection: &BTreeMap<String, usize>) -> Result<(), Fail> {
        let statements = self.render_all(per_connection)?;
        self.check_all_sessions(&statements, per_connection)?;
        self.live_check(&statements)
    }

    /// A managed report's real run: each query renders just before it runs, so a template can
    /// look at what earlier queries made (`columns()` of a temp table).
    fn query_interleaved(&mut self, per_connection: &BTreeMap<String, usize>) -> Result<(), Fail> {
        let pool = self.pool.clone().expect("the pool is made before rendering");
        self.render_destinations().map_err(|e| e.or(Code::RenderFailed))?;
        let mut checked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (c, n) in per_connection {
            if *n > 1 {
                self.check_sessions(&pool, c, *n)?;
                checked.insert(c.clone());
            }
        }
        std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        let mut i = 0;
        let mut two_tabs = None;
        let queries = self.b.queries.clone();
        for q in &queries {
            let conn = self.connection_of(&q.query)?;
            let renderer = self.renderer(Some(&conn))?;
            let sts = self.render_query(&renderer, q, &conn, &mut two_tabs)?;
            for w in renderer.take_warnings() {
                self.ui.warn(&w);
            }
            if let Some(e) = two_tabs.take() {
                return Err(e.into());
            }
            if sts.len() > 1 && checked.insert(conn.clone()) {
                self.check_sessions(&pool, &conn, sts.len())?;
            }
            let session = pool.session(&conn)?;
            for st in &sts {
                self.execute_statement(&session, i, st)?;
                i += 1;
            }
        }
        Ok(())
    }

    /// The one-session-per-connection guarantee, for every connection that runs several
    /// statements.
    fn check_all_sessions(
        &self,
        statements: &[Statement],
        per_connection: &BTreeMap<String, usize>,
    ) -> Result<(), Fail> {
        let pool = self.pool.clone().expect("the pool is made before rendering");
        for conn in per_connection.keys() {
            let n = statements.iter().filter(|s| &s.connection == conn).count();
            if n > 1 {
                self.check_sessions(&pool, conn, n)?;
            }
        }
        Ok(())
    }

    /// Fail unless `connection`'s plugin can hold one session across `n` statements.
    fn check_sessions(&self, pool: &Pool, connection: &str, n: usize) -> Result<(), Fail> {
        let session = pool.session(connection)?;
        let mut s = session.lock().unwrap();
        if !s
            .get()
            .map_err(|e| Fail::new(Code::ConnectionFailed, e))?
            .has(CAP_SESSIONS)
        {
            let kind = pool.kind(connection).unwrap_or_default();
            return Err(format!(
                "this Binding runs {n} statements on connection `{connection}`, but the `{kind}` source plugin can't hold one session across them; nothing was run"
            ).into());
        }
        Ok(())
    }

    /// The `secret_columns` of `query` in this Binding.
    pub(super) fn secret_columns(&self, query: &str) -> Vec<String> {
        self.b
            .queries
            .iter()
            .filter(|q| q.query == query)
            .flat_map(|q| q.secret_columns.iter().cloned())
            .collect()
    }

    /// Run statement `i`, spooling its result if it makes a tab.
    fn execute_statement(
        &mut self,
        session: &Arc<Mutex<Session>>,
        i: usize,
        st: &Statement,
    ) -> Result<(), Fail> {
        self.not_cancelled()?;
        let sql_log = self.ui.sql_log();
        sql_log(
            &format!("{}:{}", st.file.display(), st.line),
            &protected_sql(self.ctx.secrets(), &st.text, st.sensitive),
        );
        let spool_path = self.run_dir.join(".spool").join(format!("{i}.arrow"));
        let mut writer: Option<FileWriter<File>> = None;
        let t = Instant::now();
        let exec = {
            let mut s = session.lock().unwrap();
            let (p, _log_scope) = s
                .statement(st.sensitive)
                .map_err(|e| Fail::new(Code::ConnectionFailed, e))?;
            p.execute(&st.text, self.stages.row_limit, &mut |schema, batch| {
                if writer.is_none() {
                    let f = File::create(&spool_path).map_err(|e| e.to_string())?;
                    writer = Some(FileWriter::try_new(f, schema).map_err(|e| e.to_string())?);
                }
                writer
                    .as_mut()
                    .expect("the writer was created above")
                    .write(&batch)
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| {
                let message = format!(
                    "{}:{}: {}",
                    st.file.display(),
                    st.line,
                    masked_source_error(self.ctx.secrets(), &e, st.sensitive)
                );
                Fail::from_plugin(&e, message).or(Code::QueryFailed)
            })?
        };
        let what = match &exec {
            Execution::Result { rows, .. } => {
                format!("{} row{}", thousands(*rows), if *rows == 1 { "" } else { "s" })
            }
            Execution::NoResult {
                rows_affected: Some(n),
            } => format!("no result set ({n} affected)"),
            Execution::NoResult { .. } => "no result set".to_string(),
        };
        let at = format!("{}:{}", st.file.display(), st.line);
        self.ui.step(
            Level::Debug,
            "Executed",
            &format!("{at}  {what}"),
            Some(t.elapsed()),
        );
        // The YAML decides the tabs; the result only has to match it. A tab with no rows
        // still gets its column names.
        let (schema, rows) = match (st.tab, exec) {
            (true, Execution::Result { schema, rows }) => (schema, rows),
            (true, Execution::NoResult { .. }) => {
                return Err(Fail::new(
                    Code::NoResultSet,
                    format!(
                        "{at}: `{}` makes a tab, but its last statement returned no result set; if the query only prepares data (a temp view, a SET), add `tab: false` to it in the YAML",
                        st.query
                    ),
                ));
            }
            (false, _) => {
                drop(writer);
                let _ = std::fs::remove_file(&spool_path);
                return Ok(());
            }
        };
        {
            let mut w = match writer {
                Some(w) => w,
                None => FileWriter::try_new(File::create(&spool_path).map_err(|e| e.to_string())?, &schema)
                    .map_err(|e| e.to_string())?,
            };
            w.finish().map_err(|e| e.to_string())?;
            let secret = self.secret_columns(&st.query);
            for c in secret.iter().filter(|c| schema.field_with_name(c).is_err()) {
                self.ui.warn(&format!(
                    "  `{}` has no column `{c}`, which its `secret_columns` names: nothing of it is masked; check the spelling",
                    st.query
                ));
            }
            crate::secret_columns::register(&spool_path, &secret, self.ctx.secrets())?;
            self.produced.push(Produced {
                query: st.query.clone(),
                connection: st.connection.clone(),
                schema,
                rows,
                spool: spool_path,
                name: String::new(),
                anchor: None,
                header: None,
                columns: Default::default(),
                autofit: None,
                style: None,
            });
        }
        Ok(())
    }

    /// `--from-results`: this Binding's results from `dir` (saved by `--output-json`), as if its
    /// queries had run. No query runs and no connection opens; a query whose SQL changed since
    /// is warned about, when its SQL renders without the database.
    pub(super) fn replay(&mut self, dir: &Path) -> Result<(), Fail> {
        let shown = dir.display().to_string();
        let dir = self
            .project
            .root()
            .join(dir)
            .join(&self.report.name)
            .join(self.b.dir_name());
        if let Err(e) = crate::saved::read_index(&dir) {
            return Err(format!(
                "--from-results {shown}: no saved results for this report and Set ({e}); save them with `dre run {} --no-deliver --output-json {shown}`",
                self.report.name
            )
            .into());
        }
        self.replay_from(&dir, &format!("--from-results {shown}"))
    }

    /// Replay the saved results in `dir` (an index and its streams); `shown` names them in
    /// messages.
    pub(super) fn replay_from(&mut self, dir: &Path, shown: &str) -> Result<(), Fail> {
        let dir = dir.to_path_buf();
        let index = crate::saved::read_index(&dir).map_err(|e| format!("{shown}: {e}"))?;
        let queries: Vec<QueryEntry> = self.b.queries.iter().filter(|q| q.tab).cloned().collect();
        let missing: Vec<&str> = queries
            .iter()
            .filter(|q| index.find(&q.query).is_none())
            .map(|q| q.query.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{shown}: no saved result for {} (saved: {}); save them again with `--output-json`",
                missing
                    .iter()
                    .map(|q| format!("`{q}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                index
                    .results
                    .iter()
                    .map(|r| r.query.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into());
        }
        // Warn when a query's SQL changed since the results were saved.
        for q in &queries {
            let saved = index.find(&q.query).and_then(|r| r.sql_sha256.clone());
            let rendered = self
                .connection_of(&q.query)
                .and_then(|conn| {
                    let renderer = self.renderer(Some(&conn))?;
                    let mut two = None;
                    self.render_query(&renderer, q, &conn, &mut two)
                })
                .map(|_| {
                    std::fs::read_to_string(self.compiled_dir.join(format!("{}.sha256", q.query)))
                        .ok()
                        .map(|h| h.trim().to_string())
                });
            if let Some(r) = index.find(&q.query)
                && !r.masked_columns.is_empty()
            {
                self.ui.warn(&format!(
                    "  `{}`'s saved results have its `secret_columns` ({}) as `*****`: what they feed (a password, a message) renders as `*****`, so don't deliver this replay",
                    q.query,
                    r.masked_columns.join(", ")
                ));
            }
            match (rendered, saved) {
                (Ok(Some(now)), Some(then)) if now != then => self.ui.warn(&format!(
                    "  `{}`'s SQL changed since its results were saved; re-run with `--output-json` to refresh them",
                    q.query
                )),
                (Err(e), _) => self.ui.warn(&format!(
                    "  `{}`: can't render its SQL to compare with the saved results ({}); using them as saved",
                    q.query, e.message
                )),
                _ => {}
            }
        }
        std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        for (i, q) in queries.iter().enumerate() {
            let r = index.find(&q.query).expect("checked above");
            let spool = self.run_dir.join(".spool").join(format!("{i}.arrow"));
            let (schema, rows) = crate::saved::stream_to_spool(&dir.join(&r.file), &spool)?;
            crate::secret_columns::register(&spool, &self.secret_columns(&q.query), self.ctx.secrets())?;
            self.produced.push(Produced {
                query: q.query.clone(),
                connection: r.connection.clone(),
                schema,
                rows,
                spool,
                name: String::new(),
                anchor: None,
                header: None,
                columns: Default::default(),
                autofit: None,
                style: None,
            });
        }
        self.ui.step(
            Level::Info,
            "Replayed",
            &format!("{} result(s) from {shown}; no query ran", queries.len()),
            None,
        );
        Ok(())
    }

    /// Tab names: `tab_name`, else the query's basename.
    pub(super) fn name_result_sets(&mut self) -> Result<(), Fail> {
        let entries: BTreeMap<&str, &QueryEntry> =
            self.b.queries.iter().map(|q| (q.query.as_str(), q)).collect();
        for p in &mut self.produced {
            // Every result set comes from one of the Binding's queries.
            let q = entries[p.query.as_str()];
            p.name = q.tab_name.clone().unwrap_or_else(|| q.query.clone());
            p.anchor = q.anchor.clone();
            p.header = q.header;
            p.columns = q.columns.clone();
            p.autofit = q.autofit;
            p.style = q.style.clone();
        }
        for o in self
            .b
            .outputs
            .iter()
            .filter(|o| Format::new(&o.format).is_workbook())
        {
            let sheets = self.produced.iter().filter(|p| o.feeds(&p.query));
            formats::check_sheets(sheets.map(|p| (p.name.as_str(), p.query.as_str())))?;
        }
        Ok(())
    }

    /// `dre validate --live`: execute temp creates, `check` everything else, each on its
    /// connection; then check declared source columns against the database.
    fn live_check(&mut self, statements: &[Statement]) -> Result<(), Fail> {
        let pool = self.pool.clone().expect("the pool is made before rendering");
        let mut failures = Vec::new();
        let mut unexecuted_setup: BTreeMap<String, (PathBuf, usize)> = BTreeMap::new();
        let mut uncheckable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let sql_log = self.ui.sql_log();
        for st in statements {
            if uncheckable.contains(&st.connection) {
                continue;
            }
            let session = pool.session(&st.connection)?;
            let mut s = session.lock().unwrap();
            if !s.get()?.has(dre_protocol::CAP_CHECK) {
                let kind = pool.kind(&st.connection).unwrap_or_default();
                self.ui.warn(&format!(
                    "  not checkable: the `{kind}` source plugin (connection `{}`) can't check statements without running them",
                    st.connection
                ));
                uncheckable.insert(st.connection.clone());
                continue;
            }
            let verb = if st.kind == StatementKind::TempCreate {
                ""
            } else {
                " (check)"
            };
            sql_log(
                &format!("{}:{}{verb}", st.file.display(), st.line),
                &protected_sql(self.ctx.secrets(), &st.text, st.sensitive),
            );
            if st.kind == StatementKind::TempCreate {
                let result = {
                    let (process, _log_scope) = s.statement(st.sensitive)?;
                    process.execute(&st.text, None, &mut |_, _| Ok(()))
                };
                if let Err(e) = result {
                    failures.push(format!(
                        "{}:{}: {}",
                        st.file.display(),
                        st.line,
                        masked_source_error(self.ctx.secrets(), &e, st.sensitive)
                    ));
                }
                continue;
            }
            let result = {
                let (process, _log_scope) = s.statement(st.sensitive)?;
                process.check(&st.text)
            };
            if let Err(e) = result {
                let mut msg = format!(
                    "{}:{}: {}",
                    st.file.display(),
                    st.line,
                    masked_source_error(self.ctx.secrets(), &e, st.sensitive)
                );
                if let Some((f, l)) = unexecuted_setup.get(&st.connection) {
                    msg.push_str(&format!(
                        " (may be a false positive: the setup statement at {}:{l} wasn't executed during the check)",
                        f.display()
                    ));
                }
                failures.push(msg);
            }
            if st.kind == StatementKind::Other {
                unexecuted_setup.insert(st.connection.clone(), (st.file.clone(), st.line));
            }
        }
        self.check_source_columns(&pool, &mut failures);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Fail::new(Code::QueryFailed, failures.join("\n    ")))
        }
    }

    /// Declared source `columns` against the real table, once per source table, connection and
    /// target in this process: each must exist, and match its `data_type` (loosely) where given.
    fn check_source_columns(&mut self, pool: &Arc<Pool>, failures: &mut Vec<String>) {
        for q in self.parsed.queries.clone() {
            let Some(conn) = q.connection.clone() else {
                continue;
            };
            for key in &q.sources {
                let Some(r) = self.parsed.sources.get(key).cloned() else {
                    continue;
                };
                let Some(declared) = self
                    .project
                    .sources()
                    .get(&r.source)
                    .and_then(|s| s.table(&r.table))
                    .map(|t| t.columns.clone())
                    .filter(|c| !c.is_empty())
                else {
                    continue;
                };
                let id = (
                    self.project.root().to_path_buf(),
                    key.clone(),
                    conn.clone(),
                    self.project.profiles().target_of(Role::Connection, &conn),
                );
                if !self.ctx.first_check(id) {
                    continue;
                }
                let quote = if r.quoting.any() {
                    let kind = pool.kind(&conn).unwrap_or_default();
                    match pool.connections.identifier_quote(&kind) {
                        Ok(q) => q,
                        Err(e) => {
                            failures.push(format!("source `{key}`: can't quote its name: {e}"));
                            continue;
                        }
                    }
                } else {
                    None
                };
                let rel = r.relation(quote.as_deref());
                let runner = PoolRunner {
                    pool: pool.clone(),
                    secrets: self.ctx.secrets().clone(),
                    default: Some(conn.clone()),
                    log: self.ui.sql_log(),
                    failed: Default::default(),
                };
                let actual = match runner.columns(&format!("select * from {rel} where 1=0"), None) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!("source `{key}` ({rel} on connection `{conn}`): {e}"));
                        continue;
                    }
                };
                for c in &declared {
                    let Some(a) = actual.iter().find(|a| a.name.eq_ignore_ascii_case(&c.name)) else {
                        failures.push(format!(
                            "source `{key}`: declared column `{}` isn't in {rel} on connection `{conn}` (it has: {})",
                            c.name,
                            actual.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
                        ));
                        continue;
                    };
                    let Some(dt) = &c.data_type else { continue };
                    match crate::coltypes::matches(dt, &a.data_type) {
                        Some(true) => {}
                        Some(false) => failures.push(format!(
                            "source `{key}`: column `{}` is declared `{dt}`, but {rel} on connection `{conn}` returns {}",
                            c.name, a.data_type
                        )),
                        None => self.ui.warn(&format!(
                            "  source `{key}`: column `{}`'s `data_type: {dt}` isn't a type DRE can compare, so only its presence was checked",
                            c.name
                        )),
                    }
                }
            }
        }
    }
}
