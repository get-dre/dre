//! `on_empty`: what an output does when its queries return no rows.
//!
//! Each empty query applies its own `on_empty` first: `continue` keeps its empty section, `skip`
//! drops its tab (or per-query file), `fail` fails the run. Then, if every query of the output is
//! empty or dropped, the output's `on_empty` decides. A query's explicit setting wins over the
//! output's, with a warning that says so. "Empty" means 0 rows: a `count(*)` returning 0 is one
//! row.

use serde::{Deserialize, Serialize};

/// `on_empty:` on an output or a query entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnEmpty {
    /// Write the output (an empty section) and deliver it, with a warning.
    #[default]
    Continue,
    /// Drop the query's tab or file; for an output: write and deliver nothing.
    Skip,
    /// Fail the run.
    Fail,
}

impl OnEmpty {
    pub const NAMES: &[&str] = &["continue", "skip", "fail"];

    pub fn parse(s: &str) -> Option<OnEmpty> {
        match s {
            "continue" => Some(OnEmpty::Continue),
            "skip" => Some(OnEmpty::Skip),
            "fail" => Some(OnEmpty::Fail),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OnEmpty::Continue => "continue",
            OnEmpty::Skip => "skip",
            OnEmpty::Fail => "fail",
        }
    }
}

/// One query of an output: its name, its rows, and its own `on_empty` if it sets one.
pub struct Query<'a> {
    pub name: &'a str,
    pub rows: u64,
    pub on_empty: Option<OnEmpty>,
}

/// What to do with one output.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// The queries that returned no rows, in order.
    pub empty: Vec<String>,
    /// Queries whose tab (or file) is left out.
    pub drop: Vec<String>,
    /// Fail the run, with this message.
    pub fail: Option<String>,
    /// Write and deliver nothing, with this reason.
    pub skip: Option<String>,
    /// Warnings to show (the output goes out with no data; a conflict was settled).
    pub warnings: Vec<String>,
    /// An info line: a query was empty in an output that has data.
    pub info: Option<String>,
}

