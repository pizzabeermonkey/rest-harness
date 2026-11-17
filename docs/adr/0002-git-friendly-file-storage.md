# Store Collections and Requests as Git-Friendly YAML Files

Status: accepted

Collections, Folders, Requests, and Environments are persisted directly on the filesystem as human-readable YAML files inside a directory hierarchy (one directory per Collection and nested Folder, one `.yaml` file per Request, and an `environments/` subdirectory per Collection) rather than inside SQLite or a single monolithic JSON file. YAML provides clean diffs for Git version control and readable multi-line block scalars (`|`) for JSON/raw request bodies. Sensitive Environment values (Secrets) are stored separately in a gitignored `.secrets.yaml` file inside each Collection directory, and History Entries are stored in gitignored local state so credentials and noisy execution logs are never committed.
