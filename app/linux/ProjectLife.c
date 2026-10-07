/* Project Life — the Linux shell.
 *
 * What this program is: the window, the tray icon and the menu, and nothing else. It does not store
 * anything, decide anything about observation, or read a single archive file: it starts the same
 * `projectlife-ui` server the macOS bundle starts and the same `projectlife` core underneath it, and
 * every number it shows comes from that server's own JSON. There is exactly one implementation of
 * every mechanism in this project and it is in the core — a second one here would be a second set of
 * bugs.
 *
 * The contract with the server is four calls:
 *
 *   GET  /api/menu?shell=1   the whole menu, as data (groups -> items), so the menu bar and the tray
 *                            menu are built from one list and cannot disagree with each other
 *   GET  /api/watch          the state: `protection.state`, its label and its reason, and the build
 *   POST /api/menu/run       run one entry by id
 *   POST /api/shutdown       leave completely — the server stops the daemon it started and exits
 *
 * Three promises this shell keeps, because the round that produced it was about them:
 *
 *  1. **Closing the window does not stop the protection.** The window hides, the daemon keeps
 *     running, and the tray icon stays. The first time it happens the person is told in words, not
 *     left to infer it from a window that vanished.
 *  2. **Quitting fully says what it does.** `Quit completely` stops the observation this app started
 *     and names what stays in the archive.
 *  3. **No control is a decoration.** Every menu entry that is offered runs a real core command
 *     through the server; an entry that cannot run here is shown disabled with the reason the server
 *     gave. Nothing pretends.
 *
 * `--selftest` is how this program is checked without a person: it loads the real page in the real
 * WebKit, asks the DOM what it found, writes a PNG of the window through WebKit's own snapshot API,
 * prints one machine-readable line and exits. `tools/linux_shell_check.py` runs it under Xvfb.
 */

#define _GNU_SOURCE

#include <gtk/gtk.h>
#include <webkit2/webkit2.h>
#include <libsoup/soup.h>
#include <json-glib/json-glib.h>

#ifdef PL_HAVE_APPINDICATOR
#include <libayatana-appindicator/app-indicator.h>
#endif

#include <cairo.h>
#include <glib/gstdio.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>

/* Who made this and what it is for: the owner's name, his address and the sentence about the agent.
 * Generated from src/brand.rs by tools/brand.py; the build passes -I .. so the Linux, Windows and
 * macOS shells all include the same bytes. */
#include "pl_brand.h"
#include <unistd.h>

#ifndef PL_GTK_VERSION
#define PL_GTK_VERSION "unknown"
#endif
#ifndef PL_WEBKIT_VERSION
#define PL_WEBKIT_VERSION "unknown"
#endif
#ifndef PL_APPINDICATOR_VERSION
#define PL_APPINDICATOR_VERSION "none"
#endif

#define SHELL_NAME "Project Life"
#define SHELL_VERSION "0.9.5"

/* ---------------------------------------------------------------------------------------------
 * State
 * -------------------------------------------------------------------------------------------*/

static struct {
    /* command line */
    char *ui_bin;          /* projectlife-ui, beside this program by default */
    char *core_bin;        /* projectlife */
    char *app_home;        /* PROJECTLIFE_APP_HOME, else XDG state dir */
    char *archive;         /* optional: --archive DIR, passed to the server */
    char *lang;            /* --lang en|ru */
    int port, port_range;
    gboolean no_tray;
    char *screenshot;      /* --selftest --screenshot FILE */
    gboolean selftest;
    gboolean selftest_close;   /* emit the window's own close event, as a window manager would */
    gboolean suppress_notice;  /* the first-close notice has nobody to read it in a headless run */

    /* the server */
    GPid server_pid;
    int server_out;
    char *url, *token, *server_pid_line, *daemon_log;
    guint drain_id;

    /* the window */
    GtkWidget *window, *box, *menubar, *status;
    WebKitWebView *wv;
    gboolean loaded, quitting, close_notice_shown;
    guint poll_id;

    /* the tray */
    gboolean tray_created;
    GtkWidget *tray_menu;
    GtkWidget *tray_state_item;
#ifdef PL_HAVE_APPINDICATOR
    AppIndicator *indicator;
#endif

    /* state as the server last reported it */
    char *state, *state_reason, *state_label, *build_short;
    gboolean observing;

    /* the menu document, kept so an entry that needs a value can be asked about it */
    JsonNode *menu_doc;

    /* selftest */
    int selftest_probe_count;
    guint selftest_timeout;
} g;

static SoupSession *soup;

/* ---------------------------------------------------------------------------------------------
 * Small things: time, logging, JSON helpers
 * -------------------------------------------------------------------------------------------*/

static char *pl_now_iso(void)
{
    time_t t = time(NULL);
    struct tm tm;
    localtime_r(&t, &tm);
    char *s = g_malloc0(32);
    strftime(s, 32, "%Y-%m-%d %H:%M:%S", &tm);
    return s;
}

static void pl_log(const char *fmt, ...)
{
    char *when = pl_now_iso();
    va_list ap;
    va_start(ap, fmt);
    char *msg = g_strdup_vprintf(fmt, ap);
    va_end(ap);

    if (g.daemon_log) {
        FILE *f = fopen(g.daemon_log, "a");
        if (f) {
            fprintf(f, "%s projectlife-shell: %s\n", when, msg);
            fclose(f);
        } else {
            fprintf(stderr, "%s projectlife-shell: %s\n", when, msg);
        }
    } else {
        fprintf(stderr, "%s projectlife-shell: %s\n", when, msg);
    }
    g_free(msg);
    g_free(when);
}

/* Where this app keeps logs and its handshake. The same rule the core uses, written once here and
 * asserted by tools/linux_shell_check.py against the path the core itself prints. */
static char *pl_default_app_home(void)
{
    const char *env = g_getenv("PROJECTLIFE_APP_HOME");
    if (env && *env) return g_strdup(env);
    const char *state = g_getenv("XDG_STATE_HOME");
    const char *home = g_getenv("HOME");
    if (state && *state) return g_build_filename(state, "projectlife-app", NULL);
    if (home && *home) return g_build_filename(home, ".local/state/projectlife-app", NULL);
    return g_strdup("/tmp/projectlife-app");
}

static char *jstr(JsonObject *o, const char *key)
{
    if (!o || !json_object_has_member(o, key)) return NULL;
    JsonNode *n = json_object_get_member(o, key);
    if (!JSON_NODE_HOLDS_VALUE(n)) return NULL;
    return g_strdup(json_node_get_string(n));
}

static gint64 jint(JsonObject *o, const char *key, gint64 fallback)
{
    if (!o || !json_object_has_member(o, key)) return fallback;
    JsonNode *n = json_object_get_member(o, key);
    if (!JSON_NODE_HOLDS_VALUE(n)) return fallback;
    switch (json_node_get_value_type(n)) {
    case G_TYPE_INT64:
    case G_TYPE_INT:
    case G_TYPE_LONG:
        return json_node_get_int(n);
    case G_TYPE_DOUBLE:
        return (gint64)json_node_get_double(n);
    default:
        return fallback;
    }
}

static JsonObject *jobj(JsonNode *n)
{
    if (!n || !JSON_NODE_HOLDS_OBJECT(n)) return NULL;
    return json_node_get_object(n);
}

/* ---------------------------------------------------------------------------------------------
 * Talking to the server
 * -------------------------------------------------------------------------------------------*/

