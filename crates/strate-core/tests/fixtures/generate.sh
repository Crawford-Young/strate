#!/usr/bin/env bash
# Regenerates the synthetic fixtures (claude-home/ for discovery, workstreams-home/ for #13, cost-home/ for #14, audit-home/ and github/audit-home.json for #26, nest-home/ and github/nest-home.json for #47, perf-home/ and github/perf-home.json for #51, directs-home/ for #53, chain-home/ for #63, teams-home/ for #83). Field names come from
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

# ---- cost-home: cost and time (#14), checked against audit.mjs ----
# After a change here, regenerate the golden from claude-config's audit.mjs (see audit-golden.mjs).
K=fixtures/cost-home
K1=5e55f0f0-0000-4000-8000-0000000000f1
K2=5e55f0f0-0000-4000-8000-0000000000f2
rm -rf "$K"
mkdir -p "$K/projects/-work-cost-repo/$K1/subagents" "$K/projects/-work-cost-other"
TEXT='{"type":"text","text":"Lorem."}'
THINK='{"type":"thinking","thinking":"Lorem ipsum."}'
kat() { # sessionId cwd sidechain-fields: the envelope of the records that follow
  KS=$1; KC=$2; KSIDE=$3
}
kenv() { # HH:MM:SS uuid
  KE="\"parentUuid\":null,$KSIDE,\"timestamp\":\"2026-10-03T$1.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"$KC\",\"sessionId\":\"$KS\",\"version\":\"2.1.294\",\"gitBranch\":\"main\",\"uuid\":\"$2\""
}
kprompt() { # HH:MM:SS uuid: a typed prompt
  kenv "$1" "$2"; echo "{$KE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum dolor.\"},\"origin\":{\"kind\":\"human\"}}"
}
kresult() { # HH:MM:SS uuid toolUseId
  kenv "$1" "$2"; echo "{$KE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"$3\",\"content\":\"Lorem.\"}]}}"
}
kreply() { # HH:MM:SS uuid requestId model usage content-block
  kenv "$1" "$2"; echo "{$KE,\"type\":\"assistant\",\"requestId\":\"$3\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_$3\",\"model\":\"$4\",\"content\":[$6],\"usage\":$5}}"
}
ku() { # input cacheCreation cacheRead output [5m 1h] [webSearchRequests]: message.usage
  U="{\"input_tokens\":$1,\"cache_creation_input_tokens\":$2,\"cache_read_input_tokens\":$3,\"output_tokens\":$4"
  if [ -n "${5:-}" ]; then U="$U,\"cache_creation\":{\"ephemeral_5m_input_tokens\":$5,\"ephemeral_1h_input_tokens\":$6}"; fi
  if [ -n "${7:-}" ]; then U="$U,\"server_tool_use\":{\"web_search_requests\":$7}"; fi
  echo "$U,\"service_tier\":\"standard\"}"
}
ktool() { # id name input
  echo "{\"type\":\"tool_use\",\"id\":\"$1\",\"name\":\"$2\",\"input\":$3}"
}
# K1, claude-opus-5-5, 10:00:00..10:30:06: an AskUserQuestion wait, a subagent,
# a 22.6-min idle gap, then a <synthetic> record and a replayed uuid.
kat $K1 /work/cost-repo '"isSidechain":false'
{ kprompt 10:00:00 k-01
  echo "{\"type\":\"custom-title\",\"customTitle\":\"cost-repo-14\",\"sessionId\":\"$K1\"}"
  # streaming partials of one request: output grows 10 -> 40; only the last counts
  kreply 10:00:05 k-02 req_c1 claude-opus-5-5 "$(ku 100 500 10000 10 500 0)" "$THINK"
  kreply 10:00:08 k-03 req_c1 claude-opus-5-5 "$(ku 100 500 10000 40 500 0)" "$(ktool toolu_c1 AskUserQuestion '{"questions":[{"question":"Lorem?"}]}')"
  kresult 10:02:08 k-04 toolu_c1
  # 5m and 1h cache tiers, two web searches, an Agent dispatch
  kreply 10:02:10 k-05 req_c2 claude-opus-5-5 "$(ku 50 300 20000 60 100 200 2)" "$(ktool toolu_c2 Agent '{"subagent_type":"implementer","description":"Lorem","prompt":"Lorem ipsum."}')"
  kresult 10:05:15 k-06 toolu_c2
  # a cache write with no tier split: all 5m
  REPLAYED=$(kreply 10:05:20 k-07 req_c3 claude-opus-5-5 "$(ku 20 400 30000 30)" "$TEXT"); echo "$REPLAYED"
  kprompt 10:07:20 k-08
  kreply 10:07:25 k-09 req_c5 claude-opus-5-5 "$(ku 10 0 40000 25 0 0)" "$TEXT"
  kprompt 10:30:00 k-10
  kreply 10:30:04 k-11 req_c4 claude-opus-5-5 "$(ku 6 50000 150000 80 0 50000)" "$TEXT"
  kenv 10:30:06 k-12
  echo "{$KE,\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_syn1\",\"model\":\"<synthetic>\",\"content\":[$TEXT],\"usage\":$(ku 0 0 0 0)}}"
  echo "$REPLAYED"
} > "$K/projects/-work-cost-repo/$K1.jsonl"
kat $K1 /work/cost-repo '"isSidechain":true,"agentId":"a4000000000000001"'
{ kenv 10:02:12 ks-1; echo "{$KE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"}}"
  kreply 10:03:12 ks-2 req_s1 claude-sonnet-5-5 "$(ku 30 200 5000 20 200 0)" "$(ktool toolu_s1 Bash '{"command":"cargo test"}')"
  kresult 10:04:12 ks-3 toolu_s1
  kreply 10:05:12 ks-4 req_s2 claude-sonnet-5-5 "$(ku 10 0 8000 15 0 0)" "$TEXT"
} > "$K/projects/-work-cost-repo/$K1/subagents/agent-a4000000000000001.jsonl"
echo '{"agentType":"implementer","description":"Lorem task","toolUseId":"toolu_c2","spawnDepth":1,"requestShape":"agent","requestNonInteractive":true,"model":"claude-sonnet-5-5"}' > "$K/projects/-work-cost-repo/$K1/subagents/agent-a4000000000000001.meta.json"
# K2, 09:00:00..09:01:02: an unpriced model, then a dated haiku snapshot.
kat $K2 /work/cost-other '"isSidechain":false'
{ kprompt 09:00:00 k-21
  kreply 09:00:03 k-22 req_d1 claude-lorem-1 "$(ku 40 0 1000 12)" "$TEXT"
  kprompt 09:01:00 k-23
  kreply 09:01:02 k-24 req_d2 claude-haiku-4-5-20251001 "$(ku 8 2000 48000 22 2000 0)" "$TEXT"
} > "$K/projects/-work-cost-other/$K2.jsonl"

