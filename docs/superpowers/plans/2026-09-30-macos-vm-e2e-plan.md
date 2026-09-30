# Optional macOS VM acceptance plan: agent session resume

Date: 2026-09-30. **Not started; nothing installed.** Use if manual checks become too slow.
Feature contract: [agent resume spec](../specs/2026-09-30-agent-session-resume-meta-plan.md).

## Environment and setup

Research on this host found an arm64 M3 Pro, 36 GiB RAM, macOS 26.7.1, 95 GiB free and
Parallels Desktop 27.0.1 installed. These are dated observations, not run prerequisites.
Allow about 30–40 GiB for the guest and clones. Proposed automation uses Tart; Parallels
is a fallback. Verify current images, licensing and platform limits before installing:
[Tart](https://tart.run/), [license](https://tart.run/licensing/),
[image templates](https://github.com/cirruslabs/macos-image-templates).

1. Install Tart, clone a compatible `ghcr.io/cirruslabs/macos-tahoe-base` image as `stdusk-e2e`,
   and allocate 4 CPUs / 8 GiB. Start it with VNC and obtain its IP with `tart ip`.
2. Enable SSH key access and verify a logged-in graphical session, auto-login, Accessibility,
   Screen Recording and `osascript`/`screencapture` access. Image defaults and TCC client names
   can change; verify rather than assuming grants or modifying TCC blindly.
3. Install the real Claude/Codex CLIs and record versions. The current probe baseline is
   Claude 2.1.285 / Codex 0.159.2; capture has no version gate.
4. Build/sign the `.app` as in the release workflow and copy it to `/Applications` in the guest.
5. Manually authenticate both CLIs, answer first-run/trust prompts and enable Open at login.
   Approve in System Settings > Login Items if required.
6. Stop the guest and clone a local golden image. Clone that image for each test run.

## Acceptance cases and evidence

Drive app UI over SSH/osascript or VNC. Check `session.toml`, per-pane cwd and exact process
argv first; screenshots supplement those assertions. Use distinct known conversation IDs,
not merely the presence of any `--resume` command. Send a first turn in fresh Codex sessions.

- Two Claude tabs in one repo plus a split; `/clear` in one changes only that pane's ID.
- Two Codex panes in one repo, spaced starts; `/new` followed by a turn changes the owner only.
  Record ambiguous simultaneous starts separately, according to the spec's known limits.
- Cmd+Q, confirm and relaunch: each eligible pane resumes its saved ID in its saved cwd.
- Kill only an identified test agent: crash hint is typed without Enter. Kill the test stdusk
  instance: relaunch uses the last durable snapshot. Do not target unrelated host processes.
- Prefill types without Enter; off leaves ordinary shells. Missing transcript/cwd and duplicates
  produce the expected grouped toast and pane notices.
- Reboot the guest: Open at login starts stdusk and restores eligible sessions. Reacquire its IP;
  detect whether the host's `tart run` must also be restarted. Preserve evidence before deletion.

## Limits and cleanup

Reboot behavior has not been verified. Run one guest at a time; a reported Virtualization.framework
slot leak can require a host reboot ([issue 564](https://github.com/cirruslabs/tart/issues/564)).
Logout may require VNC login even with auto-login; prefer reboot for this test.
Use revocable credentials in a disposable guest. Never publish a logged-in image; revoke credentials
and delete run/golden images afterward. Expired credentials require another manual login.
