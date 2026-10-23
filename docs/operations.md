# Operating the experimental engine

## Before opening

Use a private directory on a persistent local filesystem. Its parent must already exist. Do not share it through NFS/SMB, mount it simultaneously into independent writer processes, or let another user edit its contents. New files/directories are restricted to the owner; verify permissions of preexisting paths yourself. Do not remove `LOCK` to resolve a lock error.

Default limits are 100,000 indexed keys and 64 MiB selected log bytes; key/value bounds are 1 KiB/64 KiB. Expired entries can remain indexed until purge/compaction. Admission limits are not a promise of low RSS at every supported cardinality. Compaction temporarily needs two keydirs, sorted references, and old+new log files. Provision twice the log cap plus filesystem margin. The in-memory mode holds every value and is primarily for semantics/testing.

`Options` can change key/log limits at open; the compact threshold must be positive and no greater than the log cap. Existing logs/key counts must fit the supplied options, so lower limits can make open fail. No live data is evicted when an admission limit is reached.

## Maintenance and health

`Db::stats()` exposes indexed-key count/bytes, selected-log bytes, mutation sequence, and health. It does not yet provide latency histograms or distinguish every live/expired key. `purge_expired()` on disk performs a complete generation compaction; `compact()` also removes overwritten/deleted history. These operations pause reads and writes. No background thread automatically schedules expiry sweeps.

Appends consider compaction at the configured threshold when history is more than twice the indexed record bytes, or when the next operation would exceed the cap. Puts preserve room for one maximum tombstone so deletes can make progress; later compaction reclaims prior tombstones. Filesystem ENOSPC can still interrupt writes even if logical limits permit them.

A capacity rejection is an ordinary refused operation. An I/O or integrity error invalidates the owner. Stop accepting state operations, preserve diagnostics, drop all clones, correct the underlying issue, and reopen. The mutation that returned an I/O error may be present or absent on recovery; do not blindly retry. A mutex poisoned by a panic also requires dropping/reopening, rather than ignoring poison.

## Backup and restore

1. Stop ingress and all application writers using the database.
2. Drop every database handle or stop the owner process. Confirm no process still owns the directory.
3. Copy the entire directory to independent storage while it is offline. `LOCK` is only a regular file in the backup; its OS lock is not copied.
4. Keep the backup immutable. Test restoration into a new directory by opening it, checking representative known data and versions, and running application validation.
5. Record the backup timestamp, supported format/application version, retention, and restoration result. Resume the original owner only after its copy is complete.

For restore, stop the intended owner, copy a known-good backup into a new private directory, open it, and validate before routing traffic there. A restored backup rolls back mutations and versions; stale clients must re-read state. Expiry remains absolute, so records can expire while the backup is stored. Set backup frequency and restore-time objectives appropriate to the application; YaliDB supplies no automatic backup scheduler or volume-loss recovery.

Never copy live files as a supported snapshot, and never edit or delete individual log/manifest files to repair a database. A missing manifest in a nonempty directory is deliberately an error, including interrupted first initialization. Preserve a copy for investigation; a production salvage/migration CLI is not yet implemented.

## Clock and platform assumptions

Clock readings are clamped within an owner lifetime. Forward jumps can expire data early; backward jumps across restart can extend visibility for records not durably purged. TTL is unsuitable for security-sensitive authorization decisions. Keep the host clock maintained and test the application's degraded behavior when state is unavailable.

The implementation calls `sync_all` and synchronizes directories. These calls rely on platform/filesystem/device flush behavior; macOS development tests do not establish Linux or power-loss durability. The core supports Unix APIs only. Consult the [production gates](production-readiness.md) before making stronger deployment claims.

## HTTP limits and stopping

The optional service enforces connection, queue and request bounds described in [HTTP hardening](http-hardening.md). SIGINT/SIGTERM stops ingress and drains in-flight work within a configured deadline. A lost response or exceeded shutdown deadline can leave an unknown mutation outcome; reopen and reconcile rather than blindly retrying. Health checks remain separate from blocking disk work, though connection saturation can still prevent admission. [Simulated power-loss tests](power-loss-testing.md) supplement process-kill tests and do not certify a real storage stack.
