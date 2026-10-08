#!/usr/bin/env bash
# Regenerates the synthetic fixtures (claude-home/ for discovery, workstreams-home/ for #13). Field names come from
# the "~/.claude format, as observed" comment on strate #10; every value is a placeholder.
set -eu
cd "$(dirname "$0")/.."
F=fixtures/claude-home
SA=5e55a0a0-0000-4000-8000-00000000000a
SB=5e55b0b0-0000-4000-8000-00000000000b
LONG=-work--worktrees-demo-repo-7-lorem-ipsum-dolor-sit-x7k2q9
rm -rf "$F"
mkdir -p "$F/sessions" "$F/teams/session-0a1b2c3d" "$F/projects/-work-demo-repo/$SA/subagents" "$F/projects/-work-demo-repo/$SA/tool-results" "$F/projects/$LONG/$SB/subagents"
printf 'not json {{{ decoy credentials\n' > "$F/.credentials.json"
printf 'garbage decoy {{{ not a live-process record\n' > "$F/sessions/123.json"
printf 'lorem ipsum tool result\n' > "$F/projects/-work-demo-repo/$SA/tool-results/toolu_01AAAAAAAAAAAAAAAAAAAAAA.txt"
echo "{\"name\":\"demo-team\",\"createdAt\":1790000000000,\"leadAgentId\":\"lead-0001\",\"leadSessionId\":\"$SA\",\"members\":[{\"name\":\"demo-reviewer\",\"agentType\":\"reviewer\"},\"opaque-member\"]}" > "$F/teams/session-0a1b2c3d/config.json"