# ---- audit-home: audit rollups (#26); hand values in tests/audit_rollup.rs ----
# Repo demo (gh JSON: github/audit-home.json): epic #8 over #9 (closed) and #10;
# #11 (closed not planned, no window) has its parent in another repo. The epic
# #8 has a window of its own too (demo-8): its direct work.
# Times are UTC; America/Toronto is UTC-4 here, so local midnight is 04:00Z.
A=fixtures/audit-home
A1=5e559a9a-0000-4000-8000-0000000000a1
A2=5e559a9a-0000-4000-8000-0000000000a2
A3=5e559a9a-0000-4000-8000-0000000000a3
A4=5e559a9a-0000-4000-8000-0000000000a4
A5=5e559a9a-0000-4000-8000-0000000000a5
A6=5e559a9a-0000-4000-8000-0000000000a6
AD="$A/projects/-work-demo"
rm -rf "$A"
mkdir -p "$AD/$A1/subagents" "$AD/$A2/subagents" "$A/projects/-work-other" "$A/projects/-work-scratch"
aat() { # sessionId cwd sidechain-fields: the envelope of the records that follow
  AS=$1; AC=$2; ASIDE=$3
}
aenv() { # YYYY-MM-DDTHH:MM:SS uuid
  AE="\"parentUuid\":null,$ASIDE,\"timestamp\":\"$1.000Z\",\"userType\":\"external\",\"entrypoint\":\"cli\",\"cwd\":\"$AC\",\"sessionId\":\"$AS\",\"version\":\"2.1.294\",\"gitBranch\":\"main\",\"uuid\":\"$2\""
}
atitle() { echo "{\"type\":\"custom-title\",\"customTitle\":\"$1\",\"sessionId\":\"$AS\"}"; }
aprompt() { # time uuid: a typed prompt
  aenv "$1" "$2"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum dolor.\"},\"origin\":{\"kind\":\"human\"}}"
}
atask() { # time uuid: a subagent's task prompt
  aenv "$1" "$2"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"}}"
}
aresult() { # time uuid toolUseId
  aenv "$1" "$2"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"$3\",\"content\":\"Lorem.\"}]}}"
}
areply() { # time uuid requestId model usage content-block
  aenv "$1" "$2"; echo "{$AE,\"type\":\"assistant\",\"requestId\":\"$3\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_$3\",\"model\":\"$4\",\"content\":[$6],\"usage\":$5}}"
}
ameta() { # agentType toolUseId
  echo "{\"agentType\":\"$1\",\"description\":\"Lorem task\",\"toolUseId\":\"$2\",\"spawnDepth\":1,\"requestShape\":\"agent\",\"requestNonInteractive\":true,\"model\":\"claude-sonnet-5-5\"}"
}
alaunch() { # time uuid toolUseId agentId: a background dispatch's immediate launch result
  aenv "$1" "$2"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"$3\",\"content\":[{\"type\":\"text\",\"text\":\"Async agent launched successfully.\\nagentId: $4 (lorem ipsum)\"}]}]},\"toolUseResult\":{\"isAsync\":true,\"status\":\"async_launched\",\"agentId\":\"$4\",\"description\":\"Lorem\",\"prompt\":\"Lorem ipsum.\",\"outputFile\":\"/work/tmp/$4.output\",\"canReadOutputFile\":true,\"resolvedModel\":\"$SONNET\"}}"
}
OPUS=claude-opus-5-5
SONNET=claude-sonnet-5-5
# Every opus request is $0.008 (a1 $0.018 with its cache write), every sonnet one $0.0035.
UO=$(ku 1000 0 10000 100)
US=$(ku 500 0 5000 200)
# A1, window demo-9, 2026-10-04 14:00:00..14:05:20Z: a foreground subagent.
aat $A1 /work/demo '"isSidechain":false'
{ atitle demo-9
  aprompt 2026-10-04T14:00:00 a1-01
  areply 2026-10-04T14:00:10 a1-02 req_a1 $OPUS "$(ku 1000 2000 10000 100)" "$(ktool toolu_a1 Agent '{"subagent_type":"implementer","description":"Lorem","prompt":"Lorem ipsum."}')"
  aresult 2026-10-04T14:02:00 a1-03 toolu_a1
  areply 2026-10-04T14:02:10 a1-04 req_a2 $OPUS "$UO" "$TEXT"
  aprompt 2026-10-04T14:05:10 a1-05
  areply 2026-10-04T14:05:20 a1-06 req_a3 $OPUS "$UO" "$TEXT"
} > "$AD/$A1.jsonl"
aat $A1 /work/demo '"isSidechain":true,"agentId":"a9000000000000001"'
{ atask 2026-10-04T14:00:20 a1s-1
  areply 2026-10-04T14:01:20 a1s-2 req_f1 $SONNET "$US" "$TEXT"
} > "$AD/$A1/subagents/agent-a9000000000000001.jsonl"
ameta implementer toolu_a1 > "$AD/$A1/subagents/agent-a9000000000000001.meta.json"
# A2, window demo-9-s2, 2026-10-05 03:50:00..04:05:10Z (across local midnight):
# a background subagent (run_in_background the string "true", as real records
# carry it; its tool_result the async launch), a link to PR #12, then a
# 12m40s idle gap.
aat $A2 /work/demo '"isSidechain":false'
{ atitle demo-9-s2
  aprompt 2026-10-05T03:50:00 a2-01
  areply 2026-10-05T03:50:10 a2-02 req_b1 $OPUS "$UO" "$(ktool toolu_b1 Agent '{"subagent_type":"recon","description":"Lorem","prompt":"Lorem ipsum.","run_in_background":"true"}')"
  alaunch 2026-10-05T03:50:15 a2-03 toolu_b1 a9000000000000002
  areply 2026-10-05T03:50:25 a2-04 req_b2 $OPUS "$UO" "$TEXT"
  echo "{\"type\":\"pr-link\",\"sessionId\":\"$A2\",\"prNumber\":12,\"prUrl\":\"https://github.com/example/demo/pull/12\",\"prRepository\":\"example/demo\",\"timestamp\":\"2026-10-05T03:50:30.000Z\"}"
  aprompt 2026-10-05T04:05:00 a2-05
  areply 2026-10-05T04:05:10 a2-06 req_b3 $OPUS "$UO" "$TEXT"
} > "$AD/$A2.jsonl"
aat $A2 /work/demo '"isSidechain":true,"agentId":"a9000000000000002"'
{ atask 2026-10-05T03:50:20 a2s-1
  areply 2026-10-05T03:52:20 a2s-2 req_g1 $SONNET "$US" "$TEXT"
} > "$AD/$A2/subagents/agent-a9000000000000002.jsonl"
ameta recon toolu_b1 > "$AD/$A2/subagents/agent-a9000000000000002.meta.json"
# A3, window demo-10 (open, no PR), 2026-10-05.
aat $A3 /work/demo '"isSidechain":false'
{ atitle demo-10; aprompt 2026-10-05T15:00:00 a3-01; areply 2026-10-05T15:00:30 a3-02 req_c1 $OPUS "$UO" "$TEXT"; } > "$AD/$A3.jsonl"
# A4, window other-3: a repo gh cannot fetch, so Repo > Issue with no GitHub data.
aat $A4 /work/other '"isSidechain":false'
{ atitle other-3; aprompt 2026-10-05T16:00:00 a4-01; areply 2026-10-05T16:00:20 a4-02 req_d1 $OPUS "$UO" "$TEXT"; } > "$A/projects/-work-other/$A4.jsonl"
# A5, never named: Unlinked.
aat $A5 /work/scratch '"isSidechain":false'
{ aprompt 2026-10-04T18:00:00 a5-01; areply 2026-10-04T18:00:40 a5-02 req_e1 $SONNET "$US" "$TEXT"; } > "$A/projects/-work-scratch/$A5.jsonl"
# A6, window demo-8 (the epic itself), 2026-10-05 10:00:00..10:00:20Z: direct work.
aat $A6 /work/demo '"isSidechain":false'
{ atitle demo-8; aprompt 2026-10-05T10:00:00 a6-01; areply 2026-10-05T10:00:20 a6-02 req_h1 $OPUS "$UO" "$TEXT"; } > "$AD/$A6.jsonl"
cat > fixtures/github/audit-home.json <<'JSON'
{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"CURSOR-A"},"nodes":[
{"number":8,"title":"Epic: lorem audit","state":"OPEN","stateReason":null,"createdAt":"2026-10-01T09:00:00Z","closedAt":null,"parent":null,
 "subIssuesSummary":{"total":2,"completed":1},"closedByPullRequestsReferences":{"nodes":[]}},
{"number":9,"title":"Ipsum rollup","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-10-02T09:00:00Z","closedAt":"2026-10-05T12:00:00Z","parent":{"number":8,"repository":{"nameWithOwner":"example/demo"}},
 "subIssuesSummary":{"total":0,"completed":0},
 "closedByPullRequestsReferences":{"nodes":[
   {"number":12,"state":"MERGED","createdAt":"2026-10-05T04:30:00Z","mergedAt":"2026-10-05T11:00:00Z","url":"https://github.com/example/demo/pull/12"}]}},
{"number":10,"title":"Dolor view","state":"OPEN","stateReason":null,"createdAt":"2026-10-02T10:00:00Z","closedAt":null,"parent":{"number":8,"repository":{"nameWithOwner":"example/demo"}},
 "subIssuesSummary":{"total":0,"completed":0},"closedByPullRequestsReferences":{"nodes":[]}},
{"number":11,"title":"Sit tracking","state":"CLOSED","stateReason":"NOT_PLANNED","createdAt":"2026-10-02T11:00:00Z","closedAt":"2026-10-03T11:00:00Z","parent":{"number":8,"repository":{"nameWithOwner":"example/elsewhere"}},
 "subIssuesSummary":{"total":0,"completed":0},"closedByPullRequestsReferences":{"nodes":[]}}
]}}}}
JSON

