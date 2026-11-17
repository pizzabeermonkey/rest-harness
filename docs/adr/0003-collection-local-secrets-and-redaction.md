# Store Secrets in Collection-Local Gitignored YAML Files

Status: accepted

Sensitive Environment values (Secrets) are stored in a gitignored `.secrets.yaml` file inside each Collection directory alongside the version-controlled `environments/*.yaml` files, rather than in an OS keyring daemon or inside the plain Environment files. When a Request is sent, values from `.secrets.yaml` override or supply variables for the selected Environment, and any Secret values are redacted as `***` before writing History Entries to disk. This prevents credentials from being committed to Git or leaked into local history logs without introducing runtime dependencies on OS-specific secret-service daemons.
