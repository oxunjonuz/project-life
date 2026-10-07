#!/usr/bin/env python3
"""Do the tests actually bite? One deliberate fault at a time, on a copy.

For every mutation below the source is copied to a scratch tree, one fault is introduced, and the
test that is supposed to catch it is run:

  * the pristine tree must PASS that test (otherwise a red result proves nothing),
  * the mutated tree must FAIL it (a "test failure", not a compile error).

A mutation that survives is printed as SURVIVED and makes the script exit non-zero. Nothing is
touched in the real project: the campaign runs entirely inside --work.

Usage: python3 tools/mutations.py [--src /work/projectlife] [--work /tmp/projectlife-mut]
"""

import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys

# (id, file, [(old, new), ...], test that must catch it, what the fault is)
MUTATIONS = [
    (
        "M1-count-skipped-dirs-every-cycle",
        "src/scan.rs",
        [("                } else if count_skipped {", "                } else if true {")],
        "ordinary_cycle_does_not_count_a_skipped_directory",
        "counting a skipped directory on every ordinary cycle (the round-288 behaviour)",
    ),
    (
        "M2-window-truncated-by-value",
        "src/scan.rs",
        [
            (
                """    let (med, p95) = if intervals.is_empty() {
        (0, 0)
    } else {
        let mut sorted = intervals.clone();
        util::median_p95(&mut sorted).unwrap_or((0, 0))
    };""",
                """    intervals.sort_unstable();
    let (med, p95) = if intervals.is_empty() {
        (0, 0)
    } else {
        (intervals[intervals.len() / 2], intervals[intervals.len() - 1])
    };""",
            )
        ],
        "observed_window_is_truncated_by_age_not_by_value",
        "sorting the interval list in place before truncating by position",
    ),
    (
        "M3-no-recovery-before-observation",
        "src/scan.rs",
        [
            (
                """    // FR-LIF-3: an interrupted prune is resolved before anything else touches this project.
    if let Some(what) = crate::lifecycle::recover_prune(archive, project)? {
        archive.log(&format!("{}: {what}", project.name));
    }
""",
                "",
            )
        ],
        "prune_killed_at_the_very_start_rolls_back",
        "an interrupted prune is never recovered",
    ),
    (
        "M4-import-into-existing-project-allowed",
        "src/lifecycle.rs",
        [("            if !j.events.is_empty() {", "            if false {")],
        "import_into_an_existing_project_is_refused_and_changes_nothing",
        "importing old history into a project that already has a journal",
    ),
    (
        "M5-stale-lock-not-taken-over",
        "src/archive.rs",
        [("                        Some(pid) => !pid_alive(pid),", "                        Some(_pid) => false,")],
        "dead_holder_lock_is_taken_over",
        "a lock left by a dead process blocks the next cycle forever",
    ),
    (
        "M6-symlink-guard-removed-from-read-loop",
        "src/scan.rs",
        [
            (
                """        if cf.kind == "symlink" {
            // Symlinks are only recorded as events (section 3); their targets are never read, and
            // this `continue` is the only place that decides it for this loop. `read_stable` also
            // refuses to follow a symlink (O_NOFOLLOW), so the rule holds even if this guard is
            // ever removed by accident — that is what the mutation control in tests/acceptance.rs
            // checks.
            continue;
        }
""",
                "",
            )
        ],
        "symlink_to_secret_outside_never_becomes_a_blob",
        "the content-read loop no longer excludes symlinks (only O_NOFOLLOW is left standing)",
    ),
    (
        "M7-both-symlink-guards-removed",
        "src/scan.rs",
        [
            # both guards go: the loop's exclusion AND the reader's refusal — this is the round-287
            # defect end to end (with only one of them removed the behaviour is still correct).
            (
                """        if cf.kind == "symlink" {
            // Symlinks are only recorded as events (section 3); their targets are never read, and
            // this `continue` is the only place that decides it for this loop. `read_stable` also
            // refuses to follow a symlink (O_NOFOLLOW), so the rule holds even if this guard is
            // ever removed by accident — that is what the mutation control in tests/acceptance.rs
            // checks.
            continue;
        }
""",
                "",
            ),
            (
                """        if md1.file_type().is_symlink() {
            return Err("refusing to read through a symlink (the link is stored as a link)".into());
        }
""",
                "",
            ),
            (
                "        fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?",
                "        fs::OpenOptions::new().read(true).open(path)?",
            ),
        ],
        ["read_stable_refuses_to_follow_a_symlink", "symlink_to_secret_outside_never_becomes_a_blob"],
        "the reader dereferences symlinks (the round-287 defect)",
    ),
    (
        "M10-move-not-detected-by-identity",
        "src/scan.rs",
        [
            (
                """            if let Some((old, _e)) = movemap.get(rel) {
                moved_from = Some(old.clone());
            } else if let Some((old, _e)) = missing""",
                """            if let Some((old, _e)) = missing""",
            )
        ],
        "rename_with_content_change_is_move_then_put",
        "a rename is no longer recognised by file identity (only a content match is left)",
    ),
    (
        "M8-rollback-forgets-the-old-journal",
        "src/lifecycle.rs",
        [
            (
                """                if let Some(old) = newest_dir_with_prefix(&project.tmp_dir(), "events.old-") {
                    fs::rename(&old, project.events_dir()).map_err(|e| format!("rollback: {e}"))?;
                    util::sync_dir(&project.dir);
                }""",
                "",
            )
        ],
        "prune_killed_after_the_first_rename_rolls_back",
        "rollback no longer puts the pre-prune journal back",
    ),
    (
        "M9-swapped-journal-meta-not-corrected",
        "src/lifecycle.rs",
        [
            (
                """            project.set_meta("lastSeq", Value::from(last));
            project.set_meta("historyStartsAt", Value::from(iso_ms(before)));
            project.save_meta()?;
            let removed = delete_unreferenced_blobs(project)?;
            remove_stale_prune_dirs(project);
            format!("completed: the swapped journal was kept, metadata corrected, {removed} blobs deleted")""",
                """            let _ = last;
            let removed = delete_unreferenced_blobs(project)?;
            remove_stale_prune_dirs(project);
            format!("completed: the swapped journal was kept, metadata NOT corrected, {removed} blobs deleted")""",
            )
        ],
        "prune_killed_after_the_journal_swap_completes",
        "the new journal is kept but the metadata still describes the old history",
    ),
    (
        "M11-trigger-never-fires",
        "src/daemon.rs",
        [("        let trigger_due = !dirty.is_empty()", "        let trigger_due = false && !dirty.is_empty()")],
        "a_notification_starts_a_pass_long_before_the_interval",
        "notifications no longer start a pass (only the 60 s interval is left)",
    ),
    (
        "M12-debounce-ignored",
        "src/daemon.rs",
        [
            (
                "    let mut debounce_ms = archive.config.i64_of(\"debounceMs\", 1500).clamp(0, 600_000);",
                "    let mut debounce_ms = 0;",
            )
        ],
        "a_notification_starts_a_pass_long_before_the_interval",
        "the 1.5 s debounce is ignored and a pass starts the instant a byte lands",
    ),
    (
        "M13-new-directory-not-watched",
        "src/watch.rs",
        [
            (
                """                if !self.path_wd.contains_key(&(root_idx, rel.clone())) {
                    self.add(root_idx, &rel);
                }
""",
                "",
            )
        ],
        "a_directory_created_while_the_daemon_runs_is_watched_at_once",
        "a directory created while the daemon runs never joins the watch set",
    ),
    (
        "M14-second-daemon-not-refused",
        "src/daemon.rs",
        [
            (
                "    let guard = archive.daemon_lock(\"daemon run\")?;",
                "    let guard: Option<crate::archive::DaemonLock> = None;",
            ),
            ("    guard.release();", "    if let Some(g) = guard {\n        g.release();\n    }"),
        ],
        "a_second_daemon_exits_with_a_clear_message_and_the_first_keeps_working",
        "a second daemon start is no longer refused (two daemons may write)",
    ),
    (
        "M15-excluded-directories-watched",
        "src/watch.rs",
        [
            (
                """            if !r.filter.decide_dir(&child_rel, &ap, &r.ignore, &r.gitignore).track {
                continue;
            }
""",
                "",
            )
        ],
        "an_excluded_directory_is_not_watched_and_its_changes_trigger_nothing",
        "the walk into the watch set ignores the filters: node_modules gets watched",
    ),
    (
        "M16-drop-injector-disabled",
        "src/watch.rs",
        [("        if self.drop_events {", "        if false && self.drop_events {")],
        "lost_notifications_do_not_lose_versions",
        "the fault injector stops injecting (the test must notice the injector is inert)",
    ),
    (
        "M17-overflow-not-reported",
        "src/watch.rs",
        [("        if overflow {", "        if false && overflow {")],
        "a_queue_overflow_is_reported_and_costs_no_version",
        "a real kernel queue overflow is not reported and starts no pass",
    ),
    (
        "M18-interval-unit-error",
        "src/daemon.rs",
        [
            (
                "    let mut interval_ms = archive.config.i64_of(\"intervalSeconds\", 5).max(1) * 1000;",
                "    let mut interval_ms = archive.config.i64_of(\"intervalSeconds\", 5).max(1);",
            )
        ],
        "a_notification_starts_a_pass_long_before_the_interval",
        "the interval is read as seconds where milliseconds are expected (the round-290 bug)",
    ),
    (
        "M19-detection-reads-content",
        "src/detect.rs",
        [
            (
                """            if !ft.is_file() {
                // FIFOs, sockets, devices: never opened, only counted.""",
                """            if ft.is_file() {
                // MUTANT: content sniffed during detection
                let _ = std::fs::read(e.path());
            }
            if !ft.is_file() {
                // FIFOs, sockets, devices: never opened, only counted.""",
            )
        ],
        "at44_detection_does_not_read_content",
        "detection reads file content (the atime of every file must move and the test must notice)",
    ),
    (
        "M20-secrets-auto-included",
        "src/filters.rs",
        [
            (
                """        if !self.include_secrets {
            if let Some(pat) = self.secret_patterns.iter().find(|p| matches_anywhere(p, rel)) {
                return Decision::skip(R_SECRET, pat);
            }
        }
""",
                "",
            )
        ],
        "at43_secrets_never_auto_included",
        "the secret rule is gone: a forced include list drags .env into the archive",
    ),
    (
        "M21-markers-ignored",
        "src/detect.rs",
        [
            (
                "        let marker_score = if markers_hit.is_empty() { 0.0 } else { 1.0 };",
                "        let marker_score = 0.0;",
            )
        ],
        "at40_developer_folder_detection",
        "markers stop contributing: package.json no longer says anything about the folder",
    ),
    (
        "M22-preset-not-saved",
        "src/cli.rs",
        [
            (
                "            meta.insert(\"preset\".into(), plan.block());",
                "            // MUTANT: the preset block is not written into project.json",
            )
        ],
        "at45_manual_preset_override",
        "the applied preset is not recorded in project.json",
    ),
    # ---------------------------------------------------------------------------------------
    # Round 292 — retention policy, the scheduler checks, the read-only MCP surface.
    # ---------------------------------------------------------------------------------------
    (
        "M23-delete-between-anchors-dropped",
        "src/retention.rs",
        [
            (
                """    for rel in prev.keys() {
        if !state.contains_key(rel) {
            let mut e = events::ev_new(0, ts, "delete");
            events::put_str(&mut e, "path", rel);
            events::put_str(&mut e, "reason", "retention-anchor");
            out.push(e);
        }
    }
""",
                "",
            )
        ],
        "a_policy_prune_does_not_resurrect_a_deleted_file",
        "the difference between two anchors is no longer written, so a deleted file comes back",
    ),
    (
        "M24-boundary-moment-not-materialised",
        "src/retention.rs",
        [
            (
                "    let boundary_anchor = verbatim_from;\n    anchors.push(boundary_anchor);",
                "    let boundary_anchor = verbatim_from;\n    // MUTANT: the boundary of the keep-everything window is planned but not materialised",
            )
        ],
        "a_policy_keeps_every_retained_moment_restorable_exactly",
        "everything newer than the boundary is kept as events but its state is no longer an anchor",
    ),
    (
        "M25-policy-applied-by-the-cycle",
        "src/scan.rs",
        [
            (
                """    // FR-LIF-3: an interrupted prune is resolved before anything else touches this project.
    if let Some(what) = crate::lifecycle::recover_prune(archive, project)? {
        archive.log(&format!("{}: {what}", project.name));
    }
""",
                """    // FR-LIF-3: an interrupted prune is resolved before anything else touches this project.
    if let Some(what) = crate::lifecycle::recover_prune(archive, project)? {
        archive.log(&format!("{}: {what}", project.name));
    }
    // MUTANT: the stored retention policy is applied from the ordinary observation cycle.
    if let Some(spec) = crate::retention::stored_policy(project) {
        if let Ok(policy) = crate::retention::Policy::parse(&spec) {
            let _ = crate::retention::apply(archive, project, &policy, crate::util::now_ms());
        }
    }
""",
            )
        ],
        "a_stored_policy_is_never_applied_by_itself",
        "a stored policy is acted on without a user command (FR-LIF-6)",
    ),
    (
        "M26-one-bucket-per-window",
        "src/retention.rs",
        [
            ("                let key = seg.unit.bucket(ts);", "                let key = 0;")
        ],
        "a_policy_keeps_every_retained_moment_restorable_exactly",
        "the buckets collapse into one, so \"one version per day\" keeps a single version",
    ),
    (
        "M27-secret-content-not-refused",
        "src/mcp.rs",
        [
            (
                """                        if !dec.track {
                            refused.push(json!({"path": rel, "reason": dec.reason, "rule": dec.rule}));
                            continue;
                        }
""",
                "",
            )
        ],
        "mcp_diff_hides_content_of_a_path_the_filters_call_a_secret",
        "pl_diff returns the bytes of a file the filters call a secret",
    ),
    (
        "M28-content-returned-without-being-asked",
        "src/mcp.rs",
        [
            (
                '                if args.get("include_content").and_then(|v| v.as_bool()).unwrap_or(false) {',
                "                if true {",
            )
        ],
        "mcp_diff_hides_content_of_a_path_the_filters_call_a_secret",
        "pl_diff returns contents although the flag was not given",
    ),
    (
        "M29-write-tool-names-forgotten",
        "src/mcp.rs",
        [
            (
                """pub fn write_tool_names() -> Vec<&'static str> {
    vec![
        "pl_restore",""",
                """pub fn write_tool_names() -> Vec<&'static str> {
    vec![
        "not_a_write_tool",""",
            )
        ],
        "mcp_refuses_write_operations_and_unknown_names",
        "a write name is no longer recognised as one, so the refusal stops explaining itself",
    ),
    (
        "M30-rate-limit-off",
        "src/mcp.rs",
        [("        if v.len() >= RATE_LIMIT_PER_SECOND {", "        if false {")],
        "mcp_rate_limiter_bites_and_recovers",
        "the rate limiter never refuses anything",
    ),
    (
        "M31-heartbeat-always-fresh",
        "src/health.rs",
        [
            (
                "    let fresh = age_ms.map(|a| a <= max_age_ms).unwrap_or(false);",
                "    let fresh = true;",
            )
        ],
        "heartbeat_check_exit_codes_are_fresh_zero_stale_one",
        "the heartbeat is reported fresh whatever its age",
    ),
    (
        "M32-healthcheck-never-fails",
        "src/health.rs",
        [
            (
                """    if checks.iter().any(|c| c.level == ERROR) {
        1
    } else if strict && checks.iter().any(|c| c.level == WARN) {
        1
    } else {
        0
    }""",
                "    let _ = strict;\n    let _ = checks;\n    0",
            )
        ],
        "healthcheck_is_one_exit_code_for_a_scheduler",
        "healthcheck always exits 0, so a scheduler is never told",
    ),
    (
        "M33-policy-stored-outside-settings",
        "src/retention.rs",
        [
            (
                "    project.settings_mut().insert(SETTING_KEY.to_string(), Value::from(policy.raw.clone()));",
                "    project.set_meta(SETTING_KEY, Value::from(policy.raw.clone()));",
            )
        ],
        "retention_set_stores_and_never_applies",
        "the policy is written at the top level of project.json instead of settings",
    ),

    # ---------------------------------------------------------------------------------------
    # Round 293 — the partial pass: a notification-driven pass that walks only what was named.
    # ---------------------------------------------------------------------------------------
    (
        "M34-scope-does-not-cover-a-subtree",
        "src/scan.rs",
        [
            (
                """        let mut cur = rel;
        while let Some(i) = cur.rfind('/') {
            cur = &cur[..i];
            if self.dirs.contains(cur) {
                return true;
            }
        }
        false
    }

    pub fn describe(&self) -> String {""",
                """        // MUTANT: a notified directory no longer covers the files inside it.
        false
    }

    pub fn describe(&self) -> String {""",
            )
        ],
        "partial_pass_finds_a_deleted_file_from_a_notification_about_its_parent",
        "a notification about a directory stops covering the files inside it, so a vanished file "
        "whose parent was named is never found",
    ),
    (
        "M35-cross-directory-rename-not-matched",
        "src/scan.rs",
        [
            (
                "                .find(|(o, e)| !used_missing.contains(o) && e.dev == Some(dev) && e.ino == Some(ino))",
                """                .find(|(o, e)| {
                    !used_missing.contains(o)
                        && e.dev == Some(dev)
                        && e.ino == Some(ino)
                        && o.rsplit_once('/').map(|(d, _)| d) == rel.rsplit_once('/').map(|(d, _)| d)
                })""",
            ),
            (
                "                .find(|(o, e)| !used_missing.contains(o) && e.hash == hash && e.size == cf.size)",
                """                .find(|(o, e)| {
                    !used_missing.contains(o)
                        && e.hash == hash
                        && e.size == cf.size
                        && o.rsplit_once('/').map(|(d, _)| d) == rel.rsplit_once('/').map(|(d, _)| d)
                })""",
            ),
        ],
        ["partial_pass_matches_a_rename_between_two_directories",
         "partial_pass_writes_one_move_for_a_rename_inside_a_directory"],
        "a rename is only recognised when both paths are in the same directory — the critical "
        "cross-directory case (the second test is the control: a rename inside one directory must "
        "still work)",
    ),
    (
        "M36-no-move-at-all",
        "src/scan.rs",
        [
            (
                """    // 1. move detected by file identity
    for rel in created_paths.clone() {""",
                """    // MUTANT: a rename is no longer recognised by file identity.
    for rel in Vec::<String>::new() {""",
            ),
            (
                """        let mut moved_from: Option<String> = None;
        if is_new {""",
                """        let mut moved_from: Option<String> = None;
        if false && is_new {""",
            ),
        ],
        ["partial_pass_writes_one_move_for_a_rename_inside_a_directory",
         "partial_pass_matches_a_rename_between_two_directories",
         "partial_pass_writes_a_batch_of_moves_for_a_folder_rename"],
        "a rename is written as delete + put instead of move (the round-287 style loss of history)",
    ),
    (
        "M37-partial-pass-postpones-the-periodic-deadline",
        "src/daemon.rs",
        [
            (
                """        // The periodic deadline is measured from the END of the last pass of any kind, so the
        // interval is a bound on the gap between two passes and a trigger can only shorten it.
        next_periodic = util::now_ms() + interval_ms.max(200);""",
                """        // MUTANT: a notification-driven pass doubles the periodic deadline.
        let scaling = if partial_wanted { 2 } else { 1 };
        next_periodic = util::now_ms() + (interval_ms * scaling).max(200);""",
            )
        ],
        "lost_notification_and_partial_pass_do_not_postpone_the_periodic_pass",
        "a partial pass postpones the periodic full pass — the optimisation becomes the promise",
    ),
    (
        "M38-partial-pass-duplicates-a-version",
        "src/scan.rs",
        [
            ("        if unchanged_meta && !opts.deep {", "        if false && unchanged_meta && !opts.deep {"),
            ('            if o.hash == hash && o.kind == "file" {', '            if false && o.hash == hash && o.kind == "file" {'),
        ],
        "a_partial_pass_and_a_full_pass_do_not_duplicate_a_version",
        "the pass stores a version for contents it already has (both the metadata check and the "
        "same-hash check are removed, because either one alone is enough)",
    ),
    (
        "M39-notified-directory-not-walked",
        "src/scan.rs",
        [
            (
                "        starts.push((path, d));",
                """        // MUTANT: a notified directory's subtree is not walked.
        let _ = (path, d);""",
            )
        ],
        "a_directory_moved_into_the_project_is_walked_and_then_watched",
        "a notification about a directory no longer brings the files inside it into scope",
    ),
    (
        "M40-no-mass-event",
        "src/scan.rs",
        [
            (
                "    if !is_initial && ((big && touched > 0) || all_gone || rewrite) {",
                "    if false && !is_initial && ((big && touched > 0) || all_gone || rewrite) {",
            )
        ],
        "partial_pass_writes_a_mass_delete_for_fifty_plus_files",
        "a mass deletion through a partial pass is written with no mass event and no lastGoodSeq",
    ),
    (
        "M41-partial-pass-walks-the-whole-project",
        "src/scan.rs",
        [
            (
                """        Some(sc) if !sc.full => {
            let w = walk_scoped(&project_root, &filter, &arc_prefixes, &own, &git, opts.count_skipped, sc);
            listed_dirs = w.dirs_read;
            vanished_dirs = w.vanished;
            (w.files, w.skips, w.errors, w.dirs_walked)
        }""",
                """        Some(sc) if !sc.full => {
            // MUTANT: the scope decides what may be written, not what is walked.
            let (f, s, e) = walk(&project_root, &filter, &arc_prefixes, &own, &git, opts.count_skipped);
            let _ = (&sc, &mut listed_dirs, &mut vanished_dirs);
            (f, s, e, 0usize)
        }""",
            )
        ],
        "a_partial_pass_touches_nothing_outside_the_notified_paths",
        "the pass reads and stores files the notification never mentioned (a full pass wearing a "
        "partial pass's name)",
    ),
    (
        "M42-unwalked-paths-dropped-from-the-cache",
        "src/cache.rs",
        [
            (
                """        if self.full {
            let map = std::mem::take(&mut self.new);
            return self.write_base(&map, "full");
        }""",
                """        if self.full || !self.staged.is_empty() {
            let map = std::mem::take(&mut self.new);
            return self.write_base(&map, "full");
        }""",
            )
        ],
        "a_partial_pass_does_not_rewrite_the_cache_base",
        "a partial pass rewrites the whole cache base from the few paths it touched, so every other "
        "entry is lost (the round-293 behaviour)",
    ),
    (
        "M43-deletions-outside-the-scope",
        "src/scan.rs",
        [
            (
                """    let covered = |rel: &str| -> bool { scope.map(|s| s.covers(rel)).unwrap_or(true) };""",
                """    let covered = |_rel: &str| -> bool { true };""",
            ),
            (
                """    let looked = |rel: &str| -> bool {
        may_write_delete(rel, &listed_dirs, &vanished_dirs, whole_project_walked)
    };""",
                """    let looked = |_rel: &str| -> bool { true };""",
            ),
            (
                """    } else {
        for (rel, entry) in cache.candidates(scope)? {""",
                """    } else {
        for (rel, entry) in cache.candidates(None)? {""",
            ),
        ],
        "a_partial_pass_touches_nothing_outside_the_notified_paths",
        "the scope is ignored: every tracked path is a candidate and everything this pass did not "
        "walk counts as deleted (a full pass wearing a partial pass's reason)",
    ),
    # ---- round 301: the stop request (the one stop mechanism every platform has) and the Windows
    #      trigger shape. Two of these are the platform differences this round is about; the rest are
    #      the faults a reviewer would find by reading the new code.
    (
        "M120-the-daemon-ignores-the-stop-request",
        "src/daemon.rs",
        [("        if archive.take_stop_request(std::process::id() as i32).is_some() {", "        if false {")],
        "a_stop_request_stops_the_daemon_it_names",
        "the daemon runs on after the request that named it",
    ),
    (
        "M121-a-request-for-another-process-is-obeyed",
        "src/archive.rs",
        [("        if named == Some(pid) {", "        if named.is_some() {")],
        "a_stop_request_is_honoured_once_and_only_by_the_process_it_names",
        "a request that names somebody else stops this process",
    ),
    (
        "M122-the-stop-request-is-not-consumed",
        "src/archive.rs",
        [("""        if named == Some(pid) {
            let _ = fs::remove_file(&p);
            return Some(line);
        }""",
          """        if named == Some(pid) {
            return Some(line);
        }""")],
        "a_stop_request_is_honoured_once_and_only_by_the_process_it_names",
        "a request is obeyed for ever instead of exactly once",
    ),
    (
        "M123-a-stale-stop-request-is-not-cleared",
        "src/daemon.rs",
        [("    if let Some(stale) = archive.clear_stale_stop_request() {",
          "    if let Some(stale) = None::<String> {")],
        "a_stop_request_that_names_somebody_else_cannot_stop_a_daemon",
        "a request left behind by a dead daemon is left where the next one can trip over it",
    ),
    (
        "M124-a-project-root-is-not-a-path",
        "src/daemon.rs",
        [("            Some((name, rel)) if !name.is_empty() => {",
          "            Some((name, rel)) if !name.is_empty() && !rel.is_empty() => {")],
        "a_notification_that_names_a_whole_project_is_a_pass_over_that_root",
        "the Windows trigger shape (a root, not a file) forces a whole-archive pass",
    ),
    (
        "M125-backslashes-are-not-normalised",
        "src/scan.rs",
        [('    let mut s = raw.replace(\'\\\\\', "/");', "    let mut s = raw.to_string();")],
        "notification_paths_written_with_backslashes_address_the_same_file",
        "a path from the other platform no longer addresses the same file",
    ),
]