# ---- nest-home: a nested epic (#47); hand values in tests/audit_rollup.rs ----
# Repo nest (owner from github/user.json): epic #1 over the nested epic #2
# and #4; #2 is over #3. #5 has only a sub-issue in another repo. One
# window each for #2 (its direct work), #3, #4 and #5, on Oct 4 and Oct 5 in
# Toronto. Every orchestrator request is opus, $0.008. nest-5 also
# dispatches a background subagent with no run_in_background flag, known
# only by its async launch result (#55).
N=fixtures/nest-home
ND="$N/projects/-work-nest"
rm -rf "$N"
mkdir -p "$ND"
nwindow() { # sessionId name prompt-time reply-time requestId
  aat "$1" /work/nest '"isSidechain":false'
  { atitle "$2"; aprompt "$3" "$2-01"; areply "$4" "$2-02" "$5" $OPUS "$UO" "$TEXT"; } > "$ND/$1.jsonl"
}
nwindow 5e55a7a7-0000-4000-8000-0000000000b1 nest-2 2026-10-04T14:00:00 2026-10-04T14:00:20 req_n1
nwindow 5e55a7a7-0000-4000-8000-0000000000b2 nest-3 2026-10-05T14:00:00 2026-10-05T14:00:30 req_n2
nwindow 5e55a7a7-0000-4000-8000-0000000000b3 nest-4 2026-10-05T15:00:00 2026-10-05T15:00:40 req_n3
N4=5e55a7a7-0000-4000-8000-0000000000b4
NB=a7000000000000001
aat $N4 /work/nest '"isSidechain":false'
{ atitle nest-5
  aprompt 2026-10-04T15:00:00 nest-5-01
  areply 2026-10-04T15:00:04 nest-5-02 req_n4 $OPUS "$UO" "$(ktool toolu_n4 Agent '{"subagent_type":"recon","description":"Lorem","prompt":"Lorem ipsum."}')"
  alaunch 2026-10-04T15:00:05 nest-5-03 toolu_n4 $NB
  areply 2026-10-04T15:00:10 nest-5-04 req_n5 $OPUS "$UO" "$TEXT"
} > "$ND/$N4.jsonl"
mkdir -p "$ND/$N4/subagents"
aat $N4 /work/nest "\"isSidechain\":true,\"agentId\":\"$NB\""
{ atask 2026-10-04T15:00:06 nest-5s-1
  areply 2026-10-04T15:00:09 nest-5s-2 req_n6 $SONNET "$US" "$TEXT"
} > "$ND/$N4/subagents/agent-$NB.jsonl"
ameta recon toolu_n4 > "$ND/$N4/subagents/agent-$NB.meta.json"
nissue() { # number title state closedAt parent sub-total sub-completed [separator]
  CLOSED=null; REASON=null; PARENT=null
  if [ "$3" = CLOSED ]; then CLOSED="\"$4\""; REASON='"COMPLETED"'; fi
  if [ -n "$5" ]; then PARENT="{\"number\":$5,\"repository\":{\"nameWithOwner\":\"acme/nest\"}}"; fi
  echo "{\"number\":$1,\"title\":\"$2\",\"state\":\"$3\",\"stateReason\":$REASON,\"createdAt\":\"2026-10-01T09:00:00Z\",\"closedAt\":$CLOSED,\"parent\":$PARENT,"
  echo " \"subIssuesSummary\":{\"total\":$6,\"completed\":$7},\"closedByPullRequestsReferences\":{\"nodes\":[]}}${8:-}"
}
{ echo '{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"CURSOR-N"},"nodes":['
  nissue 1 'Epic: lorem nest' OPEN '' '' 2 0 ,
  nissue 2 'Nested ipsum' OPEN '' 1 1 1 ,
  nissue 3 'Dolor leaf' CLOSED 2026-10-05T18:00:00Z 2 0 0 ,
  nissue 4 'Sit sibling' OPEN '' 1 0 0 ,
  nissue 5 'Amet abroad' OPEN '' '' 1 0
  echo ']}}}}'
} > fixtures/github/nest-home.json

