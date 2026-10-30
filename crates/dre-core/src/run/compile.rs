//! The render stage: each query's SQL into `target/compiled/` and its statements, and each
//! destination's path and options. Compiling (`dre compile`, `validate`, `--dry-run`) stops here,
//! with what the Binding would do.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map as JsonMap, Value as Json};

use super::session::masked_sql_summary;
use super::{
    BindingPlan, BindingRun, Fail, PlannedDestination, PlannedOutput, PlannedQuery, RenderedDest, Statement,
    rel,
};
use crate::codes::Code;
use crate::engine::Level;
use crate::profiles::{Entry, Role};
use crate::project::QueryEntry;
use crate::render::{Connections, RenderError, Renderer};
use crate::sqlsplit::{self, StatementKind};
use std::sync::atomic::Ordering;

impl BindingRun<'_> {
    /// Compile: render everything and say what the Binding would do. A render that failed
    /// because a template queried the database is `compile-needs-run`: only a run can check it.
    pub(super) fn compile(&mut self, per_connection: &BTreeMap<String, usize>) -> Result<(), Fail> {
        self.render_all(per_connection).map_err(|e| {
            if self.query_failed.load(Ordering::SeqCst) {
                Fail::new(Code::CompileNeedsRun, e.message)
            } else {
                e
            }
        })?;
        let plan = self.plan();
        self.ui.compiled(&plan);
        Ok(())
    }

    /// Render every query, then every destination, before anything runs: compiling, checking,
    /// and an unmanaged report, whose statements must all read before any runs.
    pub(super) fn render_all(
        &mut self,
        per_connection: &BTreeMap<String, usize>,
    ) -> Result<Vec<Statement>, Fail> {
        let mut statements = Vec::new();
        // Reported after the unmanaged-report check, which matters more.
        let mut two_tabs: Option<String> = None;
        let queries = self.b.queries.clone();
        for q in &queries {
            let conn = self.connection_of(&q.query)?;
            let renderer = self.renderer(Some(&conn))?;
            statements.extend(self.render_query(&renderer, q, &conn, &mut two_tabs)?);
        }
        self.render_destinations().map_err(|e| e.or(Code::RenderFailed))?;
        for r in self.renderers.values() {
            for w in r.take_warnings() {
                self.ui.warn(&w);
            }
        }
        if !self.report.managed {
            self.check_unmanaged(&statements, per_connection)?;
        }
        match two_tabs {
            Some(e) => Err(e.into()),
            None => Ok(statements),
        }
    }

    /// An unmanaged report: every rendered statement must only read (or create temp objects),
    /// and its sessions open read-only unless it does.
    fn check_unmanaged(
        &self,
        statements: &[Statement],
        per_connection: &BTreeMap<String, usize>,
    ) -> Result<(), Fail> {
        if let Some(bad) = statements.iter().find(|s| !s.kind.is_read_only_safe()) {
            return Err(format!(
                "{}:{}: unmanaged report `{}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; nothing was run — rewrite the statement, or give the report a YAML to declare it",
                bad.file.display(),
                bad.line,
                self.report.name,
                masked_sql_summary(self.ctx.secrets(), &bad.text, 60, bad.sensitive)
            ).into());
        }
        let pool = self.pool.clone().expect("the pool is made before rendering");
        for conn in per_connection.keys() {
            let session = pool.session(conn)?;
            let creates_temp = statements
                .iter()
                .any(|s| &s.connection == conn && s.kind == StatementKind::TempCreate)
                || session.lock().unwrap().loaded;
            session.lock().unwrap().want_read_only(!creates_temp);
        }
        Ok(())
    }

    /// Render one query file into its statements, writing it to `target/compiled/`.
    pub(super) fn render_query(
        &mut self,
        renderer: &Renderer,
        q: &QueryEntry,
        connection: &str,
        two_tabs: &mut Option<String>,
    ) -> Result<Vec<Statement>, Fail> {
        let mut out = Vec::new();
        let src = std::fs::read_to_string(self.project.root().join(&q.path))
            .map_err(|e| format!("{}: {e}", q.path.display()))?;
        let _ = renderer.take_sources();
        let sql = renderer
            .render(&q.path, &src)
            .map_err(|e: RenderError| e.to_string())?;
        // The manifest's `depends_on` and the query's connection come from the parse pass, so
        // a `source()` it couldn't see would make both wrong.
        let known = self
            .parsed
            .query(&q.query)
            .map(|p| p.sources.clone())
            .unwrap_or_default();
        if let Some((s, t)) = renderer
            .take_sources()
            .into_iter()
            .find(|(s, t)| !known.contains(&format!("{s}.{t}")))
        {
            return Err(format!(
                "{}: `source('{s}', '{t}')` was reached while rendering `{}`, but the parse pass didn't find it. The parse pass renders without data (`run_query()` returns no rows, `connection.*` nothing), so it can't see a `source()` that depends on them; call it where it's reached either way",
                q.path.display(),
                q.query
            ).into());
        }
        let sensitive = self.ctx.secrets().contains_secret(&sql);
        std::fs::write(
            self.compiled_dir.join(format!("{}.sql", q.query)),
            self.ctx.secrets().mask(&sql).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        // Hash the exact rendered SQL before masking display artifacts. Integrations can
        // identify executed SQL without receiving any secret embedded in it.
        let hash = crate::checksum::sha256_hex(sql.as_bytes());
        std::fs::write(self.compiled_dir.join(format!("{}.sha256", q.query)), hash)
            .map_err(|e| e.to_string())?;
        self.ui
            .step(Level::Debug, "Rendered", &q.path.display().to_string(), None);
        // 2. Split. One file makes at most one tab, from its last statement, so any earlier
        // SELECT would be lost: that's an error, not a guess.
        let parts = sqlsplit::split(&sql);
        if q.tab && parts.is_empty() {
            return Err(format!(
                "{}: `{}` makes a tab but has no statements; add a query, or `tab: false` if it's meant to be empty",
                q.path.display(),
                q.query
            ).into());
        }
        let n = parts.len();
        for (i, st) in parts.into_iter().enumerate() {
            let body = sqlsplit::strip_leading_comments(&st.text);
            let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
            let kind = sqlsplit::classify(&st.text);
            let last = i + 1 == n;
            if q.tab && !last && kind == StatementKind::Read && two_tabs.is_none() {
                *two_tabs = Some(format!(
                    "{}:{line}: `{}` has more than one SELECT, but one .sql file makes one tab (from its last statement); put each tab's query in its own .sql file and list each in `queries:`",
                    q.path.display(),
                    q.query
                ));
            }
            out.push(Statement {
                query: q.query.clone(),
                connection: connection.to_string(),
                file: q.path.clone(),
                line,
                kind,
                text: st.text,
                sensitive,
                tab: q.tab && last,
            });
        }
        Ok(out)
    }

    /// Each destination's rendered `profile`, `path` and options; `destination.*` is that
    /// destination while its values render.
    pub(super) fn render_destinations(&mut self) -> Result<(), Fail> {
        if !self.stages.destinations {
            return Ok(());
        }
        if self.b.destinations().next().is_none() {
            return Ok(());
        }
        let renderer = self.renderer(None)?;
        let connections = self
            .pool
            .as_ref()
            .expect("the pool is made before rendering")
            .connections
            .clone();
        for (key, d) in self.b.keyed_destinations() {
            let oi = key.output;
            // A per-row output's destinations are rendered for each row, with its `each`.
            if self.b.outputs[oi].for_each.is_some() {
                continue;
            }
            let profile = self
                .parsed
                .destination(key)
                .map(str::to_string)
                .ok_or_else(|| format!("destination `{}` has no profile", d.profile))?;
            // Nothing is delivered, so there's no path or options to render.
            if matches!(
                self.project.profiles().entry(Role::Destination, &profile),
                Entry::Nowhere
            ) {
                self.outs[oi].dests.push(RenderedDest {
                    profile,
                    path: None,
                    options: JsonMap::new(),
                    attach: Vec::new(),
                    hold: d.hold,
                    rate: self.opts.rate.or(d.rate),
                    zip: None,
                    zip_file: None,
                });
                continue;
            }
            renderer.set_destination(connections.profile(&profile, Some("destination")).ok());
            let rendered = (|| {
                let path = match &d.path {
                    Some(p) => Some(
                        renderer
                            .render(&self.report.file, p)
                            .map_err(|e| format!("output path: {e}"))?,
                    ),
                    None => None,
                };
                let mut options = JsonMap::new();
                for (k, v) in &d.options {
                    let v = render_json(&renderer, &self.report.file, v)
                        .map_err(|e| format!("destination `{profile}` option `{k}`: {e}"))?;
                    options.insert(k.clone(), v);
                }
                let zip = match &d.zip {
                    Some(z) => Some(crate::zip_pack::ZipSpec {
                        password: match &z.password {
                            Some(pw) => Some(
                                renderer
                                    .render(&self.report.file, pw)
                                    .map_err(|e| format!("destination `{profile}` `zip.password`: {e}"))?,
                            ),
                            None => None,
                        },
                        encryption: z.encryption,
                    }),
                    None => None,
                };
                Ok::<_, Fail>(RenderedDest {
                    profile: profile.clone(),
                    path,
                    options,
                    attach: d.attach.clone(),
                    hold: d.hold,
                    rate: self.opts.rate.or(d.rate),
                    zip,
                    zip_file: None,
                })
            })();
            renderer.set_destination(None);
            self.outs[oi].dests.push(rendered?);
        }
        Ok(())
    }

    /// What a real run of this Binding would use and produce, for `dre compile` and `validate`.
    fn plan(&mut self) -> BindingPlan {
        let compiled = self
            .b
            .queries
            .iter()
            .map(|q| {
                rel(
                    self.project.root(),
                    &self.compiled_dir.join(format!("{}.sql", q.query)),
                )
            })
            .collect();
        let mut outputs = Vec::new();
        for oi in 0..self.outs.len() {
            let file = self.file_names(oi);
            let o = &self.b.outputs[oi];
            let destinations = self.outs[oi]
                .dests
                .iter()
                .map(|d| {
                    let out = self.project.profiles().target(Role::Destination, &d.profile);
                    PlannedDestination {
                        profile: d.profile.clone(),
                        kind: out.map(|o| o.kind.clone()),
                        target: Some(self.project.profiles().target_of(Role::Destination, &d.profile)),
                        path: d.path.clone(),
                        delivers: out.is_some(),
                    }
                })
                .collect();
            outputs.push(PlannedOutput {
                name: o.name.clone(),
                format: o.format.clone(),
                output: rel(self.project.root(), &self.run_dir.join(file)),
                destinations,
            });
        }
        let first = outputs.first().cloned();
        let pool = self.pool.clone().expect("the pool is made before rendering");
        let queries = self
            .parsed
            .queries
            .iter()
            .map(|q| {
                let connection = q.connection.clone().unwrap_or_default();
                PlannedQuery {
                    query: q.query.clone(),
                    kind: pool.kind(&connection).unwrap_or_default(),
                    target: self.project.profiles().target_of(Role::Connection, &connection),
                    connection,
                    sources: q.sources.clone(),
                }
            })
            .collect();
        BindingPlan {
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            compiled,
            profile: self.parsed.inherited.clone(),
            queries,
            target: self.target.clone(),
            format: first.as_ref().map(|o| o.format.clone()).unwrap_or_default(),
            output: first.as_ref().map(|o| o.output.clone()).unwrap_or_default(),
            destinations: first.map(|o| o.destinations).unwrap_or_default(),
            outputs,
            schedules: self.b.schedules.clone(),
        }
    }
}

/// Render every string inside a destination option value.
pub(super) fn render_json(renderer: &Renderer, file: &Path, v: &Json) -> Result<Json, RenderError> {
    Ok(match v {
        Json::String(s) => Json::String(renderer.render(file, s)?),
        Json::Array(a) => Json::Array(
            a.iter()
                .map(|v| render_json(renderer, file, v))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), render_json(renderer, file, v)?)))
                .collect::<Result<_, RenderError>>()?,
        ),
        other => other.clone(),
    })
}
