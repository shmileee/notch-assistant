# Calendar

**tl;dr:** Make sure your meetings show in the Calendar app, then click **Allow access** in Settings ›
Calendar. The Calendar section then shows the meeting you're in or your next one, with a Join button, and
the rest of today and tomorrow.

## Set it up

1. Check that the Calendar app shows your meetings. The panel reads every account there that has
   Calendars turned on (System Settings › Internet Accounts): Microsoft 365 or Exchange, Google, iCloud,
   and calendars you subscribe to.
2. In the panel, click ⚙, pick Calendar and click **Allow access**. macOS asks you once.

There is no key to paste. The app reads your calendars through macOS, and nothing leaves your Mac.

> [!NOTE]
> Only the installed app can ask for access. A binary run from a terminal (`cargo run`) reads your
> calendars only if the terminal already has access under System Settings › Privacy & Security ›
> Calendars.

> [!WARNING]
> macOS ties the permission to the app's signature. Releases are all signed with the same certificate, so
> the permission carries over upgrades. A build from source (`make install`) is ad-hoc signed and new to
> macOS each time, so macOS may ask again. Once you've allowed access, the app asks by itself at launch,
> and if access is off it says so instead of showing mock meetings.

If you turned access off, Settings › Calendar has a button that opens the right page of System Settings.
Once you turn it back on there, the panel notices within a minute.

## What's shown

- **Now or next:** the meeting you're in, with the time left, or your next one today. **Join** opens the
  call when the invitation has a video-call link (Teams, Zoom, Google Meet and others; `service` in
  [`src/agenda.rs`](../src/agenda.rs) lists them).
- **The rest of today, then tomorrow.** Meetings you answered "maybe" or haven't answered are dimmed and
  say so. A meeting that is on now or starts soon gets its own Join button.
- **All-day events** at the top, and **today on a timeline** at the bottom, with a line at the current
  time.
- **Not shown:** meetings you declined and meetings that were cancelled.

A click on a meeting opens it in the Calendar app. macOS doesn't document the link for that, so when it
can't find the meeting, Calendar just opens.

## Before a meeting

- The sidebar shows how long until your next meeting, once it's less than an hour away.
- The collapsed island counts down to it beside the notch, in the calendar's colour.
- Shortly before it starts, the island drops a line under the notch with its name. Hovering it opens the
  Calendar section, where the Join button is.

`countdown` and `lead` in [`src/agenda.rs`](../src/agenda.rs) set how early these appear.

> [!NOTE]
> Only meetings that take your time count down and remind you. An event marked **Free** (a blocked-out
> slot shown as available) and a meeting on a colleague's shared calendar that you aren't invited to are
> still listed, but they never lead the section, count down or drop a reminder.

## What it reads

Today's and tomorrow's events from every calendar, through EventKit, refreshed at once when the Calendar
app reports a change and every `EVERY` in [`src/calendar.rs`](../src/calendar.rs) besides. EventKit
already splits a repeating meeting into one event per occurrence.

To see what it finds without launching the app, run `cargo run --release -- --probe-calendar`. It never
asks for access, so it works only once your terminal has it.
