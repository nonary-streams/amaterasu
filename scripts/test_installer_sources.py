"""Cheap source-level contracts for the tracked Windows installer definition."""

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[1]
PRODUCT = (ROOT / "installer" / "Product.wxs").read_text(encoding="utf-8")
BUNDLE = (ROOT / "installer" / "Bundle.wxs").read_text(encoding="utf-8")
BUILD = (ROOT / "installer" / "build-installer.ps1").read_text(encoding="utf-8")
PLAYNITE_HASHES = {
    "extension.yaml": "0E3507A22AC5C24F1E0573CAAFF52F572BC1AF9C1C18270905B2694EBFEBA72D",
    "AmaterasuPlaynite.psm1": "72CD5AADFCA739F345CC82BC989D438E7B31F432BC89B1F069B785E953D43906",
}
MIGRATION = (ROOT / "crates" / "package" / "src" / "migration.rs").read_text(encoding="utf-8")
MIGRATION_TESTS = (
    ROOT / "crates" / "package" / "src" / "migration" / "tests.rs"
).read_text(encoding="utf-8")


def test_product_has_transactional_self_upgrade_and_legacy_detection():
    assert 'UpgradeCode="{51B3E91F-BB86-4EF3-9A9D-AED8610C8E47}"' in PRODUCT
    assert 'Schedule="afterInstallInitialize"' in PRODUCT
    assert 'Property="VIBESHINE_UPGRADE_DETECTED"' in PRODUCT
    assert 'Property="VIBEPOLO_UPGRADE_DETECTED"' in PRODUCT
    assert '<MajorUpgrade' in PRODUCT


def test_import_runs_after_files_and_before_any_destructive_cutover():
    assert 'amaterasu-migration.exe' in PRODUCT
    assert 'DllEntry="WixQuietExec"' in PRODUCT
    assert 'Execute="deferred"' in PRODUCT
    assert 'Impersonate="no"' in PRODUCT
    assert 'After="InstallFiles"' in PRODUCT
    assert 'RemoveExistingProducts' not in PRODUCT
    assert 'sunshine.conf' not in PRODUCT
    assert 'ServiceControl Id="Sunshine' not in PRODUCT
    assert 'ServiceControl Id="Apollo' not in PRODUCT
    assert 'ServiceControl Id="Vibeshine' not in PRODUCT
    assert 'ServiceControl Id="Vibepollo' not in PRODUCT


def test_provision_rollback_is_journaled_and_scheduled_before_provisioning():
    assert 'unprovision-if-created' in PRODUCT
    assert 'Execute="rollback"' in PRODUCT
    assert 'Id="RollbackProvisionInstallerRuntime"' in PRODUCT
    assert 'After="SetRollbackProvisionInstallerRuntime"' in PRODUCT
    assert 'After="RollbackProvisionInstallerRuntime"' in PRODUCT
    sequence = PRODUCT.split('<InstallExecuteSequence>', 1)[1]
    assert sequence.index('Action="RollbackProvisionInstallerRuntime"') < sequence.index(
        'Action="ProvisionInstallerRuntime"'
    )
    assert 'preexisting data' in PRODUCT
    assert 'finalize-installer-provision' in PRODUCT
    assert 'SCM\'s started state' in PRODUCT


def test_operational_readiness_is_checked_after_service_start_without_competing_probe_worker():
    assert 'service-ready' in PRODUCT
    assert 'Id="VerifyAmaterasuServiceReady"' in PRODUCT
    assert 'operational-ready' in PRODUCT
    assert 'Id="VerifyAmaterasuOperationalReady"' in PRODUCT
    sequence = PRODUCT.split('<InstallExecuteSequence>', 1)[1]
    assert sequence.index('Action="SetVerifyAmaterasuServiceReady" After="StartServices"') < sequence.index(
        'Action="VerifyAmaterasuServiceReady"'
    )
    assert sequence.index('Action="VerifyAmaterasuServiceReady"') < sequence.index(
        'Action="SetVerifyAmaterasuOperationalReady"'
    ) < sequence.index('Action="VerifyAmaterasuOperationalReady"')
    assert sequence.index('Action="VerifyAmaterasuOperationalReady"') < sequence.index(
        'Action="SetFinalizePredecessorHandoff"'
    )
    assert 'does not acquire the runtime lock' in PRODUCT


