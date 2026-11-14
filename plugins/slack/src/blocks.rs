//! Block Kit `blocks:` entries: checking them, and fixing what Slack's limits would reject.
//! Core sends each entry as `{"component": "<path>", "blocks": [...], "keep": "first|last"}`
//! (an inline block has no `component`). Every fix is a warning naming the component and the
//! block's position; nothing here fails a delivery.

use serde_json::{Map, Value, json};

const MAX_BLOCKS: usize = 50;
const MAX_CHARTS: usize = 2;
const MAX_POINTS: usize = 20;
const MAX_SERIES: usize = 12;
const MAX_LABEL: usize = 20;
const MAX_TITLE: usize = 50;

/// Element types that call back to an app, which DRE has no server to answer.
const INTERACTIVE: &[&str] = &[
    "static_select",
    "external_select",
    "users_select",
    "conversations_select",
    "channels_select",
    "multi_static_select",
    "multi_external_select",
    "multi_users_select",
    "multi_conversations_select",
    "multi_channels_select",
    "overflow",
    "datepicker",
    "timepicker",
    "datetimepicker",
    "checkboxes",
    "radio_buttons",
    "plain_text_input",
    "rich_text_input",
    "email_text_input",
    "url_text_input",
    "number_input",
    "file_input",
    "workflow_button",
];

/// Problems with the option as written in YAML (components may not be rendered yet).
pub fn check(v: &Value) -> Vec<String> {
    let Some(list) = v.as_array() else {
        return vec!["slack option `blocks` must be a list".into()];
    };
    let mut errs = Vec::new();
    for (i, e) in list.iter().enumerate() {
        let n = i + 1;
        let Some(o) = e.as_object() else {
            errs.push(format!("slack `blocks` entry {n} must be a map"));
            continue;
        };
        if !o.get("component").is_some_and(Value::is_string)
            && !o.get("blocks").is_some_and(Value::is_array)
            && !o.contains_key("type")
        {
            errs.push(format!(
                "slack `blocks` entry {n} needs `component:` or a block `type:`"
            ));
        }
        if let Some(k) = o.get("keep")
            && !matches!(k.as_str(), Some("first" | "last"))
        {
            errs.push(format!(
                "slack `blocks` entry {n}: `keep` must be `first` or `last`, got {k}"
            ));
        }
    }
    errs
}

/// Fix option `v` (the rendered entries) into one message's blocks, pushing a warning per
/// fix. Fails only on an option that isn't a list of rendered entries.
pub fn fix(v: &Value, warns: &mut Vec<String>) -> Result<Vec<Value>, String> {
    let Some(entries) = v.as_array() else {
        return Err("slack option `blocks` must be a list".into());
    };
    // Each kept block, with who it is: `<component>, block <n>`.
    let (mut out, mut n, mut charts) = (Vec::<(Value, String)>::new(), 0, 0);
    for (i, e) in entries.iter().enumerate() {
        let Some(blocks) = e["blocks"].as_array() else {
            return Err(format!("slack `blocks` entry {} has no rendered `blocks`", i + 1));
        };
        let last = match e.get("keep") {
            None => false,
            Some(k) => match k.as_str() {
                Some("first") => false,
                Some("last") => true,
                _ => {
                    return Err(format!(
                        "slack `blocks` entry {}: `keep` must be `first` or `last`, got {k}",
                        i + 1
                    ));
                }
            },
        };
        let name = e["component"].as_str().unwrap_or("inline block");
        for b in blocks {
            n += 1;
            let who = format!("{name}, block {n}");
            let mut b = b.clone();
            if b["type"] == "data_visualization" {
                if charts == MAX_CHARTS {
                    warns.push(format!(
                        "{who}: Slack allows {MAX_CHARTS} charts per message; left this one out"
                    ));
                    continue;
                }
                if !fix_chart(&mut b, last, &who, warns) {
                    warns.push(format!("{who}: the chart has no points left; left it out"));
                    continue;
                }
                charts += 1;
            }
            out.push((b, who));
        }
    }
    if out.len() > MAX_BLOCKS {
        let gone: Vec<String> = out.drain(MAX_BLOCKS..).map(|(_, who)| who).collect();
        warns.push(format!(
            "the message has {} blocks; Slack allows {MAX_BLOCKS}, so left out {}",
            MAX_BLOCKS + gone.len(),
            left_out(&gone)
        ));
    }
    let interactive: Vec<String> = out
        .iter()
        .filter(|(b, _)| has_interactive(b))
        .map(|(_, who)| who.clone())
        .collect();
    if !interactive.is_empty() {
        warns.push(format!(
            "{} have interactive elements without a `url`; DRE can't answer them, so they do nothing when used",
            left_out(&interactive)
        ));
    }
    Ok(out.into_iter().map(|(b, _)| b).collect())
}

