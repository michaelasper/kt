# Changelog

All notable changes to this project will be documented in this file.

## <small>0.1.5 (2026-05-10)</small>

* fix: handle RediSearch 'Index: already exists' error variant ([dab13fe](https://github.com/michaelasper/kt/commit/dab13fe))
* ci: add Redis service for integration tests ([0dc25ed](https://github.com/michaelasper/kt/commit/0dc25ed))
* ci: remove x86_64 macos build target ([1c9831d](https://github.com/michaelasper/kt/commit/1c9831d))
* ci: set OPENSSL_DIR for macOS builds, fix clippy warning ([9c9e9d4](https://github.com/michaelasper/kt/commit/9c9e9d4))
* ci: start redis-server directly instead of brew services ([49a1f9c](https://github.com/michaelasper/kt/commit/49a1f9c))
* ci: use redis-stack-server for RediSearch module support ([2a7f095](https://github.com/michaelasper/kt/commit/2a7f095))

## <small>0.1.4 (2026-05-10)</small>

* fix: graceful fallback to full sync when stored commit missing, fix upgrade asset pattern ([107b37a](https://github.com/michaelasper/kt/commit/107b37a))

## <small>0.1.3 (2026-05-10)</small>

* fix: strip v prefix from GitHub tag before semver parse ([58f41a4](https://github.com/michaelasper/kt/commit/58f41a4))

## <small>0.1.2 (2026-05-10)</small>

* fix: accept commit SHA in get_diff_files for partial sync ([913e452](https://github.com/michaelasper/kt/commit/913e452))
* ci: fix macOS build issues and add Node.js 24 support ([96bee0d](https://github.com/michaelasper/kt/commit/96bee0d))

## <small>0.1.1 (2026-05-10)</small>

* fix: add back conventional-changelog-conventionalcommits package ([f19169e](https://github.com/michaelasper/kt/commit/f19169e))
* ci: update GitHub workflows with modern actions and fix semantic-release ([ef14094](https://github.com/michaelasper/kt/commit/ef14094))
