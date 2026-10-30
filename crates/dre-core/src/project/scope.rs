//! Report-local errors found during discovery do not stop an unrelated selection.

use super::*;

impl Loader {
    pub(super) fn record_other_error(&mut self, name: &str, diagnostics: &Diagnostics) {
        if let Some(diagnostic) = diagnostics
            .sorted()
            .into_iter()
            .find(|d| d.severity == crate::Severity::Error)
            && let Some(path) = &diagnostic.file
        {
            self.other_errors
                .entry(name.into())
                .or_insert_with(|| crate::run_results::ProjectError {
                    path: path.clone(),
                    code: diagnostic.code,
                    message: diagnostic.message.clone(),
                });
        }
    }

    pub(super) fn partition_report_errors(&mut self, project: &mut Project) {
        let Some(scope) = &self.scope else { return };
        let mut owners = self.report_owners.clone();
        for report in project.reports() {
            for file in report_files(project, report) {
                owners.entry(file).or_default().insert(report.name.clone());
            }
        }
        // A structural discovery error explains a broken definition before any secondary
        // template error caused by treating its leftover SQL as an unmanaged report.
        let mut other = BTreeMap::new();
        let mut kept = Diagnostics::default();
        let mut diagnostics = self.diags.sorted();
        diagnostics.sort_by_key(|diagnostic| {
            !matches!(
                diagnostic
                    .file
                    .as_ref()
                    .and_then(|file| file.extension())
                    .and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            )
        });
        for diagnostic in diagnostics {
            let Some(file) = &diagnostic.file else {
                kept.push(diagnostic.clone());
                continue;
            };
            let names = diagnostic
                .plugin
                .as_ref()
                .map(|(kind, name)| {
                    project
                        .reports()
                        .iter()
                        .filter(|r| uses_plugin(project, r, *kind, name))
                        .map(|r| r.name.clone())
                        .collect()
                })
                .or_else(|| owners.get(file).cloned())
                .unwrap_or_else(|| {
                    // An unreadable report YAML cannot produce a Fragment. Its directory is the
                    // report's default identity; an explicit selection that vanished still fails.
                    if file.starts_with(REPORTS_DIR) {
                        let name =
                            if matches!(file.extension().and_then(|e| e.to_str()), Some("yml" | "yaml")) {
                                file.parent().and_then(Path::file_name)
                            } else {
                                file.file_stem()
                            };
                        name.map(|n| BTreeSet::from([n.to_string_lossy().into_owned()]))
                            .unwrap_or_default()
                    } else {
                        BTreeSet::new()
                    }
                });
            if file == Path::new(PROJECT_FILE)
                || self.global_files.contains(file)
                || names.is_empty()
                || names.iter().any(|n| scope.contains(n))
            {
                kept.push(diagnostic.clone());
            } else if diagnostic.severity == crate::Severity::Error {
                for name in names {
                    other.entry(name.clone()).or_insert_with(|| crate::run_results::ProjectError {
                        path: if diagnostic.plugin.is_some() {
                            project.report(&name).map(|r| r.file.clone()).unwrap_or_else(|| file.clone())
                        } else { file.clone() },
                        code: diagnostic.code,
                        message: if let Some((kind, plugin)) = &diagnostic.plugin {
                            format!("report `{name}` uses {kind} plugin `{plugin}`, but no declared plugin package provides it; add its package under `plugins:` in dependencies.yml, then run `dre deps`")
                        } else { diagnostic.message.clone() },
                    });
                }
            }
        }
        for (name, error) in &self.other_errors {
            other.entry(name.clone()).or_insert_with(|| error.clone());
        }
        for (name, errors) in &project.parse_errors {
            if !scope.contains(name)
                && let Some(error) = errors.first()
                && let Some(path) = &error.file
            {
                other
                    .entry(name.clone())
                    .or_insert_with(|| crate::run_results::ProjectError {
                        path: path.clone(),
                        code: error.code,
                        message: error.message.clone(),
                    });
            }
        }
        self.diags = kept;
        project.project_errors = other.into_values().collect();
    }
}

