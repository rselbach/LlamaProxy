use super::*;

#[cfg(unix)]
#[test]
fn configuration_updates_preserve_dotfile_symlinks() {
    let home = std::env::temp_dir().join(format!("cpa-linked-config-{}", std::process::id()));
    fs::create_dir_all(home.join("dotfiles")).unwrap();
    let target = home.join("dotfiles/settings.json");
    fs::write(&target, "{}").unwrap();
    let link = home.join("settings.json");
    std::os::unix::fs::symlink("dotfiles/settings.json", &link).unwrap();
    let paths = vec![link.clone()];
    let before = config_images(&paths).unwrap();
    let after = vec![(link.clone(), Some(b"{\"next\":true}".to_vec()))];
    commit_config("pi", &paths, &before, &after, "update", None).unwrap();
    assert_eq!(
        fs::read(&target).unwrap(),
        after[0].1.as_ref().unwrap().clone()
    );
    assert_eq!(
        fs::read_link(&link).unwrap(),
        Path::new("dotfiles/settings.json")
    );

    let directory_link = home.join("linked-directory");
    std::os::unix::fs::symlink(home.join("dotfiles"), &directory_link).unwrap();
    let new_file = directory_link.join("new.json");
    write_config_images("pi", &vec![(new_file.clone(), Some(b"{}".to_vec()))]).unwrap();
    assert_eq!(read_agent_bytes(&new_file).unwrap(), Some(b"{}".to_vec()));
    assert_eq!(fs::read(home.join("dotfiles/new.json")).unwrap(), b"{}");
    assert!(fs::symlink_metadata(&directory_link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(validate_backup_path(&link).is_err());
    assert!(validate_backup_path(&new_file).is_err());
    assert!(validate_config_path(&directory_link).is_err());
    let dangling = home.join("dangling.json");
    std::os::unix::fs::symlink("missing.json", &dangling).unwrap();
    assert!(validate_config_path(&dangling).is_err());
    let cycle = home.join("cycle.json");
    std::os::unix::fs::symlink("cycle.json", &cycle).unwrap();
    assert!(validate_config_path(&cycle).is_err());
    assert!(validate_config_path(Path::new("relative.json")).is_err());
    assert!(validate_config_path(&home.join("../outside.json")).is_err());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn codex_model_merge_does_not_resurrect_removed_schema_fields() {
    let before = serde_json::json!({"models": [{
        "slug": "third-party-model",
        "minimal_client_version": "999.0.0",
        "model_messages": {"instructions_template": "old", "retired": "old"},
        "extensions": {"nested": [1, 2]}
    }]});
    let mut after = serde_json::json!({"models": [{
        "slug": "third-party-model",
        "model_messages": {"instructions_template": "new"}
    }]});
    let mut expected = after.clone();
    expected["models"][0]["extensions"] = before["models"][0]["extensions"].clone();
    preserve_model_extensions(
        "codex",
        Path::new(CODEX_MODEL_CATALOG_FILE),
        &before,
        &mut after,
    );
    assert_eq!(after, expected);
}

#[test]
fn mid_transaction_external_edit_is_preserved_while_earlier_writes_are_rolled_back() {
    let home = std::env::temp_dir().join(format!("cpa-transaction-race-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let paths = vec![home.join("first.json"), home.join("second.json")];
    for path in &paths {
        fs::write(path, "{}").unwrap();
    }
    let before = config_images(&paths).unwrap();
    let after = paths
        .iter()
        .map(|p| (p.clone(), Some(b"{\"next\":true}".to_vec())))
        .collect();
    let mut writes = 0;
    let result = commit_config_transaction(
        "pi",
        &paths,
        &before,
        &after,
        "update",
        None,
        None,
        &mut |client, images| {
            write_config_images(client, images)?;
            writes += 1;
            if writes == 1 {
                fs::write(&paths[1], "{\"external\":true}").unwrap();
            }
            Ok(())
        },
        true,
    );
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "{}");
    assert_eq!(
        fs::read_to_string(&paths[1]).unwrap(),
        "{\"external\":true}"
    );
    fs::remove_dir_all(home).unwrap();
}
