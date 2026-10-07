/* Project Life — the Windows shell.
 *
 * What this program is: the window, the tray icon and the menu, and nothing else. It stores nothing,
 * decides nothing about observation and never reads an archive file: it starts the same
 * `projectlife-ui` server and the same `projectlife` core the macOS and Linux apps use, and every
 * word it shows comes from that server's own answer.
 *
 * The contract with the server is four calls, the same four the Linux shell makes:
 *
 *   GET  /api/menu?shell=1&plain=1   the menu as data, in a line format (see `menu::plain`) — not
 *                                    JSON, because a hand-written JSON parser could not be tested
 *                                    on the machine this program runs on
 *   GET  /api/watch                  the state: `protection.state`, its label and its reason
 *   POST /api/menu/run               run one entry by id
 *   POST /api/shutdown               leave completely: the server stops the daemon it started
 *
 * What differs from Linux, and why — this is the part of this file that is about Windows:
 *
 *   * **No console, no signals.** A GUI process has no stdout and there is no SIGTERM to send to a
 *     detached daemon, so stopping the observation is a *request file* the core reads
 *     (`projectlife daemon stop`) and `TerminateProcess` is only the last resort after that.
 *   * **The window is a WebView2 window.** The Evergreen runtime is part of Windows 10/11 (and of
 *     Edge); `WebView2Loader.dll` ships beside this executable, and when no runtime is installed the
 *     shell says so in the system's own words instead of showing an empty frame.
 *   * **Closing the window is not quitting.** The window hides, the tray icon stays, the observation
 *     keeps running; the first close says so in a tray balloon. Leaving completely is an entry in the
 *     menu, and it says what it will do before doing it.
 *   * **Autostart is a decision, not a side effect.** The tray menu offers it, it writes exactly one
 *     value in `HKCU\...\CurrentVersion\Run`, and it says what it wrote.
 *
 * `--selftest` exists because this program cannot be run on the machine it was built on: it loads the
 * real page, asks the DOM what it found, writes what happened to `<app folder>\shell-selftest.txt`
 * *and* to the console if one can be attached, and exits with 0 or 1. `docs/PLATFORMS.md` says which
 * command to run.
 */

#define UNICODE
#define _UNICODE
#define _WIN32_WINNT 0x0A00

#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <winhttp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#include "WebView2.h"
/* Who made this and what it is for. Generated from src/brand.rs by tools/brand.py; the build passes
 * -I .. so the Linux, Windows and macOS shells include the same bytes. The Windows shell prints the
 * English sentence: this file is compiled as wide strings, and the Russian one would be a guess
 * about the compiler's code-page handling on a platform this build cannot run on. */
#include "pl_brand.h"
/* L##name needs two levels: the paste must happen after the macro expands to a string literal. */
#define PL_WIDE_(s) L##s
#define PL_WIDE(s) PL_WIDE_(s)

#define SHELL_VERSION L"0.9.5"
#define SHELL_NAME L"Project Life"
#define WM_PL_TRAY (WM_APP + 1)
#define WM_PL_TICK (WM_APP + 2)
#define TRAY_ID 1

/* Menu ids: 1000+ are the server's own entries, and the shell's own six are below that. */
enum {
    ID_OPEN = 1,
    ID_TOGGLE_WATCH,
    ID_STATUS,
    ID_DIAGNOSE,
    ID_AUTOSTART,
    ID_QUIT,
    ID_SERVER_FIRST = 1000,
};

static struct {
    HINSTANCE inst;
    HWND hwnd;
    HMENU tray_menu;
    NOTIFYICONDATAW nid;
    BOOL tray_up;
    BOOL close_notice_shown;
    BOOL quitting;
    BOOL observing;
    BOOL selftest;
    BOOL selftest_done;
    BOOL webview_ready;
    int probe_tries;
    wchar_t app_home[MAX_PATH];
    wchar_t core_bin[MAX_PATH];
    wchar_t ui_bin[MAX_PATH];
    char url[512];
    char token[128];
    char *menu_plain;
    char **item_ids;         /* the id behind each menu command */
    int item_count;
    int port;
    PROCESS_INFORMATION server;
    HANDLE server_out;
    ICoreWebView2Environment *env;
    ICoreWebView2Controller *controller;
    ICoreWebView2 *webview;
    char state[64], state_label[256], state_reason[512];
    wchar_t runtime_version[128];
} g;

/* ---------------------------------------------------------------------------------------------
 * Logging: one file, the same one the server and the core append to.
 * -------------------------------------------------------------------------------------------*/

static void pl_log(const char *fmt, ...)
{
    wchar_t path[MAX_PATH * 2];
    _snwprintf(path, MAX_PATH * 2, L"%s\\daemon.log", g.app_home);
    FILE *f = _wfopen(path, L"a, ccs=UTF-8");
    if (!f) return;
    SYSTEMTIME st;
    GetLocalTime(&st);
    fwprintf(f, L"%04d-%02d-%02d %02d:%02d:%02d projectlife-shell: ", st.wYear, st.wMonth, st.wDay,
             st.wHour, st.wMinute, st.wSecond);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(f, fmt, ap);
    va_end(ap);
    fputc('\n', f);
    fclose(f);
}

/* ---------------------------------------------------------------------------------------------
 * Where things live
 * -------------------------------------------------------------------------------------------*/

static void pl_app_home(void)
{
    wchar_t *env = _wgetenv(L"PROJECTLIFE_APP_HOME");
    if (env && *env) {
        wcsncpy(g.app_home, env, MAX_PATH - 1);
        return;
    }
    wchar_t *lad = _wgetenv(L"LOCALAPPDATA");
    if (lad && *lad) {
        _snwprintf(g.app_home, MAX_PATH, L"%s\\ProjectLife", lad);
        return;
    }
    wchar_t tmp[MAX_PATH];
    GetTempPathW(MAX_PATH, tmp);
    _snwprintf(g.app_home, MAX_PATH, L"%sProjectLife", tmp);
}

static void pl_dir_beside(wchar_t *out, size_t n, const wchar_t *name)
{
    wchar_t self[MAX_PATH * 2];
    GetModuleFileNameW(NULL, self, MAX_PATH * 2);
    wchar_t *slash = wcsrchr(self, L'\\');
    if (slash) *slash = 0;
    _snwprintf(out, n, L"%s\\%s", self, name);
}

static BOOL pl_file_exists(const wchar_t *p)
{
    DWORD a = GetFileAttributesW(p);
    return a != INVALID_FILE_ATTRIBUTES && !(a & FILE_ATTRIBUTE_DIRECTORY);
}

/* ---------------------------------------------------------------------------------------------
 * HTTP, through WinHTTP. Everything is UTF-8 on the wire; the server is on 127.0.0.1 only.
 * -------------------------------------------------------------------------------------------*/

