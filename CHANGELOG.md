# Changelog

All notable changes to Ground Station. Generated from commit history by [git-cliff](https://git-cliff.org); do not edit by hand.


## [Unreleased]

### Added

- Add Codex integration; recover from stale daemons
- Add 512px avatar for the GitHub org and other profiles
- Add style guide and a first trajectory viewer UI

### UI

- sort toggle on the trajectories list, newest first by default
- shared sort toggle; dashboard sessions default to newest first
- oldest/newest sort toggle for the trajectory event list
- keep dashboard card titles from being overlapped by captions
- overview dashboard at /, trajectories list moved to /trajectories
- prompt total column; rename in to 'in · uncached'
- cache hit rate in list, header, model rows and a per-call chart
- show in, cache write, cache read and out tokens separately
- tilde-shorten repository paths too
- tilde home paths, no median without completed runs
- hours in durations, idle status, unattributed model time, live-data fixes
- mirror Health.adapters from gsd api
- responsive trajectory list columns; ignore tsbuildinfo

### CLI and daemon

- split token counts; plain-text logs when not on a terminal

### Changed

- Installer and release pipeline
- Move the Claude Code adapter into adapters/claude-code

### Documentation

- friendlier tone, logo, banner and product screenshot

