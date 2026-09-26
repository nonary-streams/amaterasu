use super::*;

#[test]
fn canonicalizes_source_line_endings_without_rewriting_existing_crlf() {
    assert_eq!(
        normalize_crlf(b"one\ntwo\r\nthree\n"),
        b"one\r\ntwo\r\nthree\r\n"
    );
}

#[cfg(windows)]
fn owner_sid_string(path: &Path) -> String {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetNamedSecurityInfoW(
            path: *const u16,
            kind: i32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn ConvertSidToStringSidW(sid: *mut c_void, value: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the NUL-terminated name is live; Windows owns the returned descriptor.
    assert_eq!(
        unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                1,
                1,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        },
        0
    );
    let mut value: *mut u16 = std::ptr::null_mut();
    // SAFETY: owner points into the live descriptor; value is LocalFree-owned.
    assert_ne!(unsafe { ConvertSidToStringSidW(owner, &mut value) }, 0);
    let mut length = 0;
    // SAFETY: Windows returns a NUL-terminated SID string.
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, length) });
    // SAFETY: both allocations were returned by the Windows APIs above.
    unsafe {
        LocalFree(value.cast());
        LocalFree(descriptor);
    }
    sid
}

#[cfg(windows)]
type Fixture = (
    PathBuf,
    PathBuf,
    PathBuf,
    String,
    [(&'static str, String, u64); 2],
);

#[cfg(windows)]
fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "amaterasu-playnite-install-{}-{}",
        std::process::id(),
        random_identifier().unwrap()
    ));
    let profile = root.join("profile");
    let extensions = profile.join("AppData/Roaming/Playnite/Extensions");
    let source = root.join("source");
    fs::create_dir_all(&extensions).unwrap();
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("extension.yaml"), b"Id: test\n").unwrap();
    fs::write(
        source.join("AmaterasuPlaynite.psm1"),
        b"Write-Output 'test'\n",
    )
    .unwrap();
    let expected = [
        ("extension.yaml", sha256(b"Id: test\r\n"), 4096),
        (
            "AmaterasuPlaynite.psm1",
            sha256(b"Write-Output 'test'\r\n"),
            65536,
        ),
    ];
    let sid = owner_sid_string(&profile);
    (root, profile, source, sid, expected)
}

#[cfg(windows)]
fn restore_fixture_write_access(active: &Path, sid: &str) {
    grant_user_full_access_for_test(active, sid).unwrap();
    for (name, _, _) in FILES {
        grant_user_full_access_for_test(&active.join(name), sid).unwrap();
    }
}

