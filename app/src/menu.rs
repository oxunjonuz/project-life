//! The menu: one list, drawn in three places.
//!
//! The owner asked (round 299) for a full menu over the core's user-facing functions. The risk in
//! that request is the usual one: a menu tends to become a list of things that *look* like
//! features. This module avoids it structurally rather than by promise:
//!
//! * the list is **data**, and every entry names the core command line it stands for (`core`) and,
//!   for the entries that run something, the **exact argument vector** (`argv`);
//! * the window draws its menu bar and its command palette from this list — no hand-written second
//!   copy can drift from it;
//! * the macOS menu bar is built by the shell from the same JSON, so the system menu cannot offer
//!   something the app does not know how to do;
//! * the server executes an entry only if its id is in this table and only with the argv stored
//!   here. Nothing else can be run through the endpoint — the menu is not a command runner.
//!
//! Kinds:
//!
//! | kind | what happens |
//! |---|---|
//! | `View` | the window opens one of its own views (the view then reads the core as it always did) |
//! | `Page` | the window runs one of its own flows (the wizard, the export picker) by id |
//! | `Run` | the server runs `argv` with the core and shows the core's own output |
//! | `Ask` | the window asks one value (a folder, a path, a moment, a label), then a `Run` |
//! | `Confirm` | like `Run`, but the server refuses unless the window has already asked |
//! | `Info` | nothing is run: the window shows the exact core command and why it stays there |
//!
//! Placeholders inside `argv`: `{project}` is the project open in the window, `{archive}` the
//! archive root, `{input}` the single value an `Ask` collected. Each is one argument — a value is
//! never split, and a value that begins with `-` is refused before it can be read as a flag.

use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    View,
    Page,
    Run,
    Ask,
    Confirm,
    Info,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::View => "view",
            Kind::Page => "page",
            Kind::Run => "run",
            Kind::Ask => "ask",
            Kind::Confirm => "confirm",
            Kind::Info => "info",
        }
    }
}

/// Which front ends may show an entry. A shell-only entry is one the window genuinely cannot do
/// (quitting the app is the shell's act, not the page's).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Both,
    Shell,
}

impl Place {
    pub fn id(self) -> &'static str {
        match self {
            Place::Both => "both",
            Place::Shell => "shell",
        }
    }
    pub fn on_page(self) -> bool {
        self != Place::Shell
    }
}

pub struct Item {
    pub id: &'static str,
    pub group: &'static str,
    pub en: &'static str,
    pub ru: &'static str,
    pub kind: Kind,
    pub place: Place,
    /// The core command line this entry stands for, written the way a person would type it. The
    /// menu is checked against the core's own command list through this field.
    pub core: &'static str,
    /// The exact argument vector handed to the core, without the archive flag (the server pins
    /// `--archive`). Empty for entries the server does not run.
    pub argv: &'static [&'static str],
    /// For `Ask`: what the window must collect first ("", "folder", "text", "path", "moment", "label").
    pub input: &'static str,
    /// For `View`: the view id inside the window.
    pub view: &'static str,
    /// One line for the palette and the tooltip: what this does, and what it costs.
    pub note: &'static str,
    /// A chord the window honours, e.g. "meta+K". Nothing here is a chord the page does not have.
    pub key: &'static str,
    /// "" or "project": the entry needs a project open. The window disables it, with the reason,
    /// rather than letting it fail after the click.
    pub needs: &'static str,
    /// A named action the server performs itself (only "diagnose" today).
    pub special: &'static str,
}

pub struct Group {
    pub id: &'static str,
    pub en: &'static str,
    pub ru: &'static str,
}

pub const GROUPS: &[Group] = &[
    Group { id: "app", en: "Project Life", ru: "Project Life" },
    Group { id: "file", en: "File", ru: "Файл" },
    Group { id: "protect", en: "Protection", ru: "Защита" },
    Group { id: "history", en: "History", ru: "История" },
    Group { id: "restore", en: "Restore", ru: "Восстановление" },
    Group { id: "archive", en: "Archive", ru: "Хранилище" },
    Group { id: "tools", en: "Tools", ru: "Инструменты" },
    Group { id: "help", en: "Help", ru: "Справка" },
];