/// Defining fragments, queries, output templates and every transitive SQL ref are owned by
/// each report that uses them. A selected owner keeps shared-file failures fatal.
fn report_files(project: &Project, report: &Report) -> BTreeSet<PathBuf> {
    let mut files = BTreeSet::from([report.file.clone()]);
    let mut pending: Vec<PathBuf> = report
        .queries
        .iter()
        .chain(report.bindings.iter().flat_map(|b| &b.queries))
        .map(|q| q.path.clone())
        .collect();
    while let Some(path) = pending.pop() {
        if !files.insert(path.clone()) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(project.root().join(&path)) {
            for name in crate::preflight::Scan::new(&text).refs() {
                if let Some(path) = project.templates.sql.get(name) {
                    pending.push(path.clone());
                }
            }
        }
    }
    for output in report.bindings.iter().flat_map(|b| &b.outputs) {
        let template = output.template.as_ref().map(|t| t.file.as_str());
        let message = output
            .is_message()
            .then(|| output.options.get("file").and_then(Json::as_str))
            .flatten();
        for file in template.into_iter().chain(message) {
            if let Ok(path) = resolve_template(project.root(), &report.file, file) {
                files.insert(crate::slash(path.strip_prefix(project.root()).unwrap_or(&path)));
            }
        }
    }
    files
}

impl Project {
    /// Later run checks and plugin options see only the reports the loader selected.
    pub(crate) fn retain_loaded_selection(&mut self) {
        if let Some(scope) = &self.loaded_selection {
            self.reports.retain(|r| scope.contains(&r.name));
            self.parse_errors.retain(|name, _| scope.contains(name));
        }
    }
}

impl Usage {
    pub(super) fn for_selection(project: &Project, scope: &Option<BTreeSet<String>>) -> Self {
        let mut used = Self::default();
        if let Some(profile) = project.default_profile() {
            used.connection(profile, Some(PROJECT_FILE.into()), project.default_profile_line);
        }
        for report in project.reports().iter().filter(|r| in_scope(scope, &r.name)) {
            let file = Some(report.file.clone());
            for binding in &report.bindings {
                if let Some(parsed) = &binding.parsed {
                    if let Some(profile) = &parsed.inherited {
                        used.connection(profile, file.clone(), None);
                    }
                    for query in &parsed.queries {
                        if let Some(profile) = &query.connection {
                            used.connection(profile, file.clone(), None);
                        }
                    }
                    for destination in parsed.destinations.values().flatten() {
                        used.destination(destination, file.clone(), None);
                    }
                } else {
                    if let Some(profile) = &binding.profile {
                        used.connection(profile, file.clone(), None);
                    }
                    for query in &binding.queries {
                        if let Some(profile) = &query.profile {
                            used.connection(profile, file.clone(), None);
                        }
                    }
                    for destination in binding.destinations() {
                        used.destination(&destination.profile, file.clone(), None);
                    }
                }
                for output in &binding.outputs {
                    used.format(&output.format, &format!("report `{}`", report.name), &report.file);
                }
            }
        }
        used
    }
}

/// Source selectors read the parse pass's source dependencies, so they cannot scope that pass.
pub(super) fn needs_parse(selection: Option<&str>) -> bool {
    selection.is_some_and(|s| {
        s.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .any(|term| term.starts_with("source:"))
    })
}

/// Read only the target metadata here. The schedule loader still validates the whole file.
pub(super) fn schedule_selection(files: &[Rc<YamlFile>], name: Option<&str>) -> Option<String> {
    let name = name?;
    files
        .iter()
        .flat_map(|file| file.value.as_array().into_iter().flatten())
        .find(|entry| entry.get("name").and_then(Json::as_str) == Some(name))
        .and_then(|entry| entry.get("report").or_else(|| entry.get("select")))
        .and_then(Json::as_str)
        .map(str::to_string)
}

pub(super) fn schedules_need_parse(files: &[Rc<YamlFile>]) -> bool {
    files
        .iter()
        .flat_map(|file| file.value.as_array().into_iter().flatten())
        .any(|entry| needs_parse(entry.get("select").and_then(Json::as_str)))
}

fn uses_plugin(project: &Project, report: &Report, kind: PluginKind, name: &str) -> bool {
    let profile_uses = |role, profile: &str| {
        project
            .profiles
            .get(role, profile)
            .is_some_and(|p| p.targets.values().any(|target| target.kind == name))
    };
    report.bindings.iter().any(|binding| match kind {
        PluginKind::Format => binding.outputs.iter().any(|o| o.format == name),
        PluginKind::Source => {
            if let Some(parsed) = &binding.parsed {
                parsed
                    .inherited
                    .iter()
                    .chain(parsed.queries.iter().filter_map(|q| q.connection.as_ref()))
                    .any(|p| profile_uses(Role::Connection, p))
            } else {
                binding
                    .profile
                    .iter()
                    .chain(binding.queries.iter().filter_map(|q| q.profile.as_ref()))
                    .any(|p| profile_uses(Role::Connection, p))
            }
        }
        PluginKind::Destination => {
            if let Some(parsed) = &binding.parsed {
                parsed
                    .destinations
                    .values()
                    .flatten()
                    .any(|p| profile_uses(Role::Destination, p))
            } else {
                binding
                    .destinations()
                    .any(|d| profile_uses(Role::Destination, &d.profile))
            }
        }
    })
}

