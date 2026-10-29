//! Templates: the Jinja pre-flight checks and the parse pass.

use super::*;
use crate::preflight::Scan;

impl Loader {
    /// Render every Binding without a database ([`crate::parse`]): each query's connection and
    /// sources, for selection, the manifest, `dre ls` and validate. Problems go to diagnostics,
    /// except for files that already have an error (the same cause, said better).
    pub(super) fn parse_pass(&mut self, project: &mut Project, used: &mut Usage) {
        let inputs = crate::parse::Inputs {
            target: project.target_name.clone(),
            cli_vars: self.opts.vars.clone(),
            date: self.opts.date,
            scheduled_at: self.opts.scheduled_at,
            timezone: self.opts.timezone.clone(),
            schedule: None,
            started_at: None,
            process: self.ctx.clone(),
        };
        project.inputs = inputs.clone();
        let failed: BTreeSet<PathBuf> = self
            .diags
            .iter()
            .filter(|d| d.severity == crate::Severity::Error)
            .filter_map(|d| d.file.clone())
            .collect();
        let mut results = Vec::new();
        for (ri, r) in project.reports.iter().enumerate() {
            for (bi, b) in r.bindings.iter().enumerate() {
                let parsed = crate::parse::binding(project, r, b, &b.vars, &inputs);
                results.push((ri, bi, parsed));
            }
        }
        for (ri, bi, parsed) in results {
            for p in &parsed.errors {
                if failed.contains(&p.file) {
                    continue;
                }
                // A template that doesn't render may depend on this run's `--var`s: it makes
                // its own report invalid (and fails when that report runs or compiles), but
                // doesn't stop the rest of the project.
                if p.code == crate::parse::PARSE_FAILED {
                    let msg = crate::Diagnostic {
                        severity: crate::Severity::Error,
                        code: p.code,
                        message: p.message.clone(),
                        file: Some(p.file.clone()),
                        line: p.line,
                        plugin: None,
                    };
                    let errs = project
                        .parse_errors
                        .entry(project.reports[ri].name.clone())
                        .or_default();
                    if !errs.contains(&msg) {
                        errs.push(msg);
                    }
                    continue;
                }
                self.diags
                    .error(p.code, Some(p.file.clone()), p.line, p.message.clone());
            }
            for p in &parsed.warnings {
                self.diags
                    .warning(p.code, Some(p.file.clone()), p.line, p.message.clone());
            }
            let file = Some(project.reports[ri].file.clone());
            if let Some(i) = &parsed.inherited {
                used.connection(i, file.clone(), None);
            }
            for q in &parsed.queries {
                if let Some(c) = &q.connection {
                    used.connection(c, file.clone(), None);
                }
            }
            for d in parsed.destinations.values().flatten() {
                used.destination(d, file.clone(), None);
            }
            project.reports[ri].bindings[bi].parsed = Some(std::sync::Arc::new(parsed));
        }
    }

