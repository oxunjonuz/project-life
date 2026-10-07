#!/usr/bin/env python3
"""Does the macOS shell still keep the promises the window depends on?

The shell cannot be run here — there is no Mac in this container — so the next best thing is to read
the source it is built from and check the properties that made round 295 fail:

  * the child's stderr is read and appended to daemon.log (on the Mac the reason was written into a
    pipe nobody read, and the window said "reinstall");
  * the window's failure text is the server's own report, not a fixed sentence;
  * the generic "reinstall" instruction is gone;
  * the handoff socket is opened *before* the child is launched (the child connects to it; it must
    already be listening) and the child is told about it with --ipc;
  * the child is asked for an explicit local port and a ladder, not for the kernel's choice;
  * the failure text names the log file and the one command that turns a report into a measurement.

This is a source check, not a proof: it is paired with a positive control in verify.sh (a copy with
the stderr reader removed must fail it), which is the only thing that makes a grep-shaped check worth
running. Usage: python3 tools/shell_contract_check.py [--file path]
"""
import argparse
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


def method_body(source, signature):
    """The body of one function or method, by brace matching.

    Two of this round's mutations survived a check that only looked for a message *inside* the file:
    the guard was disabled and the sentence explaining it stayed. So the checks below read the body
    of the function that makes the decision, not the file as a whole.
    """
    i = source.find(signature)
    if i < 0:
        return ""
    j = source.find("{", i)
    if j < 0:
        return ""
    depth = 0
    for k in range(j, len(source)):
        c = source[k]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return source[j : k + 1]
    return ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--file", default=str(Path(__file__).resolve().parent.parent / "app/macos/ProjectLife.m"))
    args = ap.parse_args()
    p = Path(args.file)
    if not p.is_file():
        print(f"no shell source at {p}")
        return 2
    s = p.read_text()
    print(f"shell source: {p} ({len(s)} bytes)\n")

    # 1. the child's stderr is piped, read on a thread, and written to the log
    check("the child's stderr is a pipe", "t.standardError = err" in s or "t.standardError = err;" in s)
    check("a reader is started for that pipe", "readStderr:" in s and "[self readStderr:" in s)
    check("what it reads reaches the log file",
          "captureChildText" in s and "appendToLog" in s and "captureChildText:text" in s)
    check("the log file is the app's own daemon.log, and the child is told to use it",
          'stringByAppendingPathComponent:@"daemon.log"' in s and '@"--log-dir"' in s)

    # 2. the window shows the server's own report
    check("the failure alert shows the server's report, not a constant sentence",
          "self.server.lastErrorText" in s)
    check("the report names the log path", 'Everything is written here:' in s)
    check("the report names the one command that measures the machine", '--diagnose' in s)

    # 3. the generic sentence is gone
    check("nothing tells the person to reinstall the app", "Reinstall" not in s)
    check("nothing claims the reason without stating it",
          "run the bundled" not in s and "see the reason" not in s)

    # 4. the handoff socket exists before the child does
    i_open = s.find("openHandoffSocket")
    i_launch = s.find("launchAndReturnError")
    check("the handoff socket is opened before the child is launched",
          0 <= i_open < i_launch, f"open at {i_open}, launch at {i_launch}")
    check("the child is told where it is", '@"--ipc"' in s and "self.ipcPath" in s)
    check("the app binds on the child's behalf when asked", "answerNeedSocket" in s and "PLSendMsg" in s)
    check("the descriptor travels with SCM_RIGHTS", "SCM_RIGHTS" in s and "CMSG_DATA" in s)
    check("the app logs its own bind attempts, with the system's words",
          "bindLadderFrom" in s and "strerror(errno)" in s)

    # 5. an explicit local port, not the kernel's choice
    check("the child is asked for an explicit port", '@"--port", @"7717"' in s)
    check("and a ladder of neighbours", '@"--port-range"' in s)
    check("the kernel's own choice is not the first thing asked for", '@"--port", @"0"' not in s)

    # 6. the log is opened for appending and survives several writers
    check("the log is opened in append mode", "fileHandleForWritingAtPath" in s and "seekToEndOfFile" in s)

    # 7. the bundle finds its own programs (round 296: the owner measured this one on his Mac)
    check("neither program is looked up by naming the resource folder twice",
          'inDirectory:@"Resources"' not in s and 'subdirectory:@"Resources"' not in s,
          "round 295 shipped the lookup that asks for the resource folder inside the resource folder — nil on the Mac")
    resolver = method_body(s, "PLToolPath(NSString *name")
    check("the resolver reads the folder flag and refuses a folder",
          "if (isDir)" in resolver and "isExecutableFileAtPath" in resolver and "continue" in resolver,
          f"{len(resolver)} chars of PLToolPath read")
    prog = method_body(s, "PLIsProgram(NSString *path)")
    check("the program test refuses a folder itself", "if (isDir) return NO;" in prog)
    check("both programs go through the one resolver",
          'PLToolPath(@"projectlife-ui"' in s and 'PLToolPath(@"projectlife"' in s)
    check("an incomplete bundle reports every path it tried",
          "no such file" in s and "not executable" in s and "packaging fault" in s)

    # 8. the menu bar shows what is true, not what is alive
    refresh = method_body(s, "- (void)refreshState {")
    check("the menu-bar state is read from the protection report",
          'p[@"state"]' in refresh and 'p[@"protected"]' in refresh,
          f"{len(refresh)} chars of refreshState read")
    check("a full archive has its own icon and its own words",
          '"paused_full"' in refresh and "exclamationmark.triangle" in refresh)
    check("a live process is no longer enough to say Protected",
          'if (byApp) {\n        self.lastState = @"Protected";' not in s)

    # 9. JavaScript's own dialogs are answered by the shell, not swallowed
    check("the web view has a UI delegate", "WKUIDelegate" in s and "self.web.UIDelegate = self" in s)
    check("the text-input panel is implemented", "runJavaScriptTextInputPanelWithPrompt" in s)
    check("alert and confirm are implemented too",
          "runJavaScriptAlertPanelWithMessage" in s and "runJavaScriptConfirmPanelWithMessage" in s)

    # 10. the window itself does not depend on a dialog the shell may not show
    js = Path(args.file).resolve().parent.parent / "ui" / "app.js"
    if js.is_file():
        j = js.read_text()
        check("the window draws its own dialog instead of window.prompt",
              "window.prompt" not in j and "askText(" in j and "modal-back" in j,
              str(js))
    else:
        check("the window's source is where the shell's is", False, f"no {js}")

    print(f"\n{PASS} PASS, {FAIL} FAIL")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
