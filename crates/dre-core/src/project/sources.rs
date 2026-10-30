//! dbt-style `sources:` declarations.

use super::*;

impl Loader {
    /// `sources:` from every project YAML file, in dbt's shape plus DRE's `profile:`. dbt keys
    /// DRE doesn't use yet are noted once per file; any other key is an error.
    pub(super) fn parse_sources(&mut self, files: &[Rc<YamlFile>]) -> BTreeMap<String, SourceDef> {
        let mut out: BTreeMap<String, SourceDef> = BTreeMap::new();
        for yf in files {
            let file = Some(yf.display.clone());
            let top = key_line(yf, SOURCES_KEY);
            let plugin_names = match yf.node.get(SOURCES_KEY).map(|n| &n.kind) {
                Some(crate::config::node::Kind::Seq(items)) => items.iter().all(|i| i.as_str().is_some()),
                _ => false,
            };
            if plugin_names {
                // DRE 0.0.x declared source plugins here.
                self.diags.error(
                    Code::MovedPluginDeclaration,
                    file,
                    top,
                    "`sources:` declares tables now (dbt's format); list plugin packages under `plugins:` instead (e.g. `plugins: [duckdb]`)",
                );
                continue;
            }
            let items = match de::from_node::<Loose<config::sources::SourcesFile>>(&yf.node) {
                Ok(Loose::Ok(f)) => f.sources.map(|s| s.value),
                _ => None,
            };
            let items = match items {
                Some(Loose::Ok(Some(items))) => items,
                Some(Loose::Ok(None)) => continue,
                _ => {
                    self.diags.error(
                        Code::InvalidSource,
                        file,
                        top,
                        "`sources` must be a list of sources, as in dbt: `- name: sales` with `tables:`",
                    );
                    continue;
                }
            };
            let mut unsupported: BTreeSet<String> = BTreeSet::new();
            for item in items {
                if let Some(src) = self.source_def(yf, item, &mut unsupported) {
                    if let Some(prev) = out.get(&src.name) {
                        self.diags.error(
                            Code::DuplicateSource,
                            file.clone(),
                            src.line,
                            format!(
                                "source `{}` is also declared in {}; source names must be unique",
                                src.name,
                                prev.file.display()
                            ),
                        );
                        continue;
                    }
                    out.insert(src.name.clone(), src);
                }
            }
            if !unsupported.is_empty() {
                self.diags.warning(
                    Code::SourceKeyNotSupported,
                    file,
                    top,
                    format!(
                        "dbt source keys DRE doesn't support yet are ignored: {}",
                        unsupported.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                );
            }
        }
        out
    }

    fn source_def(
        &mut self,
        yf: &YamlFile,
        item: de::Located<Loose<config::sources::Source>>,
        unsupported: &mut BTreeSet<String>,
    ) -> Option<SourceDef> {
        let file = Some(yf.display.clone());
        let top = key_line(yf, SOURCES_KEY);
        let item_line = item.line().or(top);
        let Loose::Ok(m) = item.value else {
            self.diags.error(
                Code::InvalidSource,
                file,
                top,
                "each source must be a map with `name:` and `tables:`",
            );
            return None;
        };
        let Some(name) = m
            .name
            .as_ref()
            .and_then(Loose::ok)
            .filter(|n| !n.is_empty())
            .cloned()
        else {
            self.diags
                .error(Code::InvalidSource, file, top, "every source needs a `name`");
            return None;
        };
        let line = item_line;
        let ctx = format!("source `{name}`");
        let mut ok = self.source_keys(
            &yf.display,
            &m.unknown,
            &m.dbt_keys(),
            &ctx,
            SOURCE_KEYS,
            "source",
            unsupported,
        );
        let mut text = |s: &mut Self, k: &str, v: &Option<Loose<de::Text>>| -> Option<String> {
            match v {
                None => None,
                Some(Loose::Ok(t)) => Some(t.0.clone()),
                Some(Loose::Bad(_)) => {
                    s.diags.error(
                        Code::InvalidSource,
                        file.clone(),
                        line,
                        format!("{ctx}: `{k}` must be a string"),
                    );
                    ok = false;
                    None
                }
            }
        };
        let description = text(self, "description", &m.description);
        let database = text(self, "database", &m.database);
        let schema = text(self, "schema", &m.schema);
        let profile = text(self, "profile", &m.profile);
        let quoting = self.quoting(&yf.display, m.quoting, &ctx, line);
        let tags = self.source_tags(&yf.display, m.tags, &ctx, line);
        let meta = self.source_meta(&yf.display, m.meta, &ctx, line);
        let mut tables = Vec::new();
        match m.tables {
            None | Some(Loose::Ok(None)) => {}
            Some(Loose::Ok(Some(ts))) => {
                // Duplicates by name, whether or not each entry is otherwise valid.
                let mut names = BTreeSet::new();
                for t in &ts {
                    let Some(n) = t.value.ok().and_then(|t| t.name.as_ref()).and_then(Loose::ok) else {
                        continue;
                    };
                    if !names.insert(n.clone()) {
                        self.diags.error(
                            Code::DuplicateSourceTable,
                            file.clone(),
                            t.line().or(line),
                            format!("{ctx} declares table `{n}` twice"),
                        );
                        ok = false;
                    }
                }
                for t in ts {
                    if let Some(t) = self.source_table(yf, &name, t, line, unsupported)
                        && !tables.iter().any(|x: &SourceTableDef| x.name == t.name)
                    {
                        tables.push(t);
                    }
                }
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidSource,
                    file.clone(),
                    line,
                    format!("{ctx}: `tables` must be a list"),
                );
                ok = false;
            }
        }
        ok.then(|| SourceDef {
            name: name.clone(),
            description,
            database,
            schema,
            profile,
            quoting,
            tags,
            meta,
            tables,
            file: yf.display.clone(),
            line,
        })
    }

