//! The resolution of the state folder: the new default on each platform branch, the override,
//! and the old cache location the migration reads from. Pure: no process environment is read.

use super::*;

fn os(s: &str) -> Option<&OsStr> {
    Some(OsStr::new(s))
}

#[test]
fn macos_default_is_application_support() {
    assert_eq!(
        default_dir_for(Platform::MacOs, None, os("/Users/auditor"), os("/ignored")),
        PathBuf::from("/Users/auditor/Library/Application Support/camerata")
    );
}

#[test]
fn other_default_prefers_xdg_data_home_then_local_share() {
    assert_eq!(
        default_dir_for(Platform::Other, None, os("/home/u"), os("/x/data")),
        PathBuf::from("/x/data/camerata")
    );
    assert_eq!(
        default_dir_for(Platform::Other, None, os("/home/u"), None),
        PathBuf::from("/home/u/.local/share/camerata")
    );
}

/// The XDG base directory spec says a blank or relative value is ignored: a relative one would
/// put the state folder under whatever folder the process happened to start in.
#[test]
fn a_blank_or_relative_xdg_data_home_is_ignored() {
    for xdg in ["", "   ", "relative/data"] {
        assert_eq!(
            default_dir_for(Platform::Other, None, os("/home/u"), os(xdg)),
            PathBuf::from("/home/u/.local/share/camerata"),
            "{xdg:?}"
        );
    }
}

#[test]
fn the_override_wins_on_every_platform_and_camerata_is_appended() {
    for platform in [Platform::MacOs, Platform::Other] {
        assert_eq!(
            default_dir_for(platform, os("/sandbox/state"), os("/home/u"), os("/x/data")),
            PathBuf::from("/sandbox/state/camerata")
        );
        assert_eq!(
            cache_dir_for(platform, os("/sandbox/state"), os("/home/u"), os("/x/cache")),
            PathBuf::from("/sandbox/state/camerata"),
            "the disposable files follow the override too"
        );
    }
}

#[test]
fn a_blank_override_is_ignored() {
    for platform in [Platform::MacOs, Platform::Other] {
        assert_eq!(
            default_dir_for(platform, os("   "), os("/home/u"), None),
            default_dir_for(platform, None, os("/home/u"), None)
        );
    }
}

/// The old location must stay byte for byte what earlier versions wrote to, or the migration
/// looks in the wrong place and a paused inspection is never found.
#[test]
fn the_cache_location_is_unchanged() {
    assert_eq!(
        cache_dir_for(Platform::MacOs, None, os("/Users/auditor"), os("/ignored")),
        PathBuf::from("/Users/auditor/Library/Caches/camerata")
    );
    assert_eq!(
        cache_dir_for(Platform::Other, None, os("/home/u"), os("/x/cache")),
        PathBuf::from("/x/cache/camerata")
    );
    assert_eq!(
        cache_dir_for(Platform::Other, None, os("/home/u"), None),
        PathBuf::from("/home/u/.cache/camerata")
    );
}

#[test]
fn no_home_degrades_to_the_temp_dir() {
    for platform in [Platform::MacOs, Platform::Other] {
        assert_eq!(
            default_dir_for(platform, None, None, None),
            std::env::temp_dir().join("camerata")
        );
    }
}

#[test]
fn the_override_resolves_with_no_migration_candidate() {
    let env = StateEnv {
        state_override: Some("/sandbox/state".into()),
        home: Some("/home/u".into()),
        xdg_data_home: Some("/x/data".into()),
        xdg_cache_home: Some("/x/cache".into()),
    };
    for platform in [Platform::MacOs, Platform::Other] {
        assert_eq!(
            resolve(platform, &env),
            Resolved::Override(PathBuf::from("/sandbox/state/camerata"))
        );
    }
}

#[test]
fn the_default_resolves_to_the_new_folder_with_the_cache_folder_to_migrate_from() {
    let env = StateEnv {
        state_override: Some("  ".into()),
        home: Some("/Users/auditor".into()),
        xdg_data_home: Some("/x/data".into()),
        xdg_cache_home: Some("/x/cache".into()),
    };
    assert_eq!(
        resolve(Platform::MacOs, &env),
        Resolved::Default {
            new: PathBuf::from("/Users/auditor/Library/Application Support/camerata"),
            old: PathBuf::from("/Users/auditor/Library/Caches/camerata"),
        }
    );
    assert_eq!(
        resolve(Platform::Other, &env),
        Resolved::Default {
            new: PathBuf::from("/x/data/camerata"),
            old: PathBuf::from("/x/cache/camerata"),
        }
    );
}
