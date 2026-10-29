//! Outputs: layering `output:` config, reading it as typed config, and checking it.

use super::*;

impl Loader {
    /// Every merged output, typed, plus the checks across them: unique names, `queries:` naming
    /// the Binding's queries, xlsx-only `columns`, and queries that feed no output.
    pub(super) fn typed_outputs(
        &mut self,
        maps: &[Mapping],
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
        used: &mut Usage,
    ) -> Vec<Output> {
        let at = Some(file.to_path_buf());
        let several = maps.len() > 1;
        let mut outputs = Vec::new();
        for (i, m) in maps.iter().enumerate() {
            let octx = if several {
                format!(
                    "{ctx}, {}",
                    output_label(m.get("name").and_then(Value::as_str), i)
                )
            } else {
                ctx.to_string()
            };
            let subset: Vec<QueryEntry> = match m.get("queries") {
                None | Some(Value::Null) => queries.to_vec(),
                Some(v) => {
                    match string_list(v) {
                        Some(names) => {
                            for n in names.iter().filter(|n| !queries.iter().any(|q| &&q.query == n)) {
                                self.diags.error(
                                Code::UnknownQuery,
                                at.clone(),
                                None,
                                format!("{octx}: `queries` names `{n}`, which isn't one of this Binding's queries"),
                            );
                            }
                            queries
                                .iter()
                                .filter(|q| names.contains(&q.query))
                                .cloned()
                                .collect()
                        }
                        None => {
                            self.diags.error(
                                Code::InvalidField,
                                at.clone(),
                                None,
                                format!("{octx}: `queries` must be a list of query names"),
                            );
                            queries.to_vec()
                        }
                    }
                }
            };
            outputs.push(self.typed_output(m, &octx, file, &subset, used));
        }
        let mut seen = BTreeSet::new();
        for o in &outputs {
            if let Some(n) = &o.name
                && !seen.insert(n.clone())
            {
                self.diags.error(
                    Code::DuplicateOutput,
                    at.clone(),
                    None,
                    format!("{ctx}: output `{n}` is declared twice; output names must be unique"),
                );
            }
        }
        // Two unnamed outputs of one format would write the same default file.
        let mut exts = BTreeSet::new();
        for o in outputs.iter().filter(|o| o.name.is_none()) {
            if !exts.insert(Files::of(o).extension()) {
                self.diags.error(
                    Code::DuplicateOutput,
                    at.clone(),
                    None,
                    format!(
                        "{ctx}: two unnamed `{}` outputs would write the same file; give each output a `name:`",
                        o.format
                    ),
                );
            }
        }
        for q in queries.iter().filter(|q| !q.columns.is_empty()) {
            let formats: Vec<Format<'_>> = outputs
                .iter()
                .filter(|o| o.feeds(&q.query))
                .map(|o| Format::new(&o.format))
                .collect();
            if !formats.is_empty() && !formats.iter().any(|f| f.is_workbook()) {
                self.diags.error(
                    Code::InvalidField,
                    at.clone(),
                    None,
                    format!(
                        "{ctx}: `columns` on query `{}` only applies to the xlsx format",
                        q.query
                    ),
                );
            }
        }
        self.check_password_lists(&outputs, ctx, file, queries);
        self.check_zip_names(&outputs, ctx, file, queries);
        for (i, o) in outputs.iter().enumerate() {
            let octx = if several {
                format!("{ctx}, {}", o.label(i))
            } else {
                ctx.to_string()
            };
            self.check_output_templates(o, &outputs, &octx, file, queries);
            self.check_password_columns(o, &octx, file, queries);
            for d in o
                .destinations
                .iter()
                .filter(|d| d.zip.is_some() && o.is_message())
            {
                self.diags.error(
                    Code::InvalidDestinationOption,
                    at.clone(),
                    None,
                    format!(
                        "{octx}: destination `{}`: a message can't be zipped; zip the files on a file output's entry, and attach that output's files to the message instead",
                        d.profile
                    ),
                );
            }
            for d in o.destinations.iter().filter(|d| !d.attach.is_empty()) {
                for a in &d.attach {
                    let problem = match outputs.iter().find(|x| x.name.as_deref() == Some(a.as_str())) {
                        _ if !o.is_message() && d.zip.is_none() => Some(
                            "`attach` only applies to a message output, or to an entry with `zip:` (a pack)"
                                .to_string(),
                        ),
                        None => Some(format!("no output of this report is named `{a}`")),
                        Some(x) if std::ptr::eq(x, o) => Some(format!("`{a}` is this output itself")),
                        Some(x) if x.is_message() && d.zip.is_some() => Some(format!(
                            "`{a}` is a message, which can't go in a zip; attach file outputs"
                        )),
                        Some(x) if x.is_message() => Some(format!(
                            "`{a}` is a message; attach file outputs (its `.md` is linked with `outputs.{a}.location`)"
                        )),
                        Some(x) => match (&o.for_each, &x.for_each) {
                            (None, Some(_)) => Some(format!(
                                "`{a}` makes a file per row; attach it from a message with the same `for_each`, so each row's message gets that row's file"
                            )),
                            (Some(m), Some(f)) if m.query != f.query || m.key != f.key => Some(format!(
                                "`{a}` makes a file per row of `{}` by `{}`, but this message is per row of `{}` by `{}`; give them the same `for_each` driver and key so each row's message gets its own file",
                                f.query,
                                f.key.join(", "),
                                m.query,
                                m.key.join(", ")
                            )),
                            _ => None,
                        },
                    };
                    if let Some(p) = problem {
                        self.diags.error(
                            Code::InvalidDestinationOption,
                            at.clone(),
                            None,
                            format!("{octx}: destination `{}`: `attach: {a}`: {p}", d.profile),
                        );
                    }
                }
            }
        }
        for q in queries.iter().filter(|q| q.tab) {
            if !outputs.iter().any(|o| o.feeds(&q.query)) {
                self.diags.warning(
                    Code::UnusedQuery,
                    at.clone(),
                    None,
                    format!(
                        "{ctx}: query `{}` feeds no output; add it to an output's `queries:`, or give it `tab: false` if it only prepares later queries",
                        q.query
                    ),
                );
            }
        }
        outputs
    }