def test_predecessor_handoff_is_transactional_and_follows_readiness_gates():
    for command in (
        "handoff-predecessor",
        "rollback-predecessor-handoff",
        "finalize-predecessor-handoff",
        "finalize-installer-provision",
    ):
        assert command in PRODUCT
    sequence = PRODUCT.split('<InstallExecuteSequence>', 1)[1]
    assert sequence.index('Action="RollbackPredecessorHandoff"') < sequence.index(
        'Action="HandoffPredecessor"'
    )
    assert sequence.index('Action="HandoffPredecessor" Before="StartServices"') < sequence.index('Action="VerifyAmaterasuOperationalReady"')
    assert sequence.index('Action="FinalizePredecessorHandoff"') < sequence.index(
        'Action="FinalizeInstallerProvision"'
    )
    for action in (
        "RollbackPredecessorHandoff",
        "HandoffPredecessor",
        "FinalizePredecessorHandoff",
        "FinalizeInstallerProvision",
    ):
        definition = PRODUCT.split(f'Id="{action}"', 1)[1].split('/>', 1)[0]
        assert 'Impersonate="no"' in definition
        assert 'HideTarget="yes"' in definition


def test_preinstall_guards_do_not_trust_the_public_bundle_property():
    assert 'Id="AMATERASU_BUNDLE_COORDINATED" Secure="yes"' in PRODUCT
    assert 'not an authorization boundary' in PRODUCT
    assert 'Binary Id="MigrationGuardBinary"' in PRODUCT
    assert 'require-no-predecessor --runtime' in PRODUCT
    assert '--nonce &quot;[AMATERASU_BUNDLE_TOKEN]&quot;' in PRODUCT
    assert 'Id="AMATERASU_BUNDLE_TOKEN"' in PRODUCT
    assert 'Hidden="yes"' in PRODUCT.split('Id="AMATERASU_BUNDLE_TOKEN"', 1)[1].split('/>', 1)[0]
    assert 'validate-installer-paths --install-folder' in PRODUCT
    sequence = PRODUCT.split('<InstallExecuteSequence>', 1)[1]
    require_line = next(
        line for line in sequence.splitlines() if 'Action="RequireNoPredecessor"' in line
    )
    assert 'AMATERASU_BUNDLE_COORDINATED' not in require_line
    assert 'Before="InstallFiles"' in require_line
    assert sequence.index('Action="ValidateInstallerPaths"') < sequence.index(
        'Action="RequireNoPredecessor"'
    ) < sequence.index('Action="ProvisionInstallerRuntime"')
    for action in ("ValidateInstallerPaths", "RequireNoPredecessor"):
        definition = PRODUCT.split(f'Id="{action}"', 1)[1].split('/>', 1)[0]
        assert 'BinaryKey="MigrationGuardBinary"' in definition
        assert 'ExeCommand="[CustomActionData]"' in definition
        assert 'Execute="deferred"' in definition
        assert 'Impersonate="no"' in definition
        assert 'HideTarget="yes"' in definition
    for action in (
        "SetRollbackPredecessorHandoff",
        "RollbackPredecessorHandoff",
        "SetHandoffPredecessor",
        "HandoffPredecessor",
    ):
        line = next(line for line in sequence.splitlines() if f'Action="{action}"' in line)
        assert 'AMATERASU_BUNDLE_COORDINATED' in line
    for action in ("FinalizePredecessorHandoff", "FinalizeInstallerProvision"):
        line = next(line for line in sequence.splitlines() if f'Action="{action}"' in line)
        assert 'NOT AMATERASU_BUNDLE_COORDINATED' in line


def test_migration_helpers_ship_beside_the_orchestrator():
    for helper in ("amaterasu-pairing-import.exe", "amaterasu-authentication-import.exe"):
        assert helper in PRODUCT
        assert helper in BUILD


def test_paths_are_safe_with_msi_directory_property_trailing_separators():
    assert '--destination &quot;[AMATERASU_DATA].&quot;' in PRODUCT
    assert '--program-files &quot;[LEGACY_PROGRAM_FILES].&quot;' in PRODUCT


