# GitHub

**tl;dr:** In Settings › GitHub, paste a classic personal access token with the `repo` scope that is
authorized for your organization's SSO. The GitHub section then lists your open pull requests, and puts
first the ones where someone did something since you last read them.

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

## What counts

Comments, reviews, merges and closes on your pull requests, and reactions to them or to your comments,
by anyone but you.

Bots never count, and neither do accounts whose login ends in `-bot`, `_bot` or `[bot]`. A machine user
that looks like a person (a scanner that comments, say) goes in "Ignore activity from", as a
comma-separated list of logins.

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

## What it calls

Everything is read-only. `EVERY` in [`src/github.rs`](../src/github.rs) sets how often it refreshes, and
its comment says how much of the token's GraphQL budget that uses. Anything else using the same token,
such as `gh`, shares that budget.

<details>
<summary>The calls</summary>

- `POST /graphql`: your open pull requests and the recently closed ones, each with its latest comments,
  reviews, merges and closes and the reactions to them (`QUERY` in [`src/github.rs`](../src/github.rs)).
- `GET /notifications?all=true&participating=true`: your latest notifications, for when you last read
  each pull request. At start it pages back to find those for pull requests with activity. After that it
  reads one page, and reads a notification that has dropped off it with `GET /notifications/threads/{id}`.

</details>

To see what it finds without launching the app, run `cargo run --release -- --probe-github`. Unread
events are marked `*`.