# ---- perf-home: dispatch outcomes, follow-ups, API errors (#51); hand values in tests/performance.rs ----
# One orchestrator (window perf-1) and six background dispatches, 2026-10-06 10:00..10:15Z.
# Outcomes: b1 completed (its notification repeated across queue-operation,
# attachment and user records), b2 failed (completed then failed: the worst
# wins), b3 killed, b4 blocked (completed, its SubagentHandback report has a
# NEEDS_CONTEXT: line), b5 completed (NEEDS_CONTEXT only quoted mid-line), b6
# no notification (only quoted in a Bash command and its output). A
# background shell's failed notification matches no agent. Each dispatch's
# tool_result is its immediate launch result (#42), not its answer.
G=fixtures/perf-home
G1=5e55e1e1-0000-4000-8000-0000000000e1
GD="$G/projects/-work-perf"
rm -rf "$G"
mkdir -p "$GD/$G1/subagents"
B1=a6000000000000001; B2=a6000000000000002; B3=a6000000000000003
B4=a6000000000000004; B5=a6000000000000005; B6=a6000000000000006
note() { # taskId toolUseId status: a <task-notification> body, JSON-escaped
  echo "<task-notification>\\n<task-id>$1</task-id>\\n<tool-use-id>$2</tool-use-id>\\n<output-file>/work/tmp/$1.output</output-file>\\n<status>$3</status>\\n<summary>Lorem ipsum $3</summary>\\n</task-notification>"
}
gqueue() { # time operation note
  echo "{\"type\":\"queue-operation\",\"operation\":\"$2\",\"timestamp\":\"$1.000Z\",\"sessionId\":\"$G1\",\"content\":\"$3\"}"
}
gattach() { # time uuid note
  aenv "$1" "$2"; echo "{$AE,\"type\":\"attachment\",\"attachment\":{\"type\":\"queued_command\",\"prompt\":\"$3\",\"commandMode\":\"task-notification\"}}"
}
gnotice() { # time uuid note [origin]: the notification as a prompt
  aenv "$1" "$2"
  ORIGIN=""; if [ -n "${4:-}" ]; then ORIGIN=",\"origin\":{\"kind\":\"$4\"}"; fi
  echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"$3\"}$ORIGIN}"
}
gdispatch() { # time uuid requestId toolUseId [name]
  INPUT="{\"subagent_type\":\"implementer\",\"description\":\"Lorem\",\"prompt\":\"Lorem ipsum.\",\"run_in_background\":true"
  if [ -n "${5:-}" ]; then INPUT="$INPUT,\"name\":\"$5\""; fi
  areply "$1" "$2" "$3" $OPUS "$UO" "$(ktool "$4" Agent "$INPUT}")"
}
gsend() { # to...: SendMessage tool_use blocks
  B=""; i=0
  for to in "$@"; do
    i=$((i + 1)); [ -n "$B" ] && B="$B,"
    B="$B$(ktool "toolu_send_${to}_$i" SendMessage "{\"to\":\"$to\",\"message\":\"Lorem ipsum.\",\"summary\":\"Lorem\"}")"
  done
  echo "$B"
}
ghandback() { # message (JSON-escaped)
  ktool toolu_handback SubagentHandback "{\"message\":\"$1\"}"
}
gapierror() { # time uuid error text
  aenv "$1" "$2"; echo "{$AE,\"type\":\"assistant\",\"isApiErrorMessage\":true,\"error\":\"$3\",\"message\":{\"role\":\"assistant\",\"id\":\"msg_$2\",\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"$4\"}],\"usage\":$(ku 0 0 0 0)}}"
}
T=2026-10-06T10
aat $G1 /work/perf '"isSidechain":false'
{ atitle perf-1
  aprompt $T:00:00 g-01
  gdispatch $T:00:05 g-02 req_p1 toolu_p1; alaunch $T:00:06 g-03 toolu_p1 $B1
  gdispatch $T:00:10 g-04 req_p2 toolu_p2 perf-helper; alaunch $T:00:11 g-05 toolu_p2 $B2
  gdispatch $T:00:15 g-06 req_p3 toolu_p3; alaunch $T:00:16 g-07 toolu_p3 $B3
  gdispatch $T:00:20 g-08 req_p4 toolu_p4; alaunch $T:00:21 g-09 toolu_p4 $B4
  gdispatch $T:00:25 g-10 req_p5 toolu_p5; alaunch $T:00:26 g-11 toolu_p5 $B5
  gdispatch $T:00:30 g-12 req_p6 toolu_p6; alaunch $T:00:31 g-13 toolu_p6 $B6
  # quoted, not delivered: b6 stays without a notification
  areply $T:00:40 g-14 req_p7 $OPUS "$UO" "$(ktool toolu_p7 Bash "{\"command\":\"echo '$(note $B6 toolu_p6 failed)'\"}")"
  aenv $T:00:41 g-15; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_p7\",\"content\":\"$(note $B6 toolu_p6 failed)\"}]}}"
  gqueue $T:05:00 enqueue "$(note $B1 toolu_p1 completed)"
  gqueue $T:05:01 remove "$(note $B1 toolu_p1 completed)"
  gattach $T:05:01 g-16 "$(note $B1 toolu_p1 completed)"
  gnotice $T:05:02 g-17 "$(note $B1 toolu_p1 completed)" task-notification
  areply $T:05:05 g-18 req_p8 $OPUS "$UO" "$TEXT"
  gqueue $T:06:00 enqueue "$(note $B2 toolu_p2 completed)"
  gattach $T:06:01 g-19 "$(note $B2 toolu_p2 failed)"
  gnotice $T:07:00 g-20 "$(note $B3 toolu_p3 killed)"
  gqueue $T:08:00 enqueue "$(note $B4 toolu_p4 completed)"
  gqueue $T:09:00 enqueue "$(note $B5 toolu_p5 completed)"
  gqueue $T:09:30 enqueue "$(note bshell001 toolu_p0 failed)"
  areply $T:10:00 g-21 req_p9 $OPUS "$UO" "$(gsend $B1)"
  aresult $T:10:01 g-22 toolu_send_${B1}_1
  areply $T:10:05 g-23 req_p10 $OPUS "$UO" "$(gsend perf-helper nobody)"
  gapierror $T:11:00 g-24 invalid_request "Prompt is too long: 210000 tokens > 200000 maximum"
  gapierror $T:11:30 g-25 rate_limit "API Error: lorem ipsum"
  aenv $T:12:00 g-26; echo "{$AE,\"type\":\"system\",\"subtype\":\"compact_boundary\",\"content\":\"Conversation compacted\",\"compactMetadata\":{\"trigger\":\"auto\",\"preTokens\":1000}}"
  areply $T:13:00 g-27 req_p11 $OPUS "$UO" "$(ktool toolu_p11 AskUserQuestion '{"questions":[{"question":"Lorem?"}]}')"
  aresult $T:14:00 g-28 toolu_p11
  aprompt $T:15:00 g-29
  areply $T:15:05 g-30 req_p12 $OPUS "$UO" "$TEXT"
} > "$GD/$G1.jsonl"
gsub() { # agentId start-second: a subagent's envelope and task prompt
  aat $G1 /work/perf "\"isSidechain\":true,\"agentId\":\"$1\""
  atask $T:00:$2 "$1-1"
}
gfile() { echo "$GD/$G1/subagents/agent-$1.jsonl"; }
# b1: two requests, the first the deeper (100k vs 10k); a follow-up to b2 by name.
{ gsub $B1 07
  areply $T:01:00 b1-2 req_q1 $SONNET "$(ku 1000 0 99000 50)" "$(gsend perf-helper)"
  areply $T:01:10 b1-3 req_q2 $SONNET "$(ku 500 0 9500 50)" "$(ghandback "RESULT: lorem ipsum.")"; } > "$(gfile $B1)"
{ gsub $B2 12; areply $T:01:00 b2-2 req_q3 $SONNET "$US" "$(ghandback "RESULT: lorem.")"; } > "$(gfile $B2)"
{ gsub $B3 17; areply $T:01:00 b3-2 req_q4 $SONNET "$US" "$TEXT"; } > "$(gfile $B3)"
# b4: the handback is the report even with a text record after it.
{ gsub $B4 22
  areply $T:01:00 b4-2 req_q5 $SONNET "$US" "$(ghandback "RESULT: lorem.\\n- NEEDS_CONTEXT: lorem ipsum dolor.")"
  areply $T:01:05 b4-3 req_q6 $SONNET "$US" "$TEXT"; } > "$(gfile $B4)"