macro_rules! it {
    ($id:expr, $group:expr, $en:expr, $ru:expr, $kind:expr, $place:expr, $core:expr, $argv:expr,
     $input:expr, $view:expr, $note:expr, $key:expr, $needs:expr, $special:expr) => {
        Item {
            id: $id,
            group: $group,
            en: $en,
            ru: $ru,
            kind: $kind,
            place: $place,
            core: $core,
            argv: $argv,
            input: $input,
            view: $view,
            note: $note,
            key: $key,
            needs: $needs,
            special: $special,
        }
    };
}

use Kind::{Ask, Confirm, Info, Page, Run, View};
use Place::{Both, Shell};

pub const ITEMS: &[Item] = &[
    // ---------------------------------------------------------------- Project Life
    it!("app.about", "app", "About Project Life…", "О программе Project Life…", View, Both,
        "pl version", &[], "", "about",
        "the version, the build this window is, and the command that checks it from outside", "", "", ""),
    it!("app.protection", "app", "Protection status…", "Состояние защиты…", View, Both,
        "pl heartbeat-check --json", &[], "", "status",
        "whether versions are being written right now, and the reason when they are not", "", "", ""),
    it!("app.settings", "app", "Settings…", "Настройки…", View, Both,
        "pl config get|set <key> [value]", &[], "", "settings",
        "the interval, notifications, the language, and the core's own keys", "meta+,", "", ""),
    it!("app.diagnostics", "app", "Diagnostics…", "Диагностика…", View, Both,
        "pl doctor", &[], "", "diagnostics",
        "the observation log, the checks and the trigger the core reports", "", "", ""),
    it!("app.network", "app", "Network diagnosis…", "Диагностика сети…", Run, Both,
        "projectlife-ui --diagnose", &[], "", "",
        "the server writes diagnose.txt and prints its verdict: where it can and cannot listen", "", "", "diagnose"),
    it!("app.quit", "app", "Quit Project Life completely…", "Выйти из Project Life полностью…", Confirm, Shell,
        "— the app stops observation", &[], "", "",
        "stops observation and closes the app; saved versions stay in the archive", "meta+Q", "", ""),

    // ---------------------------------------------------------------- File
    it!("file.add", "file", "Add a folder to protect…", "Добавить папку под защиту…", View, Both,
        "pl add <path> [--preset …] [--yes]", &[], "", "add",
        "the four-step wizard: choose a folder, see what would be saved, name it, start", "meta+N", "", ""),
    it!("file.detect", "file", "See what a folder holds…", "Посмотреть, что в папке…", Ask, Both,
        "pl detect <path> --json", &["detect", "{input}", "--json"], "folder", "",
        "reads names and extensions only — no file is opened — and reports the profile it would use", "", "", ""),
    it!("file.archive-new", "file", "Create the archive…", "Создать хранилище…", Page, Both,
        "pl init-archive <path>", &[], "", "",
        "choose a disk for the archive; only changed contents are stored after the first copy", "", "", ""),
    it!("file.archive-use", "file", "Use an existing archive…", "Открыть существующее хранилище…", Page, Both,
        "pl init-archive <path> (existing)", &[], "", "",
        "point the app at an archive that already exists instead of creating one", "", "", ""),
    it!("file.export", "file", "Export the history…", "Экспортировать историю…", Page, Both,
        "pl export <project> --out <dir> [--pack]", &[], "", "",
        "writes the project's range to a folder you choose, with a manifest of every file", "meta+E", "project", ""),
    it!("file.import", "file", "Import an export…", "Импортировать экспорт…", View, Both,
        "pl import <export-dir> [--new NAME]", &[], "", "import",
        "brings an exported range back in as its own project", "", "", ""),
    it!("file.reveal", "file", "Show the archive in the file manager", "Показать хранилище в Finder", Page, Both,
        "pl open --archive --launch", &[], "", "",
        "reveals the archive folder; the window never draws its own file browser", "", "", ""),
    it!("file.close", "file", "Close the window", "Закрыть окно", Page, Shell,
        "— observation continues", &[], "", "",
        "closing the window does not stop observation; quitting does", "meta+W", "", ""),

    // ---------------------------------------------------------------- Protection
    it!("protect.start", "protect", "Start observation", "Включить наблюдение", Page, Both,
        "pl daemon start", &[], "", "",
        "starts the app's own observation process; the shield shows whether versions are written", "", "", ""),
    it!("protect.stop", "protect", "Stop observation", "Остановить наблюдение", Page, Both,
        "pl daemon stop", &[], "", "",
        "stops observation; changes made while it is stopped are not recorded", "", "", ""),
    it!("protect.run-once", "protect", "Observe once now", "Сделать один проход сейчас", Run, Both,
        "pl scan-once <project>", &["scan-once", "{project}"], "", "",
        "one full pass over the project, right now, outside the interval", "meta+R", "project", ""),
    it!("protect.pause", "protect", "Pause this project", "Приостановить проект", Run, Both,
        "pl pause <project>", &["pause", "{project}"], "", "",
        "keeps the history but records nothing new for this project until it is resumed", "", "project", ""),
    it!("protect.resume", "protect", "Resume this project", "Возобновить проект", Run, Both,
        "pl resume <project>", &["resume", "{project}"], "", "",
        "starts recording this project again", "", "project", ""),
    it!("protect.heartbeat", "protect", "Is anything being observed?", "Наблюдает ли что-нибудь?", Run, Both,
        "pl heartbeat-check", &["heartbeat-check"], "", "",
        "the core's own answer, with the age of the last observation", "", "", ""),
    it!("protect.health", "protect", "Health check", "Проверка состояния", Run, Both,
        "pl healthcheck", &["healthcheck"], "", "",
        "one exit code for a scheduler: heartbeat, mode, unfinished restores, doctor's findings", "", "", ""),
    it!("protect.notify", "protect", "Send a test notification", "Отправить пробное уведомление", Run, Both,
        "pl notify test", &["notify", "test"], "", "",
        "checks that notifications reach you on this machine", "", "", ""),

    // ---------------------------------------------------------------- History
    it!("history.open", "history", "History & restore", "История и восстановление", View, Both,
        "pl log <project>", &[], "", "project",
        "the moments with changes, the tree at a moment, and restore from there", "meta+1", "", ""),
    it!("history.recent", "history", "Recent changes", "Последние изменения", Run, Both,
        "pl recent --limit 20", &["recent", "--limit", "20"], "", "",
        "what changed lately, across every project", "meta+2", "", ""),
    it!("history.search", "history", "Search the history…", "Искать в истории…", Ask, Both,
        "pl log <project> --grep <text>", &["log", "{project}", "--grep", "{input}"], "text", "",
        "finds moments by what happened in them, not by opening the files", "", "project", ""),
    it!("history.content", "history", "Search inside file contents…", "Искать внутри содержимого…", Ask, Both,
        "pl log <project> --content <text>", &["log", "{project}", "--content", "{input}"], "text", "",
        "looks for the text in the stored versions themselves — slower, and it reads the archive", "", "project", ""),
    it!("history.why", "history", "Why did this file change?…", "Почему изменился файл?…", Ask, Both,
        "pl why <project> <path>", &["why", "{project}", "{input}"], "path", "",
        "the decision line for one file: which moment changed it and what the core knows about it", "", "project", ""),
    it!("history.blame", "history", "Which moment touched each line?…", "Какой момент тронул строку?…", Ask, Both,
        "pl blame <project> <path>", &["blame", "{project}", "{input}"], "path", "",
        "per-line history for one file", "", "project", ""),
    it!("history.since", "history", "What changed since…?", "Что изменилось с…?", Ask, Both,
        "pl since <project> --at <moment>", &["since", "{project}", "--at", "{input}"], "moment", "",
        "everything the archive recorded between that moment and now", "", "project", ""),
    it!("history.lastgood", "history", "The last good moment", "Последний целый момент", Run, Both,
        "pl last-good <project>", &["last-good", "{project}"], "", "",
        "the newest moment whose files are all present and readable", "", "project", ""),
    it!("history.snap", "history", "Snap a marker now…", "Поставить метку сейчас…", Ask, Both,
        "pl snap <project> <label>", &["snap", "{project}", "{input}"], "label", "",
        "one word for this moment, to come back to it by name", "", "project", ""),
    it!("history.note", "history", "Write a note…", "Написать заметку…", Ask, Both,
        "pl note <project> <text>", &["note", "{project}", "{input}"], "text", "",
        "a line stored with the project, so the reason is not lost", "", "project", ""),
    it!("history.mark", "history", "Mark this moment…", "Отметить этот момент…", Ask, Both,
        "pl mark <project> <label>", &["mark", "{project}", "{input}"], "label", "",
        "names the latest moment so it can be restored by that name", "", "project", ""),
    it!("history.suggest", "history", "What needs attention?", "Что требует внимания?", Run, Both,
        "pl suggest", &["suggest"], "", "",
        "the core's own list of the things worth doing next", "", "", ""),
    it!("history.status", "history", "What is watched right now", "Что наблюдается сейчас", Run, Both,
        "pl status --compact", &["status", "--compact"], "", "",
        "every project with its state, its version count and how long ago it was observed", "", "", ""),
    it!("history.list", "history", "Projects by risk", "Проекты по риску", Run, Both,
        "pl list --sort risk", &["list", "--sort", "risk"], "", "",
        "the same projects, ordered by what is closest to being unprotected", "", "", ""),
    it!("history.what-happened", "history", "What happened?", "Что произошло?", View, Both,
        "pl notifications", &[], "", "notifications",
        "the ledger of everything the program has told you: mass changes, stopped writing, errors", "meta+3", "", ""),
    it!("history.ledger", "history", "The last twenty messages, as text", "Последние двадцать сообщений, текстом",
        Run, Both,
        "pl notifications --limit 20", &["notifications", "--limit", "20"], "", "",
        "the same ledger in a result card; `pl notifications --json` is what the screen reads", "", "", ""),

    // ---------------------------------------------------------------- Restore
    it!("restore.preview", "restore", "Preview a restore at…", "Предпросмотр восстановления на…", Ask, Both,
        "pl restore <project> --at <moment> --preview", &["restore", "{project}", "--at", "{input}", "--preview"],
        "moment", "",
        "lists what a restore would write, and writes nothing", "", "project", ""),
    it!("restore.drill", "restore", "Rehearse a restore", "Прорепетировать восстановление", Page, Both,
        "pl drill <project>", &[], "", "",
        "restores a past moment into a temporary folder and compares it byte by byte", "", "project", ""),
    it!("restore.recover", "restore", "Finish an interrupted prune", "Завершить прерванную чистку", Confirm, Both,
        "pl recover <project>", &["recover", "{project}"], "", "",
        "completes or rolls back a prune that was killed halfway; it asks first", "", "project", ""),
    it!("restore.undo", "restore", "The command that would undo the last write",
        "Команда отмены последней записи", Run, Both,
        "pl undo <project>", &["undo", "{project}"], "", "",
        "prints the command; the menu does not perform it, and neither does the core", "", "project", ""),
    it!("restore.to-folder", "restore", "Restore a moment into a folder…", "Восстановить момент в папку…",
        View, Both,
        "pl restore <project> --at <moment> --to <dir>", &[], "", "project",
        "pick a moment in History, then restore it into a folder you choose", "", "project", ""),
    it!("restore.missing", "restore", "Put back only what is missing…", "Вернуть только недостающее…", Page, Both,
        "pl restore <project> --at <moment> --missing --into-project", &[], "", "",
        "repair after a deletion: creates the files that are gone and touches nothing else", "", "project", ""),

    // ---------------------------------------------------------------- Archive
    it!("archive.integrity", "archive", "Integrity", "Целостность", View, Both,
        "pl check", &[], "", "integrity",
        "checks, the audit, quarantine and dangling bytes, with the core's own numbers", "", "", ""),
    it!("archive.check", "archive", "Check the archive now", "Проверить хранилище сейчас", Run, Both,
        "pl check <project>", &["check", "{project}"], "", "",
        "every event's blob is present, readable and the right size", "", "project", ""),
    it!("archive.check-deep", "archive", "Deep check (hash every blob)",
        "Глубокая проверка (хеш каждого блоба)", Run, Both,
        "pl check <project> --deep", &["check", "{project}", "--deep"], "", "",
        "reads every stored byte and compares its hash with the record — slow and worth it", "", "project", ""),
    it!("archive.doctor", "archive", "Checks (doctor)", "Проверки (doctor)", Run, Both,
        "pl doctor --json", &["doctor", "--json"], "", "",
        "locks, heartbeats, unfinished restores and the disk, as data", "", "", ""),
    it!("archive.audit", "archive", "Audit the archive (read only)", "Аудит хранилища (только чтение)", Run, Both,
        "pl audit-archive", &["audit-archive"], "", "",
        "re-hashes the archive and compares it with the last recorded digest; writes nothing", "", "", ""),
    it!("archive.size", "archive", "The largest things in the archive", "Крупнейшее в хранилище", Run, Both,
        "pl size --top 10", &["size", "--top", "10"], "", "",
        "where the bytes actually are", "", "", ""),
    it!("archive.gc", "archive", "Dangling bytes (nothing is deleted)", "Висячие байты (ничего не удаляется)",
        Run, Both,
        "pl gc --dry-run", &["gc", "--dry-run"], "", "",
        "shows what no event references; the window never deletes, `check --fix` does, and it asks", "", "", ""),
    it!("archive.quarantine", "archive", "Quarantined blobs", "Карантин блобов", Run, Both,
        "pl quarantine", &["quarantine"], "", "",
        "blobs that failed a check and were set aside instead of being trusted", "", "", ""),
    it!("archive.rebuild-cache", "archive", "Rebuild the state cache", "Перестроить кэш состояния", Confirm, Both,
        "pl rebuild-cache", &["rebuild-cache"], "", "",
        "a repair step: the same cache the program rebuilds by itself when it disagrees with the journal",
        "confirm", "project", ""),
    it!("archive.retention", "archive", "Storage & retention…", "Хранилище и срок хранения…", View, Both,
        "pl retention <project>", &[], "", "storage",
        "sizes, the retention policy, its plan, and who is allowed to apply it", "", "", ""),
    it!("archive.config", "archive", "The core's settings", "Настройки ядра", Run, Both,
        "pl config get", &["config", "get"], "", "",
        "every key the core keeps, with its current value", "", "", ""),

    // ---------------------------------------------------------------- Tools
    it!("tools.presets", "tools", "The protection presets", "Профили защиты", Run, Both,
        "pl presets", &["presets"], "", "",
        "the built-in profiles, with the extensions and markers each one uses", "", "", ""),
    it!("tools.cat", "tools", "Show a file as the archive has it…", "Показать файл, как его хранит архив…",
        Ask, Both,
        "pl cat <project> --path <path>", &["cat", "{project}", "--path", "{input}"], "path", "",
        "the stored bytes of one file, printed rather than written anywhere", "", "project", ""),
    it!("tools.mcp", "tools", "MCP: what another agent may ask", "MCP: что может спросить агент", Run, Both,
        "pl mcp tools", &["mcp", "tools"], "", "",
        "the read-only tools the MCP server registers — the list, without starting anything", "", "", ""),
    it!("tools.version", "tools", "The core's version", "Версия ядра", Run, Both,
        "pl version", &["version"], "", "",
        "the core answers with its own version line", "", "", ""),
    it!("tools.completion", "tools", "Shell completion (terminal)", "Автодополнение в оболочке (терминал)",
        Info, Both,
        "pl completion bash|zsh|fish [--install]", &[], "", "",
        "shell furniture: the window shows the command because installing it changes your shell, not this app",
        "", "", ""),
    it!("tools.prompt", "tools", "A one-line status for a shell prompt", "Строка состояния для промпта",
        Info, Both,
        "pl prompt [--space]", &[], "", "",
        "for a shell prompt; the window has nothing to do with it", "", "", ""),
    it!("tools.coverage", "tools", "What is not in the window, and why", "Чего нет в окне и почему", View, Both,
        "pl help", &[], "", "cli-only",
        "the honest list of what stays in the terminal, each with its reason", "", "", ""),
    it!("tools.palette", "tools", "Every command…", "Все команды…", View, Both,
        "pl help", &[], "", "palette",
        "the searchable list of everything above, with the exact core command behind each entry", "meta+K", "", ""),

    // ---------------------------------------------------------------- Help
    it!("help.guide", "help", "Quick guide: the seven steps", "Краткое руководство: семь шагов", View, Both,
        "docs/APP.md", &[], "", "help",
        "choosing a store, adding a folder, watching, history, restore, export and import", "meta+?", "", ""),
    it!("help.docs", "help", "The documentation folder", "Папка с документацией", Page, Shell,
        "docs/", &[], "", "",
        "opens the documentation that ships inside the app bundle", "", "", ""),
    it!("help.cli-only", "help", "What the menu deliberately leaves out",
        "Что меню сознательно не делает", View, Both,
        "pl help", &[], "", "cli-only",
        "the same list as Tools → What is not in the window, kept here for the reader who looks for it",
        "", "", ""),
];

