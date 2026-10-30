//! A run's mode, as data, and the stages it selects for each Binding.
//!
//! A Binding runs through five stages: **render** its SQL and destinations, **query** (or
//! replay, or check) for its results, **format** its outputs, **deliver** them, and **record**
//! the run. The [`Mode`] says where results come from and what happens to the outputs;
//! [`Stages::select`] turns it into what each stage does, once, before the Binding starts.
//! Nothing outside this file reads the mode.

use std::path::PathBuf;

/// What a run does with each Binding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mode {
    pub results: Results,
    pub delivery: Delivery,
    /// `--preview [N]`: at most N rows per query, each message shown instead of sent, and
    /// nothing delivered.
    pub preview: Option<u64>,
}

/// Where a run's results come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Results {
    /// Run the queries (`dre run`).
    #[default]
    Query,
    /// Render the SQL and stop: `dre compile`, `dre validate`, `dre run --dry-run`.
    Compile,
    /// Render, then check each statement against the database without running it:
    /// `dre validate --live`.
    Check,
    /// `--from-results <DIR>`: the results `--output-json` saved there; no query runs.
    Replay(PathBuf),
    /// `--retry-failed <run id>`: that run's saved results, to redo its failed and never-started
    /// rows.
    Retry(String),
}

/// What happens to a run's outputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Delivery {
    #[default]
    Send,
    /// `--hold`: hold every delivery for `dre release`.
    Hold,
    /// `--no-deliver`: render no destination; every output stays in the target path.
    Keep,
}

impl Mode {
    /// `dre compile`, `dre validate` and `--dry-run`.
    pub fn compile() -> Mode {
        Mode {
            results: Results::Compile,
            ..Mode::default()
        }
    }

    /// `dre validate --live`.
    pub fn check() -> Mode {
        Mode {
            results: Results::Check,
            ..Mode::default()
        }
    }

    /// Whether the run makes run folders: anything but compiling and checking.
    pub fn records(&self) -> bool {
        !matches!(self.results, Results::Compile | Results::Check)
    }

    /// Whether several Bindings may run at once: only a run that runs them.
    pub(super) fn concurrent(&self) -> bool {
        self.records()
    }

    /// Whether destinations are rendered, and so need an entry for the run.
    pub(super) fn renders_destinations(&self) -> bool {
        self.delivery != Delivery::Keep
    }

    /// `--dry-run`, as `run_results.json`'s parameters record it.
    pub(super) fn dry_run(&self) -> bool {
        self.results == Results::Compile
    }
}

/// Where the query stage gets a Binding's results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Source {
    Compile,
    Check,
    Query,
    Replay(PathBuf),
    Retry(String),
}

/// What the deliver stage does with each output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Deliver {
    Send,
    /// Hold every destination for `dre release`.
    HoldAll,
    /// Deliver nothing; the note says why.
    Keep(&'static str),
}

/// What each stage does for one Binding.
#[derive(Debug, Clone)]
pub(super) struct Stages {
    /// Lock the Binding, make its run folder and write `run_results.json`.
    pub record: bool,
    pub source: Source,
    /// Render each query just before it runs, so a template can look at what earlier queries
    /// made (`columns()` of a temp table): a run of a managed report. Otherwise every query
    /// renders first.
    pub interleave: bool,
    /// Render each destination's path and options (not with `--no-deliver`).
    pub destinations: bool,
    /// The most rows a query returns (`--preview`), except for a Binding with a per-row output,
    /// whose population a preview counts in full.
    pub row_limit: Option<u64>,
    /// `--preview`'s row limit: messages are shown, and nothing is compared or delivered.
    pub preview: Option<u64>,
    /// Save the results in the run folder for `--retry-failed`, when a per-row output asks.
    pub keep_results: bool,
    /// Compare the schema with the last successful run's, and record it for the next.
    pub drift: bool,
    pub deliver: Deliver,
    /// At most this many rows of each per-row output (`--limit-rows`, or a preview's three).
    pub sample_rows: Option<usize>,
}