# ---------------------------------------------------------------------------------------------
# Round 294 — the bookkeeping. Each fault here removes one piece of the new design; the test that
# must catch it is named in the entry, and every one of them was run against the pristine tree
# first (a red test proves nothing if the test is red anyway).
# ---------------------------------------------------------------------------------------------
ROUND_294 = [
    (
        "M45-partial-pass-reads-the-journal-again",
        "src/scan.rs",
        [
            (
                """    let mut journal_events: Vec<Ev> = Vec::new();
    if partial_walk {
        if !cache.had_state() {""",
                """    let mut journal_events: Vec<Ev> = Vec::new();
    if false {
        if !cache.had_state() {""",
            )
        ],
        "a_partial_pass_works_while_the_journal_is_unreadable_and_a_full_pass_does_not",
        "every pass reads the whole journal again (the round-293 behaviour), which is what made a "
        "notification-driven pass fail when a journal file is unreadable",
    ),
    (
        "M46-delta-overlay-ignored",
        "src/cache.rs",
        [
            (
                """        if self.delta.mentions(path) {
            if let Ok(Some(r)) = self.delta.last(path) {
                return match r.op {
                    Op::Set(e) => Some(e),
                    Op::Del => None,
                };
            }
        }""",
                """        // MUTANT: the delta is not consulted; only the base is read.""",
            )
        ],
        "a_path_that_lives_only_in_the_delta_is_still_known",
        "a path a previous partial pass created is no longer known, so every change to it is "
        "stored as a first sight (a lost version)",
    ),
    (
        "M47-tombstone-ignored",
        "src/cache.rs",
        [
            (
                """        for r in self.delta.covered(sc)? {
            match r.op {
                Op::Set(e) => {
                    out.insert(r.path.clone(), e);
                }
                Op::Del => {
                    out.remove(&r.path);
                }
            }
        }""",
                """        for r in self.delta.covered(sc)? {
            if let Op::Set(e) = r.op {
                out.insert(r.path.clone(), e);
            }
        }""",
            )
        ],
        ["the_cache_store_reads_the_delta_over_the_base",
         "a_tombstone_keeps_a_deleted_path_out_of_the_next_pass"],
        "a tombstone no longer removes anything: a deleted path stays in the cache and the next "
        "pass that looks there reports it deleted a second time",
    ),
    (
        "M48-tracked-count-ignores-the-delta",
        "src/cache.rs",
        [
            (
                """        let tracked = if delta.unwindable.is_empty() {
            (base_entries as i64 + delta.net).max(0) as usize""",
                """        let tracked = if delta.unwindable.is_empty() {
            base_entries.max(0) as usize""",
            )
        ],
        "a_path_that_lives_only_in_the_delta_is_still_known",
        "the count of tracked paths ignores what the delta did, so it drifts from the journal and "
        "the mass-change thresholds are computed on a wrong denominator",
    ),
    (
        "M49-scope-scan-drops-the-named-path-itself",
        "src/cache.rs",
        [
            (
                """                if let Ok(Some(v)) = b.find(d) {
                    out.insert(d.clone(), entry_of(&v)?);
                }""",
                """                // MUTANT: only the subtree under a named directory is looked for.""",
            )
        ],
        "partial_passes_reach_the_same_state_as_full_passes",
        "a notification about a file that has since vanished is classified as a directory, and the "
        "path itself is then never looked up: the deletion is missed",
    ),
    (
        "M50-tail-trusted-without-validation",
        "src/events.rs",
        [
            (
                """    let (seq, ts) = probe_journal(project_dir)?;
    if t.seq_last != seq {""",
                """    let (seq, ts) = probe_journal(project_dir)?;
    let _ = (seq, ts);
    if false {""",
            )
        ],
        "a_tail_that_disagrees_with_the_journal_is_rebuilt_not_trusted",
        "the tail is believed without being checked against the journal, so a stale or lying tail "
        "hands out sequence numbers that already exist",
    ),
    (
        "M51-delta-emptied-before-the-base-is-written",
        "src/cache.rs",
        [
            (
                """    util::write_atomic(&dir.join("base.jsonl"), out.as_bytes()).map_err(|e| e.to_string())?;
    // The one crash window of this format: the base is in place and the delta still holds records
    // that are already folded into it. Folding is idempotent, so the next reader is correct either
    // way — and this is where a test kills a real process to prove it.
    crate::lifecycle::crash_if("cache_base_written");""",
                """    util::write_atomic(&dir.join("delta.jsonl"), b"").map_err(|e| e.to_string())?;
    crate::lifecycle::crash_if("cache_base_written");
    util::write_atomic(&dir.join("base.jsonl"), out.as_bytes()).map_err(|e| e.to_string())?;""",
            )
        ],
        "a_crash_between_the_base_write_and_the_delta_reset_costs_nothing",
        "the delta is emptied before the new base is in place: a crash in that window loses every "
        "record it held, and the state goes back in time",
    ),
    (
        "M52-journal-append-can-join-a-half-written-line",
        "src/cache.rs",
        [
            (
                """    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let created = len == 0;
    if len > 0 {""",
                """    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let created = len == 0;
    if false {""",
            )
        ],
        "a_half_written_last_line_is_not_joined_by_the_next_append",
        "a new record is appended straight onto a half-written last line after a crash, turning two "
        "records into one unreadable line in the middle of the journal",
    ),
]
MUTATIONS.extend(ROUND_294)

