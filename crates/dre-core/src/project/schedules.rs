//! Schedules and timings: `schedules.yml` and `timings.yml`.

use super::*;

impl Loader {
    /// Schedules live only in schedules.yml (named, with vars); anywhere else is an error.
    pub(super) fn moved_to_schedules(&mut self, file: &Path, line: Option<usize>, ctx: &str) {
        self.diags.error(
            Code::ScheduleMoved,
            Some(file.to_path_buf()),
            line,
            format!(
                "{ctx} is no longer supported; declare schedules in schedules.yml as named entries (`name`, `report:`/`select:`, `cron`/`every`/`rrule`, optional `vars`)"
            ),
        );
    }

    /// timings.yml: named timings. Returns the valid ones, and the names of those with errors
    /// (schedules using them aren't reported again).
    pub(super) fn parse_timings(
        &mut self,
        files: &[Rc<YamlFile>],
    ) -> (BTreeMap<String, Timing>, BTreeSet<String>) {
        let mut out = BTreeMap::new();
        let mut broken = BTreeSet::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for yf in files {
            let Ok(Loose::Ok(timings)) = de::from_node::<Loose<config::schedule::TimingsFile>>(&yf.node)
            else {
                continue;
            };
            for (k, v) in timings.0.0 {
                let file = Some(yf.display.clone());
                let line = k.line();
                let name = k.value;
                let mut ok = true;
                if !is_identifier(&name) {
                    self.diags.error(
                        Code::InvalidTiming,
                        file.clone(),
                        line,
                        format!(
                            "timing name `{name}` must be letters, digits and `_`, not starting with a digit"
                        ),
                    );
                    ok = false;
                }
                if let Some(prev) = seen.get(&name) {
                    self.diags.error(
                        Code::DuplicateTimingName,
                        file.clone(),
                        line,
                        format!("timing `{name}` is already declared at {prev}; timing names must be unique"),
                    );
                    continue;
                }
                seen.insert(
                    name.clone(),
                    format!("{}:{}", yf.display.display(), line.unwrap_or(0)),
                );
                let Loose::Ok(t) = v else {
                    self.diags.error(
                        Code::InvalidTiming,
                        file.clone(),
                        line,
                        format!("timing `{name}` must be a map, e.g. `{{cron: \"0 6 1 * *\", timezone: Australia/Sydney}}`"),
                    );
                    broken.insert(name);
                    continue;
                };
                for k in &t.unknown.0 {
                    self.diags.error(
                        Code::InvalidTiming,
                        file.clone(),
                        line,
                        format!("timing `{name}`: unknown key `{}`", k.name),
                    );
                    ok = false;
                }
                let block = t.block();
                let shape = schedule::validate_block(&block, "a timing", "`cron`, `every` or `rrule`");
                for e in &shape {
                    self.diags.error(
                        Code::InvalidTiming,
                        file.clone(),
                        line,
                        format!("timing `{name}`: {e}"),
                    );
                    ok = false;
                }
                let timezone = match &t.timezone {
                    None => None,
                    Some(v) => {
                        let tz = self.timezone_str(
                            v.ok().map(String::as_str),
                            &yf.display,
                            line,
                            &format!("timing `{name}`: `timezone`"),
                        );
                        ok &= tz.is_some();
                        tz
                    }
                };
                if shape.is_empty() {
                    for (code, msg) in schedule::strictness(&block) {
                        self.diags
                            .error(code, file.clone(), line, format!("timing `{name}`: {msg}"));
                    }
                    if let Some(msg) = schedule::no_time(&block) {
                        self.diags.warning(
                            Code::ScheduleNoTime,
                            file.clone(),
                            line,
                            format!("timing `{name}`: {msg}"),
                        );
                    }
                }
                if ok {
                    out.insert(
                        name,
                        Timing {
                            schedule: block,
                            timezone,
                            location: (yf.display.clone(), line),
                        },
                    );
                } else {
                    broken.insert(name);
                }
            }
        }
        (out, broken)
    }