pub fn find(id: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.id == id)
}

/// The whole menu as the window reads it: groups in order, each with its items.
///
/// `project` is the project open in the window (the shell does not know it and passes none). An
/// entry that needs a project is reported as disabled *with the reason*, so neither front end has
/// to offer a control that would fail after the click.
pub fn json(lang: &str, project: Option<&str>, archive: bool) -> Value {
    build(lang, project, archive, true)
}

/// The shell's copy: everything, including the entries only a native shell can perform.
pub fn json_shell(lang: &str, archive: bool) -> Value {
    build(lang, None, archive, false)
}

fn build(lang: &str, project: Option<&str>, archive: bool, page_only: bool) -> Value {
    let ru = lang == "ru";
    let mut groups: Vec<Value> = Vec::new();
    for g in GROUPS {
        let items: Vec<Value> = ITEMS
            .iter()
            .filter(|i| i.group == g.id && (!page_only || i.place.on_page()))
            .map(|i| item_json(i, ru, project, archive))
            .collect();
        if items.is_empty() {
            continue;
        }
        groups.push(json!({
            "id": g.id,
            "title": if ru { g.ru } else { g.en },
            "items": items,
        }));
    }
    json!({ "lang": if ru { "ru" } else { "en" }, "groups": groups })
}

fn item_json(i: &Item, ru: bool, project: Option<&str>, archive: bool) -> Value {
    let has_project = project.map(|p| !p.is_empty()).unwrap_or(false);
    let (enabled, why) = if i.needs == "project" && !has_project {
        (false, "no project is open — choose one in the sidebar")
    } else if !archive && i.kind != Kind::Info && !i.id.starts_with("file.archive-") && !i.id.starts_with("help.") {
        (false, "no archive is chosen yet — File → Create the archive…")
    } else {
        (true, "")
    };
    json!({
        "id": i.id,
        "group": i.group,
        "label": if ru { i.ru } else { i.en },
        "kind": i.kind.id(),
        "place": i.place.id(),
        "core": i.core,
        "coreRun": core_line(i),
        "input": i.input,
        "view": i.view,
        "note": i.note,
        "key": i.key,
        "needs": i.needs,
        "enabled": enabled,
        "why": why,
    })
}