    /// A zipping entry's zip is flat: two of its files with one name are an error. The names
    /// are known here for outputs that aren't per row (theirs render per row).
    fn check_zip_names(&mut self, outputs: &[Output], ctx: &str, file: &Path, queries: &[QueryEntry]) {
        let report = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The names `run` gives the output's files, from the same catalog.
        let files_of = |o: &Output| -> Option<Vec<String>> {
            if o.for_each.is_some() || o.is_message() {
                return None;
            }
            let path = o
                .destinations
                .iter()
                .filter(|d| d.zip.is_none())
                .find_map(|d| d.path.as_deref());
            if path
                .and_then(|p| p.rsplit(['/', '\\']).next())
                .is_some_and(crate::preflight::is_templated)
            {
                return None;
            }
            let stem = o.name.as_deref().unwrap_or(&report);
            let files = Files::of(o);
            let base = files.base_name(stem, None, path);
            let tabs: Vec<&str> = queries
                .iter()
                .filter(|q| q.tab && o.feeds(&q.query))
                .map(|q| q.tab_name.as_deref().unwrap_or(&q.query))
                .collect();
            let plan = files.plan(&base, &tabs).ok()?;
            Some(plan.into_iter().map(|(name, _)| name).collect())
        };
        for (i, o) in outputs.iter().enumerate() {
            for d in o.destinations.iter().filter(|d| d.zip.is_some()) {
                let Some(mut names) = files_of(o) else { continue };
                let mut known = true;
                for a in &d.attach {
                    match outputs
                        .iter()
                        .find(|x| x.name.as_deref() == Some(a.as_str()))
                        .and_then(files_of)
                    {
                        Some(n) => names.extend(n),
                        None => known = false,
                    }
                }
                if !known {
                    continue;
                }
                if let Some(n) = crate::formats::first_clash(names.iter().map(String::as_str)) {
                    self.diags.error(
                        Code::InvalidDestinationOption,
                        Some(file.to_path_buf()),
                        None,
                        format!(
                            "{ctx}, {}: destination `{}`: two files named `{n}` would go into the zip (it's flat, and nothing is renamed); give the outputs different `name:`s",
                            o.label(i),
                            d.profile
                        ),
                    );
                }
            }
        }
    }

    /// Warn when a file output holding a `secret_columns` column (a file → password list) goes
    /// to a destination profile that also receives the password-protected files it unlocks.
    fn check_password_lists(&mut self, outputs: &[Output], ctx: &str, file: &Path, queries: &[QueryEntry]) {
        let protected = |o: &Output| o.options.get("password").is_some();
        let mut locked: Vec<&str> = Vec::new();
        for o in outputs {
            for d in &o.destinations {
                if d.zip.as_ref().is_some_and(|z| z.password.is_some()) {
                    locked.push(&d.profile);
                }
            }
            let carries_protected = protected(o)
                || (o.is_message()
                    && o.destinations.iter().any(|d| {
                        d.attach.iter().any(|a| {
                            outputs
                                .iter()
                                .any(|x| x.name.as_deref() == Some(a.as_str()) && protected(x))
                        })
                    }));
            if carries_protected {
                locked.extend(o.destinations.iter().map(|d| d.profile.as_str()));
            }
        }
        for o in outputs.iter().filter(|o| !o.is_message() && !protected(o)) {
            let secret = queries
                .iter()
                .filter(|q| !q.secret_columns.is_empty() && o.feeds(&q.query))
                .map(|q| q.query.as_str())
                .collect::<Vec<_>>();
            if secret.is_empty() {
                continue;
            }
            for d in o
                .destinations
                .iter()
                .filter(|d| d.zip.as_ref().is_none_or(|z| z.password.is_none()))
            {
                // `local` keeps files on this machine: nothing travels together.
                if d.profile != crate::profiles::LOCAL_TYPE && locked.contains(&d.profile.as_str()) {
                    self.diags.warning(
                        Code::InvalidOutputOption,
                        Some(file.to_path_buf()),
                        None,
                        format!(
                            "{ctx}: output `{}` holds secret columns of `{}` and goes to destination `{}`, which also receives the password-protected files: send the password list somewhere else",
                            o.name.as_deref().unwrap_or(&o.format),
                            secret.join("`, `"),
                            d.profile
                        ),
                    );
                }
            }
        }
    }