static char *pl_http(const char *method, const char *path, const char *body, size_t *out_len)
{
    wchar_t wpath[512];
    MultiByteToWideChar(CP_UTF8, 0, path, -1, wpath, 512);

    HINTERNET session = WinHttpOpen(L"ProjectLife/1.0", WINHTTP_ACCESS_TYPE_NO_PROXY,
                                    WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
    if (!session) return NULL;
    HINTERNET conn = WinHttpConnect(session, L"127.0.0.1", (INTERNET_PORT)g.port, 0);
    if (!conn) { WinHttpCloseHandle(session); return NULL; }

    wchar_t target[768];
    _snwprintf(target, 768, L"/api/%s%stoken=%S", wpath, wcschr(wpath, L'?') ? L"&" : L"?", g.token);
    HINTERNET req = WinHttpOpenRequest(conn, strcmp(method, "POST") == 0 ? L"POST" : L"GET", target,
                                       NULL, WINHTTP_NO_REFERER, WINHTTP_DEFAULT_ACCEPT_TYPES, 0);
    if (!req) { WinHttpCloseHandle(conn); WinHttpCloseHandle(session); return NULL; }

    BOOL sent;
    if (body) {
        WinHttpAddRequestHeaders(req, L"Content-Type: application/json", -1L, WINHTTP_ADDREQ_FLAG_ADD);
        sent = WinHttpSendRequest(req, WINHTTP_NO_ADDITIONAL_HEADERS, 0, (LPVOID)body,
                                  (DWORD)strlen(body), (DWORD)strlen(body), 0);
    } else {
        sent = WinHttpSendRequest(req, WINHTTP_NO_ADDITIONAL_HEADERS, 0, WINHTTP_NO_REQUEST_DATA, 0, 0, 0);
    }
    char *out = NULL;
    if (sent && WinHttpReceiveResponse(req, NULL)) {
        size_t cap = 4096, len = 0;
        out = malloc(cap);
        if (out) {
            out[0] = 0;
            DWORD avail = 0, got = 0;
            while (WinHttpQueryDataAvailable(req, &avail) && avail > 0) {
                if (len + avail + 1 > cap) {
                    cap = (len + avail + 1) * 2;
                    char *bigger = realloc(out, cap);
                    if (!bigger) break;
                    out = bigger;
                }
                if (!WinHttpReadData(req, out + len, avail, &got) || got == 0) break;
                len += got;
                out[len] = 0;
            }
        }
    } else {
        pl_log("the server did not answer %s /api/%s (error %lu)", method, path, GetLastError());
    }
    WinHttpCloseHandle(req);
    WinHttpCloseHandle(conn);
    WinHttpCloseHandle(session);
    if (out_len) *out_len = out ? strlen(out) : 0;
    return out;
}

/* A field of a flat JSON object, as UTF-8. Not a parser: a reader for the handful of answers this
 * shell uses, and it copies raw bytes between quotes so UTF-8 (labels in Russian, for instance) goes
 * through unchanged. Everything missing or malformed answers NULL, and the caller says so. */
static char *jfield(const char *json, const char *key)
{
    if (!json) return NULL;
    char pat[128];
    _snprintf(pat, sizeof pat, "\"%s\"", key);
    const char *p = strstr(json, pat);
    if (!p) return NULL;
    p += strlen(pat);
    while (*p == ' ' || *p == ':' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (*p != '"') return NULL;
    p++;
    const char *end = p;
    while (*end && *end != '"') {
        if (*end == '\\') end++;
        end++;
    }
    size_t n = (size_t)(end - p);
    char *out = malloc(n + 1);
    if (!out) return NULL;
    memcpy(out, p, n);
    out[n] = 0;
    return out;
}

static void utf8_to_wide(const char *src, wchar_t *dst, int cap)
{
    if (!src) { dst[0] = 0; return; }
    MultiByteToWideChar(CP_UTF8, 0, src, -1, dst, cap);
}

/* ---------------------------------------------------------------------------------------------
 * The state, as the server reports it
 * -------------------------------------------------------------------------------------------*/

static void pl_refresh_state(void)
{
    char *w = pl_http("GET", "watch", NULL, NULL);
    if (!w) {
        strcpy(g.state, "unknown");
        strcpy(g.state_label, "Unavailable");
        strcpy(g.state_reason, "The interface server is not answering.");
        return;
    }
    /* The protection block, and nothing else in the document: two keys called "state" anywhere else
     * must not be able to answer for this one. */
    char *prot = strstr(w, "\"protection\"");
    const char *scope = prot ? prot : w;
    char *state = jfield(scope, "state");
    char *label = jfield(scope, "label");
    char *reason = jfield(scope, "reason");
    snprintf(g.state, sizeof g.state, "%s", state ? state : "unknown");
    snprintf(g.state_label, sizeof g.state_label, "%s", label ? label : "Unknown");
    snprintf(g.state_reason, sizeof g.state_reason, "%s", reason ? reason : "");
    free(state);
    free(label);
    free(reason);
    /* "runningByApp" is a JSON boolean; the same reader that handles strings handles `true` by
     * copying the four letters between the colon and the comma. */
    char *running = jfield(w, "runningByApp");
    if (!running) {
        /* jfield only copies quoted values, so a boolean is read here, in the open. */
        const char *p = strstr(w, "\"runningByApp\"");
        if (p) {
            p = strchr(p, ':');
            if (p) {
                p++;
                while (*p == ' ') p++;
                running = _strdup(strncmp(p, "true", 4) == 0 ? "true" : "false");
            }
        }
    }
    g.observing = running && strcmp(running, "true") == 0;
    free(running);
    free(w);
}

static void pl_set_tray_tip(void)
{
    wchar_t label[256], tip[512];
    utf8_to_wide(g.state_label, label, 256);
    _snwprintf(tip, 512, L"%s — %s", SHELL_NAME, label[0] ? label : L"…");
    wcsncpy(g.nid.szTip, tip, 127);
    g.nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    if (g.tray_up) Shell_NotifyIconW(NIM_MODIFY, &g.nid);
}

static void pl_balloon(const wchar_t *title, const wchar_t *text)
{
    if (!g.tray_up) return;
    NOTIFYICONDATAW n = g.nid;
    n.uFlags = NIF_INFO;
    n.dwInfoFlags = NIIF_INFO;
    wcsncpy(n.szInfoTitle, title, 63);
    wcsncpy(n.szInfo, text, 255);
    Shell_NotifyIconW(NIM_MODIFY, &n);
}

/* ---------------------------------------------------------------------------------------------
 * Menus: one document from the server, on two fronts
 * -------------------------------------------------------------------------------------------*/

static void pl_item_cmd(int cmd, const char *id)
{
    if (cmd < ID_SERVER_FIRST) return;
    if (cmd - ID_SERVER_FIRST >= g.item_count) return;
    if (g.item_ids[cmd - ID_SERVER_FIRST]) return;
    g.item_ids[cmd - ID_SERVER_FIRST] = _strdup(id);
}

static void pl_build_server_menu(HMENU parent)
{
    if (!g.menu_plain) {
        AppendMenuW(parent, MF_STRING | MF_GRAYED, 0,
                    L"The menu could not be read from the app server");
        return;
    }
    HMENU group = NULL;
    char *copy = _strdup(g.menu_plain);
    char *line = strtok(copy, "\n");
    while (line) {
        if (line[0] == 'G' && line[1] == '\t') {
            char *id = line + 2;
            char *title = strchr(id, '\t');
            if (title) {
                *title++ = 0;
                wchar_t wtitle[256];
                utf8_to_wide(title, wtitle, 256);
                group = CreatePopupMenu();
                AppendMenuW(parent, MF_POPUP | MF_STRING, (UINT_PTR)group, wtitle);
            }
        } else if (line[0] == 'I' && line[1] == '\t' && group) {
            /* I<TAB>group<TAB>id<TAB>enabled<TAB>kind<TAB>input<TAB>label */
            char *f[7];
            int n = 0;
            char *p = line + 2;
            while (n < 7) {
                f[n++] = p;
                char *tab = strchr(p, '\t');
                if (!tab) break;
                *tab = 0;
                p = tab + 1;
            }
            if (n < 7) { line = strtok(NULL, "\n"); continue; }
            int enabled = f[3][0] == '1';
            int cmd = ID_SERVER_FIRST + g.item_count;
            wchar_t label[256];
            utf8_to_wide(f[6], label, 256);
            AppendMenuW(group, MF_STRING | (enabled ? 0 : MF_GRAYED), cmd, label);
            if (g.item_count < 512) {
                g.item_ids[g.item_count] = NULL;
                pl_item_cmd(cmd, f[2]);
                g.item_count++;
            }
        }
        line = strtok(NULL, "\n");
    }
    free(copy);
}

static void pl_build_tray_menu(void)
{
    if (!g.tray_menu) return;
    while (GetMenuItemCount(g.tray_menu) > 0) {
        DeleteMenu(g.tray_menu, 0, MF_BYPOSITION);
    }
    wchar_t label[256];
    utf8_to_wide(g.state_label, label, 256);
    AppendMenuW(g.tray_menu, MF_STRING | MF_GRAYED, 0, label[0] ? label : L"…");
    AppendMenuW(g.tray_menu, MF_SEPARATOR, 0, NULL);
    AppendMenuW(g.tray_menu, MF_STRING, ID_OPEN, L"Open window");
    AppendMenuW(g.tray_menu, MF_STRING, ID_TOGGLE_WATCH,
                g.observing ? L"Stop observation" : L"Start observation");
    AppendMenuW(g.tray_menu, MF_STRING, ID_STATUS, L"Protection status…");
    AppendMenuW(g.tray_menu, MF_STRING, ID_DIAGNOSE, L"Network diagnosis…");
    AppendMenuW(g.tray_menu, MF_STRING, ID_AUTOSTART, L"Start with Windows (toggle)");
    AppendMenuW(g.tray_menu, MF_SEPARATOR, 0, NULL);
    pl_build_server_menu(g.tray_menu);
    AppendMenuW(g.tray_menu, MF_SEPARATOR, 0, NULL);
    AppendMenuW(g.tray_menu, MF_STRING, ID_QUIT, L"Quit completely…");
}

/* ---------------------------------------------------------------------------------------------
 * What the shell does when a person asks
 * -------------------------------------------------------------------------------------------*/

static void pl_show_window(void)
{
    ShowWindow(g.hwnd, SW_SHOW);
    SetForegroundWindow(g.hwnd);
}

static void pl_post(const char *path, const char *body)
{
    char *answer = pl_http("POST", path, body ? body : "{}", NULL);
    if (answer) {
        pl_log("%s -> %s", path, answer);
        free(answer);
    }
}

static void pl_toggle_watch(void)
{
    pl_post(g.observing ? "watch/stop" : "watch/start", "{}");
    pl_refresh_state();
    pl_build_tray_menu();
    pl_set_tray_tip();
    SetTimer(g.hwnd, 1, 900, NULL);   /* ask again shortly: the daemon needs a moment */
}

static void pl_status_dialog(void)
{
    wchar_t label[256], reason[512], line[1200];
    utf8_to_wide(g.state_label, label, 256);
    utf8_to_wide(g.state_reason, reason, 512);
    _snwprintf(line, 1200, L"%s\n\nstate: %S\n\n%s", label, g.state, reason);
    MessageBoxW(g.hwnd, line, L"Protection status", MB_OK | MB_ICONINFORMATION);
}

static void pl_diagnose(void)
{
    /* The server's own diagnosis, in the system's own words, plus the file it was written to. */
    wchar_t cmd[MAX_PATH * 3], file[MAX_PATH * 2];
    _snwprintf(cmd, MAX_PATH * 3, L"\"%s\" --diagnose --log-dir \"%s\"", g.ui_bin, g.app_home);
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;
    memset(&si, 0, sizeof si);
    memset(&pi, 0, sizeof pi);
    si.cb = sizeof si;
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;
    if (CreateProcessW(NULL, cmd, NULL, NULL, FALSE, CREATE_NO_WINDOW, NULL, NULL, &si, &pi)) {
        WaitForSingleObject(pi.hProcess, 30000);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    _snwprintf(file, MAX_PATH * 2, L"%s\\diagnose.txt", g.app_home);
    if (pl_file_exists(file)) {
        /* Notepad is present on every Windows and opens a text file; nothing is executed from it. */
        ShellExecuteW(NULL, L"open", file, NULL, NULL, SW_SHOWNORMAL);
    } else {
        MessageBoxW(g.hwnd, L"The diagnosis did not write a file; see daemon.log.", L"Network diagnosis",
                    MB_OK | MB_ICONWARNING);
    }
}

static BOOL pl_autostart_enabled(void)
{
    HKEY key;
    if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Run", 0,
                      KEY_READ, &key) != ERROR_SUCCESS) {
        return FALSE;
    }
    wchar_t value[MAX_PATH * 2];
    DWORD size = sizeof value;
    DWORD type = 0;
    BOOL found = RegQueryValueExW(key, L"ProjectLife", NULL, &type, (LPBYTE)value, &size) == ERROR_SUCCESS;
    RegCloseKey(key);
    return found;
}

static void pl_toggle_autostart(void)
{
    HKEY key;
    if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Run", 0,
                      KEY_SET_VALUE, &key) != ERROR_SUCCESS) {
        MessageBoxW(g.hwnd, L"The registry refused the change.", L"Start with Windows",
                    MB_OK | MB_ICONWARNING);
        return;
    }
    if (pl_autostart_enabled()) {
        RegDeleteValueW(key, L"ProjectLife");
        pl_log("autostart removed (HKCU\\...\\Run\\ProjectLife)");
        MessageBoxW(g.hwnd,
                    L"Project Life will no longer start with Windows.\n\n"
                    L"Removed: HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\ProjectLife",
                    L"Start with Windows", MB_OK | MB_ICONINFORMATION);
    } else {
        wchar_t self[MAX_PATH * 2], quoted[MAX_PATH * 4];
        GetModuleFileNameW(NULL, self, MAX_PATH * 2);
        _snwprintf(quoted, MAX_PATH * 4, L"\"%s\" --autostart", self);
        if (RegSetValueExW(key, L"ProjectLife", 0, REG_SZ, (const BYTE *)quoted,
                           (DWORD)((wcslen(quoted) + 1) * sizeof(wchar_t))) == ERROR_SUCCESS) {
            pl_log("autostart written: %ls", quoted);
            MessageBoxW(g.hwnd,
                        L"Project Life will start with Windows.\n\n"
                        L"Written: HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\ProjectLife",
                        L"Start with Windows", MB_OK | MB_ICONINFORMATION);
        }
    }
    RegCloseKey(key);
}

