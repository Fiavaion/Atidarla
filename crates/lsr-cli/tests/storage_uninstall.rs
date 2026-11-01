//! `lasair storage` with `LSR_MODELS_DIR` pointing at a folder the user
//! chose, in a scratch home (PRD A70 c): files of the same names as the
//! app's settings beside that folder belong to other tools and survive.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_lasair");

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lsr-cli-storage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `lasair storage <args>` with every per-user root inside `home`.
fn storage(home: &PathBuf, models: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .arg("storage")
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home.join("local"))
        .env("APPDATA", home.join("roaming"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("LSR_MODELS_DIR", models)
        .output()
        .unwrap()
}

#[test]
fn files_beside_a_chosen_model_folder_survive_uninstall_and_remove() {
    let home = temp("beside");
    let ai = home.join("AI");
    let models = ai.join("models");
    std::fs::create_dir_all(models.join("someone-elses")).unwrap();
    std::fs::write(models.join("someone-elses/w.bin"), b"w").unwrap();
    let foreign = ["secrets.toml", "hosts.toml", "accepted.toml"];
    for f in foreign {
        std::fs::write(ai.join(f), b"not lasair's").unwrap();
    }

    let list = storage(&home, &models, &["list"]);
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        !text.contains(&*ai.join("secrets.toml").to_string_lossy()),
        "{text}"
    );

    let out = storage(&home, &models, &["uninstall", "--models", "keep"]);
    assert!(out.status.success(), "{out:?}");
    let out = storage(
        &home,
        &models,
        &["remove", "packages", "models", "cache", "settings"],
    );
    assert!(out.status.success(), "{out:?}");
    for f in foreign {
        assert!(ai.join(f).is_file(), "{f} was removed");
    }
    assert!(models.join("someone-elses/w.bin").is_file());
    let _ = std::fs::remove_dir_all(&home);
}
