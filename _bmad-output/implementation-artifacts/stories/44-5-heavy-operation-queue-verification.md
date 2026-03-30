# Story 44.5: Heavy Operation Queue Verification

Status: ready-for-dev

## Story

As a **kernel maintainer**,
I want all expensive operations verified to run through appropriate queues or async boundaries,
so that no request-blocking heavy operation can degrade site responsiveness.

## Acceptance Criteria

1. Audit and document whether image style generation is correctly queued (not inline in request)
2. Audit and document whether webhook delivery is correctly queued (not inline in request)
3. Audit and document whether search index rebuild is correctly queued (not inline in request)
4. Audit and document whether bulk stage publishing uses bounded transactions (not unbounded)
5. Audit and document whether email sending is correctly queued (not inline in request)
6. Audit and document whether AI requests are async and not blocking the Tokio runtime
7. Any inline heavy operation discovered gets a tracking issue filed
8. Queue architecture documented in operational docs
9. Verify `tap_queue_worker` has a 150-second timeout on task execution
10. If all operations are correctly queued, this is a verification-only story with no code changes

## Tasks / Subtasks

- [ ] Audit image style generation path — trace from upload/request to style creation (AC: #1)
- [ ] Audit webhook delivery path — trace from event trigger to HTTP dispatch (AC: #2)
- [ ] Audit search index rebuild path — trace from trigger to index write (AC: #3)
- [ ] Audit bulk stage publishing — check transaction scope and batch sizing (AC: #4)
- [ ] Audit email sending path — trace from trigger to SMTP/API call (AC: #5)
- [ ] Audit AI request path — verify async/await without `block_in_place` or `spawn_blocking` misuse (AC: #6)
- [ ] File tracking issues for any inline heavy operations found (AC: #7)
- [ ] Document queue architecture: which queue backend, task format, retry policy, timeout (AC: #8)
- [ ] Verify `tap_queue_worker` timeout is 150s (AC: #9)
- [ ] Write summary document with findings for each audited operation (AC: #10)

## Dev Notes

### Architecture

This is primarily an audit and verification story. The expected outcome is a documented inventory of all heavy operations with confirmation that each runs through the `tap_queue_worker` system or an equivalent async boundary. The queue system uses the tap (hook) infrastructure to dispatch work items to background workers.

Key operations to trace:
- **Image styles**: Should be generated via queue worker, not synchronously during upload or first request.
- **Webhooks**: Should be dispatched via queue to avoid blocking the triggering request on external HTTP latency.
- **Search indexing**: Full rebuilds should be queued; incremental updates may be inline if fast enough.
- **Bulk publishing**: Should use batched transactions with configurable batch size to avoid long-running locks.
- **Email**: Should be queued to avoid SMTP latency blocking requests.
- **AI requests**: Should use async HTTP clients within Tokio tasks, never `std::thread::sleep` or blocking I/O.

### Testing

No new integration tests expected unless code changes are required. If fixes are needed, each fix should include its own test coverage.

### References

- `tap_queue_worker` implementation in plugin/tap infrastructure
- Image style generation in file/image services
- Search index service
- Stage publishing in content services
