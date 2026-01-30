use crate::enrich;
use std::fs;

pub(super) fn cleanup_txn_dirs(paths: &enrich::DocPackPaths, txn_id: &str, verbose: bool) {
    let txn_root = paths.txn_root(txn_id);
    if txn_root.is_dir() {
        if let Err(err) = fs::remove_dir_all(&txn_root) {
            if verbose {
                eprintln!(
                    "warning: failed to clean txn dir {}: {err}",
                    txn_root.display()
                );
            }
        }
    }
    let txns_root = paths.txns_root();
    if txns_root.is_dir() {
        match fs::read_dir(&txns_root) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    let _ = fs::remove_dir(&txns_root);
                }
            }
            Err(err) => {
                if verbose {
                    eprintln!(
                        "warning: failed to read txns dir {}: {err}",
                        txns_root.display()
                    );
                }
            }
        }
    }
}