static char *pl_http(const char *method, const char *path, const char *body)
{
    char *url = g_strdup_printf("http://127.0.0.1:%d/api/%s%s%s", g.port, path,
                                strchr(path, '?') ? "&" : "?", g.token ? g.token : "");
    /* The token travels as a query parameter (the page does the same): this is a local, per-launch
     * secret, and the server refuses anything without it. */
    char *url2 = g_strdup_printf("http://127.0.0.1:%d/api/%s%stoken=%s", g.port, path,
                                 strchr(path, '?') ? "&" : "?", g.token ? g.token : "");
    g_free(url);

    SoupMessage *msg = soup_message_new(strcmp(method, "GET") == 0 ? "GET" : "POST", url2);
    if (!msg) {
        g_free(url2);
        return NULL;
    }
    if (body) {
        GBytes *b = g_bytes_new(body, strlen(body));
        soup_message_set_request_body_from_bytes(msg, "application/json", b);
        g_bytes_unref(b);
    }
    GError *err = NULL;
    gsize len = 0;
    GBytes *resp = soup_session_send_and_read(soup, msg, NULL, &err);
    char *text = NULL;
    if (resp) {
        const char *data = g_bytes_get_data(resp, &len);
        text = g_strndup(data, len);
        g_bytes_unref(resp);
    } else if (err) {
        pl_log("the server did not answer %s /api/%s: %s", method, path, err->message);
        g_clear_error(&err);
    }
    g_object_unref(msg);
    g_free(url2);
    return text;
}

/* A JSON GET that returns a standalone tree the caller owns. */
static JsonNode *pl_get_node(const char *path)
{
    char *text = pl_http("GET", path, NULL);
    if (!text) return NULL;
    JsonParser *p = json_parser_new();
    JsonNode *out = NULL;
    if (json_parser_load_from_data(p, text, -1, NULL)) {
        out = json_node_copy(json_parser_get_root(p));
    } else {
        pl_log("the server's answer to /api/%s was not JSON (%zu bytes)", path, strlen(text));
    }
    g_object_unref(p);
    g_free(text);
    return out;
}

/* ---------------------------------------------------------------------------------------------
 * The state, as the server reports it
 * -------------------------------------------------------------------------------------------*/

static void pl_state_release(void)
{
    g_free(g.state); g.state = NULL;
    g_free(g.state_reason); g.state_reason = NULL;
    g_free(g.state_label); g.state_label = NULL;
    g_free(g.build_short); g.build_short = NULL;
}

/* One GET /api/watch, and the words follow from it: nothing here decides whether protection is
 * working — the server does, from the core's heartbeat and the storage verdict. */
static void pl_refresh_state(void)
{
    JsonNode *n = pl_get_node("watch");
    if (!n) {
        pl_state_release();
        g.state = g_strdup("unknown");
        g.state_label = g_strdup("Unavailable");
        g.state_reason = g_strdup("The interface server is not answering.");
        g.observing = FALSE;
        return;
    }
    JsonObject *o = jobj(n);
    pl_state_release();
    JsonObject *prot = jobj(json_object_get_member(o, "protection"));
    JsonObject *build = jobj(json_object_get_member(o, "build"));
    g.observing = json_object_get_boolean_member_with_default(o, "runningByApp", FALSE);
    if (build) {
        g.build_short = jstr(build, "short");
    }
    if (prot) {
        g.state = jstr(prot, "state");
        g.state_label = jstr(prot, "label");
        g.state_reason = jstr(prot, "reason");
    } else {
        g.state = g_strdup("unknown");
        g.state_label = g_strdup("Unknown");
        g.state_reason = g_strdup("This server did not report a protection state.");
    }
    json_node_unref(n);
}

/* The icon name for the state, looked up in the theme so a missing icon is a fallback and not an
 * empty space: a tick when versions are being written, a warning when writing is stopped, a crossed
 * shield when nothing is observing. */
static const char *pl_icon_for_state(void)
{
    const char *s = g.state ? g.state : "unknown";
    if (strcmp(s, "protected") == 0 || strcmp(s, "protected_low_space") == 0) return "security-high";
    if (strcmp(s, "paused_full") == 0) return "dialog-warning";
    return "security-low";
}

static void pl_update_tray(void)
{
    const char *icon = pl_icon_for_state();
    char *tip = g_strdup_printf("%s — %s", SHELL_NAME,
                                g.state_label ? g.state_label : "…");
#ifdef PL_HAVE_APPINDICATOR
    if (g.tray_created && g.indicator) {
        app_indicator_set_icon_full(g.indicator, icon, tip);
        app_indicator_set_title(g.indicator, tip);
    }
#endif
#ifdef PL_HAVE_APPINDICATOR
    if (g.tray_created && g.indicator) {
        app_indicator_set_status(g.indicator, APP_INDICATOR_STATUS_ACTIVE);
    }
#endif
    g_free(tip);
}

/* ---------------------------------------------------------------------------------------------
 * Menus: one document from the server, two fronts
 * -------------------------------------------------------------------------------------------*/

static void pl_own_menu_front(void);
static gboolean pl_selftest_poll(gpointer data);
static void pl_run_menu_id(const char *id);
static void pl_open_window(void);
static void pl_status_dialog(void);
static void pl_diagnose_dialog(void);
static void pl_toggle_watch(void);
static void pl_quit_completely(void);

/* Find an entry in the menu document the server sent. */
static JsonObject *pl_menu_item(const char *id)
{
    JsonObject *root = jobj(g.menu_doc);
    if (!root) return NULL;
    JsonArray *groups = json_object_get_array_member(root, "groups");
    if (!groups) return NULL;
    guint ng = json_array_get_length(groups);
    for (guint i = 0; i < ng; i++) {
        JsonObject *grp = jobj(json_array_get_element(groups, i));
        JsonArray *items = grp ? json_object_get_array_member(grp, "items") : NULL;
        if (!items) continue;
        guint ni = json_array_get_length(items);
        for (guint j = 0; j < ni; j++) {
            JsonObject *it = jobj(json_array_get_element(items, j));
            if (!it) continue;
            char *iid = jstr(it, "id");
            gboolean hit = iid && strcmp(iid, id) == 0;
            g_free(iid);
            if (hit) return it;
        }
    }
    return NULL;
}

static void pl_on_menu_activate(GtkMenuItem *item, gpointer data)
{
    (void)item;
    pl_run_menu_id((const char *)data);
}

static GtkWidget *pl_menu_for_group(JsonObject *grp)
{
    GtkWidget *sub = gtk_menu_new();
    JsonArray *items = json_object_get_array_member(grp, "items");
    guint n = items ? json_array_get_length(items) : 0;
    for (guint i = 0; i < n; i++) {
        JsonObject *it = jobj(json_array_get_element(items, i));
        if (!it) continue;
        char *label = jstr(it, "label");
        char *id = jstr(it, "id");
        gboolean enabled = json_object_get_boolean_member_with_default(it, "enabled", TRUE);
        char *why = jstr(it, "why");
        char *core_line = jstr(it, "coreRun");
        GtkWidget *mi = gtk_menu_item_new_with_label(label ? label : "?");
        gtk_widget_set_sensitive(mi, enabled);
        /* The core command is the tooltip, because that is the promise: every entry names the
         * command it runs, and a person can run it by hand instead. Disabled entries carry the
         * server's reason instead of hiding why they are disabled. */
        if (!enabled && why) gtk_widget_set_tooltip_text(mi, why);
        else if (core_line) gtk_widget_set_tooltip_text(mi, core_line);
        g_signal_connect(mi, "activate", G_CALLBACK(pl_on_menu_activate), g_strdup(id ? id : ""));
        gtk_menu_shell_append(GTK_MENU_SHELL(sub), mi);
        g_free(label); g_free(id); g_free(why); g_free(core_line);
    }
    return sub;
}

