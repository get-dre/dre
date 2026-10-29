//! Templates: the Jinja pre-flight checks and the parse pass.

use super::*;

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
        let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
        let mut read = |root: &Path, p: &Path| -> Option<String> {
            if let Some(s) = sources.get(p) {
                return Some(s.clone());
            }
            let s = std::fs::read_to_string(root.join(p)).ok()?;
            sources.insert(p.to_path_buf(), s.clone());
            Some(s)
        };
        let mut checked: BTreeSet<PathBuf> = BTreeSet::new();
        let root = self.root.clone();
        // Macros: syntax, env_var, run.*; their definitions, for var() checks per Binding.
        let mut defs: BTreeMap<String, (PathBuf, preflight::MacroDef)> = BTreeMap::new();
        for m in &project.templates.macros {
            if let Some(src) = read(&root, m) {
                self.check_template_text(m, &src, 0, None, &cli);
                checked.insert(m.clone());
                for d in preflight::macro_defs(&src) {
                    defs.insert(d.name.clone(), (m.clone(), d));
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
                    let Some(src) = read(&root, &q.path) else { continue };
                    let first = checked.insert(q.path.clone());
                    self.check_vars(&q.path, &src, 0, &b.vars, &cli, &ctx);
                    if first {
                        self.check_template_text(&q.path, &src, 0, None, &cli);
                    }
                    called.extend(preflight::called_names(&src));
                    refs.extend(preflight::refs(&src).into_iter().map(|(n, _)| n));
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
                    let Some(src) = read(&root, path) else { continue };
                    self.check_vars(path, &src, 0, &b.vars, &cli, &ctx);
                    if checked.insert(path.clone()) {
                        self.check_template_text(path, &src, 0, None, &cli);
                    }
                    called.extend(preflight::called_names(&src));
                    refs.extend(preflight::refs(&src).into_iter().map(|(n, _)| n));
                }
                // Macros this Binding calls, directly or through other macros.
                let mut seen = BTreeSet::new();
                while let Some(name) = called.pop() {
                    let Some((file, def)) = defs.get(&name) else {
                        continue;
                    };
                    if !seen.insert(name) {
                        continue;
                    }
                    self.check_vars(file, &def.body, def.line_offset, &b.vars, &cli, &ctx);
                    called.extend(preflight::called_names(&def.body));
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
                    let yf = std::fs::read_to_string(root.join(&r.file)).unwrap_or_default();
                    let line = yf.lines().position(|l| l.contains(v.as_str())).map(|i| i + 1);
                    self.check_template_text(&r.file, v, line.map_or(0, |l| l - 1), line, &cli);
                    self.check_vars(&r.file, v, line.map_or(0, |l| l - 1), &b.vars, &cli, &ctx);
                }
            }
        }
    }

    /// Syntax, `env_var()` and `run.*` checks for one template source.
    fn check_template_text(
        &mut self,
        file: &Path,
        src: &str,
        line_offset: usize,
        fixed_line: Option<usize>,
        _cli: &BTreeSet<&str>,
    ) {
        let f = Some(file.to_path_buf());
        if let Err((line, msg)) = preflight::check_syntax(&file.to_string_lossy(), src) {
            let line = fixed_line.or(line.map(|l| l + line_offset));
            self.diags
                .error(Code::JinjaSyntax, f, line, format!("Jinja syntax error: {msg}"));
            return;
        }
        for c in preflight::calls(src)
            .into_iter()
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
        for (name, instead, line) in preflight::removed_names(src) {
            self.diags.error(
                Code::RemovedTemplateName,
                f.clone(),
                Some(fixed_line.unwrap_or(line + line_offset)),
                format!("`{name}` was removed in DRE 0.2: use {instead}"),
            );
        }
        for (r, line) in preflight::unknown_run_refs(src) {
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
        src: &str,
        line_offset: usize,
        vars: &JsonMap<String, Json>,
        cli: &BTreeSet<&str>,
        ctx: &str,
    ) {
        for c in preflight::calls(src)
            .into_iter()
            .filter(|c| c.func == "var" && !c.has_default)
        {
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
