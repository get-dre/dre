//! Native format fixture: an unfamiliar name with explicit file and template properties.
use dre_protocol::msg::{FormatLayout, FormatProperties};
use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};

struct Fixture;

impl Format for Fixture {
    fn properties(&self) -> Option<FormatProperties> {
        if std::env::var_os("DRE_FIXTURE_NO_PROPERTIES").is_some() {
            return None;
        }
        Some(FormatProperties {
            extension: std::env::var("DRE_FIXTURE_EXTENSION").unwrap_or_else(|_| "data".into()),
            layout: if std::env::var_os("DRE_FIXTURE_DOCUMENT").is_some() {
                FormatLayout::Document
            } else if std::env::var_os("DRE_FIXTURE_WORKBOOK").is_some() {
                FormatLayout::Workbook
            } else if std::env::var_os("DRE_FIXTURE_MULTIPLE_TABLES").is_some() {
                FormatLayout::MultipleTables
            } else {
                FormatLayout::SingleTable
            },
        })
    }

    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        let mut names = Vec::new();
        while let Some(mut set) = sets.next_set()? {
            names.push(set.meta.name.clone());
            while set.next_batch()?.is_some() {}
        }
        let value = serde_json::json!({"sets": names, "template": req.template});
        std::fs::write(&req.path, serde_json::to_vec(&value)?)?;
        Ok(vec![req.path.clone()])
    }
}

fn main() {
    serve_format(About::new("folio", "1.0.0"), Fixture)
}
