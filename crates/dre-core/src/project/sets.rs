//! Sets: the `sets.yml` registry and each report's `sets:`, resolved into Bindings.

use super::*;

impl Loader {
    pub(super) fn parse_sets(&mut self, files: &[Rc<YamlFile>]) -> BTreeMap<String, SetDef> {
        let mut out = BTreeMap::new();
        let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
        for yf in files {
            let Ok(Loose::Ok(sets)) = de::from_node::<Loose<config::schedule::SetsFile>>(&yf.node) else {
                continue;
            };
            for (k, v) in sets.0.0 {
                let line = k.line();
                let name = k.value;
                if let Some(prev) = seen.get(&name) {
                    self.diags.error(
                        Code::DuplicateSet,
                        Some(yf.display.clone()),
                        line,
                        format!("Set `{name}` is also declared in {}", prev.display()),
                    );
                    continue;
                }
                seen.insert(name.clone(), yf.display.clone());
                // Anything but a map is a Set with the report's defaults.
                let v = match v {
                    Loose::Ok(Some(v)) => Some(v),
                    _ => None,
                };
                let profile = v
                    .as_ref()
                    .and_then(|v| v.profile.as_ref())
                    .and_then(Loose::ok)
                    .cloned();
                if v.as_ref().is_some_and(|v| v.profile.is_some()) && profile.is_none() {
                    self.diags.error(
                        Code::InvalidField,
                        Some(yf.display.clone()),
                        line,
                        format!("Set `{name}`: `profile` must be a string"),
                    );
                }
                let locale = match v.as_ref().and_then(|v| v.locale.as_ref()) {
                    None => None,
                    Some(l) => self.locale_str(
                        l.ok().map(String::as_str),
                        &yf.display,
                        line,
                        &format!("Set `{name}`: `locale`"),
                    ),
                };
                let vars = match v.as_ref().and_then(|v| v.vars.as_ref()) {
                    Some(Loose::Ok(m)) => m.clone(),
                    None => JsonMap::new(),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidField,
                            Some(yf.display.clone()),
                            line,
                            format!("Set `{name}`: `vars` must be a map"),
                        );
                        JsonMap::new()
                    }
                };
                out.insert(
                    name,
                    SetDef {
                        profile,
                        vars,
                        locale,
                        file: yf.display.clone(),
                        line,
                    },
                );
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_sets(
        &mut self,
        report: &str,
        sets: &de::Located<Loose<Vec<SetItem>>>,
        display: &Path,
        queries: &[QueryEntry],
        base: &BindingBase,
        project: &Project,
        used: &mut Usage,
    ) -> Vec<Binding> {
        let file = Some(display.to_path_buf());
        let sets_line = sets.line();
        let Loose::Ok(items) = &sets.value else {
            self.diags.error(
                Code::InvalidField,
                file,
                sets_line,
                format!("report `{report}`: `sets` must be a list"),
            );
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for item in items {
            let line = item.line();
            let (name, inline): (String, Option<&SetEntry>) = match &item.value {
                Loose::Ok(de::OneOf::A(s)) => (s.clone(), None),
                Loose::Ok(de::OneOf::B(m)) => match &m.name {
                    Some(Loose::Ok(n)) => (n.clone(), Some(m)),
                    _ => {
                        self.diags.error(
                            Code::InvalidField,
                            file.clone(),
                            line,
                            format!("report `{report}`: a `sets:` map entry needs a `name:`"),
                        );
                        continue;
                    }
                },
                Loose::Bad(_) => {
                    self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("report `{report}`: `sets:` entries must be names or maps"),
                    );
                    continue;
                }
            };
            if !seen.insert(name.clone()) {
                self.diags.error(
                    Code::DuplicateSet,
                    file.clone(),
                    line,
                    format!("report `{report}` lists Set `{name}` more than once"),
                );
                continue;
            }
            let registry = project.sets.get(&name);
            if inline.is_none() && registry.is_none() {
                self.diags.error(
                    Code::UnknownSet,
                    file.clone(),
                    line,
                    format!("report `{report}`: Set `{name}` isn't declared in sets.yml"),
                );
                continue;
            }
            let mut b = BindingBase {
                profile: base.profile.clone(),
                profile_at: base.profile_at.clone(),
                vars: base.vars.clone(),
                outputs: base.outputs.clone(),
                locale: base.locale.clone(),
            };
            if let Some(reg) = registry {
                if let Some(p) = &reg.profile {
                    b.profile = Some(p.clone());
                    b.profile_at = Some(ProfileAt {
                        file: reg.file.clone(),
                        line: reg.line,
                        key: format!("`profile` of Set `{name}`"),
                    });
                    used.connection(p, None, None);
                }
                b.vars.extend(reg.vars.clone());
                if reg.locale.is_some() {
                    b.locale = reg.locale.clone();
                }
            }
            let mut qs = queries.to_vec();
            let mut tab_names: Option<Mapping> = None;
            if let Some(m) = inline {
                let ctx = format!("report `{report}`, Set `{name}`");
                for k in &m.unknown.0 {
                    self.diags.error(
                        Code::UnknownKey,
                        file.clone(),
                        Some(k.line),
                        format!("{ctx}: unknown key `{}`", k.name),
                    );
                }
                if let Some(p) = &m.profile {
                    let pline = p.line();
                    match &p.value {
                        Loose::Ok(p) => {
                            b.profile = Some(p.clone());
                            b.profile_at = Some(ProfileAt {
                                file: display.to_path_buf(),
                                line: pline,
                                key: format!("`profile` of Set `{name}`"),
                            });
                            used.connection(p, file.clone(), pline);
                        }
                        Loose::Bad(_) => self.diags.error(
                            Code::InvalidField,
                            file.clone(),
                            pline,
                            format!("{ctx}: `profile` must be a string"),
                        ),
                    }
                }
                match &m.vars {
                    Some(Loose::Ok(v)) => b.vars.extend(v.clone()),
                    None => {}
                    Some(Loose::Bad(_)) => self.diags.error(
                        Code::InvalidField,
                        file.clone(),
                        line,
                        format!("{ctx}: `vars` must be a map"),
                    ),
                }
                if let Some(o) = &m.output {
                    for p in layer_outputs(&mut b.outputs, o) {
                        self.diags
                            .error(Code::InvalidField, file.clone(), line, format!("{ctx}: {p}"));
                    }
                }
                if m.schedule.is_some() {
                    self.moved_to_schedules(display, line, &format!("{ctx}: `schedule`"));
                }
                if let Some(l) = &m.locale
                    && let Some(l) = self.locale_str(
                        l.ok().map(String::as_str),
                        display,
                        line,
                        &format!("{ctx}: `locale`"),
                    )
                {
                    b.locale = Some(l);
                }
                let listed: Vec<&str> = queries.iter().map(|q| q.query.as_str()).collect();
                match (&m.exclude, &m.queries) {
                    (Some(_), Some(_)) => self.diags.error(
                        Code::ExcludeAndQueries,
                        file.clone(),
                        line,
                        format!("{ctx}: use either `exclude:` or `queries:`, not both"),
                    ),
                    (Some(ex), None) => match &ex.value {
                        Loose::Ok(names) => {
                            for q in names {
                                if !listed.contains(&q.as_str()) {
                                    self.diags.error(
                                        Code::UnknownQuery,
                                        file.clone(),
                                        ex.line(),
                                        format!("{ctx}: `exclude` names `{q}`, which isn't in the report's `queries:`"),
                                    );
                                }
                            }
                            qs.retain(|q| !names.contains(&q.query));
                        }
                        Loose::Bad(_) => self.diags.error(
                            Code::InvalidField,
                            file.clone(),
                            line,
                            format!("{ctx}: `exclude` must be a list of query names"),
                        ),
                    },
                    (None, Some(ov)) => match &ov.value {
                        Loose::Ok(items) => {
                            let mut sub = Vec::new();
                            for it in items {
                                let (qn, settings) = match it {
                                    Loose::Ok(de::OneOf::A(s)) => (s.clone(), None),
                                    Loose::Ok(de::OneOf::B(e)) => {
                                        match e.query.as_ref().and_then(Loose::ok) {
                                            Some(q) => (q.clone(), Some(e)),
                                            None => continue,
                                        }
                                    }
                                    Loose::Bad(_) => continue,
                                };
                                match queries.iter().find(|q| q.query == qn) {
                                    Some(q) => {
                                        let mut q = q.clone();
                                        if let Some(e) = settings {
                                            match &e.tab_name {
                                                None => {}
                                                Some(Loose::Ok(s)) => q.tab_name = Some(s.clone()),
                                                Some(Loose::Bad(_)) => self.diags.error(
                                                    Code::InvalidField,
                                                    file.clone(),
                                                    line,
                                                    format!("{ctx}: {}", one_tab_per_file(&qn)),
                                                ),
                                            }
                                            if let Some(Loose::Ok(b)) = &e.tab {
                                                q.tab = *b;
                                            }
                                            match &e.on_empty {
                                                None => {}
                                                Some(Loose::Ok(v)) => q.on_empty = Some(*v),
                                                Some(Loose::Bad(_)) => self.diags.error(
                                                    Code::InvalidField,
                                                    file.clone(),
                                                    line,
                                                    format!(
                                                        "{ctx}: `on_empty` of `{qn}` must be one of {}",
                                                        crate::no_data::OnEmpty::NAMES.join(", ")
                                                    ),
                                                ),
                                            }
                                            match &e.secret_columns {
                                                None => {}
                                                Some(Loose::Ok(v)) => {
                                                    for c in v {
                                                        if !q.secret_columns.contains(c) {
                                                            q.secret_columns.push(c.clone());
                                                        }
                                                    }
                                                }
                                                Some(Loose::Bad(_)) => self.diags.error(
                                                    Code::InvalidField,
                                                    file.clone(),
                                                    line,
                                                    format!("{ctx}: `secret_columns` of `{qn}` must be a list of column names"),
                                                ),
                                            }
                                        }
                                        sub.push(q);
                                    }
                                    None => self.diags.error(
                                        Code::UnknownQuery,
                                        file.clone(),
                                        ov.line(),
                                        format!("{ctx}: `queries` names `{qn}`, which isn't in the report's `queries:`"),
                                    ),
                                }
                            }
                            qs = sub;
                        }
                        Loose::Bad(_) => self.diags.error(
                            Code::InvalidField,
                            file.clone(),
                            line,
                            format!("{ctx}: `queries` must be a list"),
                        ),
                    },
                    (None, None) => {}
                }
                tab_names = match &m.tab_names {
                    None => None,
                    Some(Loose::Ok(t)) => Some(t.clone()),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidField,
                            file.clone(),
                            line,
                            format!("{ctx}: `tab_names` must be a map of query name to tab name"),
                        );
                        None
                    }
                };
            }
            let bind = self.finish_binding(report, Some(name), &b, tab_names.as_ref(), qs, display, used);
            out.push(bind);
        }
        out
    }

    /// Resolve a Binding without recording diagnostics or usage (they're reported elsewhere).
    pub(super) fn silent_binding(
        &mut self,
        report: &str,
        base: &BindingBase,
        queries: &[QueryEntry],
        file: &Path,
    ) -> Binding {
        let saved = std::mem::take(&mut self.diags);
        let mut scratch = Usage::default();
        let b = self.finish_binding(report, None, base, None, queries.to_vec(), file, &mut scratch);
        self.diags = saved;
        b
    }
}