static void pl_rebuild_menus(void)
{
    JsonNode *n = pl_get_node("menu?shell=1");
    if (!n) {
        pl_log("the menu could not be read from the server");
        return;
    }
    if (g.menu_doc) json_node_unref(g.menu_doc);
    g.menu_doc = n;

    JsonObject *root = jobj(g.menu_doc);
    JsonArray *groups = json_object_get_array_member(root, "groups");

    if (g.menubar) {
        GList *kids = gtk_container_get_children(GTK_CONTAINER(g.menubar));
        for (GList *l = kids; l; l = l->next) gtk_widget_destroy(GTK_WIDGET(l->data));
        g_list_free(kids);
    }
    if (g.tray_menu) {
        GList *kids = gtk_container_get_children(GTK_CONTAINER(g.tray_menu));
        for (GList *l = kids; l; l = l->next) gtk_widget_destroy(GTK_WIDGET(l->data));
        g_list_free(kids);
    }

    guint n_groups = groups ? json_array_get_length(groups) : 0;
    for (guint i = 0; i < n_groups; i++) {
        JsonObject *grp = jobj(json_array_get_element(groups, i));
        if (!grp) continue;
        char *title = jstr(grp, "title");
        GtkWidget *sub = pl_menu_for_group(grp);
        gtk_widget_show_all(sub);
        if (g.menubar) {
            GtkWidget *top = gtk_menu_item_new_with_label(title ? title : "?");
            gtk_menu_item_set_submenu(GTK_MENU_ITEM(top), sub);
            gtk_menu_shell_append(GTK_MENU_SHELL(g.menubar), top);
        }
        if (g.tray_menu) {
            GtkWidget *top = gtk_menu_item_new_with_label(title ? title : "?");
            GtkWidget *sub2 = pl_menu_for_group(grp);
            gtk_widget_show_all(sub2);
            gtk_menu_item_set_submenu(GTK_MENU_ITEM(top), sub2);
            gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), top);
        }
        g_free(title);
    }
    if (g.menubar) gtk_widget_show_all(g.menubar);
    /* The shell's own lines go into the tray menu after the server's groups were cleared and
     * rebuilt — so opening the menu always shows the state as of now, and the same list is on both
     * fronts. */
    pl_own_menu_front();
    if (g.tray_menu) gtk_widget_show_all(g.tray_menu);
}

/* An entry that asks for a value first: the page draws its own dialog, and so does this shell, so
 * nothing depends on a toolkit dialog that may not exist. */
static char *pl_ask_text(const char *title, const char *initial)
{
    GtkWidget *d = gtk_dialog_new_with_buttons(title ? title : "Value", GTK_WINDOW(g.window),
                                               GTK_DIALOG_MODAL | GTK_DIALOG_DESTROY_WITH_PARENT,
                                               "_Cancel", GTK_RESPONSE_CANCEL,
                                               "_OK", GTK_RESPONSE_ACCEPT, NULL);
    GtkWidget *entry = gtk_entry_new();
    if (initial) gtk_entry_set_text(GTK_ENTRY(entry), initial);
    GtkWidget *area = gtk_dialog_get_content_area(GTK_DIALOG(d));
    gtk_box_pack_start(GTK_BOX(area), entry, FALSE, FALSE, 8);
    gtk_widget_show_all(d);
    char *answer = NULL;
    if (gtk_dialog_run(GTK_DIALOG(d)) == GTK_RESPONSE_ACCEPT) {
        const char *t = gtk_entry_get_text(GTK_ENTRY(entry));
        if (t && *t) answer = g_strdup(t);
    }
    gtk_widget_destroy(d);
    return answer;
}

static void pl_run_menu_id(const char *id)
{
    if (!id || !*id) return;
    JsonObject *it = pl_menu_item(id);
    char *input = NULL;
    gboolean confirm = FALSE;
    if (it) {
        char *needs = jstr(it, "input");
        char *kind = jstr(it, "kind");
        char *label = jstr(it, "label");
        if (needs && *needs) {
            input = pl_ask_text(label ? label : "Value", "");
            if (!input) { g_free(needs); g_free(kind); g_free(label); return; }
        }
        if (kind && strcmp(kind, "confirm") == 0) {
            GtkWidget *dlg = gtk_message_dialog_new(GTK_WINDOW(g.window), GTK_DIALOG_MODAL,
                                                    GTK_MESSAGE_QUESTION, GTK_BUTTONS_YES_NO,
                                                    "%s", label ? label : id);
            gtk_message_dialog_format_secondary_text(GTK_MESSAGE_DIALOG(dlg),
                                                     "This changes what is stored in the archive. Continue?");
            confirm = gtk_dialog_run(GTK_DIALOG(dlg)) == GTK_RESPONSE_YES;
            gtk_widget_destroy(dlg);
            if (!confirm) { g_free(needs); g_free(kind); g_free(label); return; }
        }
        g_free(needs); g_free(kind); g_free(label);
    }
    /* The body is the same JSON the page sends through this route: nothing about the entry is
     * re-decided here. */
    char *esc = input ? g_strescape(input, NULL) : NULL;
    char *body = g_strdup_printf("{\"id\":\"%s\"%s%s%s%s}",
                                 id,
                                 input ? ",\"input\":\"" : "",
                                 input ? esc : "",
                                 input ? "\"" : "",
                                 confirm ? ",\"confirm\":true" : "");
    char *answer = pl_http("POST", "menu/run", body);
    if (answer) {
        pl_log("menu %s -> %s", id, answer);
        g_free(answer);
    } else {
        pl_log("menu %s: the server did not answer", id);
    }
    g_free(body);
    g_free(esc);
    g_free(input);
    /* Whatever the entry did may have changed the state. */
    pl_refresh_state();
    if (g.status) {
        char *line = g_strdup_printf("%s — %s", g.state_label ? g.state_label : "…",
                                     g.state_reason ? g.state_reason : "");
        gtk_label_set_text(GTK_LABEL(g.status), line);
        g_free(line);
    }
    pl_update_tray();
}

static gboolean pl_refresh_later(gpointer data)
{
    (void)data;
    pl_refresh_state();
    pl_update_tray();
    return FALSE;
}

static void pl_toggle_watch(void)
{
    const char *path = g.observing ? "watch/stop" : "watch/start";
    char *body = g.observing ? g_strdup("{}") : g_strdup("{}");
    char *answer = pl_http("POST", path, body);
    if (answer) {
        pl_log("%s -> %s", path, answer);
        g_free(answer);
    }
    g_free(body);
    /* The daemon takes a moment to appear in the heartbeat; ask again shortly. */
    pl_refresh_state();
    pl_update_tray();
    g_timeout_add(900, pl_refresh_later, NULL);
    g_timeout_add(2500, pl_refresh_later, NULL);
}

static void pl_status_dialog(void)
{
    pl_refresh_state();
    GtkWidget *d = gtk_message_dialog_new(GTK_WINDOW(g.window), GTK_DIALOG_MODAL | GTK_DIALOG_DESTROY_WITH_PARENT,
                                          GTK_MESSAGE_INFO, GTK_BUTTONS_CLOSE,
                                          "Protection: %s", g.state_label ? g.state_label : "unknown");
    char *extra = g_strdup_printf("%s\n\nstate: %s\n\n%s",
                                  g.state_reason ? g.state_reason : "",
                                  g.state ? g.state : "unknown",
                                  g.build_short ? g.build_short : "");
    gtk_message_dialog_format_secondary_text(GTK_MESSAGE_DIALOG(d), "%s", extra);
    g_free(extra);
    gtk_dialog_run(GTK_DIALOG(d));
    gtk_widget_destroy(d);
}

