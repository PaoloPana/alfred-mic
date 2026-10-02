# Changelog

## [Unreleased] - yyyy-mm-dd
### Changed
- Replaced `pv_recorder` (Picovoice retired its Rust SDK and yanked every version from crates.io) with a `cpal`-based recorder
- `device` now matches the ALSA device id (e.g. `hw:CARD=Device,DEV=0`) or its description; `default` selects the default input device

### Removed
- `library_path` option: `libpv_recorder.so` is no longer needed

## [0.2.0] - 2025-01-11

## [0.1.0] - 2025-01-04

### Added
- Added CI/CD 