# Round 295 — the two things the desktop app asked of the core.
ROUND_295 = [
    (
        "M53-dry-run-writes-anyway",
        "src/cli.rs",
        [(
            "    // the same estimate and the same warnings the real run prints.\n    if a.has(\"dry-run\") {",
            "    // the same estimate and the same warnings the real run prints.\n    if false {",
        )],
        "dry_run_reports_the_estimate_and_writes_nothing_at_all",
        "`pl add --dry-run` falls through and creates the project (the app's coverage step would then be a promise, not a plan)",
    ),
    (
        "M54-iso-moment-refused",
        "src/util.rs",
        [("    if s.contains('T') {", "    if false {")],
        "at_accepts_the_iso_moment_the_program_itself_prints",
        "`--at` refuses the ISO-8601 moment the program prints itself (the app could not load a moment it had just listed)",
    ),
]
MUTATIONS.extend(ROUND_295)


# Round 296 — the interface server's socket: which port it asks for, what it says when refused, and
# who may bind on whose behalf. The last five drive the real binary through tools/ui_bind_test.py,
# because that is where these promises are observable at all.
# Both binaries, built by the command that needs them: `ui_bind_test.py` drives the interface server
# *and* the core it drives, and the campaign copy has no `target/` of its own. Without the core build
# here, this test only passed because an earlier mutant (a core unit test) happened to build it first —
# which meant the campaign was not reproducible for one mutant on its own (`--only M61` refused to run:
# the pristine copy was not green). A test that depends on another test's leftovers is not a test.
APP_TEST = ("sh:cargo build --release >/dev/null 2>&1 && "
            "cargo build --release --manifest-path app/Cargo.toml >/dev/null 2>&1 && "
            "python3 /work/projectlife/tools/ui_bind_test.py --app app/target/release/projectlife-ui "
            "--pl target/release/projectlife --deny /work/projectlife/tools/bind_deny")