static void pl_diagnose_dialog(void)
{
    char *ui = g_strdup(g.ui_bin);
    char *out = NULL;
    char *argv[5] = { ui, "--diagnose", NULL, NULL, NULL };
    if (g.app_home) {
        argv[2] = "--log-dir";
        argv[3] = g.app_home;
    }
    GError *err = NULL;
    gint status = 0;
    if (!g_spawn_sync(NULL, argv, NULL, G_SPAWN_DEFAULT, NULL, NULL, &out, NULL, &status, &err)) {
        out = g_strdup_printf("could not run --diagnose: %s", err ? err->message : "?");
        g_clear_error(&err);
    }
    GtkWidget *d = gtk_dialog_new_with_buttons("Network diagnosis", GTK_WINDOW(g.window),
                                               GTK_DIALOG_MODAL | GTK_DIALOG_DESTROY_WITH_PARENT,
                                               "_Close", GTK_RESPONSE_CLOSE, NULL);
    gtk_window_set_default_size(GTK_WINDOW(d), 720, 520);
    GtkWidget *view = gtk_text_view_new();
    gtk_text_view_set_editable(GTK_TEXT_VIEW(view), FALSE);
    gtk_text_view_set_monospace(GTK_TEXT_VIEW(view), TRUE);
    GtkTextBuffer *buf = gtk_text_view_get_buffer(GTK_TEXT_VIEW(view));
    char *full = g_strdup_printf("%s\n\nwritten to: %s/diagnose.txt\n",
                                 out ? out : "(no output)", g.app_home ? g.app_home : "?");
    gtk_text_buffer_set_text(buf, full, -1);
    g_free(full);
    GtkWidget *scroll = gtk_scrolled_window_new(NULL, NULL);
    gtk_container_add(GTK_CONTAINER(scroll), view);
    gtk_box_pack_start(GTK_BOX(gtk_dialog_get_content_area(GTK_DIALOG(d))), scroll, TRUE, TRUE, 6);
    gtk_widget_show_all(d);
    gtk_dialog_run(GTK_DIALOG(d));
    gtk_widget_destroy(d);
    g_free(out);
    g_free(ui);
}

/* ---------------------------------------------------------------------------------------------
 * The window
 * -------------------------------------------------------------------------------------------*/

static void pl_open_window(void)
{
    if (!g.window) return;
    gtk_widget_show_all(g.window);
    gtk_window_present(GTK_WINDOW(g.window));
}

static void pl_on_unix_signal(int sig)
{
    /* A signal is a terminal being closed, a session ending, a `kill` — not a click. The observation
     * this app started is stopped, because nobody can reach this window any more, and the log says
     * which signal it was instead of leaving a person to guess why protection stopped. */
    pl_log("the shell was asked to stop by signal %d: stopping the observation it started "
           "(no confirmation dialog: a signal is not a question)", sig);
    g.quitting = TRUE;
    if (g.server_pid > 0) {
        char *answer = pl_http("POST", "shutdown", "{}");
        if (answer) g_free(answer);
    }
    gtk_main_quit();
}

static void pl_quit_completely_ask(gboolean ask);

static void pl_quit_completely(void)
{
    pl_quit_completely_ask(TRUE);
}

static void pl_quit_completely_ask(gboolean ask)
{
    if (ask) {
    GtkWidget *d = gtk_message_dialog_new(GTK_WINDOW(g.window), GTK_DIALOG_MODAL,
                                          GTK_MESSAGE_QUESTION, GTK_BUTTONS_NONE,
                                          "Quit Project Life completely?");
    gtk_message_dialog_format_secondary_text(GTK_MESSAGE_DIALOG(d),
        "This stops the observation this app started and closes the window.\n\n"
        "Nothing in the archive is deleted: every version already stored stays where it is, and the "
        "project folders on disk are not touched. Observation stops until you start the app again.");
    gtk_dialog_add_buttons(GTK_DIALOG(d), "_Keep running", GTK_RESPONSE_CANCEL,
                           "_Quit completely", GTK_RESPONSE_ACCEPT, NULL);
    gint r = gtk_dialog_run(GTK_DIALOG(d));
    gtk_widget_destroy(d);
    if (r != GTK_RESPONSE_ACCEPT) return;
    }

    pl_log("quitting: asking the server to stop the observation it started");
    char *answer = pl_http("POST", "shutdown", "{}");
    if (answer) {
        pl_log("shutdown -> %s", answer);
        g_free(answer);
    }
    g.quitting = TRUE;
    /* The server exits on its own; give it a moment, then leave regardless — a window that refuses
     * to close because a child is slow is worse than a child that is killed. */
    for (int i = 0; i < 60 && g.server_pid > 0; i++) {
        int st = 0;
        pid_t w = waitpid(g.server_pid, &st, WNOHANG);
        if (w == (pid_t)g.server_pid || w < 0) { g.server_pid = 0; break; }
        g_usleep(100 * 1000);
    }
    if (g.server_pid > 0) {
        pl_log("the server did not leave within 6 s; stopping it");
        kill(g.server_pid, SIGTERM);
        g.server_pid = 0;
    }
    gtk_main_quit();
}

static gboolean pl_on_delete(GtkWidget *w, GdkEvent *e, gpointer data)
{
    (void)w; (void)e; (void)data;
    /* Closing the window hides it. The protection is a separate process and does not notice. */
    gtk_widget_hide(g.window);
    pl_log("the window was closed: it is hidden, the observation was not stopped (tray=%s)%s",
           g.tray_created ? "appindicator" : "none",
           g.suppress_notice ? "; the first-close notice was suppressed: this run has nobody to read it" : "");
    if (g.suppress_notice) return TRUE;
    if (!g.close_notice_shown) {
        g.close_notice_shown = TRUE;
        GtkWidget *d = gtk_message_dialog_new(GTK_WINDOW(g.window), GTK_DIALOG_MODAL,
                                              GTK_MESSAGE_INFO, GTK_BUTTONS_OK,
                                              "Project Life is still protecting this project.");
        const char *how = g.tray_created
            ? "The icon in the system tray brings the window back, and it shows the state while the "
              "window is closed. Use its menu to stop observation or to quit completely."
            : "This desktop did not offer a tray icon, so bring the window back by starting Project "
              "Life again. Use the menu to stop observation or to quit completely.";
        gtk_message_dialog_format_secondary_text(GTK_MESSAGE_DIALOG(d), "%s", how);
        gtk_dialog_run(GTK_DIALOG(d));
        gtk_widget_destroy(d);
    }
    return TRUE;
}

static gboolean pl_on_poll(gpointer data)
{
    (void)data;
    /* Ask whether another launch wants the window (it cannot raise ours directly: it has no handle
     * to it). One file, and whoever is running reads it. */
    if (g.app_home) {
        char *req = g_build_filename(g.app_home, "show.request", NULL);
        if (g_file_test(req, G_FILE_TEST_EXISTS)) {
            unlink(req);
            pl_open_window();
        }
        g_free(req);
        /* The same channel, for the other direction: whatever started this window (a launcher, a
         * desktop session ending, the check that runs this program) can ask it to leave by name.
         * The quit path is the menu's own: stop the observation this app started, then close. */
        char *quit = g_build_filename(g.app_home, "quit.request", NULL);
        if (g_file_test(quit, G_FILE_TEST_EXISTS)) {
            unlink(quit);
            g_free(quit);
            pl_log("quit requested through the app folder; leaving the way the menu does "
                   "(no confirmation dialog: whoever asked is not here to answer one)");
            pl_quit_completely_ask(FALSE);
            return FALSE;
        }
        g_free(quit);
    }
    pl_refresh_state();
    if (g.status) {
        char *line = g_strdup_printf("%s — %s", g.state_label ? g.state_label : "…",
                                     g.state_reason ? g.state_reason : "");
        gtk_label_set_text(GTK_LABEL(g.status), line);
        g_free(line);
    }
    if (g.tray_state_item) {
        gtk_menu_item_set_label(GTK_MENU_ITEM(g.tray_state_item),
                               g.state_label ? g.state_label : "…");
    }
    pl_update_tray();
    return TRUE;
}