USAGE='"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":5}'
ENV_A="\"isSidechain\":false,\"timestamp\":\"2026-10-01T10:00:00.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"/work/demo-repo\",\"sessionId\":\"$SA\",\"version\":\"2.1.294\",\"gitBranch\":\"main\""
tool_use() { # id name subagent_type
  echo "{\"type\":\"tool_use\",\"id\":\"$1\",\"name\":\"$2\",\"input\":{\"subagent_type\":\"$3\",\"description\":\"Lorem\",\"prompt\":\"Lorem ipsum.\"}}"
}
{
  echo "{\"type\":\"ai-title\",\"sessionId\":\"$SA\",\"aiTitle\":\"Lorem ipsum dolor\"}"
  echo "{\"type\":\"file-history-snapshot\",\"messageId\":\"m-0000\",\"snapshot\":{}}"
  echo "{\"parentUuid\":null,$ENV_A,\"uuid\":\"u-a001\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum dolor sit amet.\"},\"promptId\":\"p-0001\"}"
  echo "{\"parentUuid\":\"u-a001\",$ENV_A,\"uuid\":\"u-a002\",\"type\":\"assistant\",\"requestId\":\"req_01\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_01\",\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"Lorem ipsum.\"}],$USAGE}}"
  echo "{\"parentUuid\":\"u-a002\",$ENV_A,\"uuid\":\"u-a003\",\"type\":\"assistant\",\"requestId\":\"req_01\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_01\",\"model\":\"claude-opus-5-5\",\"content\":[$(tool_use toolu_01AAAAAAAAAAAAAAAAAAAAAA Agent implementer)],$USAGE}}"
  echo "{\"parentUuid\":\"u-a003\",$ENV_A,\"uuid\":\"u-a004\",\"type\":\"assistant\",\"requestId\":\"req_01\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_01\",\"model\":\"claude-opus-5-5\",\"content\":[$(tool_use toolu_01BBBBBBBBBBBBBBBBBBBBBB Agent recon)],$USAGE}}"
  # replayed record (resume/fork) repeating the first tool_use id
  echo "{\"parentUuid\":\"u-a002\",$ENV_A,\"uuid\":\"u-a005\",\"type\":\"assistant\",\"requestId\":\"req_01\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_01\",\"model\":\"claude-opus-5-5\",\"content\":[$(tool_use toolu_01AAAAAAAAAAAAAAAAAAAAAA Agent implementer)],$USAGE}}"
  echo "{\"type\":\"future-record-kind\",\"sessionId\":\"$SA\",\"payload\":{\"lorem\":true}}"
  echo "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":["
  echo "{\"parentUuid\":\"u-a004\",$ENV_A,\"uuid\":\"u-a006\",\"type\":\"system\",\"subtype\":\"turn_duration\",\"durationMs\":1200,\"messageCount\":6,\"pendingBackgroundAgentCount\":2}"
  echo "{\"type\":\"cost-state\",\"sessionId\":\"$SA\",\"totalCostUSD\":0.5,\"modelUsage\":{}}"
  printf '%s' "{\"type\":\"last-prompt\",\"sessionId\":\"$SA\",\"lastPrompt\":\"Lorem"
} > "$F/projects/-work-demo-repo/$SA.jsonl"

sub() { # dir agentId cwd sessionId version [extra-line]
  E="\"isSidechain\":true,\"agentId\":\"$2\",\"timestamp\":\"2026-10-01T10:01:00.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"$3\",\"sessionId\":\"$4\",\"version\":\"$5\",\"gitBranch\":\"main\""
  {
    echo "{\"parentUuid\":null,$E,\"uuid\":\"$2-u1\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"}}"
    echo "{\"parentUuid\":\"$2-u1\",$E,\"uuid\":\"$2-u2\",\"type\":\"assistant\",\"requestId\":\"req_$2\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_$2\",\"model\":\"claude-sonnet-5\",\"content\":[{\"type\":\"text\",\"text\":\"Lorem.\"}],$USAGE}}"
    if [ -n "${6:-}" ]; then echo "$6"; fi
  } > "$1/agent-$2.jsonl"
}
DA="$F/projects/-work-demo-repo/$SA/subagents"
NESTED="{\"parentUuid\":\"a1000000000000001-u2\",\"isSidechain\":true,\"agentId\":\"a1000000000000001\",\"cwd\":\"/work/demo-repo\",\"sessionId\":\"$SA\",\"version\":\"2.1.294\",\"uuid\":\"a1000000000000001-u3\",\"type\":\"assistant\",\"requestId\":\"req_n1\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_n1\",\"model\":\"claude-opus-5-5\",\"content\":[$(tool_use toolu_01CCCCCCCCCCCCCCCCCCCCCC Agent Explore)],$USAGE}}"
sub "$DA" a1000000000000001 /work/demo-repo "$SA" 2.1.294 "$NESTED"
for n in 2 3 4 5 6; do sub "$DA" a100000000000000$n /work/demo-repo "$SA" 2.1.294; done
# key set: full + effort
echo '{"agentType":"implementer","description":"Lorem ipsum task","toolUseId":"toolu_01AAAAAAAAAAAAAAAAAAAAAA","spawnDepth":1,"requestShape":"agent","requestNonInteractive":true,"model":"claude-opus-5-5","effort":"high"}' > "$DA/agent-a1000000000000001.meta.json"
# key set: full
echo '{"agentType":"recon","description":"Dolor sit task","toolUseId":"toolu_01BBBBBBBBBBBBBBBBBBBBBB","spawnDepth":1,"requestShape":"agent","requestNonInteractive":true,"model":"claude-sonnet-5"}' > "$DA/agent-a1000000000000002.meta.json"
# key set: no model (nested, spawnDepth 2)
echo '{"agentType":"Explore","description":"Sit amet task","requestNonInteractive":true,"requestShape":"agent","spawnDepth":2,"toolUseId":"toolu_01CCCCCCCCCCCCCCCCCCCCCC"}' > "$DA/agent-a1000000000000003.meta.json"
# a1000000000000004: orphan jsonl, no meta
# key set: minimal, toolUseId unresolvable
echo '{"agentType":"general-purpose","description":"Consectetur task","spawnDepth":1,"toolUseId":"toolu_01ZZZZZZZZZZZZZZZZZZZZZZ"}' > "$DA/agent-a1000000000000005.meta.json"
# key set: teammate
echo '{"agentType":"reviewer","color":"blue","description":"Adipiscing task","model":"claude-sonnet-5","name":"demo-reviewer","permissionMode":"default","planModeRequired":false,"requestNonInteractive":false,"requestShape":"teammate","spawnDepth":0,"taskKind":"lorem","teamName":"demo-team"}' > "$DA/agent-a1000000000000006.meta.json"