ROUND_296 = [
    (
        "M55-kernel-choice-first",
        "app/src/listen.rs",
        [("    let mut v = Vec::new();\n    for i in 0..range.max(1) {", "    let mut v = vec![0];\n    for i in 0..range.max(1) {")],
        "app::listen::tests::the_ladder_prefers_the_asked_for_port_and_ends_with_the_kernels_choice",
        "the ladder asks the kernel for a port first and treats the explicit port as a fallback",
    ),
    (
        "M56-refusal-called-a-busy-port",
        "app/src/listen.rs",
        [('        std::io::ErrorKind::PermissionDenied => "permission",',
          '        std::io::ErrorKind::PermissionDenied => "in_use",')],
        "app::listen::tests::a_permission_refusal_says_no_port_number_will_help",
        "a system refusal is reported as a busy port — the sentence a person acts on becomes wrong",
    ),
    (
        "M57-the-app-is-never-asked",
        "app/src/listen.rs",
        [("    #[cfg(unix)]\n    if let Some(path) = ipc {", "    #[cfg(unix)]\n    if let Some(path) = None::<&Path> {")],
        "app::listen::tests::every_address_denied_is_answered_by_the_app_handing_over_a_socket",
        "when every local address is refused the app is never asked for a socket",
    ),
    (
        "M58-refusal-kept-out-of-the-log",
        "app/src/main.rs",
        [("            for l in &report {\n                log.line(l);\n            }\n", "")],
        APP_TEST,
        "the refusal is shown in the window but never written to daemon.log — the file a person is told to send",
    ),
    (
        "M59-need-socket-not-announced",
        "app/src/main.rs",
        [('        println!("{j}");\n        use std::io::Write;\n        let _ = std::io::stdout().flush();\n        if let Some(p) = announce_log.as_ref() {',
          '        use std::io::Write;\n        let _ = std::io::stdout().flush();\n        if let Some(p) = announce_log.as_ref() {')],
        APP_TEST,
        "the child stops telling the app that it needs a socket, so the handoff can never start",
    ),
    (
        "M60-inherited-descriptor-unchecked",
        "app/src/listen.rs",
        [("    if let Some(fd) = listen_fd {\n        match verify_listener(fd, port) {",
          "    if let Some(fd) = listen_fd {\n        match Ok::<u16, String>(port) {")],
        APP_TEST,
        "an inherited descriptor is served on without being checked (a wrong guess would serve on somebody else's socket)",
    ),
    (
        "M61-verdict-always-optimistic",
        "app/src/diag.rs",
        [("""    let tcp_ok = j["binds"]
        .as_object()""",
          """    let tcp_ok = true
        || j["binds"].as_object()""")],
        APP_TEST,
        "diagnose claims TCP listening works even where every bind was refused (the report would lie about the machine)",
    ),
]
MUTATIONS.extend(ROUND_296)

# Round 296, second half — what the Mac found. The faults below are the three real defects of that
# day (a threshold read as "whichever trips first", a notification per cycle, a window that said
# "Protected" over a daemon that was writing nothing) plus the packaging fault the owner found by
# reading the shell's source on his own machine.
HERE = os.path.dirname(os.path.abspath(__file__))
# The shell cannot run here, so its contract is checked by reading it — and the mutation is applied
# to the *copy's* source while the checker itself comes from the real tree.
SHELL_TEST = "sh:python3 %s/shell_contract_check.py --file app/macos/ProjectLife.m" % HERE