# b5: no handback, so the last text is the report; only an earlier one is blocked.
{ gsub $B5 27
  areply $T:01:00 b5-2 req_q7 $SONNET "$US" '{"type":"text","text":"NEEDS_CONTEXT: an earlier draft."}'
  areply $T:01:05 b5-3 req_q8 $SONNET "$US" '{"type":"text","text":"RESULT: done, no NEEDS_CONTEXT: raised."}'; } > "$(gfile $B5)"
{ gsub $B6 32; areply $T:01:00 b6-2 req_q9 $SONNET "$US" '{"type":"text","text":"NEEDS_CONTEXT: lorem."}'; } > "$(gfile $B6)"
for n in 1 3 4 5 6; do ameta implementer toolu_p$n > "$GD/$G1/subagents/agent-a600000000000000$n.meta.json"; done
echo '{"agentType":"implementer","description":"Lorem task","toolUseId":"toolu_p2","spawnDepth":1,"requestShape":"agent","requestNonInteractive":true,"model":"claude-sonnet-5-5","name":"perf-helper"}' > "$GD/$G1/subagents/agent-$B2.meta.json"
# The performance read (#51; gh JSON: github/perf-home.json, issue #1 closed
# 2026-10-06T12:00Z with PR #2, reopened once; #3 open; PR #4 reverts #2).
# G2, window perf-1 on 2026-10-05: issue #1's earlier work, outside a range
# from 10-06 but inside its whole-life cost. G3, window perf-3 on 2026-10-13
# (w/c 10-12): b7 never notified and last heard 09:01 (no report), b8 last
# heard 10:05 with an 850k-token request (85%), within 30 min of the store's
# newest record (G4's 10:31), so running; one API retry. G4, never named
# (Unlinked), is G3's /clear successor (one process root).
G2=5e55e1e1-0000-4000-8000-0000000000e2
G3=5e55e1e1-0000-4000-8000-0000000000e3
G4=5e55e1e1-0000-4000-8000-0000000000e4
B7=a6000000000000007; B8=a6000000000000008
mkdir -p "$GD/$G3/subagents"
aat $G2 /work/perf '"isSidechain":false'
{ atitle perf-1; aprompt 2026-10-05T09:00:00 j-01; areply 2026-10-05T09:00:30 j-02 req_t1 $OPUS "$UO" "$TEXT"; } > "$GD/$G2.jsonl"
U=2026-10-13T09
aat $G3 /work/perf "\"isSidechain\":false,\"session_id\":\"$G3\""
{ atitle perf-3
  aprompt $U:00:00 h-01
  gdispatch $U:00:05 h-02 req_r1 toolu_r7; alaunch $U:00:06 h-03 toolu_r7 $B7
  aenv $U:30:00 h-04; echo "{$AE,\"type\":\"system\",\"subtype\":\"api_error\",\"level\":\"error\",\"error\":{\"status\":529},\"retryInMs\":500,\"retryAttempt\":1,\"maxRetries\":10}"
  gdispatch $U:59:00 h-05 req_r2 toolu_r8; alaunch $U:59:01 h-06 toolu_r8 $B8
  areply 2026-10-13T10:20:00 h-07 req_r3 $OPUS "$UO" "$TEXT"
} > "$GD/$G3.jsonl"
aat $G3 /work/perf "\"isSidechain\":true,\"agentId\":\"$B7\""
{ atask $U:00:07 b7-1; areply $U:01:00 b7-2 req_r4 $SONNET "$US" "$TEXT"; } > "$GD/$G3/subagents/agent-$B7.jsonl"
aat $G3 /work/perf "\"isSidechain\":true,\"agentId\":\"$B8\""
{ atask $U:59:02 b8-1; areply 2026-10-13T10:05:00 b8-2 req_r5 $SONNET "$(ku 1000 0 849000 50)" "$TEXT"; } > "$GD/$G3/subagents/agent-$B8.jsonl"
ameta implementer toolu_r7 > "$GD/$G3/subagents/agent-$B7.meta.json"
ameta implementer toolu_r8 > "$GD/$G3/subagents/agent-$B8.meta.json"
aat $G4 /work/perf "\"isSidechain\":false,\"session_id\":\"$G3\""
{ aprompt 2026-10-13T10:30:00 i-01; areply 2026-10-13T10:31:00 i-02 req_s1 $OPUS "$UO" "$TEXT"; } > "$GD/$G4.jsonl"
cat > fixtures/github/perf-home.json <<'JSON'
{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"CURSOR-P"},"nodes":[
{"number":1,"title":"Lorem performance","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-10-05T09:00:00Z","closedAt":"2026-10-06T12:00:00Z","parent":null,
 "subIssuesSummary":{"total":0,"completed":0},"timelineItems":{"totalCount":1},
 "closedByPullRequestsReferences":{"nodes":[
   {"number":2,"state":"MERGED","createdAt":"2026-10-06T10:30:00Z","mergedAt":"2026-10-06T11:00:00Z","url":"https://github.com/example/perf/pull/2"}]}},
{"number":3,"title":"Ipsum follow-up","state":"OPEN","stateReason":null,"createdAt":"2026-10-06T09:00:00Z","closedAt":null,"parent":null,
 "subIssuesSummary":{"total":0,"completed":0},"timelineItems":{"totalCount":0},"closedByPullRequestsReferences":{"nodes":[]}}
]}},
"search":{"nodes":[
{"number":4,"body":"Reverts example/perf#2","mergedAt":"2026-10-06T11:40:00Z"}
]}}}
JSON

