# Predecessor compatibility boundary

This is intentionally a narrow replacement boundary, not a generic Windows
uninstaller. Discovery accepts only the four fixed family names, configuration
is copied before operational handoff, and no `UninstallString` is parsed or run.

| Family | Current local packaging evidence | Registration / retirement | Status |
|---|---|---|---|
| Sunshine | No local source checkout or installer artifact was available. | Only a Windows Installer registration whose `InstalledProductName` is exactly `Sunshine` is accepted. | MSI supported; EXE-only blocked. |
| Apollo | Vibepollo's Windows CMake keeps the legacy `Apollo` Program Files layout. It does not prove an executable uninstaller's format. | Exact MSI product-name match only. | MSI supported; EXE-only blocked. |
| Vibeshine | Local WiX/CPack metadata declares WiX, stable upgrade GUID, and a custom ARP key `...Uninstall\Vibeshine` with `[INSTALL_ROOT]uninstall.exe /quiet`. | The auxiliary ARP entry is not executed. A matching MSI registration is the retirement boundary. | MSI supported; same-root auxiliary ARP cleanup still requires an exact-key capture implementation. |
| Vibepollo | Local WiX/CPack metadata declares WiX and an Apollo-compatible install directory. | Exact MSI product-name match only. | MSI supported; EXE-only blocked. |

The known machine state has both `Vibeshine` ARP forms at
`C:\Program Files\Sunshine`: one `WindowsInstaller=1` GUID registration and
one non-MSI `Vibeshine` key pointing to `uninstall.exe`. This establishes that
the second is an auxiliary entry, not a second predecessor. The current helper
therefore discovers the MSI first and never runs the executable command.

Residual limitation: it does not yet capture and remove that exact unchanged
auxiliary key after verified MSI retirement. Until that strict capture/cleanup
is implemented, a successful MSI retirement can leave stale ARP display data;
it must not cause rollback of Amaterasu or trigger arbitrary EXE execution.