ENV_B="\"isSidechain\":false,\"timestamp\":\"2026-09-20T09:00:00.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"/work/.worktrees/demo-repo-7\",\"sessionId\":\"$SB\",\"version\":\"2.1.263\",\"gitBranch\":\"feat/7-lorem\""
{
  echo "{\"parentUuid\":null,$ENV_B,\"uuid\":\"u-b001\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"}}"
  echo "{\"parentUuid\":\"u-b001\",$ENV_B,\"uuid\":\"u-b002\",\"type\":\"assistant\",\"requestId\":\"req_b1\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_b1\",\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"text\",\"text\":\"Lorem.\"},$(tool_use toolu_02AAAAAAAAAAAAAAAAAAAAAA Task Explore)],$USAGE}}"
  # two /rename runs (each writes custom-title + agent-name); the latest name wins
  echo "{\"type\":\"custom-title\",\"customTitle\":\"demo-repo-6\",\"sessionId\":\"$SB\"}"
  echo "{\"type\":\"agent-name\",\"agentName\":\"demo-repo-6\",\"sessionId\":\"$SB\"}"
  echo "{\"type\":\"pr-link\",\"sessionId\":\"$SB\",\"prNumber\":7,\"prUrl\":\"https://github.com/example/demo-repo/pull/7\",\"prRepository\":\"example/demo-repo\",\"timestamp\":\"2026-09-20T09:05:00.000Z\"}"
  echo "{\"type\":\"custom-title\",\"customTitle\":\"demo-repo-7\",\"sessionId\":\"$SB\"}"
  echo "{\"type\":\"agent-name\",\"agentName\":\"demo-repo-7\",\"sessionId\":\"$SB\"}"
  echo "{\"parentUuid\":\"u-b002\",$ENV_B,\"uuid\":\"u-b003\",\"type\":\"assistant\",\"requestId\":\"req_b2\",\"effort\":\"medium\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_b2\",\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"text\",\"text\":\"Dolor.\"}],$USAGE}}"
} > "$F/projects/$LONG/$SB.jsonl"
DB="$F/projects/$LONG/$SB/subagents"
sub "$DB" a2000000000000001 /work/.worktrees/demo-repo-7 "$SB" 2.1.263
echo '{"agentType":"Explore","description":"Lorem task","toolUseId":"toolu_02AAAAAAAAAAAAAAAAAAAAAA","spawnDepth":1,"requestShape":"agent","requestNonInteractive":true,"model":"claude-haiku-4-5"}' > "$DB/agent-a2000000000000001.meta.json"

# ---- workstreams-home: grouping by name segments and continuation (#13) ----
W=fixtures/workstreams-home
rm -rf "$W"
# One process root (C1) whose /clear files start C1, C9, C5 in that time order.
C1=5e55c0c0-0000-4000-8000-0000000000c1
C9=5e55c0c0-0000-4000-8000-0000000000c9
C5=5e55c0c0-0000-4000-8000-0000000000c5
D1=5e55d0d0-0000-4000-8000-0000000000d1
D2=5e55d0d0-0000-4000-8000-0000000000d2
E1=5e55e0e0-0000-4000-8000-0000000000e1
E2=5e55e0e0-0000-4000-8000-0000000000e2
P="$W/projects/-work-demo-repo"
mkdir -p "$P/$C9/subagents" "$W/projects/-work-other-repo" "$W/projects/-work-demo-repo-packages-core"
n=0
at() { # sessionId cwd gitBranch root-session_id|- : the envelope of the records that follow
  WS=$1; WCWD=$2; WBR=$3; WROOT=$4
}
env_w() { # HH:MM:SS
  n=$((n + 1))
  E="\"isSidechain\":false,\"timestamp\":\"2026-10-02T$1.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"$WCWD\",\"sessionId\":\"$WS\",\"version\":\"2.1.294\",\"gitBranch\":\"$WBR\",\"uuid\":\"w-$n\""
  if [ "$WROOT" != - ]; then E="$E,\"session_id\":\"$WROOT\""; fi
}
user() { env_w "$1"; echo "{\"parentUuid\":null,$E,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"}}"; }
reply() { # HH:MM:SS [Agent tool_use id]
  env_w "$1"
  BODY='{"type":"text","text":"Lorem."}'
  if [ -n "${2:-}" ]; then BODY=$(tool_use "$2" Agent implementer); fi
  echo "{\"parentUuid\":null,$E,\"type\":\"assistant\",\"requestId\":\"req_w$n\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_w$n\",\"model\":\"claude-opus-5-5\",\"content\":[$BODY],$USAGE}}"
}
slash() { env_w "$1"; echo "{\"parentUuid\":null,$E,\"type\":\"system\",\"subtype\":\"local_command\",\"content\":\"<command-name>/$2</command-name>\",\"level\":\"info\"}"; }
title() { # what /rename writes, and what /clear carries into the next file (no timestamp)
  echo "{\"type\":\"custom-title\",\"customTitle\":\"$1\",\"sessionId\":\"$WS\"}"
  echo "{\"type\":\"agent-name\",\"agentName\":\"$1\",\"sessionId\":\"$WS\"}"
}
side() { # agentId HH:MM:SS toolUseId: a subagent of $WS started at that time
  SE="\"isSidechain\":true,\"agentId\":\"$1\",\"timestamp\":\"2026-10-02T$2.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"$WCWD\",\"sessionId\":\"$WS\",\"version\":\"2.1.294\",\"gitBranch\":\"$WBR\""
  {
    n=$((n + 1)); echo "{\"parentUuid\":null,$SE,\"uuid\":\"w-$n\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"}}"
    n=$((n + 1)); echo "{\"parentUuid\":null,$SE,\"uuid\":\"w-$n\",\"type\":\"assistant\",\"requestId\":\"req_w$n\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_w$n\",\"model\":\"claude-sonnet-5\",\"content\":[{\"type\":\"text\",\"text\":\"Lorem.\"}],$USAGE}}"
  } > "$P/$WS/subagents/agent-$1.jsonl"
  echo "{\"agentType\":\"implementer\",\"description\":\"Lorem task\",\"toolUseId\":\"$3\",\"spawnDepth\":1,\"requestShape\":\"agent\",\"requestNonInteractive\":true}" > "$P/$WS/subagents/agent-$1.meta.json"
}