/* ---------------------------------------------------------------------------------------------
 * The page's own bridge: pl://pick-folder and pl://reveal
 *
 * The page calls these only when the server said `native: true`. Serving them is what makes the
 * folder chooser a real system dialog; if this were missing the page would fall back to its own text
 * prompt (which is why the fallback exists), but a shell that cannot pick a folder is not the
 * program the design describes. Both answers are JSON, like every other answer in this project.
 * -------------------------------------------------------------------------------------------*/

static GtkWidget *pl_folder_dialog(const char *title)
{
    return gtk_file_chooser_dialog_new(title && *title ? title : "Choose a folder",
                                       GTK_WINDOW(g.window), GTK_FILE_CHOOSER_ACTION_SELECT_FOLDER,
                                       "_Cancel", GTK_RESPONSE_CANCEL,
                                       "_Choose", GTK_RESPONSE_ACCEPT, NULL);
}

static void pl_scheme_request(WebKitURISchemeRequest *req, gpointer data)
{
    (void)data;
    const char *path = webkit_uri_scheme_request_get_path(req);
    char *full = g_strdup(webkit_uri_scheme_request_get_uri(req));
    char *body = NULL;

    if (g_str_has_prefix(full, "pl://pick-folder")) {
        char *title = NULL;
        const char *q = strchr(full, '?');
        if (q) {
            char *dec = g_uri_unescape_string(q + 1, NULL);
            char *t = strstr(dec, "title=");
            if (t) title = g_strdup(t + 6);
            g_free(dec);
        }
        GtkWidget *d = pl_folder_dialog(title);
        char *chosen = NULL;
        if (gtk_dialog_run(GTK_DIALOG(d)) == GTK_RESPONSE_ACCEPT) {
            chosen = gtk_file_chooser_get_filename(GTK_FILE_CHOOSER(d));
        }
        gtk_widget_destroy(d);
        g_free(title);
        body = chosen ? g_strdup_printf("{\"path\":\"%s\"}", chosen)
                      : g_strdup("{\"path\":null}");
        g_free(chosen);
    } else if (g_str_has_prefix(full, "pl://reveal")) {
        /* No file manager call here: opening an external program to show a folder is a decision the
         * person makes, not a side effect of clicking a line in a list. The window says where the
         * folder is instead, which is the part that was missing. */
        body = g_strdup("{\"shown\":false,\"reason\":\"the Linux shell does not open a file manager; the path is in the window\"}");
    } else {
        body = g_strdup_printf("{\"error\":\"no such bridge\",\"path\":\"%s\"}", path ? path : "");
    }

    GInputStream *in = g_memory_input_stream_new_from_data(g_strdup(body), strlen(body), g_free);
    webkit_uri_scheme_request_finish(req, in, -1, "application/json");
    g_object_unref(in);
    g_free(body);
    g_free(full);
}

/* ---------------------------------------------------------------------------------------------
 * The web view
 * -------------------------------------------------------------------------------------------*/

/* One probe run by the shell itself: the DOM's answer about what the page actually shows. This is
 * not the shell guessing — it is the shipped page, in the shipped WebKit, reporting on itself. */
static void pl_selftest_probe(WebKitWebView *wv, GAsyncResult *res, gpointer data);

static void pl_selftest_snapshot(WebKitWebView *wv, GAsyncResult *res, gpointer data);

static void pl_selftest_finish(void)
{
    if (g.selftest_timeout) { g_source_remove(g.selftest_timeout); g.selftest_timeout = 0; }
    gtk_main_quit();
}

static void pl_selftest_snapshot(WebKitWebView *wv, GAsyncResult *res, gpointer data)
{
    (void)data;
    GError *err = NULL;
    cairo_surface_t *surface = webkit_web_view_get_snapshot_finish(wv, res, &err);
    if (!surface) {
        printf("selftest: screenshot=FAILED %s\n", err ? err->message : "?");
        g_clear_error(&err);
        printf("selftest: RESULT=FAIL screenshot\n");
        fflush(stdout);
        pl_selftest_finish();
        return;
    }
    cairo_status_t st = cairo_surface_write_to_png(surface, g.screenshot);
    struct stat sb;
    long long bytes = (stat(g.screenshot, &sb) == 0) ? (long long)sb.st_size : -1;
    printf("selftest: screenshot=%s bytes=%lld status=%d\n", g.screenshot, bytes, (int)st);
    printf("selftest: screenshot-size=%dx%d\n",
           cairo_image_surface_get_width(surface), cairo_image_surface_get_height(surface));
    cairo_surface_destroy(surface);
    printf("selftest: RESULT=%s\n", (st == CAIRO_STATUS_SUCCESS && bytes > 5000) ? "PASS" : "FAIL screenshot");
    fflush(stdout);
    pl_selftest_finish();
}

static void pl_selftest_probe(WebKitWebView *wv, GAsyncResult *res, gpointer data)
{
    (void)data;
    GError *err = NULL;
    JSCValue *v = webkit_web_view_evaluate_javascript_finish(wv, res, &err);
    if (!v) {
        printf("selftest: probe=FAILED %s\n", err ? err->message : "?");
        g_clear_error(&err);
        printf("selftest: RESULT=FAIL probe\n");
        fflush(stdout);
        pl_selftest_finish();
        return;
    }
    char *json = jsc_value_to_string(v);
    JsonParser *p = json_parser_new();
    if (!json_parser_load_from_data(p, json, -1, NULL)) {
        printf("selftest: probe=UNREADABLE %s\n", json ? json : "(null)");
        printf("selftest: RESULT=FAIL probe-json\n");
        fflush(stdout);
        g_object_unref(p);
        g_free(json);
        g_object_unref(v);
        pl_selftest_finish();
        return;
    }
    JsonObject *o = jobj(json_parser_get_root(p));
    if (!o) {
        printf("selftest: probe=NOT-AN-OBJECT %s\n", json ? json : "(null)");
        printf("selftest: RESULT=FAIL probe-json\n");
        fflush(stdout);
        g_object_unref(p);
        g_free(json);
        g_object_unref(v);
        pl_selftest_finish();
        return;
    }
    const char *sidebar = json_object_get_boolean_member_with_default(o, "sidebar", FALSE) ? "yes" : "no";
    const char *menu_api = json_object_get_boolean_member_with_default(o, "menuApi", FALSE) ? "yes" : "no";
    const char *build = json_object_has_member(o, "build") ? json_node_get_string(json_object_get_member(o, "build")) : "";
    const char *protection = json_object_has_member(o, "protection")
        ? json_node_get_string(json_object_get_member(o, "protection")) : "";
    gint64 text_len = json_object_get_int_member_with_default(o, "textLen", 0);
    const char *title = json_object_has_member(o, "title") ? json_node_get_string(json_object_get_member(o, "title")) : "";
    printf("selftest: page=loaded title=\"%s\" sidebar=%s textLen=%lld\n", title, sidebar, (long long)text_len);
    printf("selftest: page-build=\"%s\" protection=\"%s\" menuApi=%s\n", build, protection, menu_api);
    printf("selftest: shell-version=%s tray=%s url-port=%d\n", SHELL_VERSION,
           g.tray_created ? "appindicator" : "none", g.port);
    unsigned groups = (unsigned)json_array_get_length(json_object_get_array_member(jobj(g.menu_doc), "groups"));
    unsigned tray_items = g.tray_menu
        ? (unsigned)g_list_length(gtk_container_get_children(GTK_CONTAINER(g.tray_menu)))
        : 0;
    unsigned bar_items = g.menubar
        ? (unsigned)g_list_length(gtk_container_get_children(GTK_CONTAINER(g.menubar)))
        : 0;
    printf("selftest: menu-groups=%u menu-bar-items=%u tray-items=%u tray=%s\n",
           groups, bar_items, tray_items, g.tray_created ? "appindicator" : "none");
    gboolean ok = strcmp(sidebar, "yes") == 0 && strcmp(menu_api, "yes") == 0;
    if (ok && g.selftest_close) {
        /* The same signal a window manager sends when a person clicks the X, emitted here so the
         * handler is exercised even though there is no window manager in a headless run. */
        g_signal_emit_by_name(g.window, "delete-event", NULL, NULL, &ok);
        gboolean visible = gtk_widget_get_visible(g.window);
        char *watch = pl_http("GET", "watch", NULL);
        gboolean serving = watch != NULL;
        g_free(watch);
        printf("selftest: close window-visible=%s server-still-answering=%s\n",
               visible ? "yes" : "no", serving ? "yes" : "no");
        ok = !visible && serving;
    }
    if (!ok && g.selftest_probe_count < 12) {
        /* The page renders after its own bootstrap call; a few more tries is the difference between
         * "the window is broken" and "the window was asked too early". */
        g_object_unref(p);
        g_free(json);
        g_object_unref(v);
        g_timeout_add(700, pl_selftest_poll, wv);
        return;
    }
    if (!g.screenshot) {
        printf("selftest: RESULT=%s\n", ok ? "PASS" : "FAIL page");
        fflush(stdout);
        g_object_unref(p);
        g_free(json);
        g_object_unref(v);
        pl_selftest_finish();
        return;
    }
    g_object_unref(p);
    g_free(json);
    g_object_unref(v);
    if (!ok) {
        printf("selftest: RESULT=FAIL page\n");
        fflush(stdout);
        pl_selftest_finish();
        return;
    }
    webkit_web_view_get_snapshot(wv, WEBKIT_SNAPSHOT_REGION_VISIBLE, WEBKIT_SNAPSHOT_OPTIONS_NONE,
                                 NULL, (GAsyncReadyCallback)pl_selftest_snapshot, NULL);
}