def test_uninstall_does_not_own_or_remove_mutable_runtime_data():
    data_component = PRODUCT.split('<Component Id="DataDirectory"', 1)[1].split('</Component>', 1)[0]
    assert '<CreateFolder' not in data_component
    assert 'migration backups must remain recoverable' in data_component


def test_installer_never_uses_shell_cleanup_for_mutable_data():
    lowered = PRODUCT.lower()
    assert 'rmdir ' not in lowered
    assert 'remove-item' not in lowered
    assert 'cmd.exe' not in lowered


def test_all_predecessor_families_are_discovered_and_critical_settings_fail_closed():
    for product, directory, configuration in (
        ("Apollo", "apollo", "apollo.conf"),
        ("Sunshine", "sunshine", "sunshine.conf"),
        ("Vibeshine", "vibeshine", "vibeshine.conf"),
        ("Vibepollo", "vibepollo", "vibepollo.conf"),
    ):
        assert f"Self::{product}" in MIGRATION
        assert f'"{directory}"' in MIGRATION
        assert f'"{configuration}"' in MIGRATION
    assert "preflight_legacy_configuration_with_sensitive_sources" in MIGRATION
    assert "preflight_refuses_sensitive_configuration_without_exposing_its_contents" in MIGRATION_TESTS
    assert "approved_pairing_preserves_only_contained_configured_identity_files" in MIGRATION_TESTS


def test_build_is_tracked_and_uses_stable_harvest_guids():
    assert 'heat.exe' in BUILD
    assert ' -ag ' in BUILD
    assert 'amaterasu-migration.exe' in BUILD
    assert 'WixUtilExtension' in BUILD
    assert 'if ([string]::IsNullOrWhiteSpace($OutputDirectory))' in BUILD


def test_installer_reads_release_binaries_from_the_cargo_target_directory():
    if not shutil.which("pwsh"):
        return
    command = r"""
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($env:TEST_BUILD, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'installer build script has syntax errors' }
$function = $ast.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Get-CargoReleaseDirectory'
}, $true)
if (-not $function) { throw 'missing Cargo release directory resolver' }
Invoke-Expression $function.Extent.Text
$repository = $env:TEST_REPOSITORY
$expected = Join-Path $repository 'target\release'
if ((Get-CargoReleaseDirectory $repository '') -ne $expected) { throw 'default target mismatch' }
$expected = Join-Path $repository 'isolated\target\release'
if ((Get-CargoReleaseDirectory $repository 'isolated\target') -ne $expected) { throw 'relative target mismatch' }
$absolute = Join-Path $repository 'another\target'
$expected = Join-Path $absolute 'release'
if ((Get-CargoReleaseDirectory $repository $absolute) -ne $expected) { throw 'absolute target mismatch' }
"""
    environment = os.environ.copy()
    environment.update(
        TEST_BUILD=str(ROOT / "installer" / "build-installer.ps1"),
        TEST_REPOSITORY=str(ROOT),
    )
    result = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", command],
        capture_output=True,
        text=True,
        env=environment,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_playnite_extension_is_one_msi_owned_pair_under_program_files():
    namespace = {"w": "http://schemas.microsoft.com/wix/2006/wi"}
    product = ET.fromstring(PRODUCT)
    program_files = product.find(".//w:Directory[@Id='ProgramFiles64Folder']", namespace)
    install_folder = program_files.find("w:Directory[@Id='INSTALLFOLDER']", namespace)
    assert install_folder.attrib["Name"] == "Amaterasu"
    installed_files = product.find(".//w:DirectoryRef[@Id='INSTALLFOLDER']", namespace)
    extension = installed_files.find("w:Directory[@Id='PLAYNITEEXTENSION']", namespace)
    assert extension.attrib["Name"] == "playnite-extension"
    components = extension.findall("w:Component", namespace)
    assert len(components) == 1
    component = components[0]
    assert component.attrib["Id"] == "PlayniteExtensionFiles"
    assert component.attrib["Guid"] == "*"
    assert component.attrib["Win64"] == "yes"
    files = component.findall("w:File", namespace)
    assert {item.attrib["Name"] for item in files} == set(PLAYNITE_HASHES)
    assert sum(item.attrib.get("KeyPath") == "yes" for item in files) == 1
    for item in files:
        assert item.attrib["Source"] == (
            "$(var.StageDir)\\playnite-extension\\" + item.attrib["Name"]
        )
    feature = product.find(".//w:Feature[@Id='ProductFeature']", namespace)
    assert len(feature.findall("w:ComponentRef[@Id='PlayniteExtensionFiles']", namespace)) == 1
    assert product.find(".//w:MajorUpgrade", namespace).attrib["Schedule"] == "afterInstallInitialize"
    host = installed_files.find("w:Component[@Id='HostExecutable']", namespace)
    assert host.find("w:ServiceInstall", namespace) is not None
    assert host.find("w:ServiceControl", namespace) is not None