impl Stages {
    /// The stages `mode` runs for a Binding of a `managed` report (or not) that has a `per_row`
    /// output (or not). `limit_rows` and `only_keys` are the per-row options, which a preview's
    /// sample defers to.
    pub fn select(
        mode: &Mode,
        managed: bool,
        per_row: bool,
        limit_rows: Option<usize>,
        only_keys: bool,
    ) -> Stages {
        let source = match &mode.results {
            Results::Compile => Source::Compile,
            Results::Check => Source::Check,
            Results::Query => Source::Query,
            Results::Replay(dir) => Source::Replay(dir.clone()),
            Results::Retry(id) => Source::Retry(id.clone()),
        };
        let deliver = match (mode.delivery, mode.preview) {
            (Delivery::Keep, _) => Deliver::Keep("--no-deliver: output kept locally"),
            (_, Some(_)) => Deliver::Keep("preview: not delivered"),
            (Delivery::Hold, None) => Deliver::HoldAll,
            (Delivery::Send, None) => Deliver::Send,
        };
        let preview = mode.preview;
        Stages {
            record: mode.records(),
            interleave: mode.records() && managed,
            destinations: mode.renders_destinations(),
            row_limit: preview.filter(|_| !per_row),
            preview,
            keep_results: source == Source::Query && preview.is_none(),
            drift: preview.is_none(),
            deliver,
            sample_rows: limit_rows.or((preview.is_some() && !only_keys).then_some(3)),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages(mode: Mode) -> Stages {
        Stages::select(&mode, true, false, None, false)
    }

    #[test]
    fn a_run_records_queries_interleaved_and_delivers() {
        let s = stages(Mode::default());
        assert!(s.record && s.interleave && s.destinations && s.drift && s.keep_results);
        assert_eq!(s.source, Source::Query);
        assert_eq!(s.deliver, Deliver::Send);
        assert_eq!((s.row_limit, s.sample_rows), (None, None));
    }

    #[test]
    fn compiling_and_checking_record_nothing_and_render_everything_first() {
        for (mode, source) in [(Mode::compile(), Source::Compile), (Mode::check(), Source::Check)] {
            let s = stages(mode);
            assert!(!s.record && !s.interleave);
            assert_eq!(s.source, source);
        }
    }

    #[test]
    fn an_unmanaged_report_renders_everything_first() {
        assert!(!Stages::select(&Mode::default(), false, false, None, false).interleave);
    }

    #[test]
    fn a_preview_limits_rows_delivers_nothing_and_leaves_the_schema_alone() {
        let s = stages(Mode {
            preview: Some(10),
            ..Mode::default()
        });
        assert_eq!(s.row_limit, Some(10));
        assert_eq!(s.deliver, Deliver::Keep("preview: not delivered"));
        assert!(!s.drift && !s.keep_results && s.destinations);
        assert_eq!(s.sample_rows, Some(3));
    }

    #[test]
    fn a_preview_of_a_per_row_output_counts_its_population_in_full() {
        let mode = Mode {
            preview: Some(10),
            ..Mode::default()
        };
        assert_eq!(Stages::select(&mode, true, true, None, false).row_limit, None);
        // `--only-keys` picks the rows instead of the sample; `--limit-rows` sets its size.
        assert_eq!(Stages::select(&mode, true, true, None, true).sample_rows, None);
        assert_eq!(
            Stages::select(&mode, true, true, Some(5), false).sample_rows,
            Some(5)
        );
    }

    #[test]
    fn no_deliver_renders_no_destination_and_says_so_over_a_preview() {
        let s = stages(Mode {
            delivery: Delivery::Keep,
            preview: Some(5),
            ..Mode::default()
        });
        assert!(!s.destinations);
        assert_eq!(s.deliver, Deliver::Keep("--no-deliver: output kept locally"));
    }

    #[test]
    fn hold_holds_every_destination() {
        let s = stages(Mode {
            delivery: Delivery::Hold,
            ..Mode::default()
        });
        assert_eq!(s.deliver, Deliver::HoldAll);
    }

    #[test]
    fn a_replay_or_a_retry_keeps_no_results_of_its_own() {
        for results in [Results::Replay("saved".into()), Results::Retry("r1".into())] {
            let s = stages(Mode {
                results,
                ..Mode::default()
            });
            assert!(s.record && !s.keep_results);
        }
    }
}