    /// Warn when a password (`password:`, `zip: {password}`) reads `each.<column>` of a column
    /// the driver query doesn't mark in `secret_columns`: it would show in logs and results.
    fn check_password_columns(&mut self, o: &Output, ctx: &str, file: &Path, queries: &[QueryEntry]) {
        // The `each.<column>`s a template reads.
        let each_columns = |scan: &preflight::Scan| -> Vec<String> {
            scan.paths("each")
                .filter_map(|p| p.attr(0).map(str::to_string))
                .collect()
        };
        let secret_of = |q: &str| -> Vec<&String> {
            queries
                .iter()
                .filter(|e| e.query == q)
                .flat_map(|e| &e.secret_columns)
                .collect()
        };
        let warn = |s: &mut Self, msg: String| {
            s.diags.warning(
                Code::InvalidOutputOption,
                Some(file.to_path_buf()),
                None,
                format!("{ctx}: {msg}"),
            )
        };
        let mut passwords: Vec<(&str, &str)> = Vec::new();
        if let Some(pw) = o.options.get("password").and_then(Json::as_str) {
            passwords.push(("`password`", pw));
        }
        for d in &o.destinations {
            if let Some(pw) = d.zip.as_ref().and_then(|z| z.password.as_deref()) {
                passwords.push(("`zip.password`", pw));
            }
        }
        for (what, pw) in passwords {
            if pw.is_empty() {
                continue;
            }
            if !crate::preflight::is_templated(pw) {
                warn(
                    self,
                    format!(
                        "{what} is written in the YAML, where anyone who reads the project sees it; read it with `env_var()`, or from a query column in `secret_columns`"
                    ),
                );
                continue;
            }
            let scan = preflight::Scan::new(pw);
            if scan.called_names().iter().any(|n| n == "var") {
                warn(
                    self,
                    format!(
                        "{what} reads `var()`: vars show in the run's log and results; read it with `env_var()`, or from a query column in `secret_columns`"
                    ),
                );
            }
            if let Some(fe) = &o.for_each {
                let secret = secret_of(&fe.query);
                for col in each_columns(&scan) {
                    if !secret.iter().any(|s| **s == col) {
                        warn(
                            self,
                            format!(
                                "{what} reads `each.{col}`, which isn't in `secret_columns` of query `{}`: its values would show in logs and results; add `secret_columns: [{col}]` to the query entry",
                                fe.query
                            ),
                        );
                    }
                }
            }
            for (q, col) in scan.paths("results").filter_map(result_column) {
                let secret = secret_of(q);
                let col = col.filter(|c| !matches!(*c, "value" | "row_count"));
                let covered = match col {
                    Some(col) => secret.iter().any(|s| s.as_str() == col),
                    None => !secret.is_empty(),
                };
                if !covered {
                    let which = col.map_or_else(|| "its password column".to_string(), |c| format!("`{c}`"));
                    warn(
                        self,
                        format!(
                            "{what} reads `results.{q}`, but {which} isn't in `secret_columns` of query `{q}`: its values would show in logs and results; list it in the query entry's `secret_columns`"
                        ),
                    );
                }
            }
        }
        // A password-like driver column a message or path prints, not marked secret.
        if let Some(fe) = &o.for_each {
            let secret = secret_of(&fe.query);
            let mut texts: Vec<&str> = ["text", "title"]
                .iter()
                .filter_map(|k| o.options.get(*k).and_then(Json::as_str))
                .collect();
            texts.extend(o.destinations.iter().filter_map(|d| d.path.as_deref()));
            let mut seen = Vec::new();
            for t in texts {
                for col in each_columns(&preflight::Scan::new(t)) {
                    let lower = col.to_ascii_lowercase();
                    let looks_secret = ["password", "passwd", "pwd", "passcode", "pin", "secret"]
                        .iter()
                        .any(|w| {
                            lower == *w
                                || lower.starts_with(&format!("{w}_"))
                                || lower.ends_with(&format!("_{w}"))
                        });
                    if looks_secret && !secret.iter().any(|s| **s == col) && !seen.contains(&col) {
                        warn(
                            self,
                            format!(
                                "`each.{col}` looks like a password but isn't in `secret_columns` of query `{}`: its values would show in logs and results; add `secret_columns: [{col}]` to the query entry",
                                fe.query
                            ),
                        );
                        seen.push(col);
                    }
                }
            }
        }
    }

    fn check_output_templates(
        &mut self,
        o: &Output,
        all: &[Output],
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
    ) {
        let at = Some(file.to_path_buf());
        let err = |s: &mut Self, msg: String| {
            s.diags.error(
                Code::InvalidOutputOption,
                at.clone(),
                None,
                format!("{ctx}: {msg}"),
            )
        };
        // (what, source) of every template to check.
        let mut templates: Vec<(String, String)> = Vec::new();
        if let Some(w) = &o.when {
            templates.push(("`when`".into(), format!("{{% if {w} %}}{{% endif %}}")));
        }
        if o.is_message() {
            for k in o.options.keys() {
                if !crate::message::MESSAGE_KEYS.contains(&k.as_str()) {
                    err(
                        self,
                        format!(
                            "`{k}` isn't an option of the `message` format; its options are {}",
                            crate::message::MESSAGE_KEYS.join(", ")
                        ),
                    );
                }
            }
            if o.options.contains_key("text") && o.options.contains_key("file") {
                err(self, "use either `text:` or `file:`, not both".into());
            }
            for k in ["text", "title"] {
                match o.options.get(k) {
                    None => {}
                    Some(Json::String(t)) => templates.push((format!("`{k}`"), t.clone())),
                    Some(_) => err(self, format!("`{k}` must be a string")),
                }
            }
            match o.options.get("file") {
                None => {}
                Some(Json::String(f)) => match find_template(&self.root, f).map(std::fs::read_to_string) {
                    Some(Ok(t)) => templates.push((format!("`file` {f}"), t)),
                    Some(Err(e)) => err(self, format!("can't read message file `{f}`: {e}")),
                    None => err(
                        self,
                        format!(
                            "message file `{f}` doesn't exist (looked in the project root and templates/)"
                        ),
                    ),
                },
                Some(_) => err(self, "`file` must be a path (a string)".into()),
            }
            match o.options.get("max_rows") {
                None => {}
                Some(v) if v.as_u64().is_some_and(|n| n > 0) => {}
                Some(_) => err(self, "`max_rows` must be a positive whole number".into()),
            }
        }
        let fed: Vec<&str> = queries
            .iter()
            .filter(|q| q.tab && o.feeds(&q.query))
            .map(|q| q.query.as_str())
            .collect();
        for (what, src) in &templates {
            if let Err((line, msg)) = preflight::check_syntax("output", src) {
                let line = line.map(|l| format!(" (line {l})")).unwrap_or_default();
                err(self, format!("{what} doesn't compile{line}: {msg}"));
                continue;
            }
            let scan = preflight::Scan::new(src);
            for (name, _) in scan.attributes("results") {
                if !fed.contains(&name.as_str()) {
                    err(
                        self,
                        format!(
                            "{what} reads `results.{name}`, but `{name}` isn't one of this output's queries ({})",
                            if fed.is_empty() {
                                "none".to_string()
                            } else {
                                fed.join(", ")
                            }
                        ),
                    );
                }
            }
            if !o.is_message() && !scan.attributes("outputs").is_empty() {
                err(
                    self,
                    format!(
                        "{what} reads `outputs.*`, but a file output's `when` is decided before anything is delivered; use `outputs.*` in a message output"
                    ),
                );
                continue;
            }
            for (name, _) in scan.attributes("outputs") {
                let other = all.iter().find(|x| x.name.as_deref() == Some(name.as_str()));
                match other {
                    None => err(
                        self,
                        format!(
                            "{what} reads `outputs.{name}`, but no output of this report is named `{name}`"
                        ),
                    ),
                    Some(x) if std::ptr::eq(x, o) => err(
                        self,
                        format!("{what} reads `outputs.{name}`, which is this output itself"),
                    ),
                    Some(_) => {}
                }
            }
        }
    }