/// Every entry is a Set: a map of `profile`/`vars`, which may be empty (`plain: {}`, the
/// report's defaults) or left blank.
pub(super) fn is_set_registry(m: &Mapping) -> bool {
    m.values().any(Value::is_object)
        && m.values().all(|v| {
            v.is_null()
                || v.as_object()
                    .is_some_and(|e| e.keys().all(|k| is_one_of(k, &["profile", "vars", "locale"])))
        })
}

#[cfg(test)]
mod tests {
    use super::super::{test_loader, test_yaml};
    use super::*;

    #[test]
    fn reads_a_sets_registry_and_refuses_a_set_declared_twice() {
        let mut l = test_loader(Path::new("/p"));
        let sets = l.parse_sets(&[
            test_yaml("sets.yml", "a: {profile: duck, vars: {region: AU}}\nplain: {}\n"),
            test_yaml("more/sets.yml", "a: {vars: {region: NZ}}\n"),
        ]);
        assert_eq!(sets.keys().collect::<Vec<_>>(), vec!["a", "plain"]);
        assert_eq!(sets["a"].profile.as_deref(), Some("duck"));
        assert_eq!(sets["a"].file, PathBuf::from("sets.yml"));
        assert!(l.diags.has_errors());
    }

    #[test]
    fn a_registry_is_known_by_its_shape() {
        let map = |s: &str| {
            test_yaml("x.yml", s)
                .value
                .as_object()
                .cloned()
                .unwrap_or_default()
        };
        assert!(is_set_registry(&map("a: {vars: {x: 1}}\nb:\n")));
        assert!(!is_set_registry(&map("a: {cron: '0 6 * * *'}")));
        assert!(!is_set_registry(&map("queries: [a]")));
    }
}
