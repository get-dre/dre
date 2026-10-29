use super::*;

fn files<'a>(format: &'a str, extension: Option<&'a str>, template: bool) -> Files<'a> {
    Files {
        format: Format::new(format),
        extension,
        template,
    }
}

#[test]
fn file_names_by_format() {
    // (format, extension, stem, --output-name, destination path, name)
    let cases = [
        ("csv", None, "sales", None, None, "sales.csv"),
        ("delimited", None, "sales", None, None, "sales.txt"),
        ("fixed_width", None, "sales", None, None, "sales.txt"),
        ("parquet", None, "sales", None, None, "sales.parquet"),
        ("xlsx", None, "sales", None, None, "sales.xlsx"),
        ("pdf", None, "sales", None, None, "sales.pdf"),
        ("message", None, "daily", None, None, "daily.md"),
        ("csv", Some("dat"), "sales", None, None, "sales.dat"),
        ("csv", Some(""), "sales", None, None, "sales"),
        (
            "csv",
            None,
            "sales",
            None,
            Some("out/q1/report.csv"),
            "report.csv",
        ),
        ("csv", None, "sales", None, Some("in\\report.csv"), "report.csv"),
        ("csv", None, "sales", None, Some("out/"), "sales.csv"),
        (
            "csv",
            None,
            "sales",
            Some("x.csv"),
            Some("out/report.csv"),
            "x.csv",
        ),
    ];
    for (format, ext, stem, output_name, path, want) in cases {
        let got = files(format, ext, false).base_name(stem, output_name, path);
        assert_eq!(got, want, "{format} {ext:?} {path:?}");
    }
}

#[test]
fn one_table_formats_split_several_queries_into_files() {
    let tabs = ["north", "south"];
    // (format, template, files)
    type Case = (&'static str, bool, Vec<(&'static str, Vec<usize>)>);
    let cases: [Case; 5] = [
        (
            "csv",
            false,
            vec![("sales_north.csv", vec![0]), ("sales_south.csv", vec![1])],
        ),
        (
            "parquet",
            false,
            vec![("sales_north.csv", vec![0]), ("sales_south.csv", vec![1])],
        ),
        ("csv", true, vec![("sales.csv", vec![0, 1])]),
        ("xlsx", false, vec![("sales.csv", vec![0, 1])]),
        ("html", false, vec![("sales.csv", vec![0, 1])]),
    ];
    for (format, template, want) in cases {
        let want: Vec<(String, Vec<usize>)> = want.into_iter().map(|(n, t)| (n.to_string(), t)).collect();
        assert_eq!(
            files(format, None, template).plan("sales.csv", &tabs).unwrap(),
            want,
            "{format} {template}"
        );
    }
    // One query is never split.
    assert_eq!(
        files("csv", None, false).plan("sales.csv", &["north"]).unwrap(),
        [("sales.csv".to_string(), vec![0])]
    );
    // Without an extension, and with a tab that names a folder.
    assert_eq!(
        Files::split_names("sales", &tabs).unwrap(),
        ["sales_north", "sales_south"]
    );
    assert_eq!(
        files("csv", None, false).plan("sales.csv", &["a/b", "c"]),
        Err("`a/b` can't be used in a file name".to_string())
    );
}

#[test]
fn excel_sheet_names() {
    let cases = [
        ("Summary", None),
        ("", Some("sheet name `` must be 1 to 31 characters long")),
        (
            "a_name_that_is_much_too_long_for_excel",
            Some("sheet name `a_name_that_is_much_too_long_for_excel` must be 1 to 31 characters long"),
        ),
        (
            "Q1/Q2",
            Some("sheet name `Q1/Q2` contains `/`, which Excel doesn't allow in sheet names"),
        ),
        (
            "'quoted",
            Some("sheet name `'quoted` can't start or end with an apostrophe"),
        ),
        (
            "HISTORY",
            Some("`History` is reserved by Excel and can't be a sheet name"),
        ),
    ];
    for (name, want) in cases {
        assert_eq!(check_sheet_name(name).err().as_deref(), want, "{name}");
    }
    assert_eq!(
        check_sheets([("Sales", "sales"), ("sales", "sales_by_region")]),
        Err(
            "sheet name `sales` is used twice (by `sales` and `sales_by_region`); Excel sheet names must be unique"
                .to_string()
        )
    );
    assert_eq!(check_sheets([("Sales", "sales"), ("Costs", "costs")]), Ok(()));
}

#[test]
fn zip_name_clashes() {
    assert_eq!(first_clash(["a.csv", "b.csv"]), None);
    assert_eq!(first_clash(["a.csv", "b.csv", "a.csv", "b.csv"]), Some("a.csv"));
}

#[test]
fn what_each_format_is() {
    // (format, one table, workbook, document, takes a template, template decides sections)
    let cases = [
        ("csv", true, false, false, false, false),
        ("delimited", true, false, false, false, false),
        ("parquet", true, false, false, false, false),
        ("xlsx", false, true, false, true, false),
        ("html", false, false, true, true, true),
        ("pdf", false, false, true, true, true),
        ("message", false, false, false, false, true),
    ];
    for (name, one, workbook, document, template, sections) in cases {
        let f = Format::new(name);
        assert_eq!(
            (
                f.one_table(),
                f.is_workbook(),
                f.is_document(),
                f.takes_template(),
                f.template_decides_sections()
            ),
            (one, workbook, document, template, sections),
            "{name}"
        );
    }
}
