# Notch Assistant

A Dynamic-Island-style panel for the MacBook notch that shows your FireHydrant on-call rotation and
incidents, built with [Rune](https://github.com/anomalyco/rune). Hover the notch and it springs open; move
away and it closes after a short grace period. On an external display it sits in the middle of the menu
bar as a "virtual notch".

![Open panel](snaps/open.png)

**Demo video:** [demo/notch-assistant.mp4](demo/notch-assistant.mp4) (32 s: hover, tabs, Settings, connecting).

| Collapsed | Incidents | Off hours |
| --- | --- | --- |
| ![](snaps/collapsed.png) | ![](snaps/incidents.png) | ![](snaps/offhours.png) |

## What it shows

- **On-call:** who is primary and secondary now, your current shift with time left, your next shift, and
  a five-week rotation strip like FireHydrant's month view.
- **Incidents:** the team's open incidents with severity, status, age and commander.
- **Collapsed:** a dot left of the notch (green: you're primary, amber: secondary, grey: off call) and the
  current primary's avatar on the right.

Until it's connected it shows mock data, labelled "Mock data · Connect" in the panel.

## Install

With Homebrew (Apple silicon, macOS 13 or later):

```sh
brew install --cask shmileee/tap/notch-assistant      # add --appdir ~/Applications if /Applications needs admin
```

Or from source:

```sh
make install       # builds the app into ~/Applications, starts it, and opens it at login
make uninstall     # stops it and removes the app and the login item (settings and Keychain item stay)
```

`INSTALL_DIR=/Applications` installs it for every user; `LOGIN=0` skips open at login.

Then open the panel, click ⚙, and add your FireHydrant API key and team ID (see [FireHydrant](#firehydrant)).

- **Quitting:** the × in the open panel, or ⌘Q while Settings is in front.
- **Open at login:** a switch at the bottom left of Settings, also in System Settings › General › Login
  Items, or `"<app>/Contents/MacOS/notch-assistant" --login on|off|status`.
- **Upgrading and uninstalling:** the app restarts itself when a new version replaces it and quits when it's
  removed, so `brew upgrade` and `brew uninstall` need no extra step (Homebrew's own quit is skipped on Macs
  whose login comes from single sign-on).
- **Signing:** the app is ad-hoc signed, not notarized. The cask clears the quarantine flag on download.

## FireHydrant

Settings has three fields; environment variables override them:

| Field | Stored in | Override |
| --- | --- | --- |
| API key | login Keychain, service `notch-assistant` (never shown again; paste a new one to replace it) | `FIREHYDRANT_API_TOKEN` |
| Team ID | `~/Library/Application Support/notch-assistant/settings.json` | `FIREHYDRANT_TEAM_ID` |
| Your name on the schedule | same file | `FIREHYDRANT_ME` |

Saving reconnects and tests the connection; the panel then refreshes every five minutes. `NOTCH_MOCK=1`
forces the mock.

An API key acts as a bot user, so "you" on the schedule is found by name: `FIREHYDRANT_ME` (a full name or
FireHydrant user ID), else this Mac account's full name. It makes three read-only calls:

- `GET /v1/current_user`: the token's user, the fallback for "you".
- `GET /v1/teams/{team}/on_call_schedules`: the team's shifts from this Monday to five weeks out. A
  schedule whose name contains "secondary" is the secondary rotation. Back-to-back daily shifts for the same
  person and role count as one rotation.
- `GET /v1/incidents?teams={team}`: the team's active incidents.

To check what it reads without launching the app: `cargo run --release -- --probe` (`--probe-raw` dumps
the JSON).

## Development

```sh
cargo run --release                                        # run it (stop the installed copy first)
cargo test
cargo run -- --scene open --snap snaps/open.png           # scenes: collapsed, open, incidents, offhours
cargo run -- --scene collapsed --script scripts/hover.json --snap mid.png --at 0.75
```

Scenes and demo mode use the mock with the clock pinned to Tue 10 Nov 2026, 14:05, so images are
reproducible. The mock is modelled on a real team's calendar: weekly rotations DB → OP → RW → KB, Monday
09:00 to Friday 17:00, where the secondary is last week's primary. `NOTCH_ME=DB` changes who "you" are in
it (default `OP`).

**Recording the demo video.** `scripts/record-demo.sh` writes `demo/notch-assistant.mp4`. It runs the app
in demo mode (`NOTCH_DEMO=1`: mock data, settings in memory, a simulated connection test, a backdrop over
the desktop) and records the MacBook display while `scripts/demo/driver.swift` drives the pointer and
keyboard. Keep hands off for ~35 s. It needs Screen Recording and Accessibility permission for your
terminal, `ffmpeg` and the Xcode command-line tools. When you add a feature, add its steps to `tour` in
`driver.swift`, and update `Spot` when the layout moves. To check positions, run with `NOTCH_DEMO=1`
(plus `NOTCH_OPEN_SETTINGS=1` to open Settings at launch) and take a screenshot.

**Releasing.** Bump `version` in `Cargo.toml`, commit, then `git tag vX.Y.Z && git push --tags`.
`.github/workflows/release.yml` builds `NotchAssistant-<version>-arm64.zip` on an Apple-silicon runner and
attaches it to the GitHub release (`make dist` makes the same zip locally). Renovate in the tap then
proposes the cask update. `make icon` redraws `assets/AppIcon.icns`.

### How it works

| File | Role |
| --- | --- |
| `src/main.rs` | App setup: a transparent overlay at the top of the display under the pointer, where only the island takes clicks. Also the timers, the minute refresh and the snapshot scenes. |
| `src/notch.rs` | The notch of the current display (`NSScreen.safeAreaInsets`), or a virtual 185-pt one as tall as the menu bar. |
| `src/island.rs` | The hover state machine: `Collapsed → Arming (120 ms) → Open → Closing (400 ms)`, so a passing pointer doesn't open it. |
| `src/view.rs` | The island and its tabs. Beside a real notch, the top bar keeps clear of it. |
| `src/oncall.rs` | The `OnCallSource` trait, the mock, and the `Snapshot` the view renders. |
| `src/firehydrant.rs` | The FireHydrant client, its integration declaration and its refresh thread. |
| `src/integrations.rs` | The integration registry: each one's fields (`Secret` or `Text`), env overrides and connection test. |
| `src/store.rs` | Secrets in the Keychain (written via `security -i`, never on a command line), the rest in settings.json. |
| `src/settings.rs` | The Settings window, built from the registry. |
| `src/login.rs` | Open at login (`SMAppService`); only the app bundle can register. |
| `src/installed.rs` | Restarts the app when its installed copy is replaced and quits it when removed. |
| `src/demo.rs` | Demo mode for recordings. |
| `scripts/bundle.sh`, `Makefile` | The app bundle (Info.plist, icon, ad-hoc signature), install, uninstall and release zip. |

**Adding an integration.** Declare an `Integration` (fields, required secret, test) and add it to
`integrations::ALL`; it appears in Settings, and its values come from `integration.value(key)`. Fetch its
data on a worker thread as `firehydrant::spawn` does, give it a tab or card, and add a mock so scenes and
demo mode keep working. Another kind of sign-in (OAuth, say) is a new `FieldKind` with its own control in
`settings.rs`.

## License

[MIT](LICENSE)