/// The same menu in a form a program without a JSON library can read: one line per entry,
/// tab-separated, free text last so a space in a label cannot break a reader.
///
/// Why this exists at all: the Windows shell is C, and a hand-written JSON parser in it could not be
/// tested on the machine it runs on (there is no Windows here). A format this simple *is* testable
/// here — `the_plain_menu_and_the_json_menu_say_the_same_thing` compares the two, field by field.
///
/// ```text
/// # project-life-menu 1 <lang>
/// G<TAB><group id><TAB><group title>
/// I<TAB><group id><TAB><item id><TAB><0|1 enabled><TAB><kind><TAB><input or -><TAB><label>
/// ```
pub fn plain(lang: &str, project: Option<&str>, archive: bool, page_only: bool) -> String {
    let v = build(lang, project, archive, page_only);
    let mut out = String::new();
    out.push_str(&format!(
        "# project-life-menu 1 {}\n",
        v.get("lang").and_then(|l| l.as_str()).unwrap_or("en")
    ));
    let groups = v.get("groups").and_then(|g| g.as_array()).cloned().unwrap_or_default();
    for g in groups {
        let gid = g.get("id").and_then(|x| x.as_str()).unwrap_or("");
        out.push_str(&format!(
            "G\t{}\t{}\n",
            gid,
            one_line(g.get("title").and_then(|x| x.as_str()).unwrap_or(""))
        ));
        let items = g.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
        for i in items {
            out.push_str(&format!(
                "I\t{}\t{}\t{}\t{}\t{}\t{}\n",
                gid,
                i.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                if i.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false) { "1" } else { "0" },
                i.get("kind").and_then(|x| x.as_str()).unwrap_or(""),
                match i.get("input") {
                    Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                    _ => "-".to_string(),
                },
                one_line(i.get("label").and_then(|x| x.as_str()).unwrap_or(""))
            ));
        }
    }
    out
}