    /// Convert a merged output map into a typed `Output`, validating options and references.
    fn typed_output(
        &mut self,
        m: &Mapping,
        ctx: &str,
        file: &Path,
        queries: &[QueryEntry],
        used: &mut Usage,
    ) -> Output {
        let file_path = file.to_path_buf();
        let file = Some(file_path.clone());
        let node = crate::yaml::to_node(&Value::Object(m.clone()));
        let o: OutputConfig = match de::from_node(&node) {
            Ok(o) => o,
            // Every key is `Loose`, so a map always reads.
            Err(e) => unreachable!("an output map reads as an output: {e}"),
        };
        let format = o
            .format
            .as_ref()
            .and_then(Loose::ok)
            .cloned()
            .unwrap_or_else(|| crate::formats::DEFAULT.to_string());
        // `message` is built in; every other format is a plugin.
        if !Format::new(&format).is_message() {
            used.format(&format, ctx, &file_path);
        }
        let mut opts: JsonMap<String, Json> = o.options.0.into_iter().map(|(k, v)| (k.value, v)).collect();
        // The project's defaults for this format sit under whatever the layers set.
        if let Some(Value::Object(d)) = self.format_options.get(format.as_str()) {
            for (k, v) in d {
                if !OUTPUT_SHARED_KEYS.contains(&k.as_str()) {
                    opts.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        let destinations = match o.destination {
            None => Vec::new(),
            Some(Loose::Ok(de::OneOf::A(d))) => self
                .typed_destination(d, ctx, file.as_deref(), used)
                .into_iter()
                .collect(),
            Some(Loose::Ok(de::OneOf::B(list))) if list.is_empty() => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    None,
                    format!(
                        "{ctx}: `output.destination` is an empty list; name at least one destination, or remove it to keep the output in target/"
                    ),
                );
                Vec::new()
            }
            Some(Loose::Ok(de::OneOf::B(list))) => {
                let mut out = Vec::new();
                for (i, d) in list.into_iter().enumerate() {
                    match d {
                        Loose::Ok(d) => out.extend(self.typed_destination(d, ctx, file.as_deref(), used)),
                        Loose::Bad(_) => self.diags.error(
                            Code::InvalidField,
                            file.clone(),
                            None,
                            format!("{ctx}: `output.destination` entry {} must be a map", i + 1),
                        ),
                    }
                }
                out
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    None,
                    format!("{ctx}: `output.destination` must be a map or a list of maps"),
                );
                Vec::new()
            }
        };
        let template = match o.template {
            None => None,
            Some(t) => {
                if !Format::new(&format).takes_template() {
                    self.diags.error(
                        Code::InvalidOutputOption,
                        file.clone(),
                        None,
                        format!("{ctx}: `output.template` only applies to the xlsx, html and pdf formats"),
                    );
                }
                let t = self.typed_template(t, ctx, file.as_deref(), queries);
                if !Format::new(&format).is_workbook() && t.as_ref().is_some_and(|t| !t.bindings.is_empty()) {
                    self.diags.error(
                        Code::InvalidTemplate,
                        file.clone(),
                        None,
                        format!(
                            "{ctx}: `output.template.bindings` is for xlsx templates; an {format} template reads `results.<query>` itself"
                        ),
                    );
                }
                t
            }
        };
        let extension = match o.extension {
            de::Maybe::Absent => None,
            de::Maybe::Given(Loose::Ok(None | Some(de::OneOf::B(false)))) => Some(String::new()),
            de::Maybe::Given(Loose::Ok(Some(de::OneOf::A(e)))) => {
                let e = e.strip_prefix('.').unwrap_or(&e).to_string();
                if e.contains(['/', '\\']) || e.chars().any(char::is_whitespace) {
                    self.diags.error(
                        Code::InvalidOutputOption,
                        file.clone(),
                        None,
                        format!(
                            "{ctx}: `extension` must be a file extension like `aba` (no path, no spaces)"
                        ),
                    );
                }
                Some(e)
            }
            de::Maybe::Given(_) => {
                self.diags.error(
                    Code::InvalidOutputOption,
                    file.clone(),
                    None,
                    format!("{ctx}: `extension` must be a string (`aba`), or `\"\"`/`false` for none"),
                );
                None
            }
        };
        let name = match o.name {
            None => None,
            Some(Loose::Ok(n)) if is_identifier(&n) => Some(n),
            Some(_) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    None,
                    format!("{ctx}: output `name` must be an identifier (letters, digits, `_`)"),
                );
                None
            }
        };
        let when = match o.when {
            None => None,
            Some(Loose::Ok(de::OneOf::A(w))) => Some(w),
            Some(Loose::Ok(de::OneOf::B(b))) => Some(b.to_string()),
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.clone(),
                    None,
                    format!("{ctx}: `when` must be a Jinja expression (a string)"),
                );
                None
            }
        };
        let entries: &[QueryEntry] = queries;
        let queries = match &o.queries {
            Some(Loose::Ok(_)) => Some(queries.iter().map(|q| q.query.clone()).collect()),
            _ => None,
        };
        if extension.is_some() && Format::new(&format).is_workbook() {
            self.diags.error(
                Code::InvalidOutputOption,
                file.clone(),
                None,
                format!("{ctx}: `extension` doesn't apply to xlsx (Excel only opens .xlsx workbooks)"),
            );
        }
        let on_empty = match o.on_empty {
            None => None,
            Some(Loose::Ok(v)) => Some(v),
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidOutputOption,
                    file.clone(),
                    None,
                    format!(
                        "{ctx}: `on_empty` must be one of {}",
                        crate::no_data::OnEmpty::NAMES.join(", ")
                    ),
                );
                None
            }
        };
        // A document or message template decides its sections: a query can't drop one.
        if Format::new(&format).template_decides_sections() {
            for q in entries.iter().filter(|q| {
                q.on_empty == Some(crate::no_data::OnEmpty::Skip)
                    && output_feeds(o.queries.as_ref(), &q.query)
            }) {
                self.diags.warning(
                    Code::InvalidOutputOption,
                    file.clone(),
                    None,
                    format!(
                        "{ctx}: query `{}` has `on_empty: skip`, which has no effect on a {format} output (its template decides what to show); use `on_empty` on the output, or `results.{}.row_count` in the template",
                        q.query, q.query
                    ),
                );
            }
        }
        let for_each = match o.for_each {
            None | Some(Loose::Ok(de::OneOf::B(false))) => None,
            Some(Loose::Ok(de::OneOf::B(true))) | Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidOutputOption,
                    file.clone(),
                    None,
                    format!("{ctx}: `for_each` must be a map with `query` and `key`, or `false` to turn an inherited one off"),
                );
                None
            }
            Some(Loose::Ok(de::OneOf::A(f))) => {
                self.typed_for_each(f, ctx, file.as_deref(), entries, &format, &destinations)
            }
        };
        let file_name = match o.file_name {
            None => None,
            Some(Loose::Ok(s)) if for_each.is_some() => Some(s),
            Some(Loose::Ok(_)) => {
                self.diags.error(
                    Code::InvalidOutputOption,
                    file.clone(),
                    None,
                    format!("{ctx}: `file_name` names a per-row output's files; it needs `for_each`"),
                );
                None
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidOutputOption,
                    file.clone(),
                    None,
                    format!("{ctx}: `file_name` must be a string"),
                );
                None
            }
        };
        Output {
            name,
            format,
            queries,
            when,
            options: opts,
            destinations,
            template,
            extension,
            on_empty,
            for_each,
            file_name,
        }
    }

    /// `for_each:`: its driver is a query of the Binding that makes a result, its key is set,
    /// and every destination with a `path` names each row's own (`each.*`).
    fn typed_for_each(
        &mut self,
        f: config::report::ForEach,
        ctx: &str,
        file: Option<&Path>,
        queries: &[QueryEntry],
        format: &str,
        destinations: &[Destination],
    ) -> Option<ForEach> {
        let err = |s: &mut Self, msg: String| {
            s.diags.error(
                Code::InvalidOutputOption,
                file.map(Path::to_path_buf),
                None,
                format!("{ctx}: {msg}"),
            );
        };
        let names: Vec<&str> = queries
            .iter()
            .filter(|q| q.tab)
            .map(|q| q.query.as_str())
            .collect();
        let query = match f.query {
            Some(Loose::Ok(q)) if names.contains(&q.as_str()) => q,
            Some(Loose::Ok(q)) => {
                err(
                    self,
                    format!(
                        "`for_each.query` `{q}` isn't a query of this report that returns rows (its queries: {})",
                        names.join(", ")
                    ),
                );
                return None;
            }
            _ => {
                err(
                    self,
                    "`for_each` needs `query:`, the driver query (one row per copy of the output)".into(),
                );
                return None;
            }
        };
        let key = match f.key {
            Some(Loose::Ok(de::OneOf::A(k))) if !k.trim().is_empty() => vec![k],
            Some(Loose::Ok(de::OneOf::B(list)))
                if !list.is_empty() && list.iter().all(|k| !k.trim().is_empty()) =>
            {
                list
            }
            _ => {
                err(
                    self,
                    "`for_each` needs `key:`, the column (or list of columns) that identifies a row".into(),
                );
                return None;
            }
        };
        for k in &key {
            let lower = k.to_lowercase();
            if ["email", "mail", "name", "phone", "mobile"]
                .iter()
                .any(|w| lower.contains(w))
            {
                self.diags.warning(
                    Code::InvalidOutputOption,
                    file.map(Path::to_path_buf),
                    None,
                    format!(
                        "{ctx}: the `for_each` key `{k}` looks like personal data; keys are never masked (they name files and appear in logs), so use an opaque id such as a member number"
                    ),
                );
            }
        }
        let positive = |s: &mut Self, v: Option<Loose<u64>>, what: &str| -> Option<u64> {
            match v {
                None => None,
                Some(Loose::Ok(n)) if n > 0 => Some(n),
                Some(_) => {
                    err(
                        s,
                        format!("`for_each.{what}` must be a whole number of 1 or more"),
                    );
                    None
                }
            }
        };
        let concurrency = positive(self, f.concurrency, "concurrency").map(|n| n as usize);
        let max_population_rows = positive(self, f.max_population_rows, "max_population_rows")
            .unwrap_or(DEFAULT_MAX_POPULATION_ROWS);
        let flag = |s: &mut Self, v: Option<Loose<bool>>, what: &str| -> Option<bool> {
            match v {
                None => None,
                Some(Loose::Ok(b)) => Some(b),
                Some(Loose::Bad(_)) => {
                    err(s, format!("`for_each.{what}` must be true or false"));
                    None
                }
            }
        };
        let keep_results = flag(self, f.keep_results, "keep_results").unwrap_or(true);
        let mask_row_values = flag(self, f.mask_row_values, "mask_row_values");
        let on_row_error = match f.on_row_error {
            None => RowErrors::default(),
            Some(Loose::Ok(v)) => v,
            Some(Loose::Bad(_)) => {
                err(
                    self,
                    "`for_each.on_row_error` must be `continue` or `fail`".into(),
                );
                RowErrors::default()
            }
        };
        let on_orphan_rows = match f.on_orphan_rows {
            None => OrphanRows::default(),
            Some(Loose::Ok(v)) => v,
            Some(Loose::Bad(_)) => {
                err(self, "`for_each.on_orphan_rows` must be `warn` or `fail`".into());
                OrphanRows::default()
            }
        };
        for d in destinations {
            if let Some(p) = &d.path
                && !mentions_each(p)
            {
                err(
                    self,
                    format!(
                        "destination `{}`: a per-row output's `path` must differ per row, but `{p}` doesn't use `each.*`; every row would overwrite the last (add e.g. `{{{{ each.{} }}}}`)",
                        // `key` is never empty: an empty one is rejected where it's parsed.
                        d.profile,
                        key[0]
                    ),
                );
            }
        }
        let _ = format;
        Some(ForEach {
            query,
            key,
            concurrency,
            on_row_error,
            on_orphan_rows,
            max_population_rows,
            keep_results,
            mask_row_values,
        })
    }

    /// One `output.destination` entry: `profile`, optional `path`, and plugin options.
    fn typed_destination(
        &mut self,
        d: config::report::Destination,
        ctx: &str,
        file: Option<&Path>,
        used: &mut Usage,
    ) -> Option<Destination> {
        let Some(Loose::Ok(p)) = d.profile else {
            self.diags.error(
                Code::InvalidField,
                file.map(Path::to_path_buf),
                None,
                format!("{ctx}: `output.destination` needs a `profile:` naming a profiles.yml entry"),
            );
            return None;
        };
        let path = match d.path {
            None => None,
            Some(Loose::Ok(s)) => Some(s),
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    None,
                    format!("{ctx}: destination `{p}`: `path` must be a string"),
                );
                None
            }
        };
        used.destination(&p, file.map(Path::to_path_buf), None);
        let options = d.options.0.into_iter().map(|(k, v)| (k.value, v)).collect();
        let attach = match d.attach {
            None => Vec::new(),
            Some(Loose::Ok(de::OneOf::A(s))) => vec![s],
            Some(Loose::Ok(de::OneOf::B(list))) => list,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    None,
                    format!("{ctx}: destination `{p}`: `attach` must be an output name or a list of them"),
                );
                Vec::new()
            }
        };
        let hold = match d.hold {
            None => false,
            Some(Loose::Ok(b)) => b,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    None,
                    format!("{ctx}: destination `{p}`: `hold` must be true or false"),
                );
                false
            }
        };
        let mut rate = match d.rate {
            None => None,
            Some(Loose::Ok(s)) => match crate::rate::Rate::parse(&s) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.diags.error(
                        Code::InvalidField,
                        file.map(Path::to_path_buf),
                        None,
                        format!("{ctx}: destination `{p}`: `rate`: {e}"),
                    );
                    None
                }
            },
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    None,
                    format!("{ctx}: destination `{p}`: `rate` must be a count and a unit, like `100/minute`"),
                );
                None
            }
        };
        match (d.burst, rate.as_mut()) {
            (None, _) => {}
            (Some(Loose::Ok(n)), Some(r)) if n > 0 => r.burst = n,
            (Some(Loose::Ok(_)), None) => self.diags.error(
                Code::InvalidField,
                file.map(Path::to_path_buf),
                None,
                format!("{ctx}: destination `{p}`: `burst` needs a `rate`"),
            ),
            (Some(_), _) => self.diags.error(
                Code::InvalidField,
                file.map(Path::to_path_buf),
                None,
                format!("{ctx}: destination `{p}`: `burst` must be a whole number of 1 or more"),
            ),
        }
        let zip = match d.zip {
            None | Some(Loose::Ok(de::OneOf::A(false))) => None,
            Some(Loose::Ok(de::OneOf::A(true))) => Some(crate::zip_pack::ZipSpec {
                password: None,
                encryption: Default::default(),
            }),
            Some(Loose::Ok(de::OneOf::B(z))) => {
                for k in &z.unknown.0 {
                    self.diags.error(
                        Code::UnknownKey,
                        file.map(Path::to_path_buf),
                        Some(k.line),
                        format!("{ctx}: destination `{p}`: unknown key `{}` in `zip`; expected `password`, `encryption`", k.name),
                    );
                }
                let password = match z.password {
                    None => None,
                    Some(Loose::Ok(s)) if s.is_empty() => {
                        self.diags.error(
                            Code::InvalidField,
                            file.map(Path::to_path_buf),
                            None,
                            format!("{ctx}: destination `{p}`: `zip.password` is empty; an empty password would leave the zip unencrypted: set one, or use `zip: true`"),
                        );
                        None
                    }
                    Some(Loose::Ok(s)) => Some(s),
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidField,
                            file.map(Path::to_path_buf),
                            None,
                            format!("{ctx}: destination `{p}`: `zip.password` must be a string"),
                        );
                        None
                    }
                };
                let encryption = match z.encryption {
                    None => Default::default(),
                    Some(Loose::Ok(e)) => e,
                    Some(Loose::Bad(_)) => {
                        self.diags.error(
                            Code::InvalidField,
                            file.map(Path::to_path_buf),
                            None,
                            format!(
                                "{ctx}: destination `{p}`: `zip.encryption` must be one of {}",
                                crate::zip_pack::Encryption::NAMES.join(", ")
                            ),
                        );
                        Default::default()
                    }
                };
                if encryption == crate::zip_pack::Encryption::Zipcrypto {
                    self.diags.warning(
                        Code::InvalidField,
                        file.map(Path::to_path_buf),
                        None,
                        format!("{ctx}: destination `{p}`: `encryption: zipcrypto` is weak (it can be cracked); use it only for recipients who can't open AES-256 zips (Windows Explorer, macOS Archive Utility)"),
                    );
                    if password.is_none() {
                        self.diags.warning(
                            Code::InvalidField,
                            file.map(Path::to_path_buf),
                            None,
                            format!("{ctx}: destination `{p}`: `zip.encryption` has no effect without a `password`"),
                        );
                    }
                }
                Some(crate::zip_pack::ZipSpec { password, encryption })
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidField,
                    file.map(Path::to_path_buf),
                    None,
                    format!(
                        "{ctx}: destination `{p}`: `zip` must be true, false or `{{password, encryption}}`"
                    ),
                );
                None
            }
        };
        if zip.is_some()
            && let Some(path) = &path
            && !crate::preflight::is_templated(path.rsplit(['/', '\\']).next().unwrap_or(path))
            && let Err(e) = crate::zip_pack::zip_name(Some(path), "")
        {
            self.diags.error(
                Code::InvalidField,
                file.map(Path::to_path_buf),
                None,
                format!("{ctx}: destination `{p}`: {e}"),
            );
        }
        Some(Destination {
            profile: p,
            path,
            options,
            attach,
            hold,
            rate,
            zip,
        })
    }

    fn typed_template(
        &mut self,
        t: Loose<config::report::Template>,
        ctx: &str,
        file: Option<&Path>,
        queries: &[QueryEntry],
    ) -> Option<Template> {
        let err = |s: &mut Self, msg: String| {
            s.diags.error(
                Code::InvalidTemplate,
                file.map(Path::to_path_buf),
                None,
                format!("{ctx}: {msg}"),
            )
        };
        let Some(Loose::Ok(tf)) = t.ok().and_then(|t| t.file.clone()) else {
            err(self, "`output.template` needs a `file:`".into());
            return None;
        };
        let Loose::Ok(t) = t else {
            unreachable!("checked above")
        };
        let mut bindings = Vec::new();
        let items = match t.bindings {
            None => Vec::new(),
            Some(Loose::Ok(s)) => s,
            Some(Loose::Bad(_)) => {
                err(self, "`output.template.bindings` must be a list".into());
                Vec::new()
            }
        };
        let names: Vec<&str> = queries.iter().map(|q| q.query.as_str()).collect();
        for (i, b) in items.into_iter().enumerate() {
            let n = i + 1;
            let Loose::Ok(m) = b else {
                err(self, format!("template binding {n} must be a map"));
                continue;
            };
            let s = |v: &Option<Loose<String>>| v.as_ref().and_then(Loose::ok).cloned();
            for k in &m.unknown.0 {
                err(self, format!("template binding {n} has unknown key `{}`", k.name));
            }
            let Some(sheet) = s(&m.sheet) else {
                err(self, format!("template binding {n} needs a `sheet:`"));
                continue;
            };
            let tb = TemplateBinding {
                sheet,
                query: s(&m.query),
                result_index: m.result_index.as_ref().and_then(Loose::ok).map(|x| *x as usize),
                anchor: s(&m.anchor),
                header: m.header.as_ref().and_then(Loose::ok).copied(),
                columns: m.columns.as_ref().and_then(Loose::ok).cloned(),
                cell: s(&m.cell),
                value: s(&m.value),
                column: s(&m.column),
            };
            if let Some(q) = &tb.query
                && !names.contains(&q.as_str())
            {
                err(
                    self,
                    format!(
                        "template binding {n} uses query `{q}`, which isn't one of this Binding's queries"
                    ),
                );
            }
            if let Some(ri) = &m.result_index
                && ri.ok().is_none_or(|x| *x == 0)
            {
                err(
                    self,
                    format!("template binding {n}: `result_index` must be 1 or more"),
                );
            }
            match &tb.cell {
                Some(c) => {
                    if !options::is_cell(c) {
                        err(
                            self,
                            format!("template binding {n}: `cell` `{c}` isn't a valid cell reference"),
                        );
                    }
                    let by_value = tb.value.is_some();
                    let by_query = tb.query.is_some() && tb.column.is_some();
                    if by_value == by_query || (tb.query.is_some() != tb.column.is_some()) {
                        err(
                            self,
                            format!(
                                "template binding {n}: a single-cell binding needs exactly one of `value` or `query` + `column`"
                            ),
                        );
                    }
                    if tb.anchor.is_some() || tb.columns.is_some() {
                        err(
                            self,
                            format!(
                                "template binding {n}: `anchor`/`columns` apply to table blocks, not single cells"
                            ),
                        );
                    }
                }
                None => {
                    if tb.query.is_none() {
                        err(
                            self,
                            format!(
                                "template binding {n}: a table block needs a `query` (or use `cell` for a single cell)"
                            ),
                        );
                    }
                    if let Some(a) = &tb.anchor
                        && !options::is_cell(a)
                    {
                        err(
                            self,
                            format!("template binding {n}: `anchor` `{a}` isn't a valid cell reference"),
                        );
                    }
                    if tb.value.is_some() || tb.column.is_some() {
                        err(
                            self,
                            format!(
                                "template binding {n}: `value`/`column` apply to single-cell bindings (add `cell`)"
                            ),
                        );
                    }
                }
            }
            bindings.push(tb);
        }
        Some(Template { file: tf, bindings })
    }

    pub(super) fn check_template_files(&mut self, project: &Project) {
        let mut seen = BTreeSet::new();
        for r in &project.reports {
            for (format, t) in r.bindings.iter().flat_map(|b| {
                b.outputs
                    .iter()
                    .filter_map(|o| Some((o.format.as_str(), o.template.as_ref()?)))
            }) {
                if !seen.insert((r.name.clone(), t.file.clone(), format!("{:?}", t.bindings))) {
                    continue;
                }
                let path = match resolve_template(&self.root, &r.file, &t.file) {
                    Ok(p) => p,
                    Err(e) => {
                        self.diags.error(
                            Code::MissingTemplate,
                            Some(r.file.clone()),
                            None,
                            format!("report `{}`: {e}", r.name),
                        );
                        continue;
                    }
                };
                if !Format::new(format).is_workbook() {
                    continue;
                }
                match template_sheets(&path) {
                    Ok(sheets) => {
                        for tb in &t.bindings {
                            if !sheets.contains(&tb.sheet) {
                                self.diags.error(
                                    Code::InvalidTemplate,
                                    Some(r.file.clone()),
                                    None,
                                    format!(
                                        "report `{}`: template `{}` has no sheet `{}` (sheets: {})",
                                        r.name,
                                        t.file,
                                        tb.sheet,
                                        sheets.join(", ")
                                    ),
                                );
                            }
                        }
                    }
                    Err(e) => self.diags.error(
                        Code::InvalidTemplate,
                        Some(r.file.clone()),
                        None,
                        format!("report `{}`: can't read template `{}`: {e}", r.name, t.file),
                    ),
                }
            }
        }
    }
}

