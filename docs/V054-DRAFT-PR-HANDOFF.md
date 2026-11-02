# v0.5.4 community features — draft handoff (2026-11-02)

> **Draft / not merge-ready.** This document describes the current fork state, known gaps, and verification evidence. It is intended for upstream review and possible joint development, not approval for deployment.

## Branch and upstream status

- Source: `Maidenquinn/SurvivorsForge`, branch `rebuild/v054-clean`.
- Target: `Rezondes/SurvivorsForge`, `main`.
- At preparation time, GitHub comparison showed **30 commits ahead and 3 commits behind** upstream `main`. Rebase/merge against latest upstream before marking ready.
- The branch was reconstructed from a clean v0.5.4 base after difficult merges. Please review the actual diff rather than relying on earlier integration branches.

## Implemented in this branch

| Area | Current implementation | Verification |
| --- | --- | --- |
| Release synchronization | Existing version `published_at` refreshed from GitHub release metadata | API regression test `existing_release_refreshes_published_date_without_rescanning` passed |
| Version history | Paginated API endpoint and website pager, ordering and boundary regression coverage | API `paged_versions_keep_order_and_handle_boundaries` and web `version_history_pagination_keeps_page_links_and_boundaries` passed |
| Internal issues | Database migrations, API routes, web pages, issue filtering | Rust workspace compilation/tests exercised; browser-level review outstanding |
| Addon comments | Database migrations, API routes, web pages, threaded reply controls and dynamic paging/submission JavaScript | Rust unit tests exercised; end-to-end browser interactions outstanding |
| Moderation | Issue/comment permissions and moderation-related routes/structures | Security/permission matrix and manual role-based review outstanding |
| Localization | 14-locale `i18n.js`, language selector, template attributes, translation audit/check scripts | Rust localization tests passed; static asset serving and browser behavior require validation |

## Explicitly NOT complete

1. **GitHub Issues two-way integration is NOT implemented.** Internal issues are not equivalent to mirroring issues/comments to GitHub. Intended behavior: public read of GitHub issues/comments, GitHub-authenticated posting from the site, bidirectional synchronization, stable external references, and deduplication. Ownership of this remaining feature can be coordinated with Rezondes.
2. **Scan-result freshness after release asset replacement** still needs a focused audit and regression test; do not assume stale findings are already suppressed.
3. **Localization asset route:** the latest full Rust test run reported `multilingual_navigation_and_catalog_are_served` failing with HTTP **404** for `/static/i18n.js` in the web test harness. Check whether the static mount is absent from the test harness or production routing before treating this as resolved.
4. Browser E2E, real local Docker deployment verification, keyboard/mobile interaction checks, moderation abuse cases, and complete UX review remain outstanding.
5. The paginated versions API currently loads lightweight ordering keys before slicing the page; consider bounded SQL pagination for large histories.
6. Further pagination/lazy loading across galleries, issue discussions, pack views and administration is not claimed complete.

## Test evidence (local Docker, reported 2026-11-02)

Command: `docker exec forge-v054-rust-tests cargo test --workspace --locked`

- API regression tests for published dates and version pagination: **passed**.
- Web admin tests: **34/34 passed**.
- Web creator tests: **40/40 passed**.
- Web packs tests: **16/16 passed**, after fixing an outdated navigation assertion.
- Web public tests: **71/72 passed**. The failing test was `multilingual_navigation_and_catalog_are_served`, due to HTTP 404 for `/static/i18n.js` in the test harness.
- The **full workspace run did not pass**. Do not describe it as green.
- Separate JS checks and translation checks do not substitute for browser E2E.

## Suggested review / integration order

1. Fix or correctly scope the localization static asset 404; rerun `cargo test --workspace --locked`.
2. Confirm migrations on a clean PostgreSQL database and verify the local Docker deployment serves the localization runtime.
3. Audit scan finding invalidation after asset replacement.
4. Review API permissions, rate limits, CSRF, sanitization, comment threading, reporting, and issue moderation.
5. Exercise issue/comment UI and locale switching in a browser at desktop and mobile widths.
6. Agree on ownership and data model for GitHub Issues two-way sync before implementation.
7. Refresh against upstream `main`, resolve any conflicts, rerun the full suite, then split into smaller merge-ready PRs if preferred.

## Notes for upstream maintainer

This is deliberately a **draft handoff**, not a request to merge the entire feature set as-is. Please feel free to review or take over individual components. The code is available for collaboration while outstanding tests, GitHub synchronization, and compatibility work are completed.
