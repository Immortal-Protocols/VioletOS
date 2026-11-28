//! Host test for where Settings and the Agent stage full-system update pieces
//! (runtime/open/update.rs `staging`, `make test-unit`): only the app that
//! will run `update write` can write or read its staging file, and /data
//! never keeps it across restarts. (A shared /tmp file let any open app swap
//! the pieces between Settings' write and its `update write`; one file for
//! both apps let them corrupt each other's pieces.)
#![allow(dead_code)]
extern crate alloc;

#[path = "../policy.rs"]
mod policy;
#[path = "../../protocol.rs"]
mod protocol;
#[path = "../../../services/storage/writeback.rs"]
mod writeback;

use policy::*;


/// update.rs `staging`, as written there (the Open Runtime crate cannot be built on the host).
fn staging(app: &str) -> String {
    format!("/data/{}.ota-staging", app.trim_start_matches("/apps/"))
}

#[test]
fn only_the_updating_app_reaches_its_staging_file() {
    let source = include_str!("../update.rs");
    assert!(source.contains(r#"alloc::format!("/data/{}.ota-staging", app.trim_start_matches("/apps/"))"#),
            "update.rs and this test name the same file");
    let settings_src = include_str!("../../../apps/open/settings/src/updates.rs");
    assert!(settings_src.contains(r#"const STAGING: &str = "/data/settings.ota-staging";"#));
    let image = Profile::parse("caps: net system\n", Origin::Image);
    let installed = Profile::parse("caps: net\n", Origin::Installed);
    for (app, owner) in [("/apps/settings", "settings"), ("/apps/agent", "agent")] {
        let path = staging(app);
        assert_eq!(path_rule(owner, &image, &path, true), Ok(()), "{path}");
        assert_eq!(path_rule(owner, &image, &path, false), Ok(()), "{path}");
        for other in ["notes", "weather", "settingsx", "agent2", if owner == "agent" { "settings" } else { "agent" }] {
            for p in [&image, &installed] {
                assert!(path_rule(other, p, &path, true).is_err(), "{other} writes {path}");
                assert_eq!(path_rule(other, p, &path, false), Err(ENOENT), "{other} reads {path}");
            }
        }
        // A name the write-back never sends: pieces never go to the disk as /data files.
        assert_eq!(writeback::key(&path), None, "{path}");
    }
}
