//! Service-owner import of an offline repository bundle. No trust is enrolled
//! and no code is loaded here; activation still belongs to the installed host.
use super::*;

struct LocalRepository<'a>(&'a Path);

impl RepositoryTransport for LocalRepository<'_> {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        if !relative.split('/').all(valid_segment) {
            return Err("invalid local repository path".into());
        }
        let base = self.0.canonicalize().map_err(|error| error.to_string())?;
        let path = base
            .join(relative)
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !path.starts_with(&base) || !path.is_file() {
            return Err("local repository artifact escapes the bundle".into());
        }
        read_bounded(&path, maximum)
    }
}

/// Import one complete signed native set from disk under an existing approval.
/// This advances the ordinary catalog replay checkpoint, just like refresh.
/// The caller is the local installation owner, never a remote management route.
pub fn import_local_native(
    root: &Path,
    repository_id: &str,
    bundle: &Path,
    manifest_digest: &str,
    now: u64,
) -> PackageResult<String> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    let store = RepositoryStore::new(root.join("repositories"))?;
    let approved = store.approved(repository_id)?;
    if bootstrap.trusted_keys.get(&approved.fingerprint) != Some(&approved.public_key) {
        return Err("local import publisher is not trusted by the installed bootstrap".into());
    }
    let transport = LocalRepository(bundle);
    let raw = transport.get("catalog.json", 256 * 1024)?;
    let signature = transport.get("catalog.sig", 128)?;
    let signature = std::str::from_utf8(&signature).map_err(|error| error.to_string())?;
    let catalog = verify_catalog(
        &approved,
        &raw,
        signature,
        &CatalogCheckpoint::default(),
        &current_target(),
        now,
    )?;
    let mut matching = catalog.candidates().iter().filter(|candidate| {
        candidate.kind == CandidateKind::NativeSet && candidate.manifest_sha256 == manifest_digest
    });
    let candidate = matching
        .next()
        .ok_or("native candidate absent from local catalog")?;
    if matching.next().is_some() {
        return Err("ambiguous local native candidate".into());
    }
    let output = store.native_candidate_cache_path(repository_id, manifest_digest)?;
    if output.exists() {
        verify_candidate(&approved, &catalog, candidate, &output, &bootstrap, now)?;
    } else {
        let temporary = output.with_file_name(format!("import-{}", crate::random_identifier()?));
        let result = download_candidate(
            &transport, &approved, &catalog, candidate, &temporary, &bootstrap, now,
        )
        .and_then(|()| std::fs::rename(&temporary, &output).map_err(|error| error.to_string()));
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temporary);
        }
        result?;
    }
    // Do not advance durable metadata until the complete payload is verified.
    // accept_catalog rechecks revocation, signature and the persisted replay floor.
    store.accept_catalog(repository_id, &raw, signature, &current_target(), now)?;
    Ok(catalog.digest().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_transport_bounds_reads_and_rejects_escape_paths() {
        let root = std::env::temp_dir().join(crate::random_identifier().unwrap());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("artifact"), b"1234").unwrap();
        let source = LocalRepository(&root);
        assert_eq!(source.get("artifact", 4).unwrap(), b"1234");
        assert!(source.get("artifact", 3).is_err());
        for path in ["../artifact", "/artifact", "a/../../artifact"] {
            assert!(source.get(path, 4).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).unwrap();
            assert!(source.get("escape", 8192).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
