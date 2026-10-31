use super::*;
use std::{fs, os::unix::fs::PermissionsExt};

fn setup() -> (tempfile::TempDir, Store, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("minohomo")).unwrap();
    let output = temp.path().join("mihomo/config.yaml");
    let validator = temp.path().join("validate");
    fs::write(&validator, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&validator, fs::Permissions::from_mode(0o700)).unwrap();
    let mut settings: Value =
        serde_yaml::from_str(include_str!("../defaults/config.yaml")).unwrap();
    settings["output"] = output.to_str().unwrap().into();
    settings["mihomo-binary"] = validator.to_str().unwrap().into();
    settings["reload-command"] = serde_yaml::to_value(vec!["/bin/true"]).unwrap();
    fs::write(
        store.root.join("config.yaml"),
        serde_yaml::to_string(&settings).unwrap(),
    )
    .unwrap();
    (temp, store, output)
}
fn direct(id: u64, name: &str) -> Source {
    Source {
        id,
        kind: SourceKind::Proxy,
        link: format!("trojan://pwd@host.test:443#{name}"),
        added_at: Utc::now(),
        updated_at: None,
        proxies: vec![],
    }
}

#[test]
fn targeted_update_reuses_other_caches_and_backs_up_output() {
    let (_temp, store, output) = setup();
    let mut cached = direct(2, "Cached");
    cached.proxies = vec![links::parse_link(&cached.link).unwrap()];
    cached.updated_at = Some(Utc::now() - chrono::Duration::days(1));
    let old_timestamp = cached.updated_at;
    let mut state = State {
        next_id: 2,
        sources: vec![direct(1, "Fresh"), cached],
    };
    store.save(&state).unwrap();
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, "old config").unwrap();
    update(&store, &mut state, Target::Index(1)).unwrap();
    let saved = store.state().unwrap();
    assert!(saved.sources[0].updated_at.is_some());
    assert_eq!(saved.sources[1].updated_at, old_timestamp);
    let generated: Value = storage::read_yaml(&output).unwrap();
    assert_eq!(generated["proxies"].as_sequence().unwrap().len(), 2);
    assert_eq!(
        fs::read_to_string(output.with_extension("yaml.bak")).unwrap(),
        "old config"
    );
}

#[test]
fn update_preserves_reality_mlkem_while_applying_fingerprint_override() {
    let (_temp, store, output) = setup();
    let mut source = direct(1, "Reality");
    source.link = "vless://uuid@host.test:443?security=reality&pbk=public&sid=71645f53&fp=firefox&sni=shop.example.org&support-x25519mlkem768=true#Reality".into();
    let mut state = State {
        next_id: 1,
        sources: vec![source],
    };
    store.save(&state).unwrap();
    update(&store, &mut state, Target::All).unwrap();
    let generated: Value = storage::read_yaml(&output).unwrap();
    assert_eq!(generated["proxies"][0]["client-fingerprint"], "chrome");
    assert_eq!(
        generated["proxies"][0]["reality-opts"]["support-x25519mlkem768"],
        true
    );
    assert_eq!(generated["proxies"][0]["servername"], "shop.example.org");
    let cached = &store.state().unwrap().sources[0].proxies[0];
    assert_eq!(cached["reality-opts"]["support-x25519mlkem768"], true);
    assert_eq!(cached["client-fingerprint"], "firefox");
}

#[test]
fn rejected_config_leaves_saved_state_and_output_untouched() {
    let (_temp, store, output) = setup();
    let validator = store.settings().unwrap().mihomo_binary;
    fs::write(validator, "#!/bin/sh\nexit 1\n").unwrap();
    let mut state = State {
        next_id: 1,
        sources: vec![direct(1, "Fresh")],
    };
    store.save(&state).unwrap();
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, "original").unwrap();
    assert!(update(&store, &mut state, Target::All).is_err());
    assert_eq!(fs::read_to_string(&output).unwrap(), "original");
    assert!(store.state().unwrap().sources[0].updated_at.is_none());
}

#[test]
fn failed_reload_keeps_new_config_and_cache() {
    let (_temp, store, output) = setup();
    let path = store.root.join("config.yaml");
    let mut settings: Value = storage::read_yaml(&path).unwrap();
    settings["reload-command"] = serde_yaml::to_value(vec!["/bin/false"]).unwrap();
    fs::write(path, serde_yaml::to_string(&settings).unwrap()).unwrap();
    let mut state = State {
        next_id: 1,
        sources: vec![direct(1, "Fresh")],
    };
    store.save(&state).unwrap();
    let err = update(&store, &mut state, Target::All).unwrap_err();
    assert!(err.to_string().contains("reload failed"));
    assert!(output.exists());
    assert!(store.state().unwrap().sources[0].updated_at.is_some());
}

#[test]
fn failed_fetch_does_not_commit_earlier_successes() {
    let (_temp, store, output) = setup();
    let mut broken = direct(2, "Broken");
    broken.link = "unsupported://host.test:443".into();
    let mut state = State {
        next_id: 2,
        sources: vec![direct(1, "Fresh"), broken],
    };
    store.save(&state).unwrap();
    assert!(update(&store, &mut state, Target::All).is_err());
    assert!(!output.exists());
    assert!(
        store
            .state()
            .unwrap()
            .sources
            .iter()
            .all(|s| s.updated_at.is_none())
    );
}
