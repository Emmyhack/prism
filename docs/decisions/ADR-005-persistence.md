# ADR-005: Project persistence

Status: Proposed. Use a versioned `.prism` package with SQLite for transactional
structured state and indexes. Keep original media and large caches external.
Domain objects do not derive persistence serialization; adapters map schemas.
Save through transactions, integrity checks, fsync policy, and atomic package
replacement where supported. Gate on crash injection, disk-full, migrations,
unknown versions, backups, relinking, and portable archive tests.