fn has_interactive(v: &Value) -> bool {
    match v {
        Value::Object(o) => {
            let t = o.get("type").and_then(Value::as_str).unwrap_or_default();
            (t == "button" && !o.contains_key("url"))
                || INTERACTIVE.contains(&t)
                || o.values().any(has_interactive)
        }
        Value::Array(a) => a.iter().any(has_interactive),
        _ => false,
    }
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).chain(['…']).collect()
    }
}

/// Cut a string field in place; true when it was cut.
fn cut_field(o: &mut Map<String, Value>, key: &str, max: usize) -> bool {
    match o.get(key).and_then(Value::as_str) {
        Some(s) if s.chars().count() > max => {
            o.insert(key.into(), cut(s, max).into());
            true
        }
        _ => false,
    }
}

fn left_out(gone: &[String]) -> String {
    let shown: Vec<&str> = gone.iter().take(5).map(String::as_str).collect();
    format!("{}{}", shown.join(", "), if gone.len() > 5 { ", …" } else { "" })
}

/// Fix a chart block; false when it has no points left (Slack rejects an empty chart).
fn fix_chart(b: &mut Value, last: bool, who: &str, w: &mut Vec<String>) -> bool {
    let Some(o) = b.as_object_mut() else { return true };
    if cut_field(o, "title", MAX_TITLE) {
        w.push(format!("{who}: title cut to {MAX_TITLE} characters"));
    }
    let Some(chart) = o.get_mut("chart").and_then(Value::as_object_mut) else {
        return true;
    };
    match chart.get("type").and_then(Value::as_str) {
        Some("pie") => fix_pie(chart, who, w),
        Some("bar" | "area" | "line") => fix_xy(chart, last, who, w),
        _ => true,
    }
}

/// A number, or a numeric string (a DECIMAL column arrives as one).
fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn fix_pie(chart: &mut Map<String, Value>, who: &str, w: &mut Vec<String>) -> bool {
    let Some(segs) = chart.get_mut("segments").and_then(Value::as_array_mut) else {
        return false;
    };
    let (mut not_numbers, mut not_positive) = (0, 0);
    segs.retain_mut(|s| match number(&s["value"]) {
        None => {
            not_numbers += 1;
            false
        }
        Some(v) if v <= 0.0 => {
            not_positive += 1;
            false
        }
        Some(v) => {
            s["value"] = json!(v);
            true
        }
    });
    if not_positive > 0 {
        w.push(format!(
            "{who}: dropped {not_positive} pie values of 0 or less (Slack needs more than 0)"
        ));
    }
    if not_numbers > 0 {
        w.push(format!(
            "{who}: dropped {not_numbers} pie values that aren't numbers"
        ));
    }
    if segs.len() > MAX_SERIES {
        w.push(format!(
            "{who}: {} pie segments; Slack allows {MAX_SERIES}, kept the first {MAX_SERIES}",
            segs.len()
        ));
        segs.truncate(MAX_SERIES);
    }
    let mut cut_n = 0;
    for s in segs.iter_mut().filter_map(Value::as_object_mut) {
        cut_n += usize::from(cut_field(s, "label", MAX_LABEL));
    }
    if cut_n > 0 {
        w.push(format!("{who}: cut {cut_n} labels to {MAX_LABEL} characters"));
    }
    !segs.is_empty()
}

