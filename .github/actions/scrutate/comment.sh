#!/usr/bin/env bash
# Posts the scrutate review of a pull request as one comment, or edits the comment an earlier run
# posted. Every value comes from the environment and is used quoted; any failure is a warning and
# the step still exits 0, so a comment that cannot be posted never fails the job.
set -uo pipefail

marker='<!-- scrutate-review -->'
skip() {
  echo "::warning::scrutate review comment: $*"
  exit 0
}

[ -n "${PR_NUMBER:-}" ] || skip "not a pull request event, nothing posted"
[[ "$PR_NUMBER" =~ ^[0-9]+$ ]] || skip "the pull request number is not a number"
[[ "${BASE_SHA:-}" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || skip "the base commit is not a commit id"
[[ "${REPO:-}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || skip "the repository name is not owner/name"
git cat-file -e "$BASE_SHA^{commit}" 2> /dev/null || skip "the base commit is not in the checkout: check out with fetch-depth: 0"
head=$(git rev-parse HEAD) || skip "no commit is checked out"

body="${RUNNER_TEMP:-.}/scrutate-review.md"
code=0
scrutate review --base "$BASE_SHA" --link "${SERVER_URL:-https://github.com}/$REPO/blob/$head" > "$body" || code=$?
# 1 is a report with nothing in it, still posted so an earlier comment is brought up to date
[ "$code" -le 1 ] || skip "scrutate review failed with exit code $code"

comments=$(gh api --paginate "repos/$REPO/issues/$PR_NUMBER/comments?per_page=100") || skip "cannot list the pull request's comments"
# the earlier comment: the bot's (GITHUB_TOKEN), or the token owner's when github-token is a person's
# shellcheck disable=SC2016 # $m and $me are jq's variables
ours='.[] | select((.body | startswith($m)) and (.user.type == "Bot" or ($me != "" and .user.login == $me))) | .id'
id=$(printf '%s' "$comments" | jq -r --arg m "$marker" --arg me "" "$ours" | awk 'NR == 1') ||
  skip "cannot read the pull request's comments"
if [ -z "$id" ] && [ -n "$(printf '%s' "$comments" | jq -r --arg m "$marker" '.[] | select(.user.type != "Bot" and (.body | startswith($m))) | .id')" ]; then
  # GITHUB_TOKEN cannot read the user endpoint (403): then only the bot's comment counts
  me=$(gh api user -q .login 2> /dev/null) || me=""
  [[ "$me" =~ ^[A-Za-z0-9-]+$ ]] || me=""
  if [ -n "$me" ]; then
    id=$(printf '%s' "$comments" | jq -r --arg m "$marker" --arg me "$me" "$ours" | awk 'NR == 1') ||
      skip "cannot read the pull request's comments"
  fi
fi
[[ "$id" =~ ^[0-9]*$ ]] || skip "a comment id is not a number"

if [ -n "$id" ]; then
  gh api -X PATCH "repos/$REPO/issues/comments/$id" -F "body=@$body" > /dev/null || skip "cannot edit comment $id"
  echo "scrutate review: edited comment $id"
else
  gh api -X POST "repos/$REPO/issues/$PR_NUMBER/comments" -F "body=@$body" > /dev/null || skip "cannot post the comment"
  echo "scrutate review: posted a comment"
fi
