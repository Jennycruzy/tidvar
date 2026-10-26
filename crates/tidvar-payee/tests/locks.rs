#![cfg(target_os = "linux")]
use rand_core::{OsRng, RngCore};
use std::os::unix::fs::MetadataExt;
use tidvar_payee::store::Store;

// SQLite marks a WAL database as in use with a lock on the database file. If the store
// dropped another descriptor for that file after connecting, the kernel would release
// the lock and a second process could delete the WAL that this process still writes to.
#[test]
fn an_open_store_keeps_its_database_lock() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = [0; 16];
    OsRng.fill_bytes(&mut bytes);
    let path = std::env::temp_dir().join(format!("tidvar-lock-{}.sqlite", hex::encode(bytes)));
    let store = Store::open(&path)?;
    let inode = std::fs::metadata(&path)?.ino();
    let pid = std::process::id();
    let held = std::fs::read_to_string("/proc/locks")?.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.get(1) == Some(&"POSIX")
            && fields.get(4) == Some(&pid.to_string().as_str())
            && fields
                .get(5)
                .and_then(|device| device.rsplit(':').next())
                .is_some_and(|value| value == inode.to_string())
    });
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    assert!(held, "The store no longer holds its SQLite database lock.");
    Ok(())
}
