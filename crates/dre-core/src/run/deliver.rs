//! The deliver stage: each output to its destinations in order (or its zip), or held for
//! `dre release`.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value as Json, json};

use super::mode::Deliver;
use super::{BindingRun, Fail, OutputStatus, RenderedDest, rel};
use crate::codes::Code;
use crate::delivery::{Deliverer, Request, Sent};
use crate::engine::Level;
use crate::profiles::Role;
use crate::run_results::Delivery;

impl<'a> BindingRun<'a> {
    /// Deliver output `oi` to each of its destinations in order. A failure is recorded and the
    /// rest are still attempted; the output fails if any did.
    pub(super) fn deliver(&mut self, oi: usize) -> Result<(), Fail> {
        let result = self.deliver_output(oi);
        let o = &mut self.outs[oi];
        o.status = Some(match &result {
            Err(_) => OutputStatus::Failed,
            Ok(()) if o.deliveries.iter().any(|d| d.status == "delivered") => OutputStatus::Delivered,
            Ok(()) if o.deliveries.iter().any(|d| d.status == "held") => OutputStatus::Held,
            Ok(()) => OutputStatus::Kept,
        });
        o.error = result.as_ref().err().map(|f| f.message.clone());
        result
    }

    fn deliver_output(&mut self, oi: usize) -> Result<(), Fail> {
        if self.outs[oi].dests.is_empty() {
            self.outs[oi].delivery_note = Some("no destination declared; output stays in target/".into());
            let dir = rel(self.project.root(), &self.run_dir);
            self.ui.step(
                Level::Debug,
                "Kept",
                &format!("no destination declared: output stays in {}", dir.display()),
                None,
            );
            return Ok(());
        }
        if self.outs[oi].files.is_empty() {
            return Ok(());
        }
        let mut dests = std::mem::take(&mut self.outs[oi].dests);
        for d in dests.iter_mut().filter(|d| d.zip.is_some()) {
            match self.build_zip(oi, d) {
                Ok(z) => d.zip_file = Some(z),
                Err(e) => {
                    self.outs[oi].dests = dests;
                    return Err(e);
                }
            }
        }
        let mut failures = Vec::new();
        let mut nowhere = Vec::new();
        let mut deliverer = self.deliverer.take().unwrap_or_else(|| {
            Deliverer::new(&self.ctx, self.project, self.ui.plugin_log()).with_steps(self.ui.clone())
        });
        for d in &dests {
            let profiles = self.project.profiles();
            let target = profiles.target_of(Role::Destination, &d.profile);
            // `None` for an entry that delivers nowhere.
            let kind = profiles
                .target(Role::Destination, &d.profile)
                .map(|o| o.kind.clone());
            if d.hold || self.stages.deliver == Deliver::HoldAll {
                self.hold(oi, d);
                self.outs[oi].deliveries.push(Delivery {
                    profile: d.profile.clone(),
                    kind,
                    target,
                    status: "held",
                    location: None,
                    attempts: None,
                    error_code: None,
                    error: None,
                });
                continue;
            }
            let (status, location, attempts, error) = match self.deliver_one(&mut deliverer, oi, d) {
                Ok(Some(sent)) => ("delivered", Some(sent.location), sent.attempts, None),
                Ok(None) => {
                    nowhere.push(crate::delivery::nowhere_note(&d.profile, &target));
                    ("not_delivered", None, 1, None)
                }
                Err(e) => {
                    let e = e.or(Code::DeliveryFailed);
                    failures.push(e.clone());
                    ("failed", None, 1, Some(e))
                }
            };
            let record = Delivery {
                profile: d.profile.clone(),
                kind,
                target,
                status,
                location,
                attempts: Some(attempts).filter(|n| *n > 1),
                error_code: error.as_ref().map(|e| e.code.clone()),
                error: error.map(|e| e.message),
            };
            self.outs[oi].deliveries.push(record);
        }
        self.deliverer = Some(deliverer);
        let o = &mut self.outs[oi];
        o.dests = dests;
        if !o.deliveries.iter().any(|d| d.status == "delivered") && !nowhere.is_empty() {
            o.delivery_note = Some(nowhere.join("; "));
        }
        match failures.len() {
            0 => Ok(()),
            1 if o.dests.len() == 1 => Err(failures.remove(0)),
            n => Err(Fail::new(
                failures[0].code.clone(),
                format!(
                    "{n} of {} destinations failed: {}",
                    o.dests.len(),
                    failures
                        .iter()
                        .map(|f| f.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )),
        }
    }

    /// Build (or reuse) the zip for output `oi`'s entry `d`: its files and every attached
    /// output's, in `<run>/zips/`. Entries with the same files, name and password share one.
    fn build_zip(&mut self, oi: usize, d: &RenderedDest) -> Result<PathBuf, Fail> {
        let z = d.zip.as_ref().expect("a zipping entry");
        let label = self.b.outputs[oi].label(oi);
        let output = self.b.outputs[oi]
            .name
            .clone()
            .unwrap_or_else(|| format!("output{}", oi + 1));
        let named = d
            .path
            .as_deref()
            .or(d.options.get("attachment_name").and_then(Json::as_str));
        let name = crate::zip_pack::zip_name(named, &output).map_err(|e| {
            Fail::new(
                Code::DeliveryFailed,
                format!("{label}: destination `{}`: {e}", d.profile),
            )
        })?;
        let mut files: Vec<PathBuf> = self.outs[oi].files.iter().map(|(f, _)| f.clone()).collect();
        for a in &d.attach {
            if let Some(j) = self
                .b
                .outputs
                .iter()
                .position(|o| o.name.as_deref() == Some(a.as_str()))
            {
                files.extend(self.outs[j].files.iter().map(|(f, _)| f.clone()));
            }
        }
        let key = json!([name, files, z.password, z.encryption]).to_string();
        if let Some(p) = self.zips.get(&key) {
            return Ok(p.clone());
        }
        let to = self
            .run_dir
            .join("zips")
            .join(self.zips.len().to_string())
            .join(&name);
        let t = Instant::now();
        crate::zip_pack::build(&to, &files, z.password.as_deref(), z.encryption).map_err(|e| {
            Fail::new(
                Code::DeliveryFailed,
                format!("{label}: destination `{}`: {e}", d.profile),
            )
        })?;
        self.ui.step(
            Level::Info,
            "Zipped",
            &format!(
                "{} file(s) into {}",
                files.len(),
                rel(self.project.root(), &to).display()
            ),
            Some(t.elapsed()),
        );
        self.zips.insert(key, to.clone());
        Ok(to)
    }

    /// Record a held delivery of output `oi` to `d`, for `dre release`.
    fn hold(&mut self, oi: usize, d: &RenderedDest) {
        let zipped = d.zip_file.is_some();
        let attach: Vec<crate::release::HeldFile> = d
            .attach
            .iter()
            .filter(|_| !zipped)
            .filter_map(|name| {
                self.b
                    .outputs
                    .iter()
                    .position(|o| o.name.as_deref() == Some(name))
            })
            .flat_map(|j| {
                self.outs[j]
                    .files
                    .iter()
                    .map(|(f, _)| crate::release::held_file(f))
            })
            .collect();
        let idx = self.outs[oi].deliveries.len();
        self.held.push(crate::release::HeldEntry {
            id: format!(
                "{}/{idx}",
                self.b.outputs[oi]
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("output{}", oi + 1))
            ),
            output: self.b.outputs[oi].name.clone(),
            key: None,
            profile: d.profile.clone(),
            path: d.path.clone(),
            options: d.options.clone(),
            attach,
            rate: d.rate,
            message: self.outs[oi].message.clone(),
            files: match &d.zip_file {
                Some(z) => vec![z.clone()],
                None => self.outs[oi].files.iter().map(|(f, _)| f.clone()).collect(),
            }
            .into_iter()
            .map(|f| crate::release::HeldFile {
                sha256: crate::checksum::sha256_file(&f).unwrap_or_default(),
                path: f,
            })
            .collect(),
        });
        self.ui.step(
            Level::Info,
            "Held",
            &format!(
                "{} to `{}`: held for `dre release`",
                self.b.outputs[oi].label(oi),
                d.profile
            ),
            None,
        );
    }

    /// Deliver output `oi`'s files (or its message, or the entry's zip) to one destination.
    /// `Ok(None)`: its entry for this run is `deliver: false`, so nothing was sent.
    fn deliver_one(
        &mut self,
        deliverer: &mut Deliverer<'a>,
        oi: usize,
        d: &RenderedDest,
    ) -> Result<Option<Sent>, Fail> {
        // An output is never delivered once the run is cancelled.
        self.not_cancelled()?;
        let files: Vec<PathBuf> = match &d.zip_file {
            Some(z) => vec![z.clone()],
            None => self.outs[oi].files.iter().map(|(f, _)| f.clone()).collect(),
        };
        let o = &self.b.outputs[oi];
        let message = if o.is_message() {
            self.outs[oi].message.clone()
        } else {
            None
        };
        let attach: Vec<PathBuf> = d
            .attach
            .iter()
            .filter_map(|name| {
                self.b
                    .outputs
                    .iter()
                    .position(|o| o.name.as_deref() == Some(name))
            })
            .flat_map(|j| self.outs[j].files.iter().map(|(f, _)| f.clone()))
            .collect();
        let (store, label) = (self.store, self.label());
        // Bindings running at once never deliver to the same file.
        let claim = |remote: &str| match store.claim(&d.profile, remote, &label) {
            Some(other) => Err(Fail::new(
                Code::DeliveryFailed,
                format!(
                    "`{other}` delivers to `{remote}` on destination `{}` in this run too; Bindings running at once (`threads:`) need different paths (put the Set or a var in the path)",
                    d.profile
                ),
            )),
            None => Ok(()),
        };
        let request = Request::new(&d.profile, d.path.as_deref(), &d.options, &files)
            .message(message.as_ref(), &attach)
            .format(&o.format)
            .claim(&claim);
        let sent = deliverer.deliver(request)?;
        // With `zip:` the one file sent is the zip: the files in it keep no location of their
        // own, and the entry's delivery records the zip's.
        if let Some(sent) = &sent
            && d.zip_file.is_none()
        {
            for (at, (_, first)) in sent.files.iter().zip(self.outs[oi].files.iter_mut()) {
                if let Some(at) = at {
                    first.get_or_insert_with(|| at.clone());
                }
            }
        }
        Ok(sent)
    }
}