static void pl_quit_completely(BOOL ask)
{
    if (ask) {
        int r = MessageBoxW(g.hwnd,
                            L"Quit Project Life completely?\n\n"
                            L"This stops the observation this app started and closes the window.\n\n"
                            L"Nothing in the archive is deleted: every version already stored stays where it "
                            L"is, and your project folders are not touched. Observation stops until you start "
                            L"the app again.",
                            L"Quit Project Life", MB_YESNO | MB_ICONQUESTION);
        if (r != IDYES) return;
    }
    g.quitting = TRUE;
    pl_log("quitting: asking the server to stop the observation it started");
    pl_post("shutdown", "{}");
    if (g.server.hProcess) {
        WaitForSingleObject(g.server.hProcess, 15000);
        DWORD code = 0;
        if (GetExitCodeProcess(g.server.hProcess, &code) && code == STILL_ACTIVE) {
            pl_log("the interface server did not leave within 15 s; terminating it");
            TerminateProcess(g.server.hProcess, 0);
        }
    }
    if (g.tray_up) {
        Shell_NotifyIconW(NIM_DELETE, &g.nid);
        g.tray_up = FALSE;
    }
    DestroyWindow(g.hwnd);
    PostQuitMessage(0);
}

static void pl_run_server_item(int cmd)
{
    int idx = cmd - ID_SERVER_FIRST;
    if (idx < 0 || idx >= g.item_count) return;
    char *id = g.item_ids[idx];
    if (!id) return;
    char body[512];
    /* Entries that ask a question first are refused here in words, rather than failing in silence:
     * the window draws the dialog for those, and this menu does not pretend to. */
    char *answer = pl_http("GET", "menu?shell=1", NULL, NULL);
    int needs_input = answer ? (strstr(answer, id) && strstr(answer, "\"input\": \"text\"")) : 0;
    free(answer);
    if (needs_input) {
        wchar_t wid[128];
        utf8_to_wide(id, wid, 128);
        MessageBoxW(g.hwnd,
                    L"This entry needs a value first, and the window is where that is asked for.\n"
                    L"Open the window and run it from there.",
                    L"Project Life", MB_OK | MB_ICONINFORMATION);
        (void)wid;
        return;
    }
    _snprintf(body, sizeof body, "{\"id\":\"%s\"}", id);
    pl_post("menu/run", body);
    pl_refresh_state();
    pl_build_tray_menu();
}