#[cfg(windows)]
#[test]
fn installs_exact_pair_and_restores_previous_version_after_restart() {
    let (root, profile, source, sid, expected) = fixture();
    let extensions = profile.join("AppData/Roaming/Playnite/Extensions");
    let active = extensions.join(ACTIVE_DIRECTORY);
    fs::create_dir(&active).unwrap();
    fs::write(active.join("extension.yaml"), b"previous manifest").unwrap();
    fs::write(active.join("AmaterasuPlaynite.psm1"), b"previous module").unwrap();
    let expected_refs = [
        (expected[0].0, expected[0].1.as_str(), expected[0].2),
        (expected[1].0, expected[1].1.as_str(), expected[1].2),
    ];
    let PlayniteExtensionInstall::Installed(receipt) =
        install_with_hashes(&source, &extensions, &profile, &sid, &expected_refs).unwrap()
    else {
        panic!("upgrade returned AlreadyCurrent");
    };
    assert!(receipt.had_previous);
    assert_eq!(
        fs::read(active.join("extension.yaml")).unwrap(),
        b"Id: test\r\n"
    );
    let recovered =
        read_playnite_extension_receipt(&extensions, &receipt.transaction, &profile, &sid).unwrap();
    assert_eq!(receipt, recovered);
    assert!(
        OpenOptions::new()
            .write(true)
            .open(active.join("extension.yaml"))
            .is_err()
    );
    assert!(
        install_with_hashes(&source, &extensions, &profile, &sid, &expected_refs).is_err(),
        "pending upgrade was replaced"
    );
    restore_fixture_write_access(&active, &sid);
    rollback_with_hashes(&recovered, &profile, &sid, &expected_refs).unwrap();
    assert_eq!(
        fs::read(active.join("extension.yaml")).unwrap(),
        b"previous manifest"
    );
    assert_eq!(
        fs::read(active.join("AmaterasuPlaynite.psm1")).unwrap(),
        b"previous module"
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn first_install_finalizes_and_rejects_wrong_owner_or_source() {
    let (root, profile, source, sid, expected) = fixture();
    let extensions = profile.join("AppData/Roaming/Playnite/Extensions");
    let expected_refs = [
        (expected[0].0, expected[0].1.as_str(), expected[0].2),
        (expected[1].0, expected[1].1.as_str(), expected[1].2),
    ];
    assert!(
        install_with_hashes(&source, &extensions, &profile, "S-1-5-18", &expected_refs).is_err()
    );
    fs::write(source.join("extension.yaml"), b"changed").unwrap();
    assert!(install_with_hashes(&source, &extensions, &profile, &sid, &expected_refs).is_err());
    fs::write(source.join("extension.yaml"), b"Id: test\n").unwrap();
    fs::write(source.join("unexpected.ps1"), b"unapproved").unwrap();
    assert!(install_with_hashes(&source, &extensions, &profile, &sid, &expected_refs).is_err());
    fs::remove_file(source.join("unexpected.ps1")).unwrap();
    let PlayniteExtensionInstall::Installed(receipt) =
        install_with_hashes(&source, &extensions, &profile, &sid, &expected_refs).unwrap()
    else {
        panic!("first install returned AlreadyCurrent");
    };
    assert!(!receipt.had_previous);
    finalize_with_hashes(&receipt, &profile, &sid, &expected_refs).unwrap();
    assert_eq!(
        install_with_hashes(&source, &extensions, &profile, &sid, &expected_refs).unwrap(),
        PlayniteExtensionInstall::AlreadyCurrent
    );
    assert!(
        read_playnite_extension_receipt(&extensions, &receipt.transaction, &profile, &sid).is_err()
    );
    assert!(extensions.join(ACTIVE_DIRECTORY).is_dir());
    restore_fixture_write_access(&extensions.join(ACTIVE_DIRECTORY), &sid);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn pending_receipt_rolls_back_before_or_during_activation() {
    let (root, profile, _source, sid, expected) = fixture();
    let extensions = profile.join("AppData/Roaming/Playnite/Extensions");
    let active = extensions.join(ACTIVE_DIRECTORY);
    fs::create_dir(&active).unwrap();
    fs::write(active.join("extension.yaml"), b"previous manifest").unwrap();
    fs::write(active.join("AmaterasuPlaynite.psm1"), b"previous module").unwrap();
    let transactions = extensions.join(TRANSACTION_DIRECTORY);
    fs::create_dir(&transactions).unwrap();
    let expected_refs = [
        (expected[0].0, expected[0].1.as_str(), expected[0].2),
        (expected[1].0, expected[1].1.as_str(), expected[1].2),
    ];

    for moved_old_version in [false, true] {
        let transaction = random_identifier().unwrap();
        let receipt = PlayniteExtensionReceipt {
            extensions_directory: extensions.clone(),
            transaction: transaction.clone(),
            had_previous: true,
        };
        write_json_atomically(
            &transactions.join(format!("receipt-{transaction}.json")),
            &receipt,
        )
        .unwrap();
        if moved_old_version {
            fs::rename(&active, transactions.join(format!("backup-{transaction}"))).unwrap();
        }
        let pending = pending_playnite_extension_receipt(&extensions, &profile, &sid)
            .unwrap()
            .unwrap();
        assert_eq!(pending, receipt);
        rollback_with_hashes(&pending, &profile, &sid, &expected_refs).unwrap();
        assert_eq!(
            fs::read(active.join("extension.yaml")).unwrap(),
            b"previous manifest"
        );
        assert!(
            pending_playnite_extension_receipt(&extensions, &profile, &sid)
                .unwrap()
                .is_none()
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn target_validation_rejects_escape_and_reparse_ancestor() {
    use std::os::windows::fs::symlink_dir;
    let (root, profile, _source, sid, _expected) = fixture();
    let outside = root.join("outside/Extensions");
    fs::create_dir_all(&outside).unwrap();
    assert!(validate_target(&outside, &profile, &sid).is_err());

    let alias = profile.join("linked-playnite");
    if symlink_dir(root.join("outside"), &alias).is_ok() {
        assert!(validate_target(&alias.join("Extensions"), &profile, &sid).is_err());
    }
    fs::remove_dir_all(root).unwrap();
}