pub(super) fn builtin_output() -> Mapping {
    let mut m = Mapping::new();
    m.insert("format".into(), Value::String(crate::formats::DEFAULT.into()));
    m
}

pub(super) fn project_default_output(p: &Project) -> Option<Mapping> {
    let yf = YamlFile::parse(
        std::fs::read_to_string(p.root.join(PROJECT_FILE)).ok()?,
        PathBuf::from(PROJECT_FILE),
        &mut Diagnostics::default(),
    )?;
    yf.value.get("default_output").and_then(Value::as_object).cloned()
}

/// Apply one report or Set `output:` layer over the inherited outputs; returns the problems.
///
/// - A list replaces whatever was inherited; each entry starts from the built-in defaults.
/// - A map merges ([`merge_output`]) into the single inherited output, or into the one its
///   `name:` names. With several inherited outputs and no `name:`, it's unclear which one to
///   change: the layer is ignored and the problem says why.
pub fn layer_outputs(base: &mut Vec<Mapping>, over: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    match over {
        Value::Array(list) if list.is_empty() => problems.push(
            "`output` is an empty list; give at least one output, or remove it to use the inherited one"
                .into(),
        ),
        Value::Array(list) => {
            let mut outs = Vec::new();
            for (i, entry) in list.iter().enumerate() {
                match entry.as_object() {
                    Some(m) => {
                        let mut o = builtin_output();
                        problems.extend(merge_output(&mut o, m));
                        outs.push(o);
                    }
                    None => problems.push(format!("`output` entry {} must be a map", i + 1)),
                }
            }
            if problems.is_empty() {
                *base = outs;
            }
        }
        Value::Object(m) => {
            let name_of = |o: &Mapping| o.get("name").and_then(Value::as_str).map(str::to_string);
            let target = match m.get("name").and_then(Value::as_str) {
                Some(n) => match base.iter().position(|o| name_of(o).as_deref() == Some(n)) {
                    Some(i) => Some(i),
                    None if base.len() == 1 && name_of(&base[0]).is_none() => Some(0),
                    None => {
                        let names: Vec<String> = base
                            .iter()
                            .filter_map(name_of)
                            .map(|n| format!("`{n}`"))
                            .collect();
                        problems.push(format!(
                            "no inherited output is named `{n}`{}; override the full `output:` list to add one",
                            if names.is_empty() {
                                String::new()
                            } else {
                                format!(" (they're {})", names.join(", "))
                            }
                        ));
                        None
                    }
                },
                None if base.len() == 1 => Some(0),
                None => {
                    problems.push(format!(
                        "this overrides `output` with no `name:`, but it inherits {} outputs, so it's unclear which one to change; give the `name:` of the output to change, or override the full list",
                        base.len()
                    ));
                    None
                }
            };
            if let Some(i) = target {
                problems.extend(merge_output(&mut base[i], m));
            }
        }
        _ => problems.push("`output` must be a map or a list of maps".into()),
    }
    problems
}

