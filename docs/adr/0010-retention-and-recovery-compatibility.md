# ADR-010: Retention, backup and restore, and checkpoint compatibility

- Status: Reserved
- Owner issue: [114](../roadmap/114-retention-management-and-inspection.md) (retention classes, defaults, and expiry)
- Guide: [§4.5 retention and recovery](../architecture/implementation-guide.md#45-retention-and-recovery), [§10.2 backups and restore](../architecture/implementation-guide.md#102-backups-and-restore), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)

This number is reserved. No decision is recorded yet. The owner issue replaces this file with the ADR, using the [ADR template](template.md).

## Scope gap

Guide §14 scopes ADR-010 as backup and restore, upgrades, and checkpoint compatibility. The roadmap assigns ADR-010 to [114](../roadmap/114-retention-management-and-inspection.md), which covers retention only. No roadmap issue yet owns the backup, upgrade, and checkpoint-compatibility parts. [105](../roadmap/105-online-consistent-backup.md), [108](../roadmap/108-backup-verification-and-restore.md), and [112](../roadmap/112-format-upgrade-and-migration.md) produce the evidence those parts need.

## Open questions

- Retention defaults and precedence. The roadmap's starting recommendation is that the most conservative holder wins and that every holder is inspectable.