def _approved_playnite_source(name):
    path = ROOT / "modules" / "playnite" / "extension" / name
    if path.is_file():
        return path.read_bytes()
    return subprocess.check_output(
        ["git", "show", f"main:modules/playnite/extension/{name}"], cwd=ROOT
    )


def _stage_playnite_fixture(source, stage):
    # Invoke the real staging functions without running Cargo or WiX.
    command = r"""
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($env:TEST_BUILD, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'installer build script has syntax errors' }
foreach ($name in 'Get-ApprovedPlayniteExtension', 'Write-ApprovedPlayniteExtension') {
    $function = $ast.Find({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $true)
    if (-not $function) { throw "missing staging function $name" }
    Invoke-Expression $function.Extent.Text
}
$approved = Get-ApprovedPlayniteExtension $env:TEST_SOURCE
Write-ApprovedPlayniteExtension $approved $env:TEST_STAGE
"""
    environment = os.environ.copy()
    environment.update(
        TEST_BUILD=str(ROOT / "installer" / "build-installer.ps1"),
        TEST_SOURCE=str(source),
        TEST_STAGE=str(stage),
    )
    return subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", command],
        capture_output=True,
        text=True,
        env=environment,
        check=False,
    )


def test_playnite_staging_accepts_only_approved_sources_and_exact_crlf_bytes():
    if not shutil.which("pwsh"):
        return
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        source = root / "source"
        source.mkdir()
        original = {}
        for name, expected_hash in PLAYNITE_HASHES.items():
            original[name] = _approved_playnite_source(name).replace(b"\r\n", b"\n")
            (source / name).write_bytes(original[name])
            assert b"\r" not in original[name]
            assert (
                hashlib.sha256(original[name].replace(b"\n", b"\r\n"))
                .hexdigest()
                .upper()
                == expected_hash
            )
        (source / "unapproved.ps1").write_text("not shipped", encoding="utf-8")

        stage = root / "stage"
        result = _stage_playnite_fixture(source, stage)
        assert result.returncode == 0, result.stderr
        shipped = stage / "playnite-extension"
        assert {path.name for path in shipped.iterdir()} == set(PLAYNITE_HASHES)
        for name, expected_hash in PLAYNITE_HASHES.items():
            assert (
                hashlib.sha256((shipped / name).read_bytes()).hexdigest().upper()
                == expected_hash
            )

        for name, content in original.items():
            (source / name).write_bytes(content.replace(b"\n", b"\r\n"))
        crlf_stage = root / "crlf-stage"
        result = _stage_playnite_fixture(source, crlf_stage)
        assert result.returncode == 0, result.stderr
        for name, expected_hash in PLAYNITE_HASHES.items():
            assert (
                hashlib.sha256(
                    (crlf_stage / "playnite-extension" / name).read_bytes()
                )
                .hexdigest()
                .upper()
                == expected_hash
            )

        (source / "extension.yaml").write_bytes(
            original["extension.yaml"].replace(b"\n", b"\r\n", 1)
        )
        mixed_stage = root / "mixed-stage"
        result = _stage_playnite_fixture(source, mixed_stage)
        assert result.returncode != 0
        assert not mixed_stage.exists()

        (source / "extension.yaml").write_bytes(original["extension.yaml"] + b"# tampered\n")
        rejected_stage = root / "tampered-stage"
        result = _stage_playnite_fixture(source, rejected_stage)
        assert result.returncode != 0
        assert not rejected_stage.exists()

        (source / "extension.yaml").unlink()
        missing_stage = root / "missing-stage"
        result = _stage_playnite_fixture(source, missing_stage)
        assert result.returncode != 0
        assert not missing_stage.exists()