/* ---------------------------------------------------------------------------------------------
 * Starting the server: the same binary the other two platforms start, with the same arguments.
 * -------------------------------------------------------------------------------------------*/

static BOOL pl_start_server(void)
{
    SECURITY_ATTRIBUTES sa;
    memset(&sa, 0, sizeof sa);
    sa.nLength = sizeof sa;
    sa.bInheritHandle = TRUE;
    HANDLE rd = NULL, wr = NULL;
    if (!CreatePipe(&rd, &wr, &sa, 0)) return FALSE;
    SetHandleInformation(rd, HANDLE_FLAG_INHERIT, 0);

    wchar_t cmd[MAX_PATH * 8];
    _snwprintf(cmd, MAX_PATH * 8,
               L"\"%s\" --pl \"%s\" --port 7717 --port-range 10 --native --log-dir \"%s\"",
               g.ui_bin, g.core_bin, g.app_home);

    STARTUPINFOW si;
    PROCESS_INFORMATION pi;
    memset(&si, 0, sizeof si);
    memset(&pi, 0, sizeof pi);
    si.cb = sizeof si;
    si.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
    si.wShowWindow = SW_HIDE;
    si.hStdOutput = wr;
    si.hStdError = wr;
    si.hStdInput = NULL;

    if (!CreateProcessW(NULL, cmd, NULL, NULL, TRUE, CREATE_NO_WINDOW, NULL, NULL, &si, &pi)) {
        pl_log("cannot start %ls (error %lu)", g.ui_bin, GetLastError());
        CloseHandle(rd);
        CloseHandle(wr);
        return FALSE;
    }
    CloseHandle(wr);
    g.server = pi;
    g.server_out = rd;

    /* The first line the server prints is the whole handshake. It is read with a deadline, because a
     * program that never answers must not hang a window that could say why. */
    char line[8192];
    size_t len = 0;
    DWORD waited = 0;
    while (waited < 30000 && len < sizeof line - 2) {
        DWORD avail = 0;
        if (!PeekNamedPipe(rd, NULL, 0, NULL, &avail, NULL)) break;
        if (avail == 0) {
            Sleep(100);
            waited += 100;
            continue;
        }
        DWORD got = 0;
        if (!ReadFile(rd, line + len, (DWORD)(sizeof line - 2 - len), &got, NULL) || got == 0) break;
        len += got;
        line[len] = 0;
        if (strchr(line, '\n')) break;
    }
    if (!len) {
        pl_log("the interface server said nothing within 30 s");
        return FALSE;
    }
    pl_log("server said: %s", line);

    char *ready = jfield(line, "ready");
    BOOL is_ready = ready && strcmp(ready, "true") == 0;
    free(ready);
    if (!is_ready) {
        char *reason = jfield(line, "reason");
        char *advice = jfield(line, "advice");
        wchar_t wreason[512], wadvice[512], wlog[MAX_PATH * 2];
        utf8_to_wide(reason, wreason, 512);
        utf8_to_wide(advice, wadvice, 512);
        _snwprintf(wlog, MAX_PATH * 2, L"%s\\daemon.log", g.app_home);
        wchar_t msg[1400];
        _snwprintf(msg, 1400,
                   L"Project Life could not open its local port.\n\n%ls\n\n%ls\n\nThe full reason is in "
                   L"%ls,\nand \"Network diagnosis\" in the tray menu answers the rest.",
                   wreason, wadvice, wlog);
        pl_log("no socket: %s (%s)", reason ? reason : "?", advice ? advice : "?");
        MessageBoxW(NULL, msg, L"The interface server could not start", MB_OK | MB_ICONERROR);
        free(reason);
        free(advice);
        return FALSE;
    }
    char *url = jfield(line, "url");
    char *token = jfield(line, "token");
    char *port = jfield(line, "port");
    snprintf(g.url, sizeof g.url, "%s", url ? url : "http://127.0.0.1:7717/");
    snprintf(g.token, sizeof g.token, "%s", token ? token : "");
    g.port = port ? atoi(port) : 7717;
    free(url);
    free(token);
    free(port);
    pl_log("the interface server is ready on port %d", g.port);
    return TRUE;
}

