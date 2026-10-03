//! SQLite-specific interchange with Go. Sharing persisted state with Go is
//! separate from the behavioural contract required of every store backend.

use super::support::*;
use std::path::PathBuf;

fn go() -> PathBuf {
    std::env::var_os("BPFMAN_GO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"))
}

pub(super) fn exercise() {
    let c = Context::new();
    for (loader, unloader) in [(rust(), go()), (go(), rust())] {
        let pid = id(&c.load_cli(&loader));
        let pid_text = pid.to_string();
        c.present(pid);
        assert_eq!(
            c.json(&rust(), &["program", "get", &pid_text, "-o", "json"]),
            c.json(&go(), &["program", "get", &pid_text, "-o", "json"])
        );
        assert_eq!(
            c.json(&rust(), &["program", "list", "-o", "json"]),
            c.json(&go(), &["program", "list", "-o", "json"])
        );
        assert_eq!(
            c.run(&rust(), &["program", "get", &pid_text], true).stdout,
            c.run(&go(), &["program", "get", &pid_text], true).stdout
        );
        c.run(&unloader, &["program", "unload", &pid_text], true);
        c.absent(pid);
        assert_eq!(
            c.json(&rust(), &["program", "list", "-o", "json"])["programs"],
            serde_json::json!([])
        );
        c.no_artifacts();
    }
}