ROUND_296B = [
    (
        "M62-space-threshold-takes-the-larger-number",
        "src/space.rs",
        [("            bytes.min(by_pct)", "            bytes.max(by_pct)")],
        "space::tests::the_smaller_number_wins_where_the_two_rules_disagree",
        "the threshold becomes the larger of the fixed floor and the percentage — the rule is inverted, so a big volume stops writing far too early",
    ),
    (
        "M63-space-stop-rule-read-as-either",
        "src/space.rs",
        [(
            "    let stop = free < stop_threshold;",
            "    let stop = free < stop_threshold\n        || (total.map(|t| t > 0 && (free as f64) * 100.0 / (t as f64) < lim.stop_pct as f64).unwrap_or(false));",
        )],
        "space::tests::on_a_926_gb_volume_1_8_gb_free_is_not_a_stop",
        "FR-DSK-3 read as `free < 500 MB || free % < 1`: on a 926 GB volume with 1.8 GB free the percentage fires and recording stops — the fault the owner measured on his Mac",
    ),
    (
        "M64-notification-on-every-cycle",
        "src/daemon.rs",
        [("    if entered || due {", "    if true {")],
        "a_full_archive_speaks_once_reminds_after_ten_minutes_and_says_when_it_resumes",
        "the archive announces itself once per cycle, which is how a notification centre fills with the same sentence",
    ),
    (
        "M65-final-reminder-ignores-the-clock",
        "src/daemon.rs",
        [(
            "    let due = n.last_notify_ms == 0 || now - n.last_notify_ms >= FULL_REMIND_MS;",
            "    let due = n.last_notify_ms == 0;",
        )],
        "a_full_archive_speaks_once_reminds_after_ten_minutes_and_says_when_it_resumes",
        "the ten-minute reminder never comes: the state is announced once and then never again, which is the opposite failure to the flood",
    ),
    (
        "M66-resume-never-announced",
        "src/daemon.rs",
        [(
            "    let was_blocked = n.state == \"full\" || n.state == \"unknown\";",
            "    let was_blocked = false;",
        )],
        "a_full_archive_speaks_once_reminds_after_ten_minutes_and_says_when_it_resumes",
        "writing resumes silently, so the person never learns that the promise is being kept again",
    ),
    (
        "M67-app-calls-a-full-archive-protected",
        "app/src/api.rs",
        [("    if storage_stop {", "    if false {")],
        "app::api::tests::a_full_archive_is_not_protection_even_with_a_live_daemon",
        "the window asks about the disk again: a live daemon that cannot write is reported as protection (the 2026-10-06 defect)",
    ),
    (
        "M68-app-ignores-a-stale-heartbeat",
        "app/src/api.rs",
        [("    if running && !fresh {", "    if false {")],
        "app::api::tests::a_daemon_that_stopped_reporting_is_not_protection",
        "a daemon that has stopped reporting is still called protection",
    ),
    (
        "M69-shell-looks-in-the-resource-folder-twice",
        "app/macos/ProjectLife.m",
        [(
            "    NSString *byName = [b pathForResource:name ofType:nil];",
            "    NSString *byName = [b pathForResource:name ofType:nil inDirectory:@\"Resources\"];",
        )],
        SHELL_TEST,
        "the lookup goes back to Contents/Resources/Resources, which is where nothing lives — the reason the window said \"reinstall\" on the owner's Mac",
    ),
    (
        "M70-shell-launches-a-folder",
        "app/macos/ProjectLife.m",
        [("        if (isDir) {", "        if (false) {")],
        SHELL_TEST,
        "a plain file check is dropped, so a folder can be handed to NSTask as the program to run",
    ),
    (
        "M71-shell-claims-protection-without-looking-at-the-disk",
        "app/macos/ProjectLife.m",
        [('        NSString *state = p[@"state"] ?: @"unknown";', '        NSString *state = @"protected";')],
        SHELL_TEST,
        "the menu bar says Protected whatever the app server reports about the disk",
    ),
    (
        "M72-window-uses-the-browsers-prompt",
        "app/ui/app.js",
        [(
            "  const name = await askText(t('import_name'), dir.split('/').filter(Boolean).pop() || 'imported');",
            "  const name = window.prompt(t('import_name'), dir.split('/').filter(Boolean).pop() || 'imported');",
        )],
        SHELL_TEST,
        "the import name is asked for through a browser dialog the macOS shell does not implement: the import dies in silence again",
    ),
]
MUTATIONS.extend(ROUND_296B)

# Round 297 — the window's own layer, and the structured answers it reads.
#
# The window cannot be driven by a cargo test, so its contract is checked by reading it: the page and
# the route table are read by tools/ui_surface_check.py, and that checker is itself proved to fail
# (tools/ui_surface_control.py injects six faults and requires it to catch all six). The mutations
# below use that checker as their test, so a fault injected here is caught by the same rule the
# checker claims to enforce.
UI_TEST = "sh:python3 %s/ui_surface_check.py --root ." % HERE

ROUND_297 = [
    (
        "M76-window-write-without-confirmation",
        "app/src/api.rs",
        [(
            """    if !confirmed(req) {
        return err(409, "this stops observing the folder; the window must ask first");
    }
""",
            "",
        )],
        UI_TEST,
        "the route that stops observing a folder no longer asks the person first — a request alone is enough",
    ),
    (
        "M77-project-selector-forgets-the-choice",
        "app/ui/app.js",
        [("""<select id="ret-project" data-act="pick-project">' + ps.map((p) => '<option' + (toolProject() === p.name ? ' selected' : '') + '>'""",
          """<select id="ret-project">' + ps.map((p) => '<option>'""")],
        UI_TEST,
        "the project selectors are rebuilt without the remembered choice, so a redraw silently moves a retention policy to another project (the fault the round-297 acceptance test found)",
    ),
    (
        "M78-control-with-no-handler",
        "app/ui/app.js",
        [("case 'check': {", "case 'check-nothing': {")],
        UI_TEST,
        "a button the page draws is no longer handled: it looks like a feature and does nothing",
    ),
    (
        "M79-page-calls-a-route-that-does-not-exist",
        "app/ui/app.js",
        [("    S.diff = await api('project/diff?name=", "    S.diff = await api('project/diff-not-there?name=")],
        UI_TEST,
        "the page asks for a route the server does not answer: the window shows an error where a number should be",
    ),
    (
        "M80-cat-json-without-a-cap",
        "src/cli.rs",
        [("        const CAP: usize = 256 * 1024;", "        const CAP: usize = usize::MAX;")],
        "cat_json_caps_what_it_hands_over_and_says_that_it_did",
        "the window is handed a whole 2 GB file instead of the capped prefix, and never says it was cut",
    ),
    (
        "M81-cat-json-calls-everything-text",
        "src/cli.rs",
        [('            "encoding": if text.is_some() { "utf8" } else { "binary" },',
          '            "encoding": "utf8",')],
        "cat_json_returns_the_bytes_of_that_moment_and_marks_binary_as_binary",
        "bytes that are not text are offered as if they were, and the window would draw mojibake",
    ),
    (
        "M82-retention-json-always-says-stored",
        "src/cli.rs",
        [("                let stored = spec.is_some();", "                let stored = true;")],
        "retention_json_reports_the_stored_policy_and_what_it_would_do",
        "a project with no policy is reported as having one — the window would then offer to apply a policy that does not exist",
    ),
    (
        "M83-prune-json-says-applied-in-a-dry-run",
        "src/cli.rs",
        [('            "dryRun": dry_run,\n            "applied": false,',
          '            "dryRun": dry_run,\n            "applied": true,')],
        "prune_json_reports_the_plan_before_it_deletes_and_the_outcome_after",
        "a dry run reports itself as having deleted versions, and the window would tell the person their history was pruned when nothing happened",
    ),
    (
        "M84-why-json-miscounts-the-versions",
        "src/cli.rs",
        [('        "versionCount": versions.len(),', '        "versionCount": versions.len() + 1,')],
        "why_json_agrees_with_blame_json_about_the_same_file",
        "the count the window shows for a file's versions disagrees with the events in the journal",
    ),
    (
        "M85-preview-restores-for-real",
        "src/cli.rs",
        [("    if opts.preview {\n        return Ok(EXIT_OK);\n    }\n    if into_project {",
          "    if false {\n        return Ok(EXIT_OK);\n    }\n    if into_project {")],
        "preview_without_json_writes_nothing_either",
        "the preview stops being a preview: it writes the files the person was only shown (this mutant survived the first full campaign — see FAILURES_297.md F297-13)",
    ),
]
MUTATIONS.extend(ROUND_297)

# M86 arrived after the first full campaign: the parser's own check (tools/moment_input_check.py) was
# written for it, and the defect it mutates is one this round actually shipped for an hour.
MUTATIONS.append((
    "M86-moment-parser-accepts-garbage",
    "app/ui/app.js",
    [(r"""  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})(?:[T ](\d{1,2}):(\d{2})(?::(\d{2}))?)?$/.exec(v);
  if (!m) return null;""",
      """  const m = { 1: '1999', 2: '12', 3: '31' };
  if (false) return null;""")],
    # `--ui` must point at the *copy* the campaign is mutating: without it the checker reads its own
    # tree and passes whatever the mutation did (this is how M86 first "survived").
    "sh:python3 %s/moment_input_check.py --ui app/ui/app.js" % HERE,
    "the strict shape check is replaced by the lenient Date.parse attempt: 'not a date' becomes a "
    "moment (1999-12-31), because JavaScript reads the ':00' as a time zone offset",
))