    fn source_table(
        &mut self,
        yf: &YamlFile,
        source: &str,
        item: de::Located<Loose<config::sources::Table>>,
        source_line: Option<usize>,
        unsupported: &mut BTreeSet<String>,
    ) -> Option<SourceTableDef> {
        let file = Some(yf.display.clone());
        let item_line = item.line();
        let Loose::Ok(m) = item.value else {
            self.diags.error(
                Code::InvalidSource,
                file,
                source_line,
                format!("source `{source}`: each table must be a map with a `name`"),
            );
            return None;
        };
        let Some(name) = m
            .name
            .as_ref()
            .and_then(Loose::ok)
            .filter(|n| !n.is_empty())
            .cloned()
        else {
            self.diags.error(
                Code::InvalidSource,
                file,
                source_line,
                format!("source `{source}`: every table needs a `name`"),
            );
            return None;
        };
        let line = item_line.or(source_line);
        let ctx = format!("source `{source}`, table `{name}`");
        let mut ok = self.source_keys(
            &yf.display,
            &m.unknown,
            &m.dbt_keys(),
            &ctx,
            SOURCE_TABLE_KEYS,
            "table",
            unsupported,
        );
        let mut text = |s: &mut Self, k: &str, v: &Option<Loose<de::Text>>| -> Option<String> {
            match v {
                None => None,
                Some(Loose::Ok(t)) => Some(t.0.clone()),
                Some(Loose::Bad(_)) => {
                    s.diags.error(
                        Code::InvalidSource,
                        file.clone(),
                        line,
                        format!("{ctx}: `{k}` must be a string"),
                    );
                    ok = false;
                    None
                }
            }
        };
        let identifier = text(self, "identifier", &m.identifier);
        let description = text(self, "description", &m.description);
        let quoting = self.quoting(&yf.display, m.quoting, &ctx, line);
        let tags = self.source_tags(&yf.display, m.tags, &ctx, line);
        let meta = self.source_meta(&yf.display, m.meta, &ctx, line);
        let mut columns: Vec<SourceColumn> = Vec::new();
        match m.columns {
            None | Some(Loose::Ok(None)) => {}
            Some(Loose::Ok(Some(cs))) => {
                for c in cs {
                    let Loose::Ok(cm) = c else {
                        self.diags.error(
                            Code::InvalidSource,
                            file.clone(),
                            line,
                            format!("{ctx}: each column must be a map with a `name`"),
                        );
                        ok = false;
                        continue;
                    };
                    let Some(cname) = cm
                        .name
                        .as_ref()
                        .and_then(Loose::ok)
                        .filter(|n| !n.is_empty())
                        .cloned()
                    else {
                        self.diags.error(
                            Code::InvalidSource,
                            file.clone(),
                            line,
                            format!("{ctx}: every column needs a `name`"),
                        );
                        ok = false;
                        continue;
                    };
                    let cctx = format!("{ctx}, column `{cname}`");
                    ok &= self.source_keys(
                        &yf.display,
                        &cm.unknown,
                        &cm.dbt_keys(),
                        &cctx,
                        SOURCE_COLUMN_KEYS,
                        "column",
                        unsupported,
                    );
                    if columns.iter().any(|c| c.name.eq_ignore_ascii_case(&cname)) {
                        self.diags.error(
                            Code::DuplicateSourceColumn,
                            file.clone(),
                            line,
                            format!("{ctx} declares column `{cname}` twice"),
                        );
                        ok = false;
                        continue;
                    }
                    let s = |v: &Option<Loose<de::Text>>| v.as_ref().and_then(Loose::ok).map(|t| t.0.clone());
                    columns.push(SourceColumn {
                        name: cname,
                        description: s(&cm.description),
                        data_type: s(&cm.data_type),
                    });
                }
            }
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidSource,
                    file.clone(),
                    line,
                    format!("{ctx}: `columns` must be a list"),
                );
                ok = false;
            }
        }
        ok.then(|| SourceTableDef {
            name,
            identifier,
            description,
            quoting,
            tags,
            meta,
            columns,
        })
    }

    /// The keys of one source, table or column map that aren't DRE's: dbt's are collected into
    /// `unsupported`, anything else is an error. Returns false on an error.
    #[allow(clippy::too_many_arguments)]
    fn source_keys(
        &mut self,
        file: &Path,
        unknown: &de::UnknownKeys,
        dbt: &[&str],
        ctx: &str,
        known: &[&str],
        level: &str,
        unsupported: &mut BTreeSet<String>,
    ) -> bool {
        for k in dbt {
            unsupported.insert(format!("{level} `{k}`"));
        }
        for k in &unknown.0 {
            self.diags.error(
                Code::UnknownKey,
                Some(file.to_path_buf()),
                Some(k.line),
                format!(
                    "{ctx}: unknown key `{}`; a {level} takes {}",
                    k.name,
                    known.join(", ")
                ),
            );
        }
        unknown.0.is_empty()
    }

    fn quoting(
        &mut self,
        file: &Path,
        v: Option<Loose<config::sources::Quoting>>,
        ctx: &str,
        line: Option<usize>,
    ) -> QuotingDef {
        let mut q = QuotingDef::default();
        let Some(v) = v else { return q };
        let bad = |s: &mut Self, msg: String| {
            s.diags.error(
                Code::InvalidSource,
                Some(file.to_path_buf()),
                line,
                format!("{ctx}: {msg}"),
            )
        };
        let Loose::Ok(qm) = v else {
            bad(
                self,
                "`quoting` must be a map of `database`, `schema` and `identifier` to true or false".into(),
            );
            return q;
        };
        for (k, v, slot) in [
            ("database", qm.database, &mut q.database),
            ("schema", qm.schema, &mut q.schema),
            ("identifier", qm.identifier, &mut q.identifier),
        ] {
            match v {
                None => {}
                Some(Loose::Ok(b)) => *slot = Some(b),
                Some(Loose::Bad(_)) => bad(self, format!("`quoting.{k}` must be true or false")),
            }
        }
        for (k, v) in qm.unknown.0 {
            if v.is_boolean() {
                bad(
                    self,
                    format!(
                        "unknown `quoting` key `{}`; use `database`, `schema` or `identifier`",
                        k.value
                    ),
                );
            } else {
                bad(self, format!("`quoting.{}` must be true or false", k.value));
            }
        }
        q
    }

    fn source_tags(
        &mut self,
        file: &Path,
        v: Option<Loose<config::sources::Tags>>,
        ctx: &str,
        line: Option<usize>,
    ) -> Vec<String> {
        match v {
            None => Vec::new(),
            Some(Loose::Ok(config::sources::Tags(de::OneOf::A(t)))) => vec![t],
            Some(Loose::Ok(config::sources::Tags(de::OneOf::B(l)))) => l,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidSource,
                    Some(file.to_path_buf()),
                    line,
                    format!("{ctx}: `tags` must be a list of strings"),
                );
                Vec::new()
            }
        }
    }

    fn source_meta(
        &mut self,
        file: &Path,
        v: Option<Loose<config::sources::Meta>>,
        ctx: &str,
        line: Option<usize>,
    ) -> JsonMap<String, Json> {
        match v {
            None => JsonMap::new(),
            Some(Loose::Ok(m)) => m.0,
            Some(Loose::Bad(_)) => {
                self.diags.error(
                    Code::InvalidSource,
                    Some(file.to_path_buf()),
                    line,
                    format!("{ctx}: `meta` must be a map"),
                );
                JsonMap::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{test_loader, test_yaml};
    use super::*;

    #[test]
    fn reads_sources_with_their_tables_and_quoting() {
        let mut l = test_loader(Path::new("/p"));
        let sources = l.parse_sources(&[test_yaml(
            "reports/sources.yml",
            "sources:\n  - name: erp\n    schema: sales\n    profile: duck\n    quoting: {identifier: true}\n    tables: [{name: orders, identifier: ORDERS}]\n",
        )]);
        let erp = &sources["erp"];
        assert_eq!(
            (erp.schema.as_deref(), erp.profile.as_deref()),
            (Some("sales"), Some("duck"))
        );
        let orders = erp.table("orders").expect("the table is declared");
        assert_eq!(orders.identifier.as_deref(), Some("ORDERS"));
        assert!(orders.quoting.over(&erp.quoting).identifier);
        assert!(!l.diags.has_errors());
    }

    #[test]
    fn a_source_declared_twice_is_an_error() {
        let mut l = test_loader(Path::new("/p"));
        l.parse_sources(&[
            test_yaml("a.yml", "sources: [{name: erp, tables: [{name: t}]}]\n"),
            test_yaml("b.yml", "sources: [{name: erp, tables: [{name: u}]}]\n"),
        ]);
        assert!(l.diags.has_errors());
    }
}
