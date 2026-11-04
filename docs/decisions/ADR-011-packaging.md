# ADR-011: Cross-platform packaging

Status: Proposed. Produce native app bundles/installers: signed and notarized
macOS universal/arm64 packages first, then signed Windows packages and tested
Linux AppImage/Flatpak candidates. Bundle only audited runtime assets; generate
third-party notices and SBOMs. Updates are signed and opt-in. A platform becomes
supported only after clean installation, editing/export smoke tests, update, and
uninstallation on representative systems.
