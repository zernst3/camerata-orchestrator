# Stall Policy & Cancel: Dual Per-Project Thresholds

**Date:** 2026-06-23  
**Status:** Implemented  
**Branch:** feat/stall-policy-cancel

## Decision

Introduce per-project stall detection thresholds split by run context:
- `watched_secs` (default 120s): interactive/watched runs; stall triggers an alert
- `routine_secs` (default 600s): autonomous/walk-away runs; stall triggers cancellation

## RunKind and StallPolicy

`RunKind::Watched` → `StallPolicy::Alert`  
`RunKind::Autonomous` → `StallPolicy::Cancel`

## Cancel endpoints

- `POST /api/runs/:id/cancel` — cancel a run
- `POST /api/onboard/audit/job/:id/cancel` — cancel an audit job

## Job heartbeat

`det_tool_running` and `det_tool_done` update `JobMeta.last_activity_ms` so idle time can be tracked per job.