# ---- directs-home: director-launched units (#53); hand values in tests/directs.rs ----
# Cross-session messages, 2026-10-07Z. The sender's SendMessage result carries
# toolUseResult.msg_id; the receiver gets a user record whose origin (kind
# peer) carries the same msg_id and the sender's name. Edges: director-demo
# (R1) -> ticket-demo-repo-1 (N1), and -> ticket-demo-repo-2 (N2, messaged
# from a subagent of R1); director-alpha (X1) -> director-beta (X2). No edge:
# N1's reply to R1 (not a director's), R1's message to a session not on disk,
# R2 (R1's /clear successor, also director-demo) messaging N1 later (the first
# messager wins), general-notes (Q1) <-> ticket-demo-repo-3 (Q3), and X2's
# message back to X1 (it would close a cycle). N2 never replies. N1 dispatches
# a subagent (S2); ticket-demo-repo-2 later draws N3, a session nobody directed.
# X2 in turn directs ticket-demo-repo-4 (N4): a unit of a unit.
H=fixtures/directs-home
R1=5e55d7d7-0000-4000-8000-0000000000c1
# R2 sorts before R1 on purpose: the first messager is by time, not by id.
R2=5e55d7d7-0000-4000-8000-0000000000c0
N1=5e55d7d7-0000-4000-8000-0000000000e1
N2=5e55d7d7-0000-4000-8000-0000000000e2
N3=5e55d7d7-0000-4000-8000-0000000000e3
N4=5e55d7d7-0000-4000-8000-0000000000e4
Q1=5e55d7d7-0000-4000-8000-0000000000a1
Q3=5e55d7d7-0000-4000-8000-0000000000a3
X1=5e55d7d7-0000-4000-8000-0000000000b1
X2=5e55d7d7-0000-4000-8000-0000000000b2
S1=a7000000000000001
S2=a7000000000000002
HUB="$H/projects/-work-hub"
UNITS="$H/projects/-work-demo-repo"
rm -rf "$H"
mkdir -p "$HUB/$R1/subagents" "$UNITS/$N1/subagents" "$H/projects/-work-notes"
m() { echo "5e55ee00-0000-4000-8000-00000000000$1"; }
dname() { atitle "$1"; echo "{\"type\":\"agent-name\",\"agentName\":\"$1\",\"sessionId\":\"$AS\"}"; }
dsend() { # YYYY-MM-DDTHH:MM uuid to msgId: a SendMessage call at :00, its result at :01
  areply "$1:00" "$2" "req_$2" $OPUS "$UO" "$(ktool "toolu_$2" SendMessage "{\"to\":\"$3\",\"message\":\"Lorem ipsum.\",\"summary\":\"Lorem\"}")"
  R="{\"success\":true,\"message\":\"Lorem.\",\"display\":\"Lorem.\",\"msg_id\":\"$4\"}"
  RE=$(printf '%s' "$R" | sed 's/"/\\"/g')
  aenv "$1:01" "$2-r"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_$2\",\"content\":\"$RE\"}]},\"toolUseResult\":$R}"
}
dpeer() { # time uuid msgId senderName: a message received from another session
  aenv "$1" "$2"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Lorem ipsum.\"},\"origin\":{\"kind\":\"peer\",\"from\":\"uds:cc-msg-0000\",\"msg_id\":\"$3\",\"name\":\"$4\",\"fromMode\":\"prompting\"}}"
}
DT=2026-10-07T10
aat $R1 /work/hub "\"isSidechain\":false,\"session_id\":\"$R1\""
{ dname director-demo
  aprompt $DT:00:00 r1-01
  dsend $DT:01 r1-02 ticket-demo-repo-1 "$(m 1)"
  gdispatch $DT:01:30 r1-03 req_r1-03 toolu_r1d; aresult $DT:01:31 r1-04 toolu_r1d
  dpeer $DT:03:02 r1-05 "$(m 3)" ticket-demo-repo-1
  dsend $DT:04 r1-06 "ticket-demo-repo-9 [ref]" "$(m 4)"
} > "$HUB/$R1.jsonl"
aat $R1 /work/hub "\"isSidechain\":true,\"agentId\":\"$S1\""
{ atask $DT:01:40 s1-01; dsend $DT:02 s1-02 ticket-demo-repo-2 "$(m 2)"; } > "$HUB/$R1/subagents/agent-$S1.jsonl"
ameta implementer toolu_r1d > "$HUB/$R1/subagents/agent-$S1.meta.json"
aat $N1 /work/demo-repo "\"isSidechain\":false,\"session_id\":\"$N1\""
{ dname ticket-demo-repo-1
  dpeer $DT:01:02 n1-01 "$(m 1)" director-demo
  areply $DT:01:10 n1-02 req_n1-02 $OPUS "$UO" "$TEXT"
  gdispatch $DT:01:20 n1-05 req_n1-05 toolu_n1d; aresult $DT:01:21 n1-06 toolu_n1d
  dsend $DT:03 n1-03 director-demo "$(m 3)"
  dpeer 2026-10-07T11:01:02 n1-04 "$(m 5)" director-demo
} > "$UNITS/$N1.jsonl"
aat $N1 /work/demo-repo "\"isSidechain\":true,\"agentId\":\"$S2\""
{ atask $DT:01:25 s2-01; areply $DT:01:50 s2-02 req_s2-02 $SONNET "$US" "$TEXT"; } > "$UNITS/$N1/subagents/agent-$S2.jsonl"
ameta implementer toolu_n1d > "$UNITS/$N1/subagents/agent-$S2.meta.json"
aat $N2 /work/demo-repo "\"isSidechain\":false,\"session_id\":\"$N2\""
{ dname ticket-demo-repo-2; dpeer $DT:02:02 n2-01 "$(m 2)" director-demo
  areply $DT:02:10 n2-02 req_n2-02 $OPUS "$UO" "$TEXT"; } > "$UNITS/$N2.jsonl"
