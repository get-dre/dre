//! The record stage: the run's results named and saved (`--output-json`, `--retry-failed`),
//! the schema compared with the last successful run's and recorded for the next, and the
//! Binding's `run_results.json`.

use serde_json::{Value as Json, json};

use super::per_row::SNAPSHOT_DIR;
use super::{BindingRun, Fail, OutputStatus, Status, record_path, rel, rfc3339};
use crate::for_each::safe_name;
use crate::run_results::{self, Delivery, RunResults};

impl BindingRun<'_> {
    /// Name the result sets, register the outputs' passwords as secrets, and save the results
    /// with `--output-json`.
    pub(super) fn results(&mut self) -> Result<(), Fail> {
        self.name_result_sets()?;
        // A password option (`password:`, `zip: {password}`) read through `env_var()` is a secret.
        for o in &self.b.outputs {
            self.ctx.secrets().register_secret_fields(&o.options, &[]);
            let mut passwords: Vec<&str> = o
                .options
                .get("password")
                .and_then(Json::as_str)
                .into_iter()
                .collect();
            for d in &o.destinations {
                self.ctx.secrets().register_secret_fields(&d.options, &[]);
                passwords.extend(d.zip.as_ref().and_then(|z| z.password.as_deref()));
            }
            for pw in passwords {
                self.ctx
                    .secrets()
                    .register_secret_field(&Json::String(pw.to_string()));
                // A password written in the YAML is a secret too (validate warns about it).
                if !crate::preflight::is_templated(pw) {
                    self.ctx.secrets().register_values([pw.to_string()]);
                }
            }
        }
        self.save_output_json()
    }

    /// `--output-json`: each result's rows as JSON (secret columns masked) and its Arrow stream,
    /// for `--from-results`.
    fn save_output_json(&mut self) -> Result<(), Fail> {
        if let Some(dir) = &self.opts.output_json {
            let dir = self
                .project
                .root()
                .join(dir)
                .join(&self.report.name)
                .join(self.b.dir_name());
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut index = crate::saved::Index {
                schema_version: crate::saved::SCHEMA_VERSION.into(),
                run_id: self.run_id.clone(),
                results: Vec::new(),
            };
            for result in &self.produced {
                // Secret columns are saved masked: dre-mcp and people read these files.
                let secret = self.secret_columns(&result.query);
                let masked = result.spool.with_extension("masked.arrow");
                let spool = if secret.is_empty() {
                    &result.spool
                } else {
                    crate::secret_columns::masked_copy(&result.spool, &masked, &secret)?;
                    &masked
                };
                crate::rows::write_spool(spool, &dir.join(format!("{}.json", result.name)), result.rows)?;
                // The exact Arrow stream too, for `--from-results`.
                let file = format!("{}.arrows", result.name);
                crate::saved::spool_to_stream(spool, &dir.join(&file))?;
                index.results.push(crate::saved::SavedResult {
                    query: result.query.clone(),
                    name: result.name.clone(),
                    connection: result.connection.clone(),
                    rows: result.rows,
                    sql_sha256: std::fs::read_to_string(
                        self.compiled_dir.join(format!("{}.sha256", result.query)),
                    )
                    .ok()
                    .map(|h| h.trim().to_string()),
                    file,
                    masked_columns: secret,
                });
            }
            crate::saved::write_index(&dir, &index).map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    /// Compare the schema with the last successful run's, before anything is delivered.
    pub(super) fn check_drift(&mut self) -> Result<(), Fail> {
        if !self.stages.drift {
            return Ok(());
        }
        self.drift = self.schema_drift();
        if self.drift.is_empty() {
            return Ok(());
        }
        if self.opts.accept_schema_change {
            self.ui.warn(&format!(
                "  schema changed since the last successful run (accepted): {}",
                self.drift.join("; ")
            ));
            return Ok(());
        }
        Err(format!(
            "schema drift since the last successful run: {}; the output is in {} but was not delivered — pass --accept-schema-change to deliver it and accept the new schema",
            self.drift.join("; "),
            rel(self.project.root(), &self.run_dir).display()
        )
        .into())
    }

    /// Snapshot the schema for the next drift check (not from a run for an earlier instant
    /// than the current one).
    pub(super) fn record_schema(&mut self) {
        if self.stages.drift
            && self.becomes_current
            && let Err(error) = self.write_snapshot()
        {
            self.ui.warn(&format!(
                "delivery completed, but the schema snapshot could not be written: {error}; the next run may compare against an older schema"
            ));
        }
    }

    /// Save the run's query results in its folder, for `--retry-failed`.
    pub(super) fn save_snapshot(&mut self) -> Result<(), Fail> {
        let dir = self.run_dir.join(SNAPSHOT_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut index = crate::saved::Index {
            schema_version: crate::saved::SCHEMA_VERSION.into(),
            run_id: self.run_id.clone(),
            results: Vec::new(),
        };
        for r in &self.produced {
            let file = format!("{}.arrows", safe_name(&r.query));
            crate::saved::spool_to_stream(&r.spool, &dir.join(&file))?;
            index.results.push(crate::saved::SavedResult {
                query: r.query.clone(),
                name: r.name.clone(),
                connection: r.connection.clone(),
                rows: r.rows,
                sql_sha256: std::fs::read_to_string(self.compiled_dir.join(format!("{}.sha256", r.query)))
                    .ok()
                    .map(|h| h.trim().to_string()),
                file,
                // The run folder keeps the real values: a retry delivers what they feed.
                masked_columns: Vec::new(),
            });
        }
        crate::saved::write_index(&dir, &index).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn snapshot(&self) -> Json {
        let sets: Vec<Json> = self
            .produced
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "columns": p.schema.fields().iter().map(|f| json!({
                        "name": f.name(),
                        "type": f.data_type().to_string(),
                        "type_shape": crate::schema::type_shape(f.data_type()),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "result_sets": sets })
    }

    fn schema_drift(&mut self) -> Vec<String> {
        let path = self.schema_dir.join("last_success.json");
        let mut current = self.snapshot();
        let Ok(prev) = std::fs::read_to_string(&path) else {
            crate::schema::redact(&mut current, self.ctx.secrets());
            self.protected_snapshot = Some(current);
            return Vec::new();
        };
        let Ok(mut prev) = serde_json::from_str::<Json>(&prev) else {
            crate::schema::redact(&mut current, self.ctx.secrets());
            self.protected_snapshot = Some(current);
            return Vec::new();
        };
        let original = prev.clone();
        crate::schema::redact(&mut prev, self.ctx.secrets());
        crate::schema::redact(&mut current, self.ctx.secrets());
        if self.ctx.secrets().enabled() {
            crate::schema::align_protection(&mut prev, &mut current);
        }
        if prev != original {
            let protected =
                serde_json::to_string_pretty(&prev).expect("serializing a serde_json::Value cannot fail");
            if let Err(error) = crate::fs::write_atomic(&path, protected.as_bytes()) {
                self.ui.warn(&format!(
                    "can't protect the existing schema snapshot at {}: {error}; continuing without rewriting it",
                    path.display()
                ));
            }
        }
        let drift = crate::schema::drift(&prev, &current);
        self.protected_snapshot = Some(current);
        drift
    }

    fn write_snapshot(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.schema_dir)?;
        let snapshot = self.protected_snapshot.clone().unwrap_or_else(|| {
            let mut snapshot = self.snapshot();
            crate::schema::redact(&mut snapshot, self.ctx.secrets());
            snapshot
        });
        crate::fs::write_atomic(
            &self.schema_dir.join("last_success.json"),
            serde_json::to_string_pretty(&snapshot)?.as_bytes(),
        )
    }

    /// Every file the outputs wrote (`outputs`, as before 0.3), and one record per output
    /// (`output_results`).
    fn output_records(
        &self,
        status: &Status,
    ) -> (Vec<run_results::OutputFile>, Vec<run_results::OutputResult>) {
        let mut outputs: Vec<run_results::OutputFile> = Vec::new();
        let mut output_results: Vec<run_results::OutputResult> = Vec::new();
        for (o, run) in self.b.outputs.iter().zip(&self.outs) {
            let files: Vec<run_results::OutputFileRef> = run
                .files
                .iter()
                .map(|(f, delivered)| run_results::OutputFileRef {
                    path: record_path(self.project, f),
                    size: std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
                    delivered_to: delivered.clone(),
                })
                .collect();
            outputs.extend(files.iter().map(|f| run_results::OutputFile {
                path: f.path.clone(),
                size: f.size,
                delivered_to: f.delivered_to.clone(),
                output: o.name.clone(),
            }));
            let queries: Vec<String> = self
                .b
                .queries
                .iter()
                .filter(|q| q.tab && o.feeds(&q.query))
                .map(|q| q.query.clone())
                .collect();
            let status = match (run.status, status) {
                (Some(s), _) => s,
                (None, Status::Error) => OutputStatus::Failed,
                (None, _) => OutputStatus::Kept,
            }
            .as_str();
            output_results.push(run_results::OutputResult {
                name: o.name.clone(),
                format: o.format.clone(),
                queries,
                status,
                error: run.error.clone(),
                files,
                delivery: run.delivery_note.clone(),
                deliveries: run.deliveries.clone(),
                when: run.when,
                reason: run.reason.clone(),
                no_data: run.no_data.clone(),
                dropped: run.dropped.clone(),
                for_each: run.for_each.clone(),
                message: run.message.clone().map(|(title, text)| run_results::Message {
                    title: self.ctx.secrets().mask_message_title(&title).into_owned(),
                    text: self.ctx.secrets().mask_markdown(&text).into_owned(),
                }),
            });
        }
        (outputs, output_results)
    }

    pub(super) fn write_results(&self, status: &Status, error: Option<&Fail>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.run_dir)?;
        if !self.held.is_empty() {
            crate::release::write_held(&self.run_dir, &self.run_id, &self.held)?;
        }
        let (outputs, output_results) = self.output_records(status);
        // In delivery order: file outputs, then messages.
        let order = (0..self.outs.len())
            .filter(|&i| !self.b.outputs[i].is_message())
            .chain((0..self.outs.len()).filter(|&i| self.b.outputs[i].is_message()));
        let deliveries: Vec<Delivery> = order
            .flat_map(|i| self.outs[i].deliveries.iter().cloned())
            .collect();
        let notes: Vec<&str> = self
            .outs
            .iter()
            .filter_map(|o| o.delivery_note.as_deref())
            .collect();
        let delivery = (!notes.is_empty()).then(|| notes.join("; "));
        let results = RunResults {
            schema_version: run_results::SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            binding: self.b.dir_name().to_string(),
            managed: self.report.managed,
            profile: self.parsed.inherited.clone(),
            connections: self
                .parsed
                .connections()
                .into_iter()
                .map(str::to_string)
                .collect(),
            target: self.target.clone(),
            schedule: self.opts.schedule.clone(),
            schedule_vars: self.schedule_vars.clone(),
            vars: self.rendered_vars.clone(),
            run_date: self.date.to_string(),
            scheduled_at: self.opts.scheduled_at.map(rfc3339),
            timezone: self.calendar.tz.name().to_string(),
            params: self.opts.params(self.date),
            status: *status,
            error: error.map(|e| e.message.clone()),
            error_code: error.map(|e| e.code.clone()),
            error_kind: error.map(|e| e.code.kind()),
            preview: self.stages.preview.is_some(),
            row_limit: self.stages.preview,
            started_at: self.started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            duration_ms: self.started.elapsed().as_millis() as u64,
            result_sets: self
                .produced
                .iter()
                .map(|p| run_results::ResultSet {
                    name: p.name.clone(),
                    query: p.query.clone(),
                    connection: p.connection.clone(),
                    rows: p.rows,
                    columns: p.schema.fields().iter().map(|f| f.name().clone()).collect(),
                })
                .collect(),
            outputs,
            output_results,
            delivery,
            deliveries,
            schema_drift: self.drift.clone(),
            target_path: self.project.target_dir().to_path_buf(),
            settings: self.project.settings().clone(),
            manifest_checksum: self.opts.manifest_checksum.clone(),
            project_errors: self.opts.project_errors.clone(),
            retry_of: self.retry.as_ref().map(|r| r.original.clone()),
            release_of: None,
            released_by: None,
            released: Vec::new(),
        };
        self.store.write_results(&self.run_dir, &results)
    }
}
