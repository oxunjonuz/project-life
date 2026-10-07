#!/usr/bin/env python3
"""When there is no room, do the words name the disk? (round 298)

Round 296 fixed the *rule* — the stop threshold is the smaller of the fixed number and the
percentage, so a 926 GB volume with 1.8 GB free is not full. The owner's report of 2026-10-06 about
the build before that fix also contained a second, quieter complaint: the daemon logged ARCHIVE_FULL
"and did not create a heartbeat". That half survived into today's build. With writing stopped:

  * `heartbeat-check` — the command a cron job runs — said "nothing has been recorded recently" and
    offered `pl scan-once --all` / `pl daemon start`, neither of which can write anything here;
  * `healthcheck` blamed the schedule rather than the disk, while `doctor` in the same output said
    the disk was full;
  * `doctor`'s remedy for a full archive was `pl prune … --dry-run`, which deletes nothing and
    therefore frees nothing.

The check below stops writing without filling a disk (both numbers of the rule are raised, so the
threshold is 99 % of the volume), and then reads what each command says. Two things it insists on:

  * the *state* is measured independently of the program's own words — the journal file is read here
    with plain Python and the number of stored versions must not move while writing is stopped;
  * the space branch must be a branch: with the numbers back to normal, the ordinary remedies return.
    A program that always answered "the disk is full" would pass every other check here.

    python3 tools/no_room_words_check.py --pl /path/to/projectlife [--work DIR]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

PASS = 0
FAIL = 0


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS: {name}" + (f" — {detail}" if detail else ""))
    else:
        FAIL += 1
        print(f"  FAIL: {name}" + (f" — {detail}" if detail else ""))


def run(binary, home, archive, args, timeout=180):
    env = dict(os.environ)
    env["PROJECTLIFE_HOME"] = str(home)
    r = subprocess.run([binary, "--archive", str(archive)] + args,
                       capture_output=True, text=True, env=env, timeout=timeout)
    return r.returncode, r.stdout, r.stderr


def journal_versions(archive: Path) -> int:
    """How many versions the archive actually holds, counted from the journal files here — a
    different implementation from the one that wrote them."""
    total = 0
    for j in archive.glob("projects/*/events/*.jsonl"):
        for line in j.read_text(errors="replace").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except Exception:
                continue
            if e.get("type") in ("put", "snapshot"):
                total += 1
    return total


def set_config(binary, home, archive, key, value):
    rc, out, err = run(binary, home, archive, ["config", "set", key, value])
    assert rc == 0, f"config set {key}: {out}{err}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pl", required=True)
    ap.add_argument("--work", default="/tmp/pl-no-room")
    args = ap.parse_args()

    work = Path(args.work)
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    home = work / "home"
    archive = work / "archive"
    project = work / "project"
    home.mkdir()
    (project / "src").mkdir(parents=True)
    (project / "src" / "a.txt").write_text("first\n")
    (project / "src" / "b.txt").write_text("second\n")

    rc, out, err = run(args.pl, home, archive, ["init-archive", str(archive)])
    check("the archive is created", rc == 0, err.strip()[:120])
    rc, out, err = run(args.pl, home, archive,
                       ["add", str(project), "--name", "p", "--preset", "auto", "--yes"])
    check("the project is added", rc == 0, (out + err).strip()[:120])

    before = journal_versions(archive)
    check("the archive starts with versions in it", before > 0, f"{before} version(s)")

    # Stop writing: both numbers of the rule have to move, because the threshold is the smaller of
    # them. 99 % of any volume that is not already nearly full is above its free space.
    set_config(args.pl, home, archive, "stopFreeBytes", "1000000000000000")
    set_config(args.pl, home, archive, "stopFreePercent", "99")

    # 1. A change happens while writing is stopped.
    (project / "src" / "a.txt").write_text("second\n")
    rc, out, err = run(args.pl, home, archive, ["scan-once", "--all"])
    check("the pass refuses to write while there is no room", rc != 0 and "ARCHIVE_FULL" in out,
          f"exit {rc}")

    # 2. The state is measured here, from the journal bytes: nothing was stored.
    after = journal_versions(archive)
    check("no version was stored during the stop (counted from the journal, not from the program)",
          after == before, f"{before} -> {after}")

    # 3. heartbeat-check — the cron recipe — must name the disk.
    rc, hb, err = run(args.pl, home, archive, ["heartbeat-check"])
    check("heartbeat-check is not silent about a stopped write",
          "writing is stopped" in hb and "stop threshold" in hb, hb.strip().splitlines()[:1])
    check("heartbeat-check names the disk as the fix", "free space on the volume holding" in hb, "")
    check("heartbeat-check offers no command that cannot write",
          "pl scan-once" not in hb and "pl daemon start" not in hb,
          "; ".join(l for l in hb.splitlines() if l.startswith("fix:"))[:120])

    rc, hbj, err = run(args.pl, home, archive, ["heartbeat-check", "--json"])
    doc = json.loads(hbj[hbj.find("{"):]) if "{" in hbj else {}
    check("the machine-readable answer carries the same verdict",
          doc.get("storage", {}).get("stop") is True, json.dumps(doc.get("storage", {}))[:100])

    # 4. healthcheck must blame the disk, and the doctor in the same output must agree.
    rc, hc, err = run(args.pl, home, archive, ["healthcheck"])
    check("healthcheck is an error while nothing can be written", rc != 0, f"exit {rc}")
    check("healthcheck's heartbeat line carries the cause",
          "stop threshold" in hc and ("nothing has been recorded" in hc or "nothing has ever been recorded" in hc),
          "")
    check("healthcheck offers no command that cannot write",
          "pl scan-once" not in hc and "pl daemon start" not in hc, "")
    check("healthcheck names the full disk as a problem in its own right",
          "free space" in hc and "below the stop threshold" in hc, "")

    # 5. doctor's remedy has to be able to free space.
    rc, doc_out, err = run(args.pl, home, archive, ["doctor"])
    check("doctor reports the stopped write as an error", rc != 0, f"exit {rc}")
    fix = [l for l in doc_out.splitlines() if "fix:" in l and "prune" in l]
    check("doctor's remedy for a full archive can actually free space",
          bool(fix) and "without --dry-run" in fix[0], (fix[0] if fix else "no prune remedy found").strip()[:160])

    # 6. The room comes back, and the change made during the stop is recorded — which is what makes
    #    the sentence "nothing is lost" true rather than reassuring.
    set_config(args.pl, home, archive, "stopFreeBytes", "524288000")
    set_config(args.pl, home, archive, "stopFreePercent", "1")
    rc, out, err = run(args.pl, home, archive, ["scan-once", "--all"])
    check("the pass writes again once there is room", rc == 0, (out + err).strip()[:120])
    healed = journal_versions(archive)
    check("the change made during the stop was recorded afterwards (counted here)", healed > before,
          f"{before} -> {healed}")

    # 7. The control: the space branch is a branch, and the ordinary advice comes back.
    rc, hb2, err = run(args.pl, home, archive, ["heartbeat-check"])
    check("with room again, heartbeat-check is back to the ordinary words",
          "writing is stopped" not in hb2, hb2.strip().splitlines()[:1])
    check("and its ordinary remedies are the ones it offers",
          "pl scan-once" in hb2 or "pl daemon status" in hb2 or rc == 0, f"exit {rc}")

    print(f"\n{PASS} PASS, {FAIL} FAIL")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