/* ---------------------------------------------------------------------------------------------
 * WebView2
 * -------------------------------------------------------------------------------------------*/

/* ---------------------------------------------------------------------------------------------
 * WebView2
 *
 * Three callbacks, written out because C has no anonymous classes: creating the environment,
 * creating the controller (the thing that owns the web view), and reading the answer to the DOM
 * probe. Each is the plain COM object the SDK's header describes — QueryInterface, AddRef, Release
 * and the one Invoke that matters — and each is handed to WebView2 for exactly one call.
 * -------------------------------------------------------------------------------------------*/

typedef struct {
    ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandlerVtbl *lpVtbl;
    LONG ref;
} EnvHandler;

typedef struct {
    ICoreWebView2CreateCoreWebView2ControllerCompletedHandlerVtbl *lpVtbl;
    LONG ref;
} CtlHandler;

typedef struct {
    ICoreWebView2ExecuteScriptCompletedHandlerVtbl *lpVtbl;
    LONG ref;
} ScriptHandler;

static void pl_probe(void);
static void pl_selftest_report(const wchar_t *probe_json);

/* ---- the controller callback (defined first: the environment callback creates it) ---- */

static HRESULT STDMETHODCALLTYPE ctl_QueryInterface(
    ICoreWebView2CreateCoreWebView2ControllerCompletedHandler *This, REFIID riid, void **ppv)
{
    (void)riid;
    *ppv = This;
    return S_OK;
}
static ULONG STDMETHODCALLTYPE ctl_AddRef(
    ICoreWebView2CreateCoreWebView2ControllerCompletedHandler *This)
{
    return (ULONG)InterlockedIncrement(&((CtlHandler *)This)->ref);
}
static ULONG STDMETHODCALLTYPE ctl_Release(
    ICoreWebView2CreateCoreWebView2ControllerCompletedHandler *This)
{
    return (ULONG)InterlockedDecrement(&((CtlHandler *)This)->ref);
}
static HRESULT STDMETHODCALLTYPE ctl_Invoke(
    ICoreWebView2CreateCoreWebView2ControllerCompletedHandler *This, HRESULT result,
    ICoreWebView2Controller *controller)
{
    (void)This;
    if (FAILED(result) || !controller) {
        pl_log("the WebView2 controller could not be created (hr=0x%08lx)", (unsigned long)result);
        return result;
    }
    g.controller = controller;
    controller->lpVtbl->AddRef(controller);
    RECT rc;
    GetClientRect(g.hwnd, &rc);
    controller->lpVtbl->put_Bounds(controller, rc);
    controller->lpVtbl->put_IsVisible(controller, TRUE);
    if (SUCCEEDED(controller->lpVtbl->get_CoreWebView2(controller, &g.webview)) && g.webview) {
        g.webview->lpVtbl->AddRef(g.webview);
        wchar_t wurl[512];
        utf8_to_wide(g.url, wurl, 512);
        g.webview->lpVtbl->Navigate(g.webview, wurl);
        g.webview_ready = TRUE;
        pl_log("window navigated to %s", g.url);
    }
    return S_OK;
}

static ICoreWebView2CreateCoreWebView2ControllerCompletedHandlerVtbl ctl_vtbl = {
    ctl_QueryInterface, ctl_AddRef, ctl_Release, ctl_Invoke
};

/* ---- the environment callback ---- */

static HRESULT STDMETHODCALLTYPE env_QueryInterface(
    ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler *This, REFIID riid, void **ppv)
{
    (void)riid;
    *ppv = This;
    return S_OK;
}
static ULONG STDMETHODCALLTYPE env_AddRef(
    ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler *This)
{
    return (ULONG)InterlockedIncrement(&((EnvHandler *)This)->ref);
}
static ULONG STDMETHODCALLTYPE env_Release(
    ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler *This)
{
    return (ULONG)InterlockedDecrement(&((EnvHandler *)This)->ref);
}
static HRESULT STDMETHODCALLTYPE env_Invoke(
    ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler *This, HRESULT result,
    ICoreWebView2Environment *env)
{
    (void)This;
    if (FAILED(result) || !env) {
        pl_log("WebView2 could not be created (hr=0x%08lx). The Evergreen runtime is part of Windows "
               "10/11 and of Microsoft Edge; if it is missing, install it from Microsoft: "
               "https://developer.microsoft.com/microsoft-edge/webview2/", (unsigned long)result);
        wchar_t msg[700];
        _snwprintf(msg, 700,
                   L"The WebView2 runtime is not available on this machine (hr=0x%08lx).\n\n"
                   L"Project Life shows its interface in a WebView2 window, and that runtime is part of "
                   L"Windows 10/11 and Microsoft Edge. Install the Evergreen runtime from Microsoft and "
                   L"start Project Life again. Nothing else in this program needs it.",
                   (unsigned long)result);
        MessageBoxW(NULL, msg, L"Project Life cannot show its window", MB_OK | MB_ICONERROR);
        g.selftest_done = TRUE;
        if (g.selftest) pl_quit_completely(FALSE);
        return result;
    }
    g.env = env;
    env->lpVtbl->AddRef(env);
    LPWSTR version = NULL;
    if (SUCCEEDED(env->lpVtbl->get_BrowserVersionString(env, &version)) && version) {
        wcsncpy(g.runtime_version, version, 127);
        CoTaskMemFree(version);
    }
    CtlHandler *ctl = calloc(1, sizeof(CtlHandler));
    ctl->lpVtbl = &ctl_vtbl;
    ctl->ref = 1;
    env->lpVtbl->CreateCoreWebView2Controller(
        env, g.hwnd, (ICoreWebView2CreateCoreWebView2ControllerCompletedHandler *)ctl);
    return S_OK;
}

static ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandlerVtbl env_vtbl = {
    env_QueryInterface, env_AddRef, env_Release, env_Invoke
};

/* ---- the script callback ---- */

static HRESULT STDMETHODCALLTYPE script_QueryInterface(
    ICoreWebView2ExecuteScriptCompletedHandler *This, REFIID riid, void **ppv)
{
    (void)riid;
    *ppv = This;
    return S_OK;
}
static ULONG STDMETHODCALLTYPE script_AddRef(ICoreWebView2ExecuteScriptCompletedHandler *This)
{
    return (ULONG)InterlockedIncrement(&((ScriptHandler *)This)->ref);
}
static ULONG STDMETHODCALLTYPE script_Release(ICoreWebView2ExecuteScriptCompletedHandler *This)
{
    return (ULONG)InterlockedDecrement(&((ScriptHandler *)This)->ref);
}
static HRESULT STDMETHODCALLTYPE script_Invoke(ICoreWebView2ExecuteScriptCompletedHandler *This,
                                               HRESULT result, LPCWSTR json)
{
    (void)This;
    if (FAILED(result)) {
        pl_log("the DOM probe failed (hr=0x%08lx)", (unsigned long)result);
        return result;
    }
    pl_selftest_report(json);
    return S_OK;
}

static ICoreWebView2ExecuteScriptCompletedHandlerVtbl script_vtbl = {
    script_QueryInterface, script_AddRef, script_Release, script_Invoke
};

/* The probe asks the page, in the page's own words, what it found. This is not the shell guessing:
 * it is the shipped page reporting on itself — the same probe the Linux shell runs. */
static const wchar_t *PL_PROBE_JS =
    L"(function(){"
    L"  var t = document.body ? document.body.innerText : '';"
    L"  var m = t.match(/build app [^\n]*/);"
    L"  return JSON.stringify({"
    L"    title: document.title,"
    L"    sidebar: !!document.querySelector('.sidebar'),"
    L"    menuApi: typeof window.plMenu === 'function',"
    L"    textLen: t.length,"
    L"    build: m ? m[0] : ''"
    L"  });"
    L"})()";

static void pl_probe(void)
{
    if (!g.webview) return;
    ScriptHandler *h = calloc(1, sizeof(ScriptHandler));
    h->lpVtbl = &script_vtbl;
    h->ref = 1;
    g.webview->lpVtbl->ExecuteScript(g.webview, PL_PROBE_JS,
                                     (ICoreWebView2ExecuteScriptCompletedHandler *)h);
}

/* ---------------------------------------------------------------------------------------------
 * The window procedure
 * -------------------------------------------------------------------------------------------*/

static void pl_window_class(void);

/// The same control channel the Linux shell has: whoever started this window (a launcher, the check
/// that runs it, a session ending) can ask it to come forward or to leave, by writing one file.
static void pl_tick(void)
{
    wchar_t p[MAX_PATH * 2];
    _snwprintf(p, MAX_PATH * 2, L"%s\\show.request", g.app_home);
    if (pl_file_exists(p)) {
        DeleteFileW(p);
        pl_show_window();
    }
    _snwprintf(p, MAX_PATH * 2, L"%s\\quit.request", g.app_home);
    if (pl_file_exists(p)) {
        DeleteFileW(p);
        pl_log("quit requested through the app folder; leaving the way the menu does "
               "(no confirmation dialog: whoever asked is not here to answer one)");
        pl_quit_completely(FALSE);
        return;
    }
    pl_refresh_state();
    pl_build_tray_menu();
    pl_set_tray_tip();
}