/// Decide what `output` (the output's own setting, if any) does about its queries. `droppable`:
/// whether a query's section can be left out (false for html, pdf and messages, whose template
/// decides).
pub fn decide(output: Option<OnEmpty>, queries: &[Query<'_>], droppable: bool) -> Decision {
    let mut d = Decision {
        empty: queries
            .iter()
            .filter(|q| q.rows == 0)
            .map(|q| q.name.to_string())
            .collect(),
        ..Decision::default()
    };
    if d.empty.is_empty() {
        return d;
    }
    let list = |names: &[String]| {
        names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // Each empty query's own setting.
    let mut kept_explicitly = Vec::new();
    for q in queries.iter().filter(|q| q.rows == 0) {
        match q.on_empty {
            Some(OnEmpty::Fail) => {
                d.fail = Some(format!("query `{}` returned no rows (`on_empty: fail`)", q.name));
                return d;
            }
            Some(OnEmpty::Skip) if droppable => d.drop.push(q.name.to_string()),
            Some(OnEmpty::Continue) => kept_explicitly.push(q.name.to_string()),
            _ => {}
        }
    }
    let all_empty = queries.iter().all(|q| q.rows == 0);
    if !all_empty {
        let kept: Vec<String> = d.empty.iter().filter(|q| !d.drop.contains(q)).cloned().collect();
        d.info = Some(match (kept.is_empty(), d.drop.is_empty()) {
            (false, true) => format!("{} returned no rows; written empty", list(&kept)),
            (true, false) => format!("{} returned no rows; left out (`on_empty: skip`)", list(&d.drop)),
            _ => format!(
                "{} returned no rows; {} written empty, {} left out (`on_empty: skip`)",
                list(&d.empty),
                list(&kept),
                list(&d.drop)
            ),
        });
        return d;
    }
    // Every query is empty (or dropped): the output's setting decides.
    let empty = list(&d.empty);
    if d.drop.len() == queries.len() {
        if output.is_some_and(|o| o != OnEmpty::Skip) {
            d.warnings.push(format!(
                "every query has `on_empty: skip`, so the output is skipped although it says `on_empty: {}`",
                output.unwrap_or_default().as_str()
            ));
        }
        d.skip = Some(format!("no data: {empty} returned no rows"));
        return d;
    }
    match output.unwrap_or_default() {
        OnEmpty::Continue => d
            .warnings
            .push(format!("no data: {empty} returned no rows; the output is written empty")),
        o if !kept_explicitly.is_empty() => d.warnings.push(format!(
            "no data: {empty} returned no rows; {} {} `on_empty: continue`, which wins over the output's `on_empty: {}`, so the output is written empty",
            list(&kept_explicitly),
            if kept_explicitly.len() == 1 { "has" } else { "have" },
            o.as_str()
        )),
        OnEmpty::Skip => d.skip = Some(format!("no data: {empty} returned no rows")),
        OnEmpty::Fail => d.fail = Some(format!("no data: {empty} returned no rows (`on_empty: fail`)")),
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use OnEmpty::*;

    fn q(name: &str, rows: u64, on_empty: Option<OnEmpty>) -> Query<'_> {
        Query { name, rows, on_empty }
    }

    /// The conflict table: output setting x query settings x rows -> outcome.
    #[test]
    fn decisions() {
        // Nothing empty: nothing to do.
        let d = decide(Some(Fail), &[q("a", 1, None)], true);
        assert_eq!(d, Decision::default());

        // Default continue, all empty: written, with a warning.
        let d = decide(None, &[q("a", 0, None)], true);
        assert!(d.skip.is_none() && d.fail.is_none());
        assert!(d.warnings[0].contains("`a` returned no rows; the output is written empty"));

        // Output skip / fail, all empty.
        assert!(
            decide(Some(Skip), &[q("a", 0, None), q("b", 0, None)], true)
                .skip
                .is_some()
        );
        assert!(decide(Some(Fail), &[q("a", 0, None)], true).fail.is_some());

        // Output skip but one query has data: written, the empty one noted.
        let d = decide(Some(Skip), &[q("a", 0, None), q("b", 3, None)], true);
        assert!(d.skip.is_none() && d.warnings.is_empty());
        assert!(d.info.unwrap().contains("`a` returned no rows; written empty"));

        // A query's skip drops its tab; the rest goes out.
        let d = decide(None, &[q("a", 0, Some(Skip)), q("b", 3, None)], true);
        assert_eq!(d.drop, vec!["a"]);
        assert!(d.skip.is_none());

        // Every tab dropped although the output says continue: skipped, with a warning.
        let d = decide(
            Some(Continue),
            &[q("a", 0, Some(Skip)), q("b", 0, Some(Skip))],
            true,
        );
        assert!(d.skip.is_some());
        assert!(d.warnings[0].contains("although it says `on_empty: continue`"));

        // A query's fail fails the run even when another query has data.
        let d = decide(None, &[q("a", 0, Some(Fail)), q("b", 3, None)], true);
        assert!(d.fail.unwrap().contains("query `a` returned no rows"));

        // A query's explicit continue wins over the output's skip or fail.
        for o in [Skip, Fail] {
            let d = decide(Some(o), &[q("a", 0, Some(Continue))], true);
            assert!(d.skip.is_none() && d.fail.is_none(), "{o:?}");
            assert!(d.warnings[0].contains("which wins over the output's"), "{o:?}");
        }

        // html, pdf, messages: a query's skip can't drop a section.
        let d = decide(Some(Skip), &[q("a", 0, Some(Skip)), q("b", 3, None)], false);
        assert!(d.drop.is_empty() && d.skip.is_none());
        let d = decide(Some(Skip), &[q("a", 0, Some(Skip))], false);
        assert!(d.skip.is_some());
    }
}