/// Selection metadata survives malformed YAML and duplicate or incomplete report definitions.
/// These identities only augment a temporary selector catalog, never the executable Project.
pub(super) struct ReportIdentity {
    pub name: String,
    pub file: PathBuf,
    pub folder: Vec<String>,
    pub tags: Vec<String>,
}

impl Loader {
    pub(super) fn record_unreadable_report(&mut self, file: &Path) {
        if !file.starts_with(REPORTS_DIR) {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(self.root.join(file)) {
            let shared = text
                .lines()
                .filter_map(|line| line.split_once(':'))
                .map(|(key, _)| key.trim().trim_matches(['\'', '"']))
                .any(|key| matches!(key, "sources" | "plugins" | "packages"));
            if shared {
                self.global_files.insert(file.to_path_buf());
            }
        }
        let folder = folder_segments(file.parent().unwrap_or(Path::new("")));
        let name = folder
            .last()
            .cloned()
            .or_else(|| file.file_stem().map(|s| s.to_string_lossy().into_owned()));
        if let Some(name) = name {
            self.report_owners
                .entry(file.to_path_buf())
                .or_default()
                .insert(name.clone());
            self.report_candidates.push(ReportIdentity {
                name,
                file: file.to_path_buf(),
                folder,
                tags: Vec::new(),
            });
        }
    }

    pub(super) fn selection_catalog(&self, project: &Project) -> Project {
        let mut catalog = project.clone();
        for candidate in &self.report_candidates {
            if project.report(&candidate.name).is_some() {
                continue;
            }
            catalog.reports.push(Report {
                name: candidate.name.clone(),
                file: candidate.file.clone(),
                folder: candidate.folder.clone(),
                tags: candidate.tags.clone(),
                managed: true,
                description: None,
                meta: JsonMap::new(),
                meta_layers: Vec::new(),
                var_schema: BTreeMap::new(),
                queries: Vec::new(),
                default_set: None,
                timezone: None,
                has_sets: false,
                bindings: Vec::new(),
                base: Binding::default(),
            });
        }
        catalog
    }
}

impl Project {
    /// Installed and locked provider metadata usually resolves this without the registry. Only
    /// unknown providers for an unresolved selected plugin need registry discovery.
    pub(crate) fn retain_needed_plugins(
        &mut self,
        ctx: &crate::Context,
        install: bool,
    ) -> Result<(), String> {
        if self.loaded_selection.is_none() {
            return Ok(());
        }
        let lock = crate::lock::Lock::load(self.root())?;
        let needed: Vec<_> = self.plugin_uses.iter().map(|u| u.plugin.clone()).collect();
        let prober = crate::plugins::Prober::default();
        let mut keep = BTreeSet::new();
        let mut provided = Vec::new();
        let mut unknown = Vec::new();
        for requirement in &self.plugins {
            let installed = crate::plugins::installed(self.root(), &lock, requirement, ctx.env(), &prober);
            let mut ids: Vec<_> = installed
                .unwrap_or_default()
                .into_iter()
                .flat_map(|p| p.provides)
                .collect();
            if ids.is_empty()
                && let Some(pin) = lock.get(&requirement.name)
                && pin.from == requirement.source.lock_key()
            {
                ids.extend(pin.provides.clone());
            }
            if ids.is_empty() {
                unknown.push(requirement);
            } else if ids.iter().any(|id| needed.contains(id)) {
                keep.insert(requirement.name.clone());
                provided.extend(ids);
            }
        }
        let unresolved: Vec<_> = needed.iter().filter(|id| !provided.contains(id)).collect();
        let mut indexes = BTreeMap::new();
        if !unresolved.is_empty() {
            for requirement in unknown {
                let direct = unresolved.iter().any(|id| id.name == requirement.name);
                let relevant = if direct || !install {
                    true
                } else {
                    let key = format!("{:?}", requirement.source);
                    if !indexes.contains_key(&key) {
                        indexes.insert(
                            key.clone(),
                            crate::manager::Index::for_source(ctx, &requirement.source, &requirement.name),
                        );
                    }
                    match &indexes[&key] {
                        Ok(index) => index.plugins.iter().any(|package| {
                            package.name == requirement.name
                                && (package.provides.is_empty()
                                    || unresolved.iter().any(|id| package.provides.contains(id)))
                        }),
                        // Unknown metadata may contain the selected plugin; its normal install
                        // path reports the concrete error instead of silently omitting it.
                        Err(_) => true,
                    }
                };
                if relevant {
                    keep.insert(requirement.name.clone());
                }
            }
        }
        self.plugins
            .retain(|requirement| keep.contains(&requirement.name));
        Ok(())
    }
}