def test_bundle_owns_irreversible_legacy_retirement_after_coordinated_msi():
    assert 'Variable Name="AmaterasuDataRoot"' in BUNDLE
    assert 'bal:Overridable="yes"' in BUNDLE
    assert 'MsiPackage Id="AmaterasuMsi"' in BUNDLE
    assert '<MsiProperty Name="AMATERASU_BUNDLE_COORDINATED" Value="1" />' in BUNDLE
    assert '<MsiProperty Name="AMATERASU_DATA" Value="[AmaterasuDataRoot]" />' in BUNDLE
    prepare = BUNDLE.split('ExePackage Id="PrepareBundleUpgrade"', 1)[1].split('/>', 1)[0]
    assert 'prepare-bundle-upgrade --runtime' in prepare
    assert 'cancel-bundle-upgrade --runtime' in prepare
    assert 'LogPathVariable="BundlePrepareLog"' in prepare
    assert 'Vital="yes"' in prepare
    assert 'Permanent="no"' in prepare
    assert '<MsiProperty Name="AMATERASU_BUNDLE_TOKEN" Value="[BundlePrepareLog]" />' in BUNDLE
    assert 'ExePackage Id="RetirePredecessor"' in BUNDLE
    assert 'retire-predecessor --runtime' in BUNDLE
    assert 'Vital="yes"' in BUNDLE.split('ExePackage Id="RetirePredecessor"', 1)[1].split('/>', 1)[0]
    assert 'Permanent="yes"' in BUNDLE.split('ExePackage Id="RetirePredecessor"', 1)[1].split('/>', 1)[0]
    assert BUNDLE.index('ExePackage Id="PrepareBundleUpgrade"') < BUNDLE.index(
        'MsiPackage Id="AmaterasuMsi"'
    ) < BUNDLE.index(
        'ExePackage Id="RetirePredecessor"'
    )
    assert '[CommonAppDataFolder]' not in BUNDLE
    assert 'retire-predecessor --runtime &quot;[AmaterasuDataRoot]\\runtime&quot;' in BUNDLE


def test_bundle_journal_cleanup_cannot_trigger_post_retirement_rollback():
    cleanup = BUNDLE.split('ExePackage Id="FinalizeProvisionJournal"', 1)[1].split('/>', 1)[0]
    assert 'finalize-installer-provision' in cleanup
    assert '&quot;[AmaterasuDataRoot]\\runtime&quot;' in cleanup
    assert 'Vital="no"' in cleanup
    assert 'Permanent="yes"' in cleanup
    assert BUNDLE.index('ExePackage Id="RetirePredecessor"') < BUNDLE.index(
        'ExePackage Id="FinalizeProvisionJournal"'
    )
    assert 'WixBalExtension' in BUILD
    assert 'Amaterasu-$Version-Setup.exe' in BUILD


if __name__ == "__main__":
    test_product_has_transactional_self_upgrade_and_legacy_detection()
    test_import_runs_after_files_and_before_any_destructive_cutover()
    test_provision_rollback_is_journaled_and_scheduled_before_provisioning()
    test_operational_readiness_is_checked_after_service_start_without_competing_probe_worker()
    test_predecessor_handoff_is_transactional_and_follows_readiness_gates()
    test_preinstall_guards_do_not_trust_the_public_bundle_property()
    test_migration_helpers_ship_beside_the_orchestrator()
    test_paths_are_safe_with_msi_directory_property_trailing_separators()
    test_uninstall_does_not_own_or_remove_mutable_runtime_data()
    test_installer_never_uses_shell_cleanup_for_mutable_data()
    test_all_predecessor_families_are_discovered_and_critical_settings_fail_closed()
    test_build_is_tracked_and_uses_stable_harvest_guids()
    test_installer_reads_release_binaries_from_the_cargo_target_directory()
    test_playnite_extension_is_one_msi_owned_pair_under_program_files()
    test_playnite_staging_accepts_only_approved_sources_and_exact_crlf_bytes()
    test_bundle_owns_irreversible_legacy_retirement_after_coordinated_msi()
    test_bundle_journal_cleanup_cannot_trigger_post_retirement_rollback()
    print("Installer source contracts passed.")