    pub(super) fn preflight(&mut self, project: &Project, _used: &Usage) {
        let cli_owned: Vec<String> = self.opts.vars.keys().cloned().collect();
        let cli: BTreeSet<&str> = cli_owned.iter().map(String::as_str).collect();
        // Each file is read and scanned once, however many Bindings use it.
        let mut sources: BTreeMap<PathBuf, Option<Rc<(String, Scan)>>> = BTreeMap::new();
        let root = self.root.clone();
        let mut read = |p: &Path| -> Option<Rc<(String, Scan)>> {
            sources
                .entry(p.to_path_buf())
                .or_insert_with(|| {
                    let src = std::fs::read_to_string(root.join(p)).ok()?;
                    let scan = Scan::new(&src);
                    Some(Rc::new((src, scan)))
                })
                .clone()
        };
        let mut checked: BTreeSet<PathBuf> = BTreeSet::new();
        // Macros: syntax, env_var, run.*; their definitions, for var() checks per Binding.
        let mut defs: BTreeMap<String, (PathBuf, Rc<(String, Scan)>, usize)> = BTreeMap::new();
        for m in &project.templates.macros {
            if let Some(file) = read(m) {
                self.check_template_text(m, &file.0, &file.1, 0, None);
                checked.insert(m.clone());
                for (i, d) in file.1.macros().iter().enumerate() {
                    defs.insert(d.name.clone(), (m.clone(), file.clone(), i));
                }
            }
        }
        for r in &project.reports {
            for b in &r.bindings {
                for error in crate::variables::check_given_values(&r.var_schema, &b.vars, &self.opts.vars) {
                    self.diags
                        .error(Code::InvalidField, Some(r.file.clone()), None, error);
                }
                let ctx = match &b.set {
                    Some(s) => format!("report `{}`, Set `{s}`", r.name),
                    None => format!("report `{}`", r.name),
                };
                let mut called: Vec<String> = Vec::new();
                let mut refs: Vec<String> = Vec::new();
                for q in &b.queries {
                    let Some(file) = read(&q.path) else { continue };
                    let first = checked.insert(q.path.clone());
                    self.check_vars(&q.path, &file.1, 0, &b.vars, &cli, &ctx);
                    if first {
                        self.check_template_text(&q.path, &file.0, &file.1, 0, None);
                    }
                    called.extend(file.1.called_names().iter().cloned());
                    refs.extend(file.1.refs().map(str::to_string));
                }
                // SQL this Binding pulls in through ref(), checked in the Binding's context.
                let mut seen_refs = BTreeSet::new();
                while let Some(name) = refs.pop() {
                    let Some(path) = project.templates.sql.get(&name) else {
                        continue;
                    };
                    if !seen_refs.insert(name) {
                        continue;
                    }
                    let Some(file) = read(path) else { continue };
                    self.check_vars(path, &file.1, 0, &b.vars, &cli, &ctx);
                    if checked.insert(path.clone()) {
                        self.check_template_text(path, &file.0, &file.1, 0, None);
                    }
                    called.extend(file.1.called_names().iter().cloned());
                    refs.extend(file.1.refs().map(str::to_string));
                }
                // Macros this Binding calls, directly or through other macros.
                let mut seen = BTreeSet::new();
                while let Some(name) = called.pop() {
                    let Some((path, file, i)) = defs.get(&name) else {
                        continue;
                    };
                    if !seen.insert(name) {
                        continue;
                    }
                    let body = &file.1.macros()[*i].body;
                    self.check_vars(path, body, 0, &b.vars, &cli, &ctx);
                    called.extend(body.called_names().iter().cloned());
                }
                // Templated output values render with the same context.
                let mut values: Vec<String> = Vec::new();
                for d in b.destinations() {
                    values.extend(d.path.clone());
                    values.extend(d.options.values().flat_map(json_strings));
                }
                for t in b.outputs.iter().filter_map(|o| o.template.as_ref()) {
                    values.extend(t.bindings.iter().filter_map(|tb| tb.value.clone()));
                }
                for v in values.iter().filter(|v| preflight::is_templated(v)) {
                    let yaml = read(&r.file);
                    let line = yaml
                        .as_ref()
                        .and_then(|y| y.0.lines().position(|l| l.contains(v.as_str())))
                        .map(|i| i + 1);
                    let offset = line.map_or(0, |l| l - 1);
                    let scan = Scan::new(v);
                    self.check_template_text(&r.file, v, &scan, offset, line);
                    self.check_vars(&r.file, &scan, offset, &b.vars, &cli, &ctx);
                }
            }
        }
    }

    /// Syntax, `env_var()` and `run.*` checks for one template source and its scan.
    fn check_template_text(
        &mut self,
        file: &Path,
        src: &str,
        scan: &Scan,
        line_offset: usize,
        fixed_line: Option<usize>,
    ) {
        let f = Some(file.to_path_buf());
        if let Err((line, msg)) = preflight::check_syntax(&file.to_string_lossy(), src) {
            let line = fixed_line.or(line.map(|l| l + line_offset));
            self.diags
                .error(Code::JinjaSyntax, f, line, format!("Jinja syntax error: {msg}"));
            return;
        }
        for c in scan
            .calls()
            .iter()
            .filter(|c| c.func == "env_var" && !c.has_default)
        {
            if self.ctx.env().var(&c.name).is_none() {
                self.diags.error(
                    Code::UnsetEnvVar,
                    f.clone(),
                    Some(fixed_line.unwrap_or(c.line + line_offset)),
                    format!(
                        "`env_var('{}')`: environment variable `{}` is not set and no default is given",
                        c.name, c.name
                    ),
                );
            }
        }
        for (name, instead, line) in scan.removed_names() {
            self.diags.error(
                Code::RemovedTemplateName,
                f.clone(),
                Some(fixed_line.unwrap_or(line + line_offset)),
                format!("`{name}` was removed in DRE 0.2: use {instead}"),
            );
        }
        for (r, line) in scan.unknown_run_refs() {
            self.diags.error(
                Code::UnknownRunAttribute,
                f.clone(),
                Some(fixed_line.unwrap_or(line + line_offset)),
                format!(
                    "`{r}` isn't part of the run context; known: run.report, run.set, run.target, run.schedule, run.date (a date: .prev_month, .month_start, .yyyymmdd, ...), run.now, run.scheduled_at, run.timezone, run.date_format(...)"
                ),
            );
        }
    }

    fn check_vars(
        &mut self,
        file: &Path,
        scan: &Scan,
        line_offset: usize,
        vars: &JsonMap<String, Json>,
        cli: &BTreeSet<&str>,
        ctx: &str,
    ) {
        for c in scan.calls().iter().filter(|c| c.func == "var" && !c.has_default) {
            if !vars.contains_key(&c.name) && !cli.contains(c.name.as_str()) {
                self.diags.error(
                    Code::UnresolvedVar,
                    Some(file.to_path_buf()),
                    Some(c.line + line_offset),
                    format!(
                        "`var('{}')` has no value for {ctx} (checked --var, Set, report, folder and project vars) and no default",
                        c.name
                    ),
                );
            }
        }
    }
}