/// Merge an output layer over `base`. Changing `format` drops the lower layers' format options,
/// since they belong to a different format.
///
/// `destination` (a map or a list of maps):
/// - a list replaces whatever was inherited;
/// - a map naming a different `profile` replaces it too, so one plugin's options never leak into
///   another's;
/// - a map without `profile` (or with the same one) merges key by key into the inherited single
///   destination, so a Binding can override just `path`. With several inherited destinations
///   that's ambiguous: the layer's destination is ignored and the returned message says why.
pub fn merge_output(base: &mut Mapping, over: &Mapping) -> Option<String> {
    if let Some(f) = over.get("format")
        && base.get("format") != Some(f)
    {
        base.retain(|k, _| is_one_of(k, FORMAT_INDEPENDENT_KEYS));
    }
    let mut problem = None;
    for (k, v) in over {
        if k.as_str() == "destination"
            && let Value::Object(o) = v
        {
            let profile = |m: &Mapping| m.get("profile").cloned();
            match base.get_mut(k) {
                Some(Value::Array(list)) if o.get("profile").is_none() && list.len() > 1 => {
                    problem = Some(format!(
                        "this overrides `destination` with no `profile:`, but it inherits {} destinations, so it's unclear which one to change; override the full list instead",
                        list.len()
                    ));
                    continue;
                }
                Some(Value::Array(list))
                    if list.len() == 1
                        && list[0]
                            .as_object()
                            .is_some_and(|b| o.get("profile").is_none() || profile(b) == profile(o)) =>
                {
                    let mut b = list[0].as_object().cloned().unwrap_or_default();
                    b.extend(o.clone());
                    base.insert(k.clone(), Value::Object(b));
                    continue;
                }
                Some(Value::Object(b)) if o.get("profile").is_none() || profile(b) == profile(o) => {
                    b.extend(o.clone());
                    continue;
                }
                _ => {}
            }
        }
        base.insert(k.clone(), v.clone());
    }
    problem
}

