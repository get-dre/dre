//! The format stage: each output's `on_empty` and `when:`, its files written by its format
//! plugin into the run folder, and each message rendered.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use arrow::ipc::reader::FileReader;
use dre_protocol::CAP_MESSAGE;
use serde_json::{Map as JsonMap, Value as Json, json};

use super::compile::render_json;
use super::{
    BindingRun, Fail, OutputStatus, ShownMessage, human_bytes, per_row, record_path, rel, thousands,
};
use crate::codes::Code;
use crate::engine::Level;
use crate::formats::{Files, Format};
use crate::message::QueryResult;
use crate::plugin_host::{Plugin, WriteRequest, find_plugin};
use crate::profiles::Role;
use crate::project::PluginKind;

/// One file of an output: its name and the result sets (by index) it holds.
type FileGroup = (String, Vec<usize>);

impl BindingRun<'_> {
    /// Make every file output whose `on_empty` and `when:` let it, into the run folder; a
    /// per-row output waits for the deliver stage. Returns the file and message outputs, in
    /// the order they deliver.
    pub(super) fn make_files(&mut self) -> Result<(Vec<usize>, Vec<usize>), Fail> {
        let (file_outs, message_outs): (Vec<usize>, Vec<usize>) =
            (0..self.outs.len()).partition(|&i| !self.b.outputs[i].is_message());
        // A per-row output keeps the run's results, so `--retry-failed` redoes rows from them.
        if self.stages.keep_results
            && self
                .b
                .outputs
                .iter()
                .any(|o| o.for_each.as_ref().is_some_and(|f| f.keep_results))
        {
            self.save_snapshot()?;
        }
        // `on_empty` first, for every output: a failure stops the run before anything is written.
        for oi in 0..self.outs.len() {
            self.has_data(oi)?;
        }
        if self.retry.is_some() {
            for oi in 0..self.outs.len() {
                if self.b.outputs[oi].for_each.is_none() {
                    self.skip(oi, "--retry-failed redoes per-row outputs only");
                    self.outs[oi].reason = Some("retry".into());
                }
            }
        }
        for &oi in &file_outs {
            if self.outs[oi].status == Some(OutputStatus::Skipped) || !self.passes_when(oi)? {
                continue;
            }
            // A per-row output is made and delivered row by row, after the drift check.
            if self.b.outputs[oi].for_each.is_some() {
                continue;
            }
            let filename = self.file_names(oi);
            self.format(oi, &filename).map_err(|e| e.or(Code::FormatFailed))?;
        }
        Ok((file_outs, message_outs))
    }

    /// Output `oi`'s queries' results, `rows` capped at `max_rows`.
    pub(super) fn results_for(
        &mut self,
        oi: usize,
        max_rows: u64,
    ) -> Result<Vec<(String, Arc<QueryResult>)>, Fail> {
        let o = &self.b.outputs[oi];
        let mut out = Vec::new();
        for p in self.produced.iter().filter(|p| o.feeds(&p.query)) {
            let r = QueryResult::read(&p.spool, p.rows, max_rows, self.calendar)
                .map_err(|e| format!("can't read the result of `{}`: {e}", p.query))?;
            out.push((p.query.clone(), Arc::new(r)));
        }
        Ok(out)
    }

    /// What output `oi`'s templates read besides the usual context: `results` and `outputs`
    /// (every other named output: `location`, `files` and `status`).
    pub(super) fn template_context(
        &self,
        oi: usize,
        results: &[(String, Arc<QueryResult>)],
    ) -> minijinja::Value {
        use minijinja::Value;
        let results: BTreeMap<String, Value> = results
            .iter()
            .map(|(q, r)| (q.clone(), r.clone().value()))
            .collect();
        let mut outputs: BTreeMap<String, Value> = BTreeMap::new();
        for (j, (o, run)) in self.b.outputs.iter().zip(&self.outs).enumerate() {
            let Some(name) = &o.name else { continue };
            if j == oi {
                continue;
            }
            let locations: Vec<&str> = run
                .deliveries
                .iter()
                .filter_map(|d| d.location.as_deref())
                .collect();
            let files: Vec<String> = run
                .files
                .iter()
                .map(|(f, _)| record_path(self.project, f).to_string_lossy().to_string())
                .collect();
            let status = run.status.map_or("pending", OutputStatus::as_str);
            outputs.insert(
                name.clone(),
                Value::from(minijinja::value::Serde(json!({
                    "location": locations.join(", "),
                    "files": files,
                    "status": status,
                }))),
            );
        }
        Value::from(BTreeMap::from([
            ("results".to_string(), Value::from(results)),
            ("outputs".to_string(), Value::from(outputs)),
        ]))
    }

    /// Apply output `oi`'s `on_empty` (and its queries'). `Ok(false)`: skipped for no data;
    /// an error fails the run (`no-data`).
    fn has_data(&mut self, oi: usize) -> Result<bool, Fail> {
        let o = &self.b.outputs[oi];
        let label = o.label(oi);
        // A per-row output's population is its driver: each row's slices are checked per row.
        let queries: Vec<crate::no_data::Query<'_>> = self
            .produced
            .iter()
            .filter(|p| match &o.for_each {
                Some(f) => p.query == f.query,
                None => o.feeds(&p.query),
            })
            .map(|p| crate::no_data::Query {
                name: &p.query,
                rows: p.rows,
                on_empty: self
                    .b
                    .queries
                    .iter()
                    .find(|q| q.query == p.query)
                    .and_then(|q| q.on_empty),
            })
            .collect();
        let droppable = !(Format::new(&o.format).template_decides_sections() || o.for_each.is_some());
        let d = crate::no_data::decide(o.on_empty, &queries, droppable);
        self.outs[oi].no_data = d.empty.clone();
        for w in &d.warnings {
            self.ui.warn(&format!("  {label}: {w}"));
        }
        if let Some(info) = &d.info {
            self.ui
                .step(Level::Info, "No data", &format!("{label}: {info}"), None);
        }
        if let Some(msg) = d.fail {
            self.outs[oi].reason = Some("no_data".into());
            return Err(Fail::new(Code::NoData, format!("{label}: {msg}")));
        }
        if let Some(why) = d.skip {
            self.outs[oi].reason = Some("no_data".into());
            self.skip(oi, &why);
            return Ok(false);
        }
        self.outs[oi].dropped = d.drop;
        Ok(true)
    }

    /// Evaluate output `oi`'s `when:`. False: the output is skipped (recorded, not a failure).
    fn passes_when(&mut self, oi: usize) -> Result<bool, Fail> {
        if self.b.outputs[oi].when.is_none() {
            return Ok(true);
        }
        let results = self.results_for(oi, crate::message::DEFAULT_MAX_ROWS)?;
        self.when_holds(oi, &results)
    }

    /// Output `oi`'s `when:` over `results` (the same ones its message reads).
    fn when_holds(&mut self, oi: usize, results: &[(String, Arc<QueryResult>)]) -> Result<bool, Fail> {
        let Some(when) = self.b.outputs[oi].when.clone() else {
            return Ok(true);
        };
        let ctx = self.template_context(oi, results);
        let renderer = self.renderer(None)?;
        let label = self.b.outputs[oi].label(oi);
        let src = format!("{{% if {when} %}}true{{% endif %}}");
        let passed = renderer
            .render_with(&self.report.file, &src, ctx, false)
            .map_err(|e| format!("{label}: `when` `{when}`: {}", e.message))?
            == "true";
        self.outs[oi].when = Some(passed);
        if !passed {
            self.skip(oi, &format!("`when` is false ({when})"));
        }
        Ok(passed)
    }

    pub(super) fn skip(&mut self, oi: usize, why: &str) {
        let label = self.b.outputs[oi].label(oi);
        self.outs[oi].status = Some(OutputStatus::Skipped);
        self.outs[oi].delivery_note = Some(format!("skipped: {why}"));
        self.ui
            .step(Level::Info, "Skipped", &format!("{label}: {why}"), None);
    }

    /// Render message output `oi` and write its `.md` file. `Ok(false)`: skipped (`when:` false,
    /// or the text rendered empty).
    pub(super) fn render_message(&mut self, oi: usize) -> Result<bool, Fail> {
        let o = &self.b.outputs[oi];
        let opt = |k: &str| o.options.get(k).and_then(Json::as_str).map(str::to_string);
        let (text_src, file_src, title_src) = (opt("text"), opt("file"), opt("title"));
        let max_rows = o
            .options
            .get("max_rows")
            .and_then(Json::as_u64)
            .unwrap_or(crate::message::DEFAULT_MAX_ROWS);
        let results = self.results_for(oi, max_rows)?;
        if !self.when_holds(oi, &results)? {
            return Ok(false);
        }
        for (q, r) in results.iter().filter(|(_, r)| r.capped()) {
            self.ui.warn(&format!(
                "  results.{q}.rows holds the first {} of {} rows (`max_rows`); aggregate in SQL, or raise `max_rows`",
                thousands(r.rows.len() as u64),
                thousands(r.row_count)
            ));
        }
        let ctx = self.template_context(oi, &results);
        let renderer = self.renderer(None)?;
        let file = self.report.file.clone();
        let text = match (text_src, file_src) {
            (Some(t), _) => renderer
                .render_with(&file, &t, ctx.clone(), true)
                .map_err(|e| e.message)?,
            (None, Some(f)) => {
                let root = self.project.root();
                let path = crate::project::find_template(root, &f)
                    .ok_or_else(|| format!("message file `{f}` doesn't exist"))?;
                let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let shown = rel(root, &path);
                renderer
                    .render_with(&shown, &src, ctx.clone(), true)
                    .map_err(|e| e.to_string())?
            }
            (None, None) => crate::message::default_text(&results, self.locale()),
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            self.skip(oi, "the message is empty");
            return Ok(false);
        }
        let title = match title_src {
            Some(t) => renderer
                .render_with(&file, &t, ctx, false)
                .map_err(|e| e.message)?,
            None => format!("{}: {}", self.report.name, self.date),
        };
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let name = self.file_names(oi);
        let path = self.run_dir.join(&name);
        if let Some(prev) = (0..oi).find(|&j| self.outs[j].files.iter().any(|(f, _)| f == &path)) {
            return Err(format!(
                "it writes {name}, as {} does; give them different `name:`s",
                self.b.outputs[prev].label(prev)
            )
            .into());
        }
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, crate::message::file_body(&title, &text)).map_err(|e| e.to_string())?;
        self.outs[oi].files.push((path, None));
        // Protect preview and metadata copies; report files and deliveries retain their values.
        let shown_title = self.ctx.secrets().mask_message_title(&title);
        let shown_text = self.ctx.secrets().mask_markdown(&text);
        if let Some(n) = self.stages.preview {
            let mut notes = Vec::new();
            {
                notes.push(format!(
                    "numbers come from a sample of at most {} rows per query (--preview)",
                    thousands(n)
                ));
            }
            if let Some(w) = &self.b.outputs[oi].when {
                notes.push(format!("`when` passed ({w})"));
            }
            let length = text.chars().count();
            notes.push(format!("{} characters", thousands(length as u64)));
            notes.extend(self.destination_limits(oi, length));
            self.ui.message(&ShownMessage {
                output: self.b.outputs[oi].name.clone(),
                title: shown_title.into_owned(),
                text: shown_text.into_owned(),
                notes,
            });
        } else {
            self.ui.step(
                Level::Info,
                "Message",
                &crate::message::excerpt(&title, &text, 100, self.ctx.secrets()),
                None,
            );
        }
        self.outs[oi].message = Some((title, text));
        Ok(true)
    }

    /// For `--preview`: how each destination of message output `oi` takes it, and its limit.
    fn destination_limits(&self, oi: usize, length: usize) -> Vec<String> {
        let mut out = Vec::new();
        let Some(pool) = &self.pool else { return out };
        for d in &self.outs[oi].dests {
            let Some(t) = self.project.profiles().target(Role::Destination, &d.profile) else {
                out.push(format!("{}: delivers nowhere on this target", d.profile));
                continue;
            };
            let described = pool.connections.described(Role::Destination, &t.kind);
            let takes = described
                .as_ref()
                .is_some_and(|x| x.capabilities.iter().any(|c| c == CAP_MESSAGE));
            let limit = described.and_then(|x| x.message_limit);
            out.push(match (takes, limit) {
                (false, _) => format!("{} ({}): delivers the .md file", d.profile, t.kind),
                (true, Some(l)) if length as u64 > l => format!(
                    "{} ({}): {} of {} characters: over the limit, so it will be cut short",
                    d.profile,
                    t.kind,
                    thousands(length as u64),
                    thousands(l)
                ),
                (true, Some(l)) => format!(
                    "{} ({}): {} of {} characters",
                    d.profile,
                    t.kind,
                    thousands(length as u64),
                    thousands(l)
                ),
                (true, None) => format!("{} ({}): posts the message", d.profile, t.kind),
            });
        }
        out
    }

    /// The Binding's locale for the number filters.
    pub(super) fn locale(&self) -> crate::numbers::Locale {
        self.b
            .locale
            .as_deref()
            .and_then(|l| crate::numbers::Locale::parse(l).ok())
            .unwrap_or_default()
    }

    /// Apply `--output-path`/`--output-name` to every destination of the first output that has a
    /// path, and return output `oi`'s local file name: `--output-name` (first output only), else
    /// the first destination path's file name, else `<name>.<ext>` (the output's `name:`, else
    /// the report's).
    pub(super) fn file_names(&mut self, oi: usize) -> String {
        let first = oi == 0;
        let (output_path, output_name) = if first {
            (self.opts.output_path.clone(), self.opts.output_name.clone())
        } else {
            (None, None)
        };
        for d in self.outs[oi].dests.iter_mut().filter(|d| d.path.is_some()) {
            if let Some(p) = &output_path {
                d.path = Some(p.clone());
            }
            if let (Some(n), Some(r)) = (&output_name, d.path.as_mut()) {
                *r = match r.rfind(['/', '\\']) {
                    Some(i) => format!("{}{n}", &r[..=i]),
                    None => n.clone(),
                };
            }
        }
        let o = &self.b.outputs[oi];
        let stem = o.name.as_deref().unwrap_or(&self.report.name);
        // A zipping entry's path names the zip, not the files in it.
        let paths = self.outs[oi]
            .dests
            .iter()
            .filter(|d| d.zip.is_none())
            .filter_map(|d| d.path.as_deref());
        Files::of(o).base_name(stem, output_name.as_deref(), paths)
    }

    /// Write output `oi`'s files from the result sets of its queries.
    fn format(&mut self, oi: usize, filename: &str) -> Result<(), Fail> {
        self.not_cancelled()?;
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        let out = &self.b.outputs[oi];
        let Some(groups) = self.file_groups(oi, filename)? else {
            return Ok(());
        };
        let plugin = find_plugin(&self.ctx, self.project, PluginKind::Format, &out.format)
            .map_err(|e| Fail::new(Code::FormatFailed, e.to_string()))?;
        let mut processes = self.processes.lease(&self.ui);
        if !processes.formats.contains_key(&out.format) {
            let process = self
                .ctx
                .host()
                .start(&plugin, processes.log(), Some(self.project.root()))
                .map_err(|e| e.to_string())?;
            processes.formats.insert(out.format.clone(), process);
        }
        let template = match &out.template {
            _ if Format::new(&out.format).is_document() => {
                Some(self.document_payload(out.template.as_ref())?)
            }
            Some(t) => Some(self.template_payload(t)?),
            None => None,
        };
        let options = self.format_options(oi)?;
        for (name, idxs) in groups {
            let request = WriteRequest {
                path: self.run_dir.join(&name).to_string_lossy().into_owned(),
                format: out.format.clone(),
                options: options.clone(),
                result_sets: idxs.iter().map(|&i| self.produced[i].meta()).collect(),
                template: template.clone(),
            };
            let p = processes.formats.get_mut(&out.format).expect("started above");
            if let Err(error) = self.write_file(oi, p.as_mut(), request, &idxs) {
                if let Some(p) = processes.formats.remove(&out.format) {
                    p.close();
                }
                return Err(error);
            }
        }
        Ok(())
    }

    /// Output `oi`'s files, each with the result sets (by index) it holds; `None` when no query
    /// gives it a tab (warned about). Two outputs writing one file is an error.
    fn file_groups(&mut self, oi: usize, filename: &str) -> Result<Option<Vec<FileGroup>>, Fail> {
        let out = &self.b.outputs[oi];
        let mine: Vec<usize> = (0..self.produced.len())
            .filter(|&i| {
                out.feeds(&self.produced[i].query) && !self.outs[oi].dropped.contains(&self.produced[i].query)
            })
            .collect();
        if mine.is_empty() {
            let what = if self.outs.len() > 1 {
                format!("{} gets no tab", out.label(oi))
            } else {
                "no query makes a tab (every query has `tab: false`)".to_string()
            };
            self.ui.warn(&format!("  {what}; no output file was written"));
            return Ok(None);
        }
        let tabs: Vec<&str> = mine.iter().map(|&i| self.produced[i].name.as_str()).collect();
        let groups: Vec<FileGroup> = Files::of(out)
            .plan(filename, &tabs)?
            .into_iter()
            .map(|(name, of)| (name, of.into_iter().map(|t| mine[t]).collect()))
            .collect();
        for (name, _) in &groups {
            let path = self.run_dir.join(name);
            if let Some(prev) = (0..oi).find(|&j| self.outs[j].files.iter().any(|(f, _)| f == &path)) {
                return Err(format!(
                    "{} and {} both write {name}; give them different `name:`s or destination paths",
                    self.b.outputs[prev].label(prev),
                    out.label(oi)
                )
                .into());
            }
        }
        Ok(Some(groups))
    }

    /// Output `oi`'s format options; any holding Jinja (a pdf `password:`) rendered once.
    fn format_options(&mut self, oi: usize) -> Result<JsonMap<String, Json>, Fail> {
        let out = &self.b.outputs[oi];
        if !out.options.values().any(per_row::has_template) {
            return Ok(out.options.clone());
        }
        let renderer = self.renderer(None)?;
        let mut m = JsonMap::new();
        for (k, v) in &out.options {
            let v = if per_row::has_template(v) {
                render_json(&renderer, &self.report.file, v).map_err(|e| {
                    Fail::new(Code::RenderFailed, format!("{} option `{k}`: {e}", out.label(oi)))
                })?
            } else {
                v.clone()
            };
            m.insert(k.clone(), v);
        }
        Ok(m)
    }

    /// Write one file of output `oi` through format plugin `p`, streaming the result sets
    /// `idxs` from their spools batch by batch, so memory stays flat.
    fn write_file(
        &mut self,
        oi: usize,
        p: &mut dyn Plugin,
        request: WriteRequest,
        idxs: &[usize],
    ) -> Result<(), Fail> {
        let t = Instant::now();
        let format = request.format.clone();
        p.write(request).map_err(|e| e.to_string())?;
        for &i in idxs {
            let r = &self.produced[i];
            let reader = FileReader::try_new(File::open(&r.spool).map_err(|e| e.to_string())?, None)
                .map_err(|e| e.to_string())?;
            // A spool that can't be read ends the stream, and fails the output.
            let mut unreadable = None;
            let batches = reader.map_while(|b| b.map_err(|e| unreadable = Some(e.to_string())).ok());
            crate::plugin_host::write_result_set(p, &r.schema, batches)
                .map_err(|e| format!("{format} format: {e}"))?;
            if let Some(e) = unreadable {
                return Err(e.into());
            }
        }
        let crate::plugin_host::Written { files, warnings } = p
            .finish()
            .map_err(|e| Fail::from_plugin(&e, format!("{format} format: {e}")))?;
        for w in warnings {
            self.ui.warn(&format!("  {format} format: {w}"));
        }
        for f in files {
            let f = PathBuf::from(f);
            let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
            let shown = rel(self.project.root(), &f);
            self.ui.step(
                Level::Debug,
                "Wrote",
                &format!("{} ({})", shown.display(), human_bytes(size)),
                Some(t.elapsed()),
            );
            self.outs[oi].files.push((f, None));
        }
        Ok(())
    }

    pub(super) fn template_payload(&mut self, t: &crate::project::Template) -> Result<Json, Fail> {
        let file = crate::project::resolve_template(self.project.root(), &self.report.file, &t.file)?;
        let renderer = self.renderer(None)?;
        let mut values = JsonMap::new();
        for b in &t.bindings {
            if let (Some(cell), Some(v)) = (&b.cell, &b.value) {
                let rendered = renderer
                    .render(&self.report.file, v)
                    .map_err(|e| format!("template value: {e}"))?;
                values.insert(format!("{}!{}", b.sheet, cell), Json::String(rendered));
            }
        }
        Ok(json!({"file": file.to_string_lossy(), "bindings": t.bindings, "values": values}))
    }

    /// An html or pdf output's `template`: the template file (if any) and the context it's rendered with
    /// ([`crate::document::Context`]).
    pub(super) fn document_payload(&mut self, t: Option<&crate::project::Template>) -> Result<Json, Fail> {
        let file = match t {
            Some(t) => Some(crate::project::resolve_template(
                self.project.root(),
                &self.report.file,
                &t.file,
            )?),
            None => None,
        };
        let run = self.run_context();
        let ctx = crate::document::Context::new(
            file.as_deref(),
            self.project.root(),
            &run,
            self.vars.clone(),
            self.opts.vars.clone(),
        );
        Ok(serde_json::to_value(ctx).map_err(|e| e.to_string())?)
    }
}
