use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};

use dre_protocol::delivery::{Caps, Rules, Store, StoreError, deliver};

struct SharedStore {
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    written: Arc<Barrier>,
}

impl Store for SharedStore {
    fn caps(&self) -> Caps {
        Caps {
            create_exclusive: true,
            rename_no_replace: true,
            visible_when_complete: false,
        }
    }
    fn write(&mut self, local: &Path, remote: &str, _: bool) -> Result<(), StoreError> {
        self.files
            .lock()
            .unwrap()
            .insert(remote.into(), std::fs::read(local).unwrap());
        self.written.wait(); // Both uploads finish before either rename.
        Ok(())
    }
    fn rename(&mut self, from: &str, to: &str, _: bool) -> Result<(), StoreError> {
        let mut files = self.files.lock().unwrap();
        let bytes = files
            .remove(from)
            .ok_or_else(|| StoreError::Failed("missing upload".into()))?;
        files.insert(to.into(), bytes);
        Ok(())
    }
    fn exists(&mut self, remote: &str) -> Result<bool, StoreError> {
        Ok(self.files.lock().unwrap().contains_key(remote))
    }
    fn delete(&mut self, remote: &str) -> Result<(), StoreError> {
        self.files.lock().unwrap().remove(remote);
        Ok(())
    }
}

#[test]
fn parallel_deliveries_with_a_shared_staging_directory_keep_complete_files() {
    for same_destination in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let files = Arc::new(Mutex::new(BTreeMap::new()));
        let written = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for (i, bytes) in [b"first complete file".as_slice(), b"second complete file"]
                .into_iter()
                .enumerate()
            {
                let local = dir.path().join(i.to_string());
                std::fs::write(&local, bytes).unwrap();
                let mut store = SharedStore {
                    files: files.clone(),
                    written: written.clone(),
                };
                workers.push(scope.spawn(move || {
                    let remote = if same_destination || i == 0 {
                        "a/report.csv"
                    } else {
                        "b/report.csv"
                    };
                    deliver(
                        &mut store,
                        &local,
                        remote,
                        &Rules {
                            retries: 0,
                            temp_dir: Some("/staging".into()),
                            ..Rules::default()
                        },
                    )
                }));
            }
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        let files = files.lock().unwrap();
        assert_eq!(files.len(), if same_destination { 1 } else { 2 });
        if same_destination {
            assert!(
                [b"first complete file".as_slice(), b"second complete file"]
                    .contains(&files["a/report.csv"].as_slice())
            );
        } else {
            assert_eq!(files["a/report.csv"], b"first complete file");
            assert_eq!(files["b/report.csv"], b"second complete file");
        }
    }
}