static const char *PL_PROBE_JS =
    "(function(){"
    "  var t = document.body ? document.body.innerText : '';"
    "  var m = t.match(/build app [^\\n]*/);"
    "  var p = t.match(/(Protected|Not protecting|Recording stopped)[^\\n]*/);"
    "  return JSON.stringify({"
    "    title: document.title,"
    "    sidebar: !!document.querySelector('.sidebar'),"
    "    menuApi: typeof window.plMenu === 'function',"
    "    textLen: t.length,"
    "    build: m ? m[0] : '',"
    "    protection: p ? p[0] : ''"
    "  });"
    "})()";

static gboolean pl_selftest_poll(gpointer data)
{
    WebKitWebView *wv = WEBKIT_WEB_VIEW(data);
    if (!g.loaded) return TRUE;
    g.selftest_probe_count++;
    webkit_web_view_evaluate_javascript(wv, PL_PROBE_JS, -1, NULL, NULL, NULL,
                                        (GAsyncReadyCallback)pl_selftest_probe, NULL);
    return FALSE;
}

static void pl_on_load_changed(WebKitWebView *wv, WebKitLoadEvent ev, gpointer data)
{
    (void)data;
    if (ev == WEBKIT_LOAD_FINISHED) {
        g.loaded = TRUE;
        pl_log("the window loaded %s", webkit_web_view_get_uri(wv));
        if (g.selftest) {
            /* The page is a single-page app: it renders after its own bootstrap call. Give it a few
             * tries, then probe whatever is there. */
            g_timeout_add(700, pl_selftest_poll, wv);
        }
    }
}

/* ---------------------------------------------------------------------------------------------
 * Starting the server
 * -------------------------------------------------------------------------------------------*/

static char *pl_tool_beside(const char *name)
{
    char *self = g_file_read_link("/proc/self/exe", NULL);
    if (!self) return g_strdup(name);
    char *dir = g_path_get_dirname(self);
    char *cand = g_build_filename(dir, name, NULL);
    g_free(dir);
    g_free(self);
    if (g_file_test(cand, G_FILE_TEST_IS_EXECUTABLE)) return cand;
    g_free(cand);
    return g_strdup(name);
}

/* Read one line from the server's stdout with a deadline. The server prints exactly one JSON line
 * when it has a socket (or when it cannot get one), and that line is the whole handshake. */
static char *pl_read_line(int fd, int timeout_ms)
{
    GString *line = g_string_new(NULL);
    int waited = 0;
    while (waited < timeout_ms) {
        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(fd, &rfds);
        struct timeval tv = { 0, 100 * 1000 };
        int r = select(fd + 1, &rfds, NULL, NULL, &tv);
        if (r > 0) {
            char c;
            ssize_t n = read(fd, &c, 1);
            if (n == 0) break;
            if (n < 0) {
                if (errno == EAGAIN || errno == EINTR) continue;
                break;
            }
            if (c == '\n') return g_string_free(line, FALSE);
            g_string_append_c(line, c);
        } else {
            waited += 100;
        }
    }
    if (line->len == 0) {
        g_string_free(line, TRUE);
        return NULL;
    }
    return g_string_free(line, FALSE);
}

/* Drain the rest of the server's output into the log, so nothing it says is lost after the
 * handshake line (its refusals and its own words are exactly what a person needs). */
static gboolean pl_drain(gpointer data)
{
    int fd = (int)(gintptr)data;
    char buf[4096];
    ssize_t n;
    while ((n = read(fd, buf, sizeof(buf) - 1)) > 0) {
        buf[n] = 0;
        if (g.daemon_log) {
            FILE *f = fopen(g.daemon_log, "a");
            if (f) { fputs(buf, f); fclose(f); }
        }
    }
    if (n == 0) {
        /* The end of the server's own output means the server is gone — it was stopped from outside
         * (the CLI, or another window), or it crashed. A window whose server has left shows a page
         * that cannot do anything, so it closes and says why instead of looking alive. */
        pl_log("the interface server has left (its output ended); closing the window");
        g.server_pid = 0;
        if (!g.quitting) {
            g.quitting = TRUE;
            gtk_main_quit();
        }
        return FALSE;
    }
    return TRUE;
}