aat $N3 /work/demo-repo "\"isSidechain\":false,\"session_id\":\"$N3\""
{ dname ticket-demo-repo-2; aprompt 2026-10-07T13:00:00 n3-01
  areply 2026-10-07T13:00:10 n3-02 req_n3-02 $OPUS "$UO" "$TEXT"; } > "$UNITS/$N3.jsonl"
aat $R2 /work/hub "\"isSidechain\":false,\"session_id\":\"$R1\""
{ dname director-demo; aprompt 2026-10-07T11:00:00 r2-01
  dsend 2026-10-07T11:01 r2-02 ticket-demo-repo-1 "$(m 5)"; } > "$HUB/$R2.jsonl"
aat $Q1 /work/notes "\"isSidechain\":false,\"session_id\":\"$Q1\""
{ dname general-notes; aprompt 2026-10-07T09:00:00 q1-01
  dsend 2026-10-07T09:00 q1-02 ticket-demo-repo-3 "$(m 6)"
  dpeer 2026-10-07T09:01:02 q1-03 "$(m 7)" ticket-demo-repo-3; } > "$H/projects/-work-notes/$Q1.jsonl"
aat $Q3 /work/demo-repo "\"isSidechain\":false,\"session_id\":\"$Q3\""
{ dname ticket-demo-repo-3; dpeer 2026-10-07T09:00:02 q3-01 "$(m 6)" general-notes
  dsend 2026-10-07T09:01 q3-02 general-notes "$(m 7)"; } > "$UNITS/$Q3.jsonl"
aat $X1 /work/hub "\"isSidechain\":false,\"session_id\":\"$X1\""
{ dname director-alpha; aprompt 2026-10-07T12:00:00 x1-01
  dsend 2026-10-07T12:00 x1-02 director-beta "$(m 8)"
  dpeer 2026-10-07T12:01:02 x1-03 "$(m 9)" director-beta; } > "$HUB/$X1.jsonl"
aat $X2 /work/hub "\"isSidechain\":false,\"session_id\":\"$X2\""
{ dname director-beta; dpeer 2026-10-07T12:00:02 x2-01 "$(m 8)" director-alpha
  dsend 2026-10-07T12:01 x2-02 director-alpha "$(m 9)"
  dsend 2026-10-07T12:02 x2-03 ticket-demo-repo-4 "$(m a)"; } > "$HUB/$X2.jsonl"
aat $N4 /work/demo-repo "\"isSidechain\":false,\"session_id\":\"$N4\""
{ dname ticket-demo-repo-4; dpeer 2026-10-07T12:02:02 n4-01 "$(m a)" director-beta
  areply 2026-10-07T12:02:10 n4-02 req_n4-02 $OPUS "$UO" "$TEXT"; } > "$UNITS/$N4.jsonl"

# ---- chain-home: a /clear chain and its unit (#63); hand values in tests/chain.rs ----
# 2026-10-08Z. director-chain is one process (root L1) across three /clear
# links, L1 10:00, L2 11:00 and L3 12:00, each dispatching one foreground
# subagent (J1, J2, J3). L2 messages ticket-chain-repo-7 (U1, a --bg unit),
# which dispatches J4, then /clears into U2 (same name, J8). A second process
# (root P1) crosses workstreams: chain-repo-5 (P1, J5), then /clear and
# /rename chain-repo-6 (P2, J6), then /clear and /rename back to chain-repo-5
# (P3, J7). Every orchestrator request is opus ($0.008), every subagent's
# sonnet ($0.0035).
CH=fixtures/chain-home
L1=5e55c4c4-0000-4000-8000-0000000000a1
L2=5e55c4c4-0000-4000-8000-0000000000a2
L3=5e55c4c4-0000-4000-8000-0000000000a3
U1=5e55c4c4-0000-4000-8000-0000000000b1
U2=5e55c4c4-0000-4000-8000-0000000000b2
P1=5e55c4c4-0000-4000-8000-0000000000c1
P2=5e55c4c4-0000-4000-8000-0000000000c2
P3=5e55c4c4-0000-4000-8000-0000000000c3
CHUB="$CH/projects/-work-hub"
CREPO="$CH/projects/-work-chain-repo"
rm -rf "$CH"
mkdir -p "$CHUB" "$CREPO"
CT=2026-10-08T
cm() { echo "5e55ee11-0000-4000-8000-00000000000$1"; }
cat_() { # sessionId cwd root: an orchestrator envelope on process root `root`
  aat "$1" "$2" "\"isSidechain\":false,\"session_id\":\"$3\""
}
cwork() { # HH:MM uuid-prefix: a prompt, a foreground dispatch answered at :50, a reply
  aprompt "$CT$1:00" "$2-1"
  areply "$CT$1:05" "$2-2" "req_$2-2" $OPUS "$UO" "$(ktool "toolu_$2" Agent '{"subagent_type":"implementer","description":"Lorem","prompt":"Lorem ipsum."}')"
  aresult "$CT$1:50" "$2-3" "toolu_$2"
  areply "$CT$1:55" "$2-4" "req_$2-4" $OPUS "$UO" "$TEXT"
}
csub() { # dir sessionId agentId HH:MM uuid-prefix: the subagent `cwork` dispatched, 10..40 s in
  mkdir -p "$1/$2/subagents"
  aat "$2" /work/x "\"isSidechain\":true,\"agentId\":\"$3\""
  { atask "$CT$4:10" "$5-s1"; areply "$CT$4:40" "$5-s2" "req_$5-s2" $SONNET "$US" "$TEXT"; } > "$1/$2/subagents/agent-$3.jsonl"
  ameta implementer "toolu_$5" > "$1/$2/subagents/agent-$3.meta.json"
}
cat_ $L1 /work/hub $L1
{ dname director-chain; cwork 10:00 l1; } > "$CHUB/$L1.jsonl"
csub "$CHUB" $L1 a8000000000000001 10:00 l1
cat_ $L2 /work/hub $L1
{ dname director-chain; cwork 11:00 l2; dsend ${CT}11:02 l2-s ticket-chain-repo-7 "$(cm 1)"; } > "$CHUB/$L2.jsonl"
csub "$CHUB" $L2 a8000000000000002 11:00 l2
cat_ $L3 /work/hub $L1
{ dname director-chain; cwork 12:00 l3; } > "$CHUB/$L3.jsonl"
csub "$CHUB" $L3 a8000000000000003 12:00 l3
cat_ $U1 /work/chain-repo $U1
{ dname ticket-chain-repo-7; dpeer ${CT}11:02:02 u1-0 "$(cm 1)" director-chain; cwork 11:03 u1; } > "$CREPO/$U1.jsonl"
csub "$CREPO" $U1 a8000000000000004 11:03 u1
cat_ $U2 /work/chain-repo $U1
{ dname ticket-chain-repo-7; cwork 11:30 u2; } > "$CREPO/$U2.jsonl"
csub "$CREPO" $U2 a8000000000000008 11:30 u2
cat_ $P1 /work/chain-repo $P1
{ dname chain-repo-5; cwork 13:00 p1; } > "$CREPO/$P1.jsonl"
csub "$CREPO" $P1 a8000000000000005 13:00 p1
cat_ $P2 /work/chain-repo $P1
{ dname chain-repo-5; dname chain-repo-6; cwork 14:00 p2; } > "$CREPO/$P2.jsonl"
csub "$CREPO" $P2 a8000000000000006 14:00 p2
cat_ $P3 /work/chain-repo $P1
{ dname chain-repo-6; dname chain-repo-5; cwork 15:00 p3; } > "$CREPO/$P3.jsonl"
csub "$CREPO" $P3 a8000000000000007 15:00 p3

