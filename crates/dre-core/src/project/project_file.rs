//! Reading `dre_project.yml`: project settings, flags, `dispatch:` and folder config (`reports:`).

use super::*;

impl Loader {
    pub(super) fn parse_project_file(&mut self, yf: &Rc<YamlFile>, pf: ProjectFile) -> Option<Project> {
        let file = Some(yf.display.clone());
        for (k, line) in [("destinations", &pf.destinations), ("formats", &pf.formats)] {
            if line.is_some() {
                self.old_plugin_key(yf, k);
            }
        }
        if let Some(t) = &pf.target {
            self.diags.error(
                Code::RemovedKey,
                file.clone(),
                t.line(),
                "`target` in dre_project.yml was removed in DRE 0.2.1: give each profile its default with `target:` in profiles.yml, or choose the run's target with DRE_TARGET or --target",
            );
        }
        for k in &pf.unknown.0 {
            self.diags.error(
                Code::UnknownKey,
                file.clone(),
                Some(k.line),
                format!("unknown key `{}`", k.name),
            );
        }
        let name = match pf.name {
            Some(de::Located {
                value: Loose::Ok(s), ..
            }) if !s.trim().is_empty() => Some(s),
            Some(n) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    n.line(),
                    "`name` must be a non-empty string",
                );
                None
            }
            None => {
                self.diags.error(
                    Code::MissingField,
                    file.clone(),
                    None,
                    "missing required field `name`",
                );
                None
            }
        };
        let meta = self.typed_metadata(&yf.display, pf.meta, "meta");
        let default_profile_line = pf.default_profile.as_ref().and_then(de::Located::line);
        let default_profile = self.typed_string(&yf.display, pf.default_profile, "default_profile");
        let run_target = crate::profiles::RunTarget::resolve(self.opts.target.as_deref(), self.ctx.env());
        let default_set = self.typed_string(&yf.display, pf.default_set, "default_set");
        let vars = match pf.vars {
            None
            | Some(de::Located {
                value: Loose::Ok(None),
                ..
            }) => JsonMap::new(),
            Some(de::Located {
                value: Loose::Ok(Some(m)),
                ..
            }) => m,
            Some(v) => {
                self.diags
                    .error(Code::InvalidField, file.clone(), v.line(), "`vars` must be a map");
                JsonMap::new()
            }
        };
        let run_query_max_rows = match pf.run_query_max_rows {
            None => DEFAULT_RUN_QUERY_MAX_ROWS,
            Some(de::Located {
                value: Loose::Ok(n), ..
            }) if n > 0 => n,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    v.line(),
                    "`run_query_max_rows` must be a positive whole number",
                );
                DEFAULT_RUN_QUERY_MAX_ROWS
            }
        };
        let (run_timeout, keep_runs) = self.parse_flags(file.as_deref(), pf.flags);
        let dispatch = self.parse_dispatch(&yf.display, pf.dispatch);
        match pf.format_options {
            None | Some(de::Located { value: Loose::Ok(None), .. }) => {}
            Some(de::Located { value: Loose::Ok(Some(f)), .. }) if f.iter().all(|(_, v)| v.ok().is_some()) => {
                self.format_options = f
                    .0
                    .into_iter()
                    .filter_map(|(k, v)| match v {
                        Loose::Ok(m) => Some((k.value, Value::Object(m))),
                        Loose::Bad(_) => None,
                    })
                    .collect();
            }
            Some(f) => self.diags.error(
                Code::InvalidField,
                file.clone(),
                f.line(),
                "`format_options` must map format names to their options, e.g. `delimited: {delimiter: \"|\"}`",
            ),
        }
        let mask_secrets = match pf.mask_secrets {
            None => true,
            Some(de::Located {
                value: Loose::Ok(b), ..
            }) => b,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    v.line(),
                    "`mask_secrets` must be true or false",
                );
                true
            }
        };
        let mask_row_values = match pf.mask_row_values {
            None => true,
            Some(de::Located {
                value: Loose::Ok(b), ..
            }) => b,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    v.line(),
                    "`mask_row_values` must be true or false",
                );
                true
            }
        };
        let timezone = pf.timezone.and_then(|v| {
            let line = v.line();
            self.timezone_str(v.value.ok().map(String::as_str), &yf.display, line, "`timezone`")
        });
        let locale = pf.locale.and_then(|v| {
            let line = v.line();
            self.locale_str(v.value.ok().map(String::as_str), &yf.display, line, "`locale`")
        });
        let week_start = match pf.week_start {
            None => WeekStart::Monday,
            Some(de::Located {
                value: Loose::Ok(w), ..
            }) => w,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    v.line(),
                    "`week_start` must be `monday` or `sunday`",
                );
                WeekStart::Monday
            }
        };
        let week_numbering = match pf.week_numbering {
            None => WeekNumbering::Iso,
            Some(de::Located {
                value: Loose::Ok(w), ..
            }) => w,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    v.line(),
                    "`week_numbering` must be `iso` or `us`",
                );
                WeekNumbering::Iso
            }
        };
        let lookup_inline_max_rows = match pf.lookup_inline_max_rows {
            None => DEFAULT_INLINE_MAX_ROWS,
            Some(de::Located {
                value: Loose::Ok(n), ..
            }) => n,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    v.line(),
                    "`lookup_inline_max_rows` must be a whole number",
                );
                DEFAULT_INLINE_MAX_ROWS
            }
        };
        Some(Project {
            name: name?,
            meta,
            root: self.root.clone(),
            target_dir: self.target.dir.clone(),
            target_source: self.target.source,
            target_name: run_target.name,
            target_from: run_target.from,
            default_profile_line,
            default_profile,
            default_set,
            vars,
            keep_runs,
            run_timeout,
            reports: Vec::new(),
            sets: BTreeMap::new(),
            plugins: Vec::new(),
            plugin_uses: Vec::new(),
            plugins_incomplete: false,
            schedules: Vec::new(),
            timings: BTreeMap::new(),
            templates: crate::render::ProjectTemplates {
                run_query_max_rows,
                lookup_inline_max_rows,
                dispatch,
                ..Default::default()
            },
            mask_secrets,
            mask_row_values,
            timezone,
            locale,
            week_start,
            week_numbering,
            format_options: self
                .format_options
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_object()?.clone())))
                .collect(),
            folders: Vec::new(),
            files: Vec::new(),
            inputs: crate::parse::Inputs::new(self.ctx.clone()),
            parse_errors: BTreeMap::new(),
            sources: BTreeMap::new(),
            profiles: Profiles::default(),
            settings: self.opts.settings.clone(),
        })
    }

    /// The settings the load resolved, beside the command line's.
    pub(super) fn record_settings(&self, project: &mut Project) {
        use crate::settings::{self, Source};
        let s = &mut project.settings;
        let target = &project.profiles.run;
        let target_source = match target.from {
            crate::profiles::TargetSource::Flag => Source::Flag("--target"),
            crate::profiles::TargetSource::Env => Source::Env(settings::TARGET),
            crate::profiles::TargetSource::Default => Source::Default,
        };
        s.set("target", Some(target.name.clone()), target_source);
        let path_source = match self.target.source {
            crate::target::Source::Flag => Source::Flag("--target-path"),
            crate::target::Source::Env => Source::Env(settings::TARGET_PATH),
            crate::target::Source::Project => Source::Project(crate::target::KEY),
            crate::target::Source::Default => Source::Default,
        };
        s.set(
            "target_path",
            Some(crate::slash(&self.target.dir).display().to_string()),
            path_source,
        );
        let dir_source = match project.profiles.found_by {
            "--profiles-dir" => Source::Flag("--profiles-dir"),
            "DRE_PROFILES_DIR" => Source::Env(settings::PROFILES_DIR),
            "the project directory" => Source::Found("the project directory"),
            _ => Source::Default,
        };
        let dir = project.profiles.path.parent().map(|d| d.display().to_string());
        s.set("profiles_dir", dir, dir_source);
        let env_or_default = |var: &'static str| match self.ctx.env().setting(var) {
            Some(_) => Source::Env(var),
            None => Source::Default,
        };
        s.set(
            "plugins_dir",
            Some(
                crate::slash(&crate::plugins::plugins_dir(self.ctx.env(), Some(&self.root)))
                    .display()
                    .to_string(),
            ),
            env_or_default(settings::PLUGINS_DIR),
        );
        s.set(
            "registry_url",
            Some(crate::manager::registry_url(self.ctx.env())),
            env_or_default(settings::REGISTRY_URL),
        );
    }

    /// A metadata map. Its contents belong to the consuming tool, not core.
    pub(super) fn typed_metadata(
        &mut self,
        file: &Path,
        value: Option<de::Located<Loose<JsonMap<String, Json>>>>,
        key: &str,
    ) -> JsonMap<String, Json> {
        match value {
            None => JsonMap::new(),
            Some(de::Located {
                value: Loose::Ok(map),
                ..
            }) => map,
            Some(value) => {
                self.diags.error(
                    Code::InvalidField,
                    Some(file.to_path_buf()),
                    value.line(),
                    format!("`{key}` must be a map"),
                );
                JsonMap::new()
            }
        }
    }

    /// An optional string key of a typed file: its value, else an error.
    pub(super) fn typed_string(
        &mut self,
        file: &Path,
        v: Option<de::Located<Loose<String>>>,
        key: &str,
    ) -> Option<String> {
        match v? {
            de::Located {
                value: Loose::Ok(s), ..
            } => Some(s),
            v => {
                self.diags.error(
                    Code::InvalidField,
                    Some(file.to_path_buf()),
                    v.line(),
                    format!("`{key}` must be a string"),
                );
                None
            }
        }
    }

    /// A `timezone:` value: an IANA name, else an error at `line` (`v` is `None` when it isn't a
    /// string).
    pub(super) fn timezone_str(
        &mut self,
        v: Option<&str>,
        file: &Path,
        line: Option<usize>,
        what: &str,
    ) -> Option<String> {
        let msg = match v {
            Some(s) => match crate::dates::parse_tz(s) {
                Ok(_) => return Some(s.to_string()),
                Err(e) => format!("{what}: {e}"),
            },
            None => format!("{what} must be a string, an IANA timezone name such as `Australia/Sydney`"),
        };
        self.diags
            .error(Code::InvalidTimezone, Some(file.to_path_buf()), line, msg);
        None
    }

    /// A `locale:` value: a tag the number filters know (`de-DE`), else an error at `line` (`v`
    /// is `None` when it isn't a string).
    pub(super) fn locale_str(
        &mut self,
        v: Option<&str>,
        file: &Path,
        line: Option<usize>,
        what: &str,
    ) -> Option<String> {
        let msg = match v {
            Some(s) => match crate::numbers::Locale::parse(s) {
                Ok(_) => return Some(s.to_string()),
                Err(e) => format!("{what}: {e}"),
            },
            None => format!("{what} must be a string, a locale such as `de-DE`"),
        };
        self.diags
            .error(Code::InvalidLocale, Some(file.to_path_buf()), line, msg);
        None
    }

    /// `flags:`: settings for DRE itself. Each takes effect where it's used (downloads read
    /// `http_timeout`); returns `run_timeout`.
    fn parse_flags(
        &mut self,
        file: Option<&Path>,
        v: Option<de::Located<Loose<config::project::Flags>>>,
    ) -> (Option<std::time::Duration>, usize) {
        let flags = match v {
            None => return (None, 1),
            Some(de::Located {
                value: Loose::Ok(f), ..
            }) => f,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    v.line(),
                    "`flags` must be a map",
                );
                return (None, 1);
            }
        };
        for k in &flags.unknown.0 {
            self.diags.error(
                Code::UnknownKey,
                file.map(Path::to_path_buf),
                Some(k.line),
                format!(
                    "unknown key `flags.{}`; flags are: http_timeout, keep_runs, run_timeout",
                    k.name
                ),
            );
        }
        match flags.http_timeout {
            None => {}
            Some(de::Located {
                value: Loose::Ok(n), ..
            }) if n > 0 => self.ctx.set_project_http_timeout(n),
            Some(v) => self.diags.error(
                Code::InvalidField,
                file.map(Path::to_path_buf),
                v.line(),
                "`flags.http_timeout` must be a positive whole number of seconds",
            ),
        }
        let keep_runs = match flags.keep_runs {
            None => 1,
            Some(de::Located {
                value: Loose::Ok(n), ..
            }) if n > 0 => n as usize,
            Some(v) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    v.line(),
                    "`flags.keep_runs` must be a whole number of 1 or more",
                );
                1
            }
        };
        let Some(v) = flags.run_timeout else {
            return (None, keep_runs);
        };
        let line = v.line();
        let parsed = match v.value {
            Loose::Ok(v) => dre_protocol::delivery::parse_duration(&v),
            _ => Err("must be a duration such as `2h` or `90m`, or seconds".to_string()),
        };
        let run_timeout = match parsed {
            Ok(d) if !d.is_zero() => Some(d),
            Ok(_) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    line,
                    "`flags.run_timeout` must be more than zero",
                );
                None
            }
            Err(e) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    line,
                    format!("`flags.run_timeout` {e}"),
                );
                None
            }
        };
        (run_timeout, keep_runs)
    }

    /// `dispatch: [{macro_namespace: dre_utils, search_order: [my_project, dre_utils]}]`.
    fn parse_dispatch(&mut self, file: &Path, v: Option<config::project::DispatchList>) -> DispatchOrder {
        let mut out = DispatchOrder::new();
        let Some(v) = v else { return out };
        let line = v.line();
        let Loose::Ok(list) = v.value else {
            self.diags.error(
                Code::InvalidField,
                Some(file.to_path_buf()),
                line,
                "`dispatch` must be a list of `{macro_namespace, search_order}`",
            );
            return out;
        };
        for e in list {
            let (ns, order) = match e {
                Loose::Ok(e) => {
                    let ns = e.macro_namespace.and_then(|n| n.ok().cloned());
                    let order: Option<Vec<String>> = e
                        .search_order
                        .and_then(|s| s.ok().map(|s| s.iter().filter_map(|x| x.ok().cloned()).collect()));
                    (ns, order)
                }
                Loose::Bad(_) => (None, None),
            };
            match (ns, order) {
                (Some(ns), Some(order)) if !order.is_empty() => {
                    out.insert(ns, order);
                }
                _ => self.diags.error(
                    Code::InvalidField,
                    Some(file.to_path_buf()),
                    line,
                    "each `dispatch` entry needs `macro_namespace` and a non-empty `search_order` list",
                ),
            }
        }
        out
    }

    /// Package names are Jinja variables: they can't clash with each other's macros, the
    /// project's own macros, or DRE's functions.
    pub(super) fn check_macro_namespaces(&mut self, project: &Project) {
        let own: BTreeSet<String> = project
            .templates
            .macros
            .iter()
            .filter_map(|m| std::fs::read_to_string(self.root.join(m)).ok())
            .flat_map(|src| {
                preflight::Scan::new(&src)
                    .macros()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        for p in &project.templates.packages {
            let clash = if RESERVED_NAMES.contains(&p.name.as_str()) {
                Some("a DRE function or variable".to_string())
            } else if own.contains(&p.name) {
                Some("a macro in macros/".to_string())
            } else if p.name == project.name {
                Some("this project".to_string())
            } else {
                None
            };
            if let Some(c) = clash {
                self.diags.error(
                    Code::PackageNameClash,
                    None,
                    None,
                    format!(
                        "package `{}` has the same name as {c}; macros couldn't be called through it",
                        p.name
                    ),
                );
            }
        }
        for (ns, order) in &project.templates.dispatch {
            for n in order {
                if n != &project.name && !project.templates.packages.iter().any(|p| &p.name == n) {
                    self.diags.error(
                        Code::InvalidField,
                        Some(PathBuf::from(PROJECT_FILE)),
                        None,
                        format!("`dispatch` for `{ns}` searches `{n}`, which is neither this project nor an installed package"),
                    );
                }
            }
        }
    }

    pub(super) fn parse_folder_config(
        &mut self,
        file: &Path,
        tree: Option<de::Located<Loose<config::project::Folder>>>,
        folders: &[Vec<String>],
    ) -> BTreeMap<Vec<String>, FolderCfg> {
        let mut out = BTreeMap::new();
        let Some(tree) = tree else {
            return out;
        };
        let display = Some(file.to_path_buf());
        let mut stack = vec![(Vec::<String>::new(), tree)];
        while let Some((path, node)) = stack.pop() {
            let line = node.line();
            let Loose::Ok(f) = node.value else {
                self.diags.error(
                    Code::InvalidFolderConfig,
                    display.clone(),
                    line,
                    format!("folder config for `{}` must be a map", dotted(&path)),
                );
                continue;
            };
            let any = f.sets_anything();
            let mut cfg = FolderCfg::default();
            if let Some(meta) = f.meta {
                let value = self.typed_metadata(file, Some(meta), "+meta");
                cfg.meta = Some(MetaLayer {
                    file: crate::slash(file).to_string_lossy().into_owned(),
                    key: if path.is_empty() {
                        "reports.+meta".into()
                    } else {
                        format!("reports.{}.+meta", path.join("."))
                    },
                    meta: value,
                });
            }
            let bad = |s: &mut Self, k: &str, kline: Option<usize>, what: &str| {
                s.diags.error(
                    Code::InvalidField,
                    display.clone(),
                    kline,
                    format!("`{k}` must be {what}"),
                )
            };
            if let Some(v) = f.tags {
                match v.value {
                    Loose::Ok(t) => cfg.tags = t,
                    Loose::Bad(_) => bad(self, "+tags", v.line(), "a list of strings"),
                }
            }
            if let Some(v) = f.output {
                match v.value {
                    Loose::Ok(o) => cfg.output = Some(o),
                    Loose::Bad(_) => bad(self, "+output", v.line(), "a map"),
                }
            }
            if let Some(v) = f.profile {
                let kline = v.line();
                match v.value {
                    Loose::Ok(p) => cfg.profile = Some((p, kline)),
                    Loose::Bad(_) => bad(self, "+profile", kline, "a string"),
                }
            }
            if let Some(v) = f.schedule {
                let ctx = format!("folder `{}`: `+schedule`", dotted(&path));
                self.moved_to_schedules(file, v.line(), &ctx);
            }
            if let Some(v) = f.timezone {
                let kline = v.line();
                cfg.timezone =
                    self.timezone_str(v.value.ok().map(String::as_str), file, kline, "`+timezone`");
            }
            if let Some(v) = f.locale {
                let kline = v.line();
                cfg.locale = self.locale_str(v.value.ok().map(String::as_str), file, kline, "`+locale`");
            }
            if let Some(v) = f.vars {
                match v.value {
                    Loose::Ok(s) => cfg.vars = Some(s),
                    Loose::Bad(_) => bad(self, "+vars", v.line(), "a map"),
                }
            }
            for (k, v) in f.rest.0 {
                if k.value.starts_with('+') {
                    self.diags.error(
                        Code::UnknownKey,
                        display.clone(),
                        k.line(),
                        format!(
                            "unknown folder config `{}`; folder config keys are {}",
                            k.value,
                            FOLDER_CONFIG_KEYS.join(", ")
                        ),
                    );
                } else {
                    let mut p = path.clone();
                    p.push(k.value);
                    stack.push((p, v));
                }
            }
            if any && !path.is_empty() {
                if !folders.contains(&path) {
                    self.diags.warning(
                        Code::UnknownFolder,
                        display.clone(),
                        line,
                        format!(
                            "folder config for `{}` matches no folder under reports/",
                            dotted(&path)
                        ),
                    );
                }
                out.insert(path, cfg);
            } else if any {
                out.insert(path, cfg);
            }
        }
        out
    }
}
