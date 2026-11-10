# budget
## Notifications (producer)

Budget publishes personal notifications to the `notification` service through a transactional
outbox (`budget_notify_outbox`, drained by `libs/event`). Everything is **off by default**.

| Env | Default | Meaning |
|-----|---------|---------|
| `NOTIFY_PUBLISH_ENABLED` | `false` | Master switch: starts the outbox drainer and the daily debt reminder sweep. |
| `NOTIFICATION_GRPC_URL` | – | Required when enabled, e.g. `http://notification.philandz.svc.cluster.local:50109`. |
| `INTERNAL_SERVICE_TOKEN` | – | Required when enabled; sent as `x-internal-token` on `Publish`. |
| `DEBT_REMINDER_HOUR` | `7` | Hour (Asia/Saigon) the daily sweep runs. |
| `NOTIFY_OUTBOX_INTERVAL_SECS`, `_BATCH_SIZE`, `_MAX_ATTEMPTS`, `_RETENTION_SECS` | 5 / 50 / 8 / 604800 | Drainer tuning. |

Debt reminders (to Owner/Manager/Contributor of the debt budget, from the same schedule the debt
book shows, so amounts are estimates): 3 days before the due date, on the due date, overdue after
1 / 7 / 14 days, and 7 days before a scheduled rate change. Each reminder has a deterministic event
id and `dedup_key`, so restarts, several replicas or a re-run never notify twice.

Check the outbox: `SELECT event_type, attempts, last_error FROM budget_notify_outbox WHERE delivered_at IS NULL;`
Rows that failed permanently or ran out of attempts have `dead_at` set and are kept for inspection.

Tests: `cargo test --test debt_reminders_test` (planning, sink mapping); add
`DATABASE_URL=… -- --include-ignored` for the repository paging test.
