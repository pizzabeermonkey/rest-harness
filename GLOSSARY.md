# REST Harness

An interactive desktop REST API client for organizing, building, sending, and inspecting HTTP requests backed by git-friendly local YAML files.

## Language

**Workspace**:
A root directory on disk containing one or more Collections alongside gitignored local state such as History Entries.
_Avoid_: Project, repository, profile

**Collection**:
A top-level group of saved Requests, nested Folders, and Collection-scoped Environments stored as a subdirectory inside a Workspace.
_Avoid_: Project, suite, repo

**Folder**:
A named subdirectory inside a Collection (or another Folder) that groups related Requests hierarchically.
_Avoid_: Subcollection, group, category

**Request**:
A saved HTTP request template stored as a YAML file inside a Collection or Folder, defining the method, URL, query parameters, headers, and body.
_Avoid_: Endpoint, call, route, API

**Draft Request**:
An unsaved, in-memory HTTP request being composed in a Tab that does not yet belong to a Collection on disk.
_Avoid_: Scratch request, temp request, untitled endpoint

**Environment**:
A named set of key-value variables scoped to a Collection that can be interpolated into a Request via `{{variable}}` syntax when sent.
_Avoid_: Config, profile, stage

**Secret**:
A sensitive Environment variable value stored in a gitignored `.secrets.yaml` file inside its Collection directory and redacted (`***`) in History Entries so credentials are never leaked or committed to Git.
_Avoid_: Private variable, hidden param

**History Entry**:
An immutable local record of an executed HTTP request (with Secrets redacted) and its received response at a specific point in time, stored in the Workspace's gitignored local state.
_Avoid_: Log, audit trail, trace

**Tab**:
An open working buffer in the UI holding a Draft Request or loaded Request (tracking dirty state until explicitly saved) alongside its most recent response.
_Avoid_: Window, session, pane