# Late rename: named demo-repo-81 most of the way in; the name backfills the file.
at $C1 /work/demo-repo main $C1
{ user 10:00:00; reply 10:00:05; user 10:01:00; reply 10:01:05; user 10:02:00; reply 10:02:05
  slash 10:03:00 rename; title demo-repo-81; user 10:04:00; reply 10:04:05; } > "$P/$C1.jsonl"
# /clear successor carrying demo-repo-81 (the title repeats), renamed mid-file to
# demo-repo-82; one subagent dispatched in each segment.
at $C9 /work/demo-repo main $C1
{ title demo-repo-81; slash 11:00:00 clear; user 11:00:10; reply 11:00:15 toolu_03AAAAAAAAAAAAAAAAAAAAAA
  title demo-repo-81; user 11:01:00; reply 11:01:05; slash 11:02:00 rename; title demo-repo-82
  user 11:02:10; reply 11:02:15 toolu_03BBBBBBBBBBBBBBBBBBBBBB; user 11:03:00; reply 11:03:05; } > "$P/$C9.jsonl"
side a3000000000000001 11:00:16 toolu_03AAAAAAAAAAAAAAAAAAAAAA
side a3000000000000002 11:02:16 toolu_03BBBBBBBBBBBBBBBBBBBBBB
# /clear successor: a carried-name prelude (demo-repo-82, no reply), then
# /rename demo-repo-83, whose title repeats.
at $C5 /work/demo-repo main $C1
{ title demo-repo-82; slash 12:00:00 clear; slash 12:00:05 rename; title demo-repo-83
  user 12:00:10; reply 12:00:15; title demo-repo-83; user 12:01:00; reply 12:01:05; title demo-repo-83; } > "$P/$C5.jsonl"
# A second name spanning files: late-renamed demo-repo-90, then a carried-name
# successor renamed to demo-repo-91 just now (no reply yet).
at $D1 /work/other-repo main $D1
{ user 09:00:00; reply 09:00:05; user 09:01:00; reply 09:01:05; title demo-repo-90; reply 09:02:05; } > "$W/projects/-work-other-repo/$D1.jsonl"
at $D2 /work/other-repo main $D1
{ title demo-repo-90; slash 09:30:00 clear; user 09:30:10; reply 09:30:15; slash 09:31:00 rename; title demo-repo-91; } > "$W/projects/-work-other-repo/$D2.jsonl"
# Unnamed, no session_id: the fallback key is the dominant cwd and branch,
# which differ from the first record's.
at $E1 /work/demo-repo main -
{ echo "{\"type\":\"ai-title\",\"sessionId\":\"$E1\",\"aiTitle\":\"Lorem ipsum\"}"; user 08:00:00
  at $E1 /work/demo-repo/packages/core feat/9-lorem -; reply 08:00:05; user 08:01:00; reply 08:01:05; } > "$W/projects/-work-demo-repo-packages-core/$E1.jsonl"
at $E2 /work/demo-repo/packages/core feat/9-lorem -
{ user 08:30:00; reply 08:30:05; } > "$W/projects/-work-demo-repo-packages-core/$E2.jsonl"