/// Resolve an output's `template.file`, looked up from the folder of `yaml` (the report's file)
/// by the rule in [`crate::template_file`].
pub fn resolve_template(root: &Path, yaml: &Path, file: &str) -> Result<PathBuf, String> {
    let yaml = root.join(yaml);
    let from = yaml.parent().unwrap_or(root);
    crate::template_file::resolve(root, from, file)
}

/// Sheet names in an xlsx template, read without modifying it.
/// Where a template file named in YAML (`template:`, a message's `file:`) is read from: the
/// project root, else `templates/`.
pub fn find_template(root: &Path, file: &str) -> Option<PathBuf> {
    [root.join(file), root.join("templates").join(file)]
        .into_iter()
        .find(|p| p.is_file())
}

/// "output `name`", or "output 2" for an unnamed one, as messages name an output.
pub fn output_label(name: Option<&str>, index: usize) -> String {
    match name {
        Some(n) => format!("output `{n}`"),
        None => format!("output {}", index + 1),
    }
}

pub fn template_sheets(path: &Path) -> Result<Vec<String>, String> {
    use calamine::Reader;
    let wb: calamine::Xlsx<_> =
        calamine::open_workbook(path).map_err(|e: calamine::XlsxError| e.to_string())?;
    Ok(wb.sheet_names())
}

/// Whether an output's `queries:` (as written) includes `query`; no list: every query.
pub(super) fn output_feeds(queries: Option<&Loose<Vec<String>>>, query: &str) -> bool {
    match queries {
        Some(Loose::Ok(q)) => q.iter().any(|n| n == query),
        _ => true,
    }
}

/// `results.<query>` and the column read after it (`.first.col`, `.last.col`, `.rows[0].col`,
/// `.col`), from a read of `results`.
fn result_column(p: &preflight::Path) -> Option<(&str, Option<&str>)> {
    let query = p.attr(0)?;
    let rest = match (p.attr(1), p.steps.get(2)) {
        (Some("first" | "last"), _) => 2,
        (Some("rows"), Some(preflight::Step::Index(_))) => 3,
        _ => 1,
    };
    Some((query, p.attr(rest)))
}
