# Linear

**tl;dr:** In Settings › Linear, paste a personal API key with Read access. The Linear section then lists
the issues and projects Linear notified you about that you haven't read, then your open issues.

## Set it up

Create the key in Linear under Settings › Account › Security & access › Personal API keys. Read access is
enough: the panel never changes anything in Linear. It's kept in the login Keychain (service
`notch-assistant`), and `NOTCH_LINEAR_API_KEY` overrides it.

## What's listed

- **Unread first:** issues and projects with a notification you haven't read, newest first. A click opens
  the issue where the notification points (a comment, say) and marks it read in the panel.
- **Then your open issues:** assigned to you and started (In Progress, In Review) or not yet started
  (Todo), started ones first.

Linear's notification settings decide what you're notified about, and so what shows here, with two
exceptions:

- Notifications from bots never count: a status set by the GitHub integration, a sync error. Reminders
  with no one behind them, such as an SLA breach, do.
- Document notifications aren't shown, because Linear gives them no title or link.

## What's unread

A notification is unread while Linear still has it unread (you haven't opened it in Linear) and it came
after you last opened its issue from the panel. The panel keeps those times in
`~/Library/Application Support/notch-assistant/linear-read.json`. "Mark all read" clears the panel only,
not your Linear inbox.

> [!NOTE]
> Linear keeps unread notifications for months, so a first run counts only recent ones. How far back it
> looks is `look_back` in [`src/issues.rs`](../src/issues.rs). Older ones stay in Linear's inbox but not in
> the panel.

## What it calls

One read-only GraphQL query to `https://api.linear.app/graphql` (`QUERY` in
[`src/linear.rs`](../src/linear.rs)): your open issues (`viewer.assignedIssues`) and your latest
notifications. `EVERY` in the same file sets how often it refreshes.

To see what it finds without launching the app, run `cargo run --release -- --probe-linear`. Unread
notifications are marked `*`.