    pub(super) fn parse_schedules(
        &mut self,
        files: &[Rc<YamlFile>],
        project: &Project,
        broken_timings: &BTreeSet<String>,
    ) -> Vec<ScheduleEntry> {
        let mut out = Vec::new();
        let mut used_timings = BTreeSet::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for yf in files {
            let Ok(Loose::Ok(items)) = de::from_node::<Loose<config::schedule::SchedulesFile>>(&yf.node)
            else {
                continue;
            };
            for item in &items.0 {
                // Schedules files are recognised by every item being a map with a name or target.
                let Loose::Ok(m) = &item.value else { continue };
                let line = item.line();
                let file = Some(yf.display.clone());
                let s = |v: &Option<Loose<String>>| v.as_ref().and_then(Loose::ok).cloned();
                let (select, report, set) = (s(&m.select), s(&m.report), s(&m.set));
                let mut ok = true;
                let name = match &m.name {
                    Some(Loose::Ok(n)) if is_identifier(n) => {
                        if let Some(prev) = seen.get(n) {
                            self.diags.error(
                                Code::DuplicateScheduleName,
                                file.clone(),
                                line,
                                format!("schedule `{n}` is already declared at {prev}; schedule names must be unique"),
                            );
                            ok = false;
                        }
                        seen.insert(
                            n.clone(),
                            format!("{}:{}", yf.display.display(), line.unwrap_or(0)),
                        );
                        n.clone()
                    }
                    Some(Loose::Ok(n)) => {
                        self.diags.error(
                            Code::InvalidSchedule,
                            file.clone(),
                            line,
                            format!("schedule name `{n}` must be letters, digits and `_`, not starting with a digit"),
                        );
                        ok = false;
                        n.clone()
                    }
                    _ => {
                        self.diags.error(
                            Code::InvalidSchedule,
                            file.clone(),
                            line,
                            "every schedule needs a `name`",
                        );
                        ok = false;
                        String::new()
                    }
                };
                let vars = match &m.vars {
                    None => JsonMap::new(),
                    Some(Loose::Ok(v)) => v.clone(),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidSchedule,
                            file.clone(),
                            line,
                            format!("schedule `{name}`: `vars` must be a map"),
                        );
                        ok = false;
                        JsonMap::new()
                    }
                };
                for k in &m.unknown.0 {
                    self.diags.error(
                        Code::InvalidSchedule,
                        file.clone(),
                        line,
                        format!("unknown schedule key `{}`", k.name),
                    );
                }
                let sched = m.block();
                let timing = match &m.timing {
                    None => None,
                    Some(Loose::Ok(t)) => {
                        used_timings.insert(t.clone());
                        Some(t.clone())
                    }
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidSchedule,
                            file.clone(),
                            line,
                            format!(
                                "schedule `{name}`: `timing` must be the name of a timing in timings.yml"
                            ),
                        );
                        ok = false;
                        None
                    }
                };
                let mut resolved = None;
                let shape = if let Some(t) = &timing {
                    let mut errs = Vec::new();
                    let own: Vec<String> = m.timing_keys().into_iter().map(|k| format!("`{k}`")).collect();
                    if !own.is_empty() {
                        let keys = match own.split_last() {
                            Some((last, [])) => last.clone(),
                            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                            None => String::new(),
                        };
                        errs.push(format!(
                            "schedule `{name}` uses timing `{t}`, so it can't set {keys} too; change the timing, or give the schedule its own timing instead"
                        ));
                    }
                    match project.timings.get(t) {
                        Some(def) => resolved = Some(def.schedule.clone()),
                        None if broken_timings.contains(t) => ok = false,
                        None => {
                            let names: Vec<&str> = project.timings.keys().map(String::as_str).collect();
                            let valid = if names.is_empty() {
                                "the project has no timings.yml entries".to_string()
                            } else {
                                format!("valid names: {}", names.join(", "))
                            };
                            self.diags.error(
                                Code::UnknownTiming,
                                file.clone(),
                                line,
                                format!("schedule `{name}`: no timing `{t}`; {valid}"),
                            );
                            ok = false;
                        }
                    }
                    errs
                } else {
                    schedule::validate_block(&sched, "a schedule", "`timing`, `cron`, `every` or `rrule`")
                };
                if shape.is_empty() && timing.is_none() {
                    let block = &sched;
                    for (code, msg) in schedule::strictness(block) {
                        self.diags
                            .error(code, file.clone(), line, format!("schedule `{name}`: {msg}"));
                    }
                    if let Some(msg) = schedule::no_time(block) {
                        self.diags.warning(
                            Code::ScheduleNoTime,
                            file.clone(),
                            line,
                            format!("schedule `{name}`: {msg}"),
                        );
                    }
                }
                for e in shape {
                    self.diags.error(Code::InvalidSchedule, file.clone(), line, e);
                    ok = false;
                }
                if select.is_some() && report.is_some() {
                    self.diags.error(
                        Code::InvalidSchedule,
                        file.clone(),
                        line,
                        "use either `select:` or `report:`, not both",
                    );
                    ok = false;
                }
                if select.is_some() && set.is_some() {
                    self.diags.error(
                        Code::InvalidSchedule,
                        file.clone(),
                        line,
                        "`set:` only applies with `report:`",
                    );
                    ok = false;
                }
                if let Some(sel) = &select {
                    match selector::resolve(project, sel) {
                        Ok(r) if r.is_empty() => {
                            self.diags.error(
                                Code::SelectorMatchesNothing,
                                file.clone(),
                                line,
                                format!("selector `{sel}` matches no report"),
                            );
                            ok = false;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            self.diags.error(e.code(), file.clone(), line, e.to_string());
                            ok = false;
                        }
                    }
                }
                if let Some(rn) = &report {
                    match project.report(rn) {
                        None => {
                            self.diags.error(
                                Code::SelectorMatchesNothing,
                                file.clone(),
                                line,
                                format!("report `{rn}` doesn't exist"),
                            );
                            ok = false;
                        }
                        Some(r) => {
                            if let Some(sn) = &set
                                && r.binding(sn).is_none()
                            {
                                self.diags.error(
                                    Code::SelectorMatchesNothing,
                                    file.clone(),
                                    line,
                                    format!("report `{rn}` has no Set `{sn}`"),
                                );
                                ok = false;
                            }
                        }
                    }
                }
                if select.is_none() && report.is_none() {
                    self.diags.error(
                        Code::InvalidSchedule,
                        file.clone(),
                        line,
                        format!("schedule `{name}` needs `report:` (optionally with `set:`) or `select:`"),
                    );
                    ok = false;
                }
                let timezone = match (&m.timezone, &timing) {
                    (None, Some(t)) => project.timings.get(t).and_then(|d| d.timezone.clone()),
                    (None, None) => None,
                    (Some(v), _) => {
                        let t = self.timezone_str(
                            v.value.ok().map(String::as_str),
                            &yf.display,
                            line,
                            &format!("schedule `{name}`: `timezone`"),
                        );
                        ok &= t.is_some();
                        t
                    }
                };
                let enabled = match &m.enabled {
                    None => true,
                    Some(Loose::Ok(b)) => *b,
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidSchedule,
                            file.clone(),
                            line,
                            format!("schedule `{name}`: `enabled` must be true or false"),
                        );
                        ok = false;
                        true
                    }
                };
                if ok {
                    out.push(ScheduleEntry {
                        name,
                        select,
                        report,
                        set,
                        schedule: resolved.unwrap_or(sched),
                        timing,
                        enabled,
                        vars,
                        timezone,
                        location: (yf.display.clone(), line),
                    });
                }
            }
        }
        for (name, t) in &project.timings {
            if !used_timings.contains(name) {
                self.diags.warning(
                    Code::UnusedTiming,
                    Some(t.location.0.clone()),
                    t.location.1,
                    format!("timing `{name}` isn't used by any schedule"),
                );
            }
        }
        out
    }

    /// Warn when two schedules of one Binding would deliver to the same path: each schedule's
    /// paths are rendered with its vars and one fixed date. Paths that can't render offline (a
    /// `run_query()`, a missing `env_var()`) are skipped.
    fn check_schedule_paths(&mut self, project: &Project) {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("January 1 is a valid date");
        for report in &project.reports {
            for b in report.bindings.iter().filter(|b| b.schedules.len() > 1) {
                let paths: Vec<&String> = b.destinations().filter_map(|d| d.path.as_ref()).collect();
                if paths.is_empty() {
                    continue;
                }
                let mut rendered: Vec<(&String, Vec<String>)> = Vec::new();
                for name in &b.schedules {
                    let Some(e) = project.schedules.iter().find(|e| &e.name == name) else {
                        continue;
                    };
                    let mut vars = b.vars.clone();
                    vars.extend(e.vars.clone());
                    let Ok(r) = crate::render::Renderer::new(crate::render::RendererConfig {
                        root: &project.root,
                        project_name: &project.name,
                        templates: &project.templates,
                        context: crate::render::RunContext {
                            report: report.name.clone(),
                            set: b.set.clone(),
                            target: project.target_name.clone(),
                            schedule: Some(name.clone()),
                            date,
                            now: chrono::Utc::now(),
                            scheduled_at: None,
                            calendar: crate::dates::Calendar::default(),
                            locale: Default::default(),
                        },
                        vars,
                        cli_vars: self.opts.vars.clone(),
                        runner: None,
                        connections: None,
                        mode: crate::render::Mode::Run,
                        connection: None,
                        source_type: String::new(),
                        sources: None,
                        process: self.ctx.clone(),
                    }) else {
                        continue;
                    };
                    let out: Result<Vec<String>, _> =
                        paths.iter().map(|p| r.render(&report.file, p)).collect();
                    if let Ok(out) = out {
                        rendered.push((name, out));
                    }
                }
                for (i, (a, pa)) in rendered.iter().enumerate() {
                    for (bn, pb) in &rendered[i + 1..] {
                        if let Some(same) = pa.iter().find(|p| pb.contains(p)) {
                            self.diags.warning(
                                Code::SchedulePathClash,
                                Some(report.file.clone()),
                                None,
                                format!(
                                    "schedules `{a}` and `{bn}` both run report `{}`{} and deliver to `{same}`; the second overwrites the first — put a schedule var or `run.schedule` in the path",
                                    report.name,
                                    b.set.as_ref().map(|s| format!(", Set `{s}`,")).unwrap_or_default()
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Attach every schedules.yml entry to the Bindings it targets: `report:` + `set:` one
    /// Binding, `report:` alone all of that report's Bindings, `select:` all Bindings of every
    /// report it matches. Several schedules per Binding is the point, not a conflict.
    pub(super) fn apply_schedules(&mut self, project: &mut Project) {
        let entries = project.schedules.clone();
        for e in &entries {
            let reports: Vec<String> = match (&e.select, &e.report) {
                (Some(sel), _) => selector::resolve(project, sel)
                    .map(|r| r.into_iter().map(|r| r.name.clone()).collect())
                    .unwrap_or_default(),
                (_, Some(r)) => vec![r.clone()],
                _ => Vec::new(),
            };
            for report in project.reports.iter_mut().filter(|r| reports.contains(&r.name)) {
                for b in &mut report.bindings {
                    if e.set.is_none() || e.set == b.set {
                        b.schedules.push(e.name.clone());
                    }
                }
            }
        }
        self.check_schedule_paths(project);
        self.check_schedule_timezones(project);
    }

    /// Warn when a schedule fires in one timezone and a report it runs renders in another: the
    /// run date is then the report's date at the firing time, which may not be the day meant.
    fn check_schedule_timezones(&mut self, project: &Project) {
        for e in project.schedules.iter().filter(|e| e.timezone.is_none()) {
            let fires = crate::occurrences::firing_tz(project, e);
            let mut seen = BTreeSet::new();
            for (report, _) in crate::occurrences::bindings(project, &e.name) {
                let Some(r) = project.report(report) else { continue };
                let renders = r
                    .timezone
                    .as_ref()
                    .and_then(|t| crate::dates::parse_tz(t).ok())
                    .unwrap_or(chrono_tz::Tz::UTC);
                if renders != fires && seen.insert(report) {
                    self.diags.warning(
                        Code::ScheduleTimezoneMismatch,
                        Some(e.location.0.clone()),
                        e.location.1,
                        format!(
                            "schedule `{}` fires in {} but report `{report}` renders in {}, so its run date is {}'s date at the firing time; set `timezone:` on the schedule to fire and render in one timezone",
                            e.name,
                            fires.name(),
                            renders.name(),
                            renders.name()
                        ),
                    );
                }
            }
        }
    }
}

/// A map of names to timings: every value holds `cron`, `every` or `rrule`.
pub(super) fn is_timing_registry(m: &Mapping) -> bool {
    !m.is_empty()
        && m.values().all(|v| {
            v.as_object()
                .is_some_and(|t| ["cron", "every", "rrule"].iter().any(|k| t.contains_key(*k)))
        })
}
