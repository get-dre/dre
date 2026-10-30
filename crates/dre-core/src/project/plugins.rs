//! Profiles and plugins: which profiles and plugin packages the project uses, checked against what's declared.

use super::*;

impl Loader {
    pub(super) fn check_profiles(&mut self, project: &Project, used: &Usage) {
        let profiles = &project.profiles;
        if used.connections.is_empty() && used.destinations.is_empty() {
            return;
        }
        if !profiles.exists() {
            self.diags.error(
                Code::ProfilesMissing,
                None,
                None,
                if profiles.found_by == "~/.dre" {
                    format!(
                        "the project references profiles, but no profiles.yml was found in the project directory or at {}",
                        profiles.path.display()
                    )
                } else {
                    format!(
                        "the project references profiles, but no profiles.yml was found at {} (from {})",
                        profiles.path.display(),
                        profiles.found_by
                    )
                },
            );
            return;
        }
        for (role, refs) in [
            (Role::Connection, &used.connections),
            (Role::Destination, &used.destinations),
        ] {
            for (name, (file, line)) in refs {
                if role == Role::Destination && name == LOCAL_TYPE {
                    continue;
                }
                if !profiles.declares(role, name) {
                    self.diags.error(
                        Code::UnknownProfile,
                        file.clone(),
                        *line,
                        format!(
                            "{} profile `{name}` isn't defined under `{}:` in {}",
                            role.as_str(),
                            profiles.section_key(role),
                            profiles.path.display()
                        ),
                    );
                }
            }
        }
    }