# ---- teams-home: teammates that went idle (#83); hand values in tests/teammates.rs ----
# 2026-10-09Z. TL leads team-lorem: four teammates in its subagents/, each
# turn end an idle_notification teammate-message in TL's own transcript,
# stamped by the teammate (the `timestamp` in the message body) and
# delivered when TL next reads (the record's timestamp). lorem-mate idles
# at 10:05 and stays idle. ipsum-mate idles at 10:04, TL messages it again
# at 10:07, and it works on. dolor-mate never idles (it sends TL a plain
# message). amet-mate idles at 10:03 but works again at 10:08, before TL
# reads its notice at 10:09.
T=fixtures/teams-home
TL=5e55d5d5-0000-4000-8000-0000000000a1
TP="$T/projects/-work-team-repo"
rm -rf "$T"
mkdir -p "$TP/$TL/subagents" "$T/teams/team-lorem"
echo "{\"name\":\"team-lorem\",\"createdAt\":1791000000000,\"leadAgentId\":\"team-lead@team-lorem\",\"leadSessionId\":\"$TL\",\"members\":[{\"name\":\"lorem-mate\"},{\"name\":\"ipsum-mate\"},{\"name\":\"dolor-mate\"},{\"name\":\"amet-mate\"}]}" > "$T/teams/team-lorem/config.json"
TT=2026-10-09T10
Q='\"'
tmsg() { # from attributes body: a teammate-message, JSON-escaped for a string
  printf '<teammate-message teammate_id=%s%s%s color=%sblue%s%s>\\n%s\\n</teammate-message>' "$Q" "$1" "$Q" "$Q" "$Q" "$2" "$3"
}
tidle() { # from MM:SS.mmm: an idle_notification body, JSON-escaped
  printf '{%stype%s:%sidle_notification%s,%sfrom%s:%s%s%s,%stimestamp%s:%s%s:%sZ%s,%sidleReason%s:%savailable%s,%sresult%s:%sLorem ipsum.%s}' \
    "$Q" "$Q" "$Q" "$Q" "$Q" "$Q" "$Q" "$1" "$Q" "$Q" "$Q" "$Q" "$TT" "$2" "$Q" "$Q" "$Q" "$Q" "$Q" "$Q" "$Q" "$Q" "$Q"
}
tnotice() { # time uuid content: a user record carrying teammate-messages
  aenv "$1" "$2"; echo "{$AE,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":$3}}"
}
aat $TL /work/team-repo '"isSidechain":false'
{ atitle team-repo-1
  aprompt $TT:00:00 tl-01
  areply $TT:00:10 tl-02 req_tl-02 $OPUS "$UO" "$TEXT"
  tnotice $TT:05:01 tl-03 "\"Lorem ipsum:\n$(tmsg ipsum-mate '' "$(tidle ipsum-mate 04:00.020)")\n$(tmsg lorem-mate '' "$(tidle lorem-mate 05:00.020)")\""
  tnotice $TT:06:30 tl-04 "\"$(tmsg dolor-mate " summary=${Q}Lorem > ipsum${Q}" 'Lorem ipsum dolor.')\""
  areply $TT:07:00 tl-05 req_tl-05 $OPUS "$UO" "$(ktool toolu_tl-05 SendMessage '{"to":"ipsum-mate","message":"Lorem ipsum.","summary":"Lorem"}')"
  tnotice $TT:09:00 tl-06 "[{\"type\":\"text\",\"text\":\"$(tmsg amet-mate '' "$(tidle amet-mate 03:00.020)")\"}]"
} > "$TP/$TL.jsonl"
tmate() { # agentId name: a teammate's meta
  echo "{\"agentType\":\"general-purpose\",\"description\":\"Lorem task\",\"name\":\"$2\",\"spawnDepth\":0,\"requestShape\":\"teammate\",\"requestNonInteractive\":false,\"model\":\"$SONNET\",\"teamName\":\"team-lorem\",\"color\":\"blue\"}" > "$TP/$TL/subagents/agent-$1.meta.json"
}
tturn() { # start end uuid-prefix: a teammate's prompt at `start`, its reply at `end`
  atask "$TT:$1" "$3-1"; areply "$TT:$2" "$3-2" "req_$3-2" $SONNET "$US" "$TEXT"
}
for m in 1:lorem-mate 2:ipsum-mate 3:dolor-mate 4:amet-mate; do tmate "a700000000000000${m%%:*}" "${m#*:}"; done
aat $TL /work/team-repo '"isSidechain":true,"agentId":"a7000000000000001"'
tturn 01:00 05:00 t1 > "$TP/$TL/subagents/agent-a7000000000000001.jsonl"
aat $TL /work/team-repo '"isSidechain":true,"agentId":"a7000000000000002"'
{ tturn 01:00 04:00 t2a; tturn 07:02 07:30 t2b; } > "$TP/$TL/subagents/agent-a7000000000000002.jsonl"
aat $TL /work/team-repo '"isSidechain":true,"agentId":"a7000000000000003"'
tturn 01:00 06:00 t3 > "$TP/$TL/subagents/agent-a7000000000000003.jsonl"
aat $TL /work/team-repo '"isSidechain":true,"agentId":"a7000000000000004"'
{ tturn 01:00 03:00 t4a; tturn 08:00 08:30 t4b; } > "$TP/$TL/subagents/agent-a7000000000000004.jsonl"