static gboolean pl_start_server(void)
{
    char *argv[16];
    int i = 0;
    argv[i++] = g.ui_bin;
    if (g.archive) { argv[i++] = "--archive"; argv[i++] = g.archive; }
    argv[i++] = "--pl"; argv[i++] = g.core_bin;
    char portbuf[16], rangebuf[16];
    snprintf(portbuf, sizeof portbuf, "%d", g.port);
    snprintf(rangebuf, sizeof rangebuf, "%d", g.port_range);
    argv[i++] = "--port"; argv[i++] = portbuf;
    argv[i++] = "--port-range"; argv[i++] = rangebuf;
    argv[i++] = "--native";
    argv[i++] = "--log-dir"; argv[i++] = g.app_home;
    if (g.lang) { argv[i++] = "--lang"; argv[i++] = g.lang; }
    argv[i] = NULL;

    int out_fd = -1;
    GError *err = NULL;
    if (!g_spawn_async_with_pipes(NULL, argv, NULL, G_SPAWN_DO_NOT_REAP_CHILD | G_SPAWN_SEARCH_PATH,
                                 NULL, NULL, (GPid *)&g.server_pid, NULL, &out_fd, NULL, &err)) {
        pl_log("cannot start %s: %s", g.ui_bin, err ? err->message : "?");
        g_clear_error(&err);
        return FALSE;
    }
    g.server_out = out_fd;

    char *line = pl_read_line(out_fd, 20000);
    if (!line) {
        pl_log("the interface server said nothing within 20 s");
        return FALSE;
    }
    JsonParser *p = json_parser_new();
    if (!json_parser_load_from_data(p, line, -1, NULL)) {
        pl_log("the interface server's first line is not JSON: %s", line);
        g_object_unref(p);
        g_free(line);
        return FALSE;
    }
    JsonObject *o = jobj(json_parser_get_root(p));
    /* The server's own line, kept as it was said: it names the port, the way the socket was
     * obtained, the core it will drive and the token the window must present. A person reading
     * daemon.log after a problem needs exactly this line, and so does tools/linux_shell_check.py. */
    pl_log("server said: %s", line);
    gboolean ready = json_object_get_boolean_member_with_default(o, "ready", FALSE);
    if (ready) {
        g.url = jstr(o, "url");
        g.token = jstr(o, "token");
        /* The port the server actually got, not the one this shell asked for: the ladder may have
         * moved along, and the difference is what makes an app talk to somebody else's socket. */
        g.port = (int)jint(o, "port", g.port);
        char *how = jstr(o, "how");
        char *core = jstr(o, "core");
        char *dl = jstr(o, "daemonLog");
        if (dl) { g_free(g.daemon_log); g.daemon_log = dl; }
        pl_log("the interface server is ready on port %d (%s), core %s", g.port,
               how ? how : "?", core ? core : "?");
        g_free(how);
        g_free(core);
    } else {
        JsonObject *e = jobj(json_object_get_member(o, "error"));
        char *advice = e ? jstr(e, "advice") : NULL;
        char *reason = e ? jstr(e, "reason") : NULL;
        pl_log("the server could not open a socket: %s", reason ? reason : "?");
        /* The window shows it, in the server's words, instead of a sentence of this shell's own. */
        char *msg = g_strdup_printf(
            "Project Life could not open its local port.\n\n%s\n\n%s\n\n"
            "The full reason is in %s.",
            reason ? reason : "", advice ? advice : "", g.daemon_log ? g.daemon_log : "daemon.log");
        GtkWidget *d = gtk_message_dialog_new(NULL, GTK_DIALOG_MODAL, GTK_MESSAGE_ERROR, GTK_BUTTONS_CLOSE,
                                              "%s", "The interface server could not start");
        gtk_message_dialog_format_secondary_text(GTK_MESSAGE_DIALOG(d), "%s", msg);
        gtk_dialog_run(GTK_DIALOG(d));
        gtk_widget_destroy(d);
        g_free(msg);
        g_free(advice);
        g_free(reason);
        g_object_unref(p);
        g_free(line);
        return FALSE;
    }
    g_object_unref(p);
    g_free(line);
    /* Keep the rest of the child's words. */
    int flags = fcntl(out_fd, F_GETFL, 0);
    fcntl(out_fd, F_SETFL, flags | O_NONBLOCK);
    g.drain_id = g_timeout_add(200, pl_drain, (gpointer)(gintptr)out_fd);
    return TRUE;
}

/* ---------------------------------------------------------------------------------------------
 * main
 * -------------------------------------------------------------------------------------------*/

static void pl_tray(void)
{
#ifdef PL_HAVE_APPINDICATOR
    if (g.no_tray) return;
    g.tray_menu = gtk_menu_new();
    g.indicator = app_indicator_new("project-life", "security-high",
                                    APP_INDICATOR_CATEGORY_APPLICATION_STATUS);
    if (!g.indicator) {
        pl_log("no tray icon could be created on this desktop; the window and the menu bar remain");
        g.tray_created = FALSE;
        return;
    }
    g.tray_created = TRUE;
    app_indicator_set_status(g.indicator, APP_INDICATOR_STATUS_ACTIVE);
    app_indicator_set_menu(g.indicator, GTK_MENU(g.tray_menu));
    app_indicator_set_icon_full(g.indicator, "security-high", SHELL_NAME);
#else
    g.tray_created = FALSE;
    pl_log("this build has no tray support (libayatana-appindicator3 was not found when it was built)");
#endif
}

static void pl_own_menu_front(void)
{
    /* The entries that belong to the shell itself, on both fronts: the state, the window, the
     * observation, the diagnosis, and leaving completely. Everything else is the server's list, and
     * this is called from the rebuild so the shell's own lines are never the ones that got cleared.
     */
    GtkWidget *mi;

    if (!g.tray_menu) return;
    mi = gtk_menu_item_new_with_label("Project Life");
    gtk_widget_set_sensitive(mi, FALSE);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), mi);
    g.tray_state_item = gtk_menu_item_new_with_label(g.state_label ? g.state_label : "…");
    gtk_widget_set_sensitive(g.tray_state_item, FALSE);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), g.tray_state_item);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), gtk_separator_menu_item_new());

    mi = gtk_menu_item_new_with_label("Open window");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_open_window), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), mi);

    mi = gtk_menu_item_new_with_label(g.observing ? "Stop observation" : "Start observation");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_toggle_watch), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), mi);

    mi = gtk_menu_item_new_with_label("Protection status…");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_status_dialog), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), mi);

    mi = gtk_menu_item_new_with_label("Network diagnosis…");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_diagnose_dialog), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), mi);

    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), gtk_separator_menu_item_new());
    mi = gtk_menu_item_new_with_label("Quit completely…");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_quit_completely), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.tray_menu), mi);
}

static void pl_build_window(void)
{
    g.window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(g.window), SHELL_NAME);
    gtk_window_set_default_size(GTK_WINDOW(g.window), 1100, 720);
    g_signal_connect(g.window, "delete-event", G_CALLBACK(pl_on_delete), NULL);

    g.box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 0);
    gtk_container_add(GTK_CONTAINER(g.window), g.box);

    g.menubar = gtk_menu_bar_new();
    gtk_box_pack_start(GTK_BOX(g.box), g.menubar, FALSE, FALSE, 0);
    /* The shell's own front, as a normal menu inside the bar: the same six things the tray carries,
     * so a desktop without a tray is not a desktop without a way out. */
    GtkWidget *app_menu = gtk_menu_new();
    GtkWidget *mi;
    mi = gtk_menu_item_new_with_label("Open window");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_open_window), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(app_menu), mi);
    mi = gtk_menu_item_new_with_label("Start / stop observation");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_toggle_watch), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(app_menu), mi);
    mi = gtk_menu_item_new_with_label("Protection status…");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_status_dialog), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(app_menu), mi);
    mi = gtk_menu_item_new_with_label("Network diagnosis…");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_diagnose_dialog), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(app_menu), mi);
    gtk_menu_shell_append(GTK_MENU_SHELL(app_menu), gtk_separator_menu_item_new());
    mi = gtk_menu_item_new_with_label("Quit completely…");
    g_signal_connect_swapped(mi, "activate", G_CALLBACK(pl_quit_completely), NULL);
    gtk_menu_shell_append(GTK_MENU_SHELL(app_menu), mi);
    gtk_widget_show_all(app_menu);
    GtkWidget *app_top = gtk_menu_item_new_with_label(SHELL_NAME);
    gtk_menu_item_set_submenu(GTK_MENU_ITEM(app_top), app_menu);
    gtk_menu_shell_append(GTK_MENU_SHELL(g.menubar), app_top);

    WebKitWebContext *ctx = webkit_web_context_get_default();
    webkit_web_context_register_uri_scheme(ctx, "pl", pl_scheme_request, NULL, NULL);

    g.wv = WEBKIT_WEB_VIEW(webkit_web_view_new_with_context(ctx));
    gtk_box_pack_start(GTK_BOX(g.box), GTK_WIDGET(g.wv), TRUE, TRUE, 0);
    g_signal_connect(g.wv, "load-changed", G_CALLBACK(pl_on_load_changed), NULL);

    g.status = gtk_label_new("");
    gtk_label_set_xalign(GTK_LABEL(g.status), 0.0);
    gtk_widget_set_margin_start(g.status, 8);
    gtk_widget_set_margin_end(g.status, 8);
    gtk_widget_set_margin_top(g.status, 4);
    gtk_widget_set_margin_bottom(g.status, 4);
    gtk_box_pack_start(GTK_BOX(g.box), g.status, FALSE, FALSE, 0);

    gtk_widget_show_all(g.window);
}

