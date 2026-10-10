use loom_preferences::{PreferenceChange, PreferenceStore};

#[test]
fn stale_model_failure_preserves_a_newer_choice_across_store_instances() {
    let root = tempfile::tempdir().unwrap();
    let first = PreferenceStore::new(root.path());
    let second = PreferenceStore::new(root.path());
    first
        .update(PreferenceChange::RememberModel {
            path: "/models/first.gguf".into(),
        })
        .unwrap();
    let newer = second
        .update(PreferenceChange::RememberModel {
            path: "/models/newer.gguf".into(),
        })
        .unwrap();
    let stale = first
        .update(PreferenceChange::ForgetModel {
            expected_path: "/models/first.gguf".into(),
        })
        .unwrap();
    assert_eq!(stale, newer);
    assert_eq!(
        second.read().unwrap().last_local_model.as_deref(),
        Some("/models/newer.gguf")
    );
}

#[test]
fn incompatible_data_is_preserved_and_cannot_be_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("preferences.json");
    let bytes = br#"{"version":99,"revision":0,"model":null,"project_suggestions":{}}"#;
    std::fs::write(&path, bytes).unwrap();
    let store = PreferenceStore::new(root.path());
    assert!(store.read().is_err());
    assert!(
        store
            .update(PreferenceChange::RememberModel {
                path: "/models/new.gguf".into()
            })
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn acceptance_model_never_becomes_a_startup_preference() {
    let root = tempfile::tempdir().unwrap();
    let store = PreferenceStore::new(root.path());
    assert!(
        store
            .update(PreferenceChange::RememberModel {
                path: "/tmp/delysis-loom-smoke.test/product/models/writer.gguf".into()
            })
            .is_err()
    );
    assert_eq!(store.read().unwrap().last_local_model, None);
}