/// A field of a line-oriented format must not contain the line breaks: a label that did would turn
/// one entry into two, and a reader that trusts the format would then run the second half of a
/// sentence as a command.
fn one_line(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// The command line as the window prints it. For the entries that ask for a value first, the
/// placeholder is shown where the value will go.
pub fn core_line(i: &Item) -> String {
    if i.argv.is_empty() {
        i.core.to_string()
    } else {
        format!("pl {}", i.argv.join(" "))
    }
}

/// Fill `{project}`, `{archive}` and `{input}` in an argument vector.
///
/// Every value is one argument. A value that begins with `-` is refused here rather than handed to
/// the core: `pl log demo --grep --weird` would otherwise let a typed value be read as a flag.
pub fn substitute(
    argv: &[&'static str],
    project: Option<&str>,
    archive: Option<&str>,
    input: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(v) = input {
        if v.contains('\0') {
            return Err("the value contains a NUL byte".into());
        }
        if v.starts_with('-') {
            return Err(format!(
                "a value that begins with a dash would be read as an option by the core: {}",
                v
            ));
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    for a in argv {
        let filled: String = if *a == "{project}" {
            match project {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => return Err("this entry needs a project; none is open".into()),
            }
        } else if *a == "{archive}" {
            match archive {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => return Err("this entry needs an archive; none is chosen".into()),
            }
        } else if *a == "{input}" {
            match input {
                Some(v) if !v.is_empty() => v.to_string(),
                _ => return Err("this entry needs a value first".into()),
            }
        } else {
            (*a).to_string()
        };
        out.push(filled);
    }
    Ok(out)
}

/// Whether a value has to be collected before this entry can run.
pub fn needs_input(i: &Item) -> bool {
    i.kind == Kind::Ask && !i.input.is_empty()
}

/// Whether running this entry can change what is stored. Used by the checks: a `Confirm` entry is
/// the only kind allowed to, and it may only do so after the window has asked.
pub fn writes(i: &Item) -> bool {
    if i.kind == Kind::Confirm {
        return true;
    }
    i.argv.iter().any(|a| {
        matches!(
            *a,
            "scan-once" | "pause" | "resume" | "snap" | "note" | "mark" | "prune" | "import"
                | "init-archive" | "archive-move" | "archive-delete" | "export-and-prune"
        )
    })
}


    /// The two ways of saying the same menu have to agree, because two shells read them: the Linux
    /// shell and the page read the JSON, the Windows shell reads the plain text. A disagreement here
    /// would be a menu entry that exists on one platform and not another.
    #[test]
    fn the_plain_menu_and_the_json_menu_say_the_same_thing() {
        for lang in ["en", "ru"] {
            for shell in [true, false] {
                let j = build(lang, None, true, !shell);
                let text = plain(lang, None, true, !shell);
                let lines: Vec<&str> = text.lines().collect();
                assert!(lines[0].starts_with("# project-life-menu 1 "), "{}", lines[0]);
                let mut groups: Vec<(String, Vec<(String, String, bool)>)> = Vec::new();
                for l in &lines[1..] {
                    let f: Vec<&str> = l.split('\t').collect();
                    match f[0] {
                        "G" => groups.push((f[1].to_string(), Vec::new())),
                        "I" => {
                            let g = groups.last_mut().expect("an item before its group");
                            assert_eq!(g.0, f[1], "the item names its own group");
                            g.1.push((f[2].to_string(), f[6].to_string(), f[3] == "1"));
                        }
                        other => panic!("unknown line type {other}"),
                    }
                }
                let jgroups = j["groups"].as_array().unwrap();
                assert_eq!(groups.len(), jgroups.len(), "group count ({lang}, shell={shell})");
                for (gi, jg) in jgroups.iter().enumerate() {
                    let jid = jg["id"].as_str().unwrap();
                    assert_eq!(groups[gi].0, jid);
                    let jitems = jg["items"].as_array().unwrap();
                    assert_eq!(groups[gi].1.len(), jitems.len(), "{jid}: item count");
                    for (ii, ji) in jitems.iter().enumerate() {
                        let (id, label, enabled) = &groups[gi].1[ii];
                        assert_eq!(id, ji["id"].as_str().unwrap(), "{jid} item {ii}");
                        assert_eq!(label, ji["label"].as_str().unwrap(), "{jid} {id}: label");
                        assert_eq!(*enabled, ji["enabled"].as_bool().unwrap(), "{jid} {id}: enabled");
                    }
                }
            }
        }
    }

    #[test]
    fn a_label_cannot_break_the_line_format() {
        let t = plain("en", None, true, true);
        assert!(t.lines().all(|l| l.starts_with('#') || l.starts_with("G\t") || l.starts_with("I\t")));
        // Seven fields on an item line, and the label field is the last one.
        for l in t.lines().filter(|l| l.starts_with("I\t")) {
            assert_eq!(l.split('\t').count(), 7, "{l}");
        }
    }
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_groups_exist() {
        let mut seen = std::collections::HashSet::new();
        for i in ITEMS {
            assert!(seen.insert(i.id), "duplicate id {}", i.id);
            assert!(GROUPS.iter().any(|g| g.id == i.group), "{} has no group", i.id);
            assert!(!i.en.is_empty() && !i.ru.is_empty(), "{} lacks a label in one language", i.id);
            assert!(!i.core.is_empty(), "{} does not say which core command it stands for", i.id);
            assert!(!i.note.is_empty(), "{} has no note", i.id);
        }
    }

    /// A run entry must carry the argv it claims; a view entry must name a view; an ask entry must
    /// say what it asks for.
    #[test]
    fn every_entry_carries_what_it_needs_to_act() {
        for i in ITEMS {
            match i.kind {
                Kind::Run | Kind::Confirm => {
                    // A shell-owned entry is performed by the shell (quitting, closing the window):
                    // it carries no argv because the core is not asked to do it.
                    if i.place != Place::Shell {
                        assert!(!i.argv.is_empty() || !i.special.is_empty(), "{} would run nothing", i.id);
                    }
                }
                Kind::View => assert!(!i.view.is_empty(), "{} names no view", i.id),
                Kind::Ask => {
                    assert!(!i.input.is_empty(), "{} asks for nothing", i.id);
                    assert!(!i.argv.is_empty(), "{} asks and then runs nothing", i.id);
                }
                Kind::Info => assert!(i.argv.is_empty(), "{} is informational but carries argv", i.id),
                Kind::Page => {}
            }
            if i.place == Place::Shell {
                assert!(
                    matches!(i.kind, Kind::Confirm | Kind::Page),
                    "{} is shell-only but is not something the shell performs",
                    i.id
                );
            }
            if i.kind == Kind::Info {
                assert!(i.place != Place::Shell, "{} is informational and shell-only", i.id);
            }
        }
    }

    #[test]
    fn a_value_is_one_argument_and_a_flag_like_value_is_refused() {
        let argv = &["log", "{project}", "--grep", "{input}"];
        let v = substitute(argv, Some("demo"), None, Some("two words")).unwrap();
        assert_eq!(v, vec!["log", "demo", "--grep", "two words"]);
        assert!(substitute(argv, Some("demo"), None, Some("--weird")).is_err());
        assert!(substitute(argv, Some("demo"), None, None).is_err());
        assert!(substitute(argv, None, None, Some("x")).is_err());
        assert!(substitute(&["gc", "--dry-run"], None, None, None).is_ok());
    }

    /// The window's copy must not contain shell-only entries, and the shell's copy must contain
    /// them — otherwise the two fronts disagree about who performs what.
    #[test]
    fn the_page_copy_and_the_shell_copy_agree_about_who_acts() {
        let page = json("en", Some("demo"), true);
        let shell = json_shell("en", true);
        let ids = |v: &Value| -> Vec<String> {
            v["groups"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["items"].as_array().unwrap().clone())
                .map(|i| i["id"].as_str().unwrap().to_string())
                .collect()
        };
        let p = ids(&page);
        let s = ids(&shell);
        let shell_only = ITEMS.iter().filter(|i| i.place == Place::Shell).count();
        assert_eq!(p.len() + shell_only, s.len(), "page {} shell {}", p.len(), s.len());
        for i in ITEMS.iter().filter(|i| i.place == Place::Shell) {
            assert!(!p.contains(&i.id.to_string()), "{} is shell-only but appears in the page copy", i.id);
            assert!(s.contains(&i.id.to_string()), "{} is missing from the shell copy", i.id);
        }
    }

    #[test]
    fn an_entry_that_needs_a_project_is_disabled_with_a_reason_not_offered() {
        let v = json("en", None, true);
        let items: Vec<Value> = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["items"].as_array().unwrap().clone())
            .collect();
        let needs: Vec<Value> = items.iter().filter(|i| i["needs"] == "project").cloned().collect();
        assert!(!needs.is_empty());
        for i in &needs {
            assert_eq!(i["enabled"], json!(false), "{i}");
            assert!(i["why"].as_str().unwrap().contains("no project"), "{i}");
        }
        // With a project named, and an archive chosen, the same entries are offered.
        let v2 = json("en", Some("demo"), true);
        let items2: Vec<Value> = v2["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["items"].as_array().unwrap().clone())
            .collect();
        assert!(items2
            .iter()
            .any(|i| i["needs"] == "project" && i["enabled"] == json!(true)));
    }

    #[test]
    fn the_labels_change_with_the_language() {
        let en = json("en", None, true);
        let ru = json("ru", None, true);
        assert_eq!(en["groups"][0]["title"], json!("Project Life"));
        assert_ne!(en["groups"][1]["title"], ru["groups"][1]["title"]);
        assert_ne!(
            en["groups"][1]["items"][0]["label"],
            ru["groups"][1]["items"][0]["label"]
        );
    }
}