static void pl_usage(void)
{
    printf("Project Life %s (Linux shell)\n\n", SHELL_VERSION);
    printf("  projectlife-app [--archive DIR] [--pl PATH] [--ui PATH] [--app-home DIR]\n");
    printf("                  [--port N] [--port-range N] [--lang en|ru] [--no-tray]\n");
    printf("                  [--selftest [--screenshot FILE]] [--version]\n\n");
    printf("The window talks to `projectlife-ui`, which drives `projectlife`: this program stores\n");
    printf("nothing itself.\n\n");
    printf("%s\n", PL_WHAT_IT_IS);
    printf("By %s <%s> · %s licence\n", PL_AUTHOR, PL_AUTHOR_EMAIL, PL_LICENCE);
}

int main(int argc, char **argv)
{
    g.port = 7717;
    g.port_range = 10;
    g.core_bin = NULL;
    g.ui_bin = NULL;
    g.app_home = NULL;
    g.lang = NULL;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--pl") == 0 && i + 1 < argc) g.core_bin = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--ui") == 0 && i + 1 < argc) g.ui_bin = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--app-home") == 0 && i + 1 < argc) g.app_home = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--log-dir") == 0 && i + 1 < argc) g.app_home = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--archive") == 0 && i + 1 < argc) g.archive = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--port") == 0 && i + 1 < argc) g.port = atoi(argv[++i]);
        else if (strcmp(argv[i], "--port-range") == 0 && i + 1 < argc) g.port_range = atoi(argv[++i]);
        else if (strcmp(argv[i], "--lang") == 0 && i + 1 < argc) g.lang = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--no-tray") == 0) g.no_tray = TRUE;
        else if (strcmp(argv[i], "--selftest") == 0) g.selftest = TRUE;
        else if (strcmp(argv[i], "--selftest-close") == 0) { g.selftest = TRUE; g.selftest_close = TRUE; g.suppress_notice = TRUE; }
        else if (strcmp(argv[i], "--screenshot") == 0 && i + 1 < argc) g.screenshot = g_strdup(argv[++i]);
        else if (strcmp(argv[i], "--version") == 0) {
            printf("Project Life shell %s (Linux, GTK %s, WebKitGTK %s, appindicator %s)\n",
                   SHELL_VERSION, PL_GTK_VERSION, PL_WEBKIT_VERSION, PL_APPINDICATOR_VERSION);
            printf("%s\n", PL_WHAT_IT_IS);
            printf("By %s <%s> · %s licence\n", PL_AUTHOR, PL_AUTHOR_EMAIL, PL_LICENCE);
            printf("%s\n", PL_COPYRIGHT);
            return 0;
        } else if (strcmp(argv[i], "--help") == 0) {
            pl_usage();
            return 0;
        }
    }
    if (getenv("PROJECTLIFE_SHELL_NO_TRAY")) g.no_tray = TRUE;
    if (!g.app_home) g.app_home = pl_default_app_home();
    g_mkdir_with_parents(g.app_home, 0700);
    g.daemon_log = g_build_filename(g.app_home, "daemon.log", NULL);
    if (!g.ui_bin) g.ui_bin = pl_tool_beside("projectlife-ui");
    if (!g.core_bin) g.core_bin = pl_tool_beside("projectlife");

    /* Single instance, the honest way: a second launch cannot raise the first window, but it can ask
     * it to. Whoever is running polls `show.request`; this one writes it and leaves. */
    {
        char *shell_file = g_build_filename(g.app_home, "shell.json", NULL);
        char *text = NULL;
        if (g_file_get_contents(shell_file, &text, NULL, NULL)) {
            JsonParser *p = json_parser_new();
            if (json_parser_load_from_data(p, text, -1, NULL)) {
                JsonObject *o = jobj(json_parser_get_root(p));
                gint64 pid = json_object_get_int_member_with_default(o, "pid", 0);
                if (pid > 0 && kill((pid_t)pid, 0) == 0) {
                    char *req = g_build_filename(g.app_home, "show.request", NULL);
                    g_file_set_contents(req, "show\n", -1, NULL);
                    printf("Project Life is already running (pid %lld); its window has been asked to "
                           "come to the front.\n", (long long)pid);
                    g_free(req);
                    g_object_unref(p);
                    g_free(text);
                    g_free(shell_file);
                    return 0;
                }
                /* The handshake file outlived its process: a shell that was killed left a server
                 * behind, and a second server beside it would be a process nobody owns. It is told
                 * to leave first (which stops the daemon it started), so the next launch is a clean
                 * one — and the line says so, rather than silently cleaning up. */
                if (pid > 0) {
                    gint64 port = json_object_get_int_member_with_default(o, "port", 0);
                    if (port > 0) {
                        int fd = socket(AF_INET, SOCK_STREAM, 0);
                        if (fd >= 0) {
                            struct sockaddr_in a;
                            memset(&a, 0, sizeof a);
                            a.sin_family = AF_INET;
                            a.sin_port = htons((uint16_t)port);
                            a.sin_addr.s_addr = htonl(0x7f000001);
                            struct timeval tv = { 1, 0 };
                            setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
                            if (connect(fd, (struct sockaddr *)&a, sizeof a) == 0) {
                                const char *req = "POST /api/shutdown HTTP/1.1\r\nHost: 127.0.0.1\r\n"
                                                  "Content-Length: 2\r\nConnection: close\r\n\r\n{}";
                                ssize_t w = write(fd, req, strlen(req));
                                (void)w;
                                printf("a server from a previous run was still listening on port %lld "
                                       "with no window; it has been asked to leave.\n", (long long)port);
                            }
                            close(fd);
                        }
                    }
                }
            }
            g_object_unref(p);
            g_free(text);
        }
        char *mine = g_strdup_printf("{\"pid\":%d,\"port\":%d,\"version\":\"%s\"}\n",
                                     (int)getpid(), g.port, SHELL_VERSION);
        g_file_set_contents(shell_file, mine, -1, NULL);
        g_free(mine);
        g_free(shell_file);
    }

    gtk_init(&argc, &argv);
    soup = soup_session_new();
    signal(SIGTERM, pl_on_unix_signal);
    signal(SIGINT, pl_on_unix_signal);

    pl_log("shell %s starting (pid %d), core %s, ui %s", SHELL_VERSION, (int)getpid(),
           g.core_bin, g.ui_bin);
    if (!pl_start_server()) {
        pl_log("no server, no window");
        return 3;
    }

    pl_refresh_state();
    pl_build_window();
    pl_tray();
    pl_own_menu_front();
    pl_rebuild_menus();
    pl_update_tray();

    gtk_widget_show_all(g.window);
    if (g.selftest) gtk_widget_show(g.window);
    webkit_web_view_load_uri(g.wv, g.url);

    g.poll_id = g_timeout_add(1000, pl_on_poll, NULL);
    if (g.selftest) {
        /* A ceiling, so a run that never loads fails in seconds instead of hanging a test suite. */
        g.selftest_timeout = g_timeout_add(25000, (GSourceFunc)pl_selftest_finish, NULL);
    }
    gtk_main();

    /* Leaving the window is not leaving the protection: the server keeps running only if it was not
     * asked to stop. When the shell exits without a quit, the server is told to leave too — a server
     * with no window is a process nobody can reach. */
    if (!g.quitting && g.server_pid > 0) {
        char *answer = pl_http("POST", "shutdown", "{}");
        if (answer) { pl_log("shutdown on exit -> %s", answer); g_free(answer); }
        for (int i = 0; i < 50; i++) {
            int st = 0;
            pid_t w = waitpid(g.server_pid, &st, WNOHANG);
            if (w == (pid_t)g.server_pid || w < 0) { g.server_pid = 0; break; }
            g_usleep(100 * 1000);
        }
        if (g.server_pid > 0) kill(g.server_pid, SIGTERM);
    }
    pl_log("shell leaving");
    return 0;
}
