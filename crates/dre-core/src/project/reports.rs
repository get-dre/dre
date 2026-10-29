//! Reports: merging YAML fragments by name, resolving queries, managed and unmanaged reports.

use super::*;

impl Loader {
    pub(super) fn merge_reports(&mut self, fragments: Vec<Fragment>) -> Vec<RawReport> {
        let mut by_name: BTreeMap<String, Vec<Fragment>> = BTreeMap::new();
        for f in fragments {
            by_name.entry(f.name.clone()).or_default().push(f);
        }
        let mut out = Vec::new();
        for (name, frags) in by_name {
            let definers: Vec<usize> = (0..frags.len())
                .filter(|i| frags[*i].map.contains_key("queries"))
                .collect();
            // `definers` holds indexes into `frags`.
            if let [first, second, ..] = definers.as_slice() {
                let second = &frags[*second].file;
                self.diags.error(
                    Code::DuplicateReportName,
                    Some(second.display.clone()),
                    key_line(second, "queries"),
                    format!(
                        "report `{name}` is declared in both {} and {}; report names must be unique project-wide",
                        frags[*first].file.display.display(),
                        second.display.display()
                    ),
                );
                continue;
            }
            let Some(&def) = definers.first() else {
                for f in &frags {
                    let msg = if f.explicit_name {
                        format!(
                            "report `{name}` has no `queries`; a config fragment must name a report that declares `queries:`"
                        )
                    } else {
                        format!("report `{name}` has no `queries`")
                    };
                    self.diags
                        .error(Code::MissingQueries, Some(f.file.display.clone()), None, msg);
                }
                continue;
            };
            let mut keys: BTreeMap<String, Located<Value>> = BTreeMap::new();
            let mut owner: BTreeMap<String, usize> = BTreeMap::new();
            let mut lines: BTreeMap<String, Option<usize>> = BTreeMap::new();
            for (i, f) in frags.iter().enumerate() {
                for (k, v) in &f.map {
                    let k = k.as_str();
                    if k == "name" || !REPORT_KEYS.contains(&k) {
                        continue;
                    }
                    if let Some(prev) = keys.get(k) {
                        self.diags.error(
                            Code::ConflictingDeclaration,
                            Some(f.file.display.clone()),
                            key_line(&f.file, k),
                            format!(
                                "`{k}` for report `{name}` is declared in both {} and {}",
                                prev.file.display.display(),
                                f.file.display.display()
                            ),
                        );
                        continue;
                    }
                    keys.insert(
                        k.to_string(),
                        Located {
                            value: v.clone(),
                            file: f.file.clone(),
                        },
                    );
                    owner.insert(k.to_string(), i);
                    lines.insert(k.to_string(), key_line(&f.file, k));
                }
            }
            let file = frags[def].file.clone();
            let folder = frags[def].folder.clone();
            let mut typed = ReportFile::default();
            for (i, f) in frags.into_iter().enumerate() {
                let owns = |k: &str| owner.get(k) == Some(&i);
                let ReportFile {
                    name: _,
                    description,
                    meta,
                    output: _,
                    plugins: _,
                    sources: _,
                    tags,
                    queries,
                    profile,
                    default_set,
                    vars,
                    var_schema,
                    timezone,
                    locale,
                    sets,
                    schedule,
                    unknown: _,
                } = f.typed;
                if owns("description") {
                    typed.description = description;
                }
                if owns("meta") {
                    typed.meta = meta;
                }
                if owns("tags") {
                    typed.tags = tags;
                }
                if owns("queries") {
                    typed.queries = queries;
                }
                if owns("profile") {
                    typed.profile = profile;
                }
                if owns("default_set") {
                    typed.default_set = default_set;
                }
                if owns("vars") {
                    typed.vars = vars;
                }
                if owns("var_schema") {
                    typed.var_schema = var_schema;
                }
                if owns("timezone") {
                    typed.timezone = timezone;
                }
                if owns("locale") {
                    typed.locale = locale;
                }
                if owns("sets") {
                    typed.sets = sets;
                }
                if owns("schedule") {
                    typed.schedule = schedule;
                }
            }
            out.push(RawReport {
                name,
                file,
                folder,
                keys,
                lines,
                typed,
            });
        }
        out
    }