def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read())
    return h.hexdigest()


def reap_test_daemons():
    """Kill daemons left behind by a test that FAILED.

    A test kills its own daemon at the end; a test that fails half-way never gets there, and the
    daemon it started — pointed at a fixture in /tmp that nobody will ever look at again — lives for
    ever. Round 293 found 46 of them, and they had exhausted this machine's process budget. Only
    daemons whose archive is a test fixture are touched, and only right after a test has finished.

    Round 300: the name pattern was too narrow. It knew `projectlife-test-*` and missed the fixtures
    this round's own daemon tests created (`pl300-cfg-*`), so nine of them were still running against
    archives that had already been deleted. Any daemon whose archive lives under /tmp is a test
    fixture — the owner's archive is on a real volume — so the test is now the path, not the name.
    """
    killed = 0
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open("/proc/%s/cmdline" % pid, "rb") as f:
                cmd = f.read().decode("utf-8", "replace").replace("\0", " ")
        except Exception:
            continue
        if "projectlife" in cmd and "daemon run" in cmd and ("projectlife-test-" in cmd or "/tmp/" in cmd):
            try:
                os.kill(int(pid), 15)
                killed += 1
            except Exception:
                pass
        # Round 300: the same leak, one layer up. The interface server a checker starts is told to
        # quit, but a checker killed by a budget never gets there — and an interface server left from
        # a run two hours ago still holds its port and its log file. Only servers whose command line
        # points into /tmp are touched: the owner's own app never runs from there.
        elif "projectlife-ui" in cmd and ("/tmp/" in cmd or " /tmp " in cmd):
            try:
                os.kill(int(pid), 15)
                killed += 1
            except Exception:
                pass
    return killed


def run_test(work, test):
    env = dict(os.environ)
    # Three kinds of test live in this campaign:
    #   name            a cargo test in the core crate
    #   app::name       a cargo test in the desktop app crate (its own src/)
    #   sh:<command>    any command, run in the copy — used for the interface server's own
    #                   integration test, which drives the built binary rather than a unit test
    if test.startswith("sh:"):
        r = subprocess.run(["/bin/sh", "-c", test[3:]], cwd=work, capture_output=True, text=True, env=env)
        reap_test_daemons()
        out = r.stdout + r.stderr
        if "error[" in out or "could not compile" in out:
            return "compile_error", out
        # A checker may print PASS/FAIL instead of cargo's "test result". Either marker means the
        # test really ran; neither means it did not run at all (round 292's trap), which stays a
        # "not_found" and is never counted as a catch.
        if "PASS" not in out and "FAIL" not in out and "test result" not in out:
            return "not_found", out
        return ("passed" if r.returncode == 0 else "failed"), out
    if test.startswith("app::"):
        env["CARGO_TARGET_DIR"] = os.path.join(work, "app", "target")
        r = subprocess.run(["cargo", "test", "--release", test[5:]], cwd=os.path.join(work, "app"),
                           capture_output=True, text=True, env=env)
        reap_test_daemons()
        out = r.stdout + r.stderr
        if "error[" in out or "could not compile" in out:
            return "compile_error", out
        ran = sum(int(m.group(1)) + int(m.group(2)) for m in
                  re.finditer(r"test result: \w+\. (\d+) passed; (\d+) failed", out))
        if ran == 0:
            return "not_found", out
        return ("passed" if r.returncode == 0 and "FAILED" not in out else "failed"), out
    env["CARGO_TARGET_DIR"] = os.path.join(work, "target")
    # No `--test acceptance`: the name is looked up in every test binary, so a test that lives in
    # another file cannot be silently skipped (that is how the round-292 mutations survived once).
    r = subprocess.run(
        ["cargo", "test", "--release", test],
        cwd=work,
        capture_output=True,
        text=True,
        env=env,
    )
    reap_test_daemons()
    out = r.stdout + r.stderr
    if "error[" in out or "could not compile" in out:
        return "compile_error", out
    ran = 0
    for line in out.splitlines():
        # "test result: ok. 1 passed; 0 failed; …" — a FAILED test still reports 0 passed, so the
        # number that proves something ran is passed + failed.
        m = re.search(r"test result: \w+\. (\d+) passed; (\d+) failed", line)
        if m:
            ran += int(m.group(1)) + int(m.group(2))
    if ran == 0:
        # A test that never ran cannot catch anything: say so instead of calling it a pass.
        return "not_found", out
    if r.returncode == 0 and "FAILED" not in out:
        return "passed", out
    return "failed", out





# ---------------------------------------------------------------------------- round 298
# The owner's report of 2026-10-06 had one part that survived into today's build: when writing is
# stopped for want of room, the diagnostics blamed the scheduler and offered commands that cannot
# write. And the round's own lesson — he reported on a bundle that had been replaced hours before —
# is why the app now says which build it is, so an answer that is constant or remembered has to die.
ROUND_298 = [
    (
        'M87-heartbeat-check-ignores-the-full-disk',
        'src/cli.rs',
        [
            ('            if hb.storage.stop {', '            if false {'),
        ],
        'heartbeat_check_on_a_full_disk_names_the_disk_and_not_the_scheduler',
        'the cron recipe goes back to telling the owner to start a daemon that is already running and to run a pass that cannot write',
    ),
    (
        'M88-healthcheck-blames-the-schedule',
        'src/health.rs',
        [
            ('    if hb.storage.stop {', '    if false {'),
        ],
        'healthcheck_names_space_as_the_reason_nothing_is_recorded',
        "the heartbeat line calls a full archive 'nothing has been observed recently' and hands out a remedy that cannot write",
    ),
    (
        'M89-doctor-offers-a-remedy-that-deletes-nothing',
        'src/doctor.rs',
        [
            ('                fix: if space.stop {\n                    Some(\n                        "write is stopped until there is room: free space on that volume, or drop versions with \\\n                         `pl prune <project> --policy \\"7d:all,30d:1/day,365d:1/month\\"` (without --dry-run it performs \\\n                         the plan; --dry-run only prints it)"\n                            .into(),\n                    )\n', '                fix: if space.stop {\n                    Some(\n                        "pl prune <project> --policy \\"7d:all,30d:1/day,365d:1/month\\" --dry-run".into(),\n                    )\n'),
        ],
        'doctor_offers_a_remedy_that_can_free_space',
        'the fix for a full archive is the dry run again: it prints the plan, deletes nothing and frees nothing',
    ),
    (
        'M90-build-identity-is-a-constant',
        'app/src/api.rs',
        [
            ('            Some(p) => match pl::file_hash_cached(&p) {', '            Some(p) => match pl::file_hash_cached(&p).map(|(b, _h)| (b, "0".repeat(64))) {'),
        ],
        'app::the_build_identity_is_the_hash_of_the_file_on_disk',
        "the app answers 'which build am I?' with a fixed string — the very ambiguity this round's work exists to end",
    ),
    (
        'M91-build-identity-remembered-forever',
        'app/src/pl.rs',
        [
            ('            if *s == size && *m == mtime {\n                return Ok((size, h.clone()));\n            }', '            if true {\n                let _ = (s, m, mtime);\n                return Ok((size, h.clone()));\n            }'),
        ],
        'app::the_build_identity_is_the_hash_of_the_file_on_disk',
        'a replaced binary keeps being reported under the hash of the file it replaced',
    ),
]
MUTATIONS.extend(ROUND_298)


# ---------------------------------------------------------------------------- round 299
# The menu. The failure this round invites is a menu of entries that *look* like features: a label
# with nothing behind it, a command the core does not have, a page that never receives the click.
# The checker is tools/menu_contract_check.py — the rules are the ones the owner can run himself,
# and tools/menu_control.py proves the checker can fail (eleven faults injected, eleven caught).
#
#   MENU_STATIC  reads the sources: registry, page, route table, shell.
#   MENU_LIVE    builds the copy and drives the real server: every entry is run, the output is
#                compared with the core's own, the confirmations are tried without and with asking.
MENU_STATIC = "sh:python3 %s/menu_contract_check.py --root . --static-only" % HERE
MENU_LIVE = ("sh:cargo build --release >/dev/null 2>&1 && "
             "cargo build --release --manifest-path app/Cargo.toml >/dev/null 2>&1 && "
             "python3 %s/menu_contract_check.py --root . --work /tmp/pl-menu-mut" % HERE)

