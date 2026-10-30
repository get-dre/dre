//! `dre fonts`: the PDF fallback fonts in `~/.dre/fonts`. `install` fills the cache ahead of time
//! for offline servers and Docker image builds, with the same registry download and checksum
//! check a run uses; `ls` lists what's cached.

use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::output::{Printer, Tone};

/// How a PDF picks fallback fonts and how to get them offline: `dre fonts --help`.
pub const ABOUT: &str = "\
PDFs from the `document` plugin use IBM Plex and the project's own `@font-face` fonts. For text \
they can't draw (Chinese, Japanese, Korean, Arabic, Hebrew, Devanagari, Thai), a run fetches the \
script's pinned Noto Sans from the registry once and caches it in ~/.dre/fonts.

Han characters follow the HTML `lang` of their element or its nearest ancestor: `ja` Japanese, \
`ko` Korean, `zh` Simplified Chinese, `zh-Hant`/`zh-TW`/`zh-HK` Traditional Chinese. Without a CJK \
`lang`, kana means Japanese, Hangul Korean, and Han alone Simplified Chinese.

Offline, with nothing cached, the PDF shows boxes for that text and the run warns (or fails, with \
`missing_fonts: error`). Three ways to have the fonts offline:
  1. set DRE_REGISTRY_URL to an offline copy of the registry (its index and font files);
  2. run `dre fonts install <script>...` on a connected machine and copy ~/.dre/fonts, or run it \
in the Docker image build;
  3. add fonts covering the text to the project with `@font-face`.";

#[derive(Subcommand)]
pub enum FontsCommand {
    /// Download and verify fallback fonts into ~/.dre/fonts ahead of a run: by script (`ja`,
    /// `zh-Hans`, `zh-Hant`, `ko`, `Arab`, `Hebr`, `Deva`, `Thai`) or language (`ar`, `he`, `hi`,
    /// `th`, `zh`, `zh-TW`), or `--all`.
    Install(InstallArgs),
    /// List the cached fonts, the scripts each covers, and whether its files verify.
    Ls,
}

#[derive(Args)]
pub struct InstallArgs {
    /// Scripts or languages, like `ja zh-Hans ar`.
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    scripts: Vec<String>,
    /// Every font in the registry.
    #[arg(long)]
    all: bool,
}

/// `dre fonts install`.
pub fn install(ctx: &dre_core::Context, a: &InstallArgs, printer: &Printer) -> ExitCode {
    // An empty list asks for every package; clap makes it `--all` only.
    let scripts = if a.all { Vec::new() } else { a.scripts.clone() };
    match dre_core::fonts::install(ctx, &scripts) {
        Ok(installed) => {
            for f in installed {
                let verb = if f.downloaded { "Installed" } else { "Cached" };
                printer.line(
                    Tone::Good,
                    verb,
                    &format!(
                        "{} {} ({}) in {}",
                        f.name,
                        f.version,
                        f.scripts.join(", "),
                        f.dir.display()
                    ),
                );
            }
            crate::exit::ok()
        }
        Err(e) => {
            printer.error(&e);
            crate::exit::failed()
        }
    }
}

/// `dre fonts ls`.
pub fn ls(ctx: &dre_core::Context) -> ExitCode {
    let found = dre_core::fonts::cached(ctx.env());
    let dir = ctx.env().dre_home().join("fonts");
    if found.is_empty() {
        println!(
            "No fonts cached in {}; `dre fonts install <script>` adds them",
            dir.display()
        );
        return crate::exit::ok();
    }
    let mut rows = vec![[
        "PACKAGE".to_string(),
        "VERSION".into(),
        "SCRIPTS".into(),
        "SIZE".into(),
        "STATUS".into(),
    ]];
    for f in found {
        rows.push([
            f.name,
            f.version,
            if f.scripts.is_empty() {
                "?".into()
            } else {
                f.scripts.join(", ")
            },
            human_bytes(f.bytes),
            if f.verified {
                "ok".into()
            } else {
                "incomplete: a run downloads it again".into()
            },
        ]);
    }
    println!("Fonts in {}:", dir.display());
    print!("{}", crate::ls::columns(rows));
    crate::exit::ok()
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}