    pub(super) fn resolve_queries(
        &mut self,
        r: &RawReport,
        index: &BTreeMap<String, Vec<PathBuf>>,
        referenced: &mut BTreeSet<String>,
    ) -> Vec<QueryEntry> {
        let Some(q) = &r.typed.queries else {
            return Vec::new();
        };
        let file = r
            .keys
            .get("queries")
            .map_or_else(|| r.file.display.clone(), |k| k.file.display.clone());
        let Loose::Ok(items) = &q.value else {
            self.diags.error(
                Code::InvalidField,
                Some(file),
                q.line(),
                format!("report `{}`: `queries` must be a list", r.name),
            );
            return Vec::new();
        };
        if items.is_empty() {
            self.diags.error(
                Code::MissingQueries,
                Some(file.clone()),
                q.line(),
                format!("report `{}` has an empty `queries` list", r.name),
            );
        }
        let mut out = Vec::new();
        for item in items {
            if let Some(e) = self.query_entry(&r.name, item, &file, index) {
                referenced.insert(e.query.clone());
                out.push(e);
            } else if let Some(n) = query_item_name(item) {
                referenced.insert(n.to_string());
            }
        }
        out
    }

    fn query_entry(
        &mut self,
        report: &str,
        item: &QueryItem,
        file: &Path,
        index: &BTreeMap<String, Vec<PathBuf>>,
    ) -> Option<QueryEntry> {
        let file = Some(file.to_path_buf());
        let line = item.line();
        let (name, m) = match &item.value {
            Loose::Ok(de::OneOf::A(s)) => (s.clone(), None),
            Loose::Ok(de::OneOf::B(m)) => match &m.query {
                Some(Loose::Ok(s)) => (s.clone(), Some(m)),
                _ => {
                    self.diags.error(
                        Code::InvalidField,
                        file,
                        line,
                        format!("report `{report}`: a `queries` map entry needs a `query:` name"),
                    );
                    return None;
                }
            },
            Loose::Bad(_) => {
                self.diags.error(
                    Code::InvalidField,
                    file,
                    line,
                    format!("report `{report}`: `queries` entries must be names or maps"),
                );
                return None;
            }
        };
        if name.ends_with(".sql") || name.contains('/') || name.contains('\\') {
            self.diags.error(
                Code::InvalidQueryName,
                file,
                line,
                format!(
                    "report `{report}`: query `{name}` must be a bare .sql basename, with no path and no extension"
                ),
            );
            return None;
        }
        let mut e = QueryEntry {
            query: name.clone(),
            path: PathBuf::new(),
            profile: None,
            tab: true,
            tab_name: None,
            anchor: None,
            header: None,
            columns: BTreeMap::new(),
            autofit: None,
            style: None,
            on_empty: None,
            secret_columns: Vec::new(),
        };
        if let Some(m) = m {
            for k in &m.unknown.0 {
                self.diags.error(
                    Code::UnknownKey,
                    file.clone(),
                    Some(k.line),
                    format!("report `{report}`: unknown key `{}` on query `{name}`", k.name),
                );
            }
            match &m.profile {
                None => {}
                Some(Loose::Ok(p)) if !p.trim().is_empty() => e.profile = Some(p.clone()),
                Some(_) => self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    line,
                    format!("report `{report}`: `profile` of `{name}` must be a connection name"),
                ),
            }
            e.tab_name = match &m.tab_name {
                None => None,
                Some(Loose::Ok(s)) => Some(s.clone()),
                Some(Loose::Bad(f)) if f.kind == "a list" => {
                    self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: {}", one_tab_per_file(&name)),
                    );
                    None
                }
                Some(Loose::Bad(_)) => {
                    self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: `tab_name` of `{name}` must be a string"),
                    );
                    None
                }
            };
            match &m.tab {
                None => {}
                Some(Loose::Ok(b)) => e.tab = *b,
                Some(Loose::Bad(_)) => self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    line,
                    format!("report `{report}`: `tab` of `{name}` must be true or false"),
                ),
            }
            if !e.tab && e.tab_name.is_some() {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    line,
                    format!("report `{report}`: `{name}` has `tab: false`, so its `tab_name` would never be used; remove one of them"),
                );
            }
            if let Some(a) = &m.anchor {
                match a {
                    Loose::Ok(s) if options::is_cell(s) => e.anchor = Some(s.clone()),
                    _ => self.diags.error(
                        Code::InvalidCell,
                        file.clone(),
                        line,
                        format!("report `{report}`: `anchor` of `{name}` must be a cell reference like `A1`"),
                    ),
                }
            }
            if let Some(h) = &m.header {
                match h {
                    Loose::Ok(b) => e.header = Some(*b),
                    Loose::Bad(_) => self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: `header` of `{name}` must be true or false"),
                    ),
                }
            }
            match &m.on_empty {
                None => {}
                Some(Loose::Ok(v)) => e.on_empty = Some(*v),
                Some(Loose::Bad(_)) => self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    line,
                    format!(
                        "report `{report}`: `on_empty` of `{name}` must be one of {}",
                        crate::no_data::OnEmpty::NAMES.join(", ")
                    ),
                ),
            }
            match &m.secret_columns {
                None => {}
                Some(Loose::Ok(v)) => e.secret_columns = v.clone(),
                Some(Loose::Bad(_)) => self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    line,
                    format!("report `{report}`: `secret_columns` of `{name}` must be a list of column names"),
                ),
            }
            if let Some(a) = &m.autofit {
                match a {
                    Loose::Ok(b) => e.autofit = Some(*b),
                    Loose::Bad(_) => self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: `autofit` of `{name}` must be true or false"),
                    ),
                }
            }
            if let Some(st) = &m.style {
                let (style, errs) = dre_protocol::style::parse_sheet(st);
                for err in errs {
                    self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: query `{name}`: `style`: {err}"),
                    );
                }
                e.style = Some(style);
            }
            if let Some(c) = &m.columns {
                let (columns, errs) = dre_protocol::options::parse_columns(c);
                for err in errs {
                    self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: query `{name}`: {err}"),
                    );
                }
                e.columns = columns;
            }
        }
        match index.get(&name) {
            Some(paths) => {
                e.path = paths[0].clone();
                Some(e)
            }
            None => {
                self.diags.error(
                    Code::UnknownQuery,
                    file,
                    line,
                    format!("report `{report}`: query `{name}` doesn't match any .sql file under reports/"),
                );
                None
            }
        }
    }

    pub(super) fn resolve_managed(
        &mut self,
        r: &RawReport,
        queries: Vec<QueryEntry>,
        project: &Project,
        folders: &BTreeMap<Vec<String>, FolderCfg>,
        used: &mut Usage,
    ) -> Option<Report> {
        let name = r.name.clone();
        let layers = folder_layers(folders, &r.folder);
        let key = |k: &str| r.keys.get(k);
        let located = |k: &str| {
            r.keys
                .get(k)
                .map(|l| (l.file.display.clone(), r.lines.get(k).copied().flatten()))
        };
        let typed = &r.typed;
        let var_schema = match &typed.var_schema {
            None => BTreeMap::new(),
            Some(de::Located {
                value: Loose::Ok(schema),
                ..
            }) => schema.clone(),
            Some(value) => {
                let (file, _) = located("var_schema").expect("the key is present: its value was read");
                self.diags.error(Code::InvalidField, Some(file), value.line(),
                    "`var_schema` must map variable names to declarations with a supported type, default, allowed values, description, required flag, pattern and max_length");
                BTreeMap::new()
            }
        };
        for (name, definition) in &var_schema {
            for problem in definition.problems(name) {
                self.diags
                    .error(Code::InvalidField, Some(r.file.display.clone()), None, problem);
            }
            if let Some(value) = &definition.default
                && let Err(error) = definition.check(name, value)
            {
                self.diags
                    .error(Code::InvalidField, Some(r.file.display.clone()), None, error);
            }
            if name.trim().is_empty() {
                self.diags.error(
                    Code::InvalidField,
                    Some(r.file.display.clone()),
                    None,
                    "variable names must be non-empty",
                );
            }
        }
        let description = self.typed_string(
            &located("description").map_or_else(|| r.file.display.clone(), |(f, _)| f),
            typed.description.clone(),
            "description",
        );
        let meta_file = located("meta").map_or_else(|| r.file.display.clone(), |(f, _)| f);
        let meta = self.typed_metadata(&meta_file, typed.meta.clone(), "meta");
        let mut meta_layers = metadata_layers(project, &layers);
        if typed.meta.is_some() {
            meta_layers.push(MetaLayer {
                file: crate::slash(&meta_file).to_string_lossy().into_owned(),
                key: "meta".into(),
                meta: meta.clone(),
            });
        }

        let mut tags: Vec<String> = layers.iter().flat_map(|l| l.tags.clone()).collect();
        if let Some(t) = &typed.tags {
            match &t.value {
                Loose::Ok(l) => tags.extend(l.iter().cloned()),
                Loose::Bad(_) => {
                    let (f, l) = located("tags").expect("the key is present: its value was read");
                    self.diags.error(
                        Code::InvalidField,
                        Some(f),
                        l,
                        format!("report `{name}`: `tags` must be a list of strings"),
                    );
                }
            }
        }
        dedup(&mut tags);

        if key("profile").is_some() && key("sets").is_some() {
            let (f, l) = located("sets").expect("the key is present: its value was read");
            self.diags.error(
                Code::ProfileAndSets,
                Some(f),
                l,
                format!("report `{name}` declares both `profile:` and `sets:`; use one or the other"),
            );
        }

        let report_profile = match &typed.profile {
            Some(p) => match &p.value {
                Loose::Ok(s) => {
                    let (f, l) = located("profile").expect("the key is present: its value was read");
                    used.connection(s, Some(f), l);
                    Some(s.clone())
                }
                Loose::Bad(_) => {
                    let (f, l) = located("profile").expect("the key is present: its value was read");
                    self.diags.error(
                        Code::InvalidField,
                        Some(f),
                        l,
                        format!("report `{name}`: `profile` must be a string"),
                    );
                    None
                }
            },
            None => None,
        };
        let folder_profile = layers
            .iter()
            .rev()
            .find_map(|l| l.profile.as_ref().map(|p| p.0.clone()));
        let report_timezone = match &typed.timezone {
            Some(t) => {
                let (f, l) = located("timezone").expect("the key is present: its value was read");
                self.timezone_str(
                    t.value.ok().map(String::as_str),
                    &f,
                    l,
                    &format!("report `{name}`: `timezone`"),
                )
            }
            None => None,
        };
        let timezone = report_timezone
            .or_else(|| layers.iter().rev().find_map(|l| l.timezone.clone()))
            .or_else(|| project.timezone.clone());
        let report_locale = match &typed.locale {
            Some(t) => {
                let (f, l) = located("locale").expect("the key is present: its value was read");
                self.locale_str(
                    t.value.ok().map(String::as_str),
                    &f,
                    l,
                    &format!("report `{name}`: `locale`"),
                )
            }
            None => None,
        };
        let locale = report_locale
            .or_else(|| layers.iter().rev().find_map(|l| l.locale.clone()))
            .or_else(|| project.locale.clone());
        let profile_at = if report_profile.is_some() {
            located("profile").map(|(file, line)| ProfileAt {
                file,
                line,
                key: "`profile`".into(),
            })
        } else {
            inherited_profile_at(&layers, project)
        };
        let base_profile = report_profile
            .or(folder_profile)
            .or(project.default_profile.clone());

        let mut vars = project.vars.clone();
        for l in &layers {
            if let Some(v) = &l.vars {
                vars.extend(v.clone());
            }
        }
        if let Some(v) = &typed.vars {
            match &v.value {
                Loose::Ok(m) => vars.extend(m.clone()),
                Loose::Bad(_) => {
                    let (f, l) = located("vars").expect("the key is present: its value was read");
                    self.diags.error(
                        Code::InvalidField,
                        Some(f),
                        l,
                        format!("report `{name}`: `vars` must be a map"),
                    );
                }
            }
        }

        let mut output = builtin_output();
        for (name, definition) in &var_schema {
            if let Some(value) = &definition.default {
                vars.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
        let mut merge_problems = Vec::new();
        if let Some(o) = project_default_output(project) {
            merge_problems.extend(merge_output(&mut output, &o));
        }
        for l in &layers {
            if let Some(o) = &l.output {
                merge_problems.extend(merge_output(&mut output, o));
            }
        }
        for p in merge_problems {
            self.diags.error(
                Code::InvalidField,
                Some(r.file.display.clone()),
                None,
                format!("report `{name}`: folder config: {p}"),
            );
        }
        let mut outputs = vec![output];
        if let Some(o) = key("output") {
            for p in layer_outputs(&mut outputs, &o.value) {
                let (f, l) = located("output").expect("the key is present: its value was read");
                self.diags
                    .error(Code::InvalidField, Some(f), l, format!("report `{name}`: {p}"));
            }
        }

        if typed.schedule.is_some() {
            let (f, l) = located("schedule").expect("the key is present: its value was read");
            self.moved_to_schedules(&f, l, &format!("report `{name}`: `schedule`"));
        }

        let default_set = typed.default_set.as_ref().and_then(|d| d.value.ok().cloned());

        let base = BindingBase {
            profile: base_profile,
            profile_at,
            vars,
            outputs,
            locale,
        };
        let has_sets = key("sets").is_some();
        let report_base = self.silent_binding(&name, &base, &queries, &r.file.display);
        let mut bindings = Vec::new();
        if let (Some(s), Some(at)) = (&typed.sets, key("sets")) {
            bindings = self.resolve_sets(&name, s, &at.file.display, &queries, &base, project, used);
            let declared: Vec<&str> = bindings.iter().filter_map(|b| b.set.as_deref()).collect();
            if let Some(d) = &default_set
                && !declared.contains(&d.as_str())
            {
                let (f, l) = located("default_set").expect("the key is present: its value was read");
                self.diags.error(
                    Code::UnknownDefaultSet,
                    Some(f),
                    l,
                    format!("report `{name}`: `default_set` `{d}` isn't one of the report's `sets:`"),
                );
            }
        } else {
            if default_set.is_some() {
                let (f, l) = located("default_set").expect("the key is present: its value was read");
                self.diags.error(
                    Code::UnknownDefaultSet,
                    Some(f),
                    l,
                    format!("report `{name}`: `default_set` needs a `sets:` list"),
                );
            }
            let b = self.finish_binding(&name, None, &base, None, queries.clone(), &r.file.display, used);
            bindings.push(b);
        }

        Some(Report {
            name,
            var_schema,
            description,
            meta,
            meta_layers,
            managed: true,
            file: r.file.display.clone(),
            folder: r.folder.clone(),
            tags,
            queries,
            default_set,
            timezone,
            has_sets,
            bindings,
            base: report_base,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_binding(
        &mut self,
        report: &str,
        set: Option<String>,
        base: &BindingBase,
        tab_names: Option<&Mapping>,
        mut queries: Vec<QueryEntry>,
        file: &Path,
        used: &mut Usage,
    ) -> Binding {
        let ctx = match &set {
            Some(s) => format!("report `{report}`, Set `{s}`"),
            None => format!("report `{report}`"),
        };
        if let Some(t) = tab_names {
            for (k, v) in t {
                let q = k.as_str();
                match queries.iter_mut().find(|e| e.query == q) {
                    Some(e) => match v {
                        Value::String(s) => e.tab_name = Some(s.clone()),
                        _ => self.diags.error(
                            Code::InvalidField,
                            Some(file.to_path_buf()),
                            None,
                            format!("{ctx}: {}", one_tab_per_file(q)),
                        ),
                    },
                    None => self.diags.error(
                        Code::UnknownQuery,
                        Some(file.to_path_buf()),
                        None,
                        format!("{ctx}: `tab_names` names `{q}`, which isn't one of this Binding's queries"),
                    ),
                }
            }
        }
        let outputs = self.typed_outputs(&base.outputs, &ctx, file, &queries, used);
        if let Some(p) = &base.profile {
            used.connection(p, None, None);
        }
        Binding {
            set,
            profile: base.profile.clone(),
            vars: base.vars.clone(),
            queries: std::mem::take(&mut queries),
            locale: base.locale.clone(),
            outputs,
            schedules: Vec::new(),
            parsed: None,
            profile_at: base.profile_at.clone(),
        }
    }

    pub(super) fn resolve_unmanaged(
        &mut self,
        name: &str,
        path: &Path,
        project: &Project,
        folders: &BTreeMap<Vec<String>, FolderCfg>,
        used: &mut Usage,
    ) -> Report {
        let folder = folder_segments(path.parent().unwrap_or(Path::new("")));
        let layers = folder_layers(folders, &folder);
        let mut tags: Vec<String> = layers.iter().flat_map(|l| l.tags.clone()).collect();
        dedup(&mut tags);
        let profile = layers
            .iter()
            .rev()
            .find_map(|l| l.profile.as_ref().map(|p| p.0.clone()))
            .or(project.default_profile.clone());
        let mut vars = project.vars.clone();
        for l in &layers {
            if let Some(v) = &l.vars {
                vars.extend(v.clone());
            }
        }
        let mut output = builtin_output();
        let mut merge_problems = Vec::new();
        if let Some(o) = project_default_output(project) {
            merge_problems.extend(merge_output(&mut output, &o));
        }
        for l in &layers {
            if let Some(o) = &l.output {
                merge_problems.extend(merge_output(&mut output, o));
            }
        }
        for p in merge_problems {
            self.diags.error(
                Code::InvalidField,
                Some(path.to_path_buf()),
                None,
                format!("folder config: {p}"),
            );
        }
        self.diags.warning(
            Code::UnmanagedReport,
            Some(path.to_path_buf()),
            None,
            format!(
                "`{name}` is an unmanaged report (no YAML lists it in `queries:`); unmanaged reports are for quick tests — add a YAML to make it a managed report"
            ),
        );
        self.check_unmanaged_sql(name, path);
        let query = QueryEntry {
            query: name.to_string(),
            path: path.to_path_buf(),
            profile: None,
            tab: true,
            tab_name: None,
            anchor: None,
            header: None,
            columns: BTreeMap::new(),
            autofit: None,
            style: None,
            on_empty: None,
            secret_columns: Vec::new(),
        };
        let base = BindingBase {
            profile: profile.clone(),
            profile_at: inherited_profile_at(&layers, project),
            vars,
            outputs: vec![output],
            locale: layers
                .iter()
                .rev()
                .find_map(|l| l.locale.clone())
                .or_else(|| project.locale.clone()),
        };
        if let Some(p) = &profile {
            used.connection(p, None, None);
        }
        let b = self.finish_binding(name, None, &base, None, vec![query.clone()], path, used);
        Report {
            name: name.to_string(),
            description: None,
            meta: JsonMap::new(),
            meta_layers: metadata_layers(project, &layers),
            var_schema: BTreeMap::new(),
            managed: false,
            file: path.to_path_buf(),
            folder,
            tags,
            queries: vec![query],
            default_set: None,
            timezone: layers
                .iter()
                .rev()
                .find_map(|l| l.timezone.clone())
                .or_else(|| project.timezone.clone()),
            has_sets: false,
            base: b.clone(),
            bindings: vec![b],
        }
    }

    /// Best-effort check on the raw text; the run path re-checks the rendered SQL.
    fn check_unmanaged_sql(&mut self, name: &str, path: &Path) {
        let Ok(text) = std::fs::read_to_string(self.root.join(path)) else {
            return;
        };
        let head = sqlsplit::strip_leading_comments(&text);
        if head.starts_with("{{") || head.starts_with("{%") || head.starts_with("{#") {
            return;
        }
        for st in sqlsplit::split(&text) {
            if !sqlsplit::classify(&st.text).is_read_only_safe() {
                let body = sqlsplit::strip_leading_comments(&st.text);
                let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
                self.diags.error(
                    Code::UnmanagedSideEffect,
                    Some(path.to_path_buf()),
                    Some(line),
                    format!(
                        "unmanaged report `{name}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; rewrite the statement, or give the report a YAML to declare it",
                        dre_protocol::util::summarize(body, 60)
                    ),
                );
            }
        }
    }
}

pub(super) struct RawReport {
    name: String,
    file: Rc<YamlFile>,
    folder: Vec<String>,
    /// Each key's value and the fragment it's in, for the keys not yet read as typed config.
    keys: BTreeMap<String, Located<Value>>,
    /// The line of each key, in its fragment.
    lines: BTreeMap<String, Option<usize>>,
    /// The typed keys, each from the fragment that declares it.
    typed: ReportFile,
}

pub(super) struct BindingBase {
    pub(super) profile: Option<String>,
    pub(super) profile_at: Option<ProfileAt>,
    pub(super) vars: JsonMap<String, Json>,
    /// The merged outputs, in order: one unless a layer gave a list.
    pub(super) outputs: Vec<Mapping>,
    pub(super) locale: Option<String>,
}