fn fix_xy(chart: &mut Map<String, Value>, last: bool, who: &str, w: &mut Vec<String>) -> bool {
    let Some(mut series) = chart.get("series").and_then(Value::as_array).cloned() else {
        return false;
    };
    let strs = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let mut cats = chart
        .get("axis_config")
        .and_then(|a| a.get("categories"))
        .map(strs)
        .unwrap_or_default();
    if !cats.is_empty() {
        let mut outside: Vec<String> = Vec::new();
        for l in series
            .iter()
            .flat_map(|s| s["data"].as_array().into_iter().flatten())
            .filter_map(|p| p["label"].as_str())
        {
            if !cats.iter().any(|c| c == l) {
                outside.push(l.to_string());
            }
        }
        if !outside.is_empty() {
            w.push(format!(
                "{who}: left out {} points whose label isn't in `axis_config.categories` ({})",
                outside.len(),
                left_out(&outside)
            ));
        }
    } else {
        for p in series
            .iter()
            .flat_map(|s| s["data"].as_array().into_iter().flatten())
        {
            if let Some(l) = p["label"].as_str()
                && !cats.iter().any(|c| c == l)
            {
                cats.push(l.to_string());
            }
        }
    }
    if cats.len() > MAX_POINTS {
        let gone: Vec<String> = if last {
            cats.drain(..cats.len() - MAX_POINTS).collect()
        } else {
            cats.split_off(MAX_POINTS)
        };
        w.push(format!(
            "{who}: {} points; Slack allows {MAX_POINTS}, kept the {} {MAX_POINTS} and left out {}",
            MAX_POINTS + gone.len(),
            if last { "last" } else { "first" },
            left_out(&gone)
        ));
    }
    if series.len() > MAX_SERIES {
        w.push(format!(
            "{who}: {} series; Slack allows {MAX_SERIES}, kept the first {MAX_SERIES}",
            series.len()
        ));
        series.truncate(MAX_SERIES);
    }
    // A point's label is its category's, so the categories count the cut labels once.
    let (mut filled, mut names_cut) = (0, 0);
    for s in series.iter_mut().filter_map(Value::as_object_mut) {
        let old = s
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let data: Vec<Value> = cats
            .iter()
            .map(|c| {
                let mut p = old.iter().find(|p| p["label"] == *c).cloned().unwrap_or_else(|| {
                    filled += 1;
                    json!({"label": c, "value": 0})
                });
                if let Some(p) = p.as_object_mut() {
                    cut_field(p, "label", MAX_LABEL);
                }
                p
            })
            .collect();
        s.insert("data".into(), data.into());
        names_cut += usize::from(cut_field(s, "name", MAX_LABEL));
    }
    if filled > 0 {
        w.push(format!("{who}: {filled} missing points filled with 0"));
    }
    let mut axis = chart
        .get("axis_config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let labels_cut = cats.iter().filter(|c| c.chars().count() > MAX_LABEL).count();
    axis.insert(
        "categories".into(),
        cats.iter().map(|c| cut(c, MAX_LABEL)).collect::<Vec<_>>().into(),
    );
    let long_axis = ["x_label", "y_label"]
        .iter()
        .filter(|k| cut_field(&mut axis, k, MAX_TITLE))
        .count();
    if labels_cut > 0 {
        w.push(format!(
            "{who}: cut {labels_cut} labels to {MAX_LABEL} characters"
        ));
    }
    if names_cut > 0 {
        w.push(format!(
            "{who}: cut {names_cut} series names to {MAX_LABEL} characters"
        ));
    }
    if long_axis > 0 {
        w.push(format!("{who}: cut axis labels to {MAX_TITLE} characters"));
    }
    let empty = cats.is_empty() || series.is_empty();
    chart.insert("series".into(), series.into());
    chart.insert("axis_config".into(), axis.into());
    !empty
}