ROUND_299 = [
    (
        'M92-menu-entry-names-a-command-the-core-does-not-have',
        'app/src/menu.rs',
        [('"pl detect <path> --json", &["detect", "{input}", "--json"]',
          '"pl frobnicate <path>", &["detect", "{input}", "--json"]')],
        MENU_STATIC,
        'a menu entry advertises a command the core does not have — the entry would fail the moment it was used',
    ),
    (
        'M93-menu-view-entry-names-a-view-that-is-not-drawn',
        'app/src/menu.rs',
        [('"pl version", &[], "", "about",', '"pl version", &[], "", "aboutx",')],
        MENU_STATIC,
        'an entry opens a view the window cannot draw: clicking it would leave the window on the old screen',
    ),
    (
        'M94-menu-page-entry-is-not-wired-into-the-page',
        'app/ui/app.js',
        [("  'file.export': () => exportHistory(),\n", "")],
        MENU_STATIC,
        'a page entry with no flow behind it: the menu shows "Export the history…" and the click does nothing',
    ),
    (
        'M95-menu-runner-accepts-an-id-outside-the-registry',
        'app/src/menu.rs',
        [("pub fn find(id: &str) -> Option<&'static Item> {\n    ITEMS.iter().find(|i| i.id == id)\n}",
          "pub fn find(id: &str) -> Option<&'static Item> {\n    ITEMS.iter().find(|i| i.id == id).or(Some(&ITEMS[0]))\n}")],
        MENU_LIVE,
        'the menu endpoint becomes a general command runner: any id it does not know is quietly performed as the first entry',
    ),
    (
        'M96-menu-runs-a-change-without-the-confirmation',
        'app/src/api.rs',
        [("    if item.kind == crate::menu::Kind::Confirm && !confirmed {", "    if false {")],
        MENU_LIVE,
        'an entry that changes what is stored runs without the window having asked — the confirmation becomes decoration',
    ),
    (
        'M97-menu-splits-a-value-into-several-arguments',
        'app/src/menu.rs',
        [("        out.push(filled);",
          "        for part in filled.split_whitespace() { out.push(part.to_string()); }\n        continue;")],
        MENU_LIVE,
        'a value with a space in it becomes two arguments: "my notes.txt" is searched as "my" and "notes.txt"',
    ),
    (
        'M98-menu-passes-a-flag-shaped-value-on',
        'app/src/menu.rs',
        [("        if v.starts_with('-') {", "        if false {")],
        MENU_LIVE,
        'a label beginning with a dash reaches the core and is read as an option — the mark is saved with the wrong label and nothing says so',
    ),
    (
        'M99-page-stops-reading-the-menu-from-the-server',
        'app/ui/app.js',
        [("    MENU = await api('menu' + q);", "    MENU = { groups: [] };")],
        MENU_STATIC,
        'the window stops asking the server for the menu: the bar would be a copy that drifts from what the app can do',
    ),
    (
        'M100-shell-stops-taking-the-menu-from-the-server',
        'app/macos/ProjectLife.m',
        [("    NSDictionary *doc = [self menuDocument];", "    NSDictionary *doc = nil;")],
        MENU_STATIC,
        'the macOS menu bar stops being drawn from the server\'s answer, so it can offer entries the app cannot perform',
    ),
    (
        'M101-menu-lets-a-change-happen-without-asking',
        'app/src/menu.rs',
        [('Confirm, Both,\n        "pl recover <project>"', 'Run, Both,\n        "pl recover <project>"')],
        MENU_LIVE,
        'an entry that changes the archive is declared as a plain read: the safety rule that separates the two is what makes the menu honest',
    ),
]
MUTATIONS.extend(ROUND_299)



# ---------------------------------------------------------------------------- round 300
# Three new promises, and the faults that would quietly remove each one.
#
#   repair      "creates only; never overwrites, never deletes" — the fault is the guard, in the plan
#               and again at write time (a plan is a picture of the past; the write is the present).
#   the ledger  "the program's own messages are still readable tomorrow" — the fault is a line that
#               stops carrying the project, the kind, or the command that repairs the damage.
#   FR-CFG-3    "a configuration change applies without a restart" — the fault is a comparison that is
#               always equal, a handler that is never installed, or an interval that is read once.
ROUND_300 = [
    (
        'M102-repair-overwrites-because-the-plan-stops-looking',
        'src/restore.rs',
        [("        if opts.missing && fs::symlink_metadata(&dst).is_ok() {\n            // Something already occupies this path",
          "        if false {\n            // Something already occupies this path")],
        'a_repair_puts_back_only_what_is_missing_and_leaves_everything_else_alone',
        'the repair plans an overwrite for every file that exists: it would revert the hand-written work it exists to protect',
    ),
    (
        'M103-repair-overwrites-in-the-window-between-plan-and-write',
        'src/restore.rs',
        [("        if opts.missing && fs::symlink_metadata(&dst).is_ok() {\n            // The plan said this path was empty",
          "        if false {\n            // The plan said this path was empty")],
        'a_file_that_appears_between_the_plan_and_the_write_is_never_overwritten',
        'the rule holds in the plan but not at the moment of writing: a file created in between is destroyed',
    ),
    (
        'M104-repair-and-clean-both-run',
        'src/restore.rs',
        [('    if opts.missing && opts.clean {\n        return Err("--missing only creates; --clean deletes. Choose one: --missing never touches a file that exists".into());\n    }\n',
          '')],
        'a_plan_says_which_mode_built_it_and_refuses_the_two_flags_that_disagree',
        'two contradictory instructions are accepted together instead of being refused by name (the CLI guard alone is not the promise; the library refuses too)',
    ),
    (
        'M105-restore-missing-ignores-its-own-flag',
        'src/restore.rs',
        [("        missing: opts.missing,", "        missing: false,")],
        'a_plan_says_which_mode_built_it_and_refuses_the_two_flags_that_disagree',
        'the plan reports itself as an ordinary restore while behaving as a repair — every reader of the plan is misled (checked in both directions, so a constant cannot satisfy it)',
    ),
    (
        'M106-repair-forgets-how-many-files-it-left-alone',
        'src/restore.rs',
        [("            present += 1;\n            continue;", "            continue;")],
        'a_repair_puts_back_only_what_is_missing_and_leaves_everything_else_alone',
        'the files that were left alone stop being counted: "nothing was touched" becomes unverifiable',
    ),
    (
        'M107-nothing-is-written-to-the-ledger',
        'src/daemon.rs',
        [("    append_notification(archive, kind, project, title, body);", "    let _ = (kind, project);")],
        'a_mass_deletion_leaves_a_readable_line_with_the_project_and_the_repair_command',
        'notifications go back to being a toast that vanishes: nothing survives to be read the next day',
    ),
    (
        'M108-the-ledger-line-forgets-which-project-it-was-about',
        'src/daemon.rs',
        [("    if let Some(p) = project {\n        v[\"project\"] = Value::from(p);\n    }", "")],
        'a_mass_deletion_leaves_a_readable_line_with_the_project_and_the_repair_command',
        'a mass deletion is recorded without the project it happened to: the line cannot lead anywhere',
    ),
    (
        'M109-the-ledger-ignores-its-own-limit',
        'src/daemon.rs',
        [("        rows.drain(0..rows.len() - limit);", "        let _ = limit;")],
        'the_ledger_is_what_the_core_reads_back_and_the_limit_is_respected',
        'a request for the last two messages returns the whole history instead',
    ),
    (
        'M110-the-mass-line-loses-the-repair-command',
        'src/daemon.rs',
        [('\\nPut back only what is gone: pl restore {} --at \\"{}\\" --missing --into-project --yes', ''),
         ('                    project.name,\n                    project.name,\n                    project.name,\n                    util::fmt_local(good_ts)\n                );',
          '                    project.name,\n                    util::fmt_local(good_ts)\n                );')],
        'a_mass_deletion_leaves_a_readable_line_with_the_project_and_the_repair_command',
        'the notification names the accident but not the one command that repairs it without touching anything else',
    ),
    (
        'M111-every-ledger-line-claims-the-same-kind',
        'src/daemon.rs',
        [('        "kind": kind,', '        "kind": "notice",')],
        'a_mass_deletion_leaves_a_readable_line_with_the_project_and_the_repair_command',
        'the kind becomes a constant: filtering for the mass changes finds nothing, and the screen shows one undifferentiated stream',
    ),
    (
        'M112-the-configuration-file-is-never-noticed-to-have-changed',
        'src/archive.rs',
        [("        if fp == self.config_fp {\n            return None;\n        }", "        if true {\n            return None;\n        }")],
        'a_configuration_file_that_changed_is_re_read_and_the_changed_keys_are_named',
        'FR-CFG-3 disappears: the running process keeps the configuration it started with and never says so',
    ),
    (
        'M113-a-read-that-changed-nothing-is-reported-as-a-change',
        'src/archive.rs',
        [("        if fp == self.config_fp {\n            return None;\n        }",
          "        if fp == self.config_fp {\n            return Some(Vec::new());\n        }")],
        'a_configuration_file_that_changed_is_re_read_and_the_changed_keys_are_named',
        'every poll claims a configuration change: a log full of re-reads that never happened',
    ),
    (
        'M114-the-daemon-keeps-the-interval-it-started-with',
        'src/daemon.rs',
        [('    if has("intervalSeconds") || has("minIntervalSeconds") || has("maxIntervalSeconds") || has("autoInterval") {',
          '    if false && (has("intervalSeconds") || has("minIntervalSeconds") || has("maxIntervalSeconds") || has("autoInterval")) {')],
        'a_running_daemon_applies_a_configuration_change_without_a_restart',
        'the configuration is re-read and logged, but the interval in force never changes: the change applies in words only',
    ),
    (
        'M115-the-notification-trigger-cannot-be-turned-off-live',
        'src/daemon.rs',
        [('    if has("watchTriggers") && triggers.is_none() {', '    if false && has("watchTriggers") && triggers.is_none() {')],
        'the_notification_trigger_can_be_turned_off_while_the_daemon_runs',
        'the key is in the configuration and in the documentation, and switching it does nothing until a restart',
    ),
    (
        'M116-the-record-of-a-re-read-is-always-zero',
        'src/archive.rs',
        [('            "reloads": self.config_reloads,', '            "reloads": 0,')],
        'a_running_daemon_applies_a_configuration_change_without_a_restart',
        'the cross-process record claims no re-read has ever happened, so no other process can check the claim',
    ),
    (
        'M117-sighup-is-not-handled',
        'src/daemon.rs',
        [("        libc::sigaction(libc::SIGHUP, &hup, std::ptr::null_mut());", "        let _ = (hup, on_hup as *const () as usize);")],
        'a_running_daemon_applies_a_configuration_change_without_a_restart',
        'the signal the specification names as one of the two ways to re-read is silently ignored',
    ),
]
MUTATIONS.extend(ROUND_300)

