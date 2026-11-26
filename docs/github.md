# GitHub

**tl;dr:** In Settings › GitHub, paste a classic personal access token with the `repo` scope that is
authorized for your organization's SSO. The GitHub section then lists your open pull requests and the ones
you're asked to review, and puts first the ones where something happened since you last read them.

## Set it up

Settings has two fields. An environment variable overrides each one.

| Field | Stored in | Override |
| --- | --- | --- |
| Token | login Keychain, service `notch-assistant` | `NOTCH_GITHUB_TOKEN` |
| Ignore activity from | `~/Library/Application Support/notch-assistant/settings.json` | `NOTCH_GITHUB_IGNORE` |

Create the token in GitHub under Settings › Developer settings › Personal access tokens (classic), then use
Configure SSO to authorize it for your organization. If `gh` is logged in to that account,
`gh auth token` prints a token that already works.

> [!NOTE]
> It has to be a classic token. A fine-grained token can't read GitHub's notifications, and without them
> reading a pull request on github.com doesn't clear it in the panel; only opening it from the panel does.

## What's listed

A switch at the top picks one of two lists:

- **Your pull requests:** open ones, and closed ones while something on them is unread. Comments, reviews,
  merges and closes, and reactions to your pull request or to your comments, by anyone but you, are what
  can be unread. "Ready to merge" marks the ones GitHub would merge now: approved, every required check
  and rule met, and no conflicts. They come first after the unread ones. "Conflicts" marks the ones that
  need their base branch merged in first.
- **To review:** open pull requests, not drafts, that ask for a review from you or a team you're in. The
  request is what can be unread. Requests to you come first.

> [!NOTE]
> A request that asks only a team of yours is listed, and a new one gets a peek under the notch, but it
> doesn't add to the counts on the sidebar or the closed notch. Teams get many requests, and most are a
> teammate's to answer. Its count in the list is grey instead of purple.

Bots never count, and neither do accounts whose login ends in `-bot`, `_bot` or `[bot]`, so pull
requests they open aren't listed to review. A machine user that looks like a person (a scanner that
comments, say) goes in "Ignore activity from", as a comma-separated list of logins.

> [!NOTE]
> At most 100 of your open pull requests are listed: the 100 updated most recently. GitHub gives no
> more in one search, and the panel says so when you have more.

## What's unread

Anything that happened after you last read the pull request, either on github.com or from the panel.
GitHub's notification for the pull request records when you read it there. The panel keeps both times
in `~/Library/Application Support/notch-assistant/github-read.json`, so a read survives the notification
getting old.

A pull request with neither time counts from when the app first connected, so the first run doesn't flag
old comments. "Mark all read" clears the panel only, not your GitHub inbox.

> [!NOTE]
> GitHub sends no notification for a reaction, so it can't tell the panel you've seen one. A reaction can
> stay unread after you look at the pull request on github.com. Open the pull request from the panel to
> clear it.

## How fresh it is

Every minute the panel asks GitHub whether your notifications changed. Asking costs nothing while they
haven't: GitHub answers "not modified" and doesn't count it. When a notification shows a new comment,
review, merge or request on a pull request the panel lists, it refreshes that list right away.

Some changes never send a notification, so a cheap check on a timer catches those. The constants in
[`src/github.rs`](../src/github.rs) set each delay, and their comments say what it costs.

| What changed | Shows within |
| --- | --- |
| A comment, review, merge or close; a new request to review; a read on github.com | about a minute, and at most `GAP` after the last one |
| A reaction to your pull request or to one of your latest comments; checks finishing; "Ready to merge" | `STATE_EVERY` |
| A teammate answering your team's request; a request made again on a pull request you reviewed | `REVIEWS_EVERY` |
| A read on github.com of an older notification | `THREADS_EVERY` |
| A reaction to one of your inline review comments | `OWN_EVERY` |

> [!NOTE]
> GitHub counts its API budget per person, not per token: the panel shares yours with `gh` and anything
> else that calls GitHub as you. When GitHub says the budget is spent, the panel waits as long as GitHub
> asks before trying again.

<details>
<summary>The calls</summary>

All of them are read-only.

- `GET /notifications?all=true&participating=true`, every `EVERY`, with the ETag of the last answer, so
  an unchanged answer is free. A changed notification says which list to refresh, and its
  `last_read_at` says when you last read that pull request on github.com. Reading a notification
  doesn't move it up the list, so every `THREADS_EVERY` the panel reads the ones that have dropped off
  the first page one by one (`GET /notifications/threads/{id}`, also with ETags). At start it pages
  back to find them.
- `POST /graphql` with `STATE`, every `STATE_EVERY`: whether GitHub would merge each of your open pull
  requests now, their checks, and how many reactions they and your latest comments have. A change in
  the reactions, or in which pull requests are open, calls for `QUERY`.
- `POST /graphql` with `QUERY`: your open pull requests and the recently closed ones, each with its
  latest comments, reviews, merges and closes and the reactions to them. It runs when a notification or
  `STATE` calls for it, and at least every `OWN_EVERY`.
- `POST /graphql` with `REVIEWS`: the pull requests waiting for your review or a team's, 100 a page.
  Renovate and Dependabot ask for the most reviews, so the search leaves them out to fit one page.
  It runs when a new request or a change to a listed one comes, and at least every `REVIEWS_EVERY`.

Without readable notifications (a fine-grained token, or while they fail to load), your pull requests
and the requests to review refresh every `BLIND_EVERY` instead.

</details>

To see what it finds without launching the app, run `cargo run --release -- --probe-github`. Unread
events are marked `*`.

## Notes in the panel

When something doesn't work, the panel keeps showing what it last got, says **Partly live** at the top,
and adds a note under the list saying what's wrong:

| Note | What it means |
| --- | --- |
| To review couldn't refresh, Checks and merge state couldn't refresh | That call to GitHub failed. The panel tries again after a minute, then waits twice as long each time, up to that part's usual delay. |
| Notifications couldn't refresh | GitHub's notifications didn't answer (the network, or a rate limit). Until they do, your pull requests and the requests to review refresh every `BLIND_EVERY`. |
| GitHub's notifications aren't readable with this token | GitHub refused them, usually because the token is fine-grained. Use a classic token. |
| Some organizations hide their pull requests | Authorize the token for that organization's SSO. |
| Showing the … most recently updated … | A list is longer than the panel reads: your open pull requests (see [What's listed](#whats-listed)) or the requests to review. The rest aren't shown. Nothing is wrong, so the panel still says Live. |