static LRESULT CALLBACK pl_wndproc(HWND h, UINT msg, WPARAM wp, LPARAM lp)
{
    switch (msg) {
    case WM_SIZE:
        if (g.controller) {
            RECT rc;
            GetClientRect(h, &rc);
            g.controller->lpVtbl->put_Bounds(g.controller, rc);
        }
        return 0;
    case WM_PL_TRAY:
        if (LOWORD(lp) == WM_RBUTTONUP || LOWORD(lp) == WM_CONTEXTMENU) {
            POINT pt;
            GetCursorPos(&pt);
            SetForegroundWindow(h);
            pl_refresh_state();
            pl_build_tray_menu();
            TrackPopupMenu(g.tray_menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, h, NULL);
            return 0;
        }
        if (LOWORD(lp) == WM_LBUTTONDBLCLK) {
            pl_show_window();
            return 0;
        }
        return 0;
    case WM_TIMER:
        if (wp == 1) {
            KillTimer(h, 1);
            pl_refresh_state();
            pl_build_tray_menu();
            pl_set_tray_tip();
            return 0;
        }
        if (wp == 3) {
            pl_tick();
            return 0;
        }
        if (wp == 2 && g.selftest && !g.selftest_done) {
            g.probe_tries++;
            if (g.probe_tries > 12) {
                g.selftest_done = TRUE;
                pl_log("selftest: the page never rendered its sidebar");
                KillTimer(h, 2);
                if (g.selftest) pl_quit_completely(FALSE);
                return 0;
            }
            pl_probe();
            return 0;
        }
        return 0;
    case WM_COMMAND: {
        int cmd = LOWORD(wp);
        switch (cmd) {
        case ID_OPEN: pl_show_window(); return 0;
        case ID_TOGGLE_WATCH: pl_toggle_watch(); return 0;
        case ID_STATUS: pl_refresh_state(); pl_status_dialog(); return 0;
        case ID_DIAGNOSE: pl_diagnose(); return 0;
        case ID_AUTOSTART: pl_toggle_autostart(); return 0;
        case ID_QUIT: pl_quit_completely(TRUE); return 0;
        default: pl_run_server_item(cmd); return 0;
        }
    }
    case WM_CLOSE:
        /* Closing the window hides it. The observation is a separate process and does not notice. */
        ShowWindow(h, SW_HIDE);
        pl_log("the window was closed: it is hidden, the observation was not stopped (tray=%s)",
               g.tray_up ? "yes" : "no");
        if (g.selftest) return 0;
        if (!g.close_notice_shown) {
            g.close_notice_shown = TRUE;
            if (g.tray_up) {
                pl_balloon(L"Project Life is still protecting this project",
                           L"The icon beside the clock brings the window back. Its menu stops the "
                           L"observation or quits completely.");
            } else {
                MessageBoxW(h,
                            L"Project Life is still protecting this project, but this session did not "
                            L"give it a tray icon.\n\nStart Project Life again to bring the window back.",
                            L"Project Life", MB_OK | MB_ICONINFORMATION);
            }
        }
        return 0;
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(h, msg, wp, lp);
}

/* ---------------------------------------------------------------------------------------------
 * The address of the app: the same rule as the core and the Linux shell, so one machine has one
 * folder. (Written out here because a Windows shell has no other way to reach the core's rule.)
 * -------------------------------------------------------------------------------------------*/

static void pl_ensure_dir(const wchar_t *path)
{
    wchar_t tmp[MAX_PATH * 2];
    wcsncpy(tmp, path, MAX_PATH * 2 - 1);
    for (wchar_t *p = tmp + 3; *p; p++) {
        if (*p == L'\\') {
            *p = 0;
            CreateDirectoryW(tmp, NULL);
            *p = L'\\';
        }
    }
    CreateDirectoryW(tmp, NULL);
}

/* ---------------------------------------------------------------------------------------------
 * Selftest: the only way this program can be checked on a machine it was not built on.
 * -------------------------------------------------------------------------------------------*/

static FILE *g_selftest_file;

static void pl_say(const wchar_t *fmt, ...)
{
    wchar_t line[1024];
    va_list ap;
    va_start(ap, fmt);
    _vsnwprintf(line, 1024, fmt, ap);
    va_end(ap);
    if (g_selftest_file) {
        fwprintf(g_selftest_file, L"%ls\n", line);
        fflush(g_selftest_file);
    }
    fwprintf(stdout, L"%ls\n", line);
    fflush(stdout);
}

static void pl_selftest_report(const wchar_t *probe_json)
{
    if (!g.selftest || g.selftest_done) return;
    char utf8[2048];
    WideCharToMultiByte(CP_UTF8, 0, probe_json ? probe_json : L"", -1, utf8, 2048, NULL, NULL);
    char *sidebar = jfield(utf8, "sidebar");
    char *menu_api = jfield(utf8, "menuApi");
    char *build = jfield(utf8, "build");
    const char *side_ok = sidebar && strcmp(sidebar, "true") == 0 ? "yes" : "no";
    const char *menu_ok = menu_api && strcmp(menu_api, "true") == 0 ? "yes" : "no";
    if (strcmp(side_ok, "no") == 0 && g.probe_tries < 12) {
        free(sidebar);
        free(menu_api);
        free(build);
        return;
    }
    g.selftest_done = TRUE;
    KillTimer(g.hwnd, 2);
    pl_refresh_state();
    int groups = 0;
    if (g.menu_plain) {
        for (const char *p = g.menu_plain; *p; p++) {
            if (p[0] == 'G' && p[1] == '\t') groups++;
        }
    }
    BOOL ok = strcmp(side_ok, "yes") == 0 && strcmp(menu_ok, "yes") == 0 && groups > 0;
    pl_say(L"selftest: shell-version=%ls", SHELL_VERSION);
    pl_say(L"selftest: webview2-runtime=\"%ls\"", g.runtime_version);
    pl_say(L"selftest: page sidebar=%S menuApi=%S build=\"%S\"", side_ok, menu_ok, build ? build : "");
    pl_say(L"selftest: server-port=%d protection=\"%S\" state=\"%S\"", g.port, g.state_label, g.state);
    pl_say(L"selftest: menu-groups=%d menu-items=%d tray=%s", groups, g.item_count,
           g.tray_up ? L"yes" : L"no");
    pl_say(L"selftest: RESULT=%ls", ok ? L"PASS" : L"FAIL page");
    pl_log("selftest finished: %s", ok ? "PASS" : "FAIL page");
    free(sidebar);
    free(menu_api);
    free(build);
    if (g.selftest) pl_quit_completely(FALSE);
}

/* ---------------------------------------------------------------------------------------------
 * main
 * -------------------------------------------------------------------------------------------*/

int WINAPI wWinMain(HINSTANCE inst, HINSTANCE prev, LPWSTR cmdline, int show)
{
    (void)prev;
    (void)cmdline;
    (void)show;
    g.inst = inst;

    int argc = 0;
    LPWSTR *argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    BOOL autostart = FALSE;
    for (int i = 1; i < argc; i++) {
        if (!wcscmp(argv[i], L"--selftest")) g.selftest = TRUE;
        else if (!wcscmp(argv[i], L"--version")) {
            /* No console in a GUI process: try to borrow the parent's, then write the answer to the
             * folder as well, so the version is readable without a terminal. */
            if (AttachConsole(ATTACH_PARENT_PROCESS)) {
                freopen("CONOUT$", "w", stdout);
            }
            wprintf(L"Project Life shell %ls (Windows, WebView2)\n", SHELL_VERSION);
            wprintf(L"%ls\n", PL_WIDE(PL_WHAT_IT_IS));
            wprintf(L"By %ls <%ls> \xc2\xb7 %ls licence\n", PL_WIDE(PL_AUTHOR), PL_WIDE(PL_AUTHOR_EMAIL),
                    PL_WIDE(PL_LICENCE));
            wprintf(L"%ls\n", PL_WIDE(PL_COPYRIGHT));
            return 0;
        } else if (!wcscmp(argv[i], L"--help")) {
            if (AttachConsole(ATTACH_PARENT_PROCESS)) freopen("CONOUT$", "w", stdout);
            wprintf(L"Project Life %ls (Windows shell)\n\n"
                    L"  ProjectLife.exe [--selftest] [--autostart] [--version]\n\n"
                    L"The window talks to projectlife-ui, which drives projectlife: this program stores "
                    L"nothing itself.\n\n"
                    L"%ls\nBy %ls <%ls>\n",
                    SHELL_VERSION, PL_WIDE(PL_WHAT_IT_IS), PL_WIDE(PL_AUTHOR),
                    PL_WIDE(PL_AUTHOR_EMAIL));
            return 0;
        } else if (!wcscmp(argv[i], L"--autostart")) autostart = TRUE;
    }
    LocalFree(argv);

    pl_app_home();
    pl_ensure_dir(g.app_home);
    /* The names carry a `pl-` prefix on purpose: Windows is case-insensitive, so a folder cannot
     * hold `ProjectLife.exe` and `projectlife.exe` at once — one would overwrite the other, silently.
     * `pl` is also what the project's own menu calls the core command, so the name matches the
     * documentation people already read. */
    pl_dir_beside(g.ui_bin, MAX_PATH, L"pl-ui.exe");
    pl_dir_beside(g.core_bin, MAX_PATH, L"pl.exe");
    if (!pl_file_exists(g.ui_bin)) wcsncpy(g.ui_bin, L"pl-ui.exe", MAX_PATH);
    if (!pl_file_exists(g.core_bin)) wcsncpy(g.core_bin, L"pl.exe", MAX_PATH);

    if (g.selftest) {
        wchar_t p[MAX_PATH * 2];
        _snwprintf(p, MAX_PATH * 2, L"%s\\shell-selftest.txt", g.app_home);
        g_selftest_file = _wfopen(p, L"w, ccs=UTF-8");
        if (AttachConsole(ATTACH_PARENT_PROCESS)) {
            freopen("CONOUT$", "w", stdout);
        }
    }
    if (autostart) pl_log("started by the Windows autostart entry");

    /* One shell per machine: a second launch shows the window of the first one instead of starting a
     * second server that nobody can reach. A named mutex is what Windows has for this. */
    HANDLE one = CreateMutexW(NULL, TRUE, L"ProjectLife.Shell.Instance");
    if (one && GetLastError() == ERROR_ALREADY_EXISTS) {
        /* A second launch cannot raise the first window itself (it has no handle to it), and Windows
         * shows a message box from a process that is about to exit; the running shell — the one that
         * holds the mutex — is the one that reads this request and brings itself forward. */
        wchar_t req[MAX_PATH * 2];
        _snwprintf(req, MAX_PATH * 2, L"%s\\show.request", g.app_home);
        FILE *r = _wfopen(req, L"w, ccs=UTF-8");
        if (r) { fwprintf(r, L"show\n"); fclose(r); }
        MessageBoxW(NULL, L"Project Life is already running; its window has been asked to come to the "
                          L"front.", L"Project Life", MB_OK | MB_ICONINFORMATION);
        CloseHandle(one);
        return 0;
    }

    /* The handshake file, so a second launch (and a person) can see who is running. */
    {
        wchar_t p[MAX_PATH * 2];
        _snwprintf(p, MAX_PATH * 2, L"%s\\shell.json", g.app_home);
        FILE *f = _wfopen(p, L"w, ccs=UTF-8");
        if (f) {
            fwprintf(f, L"{\"pid\": %lu, \"version\": \"%ls\"}\n", GetCurrentProcessId(), SHELL_VERSION);
            fclose(f);
        }
    }

    pl_window_class();
    g.hwnd = CreateWindowExW(0, L"ProjectLifeShell", SHELL_NAME, WS_OVERLAPPEDWINDOW,
                             CW_USEDEFAULT, CW_USEDEFAULT, 1180, 780, NULL, NULL, inst, NULL);
    if (!g.hwnd) {
        pl_log("no window could be created (error %lu)", GetLastError());
        return 2;
    }

    /* The tray icon. Windows 11 hides new icons by default, which the balloon on the first close
     * speaks to; on Windows 10 it appears beside the clock. */
    g.nid.cbSize = sizeof g.nid;
    g.nid.hWnd = g.hwnd;
    g.nid.uID = TRAY_ID;
    g.nid.uCallbackMessage = WM_PL_TRAY;
    g.nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    g.nid.hIcon = LoadIconW(inst, L"ProjectLife");
    if (!g.nid.hIcon) g.nid.hIcon = LoadIconW(NULL, IDI_APPLICATION);
    wcsncpy(g.nid.szTip, SHELL_NAME, 127);
    g.tray_up = Shell_NotifyIconW(NIM_ADD, &g.nid) ? TRUE : FALSE;
    if (!g.tray_up) pl_log("no tray icon could be added; the window's own menu carries the same list");
    g.tray_menu = CreatePopupMenu();
    g.item_ids = calloc(512, sizeof(char *));

    if (!pl_start_server()) {
        if (g.selftest) {
            pl_say(L"selftest: RESULT=FAIL server");
            if (g_selftest_file) fclose(g_selftest_file);
        }
        return 3;
    }

    /* The menu comes from the server as data, in the line format that needs no JSON parser. */
    g.menu_plain = pl_http("GET", "menu?shell=1&plain=1", NULL, NULL);
    pl_refresh_state();
    pl_build_tray_menu();
    pl_set_tray_tip();
    pl_log("tray menu built: %d server entr%s", g.item_count, g.item_count == 1 ? "y" : "ies");

    {
        wchar_t data[MAX_PATH * 2];
        _snwprintf(data, MAX_PATH * 2, L"%s\\WebView2", g.app_home);
        pl_ensure_dir(data);
        EnvHandler *h = calloc(1, sizeof(EnvHandler));
        h->lpVtbl = &env_vtbl;
        h->ref = 1;
        HRESULT hr = CreateCoreWebView2EnvironmentWithOptions(
            NULL, data, NULL, (ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler *)h);
        if (FAILED(hr)) {
            pl_log("CreateCoreWebView2EnvironmentWithOptions failed (hr=0x%08lx)", (unsigned long)hr);
            wchar_t msg[600];
            _snwprintf(msg, 600,
                       L"The WebView2 runtime could not be started (hr=0x%08lx).\n\n"
                       L"Install the Evergreen runtime from Microsoft and start Project Life again.",
                       (unsigned long)hr);
            MessageBoxW(NULL, msg, L"Project Life cannot show its window", MB_OK | MB_ICONERROR);
            return 4;
        }
    }

    SetTimer(g.hwnd, 3, 1000, NULL);
    ShowWindow(g.hwnd, g.selftest ? SW_SHOWMINNOACTIVE : SW_SHOW);
    UpdateWindow(g.hwnd);
    if (g.selftest) SetTimer(g.hwnd, 2, 700, NULL);

    MSG msg;
    while (GetMessageW(&msg, NULL, 0, 0) > 0) {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if (!g.quitting && g.server.hProcess) {
        pl_post("shutdown", "{}");
        WaitForSingleObject(g.server.hProcess, 10000);
        DWORD code = 0;
        if (GetExitCodeProcess(g.server.hProcess, &code) && code == STILL_ACTIVE) TerminateProcess(g.server.hProcess, 0);
    }
    if (g.tray_up) Shell_NotifyIconW(NIM_DELETE, &g.nid);
    free(g.menu_plain);
    pl_log("shell leaving");
    if (g_selftest_file) fclose(g_selftest_file);
    if (one) {
        ReleaseMutex(one);
        CloseHandle(one);
    }
    return 0;
}

static void pl_window_class(void)
{
    WNDCLASSEXW wc;
    memset(&wc, 0, sizeof wc);
    wc.cbSize = sizeof wc;
    wc.lpfnWndProc = pl_wndproc;
    wc.hInstance = g.inst;
    wc.lpszClassName = L"ProjectLifeShell";
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.hIcon = LoadIconW(g.inst, L"ProjectLife");
    RegisterClassExW(&wc);
}