# Round 302 — who made it, what it is for, and which version this is.
#
# Two facts that used to be typed by hand in five to twelve files, and had already drifted: the
# owner's name and address (round 302), and the version number (0.9.4 in the VERSION file and the
# shells, 0.9.3 in both Cargo.toml files and the macOS Info.plist). The faults below are the ways
# that drift comes back. Their tests are the two checkers, which run inside the campaign copy.
BRAND_HEADER_TEST = "sh:python3 %s/brand.py --root . check-fresh app/pl_brand.h" % HERE
VERSION_TEST = "sh:python3 %s/version_check.py --root ." % HERE

ROUND_302 = [
    (
        'M126-the-generated-header-no-longer-matches-the-source',
        'app/pl_brand.h',
        [('#define PL_AUTHOR "Oxunjon Ubaydllayev"', '#define PL_AUTHOR "Someone Else"')],
        BRAND_HEADER_TEST,
        'the shells would compile a name that is not the one the core prints and the licence names',
    ),
    (
        'M127-the-source-changes-and-the-header-is-left-behind',
        'src/brand.rs',
        [('pub const AUTHOR: &str = "Oxunjon Ubaydllayev";', 'pub const AUTHOR: &str = "Oxunjon Ubaydllayev.";')],
        BRAND_HEADER_TEST,
        'the name is edited in the one place, and the header the three shells include keeps the old bytes',
    ),
    (
        'M128-the-purpose-sentence-changes-and-nothing-is-regenerated',
        'src/brand.rs',
        [('it keeps everything — every version of every file you protect',
          'it keeps most things — every version of every file you protect')],
        BRAND_HEADER_TEST,
        'the sentence the owner wrote is edited in the source and the shells keep saying the old one',
    ),
    (
        'M129-the-app-invents-the-name-instead-of-asking-the-core',
        'app/src/api.rs',
        [('    match ctx.core().json(&args) {\n        Ok(v) => v,',
          '    match ctx.core().json(&args) {\n        Ok(_v) => json!({"author": "Project Life", "authorEmail": "noreply@localhost"}),')],
        'app::the_program_block_is_the_cores_own_answer',
        'the About screen shows a name nobody wrote: the app stops asking the core and answers from a constant',
    ),
    (
        'M130-the-bootstrap-stops-carrying-the-author',
        'app/src/api.rs',
        [('            "author": program["author"].clone(),', '            "author": Value::Null,')],
        'app::the_program_block_is_the_cores_own_answer',
        'the server still knows who made the program and never tells the window: the About screen goes blank',
    ),
    (
        'M131-the-about-screen-stops-showing-the-address',
        'app/ui/app.js',
        [("  const email = app.authorEmail || p.authorEmail || '';", "  const email = '';")],
        'sh:python3 %s/ui_surface_check.py --root .' % HERE,
        'the address the owner asked for is dropped from the one screen that describes the program',
    ),
    (
        'M132-a-shell-announces-a-version-that-does-not-exist',
        'app/linux/ProjectLife.c',
        [('#define SHELL_VERSION "0.9.5"', '#define SHELL_VERSION "0.9.4"')],
        VERSION_TEST,
        'the Linux shell and the core disagree about which version this is (the state round 301 shipped in)',
    ),
    (
        'M133-the-crate-forgets-which-version-it-is',
        'Cargo.toml',
        [('version = "0.9.5"', 'version = "0.9.3"')],
        VERSION_TEST,
        'the core prints one version and every package file names another — what the owner would read first',
    ),
    (
        'M134-the-plist-token-becomes-a-literal',
        'app/macos/Info.plist',
        [('    <string>@VERSION@</string>', '    <string>0.9.3</string>')],
        VERSION_TEST,
        'the version is typed into the bundle by hand again, which is how it drifted one release behind',
    ),
]
MUTATIONS.extend(ROUND_302)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--work", default="/tmp/projectlife-mut")
    ap.add_argument("--only", default="")
    # The campaign is long. `--slice a:b` runs one part of the list (python slicing), so it can be
    # run in pieces across separate invocations when one run would exceed a command's lifetime.
    ap.add_argument("--slice", default="")
    args = ap.parse_args()
    src, work = os.path.abspath(args.src), args.work

    # Round 297: a campaign run with --work inside the source tree leaves a *mutated copy of the
    # code* lying in a tmp folder of the project. On 2026-10-06 exactly that happened: the owner's
    # other agent read `tmp/verify296/mut/app/macos/ProjectLife.m:631` — a fault injected by this
    # script — and reported it as the current source. A mutation campaign may not leave its
    # damaged copies where someone can mistake them for the program.
    if os.path.commonpath([work, src]) == src:
        print("refusing to run: --work is inside the source tree (%s)" % src)
        print("a campaign copy contains deliberately broken code; it must not sit next to the real sources")
        print("use a work tree outside the project, e.g. --work /tmp/projectlife-mut")
        return 2

    if os.path.isdir(work):
        shutil.rmtree(work)
    os.makedirs(work)
    if os.path.isdir(os.path.join(src, "app")):
        shutil.copytree(os.path.join(src, "app"), os.path.join(work, "app"),
                        ignore=shutil.ignore_patterns("target"))
    for item in ["Cargo.toml", "Cargo.lock", "VERSION", "src", "tests"]:
        s = os.path.join(src, item)
        d = os.path.join(work, item)
        if os.path.isdir(s):
            shutil.copytree(s, d)
        else:
            shutil.copy2(s, d)

    items = MUTATIONS
    if args.slice:
        a, _, b = args.slice.partition(":")
        items = MUTATIONS[int(a or 0):int(b) if b else None]
        print("== slice %s: %d mutation(s) ==" % (args.slice, len(items)))

    pristine = {}
    for _, f, _, _, _ in items:
        pristine[f] = sha(os.path.join(src, f))

    print("== baseline: the untouched copy must pass every test used below ==")
    bad = 0
    for mid, f, edits, test, why in items:
        if args.only and args.only not in mid:
            continue
        for one in ([test] if isinstance(test, str) else test):
            verdict, _ = run_test(work, one)
            if verdict != "passed":
                print("  BASELINE NOT GREEN for %s / %s (%s)" % (mid, one, verdict))
                bad += 1
    if bad:
        print("refusing to run the campaign: the pristine tree is not green")
        return 2


    survivors = []
    not_applied = []
    for mid, f, edits, test, why in items:
        if args.only and args.only not in mid:
            continue
        target = os.path.join(work, f)
        shutil.copy2(os.path.join(src, f), target)
        text = open(target).read()
        ok = True
        for old, new in edits:
            if text.count(old) != 1:
                # Round 297: this line has existed since the campaign was written, and it still
                # counted as a "survivor" — the same word used for "the tests did not notice". The
                # two are different failures and the summary now says which is which.
                print("  %-42s NOT APPLIED (the pattern matched %d times) — the fault was never "
                      "injected, so nothing was tested" % (mid, text.count(old)))
                ok = False
                not_applied.append(mid)
                break
            text = text.replace(old, new)
        if not ok:
            # Not a survivor: nothing was injected, so nothing was measured. Listing it under
            # "SURVIVORS" (as this did) says the tests failed to notice a fault that never existed.
            continue
        open(target, "w").write(text)
        verdicts, out = [], ""
        for one in ([test] if isinstance(test, str) else test):
            v, o = run_test(work, one)
            verdicts.append("%s=%s" % (one, v))
            if v == "failed":
                out = o
        caught = "failed" in [v.split("=")[-1] for v in verdicts]
        print("  %-42s %-9s %s   (%s)" % (mid, verdicts[0].split("=")[-1], "CAUGHT" if caught else "SURVIVED", why))
        if not caught:
            survivors.append(mid)
            for line in out.strip().splitlines()[-12:]:
                print("      | " + line)
        shutil.copy2(os.path.join(src, f), target)

    print("== source unchanged check ==")
    drift = 0
    for f, h in pristine.items():
        if sha(os.path.join(src, f)) != h:
            print("  DRIFT in %s" % f)
            drift += 1
    print("  %d file(s) compared, %d drifted" % (len(pristine), drift))

    print("== result ==")
    if not_applied:
        print("  NOT APPLIED (%d) — the fault was never injected, so nothing about it was tested: %s"
              % (len(not_applied), ", ".join(not_applied)))
    if survivors:
        print("  SURVIVORS (%d): %s" % (len(survivors), ", ".join(survivors)))
        return 1
    if not_applied:
        # A campaign whose faults were not all injected has not finished, even with no survivors.
        return 1
    print("  every mutation was caught by its test")
    return 0 if drift == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
