# Security and privacy

## What the program does on the network

Nothing. There are no sockets, no telemetry, no accounts, no update checks, no crash reporting. A
test asserts it: the archive and the project are local paths, and no code path opens a connection.
If you want to prove it yourself: `strace -f -e trace=network projectlife scan-once --all` shows no
connect() calls, and there is no HTTP client in the binary at all (the dependency list is `sha2`,
`serde`, `serde_json`, `libc`).

## The threat it addresses

An AI agent, a script or a careless command can rewrite, move or delete a whole project in seconds.
Project Life keeps an external record of what the project looked like, so the state from a minute
ago still exists after the folder is gone. It is a flight recorder, not an access-control system.

## What it protects against

* **Mass deletion or rewrites** in the project: every observed file has a version in the archive,
  and the restore path never writes into the project unless explicitly asked.
* **Corruption in transit**: contents are written as blobs addressed by their own sha256, verified on
  every read; a mismatch is an error, never a silently wrong file.
* **Silent data loss** in normal use: the journal is append-only, the cache is disposable, and a
  crash cannot produce an event without contents.
* **Secrets leaking into the archive by accident**: `.env`, keys and credentials are skipped by
  default, with the reason and rule recorded and printed.
* **Path traversal**: a path in the journal or an export that escapes the target folder is refused,
  so a tampered journal cannot write outside the folder you asked for.
* **Undetected tampering with the archive itself**: `audit-archive` digests every archive file, so
  removal or substitution is detectable (see below).

## What it does NOT protect against, stated plainly

1. **An agent running as your user can delete the archive.** The program cannot prevent this and does
   not claim to. What it offers: a different disk, 0700 permissions, `export`, and `audit-archive`
   for detection after the fact.
2. **The archive is not encrypted.** Anyone who can read the disk can read every version of your
   source. That is why secrets are excluded by default, and why turning `includeSecrets` on prints a
   warning.
3. **The project folder itself is not protected** from being modified: the program only reads it.
4. **A file that was never observed is not recoverable** — this is a limitation, and it is in
   `LIMITATIONS.md` rather than being quietly implied.

## Permissions

* Archive root: created with `0700` (owner only) on Unix.
* Blob files: `0400` after writing, where the OS supports it.
* The location file lives in your user configuration directory
  (`~/.config/projectlife/location.json`, `~/Library/Application Support/projectlife/location.json`,
  `%APPDATA%\projectlife\location.json`).
* The program writes **nothing** inside the project folder. The only exception is an explicit
  `restore --into-project`. `.projectlifeignore` is only read, never created.

## Confirmation of irreversible operations

`prune`, `archive-delete`, `restore --clean` and `check --fix` require typing the project name or an
explicit `--yes`. Without an interactive terminal and without `--yes` they refuse instead of
assuming consent. `archive-delete` also prints what will be lost (versions, size, history range) and
can export first (`--export-first`).

## Tampering detection in detail

```sh
projectlife audit-archive --update     # writes manifest/YYYY-MM-DD.jsonl: path, sha256, size, ts
```

Each later run compares the current archive against the newest digest and reports:

```
REMOVED from the archive: projects/<id>/blobs/ab/cd/<hash>
CHANGED inside the archive: projects/<id>/events/2026-10.jsonl
```

Without `--update` nothing is written, so a digest can never hide what it found. A digest is only as
good as where it is kept: if you copy `manifest/` to another machine or disk, an attacker who wipes
the archive cannot also wipe the evidence of what was there.

`pl drill <project>` complements this from the other side: it proves that a state which *should* be
restorable still restores byte for byte, on your machine, with your data.

## The agent surface (round 292)

An agent that can read the archive is useful; an agent that can rewrite history is not. The MCP
server is therefore built as a reading window:

- **Not registered, not callable.** The write operations (`restore`, `panic`, `prune`, `import`,
  `export-and-prune`, `archive-delete`, `mark`, `pause`/`resume`/`remove`, `scan-once`,
  `doctor --fix-lock`, `check --fix`) do not appear in `tools/list`, and a call is refused with the
  words "write operation" so the agent learns why rather than guessing.
- **Contents are not handed out by default.** `pl_diff` returns `added`/`removed`/`changed`; bytes
  need `include_content: true`, are capped, and are still refused for any path the project's own
  filters classify as a secret — the refusal names the path, the reason and the rule. The *fact* of a
  change is reported even then, because a secret is a file, not a change.
- **The process touches one file.** `logs/mcp.log`: timestamp, client name, tool, parameters, outcome,
  duration. The parameters are logged as the question, never the contents. `tools/mcp_client.py`
  hashes the whole archive around a full exercise of every tool and requires exactly one changed file.
- **Enforced, not promised.** `tools/mcp_audit.py` strips comments and strings from the two files
  that make up the reading surface and refuses a list of write symbols; it also runs a positive
  control on a copy with a write call injected by hand, so an audit that has gone blind fails.
- **No network.** The server speaks stdio only; the archive is still never encrypted, so an MCP
  client with filesystem access can read blobs directly — the server adds a *discoverable* window,
  not a new boundary. Do not expose it over a socket: it has no authentication by design, because it
  has nothing to authenticate to.

