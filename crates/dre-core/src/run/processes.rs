//! Format and destination processes leased by active output workers and kept until the run ends.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use dre_protocol::host::LogSink;

use crate::delivery::ProcessCache;
use crate::engine::Events;
use crate::plugin_host::Plugin;

pub(super) struct Processes {
    pub formats: HashMap<String, Box<dyn Plugin>>,
    pub destinations: ProcessCache,
    log: Arc<Mutex<LogSink>>,
}

impl Processes {
    pub fn log(&self) -> LogSink {
        let log = self.log.clone();
        Arc::new(move |plugin, line| (log.lock().unwrap())(plugin, line))
    }
}

impl Drop for Processes {
    fn drop(&mut self) {
        for (_, process) in self.formats.drain() {
            process.close();
        }
    }
}

#[derive(Clone)]
pub(super) struct Pool {
    idle: Arc<Mutex<Vec<Processes>>>,
    fallback: LogSink,
}

impl Pool {
    pub fn new(fallback: LogSink) -> Self {
        Self {
            idle: Arc::default(),
            fallback,
        }
    }

    pub fn lease(&self, events: &Events) -> Lease {
        let processes = self.idle.lock().unwrap().pop().unwrap_or_else(|| {
            let log = Arc::new(Mutex::new(events.plugin_log()));
            Processes {
                formats: HashMap::new(),
                destinations: ProcessCache::default(),
                log,
            }
        });
        *processes.log.lock().unwrap() = events.plugin_log();
        Lease {
            processes: Some(processes),
            idle: self.idle.clone(),
            fallback: self.fallback.clone(),
        }
    }
}

pub(super) struct Lease {
    processes: Option<Processes>,
    idle: Arc<Mutex<Vec<Processes>>>,
    fallback: LogSink,
}

impl Deref for Lease {
    type Target = Processes;
    fn deref(&self) -> &Self::Target {
        self.processes.as_ref().expect("leased until drop")
    }
}
impl DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.processes.as_mut().expect("leased until drop")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(processes) = self.processes.take() {
            *processes.log.lock().unwrap() = self.fallback.clone();
            self.idle.lock().unwrap().push(processes);
        }
    }
}