    /// A `plugins:` entry in its map form: `{name: foo, github: acme/dre-foo, version: "^1"}`.
    fn plugin_entry(
        &mut self,
        m: &config::dependencies::PluginEntry,
        file: &Path,
        line: Option<usize>,
    ) -> Option<(String, Option<Value>, PluginSource)> {
        let file = Some(file.to_path_buf());
        let name = m.name.ok().cloned().unwrap_or_default();
        let err = |s: &mut Self, msg: String| {
            s.diags.error(
                Code::InvalidPluginDeclaration,
                file.clone(),
                line,
                format!("plugin package `{name}`: {msg}"),
            );
        };
        if name.is_empty() {
            err(self, "`name` must be a non-empty string".into());
            return None;
        }
        if let Some(k) = m.unknown.0.first() {
            err(
                self,
                format!(
                    "unknown key `{}`; use `name`, `version` and one of `github`, `local`, `registry`",
                    k.name
                ),
            );
            return None;
        }
        let given: Vec<(&str, &str)> = [
            ("github", &m.github),
            ("local", &m.local),
            ("registry", &m.registry),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k, v.as_str().unwrap_or(""))))
        .collect();
        let source = match given.as_slice() {
            [] => PluginSource::Default,
            [(k, "")] => {
                err(self, format!("`{k}` must be a non-empty string"));
                return None;
            }
            [("github", r)] => {
                let ok = r.split('/').count() == 2 && r.split('/').all(|p| !p.is_empty());
                if !ok {
                    err(self, format!("`github: {r}` must be `owner/repo`"));
                    return None;
                }
                PluginSource::Github(r.to_string())
            }
            [("local", p)] => {
                if m.version.is_some() {
                    err(
                        self,
                        "a `local` package has no `version`: it's used as it is".into(),
                    );
                    return None;
                }
                PluginSource::Local(p.to_string())
            }
            [(_, u)] => PluginSource::Registry(u.to_string()),
            _ => {
                err(self, "give only one of `github`, `local` and `registry`".into());
                return None;
            }
        };
        Some((name, m.version.clone(), source))
    }

    /// The `plugins:` declarations, merged across files; and every plugin the project uses, for
    /// [`crate::plugins::check_uses`] to check against what the declared packages provide.
    pub(super) fn check_plugins(
        &mut self,
        decls: &[(Rc<YamlFile>, Mapping)],
        project: &mut Project,
        used: &Usage,
    ) {
        struct Decl {
            req: semver::VersionReq,
            raw: String,
            file: PathBuf,
            source: PluginSource,
        }
        let mut by_package: BTreeMap<String, Vec<Decl>> = BTreeMap::new();
        for (yf, _) in decls {
            {
                let Ok(config::dependencies::PluginsKey { plugins: Some(v) }) = de::from_node(&yf.node)
                else {
                    continue;
                };
                let line = v.line();
                let file = Some(yf.display.clone());
                let entries: Vec<(String, Option<Value>, PluginSource, Option<usize>)> = match v.value {
                    Loose::Ok(de::OneOf::A(items)) => items
                        .into_iter()
                        .filter_map(|i| {
                            let iline = i.line();
                            match i.value {
                                Loose::Ok(de::OneOf::A(name)) => Some((name.0, None, PluginSource::Default, iline)),
                                Loose::Ok(de::OneOf::B(de::OneOf::B(entry))) => self
                                    .plugin_entry(&entry, &yf.display, iline)
                                    .map(|(n, c, s)| (n, c, s, iline)),
                                Loose::Ok(de::OneOf::B(de::OneOf::A(pin))) => Some((
                                    pin.name.value,
                                    Some(pin.version),
                                    PluginSource::Default,
                                    iline,
                                )),
                                Loose::Bad(_) => {
                                    self.diags.error(
                                        Code::InvalidPluginDeclaration,
                                        file.clone(),
                                        iline,
                                        "each `plugins` entry is a package name, `name: \"<version>\"`, or a map with `name:` and one of `github:`, `local:`, `registry:`",
                                    );
                                    None
                                }
                            }
                        })
                        .collect(),
                    Loose::Ok(de::OneOf::B(m)) => m
                        .0
                        .into_iter()
                        .map(|(k, v)| {
                            let kline = k.line();
                            (k.value, Some(v), PluginSource::Default, kline)
                        })
                        .collect(),
                    Loose::Bad(f) if f.kind == "nothing" => Vec::new(),
                    Loose::Bad(_) => {
                        self.diags.error(
                            Code::InvalidPluginDeclaration,
                            file.clone(),
                            line,
                            "`plugins` must be a list like `- duckdb: \">=1.0\"`",
                        );
                        continue;
                    }
                };
                for (name, c, source, eline) in entries {
                    if !dre_protocol::valid_name(&name) {
                        self.diags.error(
                            Code::InvalidPluginDeclaration,
                            file.clone(),
                            eline,
                            format!(
                                "plugin package `{name}`: a package name is lowercase letters, digits and `_`"
                            ),
                        );
                        continue;
                    }
                    let raw = match &c {
                        None | Some(Value::Null) => "*".to_string(),
                        Some(v) => crate::yaml::scalar_str(v).unwrap_or_default(),
                    };
                    match semver::VersionReq::parse(&raw) {
                        Ok(req) => by_package.entry(name).or_default().push(Decl {
                            req,
                            raw,
                            file: yf.display.clone(),
                            source,
                        }),
                        Err(e) => self.diags.error(
                            Code::InvalidVersionConstraint,
                            file.clone(),
                            eline,
                            format!("plugin package `{name}`: invalid version constraint `{raw}`: {e}"),
                        ),
                    }
                }
            }
        }
        let mut out = Vec::new();
        for (name, ds) in &by_package {
            // Every declaration of one package must agree on where it comes from.
            let source = ds
                .iter()
                .map(|d| &d.source)
                .find(|s| !s.is_default())
                .cloned()
                .unwrap_or_default();
            if let Some(other) = ds.iter().find(|d| !d.source.is_default() && d.source != source) {
                self.diags.error(
                    Code::ConflictingPluginSources,
                    Some(other.file.clone()),
                    None,
                    format!(
                        "plugin package `{name}` is declared with two sources: {} and {}",
                        source.lock_key().unwrap_or_default(),
                        other.source.lock_key().unwrap_or_default()
                    ),
                );
                project.plugins_incomplete = true;
                continue;
            }
            let reqs: Vec<&semver::VersionReq> = ds.iter().map(|d| &d.req).collect();
            if !constraints::compatible(&reqs) {
                for (i, a) in ds.iter().enumerate() {
                    for b in &ds[i + 1..] {
                        if !constraints::compatible(&[&a.req, &b.req]) {
                            self.diags.error(
                                Code::ConflictingPluginConstraints,
                                Some(b.file.clone()),
                                None,
                                format!(
                                    "plugin package `{name}` is declared `{}` in {} and `{}` in {}; no version satisfies both",
                                    a.raw,
                                    a.file.display(),
                                    b.raw,
                                    b.file.display()
                                ),
                            );
                        }
                    }
                }
                project.plugins_incomplete = true;
                continue;
            }
            let combined = constraints::combine(&reqs);
            let mut files: Vec<PathBuf> = ds.iter().map(|d| d.file.clone()).collect();
            files.dedup();
            out.push(PluginRequirement {
                name: name.clone(),
                version: combined.to_string(),
                declared_in: files,
                source,
            });
        }
        project.plugins = out;

        let mut uses = Vec::new();
        let profiles = &project.profiles;
        for (role, kind, refs) in [
            (Role::Connection, PluginKind::Source, &used.connections),
            (Role::Destination, PluginKind::Destination, &used.destinations),
        ] {
            for (name, (file, line)) in refs {
                let Some(p) = profiles.get(role, name) else {
                    continue;
                };
                let role_name = role.as_str();
                for out_ in p.targets.values() {
                    if kind == PluginKind::Destination && out_.kind == LOCAL_TYPE {
                        continue;
                    }
                    let id = PluginId::new(kind, out_.kind.clone());
                    if uses.iter().any(|u: &PluginUse| u.plugin == id) {
                        continue;
                    }
                    uses.push(PluginUse {
                        plugin: id,
                        file: if self.opts.select.is_some() || self.opts.schedule.is_some() {
                            file.clone().or_else(|| profiles.file.clone())
                        } else { profiles.file.clone() },
                        line: if self.opts.select.is_some() || self.opts.schedule.is_some() {
                            *line
                        } else { profiles.line_of(role, name) },
                        what: format!("`type: {}` used by {role_name} profile `{name}`", out_.kind),
                    });
                }
            }
        }
        for (fmt, (ctx, file)) in &used.formats {
            let note = if fmt == crate::formats::DEFAULT {
                " (csv is the built-in default output)"
            } else {
                ""
            };
            uses.push(PluginUse {
                plugin: PluginId::new(PluginKind::Format, fmt.clone()),
                file: Some(file.clone()),
                line: None,
                what: format!("format `{fmt}` is used by {ctx}{note}"),
            });
        }
        project.plugin_uses = uses;
    }

    /// `sources:`, `formats:` or `destinations:` where plugins were once declared.
    pub(super) fn old_plugin_key(&mut self, yf: &YamlFile, key: &str) {
        self.diags.error(
            Code::MovedPluginDeclaration,
            Some(yf.display.clone()),
            yf.line_of(key, None),
            format!(
                "`{key}:` no longer declares plugins: list plugin packages under `plugins:` instead (e.g. `plugins: [duckdb, object_store]`)"
            ),
        );
    }
}

