//! Discovery: walking the project's files, classifying each YAML file by its shape, and indexing `.sql` files.

use super::*;

impl Loader {
    pub(super) fn discover(&mut self) -> Discovered {
        let mut d = Discovered {
            yaml: Vec::new(),
            sql: Vec::new(),
            macros: Vec::new(),
            lookups: Vec::new(),
            folders: Vec::new(),
            sources: Vec::new(),
        };
        let target_inside = self.target_inside.clone();
        let root = self.root.clone();
        let walker = walkdir::WalkDir::new(&self.root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                // Generated or installed, never part of the project's own sources.
                e.depth() == 0
                    || !(name.starts_with('.')
                        || target_inside
                            .as_deref()
                            .is_some_and(|t| e.path().strip_prefix(&root).is_ok_and(|r| r == t))
                        || (e.depth() == 1 && [TARGET_DIR, DEPS_DIR, LOGS_DIR].contains(&name.as_ref())))
            });
        for e in walker.filter_map(Result::ok) {
            let rel = self.rel(e.path());
            let in_reports = rel.starts_with(REPORTS_DIR);
            if e.file_type().is_dir() {
                if in_reports && rel != Path::new(REPORTS_DIR) {
                    d.folders.push(folder_segments(&rel));
                }
                continue;
            }
            let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("");
            let yaml = matches!(ext, "yml" | "yaml") && rel != Path::new(crate::profiles::PROFILES_FILE);
            if yaml || in_reports || rel.starts_with(MACROS_DIR) || rel.starts_with(LOOKUPS_DIR) {
                d.sources.push(rel.clone());
            }
            if rel.starts_with(LOOKUPS_DIR) {
                d.lookups.push(rel);
                continue;
            }
            match ext {
                // A profiles.yml at the root holds connections, not project config.
                "yml" | "yaml"
                    if rel != Path::new(PROJECT_FILE) && rel != Path::new(crate::profiles::PROFILES_FILE) =>
                {
                    d.yaml.push(e.path().to_path_buf())
                }
                "sql" if rel.starts_with(MACROS_DIR) => d.macros.push(rel),
                "sql" if in_reports => d.sql.push(rel),
                _ => {}
            }
        }
        d
    }

    pub(super) fn classify(
        &mut self,
        yf: &Rc<YamlFile>,
        fragments: &mut Vec<Fragment>,
        plugins: &mut Vec<(Rc<YamlFile>, Mapping)>,
        sets: &mut Vec<Rc<YamlFile>>,
        schedules: &mut Vec<Rc<YamlFile>>,
        timings: &mut Vec<Rc<YamlFile>>,
    ) {
        let in_reports = yf.display.starts_with(REPORTS_DIR);
        let timings_file = yf.display.file_name() == Some(std::ffi::OsStr::new(TIMINGS_FILE));
        match &yf.value {
            Value::Null => {}
            Value::Array(items)
                if !items.is_empty()
                    && items
                        .iter()
                        .all(|i| ["name", "select", "report"].iter().any(|k| i.get(k).is_some())) =>
            {
                schedules.push(yf.clone());
            }
            Value::Object(m) => {
                let pl = pick(&yf.value, PLUGIN_KEYS);
                if !pl.is_empty() {
                    plugins.push((yf.clone(), pl));
                }
                // `packages:` belongs to the root dependency files, read by `packages::declared`.
                let dependency_file = packages::DEPENDENCY_FILES
                    .iter()
                    .any(|f| yf.display == Path::new(f));
                if !dependency_file && m.contains_key("packages") {
                    self.diags.error(
                        Code::MisplacedPackages,
                        Some(yf.display.clone()),
                        yf.line_of("packages", None),
                        "`packages:` goes in dependencies.yml or packages.yml at the project root",
                    );
                }
                if dependency_file {
                    for k in m.keys().map(String::as_str) {
                        if OLD_PLUGIN_KEYS.contains(&k) {
                            self.old_plugin_key(yf, k);
                        }
                    }
                }
                let has_sources = m.contains_key(SOURCES_KEY);
                let rest: Mapping = m
                    .iter()
                    .filter(|(k, _)| !is_one_of(k, PLUGIN_KEYS) && k.as_str() != "packages")
                    .filter(|(k, _)| k.as_str() != SOURCES_KEY)
                    // dbt's `version: 2` at the top of a sources file.
                    .filter(|(k, _)| !(has_sources && k.as_str() == "version"))
                    .filter(|(k, _)| !(dependency_file && is_one_of(k, OLD_PLUGIN_KEYS)))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if rest.is_empty() {
                    return;
                }
                let looks_like_report = rest.contains_key("queries") || rest.contains_key("name");
                if !in_reports && (timings_file || is_timing_registry(&rest)) {
                    timings.push(yf.clone());
                } else if in_reports || looks_like_report {
                    self.report_fragment(yf.clone(), rest, fragments);
                } else if is_set_registry(&rest) {
                    sets.push(yf.clone());
                } else {
                    self.diags.warning(
                        Code::UnrecognizedYaml,
                        Some(yf.display.clone()),
                        None,
                        "not recognised as report, Set, plugin or schedule config; ignored",
                    );
                }
            }
            _ => self.diags.warning(
                Code::UnrecognizedYaml,
                Some(yf.display.clone()),
                None,
                "not recognised as report, Set, plugin or schedule config; ignored",
            ),
        }
    }

    fn report_fragment(&mut self, yf: Rc<YamlFile>, map: Mapping, out: &mut Vec<Fragment>) {
        for k in map.keys().map(String::as_str) {
            if !REPORT_KEYS.contains(&k) {
                self.diags.error(
                    Code::UnknownKey,
                    Some(yf.display.clone()),
                    key_line(&yf, k),
                    format!("unknown report key `{k}`"),
                );
            }
        }
        let typed = match de::from_node::<Loose<ReportFile>>(&yf.node) {
            Ok(Loose::Ok(r)) => r,
            Ok(Loose::Bad(_)) => return,
            Err(e) => {
                self.diags
                    .error(Code::InvalidReport, Some(yf.display.clone()), e.line, e.message);
                return;
            }
        };
        let folder = folder_segments(yf.display.parent().unwrap_or(Path::new("")));
        let (name, explicit_name) = match &typed.name {
            Some(de::Located {
                value: Loose::Ok(s), ..
            }) if !s.is_empty() => (s.clone(), true),
            Some(n) => {
                self.diags.error(
                    Code::InvalidField,
                    Some(yf.display.clone()),
                    n.line(),
                    "`name` must be a non-empty string",
                );
                return;
            }
            None => match folder.last() {
                Some(f) => (f.clone(), false),
                None => {
                    self.diags.error(
                        Code::MissingField,
                        Some(yf.display.clone()),
                        None,
                        "a report outside a report folder needs an explicit `name:`",
                    );
                    return;
                }
            },
        };
        out.push(Fragment {
            file: yf,
            name,
            explicit_name,
            folder,
            map,
            typed,
        });
    }

    /// Literal `ref('name')` calls in SQL and macro files: each must name a project `.sql` file.
    /// Returns the names referenced.
    pub(super) fn check_refs(
        &mut self,
        sql: &[PathBuf],
        macros: &[PathBuf],
        index: &BTreeMap<String, Vec<PathBuf>>,
        lookups: &BTreeMap<String, Lookup>,
    ) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        // name → the names its file refs, for cycle detection.
        let mut graph: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        for file in sql.iter().chain(macros) {
            let Ok(src) = std::fs::read_to_string(self.root.join(file)) else {
                continue;
            };
            let is_sql = !file.starts_with(MACROS_DIR);
            for (name, line) in preflight::Scan::new(&src).refs_at().iter().cloned() {
                if index.contains_key(&name) {
                    if is_sql {
                        let stem = file
                            .file_stem()
                            .expect("the path ends in a file name")
                            .to_string_lossy()
                            .to_string();
                        graph.entry(stem).or_default().push((name.clone(), line));
                    }
                    names.insert(name);
                } else if !lookups.contains_key(&name) {
                    self.diags.error(
                        Code::UnknownRef,
                        Some(file.clone()),
                        Some(line),
                        format!("`ref('{name}')`: there's no `{name}.sql` under reports/ and no lookup `{name}` under lookups/"),
                    );
                }
            }
        }
        // Report each cycle once, at the ref that closes it.
        let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
        for (start, edges) in &graph {
            let mut path = vec![start.clone()];
            let mut stack = vec![edges.iter()];
            while let Some(it) = stack.last_mut() {
                let Some((next, line)) = it.next() else {
                    stack.pop();
                    path.pop();
                    continue;
                };
                if let Some(i) = path.iter().position(|n| n == next) {
                    let mut cycle = path[i..].to_vec();
                    let mut key = cycle.clone();
                    key.sort();
                    if reported.insert(key) {
                        cycle.push(next.clone());
                        let file = path.last().and_then(|n| index.get(n)).and_then(|p| p.first());
                        self.diags.error(
                            Code::RefCycle,
                            file.cloned(),
                            Some(*line),
                            format!("`ref()` cycle: {}", cycle.join(" → ")),
                        );
                    }
                    continue;
                }
                if let Some(edges) = graph.get(next) {
                    path.push(next.clone());
                    stack.push(edges.iter());
                }
            }
        }
        names
    }

    /// `.sql` files by basename. Every list holds at least one path.
    pub(super) fn index_sql(&mut self, sql: &[PathBuf]) -> BTreeMap<String, Vec<PathBuf>> {
        let mut index: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for p in sql {
            let stem = p
                .file_stem()
                .expect("the path ends in a file name")
                .to_string_lossy()
                .to_string();
            index.entry(stem).or_default().push(p.clone());
        }
        for (name, paths) in &index {
            if let [_, second, ..] = paths.as_slice() {
                self.diags.error(
                    Code::DuplicateSqlName,
                    Some(second.clone()),
                    None,
                    format!(
                        "`{name}.sql` exists more than once: {}; .sql basenames must be unique project-wide",
                        paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_loader;
    use super::*;

    #[test]
    fn finds_yaml_sql_macros_and_folders_but_not_the_target_folder() {
        let dir = tempfile::tempdir().expect("a temp dir");
        for (path, text) in [
            ("dre_project.yml", "name: p\n"),
            ("reports/finance/monthly/monthly.yml", "queries: [monthly]\n"),
            ("reports/finance/monthly/monthly.sql", "select 1\n"),
            ("macros/helpers.sql", "{% macro x() %}1{% endmacro %}\n"),
            ("target/compiled/old.yml", "queries: [old]\n"),
        ] {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        let found = test_loader(dir.path()).discover();
        let rel = |v: &[PathBuf]| -> Vec<String> {
            v.iter()
                .map(|p| {
                    crate::slash(p.strip_prefix(dir.path()).unwrap_or(p))
                        .display()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(rel(&found.yaml), vec!["reports/finance/monthly/monthly.yml"]);
        assert_eq!(rel(&found.sql), vec!["reports/finance/monthly/monthly.sql"]);
        assert_eq!(rel(&found.macros), vec!["macros/helpers.sql"]);
        assert!(
            found
                .folders
                .contains(&vec!["finance".to_string(), "monthly".to_string()])
        );
    }
}
