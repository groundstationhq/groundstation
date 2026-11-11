# Changelog

All notable changes to Ground Station. Generated from commit history by [git-cliff](https://git-cliff.org); do not edit by hand.


## [0.2.0] - 2026-11-11

### Added

- feat: publish the backend's image with every release (#36)
- feat: add a Helm chart for the backend (#35)
- feat: let uploads go to an address of their own, apart from the web (#34)
- feat: add the backend: ingest, hosted API, insights worker and cloud login (#27)
- feat: record the git commit and branch on every event (#16)
- feat: live overview charts; gsd serves tool latency at /v1/stats/tools (#11)

### Fixed

- fix: keep paths, hostnames and the upload token from leaking off the machine (#18)

### UI

- replace the demo fallback with sign-in and error screens (#13)
- filter the trajectory event list by kind or failure; Makefile for local loops (#9)

### CLI and daemon

- report the last upload failure; make /v1/health fit both hosts (#17)
- drop events the backend refuses 3 times (#15)

### Changed

- chore: keep each crate's version in one place, so a release can bump any of its parts (#37)
- chore: keep internal planning docs out of the repository (#20)
- chore: move API wire types to groundstation-api (#14)

### Documentation

- docs: add the cloud architecture design (#12)

## [0.1.2] - 2026-09-29

### Added

- feat: add `groundstation update` to install the latest release and restart gsd (#8)

### UI

- default the trajectory event list to newest first (#7)

### Documentation

- docs: describe the branch and PR workflow; accept lowercase changelog prefixes

## [0.1.1] - 2026-09-29

### Changed

- opensource release