/// Everything the project references, for profile and plugin checks.
#[derive(Default)]
pub(super) struct Usage {
    connections: BTreeMap<String, (Option<PathBuf>, Option<usize>)>,
    destinations: BTreeMap<String, (Option<PathBuf>, Option<usize>)>,
    /// format → first user, for messages.
    formats: BTreeMap<String, (String, PathBuf)>,
}

impl Usage {
    pub(super) fn include_project_wide(&mut self, other: &Self) {
        for (name, at) in &other.connections {
            if at.0.as_deref() == Some(Path::new(PROJECT_FILE)) {
                self.connections.insert(name.clone(), at.clone());
            }
        }
        for (name, at) in &other.destinations {
            if at.0.as_deref() == Some(Path::new(PROJECT_FILE)) {
                self.destinations.insert(name.clone(), at.clone());
            }
        }
    }

    /// A connection profile used by name. A value holding Jinja is recorded once the parse pass
    /// has rendered it.
    pub(super) fn connection(&mut self, p: &str, file: Option<PathBuf>, line: Option<usize>) {
        if preflight::is_templated(p) {
            return;
        }
        let e = self.connections.entry(p.to_string()).or_insert((None, None));
        if e.0.is_none() {
            *e = (file, line);
        }
    }
    pub(super) fn destination(&mut self, p: &str, file: Option<PathBuf>, line: Option<usize>) {
        if preflight::is_templated(p) {
            return;
        }
        let e = self.destinations.entry(p.to_string()).or_insert((None, None));
        if e.0.is_none() {
            *e = (file, line);
        }
    }
    pub(super) fn format(&mut self, f: &str, ctx: &str, file: &Path) {
        self.formats
            .entry(f.to_string())
            .or_insert_with(|| (ctx.to_string(), file.to_path_buf()));
    }
}
