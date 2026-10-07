/* Project Life — window logic.
 *
 * Every number on this screen comes from the core program or from the archive itself; nothing is
 * invented here. Where a value would be a guess, the screen says what is not known yet. Buttons
 * that exist do something; there is nothing that only looks like a feature. */

const TOKEN = new URLSearchParams(location.search).get('token') || '';

/* ------------------------------------------------------------------ api */

async function api(path, opts = {}) {
  const headers = Object.assign({ 'X-PL-Token': TOKEN }, opts.headers || {});
  if (opts.body !== undefined) headers['Content-Type'] = 'application/json';
  const res = await fetch('/api/' + path + (path.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(TOKEN), {
    method: opts.method || (opts.body !== undefined ? 'POST' : 'GET'),
    headers,
    body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
  });
  let data = null;
  try { data = await res.json(); } catch (e) { data = { error: 'the app did not answer with JSON' }; }
  if (!res.ok) {
    const msg = (data && data.error) || ('request failed (' + res.status + ')');
    throw new Error(msg);
  }
  return data;
}

/* ------------------------------------------------------------------ text */

const T = {
  en: {
    tagline: 'Local. Private. Yours.',
    chosen_folders: 'Chosen folders',
    nav_dashboard: 'Dashboard',
    nav_restore: 'History & restore',
    nav_diagnostics: 'Diagnostics',
    nav_settings: 'Settings',
    nav_import: 'Import',
    loading: 'Loading…',
    no_folders: 'No folders yet',
    welcome_title: 'Welcome to Project Life',
    welcome_hint: 'A quiet safety net for the work that matters: it keeps the versions of the folders you choose, on a disk you choose.',
    choose_archive: 'Choose where to keep the archive',
    archive_hint: 'The archive should live on a different disk than your work. Only files that change are stored after the first copy.',
    use_this: 'Use this folder',
    create_archive: 'Create the archive here',
    choose_folder: 'Choose folder…',
    add_folder: 'Add a folder',
    add_title: 'Add folder',
    add_hint: 'Protect only a folder you choose — Project Life never watches the whole computer.',
    step_choose: 'Choose folder',
    step_coverage: 'Coverage',
    step_storage: 'Storage',
    step_start: 'Start',
    folder_to_protect: 'Folder to protect',
    selecting: 'Choose a folder on this computer.',
    detected: 'Detected',
    no_convincing: 'No preset was convincing for this folder',
    confidence: 'confidence',
    will_protect: 'files ready to protect',
    estimated_size: 'Estimated archive size',
    not_protected: 'What is not protected',
    adjust: 'Adjust the list of protected types',
    adjust_hint: 'Untick a type to leave it out, tick one to include it. The list is what will be watched.',
    files_here: 'files in this folder',
    back: 'Back',
    continue: 'Continue',
    start_protecting: 'Start protecting',
    storage_title: 'Where the archive lives',
    same_disk: 'The archive is on the same disk as this folder',
    same_disk_hint: 'If that disk is lost, both the work and its history are lost with it.',
    separate_disk: 'The archive is on a different disk',
    free: 'free',
    summary_folder: 'Folder',
    summary_coverage: 'Coverage',
    summary_archive: 'Archive',
    summary_excluded: 'Excluded',
    start_note: 'Only versions observed while protection is running and the archive disk is connected can be restored.',
    adding: 'Copying the first snapshot…',
    added: 'Folder added',
    project: 'Project',
    versions: 'versions',
    last_observed: 'Last observed',
    never: 'never',
    archive_size: 'Archive size',
    skipped: 'Skipped',
    history: 'History & restore',
    moments: 'Moments with changes',
    no_moments: 'No changes have been observed yet.',
    tree_at: 'Files at this moment',
    select_all: 'Select all',
    nothing_selected: 'Select at least one file.',
    restore_to: 'Restore to',
    restore_selected: 'Restore selected to a new folder',
    restore_go: 'Restore',
    restored_ok: 'files verified byte by byte',
    mismatches: 'mismatches',
    open_folder: 'Open the restored folder',
    export: 'Export the history',
    export_hint: 'Writes the archive range to a folder you choose, with a manifest of every file.',
    export_go: 'Export…',
    importing: 'Import an export',
    import_hint: 'Brings an exported range back into this archive as its own project.',
    import_name: 'Name for the imported project',
    import_go: 'Import',
    diagnostics: 'Diagnostics',
    run_pass: 'Observe once now',
    doctor: 'Checks',
    daemon_log: 'Observation log',
    settings: 'Settings',
    interval: 'Observation interval (seconds)',
    interval_hint: 'The core observes every cycle; the promise’s precision equals this period.',
    save_interval_live: 'A running observation re-reads this by itself: the new interval is in force from its next cycle, with no restart.',
    save_interval_live_short: 'applies to the running observation',
    config_reread: 'configuration re-read without a restart',
    save_interval: 'Save interval',
    notifications: 'System notifications',
    language: 'Interface language',
    show_raw: 'Show the core’s raw output',
    watch_start: 'Start observation',
    watch_stop: 'Stop observation',
    watch_running: 'Observation is running',
    watch_stopped: 'Observation is stopped',
    watch_elsewhere: 'Observation is running, started outside this app',
    watch_stalled: 'A daemon is running but has not observed anything recently',
    protected_timer: 'Protected by an external timer',
    archive_missing: 'The archive disk is not reachable',
    protected: 'Protected',
    protected_low_space: 'Protected · little free space',
    paused_full: 'Recording stopped: not enough free space',
    not_protecting_stale: 'Not protecting — observation stopped reporting',
    protection_unknown: 'Unknown — the core did not answer',
    confirm: 'OK',
    paused: 'Paused',
    not_watching: 'Not watching',
    state: 'State',
    pause: 'Pause',
    resume: 'Resume',
    path: 'Path',
    type_your_path: 'Path to the folder',
    required: 'This is required',
    cancel: 'Cancel',
    done: 'Done',
    failed: 'Failed',
    working: 'Working…',
    moment: 'Moment',
    pick_moment: 'Pick a date and time',
    load_moment: 'Load this moment',
    no_tree: 'Load a moment to see the files it held.',
    stale: 'not observed recently',
    ok_fresh: 'checked',
    ago: 'ago',
    details: 'Details',
    name: 'Name',
    files: 'files',
    size: 'Size',
    kind: 'Kind',
    hash: 'Hash',
    store: 'The archive',
    not_yet: 'not chosen yet',
    existing_archive: 'Use the existing archive',
    clear: 'Clear',
    // round 297 — history, integrity, storage, retention
    nav_integrity: 'Integrity',
    nav_storage: 'Storage & retention',
    nav_notifications: 'What happened',
    notif_hint: 'One line for every message the program raised: a mass change, writing stopped for space, writing resumed, an unreachable archive, a cycle error. Read from the archive\'s own ledger file.',
    notif_empty: 'Nothing has been recorded yet. The ledger fills when the program has something to tell you.',
    notif_none_filter: 'Nothing of that kind has been recorded.',
    notif_refresh: 'Refresh',
    notif_all: 'All',
    notif_mass: 'Mass changes',
    notif_space: 'Disk space',
    notif_error: 'Errors',
    notif_lifecycle: 'Observation',
    notif_ledger: 'ledger',
    repair_btn: 'Give back the missing files\u2026',
    repair_title: 'Put back only what is missing',
    repair_hint: 'Files that exist right now are counted and then left exactly as they are: this never overwrites and never deletes.',
    repair_ask_moment: 'Which moment should the missing files come from?',
    repair_confirm: 'The missing files will be put back. Nothing that exists will be overwritten or deleted.',
    repair_created: 'created and verified byte for byte',
    repair_present: 'file(s) already present, left alone',
    repair_after: 'checked afterwards by this window',
    repair_overwritten: 'pre-existing file(s) changed',
    repair_deleted: 'file(s) deleted',
    repair_clean: 'nothing was changed and nothing was deleted',
    repair_violated: 'THE REPAIR CHANGED SOMETHING IT MUST NOT',
    repair_setting: 'A saved setting applies to a running observation without a restart',
    repair_setting_hint: 'The observation re-reads the configuration by itself; the archive keeps a record of every re-read in config_state.json.',
    tab_history: 'Moments',
    tab_versions: 'Versions of a file',
    tab_compare: 'Compare',
    tab_tools: 'Project tools',
    pick_file: 'Pick a file',
    filter_paths: 'Filter paths…',
    versions_of: 'Versions of',
    no_file: 'Choose a file to see its versions.',
    show_version: 'Preview',
    compare_current: 'Compare with the folder now',
    compare_prev: 'Compare with the previous version',
    v_latest: 'Latest saved version',
    v_first: 'First saved version',
    content_at: 'Content at',
    binary_content: 'This version is not text — the window shows the size and the hash and does not pretend to draw it.',
    truncated_content: 'Only the first 256 KB are shown; the version itself is complete in the archive.',
    nothing_stored: 'No version is stored for this path. The archive holds nothing for it yet.',
    diff_from: 'From',
    diff_to: 'To',
    diff_current: 'compare with the folder now',
    run_compare: 'Compare',
    added: 'added',
    changed: 'changed',
    removed: 'removed',
    compare_hint: 'Two moments from the archive, or one moment and the folder as it is now.',
    note_label: 'Note',
    note_save: 'Save note',
    mark_label: 'Mark this moment',
    mark_save: 'Save mark',
    last_good_load: 'Show the last good point',
    last_good_none: 'No mass event has been recorded, so there is no last-good point to compute.',
    apply_filters_run: 'Re-apply the file filters',
    relink_run: 'Point this project at another folder…',
    remove_run: 'Stop observing this folder',
    remove_confirm: 'History is kept. The folder stops being observed; nothing is deleted.',
    relink_confirm: 'The project will observe the new folder from now on.',
    check_fast: 'Check integrity',
    check_deep: 'Deep check (reads every blob)',
    check_result: 'Integrity',
    missing_blobs: 'Missing blobs',
    corrupted_blobs: 'Blobs that fail their hash',
    dangling_blobs: 'Blobs nothing refers to',
    all_present: 'every blob the journal refers to is present and readable',
    drill_run: 'Rehearse a restore (drill)',
    drill_hint: 'Restores the last moment into a temporary folder and compares byte for byte. It touches nothing else.',
    audit_run: 'Audit the archive against its digest',
    audit_hint: 'Hashes the archive and compares it with the digest recorded last. It writes nothing unless you ask for --update in the terminal.',
    audit_ok: 'no file inside the archive changed since the last digest',
    quarantine_load: 'Show quarantined bytes',
    quarantine_none: 'Nothing is in quarantine.',
    gc_run: 'Show what nothing refers to',
    recover_run: 'Finish or roll back an interrupted prune',
    recover_confirm: 'An interrupted prune left a journal. This finishes it or rolls it back; it does not delete anything else.',
    recent_load: 'Recent operations',
    storage_space: 'Space',
    storage_by_project: 'What each project takes',
    storage_load: 'Load sizes',
    free_space: 'free',
    threshold_note: 'Writing stops only below the smaller of 500 MB and 1% of the disk — a low-space warning does not stop it.',
    retention_title: 'Retention policy',
    retention_none: 'No policy is stored. A policy is a sentence about what to keep, for example 7d:all,30d:1/day,365d:1/month.',
    retention_stored: 'Stored policy',
    retention_applied: 'Last applied',
    retention_never: 'never (stored, not applied)',
    retention_save: 'Store this policy',
    retention_preview: 'Preview what it would remove',
    retention_apply: 'Apply it (deletes versions)',
    retention_apply_confirm: 'This deletes the versions the plan lists. Older moments cannot be restored afterwards.',
    retention_saved_note: 'Storing a policy deletes nothing: it is read only when a prune is asked for.',
    versions_kept: 'versions kept',
    dropped: 'dropped',
    new_history_start: 'History will start at',
    health_load: 'Scheduler checks',
    config_all_load: 'Every setting the core knows',
    moment_hint: '2026-10-06 16:04 (or pick from the calendar)',
    bad_moment: 'That is not a moment I can read',
    attention: 'What needs attention',
    attention_load: 'Ask the core',
    attention_hint: 'The core\'s own list of what needs doing now: gaps, mass events, dangling blobs, a stale heartbeat.',
    attention_none: 'Nothing needs attention: the heartbeat is fresh, no gaps, no mass events, no dangling blobs.',
    cli_only_title: 'Deliberately left to the terminal',
    cli_only_hint: 'Real commands the window does not perform, each with its reason. Where the menu shows the command instead of running it, the entry says so.',
    /* ---------------------------------------------------------- the menu (round 299) */
    menu_result: 'What the menu just did',
    menu_command_ran: 'The core command that ran',
    menu_command_behind: 'The core command behind this entry',
    menu_exit: 'exit code',
    menu_output: 'The core’s own output',
    menu_nothing_ran: 'Nothing was run: this entry names the command and says why it stays there.',
    menu_disabled: 'Not available yet',
    menu_asking: 'This entry needs one value',
    menu_confirm_note: 'This changes what is stored. Read the command, then decide.',
    menu_run: 'Run it',
    menu_cancel: 'Cancel',
    palette_title: 'Every command',
    palette_hint: 'Type to filter · ↑ ↓ to choose · Enter to run · Esc to close',
    palette_none: 'Nothing matches',
    palette_open: 'Every command…',
    view_about: 'About Project Life',
    about_author: 'Author',
    about_author_missing: 'The author is read from the core, and the core did not answer',
    view_status: 'Protection status',
    view_help: 'Quick guide',
    help_intro: 'The seven steps of a first day, and what each one proves.',
    ask_text: 'Value',
    ask_path: 'Path inside the project',
    ask_label: 'One word for this moment',
    ask_moment: 'Moment (YYYY-MM-DD HH:MM)',
    ask_folder: 'Folder',
    ask_path_prompt: 'A path relative to the project root, e.g. src/app.ts',
    menu_open_hint: 'Menus are also in the macOS menu bar, and every entry is in ⌘K.',
    menu_stale: 'The menu could not be read from the app server',
  },
  ru: {
    tagline: 'Локально. Приватно. Ваше.',
    chosen_folders: 'Защищённые папки',
    nav_dashboard: 'Обзор',
    nav_restore: 'История и восстановление',
    nav_diagnostics: 'Диагностика',
    nav_settings: 'Настройки',
    nav_import: 'Импорт',
    loading: 'Загрузка…',
    no_folders: 'Пока ни одной папки',
    welcome_title: 'Добро пожаловать в Project Life',
    welcome_hint: 'Тихая страховка для работы, которая важна: хранит версии выбранных папок на выбранном вами диске.',
    choose_archive: 'Выберите, где хранить архив',
    archive_hint: 'Архив лучше держать на другом диске, чем сама работа. После первой копии сохраняется только то, что меняется.',
    use_this: 'Использовать эту папку',
    create_archive: 'Создать архив здесь',
    choose_folder: 'Выбрать папку…',
    add_folder: 'Добавить папку',
    add_title: 'Добавление папки',
    add_hint: 'Защищается только выбранная папка — Project Life не следит за всем компьютером.',
    step_choose: 'Папка',
    step_coverage: 'Что сохраняется',
    step_storage: 'Хранилище',
    step_start: 'Запуск',
    folder_to_protect: 'Папка для защиты',
    selecting: 'Выберите папку на этом компьютере.',
    detected: 'Определено',
    no_convincing: 'Ни один набор не подошёл уверенно',
    confidence: 'уверенность',
    will_protect: 'файлов будет защищено',
    estimated_size: 'Ожидаемый размер архива',
    not_protected: 'Что не защищается',
    adjust: 'Настроить список защищаемых типов',
    adjust_hint: 'Снимите галочку, чтобы не сохранять этот тип; поставьте — чтобы сохранять.',
    files_here: 'файлов в этой папке',
    back: 'Назад',
    continue: 'Продолжить',
    start_protecting: 'Начать защиту',
    storage_title: 'Где живёт архив',
    same_disk: 'Архив на том же диске, что и папка',
    same_disk_hint: 'Если этот диск потеряется, вместе с работой пропадёт и её история.',
    separate_disk: 'Архив на другом диске',
    free: 'свободно',
    summary_folder: 'Папка',
    summary_coverage: 'Что сохраняется',
    summary_archive: 'Архив',
    summary_excluded: 'Исключено',
    start_note: 'Восстановить можно только те версии, которые были наблюдены при работающей защите и подключённом диске архива.',
    adding: 'Копируется первый снимок…',
    added: 'Папка добавлена',
    project: 'Проект',
    versions: 'версий',
    last_observed: 'Последнее наблюдение',
    never: 'никогда',
    archive_size: 'Размер архива',
    skipped: 'Пропущено',
    history: 'История и восстановление',
    moments: 'Моменты с изменениями',
    no_moments: 'Изменений пока не наблюдалось.',
    tree_at: 'Файлы на этот момент',
    select_all: 'Выбрать все',
    nothing_selected: 'Выберите хотя бы один файл.',
    restore_to: 'Восстановить в',
    restore_selected: 'Восстановить выбранное в отдельную папку',
    restore_go: 'Восстановить',
    restored_ok: 'файлов сверено побайтно',
    mismatches: 'расхождений',
    open_folder: 'Открыть папку восстановления',
    export: 'Экспорт истории',
    export_hint: 'Записывает диапазон архива в выбранную папку вместе с манифестом всех файлов.',
    export_go: 'Экспортировать…',
    importing: 'Импорт экспорта',
    import_hint: 'Возвращает экспортированный диапазон в этот архив отдельным проектом.',
    import_name: 'Имя импортируемого проекта',
    import_go: 'Импортировать',
    diagnostics: 'Диагностика',
    run_pass: 'Наблюсти один раз сейчас',
    doctor: 'Проверки',
    daemon_log: 'Журнал наблюдения',
    settings: 'Настройки',
    interval: 'Интервал наблюдения (секунды)',
    interval_hint: 'Ядро наблюдает каждый цикл; точность обещания равна этому периоду.',
    save_interval_live: 'Работающее наблюдение перечитывает это само: новый интервал действует со следующего цикла, перезапуск не нужен.',
    save_interval_live_short: 'применится к работающему наблюдению',
    config_reread: 'конфигурация перечитана без перезапуска',
    save_interval: 'Сохранить интервал',
    notifications: 'Системные уведомления',
    language: 'Язык интерфейса',
    show_raw: 'Показывать сырой вывод ядра',
    watch_start: 'Начать наблюдение',
    watch_stop: 'Остановить наблюдение',
    watch_running: 'Наблюдение работает',
    watch_stopped: 'Наблюдение остановлено',
    watch_elsewhere: 'Наблюдение работает, запущено вне этого приложения',
    watch_stalled: 'Демон запущен, но давно ничего не наблюдал',
    protected_timer: 'Защищено внешним таймером',
    archive_missing: 'Диск архива недоступен',
    protected: 'Защищено',
    protected_low_space: 'Защищено · мало свободного места',
    paused_full: 'Запись остановлена: недостаточно места',
    not_protecting_stale: 'Не защищает — наблюдение перестало отмечаться',
    protection_unknown: 'Неизвестно — ядро не ответило',
    confirm: 'ОК',
    paused: 'Пауза',
    not_watching: 'Не наблюдает',
    state: 'Состояние',
    pause: 'Пауза',
    resume: 'Продолжить',
    path: 'Путь',
    type_your_path: 'Путь к папке',
    required: 'Это обязательное поле',
    cancel: 'Отмена',
    done: 'Готово',
    failed: 'Ошибка',
    working: 'Работает…',
    moment: 'Момент',
    pick_moment: 'Выберите дату и время',
    load_moment: 'Загрузить этот момент',
    no_tree: 'Загрузите момент, чтобы увидеть файлы.',
    stale: 'давно нет наблюдения',
    ok_fresh: 'проверено',
    ago: 'назад',
    details: 'Подробности',
    name: 'Имя',
    files: 'файлов',
    size: 'Размер',
    kind: 'Тип',
    hash: 'Хеш',
    store: 'Архив',
    not_yet: 'пока не выбран',
    existing_archive: 'Использовать существующий архив',
    clear: 'Сбросить',
    // круг 297 — история, целостность, хранилище, срок хранения
    nav_integrity: 'Целостность',
    nav_storage: 'Хранилище и срок хранения',
    nav_notifications: 'Что произошло',
    notif_hint: 'Строка на каждое сообщение программы: массовое изменение, остановка записи из-за места, возобновление записи, недоступный архив, ошибка цикла. Читается из собственного журнала архива.',
    notif_empty: 'Пока ничего не записано. Журнал заполняется, когда программе есть что сказать.',
    notif_none_filter: 'Записей такого рода нет.',
    notif_refresh: 'Обновить',
    notif_all: 'Все',
    notif_mass: 'Массовые изменения',
    notif_space: 'Место на диске',
    notif_error: 'Ошибки',
    notif_lifecycle: 'Наблюдение',
    notif_ledger: 'журнал',
    repair_btn: 'Вернуть недостающие файлы\u2026',
    repair_title: 'Вернуть только то, чего не хватает',
    repair_hint: 'Файлы, которые есть сейчас, будут посчитаны и оставлены ровно как есть: ничего не перезаписывается и не удаляется.',
    repair_ask_moment: 'Из какого момента вернуть недостающие файлы?',
    repair_confirm: 'Недостающие файлы будут возвращены. Ничего существующего не будет перезаписано или удалено.',
    repair_created: 'создано и сверено побайтово',
    repair_present: 'файлов уже на месте, не тронуты',
    repair_after: 'проверено после этого самим окном',
    repair_overwritten: 'существующих файлов изменено',
    repair_deleted: 'файлов удалено',
    repair_clean: 'ничего не изменено и ничего не удалено',
    repair_violated: 'ВОССТАНОВЛЕНИЕ ИЗМЕНИЛО ТО, ЧЕГО НЕ ДОЛЖНО',
    repair_setting: 'Сохранённая настройка применяется к работающему наблюдению без перезапуска',
    repair_setting_hint: 'Наблюдение само перечитывает конфигурацию; архив хранит запись о каждом перечитывании в config_state.json.',
    tab_history: 'Моменты',
    tab_versions: 'Версии файла',
    tab_compare: 'Сравнение',
    tab_tools: 'Инструменты проекта',
    pick_file: 'Выберите файл',
    filter_paths: 'Фильтр путей…',
    versions_of: 'Версии',
    no_file: 'Выберите файл, чтобы увидеть его версии.',
    show_version: 'Просмотр',
    compare_current: 'Сравнить с папкой сейчас',
    compare_prev: 'Сравнить с предыдущей версией',
    v_latest: 'Последняя сохранённая версия',
    v_first: 'Первая сохранённая версия',
    content_at: 'Содержимое на',
    binary_content: 'Эта версия не текст — окно показывает размер и хеш и не делает вид, что рисует её.',
    truncated_content: 'Показаны первые 256 КБ; сама версия в архиве целиком.',
    nothing_stored: 'Для этого пути версий нет — архив по нему пока ничего не хранит.',
    diff_from: 'С',
    diff_to: 'По',
    diff_current: 'сравнить с папкой сейчас',
    run_compare: 'Сравнить',
    added: 'добавлено',
    changed: 'изменено',
    removed: 'удалено',
    compare_hint: 'Два момента из архива — или момент и папка, как она есть сейчас.',
    note_label: 'Заметка',
    note_save: 'Сохранить заметку',
    mark_label: 'Отметить этот момент',
    mark_save: 'Сохранить метку',
    last_good_load: 'Показать последнюю хорошую точку',
    last_good_none: 'Массовых событий не было — считать «последнюю хорошую точку» не из чего.',
    apply_filters_run: 'Применить фильтры файлов заново',
    relink_run: 'Направить проект на другую папку…',
    remove_run: 'Прекратить наблюдение за папкой',
    remove_confirm: 'История сохранится. Папка перестанет наблюдаться; ничего не удаляется.',
    relink_confirm: 'Проект будет наблюдать новую папку с этого момента.',
    check_fast: 'Проверить целостность',
    check_deep: 'Глубокая проверка (читает каждый блоб)',
    check_result: 'Целостность',
    missing_blobs: 'Отсутствующие блобы',
    corrupted_blobs: 'Блобы, не прошедшие хеш',
    dangling_blobs: 'Блобы, на которые никто не ссылается',
    all_present: 'каждый блоб, на который ссылается журнал, на месте и читается',
    drill_run: 'Провести репетицию восстановления',
    drill_hint: 'Восстанавливает последний момент во временную папку и сравнивает побайтно. Больше ничего не трогает.',
    audit_run: 'Сверить архив с его отпечатком',
    audit_hint: 'Хеширует архив и сравнивает с отпечатком, записанным в прошлый раз. Ничего не пишет, пока вы не попросите --update в терминале.',
    audit_ok: 'ни один файл внутри архива не изменился с прошлого отпечатка',
    quarantine_load: 'Показать отложенные байты',
    quarantine_none: 'В карантине ничего нет.',
    gc_run: 'Показать то, на что никто не ссылается',
    recover_run: 'Довести или откатить прерванную обрезку',
    recover_confirm: 'Прерванная обрезка оставила журнал. Это доведёт её или откатит; больше ничего не удаляется.',
    recent_load: 'Последние операции',
    storage_space: 'Место',
    storage_by_project: 'Сколько занимает каждый проект',
    storage_load: 'Загрузить размеры',
    free_space: 'свободно',
    threshold_note: 'Запись останавливается только ниже меньшего из двух чисел — 500 МБ и 1 % диска; предупреждение о месте её не останавливает.',
    retention_title: 'Политика хранения',
    retention_none: 'Политика не задана. Политика — это фраза о том, что хранить, например 7d:all,30d:1/day,365d:1/month.',
    retention_stored: 'Сохранённая политика',
    retention_applied: 'Последнее применение',
    retention_never: 'никогда (сохранена, не применялась)',
    retention_save: 'Сохранить политику',
    retention_preview: 'Показать, что она удалит',
    retention_apply: 'Применить (удаляет версии)',
    retention_apply_confirm: 'Это удалит версии из плана. Более старые моменты нельзя будет восстановить.',
    retention_saved_note: 'Сохранение политики ничего не удаляет: она читается только тогда, когда обрезку просят.',
    versions_kept: 'версий останется',
    dropped: 'удалено',
    new_history_start: 'История будет начинаться с',
    health_load: 'Проверки для планировщика',
    config_all_load: 'Все настройки, которые знает ядро',
    moment_hint: '2026-10-06 16:04 (или выберите в календаре)',
    bad_moment: 'Это не момент, который я могу прочитать',
    attention: 'На что обратить внимание',
    attention_load: 'Спросить ядро',
    attention_hint: 'Собственный список ядра: разрывы наблюдения, массовые события, висячие блобы, устаревшее сердцебиение.',
    attention_none: 'Ничего не требует внимания: сердцебиение свежее, разрывов, массовых событий и висячих блобов нет.',
    cli_only_title: 'Сознательно оставлено в терминале',
    cli_only_hint: 'Настоящие команды, которых окно не выполняет, — у каждой своя причина. Там, где меню показывает команду вместо запуска, это сказано прямо.',
    /* ---------------------------------------------------------- меню (раунд 299) */
    menu_result: 'Что только что сделало меню',
    menu_command_ran: 'Команда ядра, которая выполнилась',
    menu_command_behind: 'Команда ядра за этим пунктом',
    menu_exit: 'код выхода',
    menu_output: 'Собственный вывод ядра',
    menu_nothing_ran: 'Ничего не выполнялось: пункт называет команду и объясняет, почему она остаётся там.',
    menu_disabled: 'Пока недоступно',
    menu_asking: 'Этому пункту нужно одно значение',
    menu_confirm_note: 'Это меняет то, что хранится. Прочитайте команду и решите.',
    menu_run: 'Выполнить',
    menu_cancel: 'Отмена',
    palette_title: 'Все команды',
    palette_hint: 'Печатайте для поиска · ↑ ↓ выбор · Enter выполнить · Esc закрыть',
    palette_none: 'Ничего не найдено',
    palette_open: 'Все команды…',
    view_about: 'О программе Project Life',
    about_author: 'Автор',
    about_author_missing: 'Автор читается из ядра, и ядро не ответило',
    view_status: 'Состояние защиты',
    view_help: 'Краткое руководство',
    help_intro: 'Семь шагов первого дня и то, что каждый из них доказывает.',
    ask_text: 'Значение',
    ask_path: 'Путь внутри проекта',
    ask_label: 'Одно слово для этого момента',
    ask_moment: 'Момент (ГГГГ-ММ-ДД ЧЧ:ММ)',
    ask_folder: 'Папка',
    ask_path_prompt: 'Путь относительно корня проекта, например src/app.ts',
    menu_open_hint: 'Эти же меню есть в строке меню macOS, а любой пункт — в ⌘K.',
    menu_stale: 'Меню не удалось прочитать у сервера приложения',
  },
};

let LANG = 'en';
function t(key) {
  const d = T[LANG] || T.en;
  return d[key] !== undefined ? d[key] : (T.en[key] !== undefined ? T.en[key] : key);
}

function chooseLang() {
  const prefs = localStorage.getItem('pl.lang');
  if (prefs) return prefs;
  const n = (navigator.language || 'en').toLowerCase();
  return n.startsWith('ru') ? 'ru' : 'en';
}

/* ------------------------------------------------------------------ helpers */

function esc(s) {
  return String(s === null || s === undefined ? '' : s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

function bytes(n) {
  if (n === null || n === undefined) return '—';
  let b = Number(n), i = 0;
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
  return (i === 0 ? b + ' B' : b.toFixed(1) + ' ' + u[i]);
}

function msAgo(ms) {
  if (ms === null || ms === undefined) return '';
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return s + ' s';
  const m = Math.round(s / 60);
  if (m < 60) return m + ' min';
  const h = Math.round(m / 60);
  if (h < 24) return h + ' h';
  return Math.round(h / 24) + ' d';
}

/// Read a moment the person typed or picked.
///
/// `<input type="datetime-local">` is supported by Safari only from 14.1 (macOS 11.3), while the
/// bundle declares 11.0 as its minimum: on 11.0-11.2 the control is a plain text field and the page
/// must still understand what is in it. Accepts what a date picker produces (`2026-10-06T16:04`) and
/// what a person types (`2026-10-06 16:04`, `2026-10-06`), and refuses anything else.
///
/// It refuses *strictly*, and that is the whole point. The first version of this function tried the
/// input against `Date.parse` with a few variations, and `Date.parse("notTa date:00")` — which is
/// what "not a date" becomes once a space is replaced — is read by JavaScript as a date with a time
/// zone offset: it returns 1999-12-31, a perfectly plausible-looking moment. So the shape is checked
/// first, then the parts, then the date is built locally and read back: 2026-02-31 is refused too.
function momentFromInput(value) {
  const v = (value || '').trim();
  if (!v) return null;
  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})(?:[T ](\d{1,2}):(\d{2})(?::(\d{2}))?)?$/.exec(v);
  if (!m) return null;
  const y = +m[1], mo = +m[2], d = +m[3];
  const hh = m[4] === undefined ? 0 : +m[4];
  const mi = m[5] === undefined ? 0 : +m[5];
  const ss = m[6] === undefined ? 0 : +m[6];
  if (mo < 1 || mo > 12 || d < 1 || d > 31 || hh > 23 || mi > 59 || ss > 59) return null;
  const t = new Date(y, mo - 1, d, hh, mi, ss, 0);
  if (t.getFullYear() !== y || t.getMonth() !== mo - 1 || t.getDate() !== d) return null;
  return t.getTime();
}

function localFromMs(ms) {
  const d = new Date(ms);
  const p = (x) => String(x).padStart(2, '0');
  return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' +
    p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
}

function isoUtc(ms) {
  const d = new Date(ms);
  return d.toISOString().replace(/\.\d+Z$/, '.000Z');
}

function html(strings, ...vals) { return strings.map((s, i) => s + (vals[i] !== undefined ? vals[i] : '')).join(''); }

function toast(msg, isErr) {
  const host = document.getElementById('toast-host');
  host.innerHTML = '<div class="toast" style="' + (isErr ? 'background:#AC3838' : '') + '">' + esc(msg) + '</div>';
  setTimeout(() => { host.innerHTML = ''; }, isErr ? 6000 : 3000);
}

/* ------------------------------------------------------------------ state */

let S = {
  boot: null,
  view: 'dashboard',
  project: null,
  wizard: null,
  tree: null,
  moment: null,
  jobId: null,
  job: null,
  doctor: null,
  log: null,
  error: null,
  raw: false,
  busy: false,
  // round 297 — the parts of the core the window now shows
  tab: 'history',
  versions: null, content: null, file: null, diff: null, compare: null,
  check: null, size: null, gc: null, audit: null, quarantine: null,
  retention: null, recent: null, configAll: null, health: null, lastGood: null,
  noteDraft: null, filter: '', suggest: null, toolProject: null,
  // round 300 — the notification ledger, and the repair it leads to
  notif: null, notifFilter: 'all',
};

async function refreshBoot() {
  S.boot = await api('bootstrap');
  if (S.project && !S.boot.projects.some((p) => p.name === S.project)) S.project = null;
}

async function pollJob() {
  if (!S.jobId) return;
  try {
    S.job = await api('job?id=' + S.jobId);
  } catch (e) { return; }
  if (S.job.state !== 'running') {
    S.jobId = null;
    await refreshBoot();
  }
  render();
}

/* ------------------------------------------------------------------ native bridges */

/* ------------------------------------------------------------------ one small dialog
 *
 * The macOS shell has no JavaScript dialog handler unless the app implements one, so
 * the browser's own prompt() returned null there and the import stopped without a word: the owner selected a
 * folder and nothing happened. This dialog is a DOM element drawn by this page, so it works in the
 * shell, in a browser, and in the end-to-end test that drives the same bytes.
 */
function askText(title, initial) {
  return new Promise(function (resolve) {
    const back = document.createElement('div');
    back.className = 'modal-back';
    back.innerHTML =
      '<div class="modal" role="dialog" aria-modal="true">' +
      '<h3>' + esc(title) + '</h3>' +
      '<input id="pl-ask-input" type="text" value="' + esc(initial || '') + '">' +
      '<div class="modal-actions">' +
      '<button class="btn primary" id="pl-ask-ok">' + esc(t('confirm')) + '</button>' +
      '<button class="btn" id="pl-ask-cancel">' + esc(t('cancel')) + '</button>' +
      '</div></div>';
    document.body.appendChild(back);
    const input = back.querySelector('#pl-ask-input');
    const finish = function (value) { back.remove(); resolve(value); };
    const accept = function () { finish(input.value.trim() ? input.value.trim() : null); };
    back.querySelector('#pl-ask-ok').onclick = accept;
    back.querySelector('#pl-ask-cancel').onclick = function () { finish(null); };
    input.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') { e.preventDefault(); accept(); }
      if (e.key === 'Escape') { e.preventDefault(); finish(null); }
    });
    input.focus();
    input.select();
  });
}

async function pickFolder(title) {
  if (S.boot && S.boot.app.native) {
    try {
      const r = await fetch('pl://pick-folder?title=' + encodeURIComponent(title || ''));
      const j = await r.json();
      return j && j.path ? j.path : null;
    } catch (e) { /* fall through to the text prompt */ }
  }
  const answer = await askText(t('type_your_path'), '');
  return answer ? answer : null;
}

async function revealFolder(path) {
  if (S.boot && S.boot.app.native) {
    try { await fetch('pl://reveal?path=' + encodeURIComponent(path)); return; } catch (e) { /* ignore */ }
  }
  toast(path);
}

/* ------------------------------------------------------------------ actions */

async function chooseArchive(create) {
  const path = await pickFolder(t('choose_archive'));
  if (!path) return;
  S.busy = true; render();
  try {
    const r = create ? await api('archive/init', { body: { path } }) : await api('archive/use', { body: { path } });
    await refreshBoot();
    toast(r.created ? t('create_archive') + ': ' + path : path);
    if (S.wizard && S.wizard.step === 3) { S.wizard.archiveRoot = r.archiveRoot; }
  } catch (e) { S.error = e.message; }
  S.busy = false;
  render();
}

async function startWatch(intervalSeconds) {
  S.busy = true; render();
  try {
    const r = await api('watch/start', { body: { intervalSeconds: intervalSeconds || undefined } });
    if (r.already) toast(t('watch_running'));
    else toast(t('watch_running'));
    setTimeout(refreshBoot, 1200);
  } catch (e) { S.error = e.message; }
  S.busy = false;
  render();
}

async function stopWatch() {
  S.busy = true; render();
  try {
    const r = await api('watch/stop', { body: {} });
    toast(r.stopped ? t('watch_stopped') : t('watch_stopped'));
    setTimeout(refreshBoot, 800);
  } catch (e) { S.error = e.message; }
  S.busy = false;
  render();
}

async function setConfig(key, value) {
  try {
    await api('config', { body: { key, value } });
    await refreshBoot();
    // Round 300 (FR-CFG-3): a saved setting reaches a running observation by itself, so the honest
    // message is not "saved" but what will happen to it.
    toast(key + ' = ' + value + ' · ' + t('save_interval_live_short'));
  } catch (e) { S.error = e.message; render(); }
}

async function pauseProject(name, doPause) {
  try {
    await api('project/pause', { body: { name, pause: doPause } });
    await refreshBoot();
  } catch (e) { S.error = e.message; render(); }
}

async function runPass() {
  try {
    const r = await api('pass', { body: {} });
    S.jobId = r.job; S.job = null;
    render();
  } catch (e) { S.error = e.message; render(); }
}

/* ------------------------------------------------------------------ wizard */

function wizardStart() {
  S.wizard = { step: 1, folder: null, detection: null, preset: null, presetList: null, editAdd: [], editRemove: [], preview: null, name: '', archiveRoot: null, includeLargeFiles: false };
  S.view = 'add';
  S.error = null;
  render();
  loadPresets();
}

async function loadPresets() {
  try {
    S.wizard.presetList = await api('presets');
    if (!S.wizard.preset) S.wizard.preset = 'custom';
    render();
  } catch (e) { S.error = e.message; render(); }
}

async function wizardPickFolder() {
  const path = await pickFolder(t('folder_to_protect'));
  if (!path) return;
  S.busy = true; render();
  try {
    const det = await api('detect', { body: { path } });
    S.wizard.folder = path;
    S.wizard.detection = det;
    S.wizard.name = path.split('/').filter(Boolean).pop() || 'project';
    S.wizard.preset = 'auto';
    S.wizard.step = 2;
    S.wizard.preview = null;
  } catch (e) { S.error = e.message; }
  S.busy = false;
  render();
}

function presetOf(id) {
  if (!S.wizard || !S.wizard.presetList) return null;
  return S.wizard.presetList.find((p) => p.id === id) || null;
}

function effectivePresetId() {
  const w = S.wizard;
  if (w.preset === 'auto') {
    const det = w.detection;
    if (det && det.best && det.best.id && !det.suggestCustom) return det.best.id;
    return 'custom';
  }
  return w.preset;
}

async function wizardPreview() {
  const w = S.wizard;
  S.busy = true; render();
  try {
    const body = { path: w.folder, preset: effectivePresetId(), editAdd: w.editAdd, editRemove: w.editRemove, name: w.name, includeLargeFiles: w.includeLargeFiles };
    w.preview = await api('add/preview', { body });
    w.step = 3;
  } catch (e) { S.error = e.message; }
  S.busy = false;
  render();
}

async function wizardStartProtect() {
  const w = S.wizard;
  try {
    const body = { path: w.folder, preset: effectivePresetId(), editAdd: w.editAdd, editRemove: w.editRemove, name: w.name, includeLargeFiles: w.includeLargeFiles };
    const r = await api('add', { body });
    S.jobId = r.job;
    S.job = null;
    S.wizard = Object.assign({}, w, { step: 5 });
    render();
  } catch (e) { S.error = e.message; render(); }
}

/* ------------------------------------------------------------------ project */

async function openProject(name) {
  S.project = name;
  S.toolProject = name;
  S.view = 'project';
  S.tree = null;
  S.moment = null;
  S.error = null;
  if (menuProject !== S.project) { await loadMenu(); menuProject = S.project; }
  render();
  try {
    const d = await api('project?name=' + encodeURIComponent(name));
    S.projectDetail = d;
    if (d.moments && d.moments.length && !S.moment) {
      S.moment = d.moments[0];
      await loadTree(d.moments[0].atIso);
    }
  } catch (e) { S.error = e.message; }
  render();
}

async function loadTree(atIso) {
  if (!S.project) return;
  try {
    const r = await api('project/tree?name=' + encodeURIComponent(S.project) + '&at=' + encodeURIComponent(atIso));
    S.tree = r.files || [];
    S.selected = new Set();
  } catch (e) { S.error = e.message; S.tree = null; }
  render();
}

async function restoreSelected() {
  const paths = Array.from(S.selected || []);
  if (!paths.length) { S.error = t('nothing_selected'); render(); return; }
  let to = S.restoreTo;
  if (!to) {
    const stamp = new Date().toISOString().replace(/[-:]/g, '').slice(0, 13).replace('T', '-');
    to = await pickFolder(t('restore_to'));
    if (!to) return;
    to = to + '/' + S.project + '-' + stamp;
  }
  S.restoreTo = to;
  try {
    const r = await api('project/restore', { body: { name: S.project, at: S.moment.atIso, paths, to } });
    S.jobId = r.job; S.job = null;
    render();
  } catch (e) { S.error = e.message; render(); }
}

async function exportHistory() {
  const out = await pickFolder(t('export'));
  if (!out) return;
  try {
    const r = await api('project/export', { body: { name: S.project, out: out + '/' + S.project + '-export' } });
    S.jobId = r.job; S.job = null;
    render();
  } catch (e) { S.error = e.message; render(); }
}

async function importExport() {
  const dir = await pickFolder(t('importing'));
  if (!dir) return;
  // Not the browser's prompt(): on macOS the shell showed nothing and the import died in silence.
  const name = await askText(t('import_name'), dir.split('/').filter(Boolean).pop() || 'imported');
  if (!name) return;
  try {
    const r = await api('import', { body: { dir, newName: name } });
    S.jobId = r.job; S.job = null;
    render();
  } catch (e) { S.error = e.message; render(); }
}

/* ------------------------------------------------------------------ status helpers */

function protectionState() {
  const b = S.boot;
  if (!b) return { level: 'plain', label: t('loading'), detail: '' };
  const arch = b.archive;
  if (!arch.configured || !arch.isArchive) return { level: 'plain', label: t('no_folders'), detail: t('choose_archive') };
  if (!arch.exists) return { level: 'err', label: t('archive_missing'), detail: arch.root };
  if (!b.projects.length) return { level: 'plain', label: t('no_folders'), detail: arch.root };
  // The answer comes from the core, in one place, so that the window cannot say "Protected" while
  // the daemon is refusing to write — which is exactly what happened on 2026-10-06.
  const p = (b.watch && b.watch.protection) || null;
  if (!p) {
    const hb = (b.watch && b.watch.heartbeat) || {};
    const observed = (hb.ageMs === null || hb.ageMs === undefined) ? '' : msAgo(hb.ageMs) + ' ' + t('ago');
    return { level: 'warn', label: t('watch_stopped'), detail: t('last_observed') + ' ' + observed };
  }
  const detail = p.reason || '';
  const level = p.state === 'protected' ? 'ok'
    : p.state === 'protected_low_space' ? 'warn'
    : p.state === 'unknown' ? 'plain'
    : p.state === 'paused_full' ? 'err'
    : 'warn';
  const key = { protected: 'protected', protected_low_space: 'protected_low_space', paused_full: 'paused_full',
                stale: 'not_protecting_stale', stopped: 'watch_stopped', unknown: 'protection_unknown' }[p.state];
  const label = (key && t(key)) || p.label || p.state;
  return { level: level, label: label, detail: detail, storage: p.storage || null, protected: !!p.protected };
}

/* ------------------------------------------------------------------ render */

/// The fields whose contents must survive a redraw.
///
/// The window rebuilds #main on every bootstrap poll (3 s) and every 700 ms while a job runs. A
/// redraw replaces the elements, and an element that is replaced forgets what was typed into it: the
/// acceptance test found this by typing a moment into the compare field and watching the field come
/// back empty — which for a person means "I typed a date, looked at the log for two seconds, and my
/// date was gone". The values are read before the redraw and put back after it.
const STICKY_FIELDS = ['pick-moment', 'cmp-from', 'cmp-to', 'set-interval', 'pl-note', 'pl-mark', 'ret-policy'];

function captureFields() {
  const keep = {};
  for (const id of STICKY_FIELDS) {
    const el = document.getElementById(id);
    if (el) keep[id] = { value: el.value, focused: document.activeElement === el };
  }
  const now = document.getElementById('cmp-now');
  if (now) keep['cmp-now'] = { checked: now.checked };
  return keep;
}

function restoreFields(keep) {
  for (const id of Object.keys(keep)) {
    const el = document.getElementById(id);
    if (!el || !keep[id]) continue;
    if (keep[id].value !== undefined) el.value = keep[id].value;
    if (keep[id].checked !== undefined) el.checked = keep[id].checked;
    if (keep[id].focused) el.focus();
  }
}

function render() {
  if (!S.boot) {
    document.getElementById('main').innerHTML = '<p class="muted">' + esc(t('loading')) + '</p>';
    return;
  }
  const keepFields = captureFields();
  renderNav();
  renderFolders();
  renderSideFoot();
  renderMenuBar();
  const main = document.getElementById('main');
  let html = '';
  if (S.jobId || S.job) html += jobCard();
  html += resultCard();
  html += S.error ? errorCard() : '';
  const views = VIEWS();
  if (views[S.view]) html += views[S.view]();
  html += paletteOverlay();
  main.innerHTML = html;
  restoreFields(keepFields);
  if (S.jobId || S.job) pollJobLater();
}

let jobTimer = null;
function pollJobLater() {
  if (jobTimer) return;
  jobTimer = setTimeout(async () => {
    jobTimer = null;
    await pollJob();
    if (S.jobId || (S.job && S.job.state === 'running')) pollJobLater();
  }, 700);
}

/// Keep the restore buttons in step with the selection without rebuilding the table.
function updateRestoreButtons() {
  const n = (S.selected || new Set()).size;
  document.querySelectorAll('button[data-act="restore"]').forEach((b) => {
    b.disabled = n === 0;
    b.textContent = t('restore_go') + ' (' + n + ')';
  });
}

function errorCard() {
  return '<div class="banner err"><div><h3>' + esc(t('failed')) + '</h3><p>' + esc(S.error) + '</p></div>' +
    '<div class="spacer"></div><button class="btn" data-act="dismiss-error">' + esc(t('cancel')) + '</button></div>';
}

function jobCard() {
  const j = S.job;
  if (!j) return '<div class="card"><h3>' + esc(t('working')) + '</h3><div class="log">job ' + esc(String(S.jobId || '')) + '</div></div>';
  const lines = (j.lines || []).map((l) => '<span class="' + (l.startsWith('!') ? 'err' : '') + '">' + esc(l) + '</span>').join('\n');
  let head = '';
  if (j.state === 'running') head = '<h3>' + esc(t('working')) + ' — ' + esc(j.label) + '</h3>';
  else if (j.state === 'done') head = '<h3 class="ok">' + esc(t('done')) + ' — ' + esc(j.label) + '</h3>';
  else head = '<h3 style="color:#AC3838">' + esc(t('failed')) + ' — ' + esc(j.label) + '</h3>';
  let extra = '';
  if (j.state === 'done' && j.result) extra += resultSummary(j);
  if (j.error) extra += '<p class="muted small">' + esc(j.error) + '</p>';
  return '<div class="card"><div class="row between"><div>' + head + '</div>' +
    (j.state === 'running' ? '' : '<button class="btn" data-act="dismiss-job">' + esc(t('cancel')) + '</button>') +
    '</div>' + extra + '<div class="log">' + lines + '</div></div>';
}

function resultSummary(j) {
  const r = j.result || {};
  let out = '';
  if (j.kind === 'restore') {
    const bad = (r.mismatched || []).length;
    out += '<p><strong>' + esc(String(r.verified)) + '</strong> ' + esc(t('restored_ok')) + ', ' +
      (bad === 0 ? '<span class="pill ok">0 ' + esc(t('mismatches')) + '</span>' : '<span class="pill err">' + bad + ' ' + esc(t('mismatches')) + '</span>') +
      ' · ' + esc(r.target) + '</p>';
    if (r.symlinksNotCompared) out += '<p class="small muted">' + esc(String(r.symlinksNotCompared)) + ' symlink(s) not compared by content</p>';
    for (const m of (r.mismatched || []).slice(0, 20)) out += '<p class="small mono">' + esc(JSON.stringify(m)) + '</p>';
    out += '<button class="btn" data-act="reveal" data-path="' + esc(r.target) + '">' + esc(t('open_folder')) + '</button>';
  } else if (j.kind === 'repair') {
    const violated = (r.overwritten || []).length + (r.deletedByRepair || []).length;
    out += '<p><strong>' + esc(String(r.verified)) + '</strong> ' + esc(t('repair_created')) +
      ' · ' + esc(t('repair_present')) + ': <strong>' + esc(String(r.presentBefore)) + '</strong></p>';
    out += '<p>' + (violated === 0
      ? '<span class="pill ok">' + esc(t('repair_clean')) + '</span>'
      : '<span class="pill err">' + esc(t('repair_violated')) + '</span>') +
      ' — ' + esc(t('repair_overwritten')) + ': ' + esc(String((r.overwritten || []).length)) +
      ', ' + esc(t('repair_deleted')) + ': ' + esc(String((r.deletedByRepair || []).length)) + '</p>';
    for (const m of (r.overwritten || []).slice(0, 20)) out += '<p class="small mono">' + esc(JSON.stringify(m)) + '</p>';
    for (const m of (r.deletedByRepair || []).slice(0, 20)) out += '<p class="small mono">' + esc(JSON.stringify(m)) + '</p>';
    out += '<p class="small muted">' + esc(t('repair_after')) + ' · ' + esc(String(r.root)) + '</p>';
    out += '<button class="btn" data-act="reveal" data-path="' + esc(r.root) + '">' + esc(t('open_folder')) + '</button>';
  } else if (j.kind === 'export') {
    out += '<p>' + esc(String(r.manifestOk)) + ' / ' + esc(String(r.manifestLines)) + ' ' + esc(t('files')) + ' ' +
      ((r.manifestBad || []).length === 0 ? '<span class="pill ok">manifest ok</span>' : '<span class="pill err">manifest mismatch</span>') + '</p>';
  } else if (j.kind === 'import') {
    out += '<p>' + esc(String(r.exportedEvents)) + ' event line(s) exported on disk</p>';
    if (r.project) out += '<p class="small">' + esc(r.project.name) + ' — ' + esc(String(r.project.versions)) + ' ' + esc(t('versions')) + '</p>';
  } else if (j.kind === 'add' && r.project) {
    out += '<p>' + esc(r.project.name) + ' · ' + esc(String(r.project.versions)) + ' ' + esc(t('versions')) +
      ' · ' + esc(bytes(r.project.archiveBytes)) + '</p>';
    out += '<button class="btn" data-act="open-project" data-name="' + esc(r.project.name) + '">' + esc(t('history')) + '</button>';
  } else if (j.kind === 'pass') {
    out += '<p class="small mono">' + esc(JSON.stringify(r).slice(0, 400)) + '</p>';
  }
  return out;
}

function renderNav() {
  const items = [
    ['dashboard', t('nav_dashboard')],
    ['project', t('nav_restore')],
    ['integrity', t('nav_integrity')],
    ['storage', t('nav_storage')],
    ['notifications', t('nav_notifications')],
    ['import', t('nav_import')],
    ['diagnostics', t('nav_diagnostics')],
    ['settings', t('nav_settings')],
  ];
  document.getElementById('nav').innerHTML = items.map(([v, label]) =>
    '<button data-act="nav" data-view="' + v + '" class="' + (S.view === v ? 'active' : '') + '">' + esc(label) + '</button>'
  ).join('');
}

function renderFolders() {
  const host = document.getElementById('folders');
  const ps = S.boot.projects || [];
  if (!ps.length) {
    host.innerHTML = '<button data-act="nav" data-view="add">' + esc(t('add_folder')) + '</button>';
    return;
  }
  host.innerHTML = ps.map((p) => {
    const state = p.state === 'active' ? 'ok' : (p.state === 'paused' ? 'warn' : 'err');
    return '<button data-act="open-project" data-name="' + esc(p.name) + '" class="' + (S.project === p.name ? 'active' : '') + '">' +
      esc(p.name) + '<span class="sub"><span class="dot ' + state + '"></span> ' + esc(String(p.versions)) + ' ' + esc(t('versions')) + '</span></button>';
  }).join('') + '<button data-act="nav" data-view="add">+ ' + esc(t('add_folder')) + '</button>';
}

function renderSideFoot() {
  const st = protectionState();
  // Which build is this? The line exists because the owner reported three failures against a bundle
  // that had been replaced hours earlier and nothing on screen said so. The tooltip carries the
  // command that checks it from outside the app.
  const b = (S.boot && S.boot.build) || null;
  const build = (b && b.short)
    ? '<p class="small muted" style="margin:8px 0 0;text-align:center" title="' + esc(b.check || '') + '">build ' + esc(b.short) + '</p>'
    : '';
  // Who made it, in the footer the user sees on every screen — the owner asked for the authorship to
  // be everywhere the program describes itself, and the sidebar is on every screen.
  const app = (S.boot && S.boot.app) || {};
  const author = (app.author && app.authorEmail)
    ? '<p class="small muted" style="margin:6px 0 0;text-align:center">' + esc(t('about_author')) + ': ' +
      '<span title="' + esc(app.authorEmail) + '">' + esc(app.author) + '</span></p>'
    : '';
  document.getElementById('sidefoot').innerHTML =
    '<div class="pill ' + st.level + '" style="display:flex;justify-content:center">' + esc(st.label) + '</div>' +
    '<p class="small muted" style="margin:8px 0 0;text-align:center">' + esc(st.detail || '') + '</p>' +
    build + author +
    (S.boot.watch.runningByApp ? '<button class="btn small" style="width:100%;margin-top:8px" data-act="watch-stop">' + esc(t('watch_stop')) + '</button>'
      : '<button class="btn small" style="width:100%;margin-top:8px" data-act="watch-start">' + esc(t('watch_start')) + '</button>');
}

/* ------------------------------- dashboard */

function viewDashboard() {
  const b = S.boot;
  const st = protectionState();
  let h = '<div class="main-header"><h2>' + esc(t('nav_dashboard')) + '</h2><p>' + esc(t('tagline')) +
    ' · ' + esc(b.app.nowLocal || '') + '</p></div>';

  if (!b.archive.configured || !b.archive.isArchive) {
    h += '<div class="card"><h3>' + esc(t('welcome_title')) + '</h3><p class="hint">' + esc(t('welcome_hint')) + '</p>' +
      '<div class="row"><button class="btn primary big" data-act="archive-create">' + esc(t('choose_archive')) + '</button>' +
      '<button class="btn big" data-act="archive-use">' + esc(t('existing_archive')) + '</button></div>' +
      '<p class="small muted" style="margin-top:10px">' + esc(t('archive_hint')) + '</p></div>';
    return h;
  }

  h += '<div class="banner ' + st.level + '"><div><h3>' + esc(st.label) + '</h3><p>' + esc(st.detail) + '</p></div>' +
    '<div class="spacer"></div>' +
    (b.watch.runningByApp
      ? '<button class="btn" data-act="watch-stop">' + esc(t('watch_stop')) + '</button>'
      : '<button class="btn primary" data-act="watch-start">' + esc(t('watch_start')) + '</button>') +
    '</div>';

  if (!b.projects.length) {
    h += '<div class="empty"><p>' + esc(t('no_folders')) + '</p>' +
      '<button class="btn primary" data-act="nav" data-view="add">' + esc(t('add_folder')) + '</button></div>';
  } else {
    h += '<div class="grid">' + b.projects.map(projectCard).join('') + '</div>';
  }

  const hb = b.watch.heartbeat || {};
  h += '<div class="card"><h3>' + esc(t('attention')) + '</h3>' +
    '<div class="row between"><p class="hint" style="margin:0">' + esc(t('attention_hint')) + '</p>' +
    '<button class="btn" data-act="load-suggest">' + esc(t('attention_load')) + '</button></div>';
  if (S.suggest) {
    h += S.suggest.length
      ? '<div class="stack small" style="margin-top:8px">' + S.suggest.map((s) =>
          '<div>' + esc(String(s.text || '')) + '<div class="muted mono small">' + esc(String(s.command || '')) + '</div></div>').join('') + '</div>'
      : '<p class="hint" style="margin-top:8px">' + esc(t('attention_none')) + '</p>';
  }
  h += '</div>';
  h += '<div class="card tight"><h3>' + esc(t('store')) + '</h3>' +
    '<div class="kv">' +
    '<dt>' + esc(t('path')) + '</dt><dd class="mono">' + esc(b.archive.root) + '</dd>' +
    '<dt>' + esc(t('free')) + '</dt><dd>' + esc(bytes(b.archive.freeBytes)) + ' / ' + esc(bytes(b.archive.totalBytes)) + '</dd>' +
    '<dt>' + esc(t('interval')) + '</dt><dd>' + esc(String(b.config.intervalSeconds)) + ' s</dd>' +
    '<dt>' + esc(t('state')) + '</dt><dd>' + esc(hb.mode || '—') + (hb.ageMs !== undefined && hb.ageMs !== null ? ' · ' + esc(msAgo(hb.ageMs)) + ' ' + esc(t('ago')) : '') + '</dd>' +
    '</div>' + triggerLine(b) +
    '<div class="row" style="margin-top:10px"><button class="btn" data-act="nav" data-view="settings">' + esc(t('settings')) + '</button>' +
    '<button class="btn" data-act="run-pass">' + esc(t('run_pass')) + '</button></div>' +
    '</div>';
  return h;
}

function projectCard(p) {
  const state = p.state === 'active' ? 'ok' : (p.state === 'paused' ? 'warn' : 'err');
  const skipped = p.skippedByReason && Object.keys(p.skippedByReason).length
    ? Object.entries(p.skippedByReason).map(([k, v]) => esc(k) + ' ' + v).join(' · ') : '';
  return '<div class="card"><div class="row between"><h3>' + esc(p.name) + '</h3>' +
    '<span class="pill ' + state + '">' + esc(p.stateLabel || p.state) + '</span></div>' +
    '<p class="small mono muted" style="margin:2px 0 10px">' + esc(p.projectRoot) + '</p>' +
    '<div class="kv">' +
    '<dt>' + esc(t('versions')) + '</dt><dd>' + esc(String(p.versions)) + '</dd>' +
    '<dt>' + esc(t('last_observed')) + '</dt><dd>' + esc(p.lastObservedAt ? localFromMs(Date.parse(p.lastObservedAt)) : t('never')) + '</dd>' +
    '<dt>' + esc(t('archive_size')) + '</dt><dd>' + esc(bytes(p.archiveBytes)) + '</dd>' +
    (skipped ? '<dt>' + esc(t('skipped')) + '</dt><dd class="small">' + skipped + '</dd>' : '') +
    '</div>' +
    '<div class="row" style="margin-top:12px">' +
    '<button class="btn" data-act="open-project" data-name="' + esc(p.name) + '">' + esc(t('history')) + '</button>' +
    '<button class="btn" data-act="pause" data-name="' + esc(p.name) + '" data-pause="' + (p.state === 'paused' ? 'false' : 'true') + '">' +
    esc(p.state === 'paused' ? t('resume') : t('pause')) + '</button>' +
    '</div></div>';
}

/* ------------------------------- add folder */

function viewAdd() {
  const w = S.wizard || {};
  const steps = [t('step_choose'), t('step_coverage'), t('step_storage'), t('step_start')];
  const cur = Math.min(w.step || 1, 4);
  let h = '<div class="main-header"><h2>' + esc(t('add_title')) + '</h2><p>' + esc(t('add_hint')) + '</p></div>';
  h += '<div class="steps">' + steps.map((s, i) => {
    const n = i + 1;
    const cls = n < cur ? 'done' : (n === cur ? 'on' : '');
    return '<div class="' + cls + '">' + (n < cur ? '✓' : (n === cur ? '●' : '')) + ' ' + n + '&nbsp; ' + esc(s) + '</div>';
  }).join('') + '</div>';

  if (w.step === 5) {
    h += '<div class="card"><h3>' + esc(t('added')) + '</h3><p class="hint">' + esc(w.folder) + '</p>' +
      '<div class="row"><button class="btn primary" data-act="nav" data-view="dashboard">' + esc(t('nav_dashboard')) + '</button>' +
      (S.boot.watch.runningByApp ? '' : '<button class="btn" data-act="watch-start">' + esc(t('watch_start')) + '</button>') +
      '</div></div>';
    return h;
  }

  if (w.step === 1) {
    h += '<div class="card"><h3>' + esc(t('step_choose')) + '</h3><p class="hint">' + esc(t('selecting')) + '</p>' +
      '<button class="btn primary big" data-act="wizard-pick">' + esc(t('choose_folder')) + '</button>' +
      (w.folder ? '<p class="mono small" style="margin-top:10px">' + esc(w.folder) + '</p>' : '') + '</div>';
    return h;
  }

  const det = w.detection || {};
  const best = det.best || null;
  if (w.step === 2) {
    const pid = effectivePresetId();
    const pl = S.wizard.presetList || [];
    h += '<div class="card"><h3>' + esc(t('folder_to_protect')) + '</h3>' +
      '<p class="mono small" style="margin:0 0 10px">' + esc(w.folder) + '</p>' +
      '<label class="field">' + esc(t('name')) + '<input type="text" id="wiz-name" value="' + esc(w.name || '') + '"></label>' +
      '</div>';

    h += '<div class="card"><h3>' + esc(t('detected')) + '</h3>';
    if (best && !det.suggestCustom) {
      h += '<p class="hint">' + esc(best.id + ' / ' + best.displayName) + ' · ' + esc(String(best.confidence)) + '% ' + esc(t('confidence')) + '</p>';
    } else {
      h += '<p class="hint">' + esc(t('no_convincing')) + '</p>';
    }
    h += '<div class="stack">';
    const autoId = best && !det.suggestCustom ? best.id : 'custom';
    for (const p of pl) {
      const isAuto = p.id === autoId;
      h += '<label class="check"><input type="radio" name="preset" value="' + esc(p.id) + '" data-act="preset" ' +
        (pid === p.id ? 'checked' : '') + '> <span>' + esc(p.displayName) + ' <span class="muted small">(' + esc(p.id) + ')</span>' +
        (isAuto ? ' <span class="pill info">' + esc(t('detected')) + '</span>' : '') + '</span></label>';
    }
    h += '<label class="check"><input type="radio" name="preset" value="custom" data-act="preset" ' + (pid === 'custom' ? 'checked' : '') +
      '> <span>Custom (you name the extensions)</span></label>';
    h += '</div></div>';

    const exts = (det.extensionCounts || null) || (det.extensions || {});
    const keys = Object.keys(exts).filter((k) => k !== '(none)').sort();
    if (keys.length) {
      const preset = presetOf(pid);
      const inc = preset ? (preset.includeAfterPolicy || preset.include || []) : [];
      h += '<div class="card"><h3>' + esc(t('adjust')) + '</h3><p class="hint">' + esc(t('adjust_hint')) + '</p><div class="tagset">' +
        keys.map((e) => {
          const on = pid === 'custom' ? !S.wizard.editRemove.includes('*' + e) : inc.includes('*' + e);
          return '<label class="check" style="border:1px solid var(--line);border-radius:999px;padding:2px 9px">' +
            '<input type="checkbox" data-act="ext" data-ext="' + esc(e) + '" data-on="' + (on ? '1' : '0') + '" ' + (on ? 'checked' : '') + '>' +
            '<span class="mono">' + esc(e) + '</span> <span class="muted small">' + esc(String(exts[e])) + '</span></label>';
        }).join('') + '</div></div>';
    }
    if (w.preview) {
      h += previewBlock(w.preview);
    }
    h += '<div class="row"><button class="btn" data-act="wizard-back">' + esc(t('back')) + '</button>' +
      '<button class="btn primary" data-act="wizard-preview"' + (S.busy ? ' disabled' : '') + '>' + esc(t('continue')) + '</button></div>';
    return h;
  }

  if (w.step === 3) {
    h += previewBlock(w.preview || {});
    const arch = S.boot.archive;
    const sameVol = w.preview && w.preview.sameVolume;
    h += '<div class="card"><h3>' + esc(t('storage_title')) + '</h3>';
    if (arch.isArchive) {
      h += '<p class="mono small" style="margin:0 0 6px">' + esc(arch.root) + '</p>' +
        '<p class="hint">' + esc(bytes(arch.freeBytes)) + ' ' + esc(t('free')) + '</p>' +
        '<div class="banner ' + (sameVol ? 'warn' : 'ok') + '"><div><h3>' + esc(sameVol ? t('same_disk') : t('separate_disk')) + '</h3>' +
        '<p>' + esc(sameVol ? t('same_disk_hint') : '') + '</p></div></div>';
    } else {
      h += '<p class="hint">' + esc(t('archive_hint')) + '</p>' +
        '<button class="btn primary" data-act="archive-create">' + esc(t('choose_archive')) + '</button>';
    }
    h += '<div class="row" style="margin-top:12px"><button class="btn" data-act="wizard-back">' + esc(t('back')) + '</button>' +
      '<button class="btn primary" data-act="wizard-next"' + (!arch.isArchive ? ' disabled' : '') + '>' + esc(t('continue')) + '</button></div></div>';
    return h;
  }

  // step 4
  const arch = S.boot.archive;
  const pv = w.preview || {};
  h += '<div class="card"><h3>' + esc(t('step_start')) + '</h3><div class="kv">' +
    '<dt>' + esc(t('summary_folder')) + '</dt><dd class="mono">' + esc(w.folder) + '</dd>' +
    '<dt>' + esc(t('summary_coverage')) + '</dt><dd>' + esc(effectivePresetId()) + ' · ' + esc(String(pv.files || 0)) + ' ' + esc(t('files')) + ' · ' + esc(bytes(pv.bytes)) + '</dd>' +
    '<dt>' + esc(t('summary_archive')) + '</dt><dd class="mono">' + esc(arch.root || t('not_yet')) + '</dd>' +
    '<dt>' + esc(t('summary_excluded')) + '</dt><dd class="small">' + esc(excludedLine(pv)) + '</dd>' +
    '</div><p class="hint" style="margin-top:12px">' + esc(t('start_note')) + '</p>' +
    '<div class="row"><button class="btn" data-act="wizard-back">' + esc(t('back')) + '</button>' +
    '<button class="btn primary big" data-act="wizard-start"' + (S.busy ? ' disabled' : '') + '>' + esc(t('start_protecting')) + '</button></div></div>';
  return h;
}

function excludedLine(pv) {
  const s = pv.skippedByReason || {};
  const hard = { secret: 'secrets (never copied)', temporary: 'temporary files', too_large: 'files over the size limit' };
  const known = Object.keys(s).map((k) => (hard[k] || k) + ' (' + s[k] + ')');
  return known.length ? known.join(', ') : '—';
}

function previewBlock(pv) {
  const s = pv.skippedByReason || {};
  const hard = { secret: 'secrets (passwords, keys) — never copied', temporary: 'temporary files', too_large: 'very large files', hidden: 'hidden files', ignored_dir: 'dependency and hidden folders', not_in_preset: 'types not in the chosen list', binary: 'binary files', rule: 'excluded by a rule' };
  let h = '<div class="card"><h3>' + esc(String(pv.files || 0)) + ' ' + esc(t('will_protect')) + '</h3>' +
    '<p class="hint">' + esc(t('estimated_size')) + ': ' + esc(bytes(pv.bytes)) + '</p>';
  const keys = Object.keys(s);
  if (keys.length) {
    h += '<h3 style="font-size:14px;margin-top:10px">' + esc(t('not_protected')) + '</h3><div class="stack small">' +
      keys.map((k) => '<div><strong>' + esc(hard[k] || k) + '</strong> <span class="muted">— ' + esc(String(s[k])) + ' path(s)</span></div>').join('') +
      '</div>';
  }
  const top = pv.topSkippedDirs || [];
  if (top.length) {
    h += '<p class="small muted" style="margin-top:8px">' + top.map((d) => esc(d.path) + ' (' + esc(d.reason) + ', ' + d.files + ')').join(' · ') + '</p>';
  }
  h += '</div>';
  return h;
}

/* ------------------------------- project view */

function viewProject() {
  if (!S.project) {
    const ps = S.boot.projects || [];
    let h = '<div class="main-header"><h2>' + esc(t('nav_restore')) + '</h2><p>' + esc(t('moments')) + '</p></div>';
    if (!ps.length) return h + '<div class="empty"><p>' + esc(t('no_folders')) + '</p></div>';
    h += '<div class="grid">' + ps.map(projectCard).join('') + '</div>';
    return h;
  }
  const d = S.projectDetail || { moments: [] };
  const tabs = [['history', t('tab_history')], ['versions', t('tab_versions')], ['compare', t('tab_compare')], ['tools', t('tab_tools')]];
  const tabBar = '<div class="tabs">' + tabs.map(([v, label]) =>
    '<button data-act="tab" data-tab="' + v + '" class="' + (S.tab === v ? 'active' : '') + '">' + esc(label) + '</button>').join('') + '</div>';
  let h = '<div class="main-header"><h2>' + esc(S.project) + '</h2><p>' + esc(t('history')) + ' · ' + esc(String(d.events || 0)) + '</p></div>' + tabBar;
  if (S.tab === 'versions') return h + viewVersionsTab();
  if (S.tab === 'compare') return h + viewCompareTab();
  if (S.tab === 'tools') return h + viewToolsTab();
  h += '<div class="card"><div class="row between"><h3>' + esc(t('moments')) + '</h3>' +
    '<div class="row"><input type="datetime-local" id="pick-moment" placeholder="' + esc(t('moment_hint')) + '"><button class="btn" data-act="load-picked">' + esc(t('load_moment')) + '</button></div></div>';
  if (!d.moments || !d.moments.length) {
    h += '<p class="hint">' + esc(t('no_moments')) + '</p>';
  } else {
    h += '<div class="timeline scroll">' + d.moments.map((m) =>
      '<button data-act="moment" data-at="' + esc(m.atIso) + '" class="' + (S.moment && S.moment.atIso === m.atIso ? 'active' : '') + '">' +
      '<span class="when">' + esc(localFromMs(m.at)) + '</span>' +
      '<span class="what">+' + m.puts + ' ~' + m.moves + ' −' + m.deletes + (m.skips ? ' (' + m.skips + ' ' + esc(t('skipped')) + ')' : '') + '</span></button>').join('') + '</div>';
  }
  h += '</div>';

  if (S.tree) {
    const sel = S.selected || new Set();
    h += '<div class="card"><div class="row between"><h3>' + esc(t('tree_at')) + ' · ' + esc(localFromMs(Date.parse(S.moment.atIso))) + '</h3>' +
      '<div class="row"><span class="muted small">' + S.tree.length + ' ' + esc(t('files')) + '</span>' +
      '<button class="btn" data-act="select-all">' + esc(t('select_all')) + '</button>' +
      '<button class="btn primary" data-act="restore"' + (sel.size ? '' : ' disabled') + '>' + esc(t('restore_go')) + ' (' + sel.size + ')</button></div></div>' +
      '<div class="scroll"><table><thead><tr><th style="width:28px"></th><th>' + esc(t('path')) + '</th><th class="num">' + esc(t('size')) + '</th><th>' + esc(t('kind')) + '</th></tr></thead><tbody>' +
      S.tree.map((f) => '<tr class="' + (sel.has(f.path) ? 'selected' : '') + '"><td><input type="checkbox" data-act="file" data-path="' + esc(f.path) + '" ' + (sel.has(f.path) ? 'checked' : '') + '></td>' +
        '<td class="mono">' + esc(f.path) + '</td><td class="num">' + esc(bytes(f.size)) + '</td><td class="small muted">' + esc(f.kind) + '</td></tr>').join('') +
      '</tbody></table></div>' +
      '<div class="row" style="margin-top:12px"><button class="btn" data-act="restore-dest">' + esc(t('restore_to')) + '…</button>' +
      '<span class="mono small">' + esc(S.restoreTo || '') + '</span></div>' +
      '<div class="row" style="margin-top:10px"><button class="btn primary big" data-act="restore"' + (sel.size ? '' : ' disabled') + '>' +
      esc(t('restore_selected')) + '</button></div></div>';
  } else {
    h += '<div class="empty"><p>' + esc(t('no_tree')) + '</p></div>';
  }

  h += '<div class="card"><h3>' + esc(t('export')) + '</h3><p class="hint">' + esc(t('export_hint')) + '</p>' +
    '<button class="btn" data-act="export">' + esc(t('export_go')) + '</button></div>';
  return h;
}

/// The trigger the core itself reports: the notification backend where one exists, the periodic
/// pass where one does not. This is the program's own sentence, printed unaltered.
function triggerLine(b) {
  const tr = (b.watch && b.watch.trigger) || null;
  const describe = tr && tr.describe ? String(tr.describe) : '';
  const interval = b.config && b.config.intervalSeconds ? String(b.config.intervalSeconds) : '';
  if (!describe && !interval) return '';
  return '<p class="small muted">' +
    esc(describe || '') +
    (interval ? (describe ? ' · ' : '') + 'interval ' + esc(interval) + ' s' : '') +
    '</p>';
}

/* ------------------------------- diagnostics & settings */

function viewDiagnostics() {
  let h = '<div class="main-header"><h2>' + esc(t('diagnostics')) + '</h2><p>' + esc(protectionState().detail) + '</p></div>';
  h += '<div class="card"><div class="row between"><h3>' + esc(t('doctor')) + '</h3>' +
    '<button class="btn" data-act="load-doctor">' + esc(t('doctor')) + '</button></div>';
  if (S.doctor) {
    h += '<div class="stack small">' + (S.doctor.checks || []).map((c) =>
      '<div><span class="pill ' + (c.level === 'OK' ? 'ok' : (c.level === 'WARN' ? 'warn' : 'err')) + '">' + esc(c.level) + '</span> ' +
      esc(c.text) + (c.fix ? '<div class="muted mono small">fix: ' + esc(c.fix) + '</div>' : '') + '</div>').join('') + '</div>';
  } else {
    h += '<p class="hint">—</p>';
  }
  h += '</div>';
  h += '<div class="card"><div class="row between"><h3>' + esc(t('health_load')) + '</h3>' +
    '<button class="btn" data-act="load-health">' + esc(t('health_load')) + '</button></div>';
  if (S.health) {
    h += '<div class="stack small">' + S.health.map((c) =>
      '<div><span class="pill ' + (c.level === 'OK' ? 'ok' : (c.level === 'WARN' ? 'warn' : 'err')) + '">' + esc(c.level) + '</span> ' +
      esc(c.text) + (c.fix ? '<div class="muted mono small">' + esc(c.fix) + '</div>' : '') + '</div>').join('') + '</div>';
  }
  h += '</div>';
  h += '<div class="card"><div class="row between"><h3>' + esc(t('config_all_load')) + '</h3>' +
    '<button class="btn" data-act="load-config">' + esc(t('config_all_load')) + '</button></div>';
  if (S.configAll) {
    h += '<div class="kv">' + Object.keys(S.configAll).map((k) =>
      '<dt class="mono">' + esc(k) + '</dt><dd>' + esc(String(S.configAll[k])) + '</dd>').join('') + '</div>';
  }
  h += '</div>';
  h += cliOnlyCard();
  h += '<div class="card"><div class="row between"><h3>' + esc(t('daemon_log')) + '</h3>' +
    '<button class="btn" data-act="load-log">' + esc(t('daemon_log')) + '</button></div>';
  if (S.log) {
    h += '<div class="log">' + S.log.lines.map(esc).join('\n') + '</div>';
  }
  h += '</div>';
  return h;
}

function viewSettings() {
  const b = S.boot;
  let h = '<div class="main-header"><h2>' + esc(t('settings')) + '</h2><p>' + esc(b.archive.root || t('not_yet')) + '</p></div>';
  h += '<div class="card"><h3>' + esc(t('storage_title')) + '</h3>' +
    '<p class="mono small">' + esc(b.archive.root || t('not_yet')) + '</p>' +
    '<p class="hint">' + esc(bytes(b.archive.freeBytes)) + ' ' + esc(t('free')) + ' · ' + esc(b.app.platform) + ' · core ' + esc(b.app.coreVersion) + '</p>' +
    '<div class="row"><button class="btn" data-act="archive-use">' + esc(t('existing_archive')) + '</button>' +
    '<button class="btn" data-act="archive-create">' + esc(t('create_archive')) + '</button></div></div>';

  h += '<div class="card"><h3>' + esc(t('interval')) + '</h3><p class="hint">' + esc(t('interval_hint')) + '</p>' +
    '<div class="row"><input type="number" id="set-interval" min="1" max="60" value="' + esc(String(b.config.intervalSeconds || 5)) + '">' +
    '<button class="btn" data-act="save-interval">' + esc(t('save_interval')) + '</button></div>' +
    '<p class="hint">' + esc(t('save_interval_live')) + '</p>' +
    (b.watch.configState && b.watch.configState.reloads > 0
      ? '<p class="small muted">' + esc(t('config_reread')) + ': ' + esc(String(b.watch.configState.reloads)) +
        ' · ' + esc(String(b.watch.configState.atIso || '')) +
        ' · ' + esc(String((b.watch.configState.changedKeys || []).join(', '))) + '</p>'
      : '') +
    '<p class="small muted">' + esc(String(b.watch.mode || '')) + '</p>' +
    triggerLine(b) + '</div>';

  h += '<div class="card"><h3>' + esc(t('notifications')) + '</h3>' +
    '<label class="check"><input type="checkbox" data-act="config-bool" data-key="notifications" ' + (b.config.notifications ? 'checked' : '') + '> ' +
    '<span class="mono">notifications = ' + esc(String(b.config.notifications)) + '</span></label>' +
    '<p class="small muted">The core writes into the system log; the menu bar item shows the state.</p></div>';

  h += '<div class="card"><h3>' + esc(t('language')) + '</h3><div class="row">' +
    ['en', 'ru'].map((l) => '<label class="check"><input type="radio" name="lang" data-act="lang" value="' + l + '" ' + (LANG === l ? 'checked' : '') + '> <span>' + (l === 'en' ? 'English' : 'Русский') + '</span></label>').join('') +
    '</div></div>';

  h += '<div class="card"><h3>' + esc(t('details')) + '</h3><div class="kv">' +
    '<dt>app</dt><dd class="mono">' + esc(b.app.version) + '</dd>' +
    '<dt>core</dt><dd class="mono">' + esc(b.app.coreVersion) + '</dd>' +
    '<dt>location file</dt><dd class="mono small">' + esc(b.archive.locationFile) + '</dd>' +
    '<dt>daemon log</dt><dd class="mono small">' + esc(b.daemonLog) + '</dd>' +
    '</div><div class="row" style="margin-top:10px">' +
    '<button class="btn" data-act="toggle-raw">' + esc(t('show_raw')) + ': ' + (S.raw ? 'on' : 'off') + '</button>' +
    '</div></div>';
  return h;
}

function viewImport() {
  let h = '<div class="main-header"><h2>' + esc(t('importing')) + '</h2><p>' + esc(t('import_hint')) + '</p></div>';
  h += '<div class="card"><p class="hint">' + esc(t('import_hint')) + '</p>' +
    '<button class="btn primary" data-act="import">' + esc(t('import_go')) + '</button></div>';
  return h;
}


/* ==========================================================================================
 * Round 297 — the parts of the core the window used to hide.
 *
 * History: the versions of one file, the content of any one of them, and a comparison between
 * two moments. Integrity: whether the archive can still be read back. Storage: what it costs and
 * what the retention policy would remove. Each control calls one core command; each number on
 * the screen is the core's answer, printed here unaltered.
 * ========================================================================================== */

function askConfirm(title, detail) {
  return new Promise(function (resolve) {
    const back = document.createElement('div');
    back.className = 'modal-back';
    back.innerHTML =
      '<div class="modal" role="dialog" aria-modal="true">' +
      '<h3>' + esc(title) + '</h3>' +
      (detail ? '<p class="hint">' + esc(detail) + '</p>' : '') +
      '<div class="modal-actions">' +
      '<button class="btn primary" id="pl-c-ok">' + esc(t('confirm')) + '</button>' +
      '<button class="btn" id="pl-c-no">' + esc(t('cancel')) + '</button>' +
      '</div></div>';
    document.body.appendChild(back);
    const finish = function (v) { back.remove(); resolve(v); };
    back.querySelector('#pl-c-ok').onclick = function () { finish(true); };
    back.querySelector('#pl-c-no').onclick = function () { finish(false); };
    back.querySelector('#pl-c-ok').focus();
  });
}

function shortHash(h) { return (h || '').slice(0, 12); }

/// Which project the tools on this page point at.
///
/// Round 297: the selects used to be rebuilt on every redraw, and a rebuilt <select> forgets what
/// was chosen — so picking a project and then doing anything else silently moved the choice back to
/// the first one. The choice is kept here instead, and every select is drawn with it.
function toolProject() {
  if (S.toolProject) return S.toolProject;
  if (S.project) return S.project;
  const first = (S.boot && S.boot.projects && S.boot.projects[0]) || null;
  return first ? first.name : '';
}

function listCard(title, items, kind) {
  if (!items || !items.length) return '<p class="small muted">0 ' + esc(t(kind)) + '</p>';
  return '<p class="small"><strong>' + items.length + '</strong> ' + esc(t(kind)) + '</p>' +
    '<div class="scroll small mono">' + items.slice(0, 200).map((x) => esc(typeof x === 'string' ? x : JSON.stringify(x))).join('<br>') + '</div>';
}

async function loadVersions(path) {
  S.file = path; S.versions = null; S.content = null; S.diff = null; S.error = null;
  render();
  try {
    const n = encodeURIComponent(S.project);
    S.versions = await api('project/why?name=' + n + '&path=' + encodeURIComponent(path));
    if (S.versions && S.versions.versionCount) {
      S.content = await api('project/file?name=' + n + '&path=' + encodeURIComponent(path));
    }
  } catch (e) { S.error = e.message; }
  render();
}

async function loadContentAt(atIso) {
  try {
    S.content = await api('project/file?name=' + encodeURIComponent(S.project) +
      '&path=' + encodeURIComponent(S.file) + '&at=' + encodeURIComponent(atIso));
  } catch (e) { S.error = e.message; }
  render();
}

async function compareVersion(atIso, prevIso) {
  try {
    S.diff = await api('project/diff?name=' + encodeURIComponent(S.project) +
      '&from=' + encodeURIComponent(prevIso || atIso) + '&to=' + encodeURIComponent(atIso));
  } catch (e) { S.error = e.message; }
  render();
}

async function runCompare() {
  const a = (document.getElementById('cmp-from') || {}).value;
  const b = (document.getElementById('cmp-to') || {}).value;
  const now = (document.getElementById('cmp-now') || {}).checked;
  const fromMs = momentFromInput(a);
  if (fromMs === null) {
    S.error = t('bad_moment') + (a ? ': ' + a : '');
    render();
    return;
  }
  const from = isoUtc(fromMs);
  let q = 'project/diff?name=' + encodeURIComponent(S.project) + '&from=' + encodeURIComponent(from);
  if (!now && b) {
    const toMs = momentFromInput(b);
    if (toMs === null) { S.error = t('bad_moment') + ': ' + b; render(); return; }
    q += '&to=' + encodeURIComponent(isoUtc(toMs));
  }
  try { S.diff = await api(q); } catch (e) { S.error = e.message; }
  render();
}

function diffCard() {
  const d = S.diff;
  if (!d) return '';
  let h = '<div class="card"><h3>' + esc(t('run_compare')) + '</h3>';
  h += '<p class="small muted mono">' + esc(String(d.fromLocal || d.from || '')) + ' → ' + esc(String(d.toLocal || d.to || '')) + '</p>';
  h += '<div class="kv"><dt>' + esc(t('added')) + '</dt><dd>' + (d.added || []).length + '</dd>' +
    '<dt>' + esc(t('changed')) + '</dt><dd>' + (d.changed || []).length + '</dd>' +
    '<dt>' + esc(t('removed')) + '</dt><dd>' + (d.removed || []).length + '</dd></div>';
  h += listCard(t('changed'), d.changed, 'changed');
  h += listCard(t('added'), d.added, 'added');
  h += listCard(t('removed'), d.removed, 'removed');
  return h + '</div>';
}

/* ------------------------------- the project's three extra tabs */

function viewVersionsTab() {
  let h = '';
  if (!S.tree || !S.tree.length) {
    return '<div class="empty"><p>' + esc(t('no_tree')) + '</p></div>';
  }
  const filter = (S.filter || '').toLowerCase();
  const files = S.tree.filter((f) => !filter || f.path.toLowerCase().indexOf(filter) >= 0);
  h += '<div class="card"><div class="row between"><h3>' + esc(t('pick_file')) + '</h3>' +
    '<input id="flt" placeholder="' + esc(t('filter_paths')) + '" value="' + esc(S.filter || '') + '" data-act="filter-input">' +
    '<span class="muted small">' + files.length + ' / ' + S.tree.length + '</span></div>' +
    '<div class="scroll" style="max-height:240px"><table><tbody>' +
    files.slice(0, 500).map((f) => '<tr class="' + (S.file === f.path ? 'selected' : '') + '">' +
      '<td><button class="btn small" data-act="open-file" data-path="' + esc(f.path) + '">' + esc(t('versions_of')) + '</button></td>' +
      '<td class="mono">' + esc(f.path) + '</td><td class="num">' + esc(bytes(f.size)) + '</td></tr>').join('') +
    '</tbody></table></div></div>';

  if (!S.file) return h + '<div class="empty"><p>' + esc(t('no_file')) + '</p></div>';
  const v = S.versions || { versions: [] };
  h += '<div class="card"><h3>' + esc(t('versions_of')) + ' <span class="mono">' + esc(S.file) + '</span></h3>';
  if (v.decision && !v.decision.track) {
    h += '<p class="small warn">' + esc(t('check_result')) + ': not tracked — ' + esc(v.decision.reason || '') + ' (' + esc(v.decision.rule || '') + ')</p>';
  }
  if (!v.versionCount) {
    h += '<p class="hint">' + esc(t('nothing_stored')) + '</p>';
  } else {
    h += '<div class="scroll"><table><thead><tr><th>' + esc(t('moment')) + '</th><th class="num">' + esc(t('size')) + '</th>' +
      '<th>' + esc(t('hash')) + '</th><th></th></tr></thead><tbody>' +
      v.versions.slice().reverse().map((row, i, arr) => {
        const prev = arr[i + 1];
        // The design labels the two ends of the list; the relative time is what a person reads first.
        const tag = i === 0 ? t('v_latest') : (i === arr.length - 1 ? t('v_first') : '');
        return '<tr><td>' + esc(row.atLocal) +
          ' <span class="muted small">· ' + esc(msAgo(Date.now() - row.at)) + ' ' + esc(t('ago')) + '</span>' +
          (tag ? ' <span class="pill">' + esc(tag) + '</span>' : '') + '</td>' +
          '<td class="num">' + esc(bytes(row.size)) + '</td>' +
          '<td class="mono small">' + esc(shortHash(row.hash)) + '</td><td>' +
          '<button class="btn small" data-act="show-version" data-at="' + esc(row.atIso) + '">' + esc(t('show_version')) + '</button>' +
          // The design's primary action for a version: compare it with the folder as it is now.
          (i === 0 ? '' : ' <button class="btn small" data-act="cmp-current-version" data-at="' + esc(row.atIso) + '">' + esc(t('compare_current')) + '</button>') +
          (prev ? ' <button class="btn small" data-act="cmp-version" data-at="' + esc(row.atIso) + '" data-prev="' + esc(prev.atIso) + '">' + esc(t('compare_prev')) + '</button>' : '') +
          '</td></tr>';
      }).join('') + '</tbody></table></div>';
  }
  h += '</div>';

  if (S.content) {
    const c = S.content;
    h += '<div class="card"><div class="row between"><h3>' + esc(t('content_at')) + ' ' + esc(c.atLocal || '') + '</h3>' +
      '<span class="small muted mono">' + esc(String(c.bytes)) + ' B · ' + esc(shortHash(c.sha256)) + ' · ' + esc(c.encoding) + '</span></div>';
    if (c.text === null || c.text === undefined) {
      h += '<p class="hint">' + esc(t('binary_content')) + '</p>';
    } else {
      h += '<pre class="code">' + esc(c.text) + '</pre>';
      if (c.truncated) h += '<p class="small muted">' + esc(t('truncated_content')) + '</p>';
    }
    h += '</div>';
  }
  h += diffCard();
  return h;
}

function viewCompareTab() {
  let h = '<div class="card"><h3>' + esc(t('run_compare')) + '</h3><p class="hint">' + esc(t('compare_hint')) + '</p>' +
    '<div class="row"><label class="check">' + esc(t('diff_from')) + ' <input type="datetime-local" id="cmp-from" placeholder="' + esc(t('moment_hint')) + '"></label>' +
    '<label class="check">' + esc(t('diff_to')) + ' <input type="datetime-local" id="cmp-to" placeholder="' + esc(t('moment_hint')) + '"></label>' +
    '<label class="check"><input type="checkbox" id="cmp-now"> ' + esc(t('diff_current')) + '</label>' +
    '<button class="btn primary" data-act="run-compare">' + esc(t('run_compare')) + '</button></div></div>';
  h += diffCard();
  return h;
}

function viewToolsTab() {
  const d = S.projectDetail || {};
  let h = '<div class="card"><h3>' + esc(t('note_label')) + '</h3>' +
    '<textarea id="pl-note" rows="3" style="width:100%">' + esc(S.noteDraft !== null && S.noteDraft !== undefined ? S.noteDraft : ((S.boot.projects.find((p) => p.name === S.project) || {}).note || '')) + '</textarea>' +
    '<div class="row" style="margin-top:8px"><button class="btn primary" data-act="note-save">' + esc(t('note_save')) + '</button></div></div>';

  h += '<div class="card"><h3>' + esc(t('mark_label')) + '</h3>' +
    '<div class="row"><input id="pl-mark" placeholder="' + esc(t('mark_label')) + '">' +
    '<button class="btn" data-act="mark-save">' + esc(t('mark_save')) + '</button></div>' +
    '<p class="small muted">' + esc(t('pick_moment')) + '</p></div>';

  h += '<div class="card"><h3>' + esc(t('last_good_load')) + '</h3>';
  if (S.lastGood) {
    h += S.lastGood.found
      ? '<p>' + esc(String(S.lastGood.atLocal)) + ' — ' + esc(String(S.lastGood.why)) + '</p>'
      : '<p class="hint">' + esc(t('last_good_none')) + '</p>';
  }
  h += '<button class="btn" data-act="last-good">' + esc(t('last_good_load')) + '</button></div>';

  h += '<div class="card"><h3>' + esc(t('tab_tools')) + '</h3><div class="row">' +
    '<button class="btn" data-act="apply-filters">' + esc(t('apply_filters_run')) + '</button>' +
    '<button class="btn" data-act="relink">' + esc(t('relink_run')) + '</button>' +
    '<button class="btn" data-act="remove-project">' + esc(t('remove_run')) + '</button>' +
    '</div><p class="small muted">' + esc(t('remove_confirm')) + '</p>' +
    (S.filtersResult ? '<div class="log small">' + esc(JSON.stringify(S.filtersResult, null, 1)) + '</div>' : '') + '</div>';

  h += '<div class="card"><h3>' + esc(t('recent_load')) + '</h3>' +
    '<button class="btn" data-act="load-recent">' + esc(t('recent_load')) + '</button>' +
    (S.recent ? '<div class="stack small">' + S.recent.map((r) =>
      '<div>' + esc(String(r.atLocal || r.at || '')) + ' · <span class="mono">' + esc(String(r.kind || '')) + '</span> · ' + esc(String(r.text || r.project || '')) + '</div>').join('') + '</div>' : '') +
    '</div>';
  return h;
}

/* ------------------------------- integrity */

function viewIntegrity() {
  let h = '<div class="main-header"><h2>' + esc(t('nav_integrity')) + '</h2><p>' + esc(t('check_result')) + ' · ' + esc(t('audit_run')) + '</p></div>';
  const ps = (S.boot.projects || []);

  h += '<div class="card"><h3>' + esc(t('check_fast')) + '</h3>' +
    '<div class="row"><button class="btn" data-act="check" data-deep="0">' + esc(t('check_fast')) + '</button>' +
    '<button class="btn" data-act="check" data-deep="1">' + esc(t('check_deep')) + '</button></div>';
  if (S.check) {
    h += '<div class="stack">' + S.check.map((r) => {
      const bad = (r.missingBlobs || []).length + (r.corruptedBlobs || []).length;
      return '<div class="card tight"><div class="row between"><h3>' + esc(r.project) + '</h3>' +
        (bad === 0 ? '<span class="pill ok">' + esc(t('all_present')) + '</span>' : '<span class="pill err">' + bad + ' ' + esc(t('check_result')) + '</span>') + '</div>' +
        '<div class="kv"><dt>' + esc(t('versions')) + '</dt><dd>' + r.versions + '</dd>' +
        '<dt>' + esc(t('size')) + '</dt><dd>' + esc(bytes(r.bytes)) + '</dd>' +
        '<dt>' + esc(t('dangling_blobs')) + '</dt><dd>' + (r.danglingBlobs || []).length + '</dd></div>' +
        listCard('', r.missingBlobs, 'missing_blobs') + listCard('', r.corruptedBlobs, 'corrupted_blobs') +
        '</div>';
    }).join('') + '</div>';
  }
  h += '</div>';

  h += '<div class="card"><h3>' + esc(t('drill_run')) + '</h3><p class="hint">' + esc(t('drill_hint')) + '</p>' +
    '<div class="row"><select id="drill-project" data-act="pick-project">' + ps.map((p) => '<option' + (toolProject() === p.name ? ' selected' : '') + '>' + esc(p.name) + '</option>').join('') + '</select>' +
    '<button class="btn" data-act="drill">' + esc(t('drill_run')) + '</button></div></div>';

  h += '<div class="card"><h3>' + esc(t('audit_run')) + '</h3><p class="hint">' + esc(t('audit_hint')) + '</p>' +
    '<button class="btn" data-act="audit">' + esc(t('audit_run')) + '</button>';
  if (S.audit) {
    h += '<p>' + esc(String(S.audit.filesHashed)) + ' ' + esc(t('files')) + ' · ' +
      (S.audit.ok ? '<span class="pill ok">' + esc(t('audit_ok')) + '</span>' : '<span class="pill err">' + (S.audit.differences || []).length + ' ' + esc(t('changed')) + '</span>') + '</p>' +
      listCard('', S.audit.differences, 'changed');
  }
  h += '</div>';

  h += '<div class="card"><h3>' + esc(t('quarantine_load')) + '</h3>' +
    '<button class="btn" data-act="quarantine">' + esc(t('quarantine_load')) + '</button>';
  if (S.quarantine) {
    h += '<p class="small">' + String(S.quarantine.count) + '</p>' +
      ((S.quarantine.count ? '' : '<p class="hint">' + esc(t('quarantine_none')) + '</p>')) +
      '<div class="scroll small mono">' + (S.quarantine.entries || []).map((e) => esc(e.project + '/' + e.hash + ' ' + bytes(e.bytes))).join('<br>') + '</div>';
  }
  h += '</div>';

  h += '<div class="card"><h3>' + esc(t('gc_run')) + '</h3>' +
    '<button class="btn" data-act="gc">' + esc(t('gc_run')) + '</button>';
  if (S.gc) h += '<p class="small mono">' + esc(JSON.stringify(S.gc)) + '</p>';
  h += '</div>';

  h += '<div class="card"><h3>' + esc(t('recover_run')) + '</h3><p class="hint">' + esc(t('recover_confirm')) + '</p>' +
    '<div class="row"><select id="recover-project" data-act="pick-project">' + ps.map((p) => '<option' + (toolProject() === p.name ? ' selected' : '') + '>' + esc(p.name) + '</option>').join('') + '</select>' +
    '<button class="btn" data-act="recover">' + esc(t('recover_run')) + '</button></div>' +
    (S.recovered ? '<div class="log small">' + esc(JSON.stringify(S.recovered, null, 1)) + '</div>' : '') + '</div>';
  return h;
}

/* ------------------------------- storage & retention */

function viewStorage() {
  const b = S.boot;
  let h = '<div class="main-header"><h2>' + esc(t('nav_storage')) + '</h2><p class="mono small">' + esc(b.archive.root || '') + '</p></div>';

  h += '<div class="card"><h3>' + esc(t('storage_space')) + '</h3><div class="kv">' +
    '<dt>' + esc(t('store')) + '</dt><dd class="mono">' + esc(b.archive.root || t('not_yet')) + '</dd>' +
    '<dt>' + esc(t('free_space')) + '</dt><dd>' + esc(bytes(b.archive.freeBytes)) + ' / ' + esc(bytes(b.archive.totalBytes)) + '</dd>' +
    '</div><p class="small muted">' + esc(t('threshold_note')) + '</p></div>';

  h += '<div class="card"><h3>' + esc(t('storage_by_project')) + '</h3>' +
    '<button class="btn" data-act="load-size">' + esc(t('storage_load')) + '</button>';
  if (S.size) {
    h += '<div class="scroll"><table><thead><tr><th>' + esc(t('name')) + '</th><th class="num">' + esc(t('size')) + '</th>' +
      '<th class="num">' + esc(t('versions')) + '</th><th class="num">blobs</th><th>' + esc(t('last_observed')) + '</th></tr></thead><tbody>' +
      S.size.map((r) => '<tr><td class="mono">' + esc(r.name) + '</td><td class="num">' + esc(bytes(r.bytes)) + '</td>' +
        '<td class="num">' + r.versions + '</td><td class="num">' + r.blobs + '</td>' +
        '<td class="small muted">' + esc(r.lastObservedAt || '—') + '</td></tr>').join('') + '</tbody></table></div>';
  }
  h += '</div>';

  const ps = (b.projects || []);
  h += '<div class="card"><h3>' + esc(t('retention_title')) + '</h3><p class="hint">' + esc(t('retention_saved_note')) + '</p>' +
    '<div class="row"><select id="ret-project" data-act="pick-project">' + ps.map((p) => '<option' + (toolProject() === p.name ? ' selected' : '') + '>' + esc(p.name) + '</option>').join('') + '</select>' +
    '<button class="btn" data-act="retention-load">' + esc(t('retention_title')) + '</button></div>';
  if (S.retention) {
    const r = S.retention;
    if (!r.stored) h += '<p class="hint" style="margin-top:10px">' + esc(t('retention_none')) + '</p>';
    h += '<div class="kv" style="margin-top:10px">' +
      '<dt>' + esc(t('retention_stored')) + '</dt><dd class="mono">' + esc(r.policy || '—') + '</dd>' +
      '<dt>' + esc(t('retention_applied')) + '</dt><dd>' + esc(r.appliedAtLocal || t('retention_never')) + '</dd>' +
      ((r.plan || r.planError) ? '<dt>' + esc(t('versions_kept')) + '</dt><dd>' +
        (r.plan ? (r.plan.versionsKept + ' / ' + r.plan.versionsBefore + ' · ' + esc(t('dropped')) + ' ' + r.plan.dropped) : esc(String(r.planError))) + '</dd>' : '') +
      ((r.plan) ? '<dt>' + esc(t('new_history_start')) + '</dt><dd>' + esc(r.plan.newHistoryStartsAtLocal) + '</dd>' : '') +
      '</div>';
    if (r.plan && r.plan.windows) {
      h += '<div class="scroll small"><table><thead><tr><th>age</th><th>keep</th><th class="num">buckets</th><th class="num">kept</th><th class="num">' + esc(t('dropped')) + '</th></tr></thead><tbody>' +
        r.plan.windows.map((w) => '<tr><td>' + w.fromDays + '-' + w.toDays + 'd</td><td>' + esc(w.keep) + '</td>' +
          '<td class="num">' + w.buckets + '</td><td class="num">' + w.kept + '</td><td class="num">' + w.dropped + '</td></tr>').join('') +
        '</tbody></table></div>';
    }
  }
  h += '<div class="row" style="margin-top:10px"><input id="ret-policy" placeholder="7d:all,30d:1/day,365d:1/month" value="' + esc((S.retention && S.retention.policy) || '') + '">' +
    '<button class="btn" data-act="retention-save">' + esc(t('retention_save')) + '</button>' +
    '<button class="btn" data-act="retention-preview">' + esc(t('retention_preview')) + '</button>' +
    '<button class="btn" data-act="retention-apply">' + esc(t('retention_apply')) + '</button></div>' +
    '<p class="small muted">' + esc(t('retention_apply_confirm')) + '</p>';
  if (S.prune) {
    h += '<div class="log small">' + esc(JSON.stringify(S.prune, null, 1)) + '</div>';
  }
  h += '</div>';
  return h;
}

/* ------------------------------- the honest list of what stays out of the window
 *
 * Round 299 gave the window a menu over the core's functions, which makes this list shorter — and
 * makes keeping it true more important, not less. Every row here is a command the menu does *not*
 * run; where the menu shows the command instead of running it, the row says so.
 */

function cliOnlyCard() {
  const rows = [
    ['pl panic <project>', 'it asks a terminal which moment to fall back to; the window does the same job through History & restore'],
    ['pl export-and-prune', 'two irreversible things in one command: the window keeps them apart'],
    ['pl archive-move / archive-delete', 'they move or delete the whole archive; one wrong click costs everything'],
    ['pl daemon install', 'installing a background service (launchd/systemd) is a system change the person makes, not a window — the menu starts and stops the app’s own process'],
    ['pl-mcp', 'a read-only server for other agents, started by the agent that needs it; the menu lists what it registers (pl mcp tools)'],
    ['pl watch <project> --for S', 'a live terminal stream; the window has the log and the trigger state instead'],
    ['pl partial-pass', 'the pass the daemon runs by itself; on the terminal it exists to be measured'],
    ['pl check --fix, pl audit-archive --update, pl prune --before', 'the sharp forms: they delete or freeze something, and each of them asks a question a window cannot honestly ask. The menu runs the read-only forms'],
    ['pl completion --install, pl prompt', 'shell furniture: the menu shows the command rather than running it, because installing completion changes your shell and not this app'],
  ];
  return '<div class="card"><h3>' + esc(t('cli_only_title')) + '</h3><p class="hint">' + esc(t('cli_only_hint')) + '</p>' +
    '<div class="stack small">' + rows.map((r) => '<div><span class="mono">' + esc(r[0]) + '</span> — ' + esc(r[1]) + '</div>').join('') + '</div></div>';
}

/* ==========================================================================================
 * Round 299 — the menu.
 *
 * Every entry comes from the app server (`GET /api/menu`), which builds it from one table in the
 * Rust source (`app/src/menu.rs`). This file holds no second copy of that table: an entry that is
 * not in the server's answer is not in this window either, and the macOS menu bar — which reads the
 * same route — cannot offer anything different.
 *
 * What this page does with an entry is small and explicit:
 *   view     open one of VIEWS()
 *   page     run one of PAGE_ITEMS (the flow the window already had)
 *   info     show the command and why it stays in the terminal; run nothing
 *   run      ask first if the entry needs a value, then POST /api/menu/run
 *   confirm  ask the person with the command in front of them, then POST with confirm:true
 * ========================================================================================== */

let MENU = null;          // the server's answer, in the current language
let MENU_OPEN = null;     // the group whose dropdown is open
let MENU_ERROR = null;    // why there is no menu, when there is none
let PALETTE = null;       // { query, index } while the palette is open
let RUN_RESULT = null;    // the last entry that ran, and what the core answered
let menuProject = null;   // the project the current MENU was fetched for

/// The views this window can draw. The checker requires every kind=view entry to name one of these.
function VIEWS() {
  return {
    dashboard: viewDashboard, add: viewAdd, project: viewProject, integrity: viewIntegrity,
    storage: viewStorage, import: viewImport, diagnostics: viewDiagnostics, settings: viewSettings,
    about: viewAbout, status: viewStatus, help: viewHelp, 'cli-only': viewCliOnly, palette: viewPalette,
    notifications: viewNotifications,
  };
}

/// The flows the window already had, reachable from the menu by id. Each call is the same one the
/// button in the window makes — the menu adds an entry point, not a second implementation.
const PAGE_ITEMS = {
  'file.archive-new': () => chooseArchive(true),
  'file.archive-use': () => chooseArchive(false),
  'file.export': () => exportHistory(),
  'file.reveal': () => revealArchive(),
  'protect.start': () => startWatch((S.boot && S.boot.config && S.boot.config.intervalSeconds) || 5),
  'protect.stop': () => stopWatch(),
  'restore.drill': () => drillNow(),
  'restore.missing': () => repairFlow(S.project),
};

async function drillNow() {
  const name = S.toolProject || S.project;
  if (!name) { toast(t('menu_disabled')); return; }
  const r = await api('drill', { body: { name: name } });
  S.jobId = r.job;
  S.job = null;
  render();
}

async function revealArchive() {
  const root = S.boot && S.boot.archive ? (S.boot.archive.root || '') : '';
  if (!root) { toast(t('menu_disabled')); return; }
  await revealFolder(root);
}

function allItems() {
  if (!MENU || !MENU.groups) return [];
  const out = [];
  for (const g of MENU.groups) for (const i of (g.items || [])) out.push(Object.assign({ groupTitle: g.title }, i));
  return out;
}

function itemById(id) {
  return allItems().find((i) => i.id === id) || null;
}

/// Read the menu from the server. The project is passed so that entries which need one come back
/// disabled *with the reason* rather than failing after the click.
async function loadMenu() {
  try {
    const q = S.project ? '?project=' + encodeURIComponent(S.project) : '';
    MENU = await api('menu' + q);
    MENU_ERROR = null;
  } catch (e) {
    MENU = null;
    MENU_ERROR = e.message;
  }
}

function renderMenuBar() {
  const host = document.getElementById('menubar');
  if (!host) return;
  if (MENU_ERROR) {
    host.innerHTML = '<span class="mbrand">Project Life</span>' +
      '<span class="mwhy">' + esc(t('menu_stale')) + ': ' + esc(MENU_ERROR) + '</span>';
    return;
  }
  if (!MENU) { host.innerHTML = '<span class="mbrand">Project Life</span>'; return; }
  let h = '<span class="mbrand">Project Life</span>';
  h += (MENU.groups || []).map((g) =>
    '<span class="mgroup' + (MENU_OPEN === g.id ? ' open' : '') + '">' +
    '<button data-act="menu-open" data-group="' + esc(g.id) + '">' + esc(g.title) + '</button>' +
    ((MENU_OPEN === g.id) ? '<div class="mdrop">' + (g.items || []).map(menuItemRow).join('') + '</div>' : '') +
    '</span>').join('');
  h += '<span class="spacer"></span>';
  h += '<button class="mkey-btn" data-act="palette-open" title="' + esc(t('palette_hint')) + '">' +
    esc(t('palette_open')) + ' ⌘K</button>';
  host.innerHTML = h;
}

function menuItemRow(i) {
  const key = i.key ? '<span class="mkey">' + esc(String(i.key).replace('meta+', '⌘')) + '</span>' : '';
  const why = i.enabled ? '' : ' <span class="mwhy">' + esc(i.why) + '</span>';
  return '<div class="mrow">' +
    '<button class="mitem' + (i.enabled ? '' : ' off') + '" data-act="menu-item" data-id="' + esc(i.id) + '"' +
    (i.enabled ? '' : ' disabled') + ' title="' + esc(i.coreRun + (i.why ? ' — ' + i.why : '')) + '">' +
    '<span class="mlabel">' + esc(i.label) + why + '</span>' + key + '</button>' +
    '<span class="mnote">' + esc(i.note) + '</span></div>';
}

async function openView(view) {
  if (!VIEWS()[view]) { S.error = 'no such view: ' + view; render(); return; }
  S.view = view;
  S.error = null;
  if (view === 'add') { wizardStart(); return; }
  if (view === 'project' && !S.project) S.project = (S.boot.projects[0] || {}).name || null;
  if (view === 'project' && S.project && !S.projectDetail) { await openProject(S.project); return; }
  if (view === 'notifications') { await loadNotifications(); return; }
  render();
}

/// Run one menu entry. This is the only place that decides what "running" an entry means, so the
/// menu bar in the window, the ⌘K palette and the macOS menu bar all end up in this function.
async function runMenuItem(id) {
  const item = itemById(id);
  MENU_OPEN = null;
  PALETTE = null;
  if (!item) { S.error = t('menu_stale') + ': ' + id; render(); return; }
  if (!item.enabled) { toast(item.why || t('menu_disabled')); render(); return; }
  if (item.kind === 'view') { await openView(item.view); return; }
  if (item.kind === 'page') {
    const fn = PAGE_ITEMS[id];
    if (!fn) { S.error = 'the window has no flow for ' + id; render(); return; }
    await fn();
    return;
  }
  if (item.kind === 'info') {
    RUN_RESULT = { id: id, kind: 'info', ran: false, core: item.core, coreRun: item.core, note: item.note };
    render();
    return;
  }
  let input = '';
  if (item.kind === 'ask') {
    if (item.input === 'folder') input = (await pickFolder(t('ask_folder'))) || '';
    else if (item.input === 'path') input = (await askText(t('ask_path'), t('ask_path_prompt'))) || '';
    else if (item.input === 'moment') input = (await askText(t('ask_moment'), localFromMs(Date.now()))) || '';
    else if (item.input === 'label') input = (await askText(t('ask_label'), '')) || '';
    else input = (await askText(t('ask_text'), '')) || '';
    if (!input) return;
    if (item.input === 'moment') {
      const ms = momentFromInput(input);
      if (ms === null) { S.error = t('bad_moment') + ': ' + input; render(); return; }
      input = isoUtc(ms);
    }
  }
  let confirm = false;
  if (item.kind === 'confirm') {
    if (!(await askConfirm(t('menu_confirm_note'), item.coreRun + ' — ' + item.note))) return;
    confirm = true;
  }
  const body = { id: id, confirm: confirm };
  if (S.project) body.project = S.project;
  if (input) body.input = input;
  try {
    RUN_RESULT = await api('menu/run', { body: body });
  } catch (e) {
    RUN_RESULT = {
      id: id, kind: item.kind, ran: true, error: e.message,
      core: item.core, coreRun: item.coreRun,
    };
  }
  render();
}

function resultCard() {
  const r = RUN_RESULT;
  if (!r) return '';
  let h = '<div class="card result"><div class="row between"><h3>' + esc(t('menu_result')) + '</h3>' +
    '<button class="btn small" data-act="result-close">' + esc(t('menu_cancel')) + '</button></div>';
  h += '<p class="small muted">' + esc(r.note || '') + '</p>';
  h += '<p class="small mono">' + esc(t('menu_command_behind')) + ': ' + esc(r.coreRun || r.core || '') + '</p>';
  if (r.ran === false || r.kind === 'info') {
    return h + '<p>' + esc(t('menu_nothing_ran')) + '</p></div>';
  }
  if (r.error) return h + '<p class="err mono">' + esc(r.error) + '</p></div>';
  h += '<p class="small">' + esc(t('menu_exit')) + ' <strong>' + esc(String(r.exit)) + '</strong>' +
    (r.ms !== undefined ? ' · ' + esc(String(r.ms)) + ' ms' : '') +
    (r.writes ? ' · ' + esc(t('menu_confirm_note')) : '') + '</p>';
  h += '<div class="scroll"><pre class="log">' + esc((r.stdout || '') + (r.stderr || '')) + '</pre></div>';
  return h + '</div>';
}

/* ---------------------------------------------------------------- the ⌘K palette */

function paletteMatches() {
  const q = ((PALETTE && PALETTE.query) || '').toLowerCase().trim();
  const items = allItems();
  if (!q) return items;
  return items.filter((i) =>
    (i.label + ' ' + i.id + ' ' + i.core + ' ' + i.note + ' ' + (i.groupTitle || '')).toLowerCase().includes(q));
}

function paletteRows() {
  const items = paletteMatches();
  const idx = Math.max(0, Math.min((PALETTE && PALETTE.index) || 0, items.length - 1));
  return items.map((i, n) =>
    '<div class="prompt-row' + (n === idx ? ' active' : '') + '" data-act="palette-run" data-id="' + esc(i.id) + '">' +
    '<span class="pl-group">' + esc(i.groupTitle || '') + '</span>' +
    '<span class="pl-label">' + esc(i.label) + (i.enabled ? '' : ' — ' + esc(i.why)) + '</span>' +
    '<span class="pl-core mono">' + esc(i.coreRun) + '</span></div>').join('');
}

function paletteOverlay() {
  if (!PALETTE) return '';
  const rows = paletteRows();
  return '<div class="modal-back" id="palette-back"><div class="modal palette" role="dialog" aria-modal="true">' +
    '<h3>' + esc(t('palette_title')) + '</h3>' +
    '<input id="palette-input" type="text" autocomplete="off" placeholder="' + esc(t('palette_hint')) + '" value="' + esc(PALETTE.query) + '">' +
    '<div class="prompt-list">' + (rows || '<div class="prompt-row"><span class="pl-label">' + esc(t('palette_none')) + '</span></div>') + '</div>' +
    '</div></div>';
}

function paletteKey(ev) {
  const items = paletteMatches();
  const idx = Math.max(0, Math.min(PALETTE.index || 0, items.length - 1));
  if (ev.key === 'Escape') { PALETTE = null; render(); return true; }
  if (ev.key === 'ArrowDown') { PALETTE.index = Math.min(idx + 1, items.length - 1); render(); return true; }
  if (ev.key === 'ArrowUp') { PALETTE.index = Math.max(idx - 1, 0); render(); return true; }
  if (ev.key === 'Enter') {
    const chosen = items[idx];
    if (chosen) runMenuItem(chosen.id);
    return true;
  }
  return false;
}

/// Chords come from the server's own list: a chord exists because an entry declares it.
function chordItem(ev) {
  const key = 'meta+' + (ev.key.length === 1 ? ev.key.toUpperCase() : ev.key);
  return allItems().find((i) => i.key && i.key.toLowerCase() === key.toLowerCase()) || null;
}

/* Typing in the palette filters in place: rebuilding the whole window would take the caret out of
 * the field with every keystroke. */
document.addEventListener('input', (ev) => {
  const el = ev.target;
  if (!el || el.id !== 'palette-input') return;
  if (!PALETTE) return;
  PALETTE.query = el.value;
  PALETTE.index = 0;
  const list = document.querySelector('.prompt-list');
  if (list) {
    const rows = paletteRows();
    list.innerHTML = rows || '<div class="prompt-row"><span class="pl-label">' + esc(t('palette_none')) + '</span></div>';
  }
});

document.addEventListener('keydown', async (ev) => {
  const meta = ev.metaKey || ev.ctrlKey;
  const inField = ev.target && (ev.target.tagName === 'INPUT' || ev.target.tagName === 'SELECT' || ev.target.tagName === 'TEXTAREA');
  if (PALETTE && (!inField || (ev.target && ev.target.id === 'palette-input'))) {
    if (paletteKey(ev)) { ev.preventDefault(); return; }
    if (inField) return;
  }
  if (meta && (ev.key === 'k' || ev.key === 'K')) {
    ev.preventDefault();
    PALETTE = { query: '', index: 0 };
    render();
    const box = document.getElementById('palette-input');
    if (box) box.focus();
    return;
  }
  if (meta && !inField) {
    const item = chordItem(ev);
    if (item) { ev.preventDefault(); await runMenuItem(item.id); }
  }
});

/* ------------------------------------------------------------------ views for the menu */

// ------------------------------------------------------------------------------------------
// The notification ledger (round 300) and the repair it leads to
//
// Until this round a notification was a toast that vanished: if you were not at the machine, the
// program had told you nothing you could still find. The core now writes one structured line per
// message into <archive>/logs/notifications.jsonl, and this screen is a view over that file — no
// invented entries, no placeholder rows.

async function loadNotifications() {
  try {
    const kinds = S.notifFilter && S.notifFilter !== 'all' ? '&kind=' + encodeURIComponent(S.notifFilter) : '';
    S.notif = await api('notifications?limit=100' + kinds);
  } catch (e) {
    S.notif = { notifications: [], error: String(e.message || e) };
  }
  render();
}

function notifPill(kind) {
  const cls = kind === 'mass' ? 'warn' : (kind === 'error' || kind === 'space' ? 'err' : 'ok');
  return '<span class="pill ' + cls + '">' + esc(kind) + '</span>';
}

function viewNotifications() {
  const n = S.notif;
  let h = '<div class="main-header"><h2>' + esc(t('nav_notifications')) + '</h2>' +
    '<p>' + esc(t('notif_hint')) + '</p></div>';
  const filters = [['all', t('notif_all')], ['mass', t('notif_mass')], ['space', t('notif_space')],
                   ['error', t('notif_error')], ['lifecycle', t('notif_lifecycle')]];
  h += '<div class="row" style="gap:6px;flex-wrap:wrap;margin-bottom:10px">';
  for (const [k, label] of filters) {
    h += '<button class="btn small' + (S.notifFilter === k ? ' primary' : '') + '" data-act="notif-filter" data-kind="' + esc(k) + '">' + esc(label) + '</button>';
  }
  h += '<div class="spacer"></div><button class="btn small" data-act="notif-refresh">' + esc(t('notif_refresh')) + '</button></div>';
  if (!n) { h += '<p class="muted">' + esc(t('loading')) + '</p>'; return h; }
  if (n.error) { h += '<div class="banner err"><div><h3>' + esc(t('failed')) + '</h3><p>' + esc(n.error) + '</p></div></div>'; return h; }
  const rows = (n.notifications || []).slice().reverse();
  if (!rows.length) {
    h += '<div class="card"><p>' + esc(S.notifFilter && S.notifFilter !== 'all' ? t('notif_none_filter') : t('notif_empty')) + '</p>' +
      '<p class="small muted">' + esc(String(n.ledger || '')) + '</p></div>';
    return h;
  }
  h += '<div class="card"><div class="list">';
  for (const r of rows) {
    const kind = r.kind || 'notice';
    h += '<div class="row" style="align-items:flex-start;gap:10px;padding:8px 0;border-bottom:1px solid #DDE2E8">' +
      '<div style="min-width:150px" class="small mono muted">' + esc(r.local || r.atIso || '') + '</div>' +
      '<div style="min-width:110px">' + notifPill(kind) + '</div>' +
      '<div style="flex:1"><strong>' + esc(r.title || '') + '</strong>' +
      (r.project ? ' <span class="small muted">[' + esc(r.project) + ']</span>' : '') +
      '<div class="small" style="white-space:pre-wrap">' + esc(r.body || '') + '</div></div>';
    if (r.project) {
      h += '<div><button class="btn small" data-act="repair-start" data-name="' + esc(r.project) + '">' + esc(t('repair_btn')) + '</button></div>';
    }
    h += '</div>';
  }
  h += '</div>';
  h += '<p class="small muted">' + esc(t('notif_ledger')) + ': ' + esc(String(n.ledger || '')) + '</p></div>';
  return h;
}

/// The repair the mass-change notification points at: put back what is gone, touch nothing else.
///
/// The moment defaults to the last good state (the moment before the last mass event) and is shown
/// before anything is written: the window asks the core for a plan with `--missing --preview`, and
/// that plan is what the confirmation carries.
async function repairFlow(name, moment) {
  if (!name) { toast(t('menu_disabled')); return; }
  let at = moment || null;
  try {
    const lg = await api('project/last-good?name=' + encodeURIComponent(name));
    if (lg && lg.found) at = at || lg.atIso || lg.atLocal || null;
  } catch (e) { /* no last-good: ask below */ }
  if (!at) at = await askText(t('repair_ask_moment'), localFromMs(Date.now() - 600000));
  if (!at) return;
  let plan = null;
  try {
    plan = await api('project/preview', { body: { name: name, at: at, intoProject: true, missing: true } });
  } catch (e) {
    S.error = String(e.message || e);
    render();
    return;
  }
  const lines = t('repair_title') + '\n' + t('repair_hint') + '\n\n' +
    'moment: ' + (plan.momentLocal || at) + '\n' +
    'to create: ' + plan.create + '\n' +
    'to overwrite: ' + plan.overwrite + '\n' +
    'already on disk, left alone: ' + plan.present + '\n' +
    'to delete: ' + plan.deleteExtra + '\n' +
    (plan.missingBlobs && plan.missingBlobs.length ? 'missing from the archive: ' + plan.missingBlobs.length + '\n' : '');
  const ok = await askConfirm(t('repair_confirm'), lines);
  if (!ok) return;
  const r = await api('project/repair', { body: { name: name, at: at, intoProject: true } });
  S.jobId = r.job;
  S.job = null;
  render();
}

function viewAbout() {
  const b = (S.boot && S.boot.build) || null;
  const archive = (S.boot && S.boot.archive) || {};
  const app = (S.boot && S.boot.app) || {};
  const p = (S.boot && S.boot.program) || {};
  const ru = (LANG === 'ru');
  // What the program is for, in the language the window is in. Both sentences come from the core
  // (`src/brand.rs`) — the page does not hold a copy of either.
  const what = ru ? (p.whatItIsRu || app.whatItIsRu) : (p.whatItIs || app.whatItIs);
  const author = app.author || p.author || '';
  const email = app.authorEmail || p.authorEmail || '';
  const unavailable = p.unavailable ? (p.why || t('about_unavailable')) : '';
  let h = '<div class="main-header"><h2>' + esc(t('view_about')) + '</h2><p>' + esc(t('tagline')) + '</p></div>';
  h += '<div class="card"><h3>' + esc('Project Life') + '</h3>' +
    '<p class="small mono">' + esc(b ? b.short : '') + '</p>';
  if (what) h += '<p>' + esc(what) + '</p>';
  if (unavailable) h += '<p class="small muted">' + esc(t('about_author_missing') + ': ' + unavailable) + '</p>';
  if (author) {
    h += '<p class="small">' + esc(t('about_author')) + ': <span class="mono">' + esc(author) + '</span>';
    if (email) h += ' &lt;<span class="mono">' + esc(email) + '</span>&gt;';
    h += '</p>';
  }
  if (p.copyright || p.licence) {
    h += '<p class="small muted">' + esc(p.licence ? (p.licence + ' licence') : '') +
      (p.copyright ? ' · ' + esc(p.copyright) : '') + '</p>';
  }
  h += '<p class="small muted">' + esc(b && b.check ? b.check : '') + '</p>';
  h += '<p class="small">' + esc(t('summary_archive')) + ': <span class="mono">' + esc(archive.root || '—') + '</span></p>';
  h += '<p class="small">' + esc(t('version')) + ': <span class="mono">' + esc(app.version || '') + '</span>' +
    ' · core: <span class="mono">' + esc(app.coreVersion || '') + '</span></p>';
  h += '<p class="small muted">' + esc(t('menu_open_hint')) + '</p></div>';
  h += '<div class="card"><h3>' + esc(t('menu_command_behind')) + '</h3>' +
    '<div class="stack small"><div><span class="mono">app.about</span> — ' + esc('pl version') + '</div></div>' +
    '<button class="btn" data-act="menu-item" data-id="tools.version">' + esc(t('menu_run')) + ': pl version</button></div>';
  return h;
}

function viewStatus() {
  const st = protectionState();
  const b = S.boot || {};
  const p = (b.watch && b.watch.protection) || null;
  let h = '<div class="main-header"><h2>' + esc(t('view_status')) + '</h2><p>' + esc(b.app && b.app.nowLocal ? b.app.nowLocal : '') + '</p></div>';
  h += '<div class="banner ' + st.level + '"><div><h3>' + esc(st.label) + '</h3><p>' + esc(st.detail || '') + '</p></div></div>';
  h += '<div class="card"><h3>' + esc('pl heartbeat-check --json') + '</h3><pre class="log">' + esc(JSON.stringify(p || b.heartbeat || {}, null, 2)) + '</pre>' +
    '<button class="btn" data-act="menu-item" data-id="protect.heartbeat">' + esc(t('menu_run')) + '</button></div>';
  h += triggerLine(b) ? '<div class="card">' + triggerLine(b) + '</div>' : '';
  return h;
}

function viewHelp() {
  const steps = [
    [t('choose_archive'), 'pl init-archive <path>'],
    [t('add_folder'), 'pl add <path> --preset auto --yes'],
    [t('summary_coverage'), 'pl detect <path>'],
    [t('watch_running'), 'pl daemon start'],
    [t('moments'), 'pl log <project>'],
    [t('restore_selected'), 'pl restore <project> --at <moment> --to <dir>'],
    [t('export'), 'pl export <project> --out <dir>'],
    [t('importing'), 'pl import <export-dir> --new <name>'],
  ];
  let h = '<div class="main-header"><h2>' + esc(t('view_help')) + '</h2><p>' + esc(t('help_intro')) + '</p></div>';
  h += '<div class="card"><h3>' + esc(t('palette_title')) + '</h3><p class="hint">' + esc(t('palette_hint')) + '</p>' +
    '<p class="hint">' + esc(t('menu_open_hint')) + '</p>' +
    '<button class="btn" data-act="palette-open">' + esc(t('palette_open')) + '</button></div>';
  h += '<div class="card"><h3>' + esc(t('nav_dashboard')) + '</h3><div class="stack small">' +
    steps.map(([what, cmd]) => '<div>' + esc(what) + ' — <span class="mono">' + esc(cmd) + '</span></div>').join('') +
    '</div></div>';
  h += cliOnlyCard();
  return h;
}

function viewCliOnly() {
  let h = '<div class="main-header"><h2>' + esc(t('cli_only_title')) + '</h2><p>' + esc(t('cli_only_hint')) + '</p></div>';
  return h + cliOnlyCard();
}

function viewPalette() {
  let h = '<div class="main-header"><h2>' + esc(t('palette_title')) + '</h2><p>' + esc(t('palette_hint')) + '</p></div>';
  h += '<div class="card"><button class="btn primary" data-act="palette-open">' + esc(t('palette_open')) + '</button></div>';
  h += '<div class="card"><h3>' + esc(t('menu_result')) + '</h3><div class="scroll"><table><thead><tr>' +
    '<th>' + esc(t('project')) + '</th><th>' + esc(t('path')) + '</th><th>' + esc(t('menu_command_behind')) + '</th></tr></thead><tbody>' +
    allItems().map((i) => '<tr' + (i.enabled ? '' : ' class="muted"') + '><td>' + esc(i.groupTitle || '') + '</td>' +
      '<td><button class="link" data-act="menu-item" data-id="' + esc(i.id) + '"' + (i.enabled ? '' : ' disabled') + '>' +
      esc(i.label) + '</button>' + (i.enabled ? '' : ' <span class="mwhy">' + esc(i.why) + '</span>') + '</td>' +
      '<td class="mono small">' + esc(i.coreRun) + '</td></tr>').join('') +
    '</tbody></table></div></div>';
  return h;
}

/* ------------------------------------------------------------------ events */

document.addEventListener('click', async (ev) => {
  const el = ev.target.closest('[data-act]');
  if (!el) return;
  const act = el.getAttribute('data-act');
  // Checkboxes and radios report themselves through `change`, not `click`: re-rendering the page
  // from the click handler would replace the input before the browser commits its new state.
  if (act === 'file' || act === 'ext' || act === 'preset' || act === 'lang' || act === 'config-bool' || act === 'filter-input') return;
  const name = el.getAttribute('data-name');
  try {
    switch (act) {
      case 'nav':
        await openView(el.getAttribute('data-view'));
        return;
      case 'menu-open': {
        const g = el.getAttribute('data-group');
        MENU_OPEN = MENU_OPEN === g ? null : g;
        if (MENU_OPEN) await loadMenu();
        render();
        return;
      }
      case 'menu-item':
        await runMenuItem(el.getAttribute('data-id'));
        return;
      case 'palette-open':
        PALETTE = { query: '', index: 0 };
        render();
        {
          const box = document.getElementById('palette-input');
          if (box) box.focus();
        }
        return;
      case 'palette-run':
        await runMenuItem(el.getAttribute('data-id'));
        return;
      case 'result-close':
        RUN_RESULT = null;
        break;
      case 'open-project':
        await openProject(name);
        return;
      case 'dismiss-error': S.error = null; break;
      case 'notif-refresh':
        await loadNotifications();
        return;
      case 'notif-filter':
        S.notifFilter = el.getAttribute('data-kind') || 'all';
        await loadNotifications();
        return;
      case 'repair-start':
        await repairFlow(el.getAttribute('data-name'));
        return;
      case 'dismiss-job': S.job = null; S.jobId = null; break;
      case 'watch-start': await startWatch(S.boot.config.intervalSeconds); return;
      case 'watch-stop': await stopWatch(); return;
      case 'save-interval': {
        const v = parseInt(document.getElementById('set-interval').value, 10) || 5;
        await setConfig('intervalSeconds', v);
        return;
      }
      case 'run-pass': await runPass(); return;
      case 'load-doctor': S.doctor = await api('doctor'); break;
      case 'load-log': S.log = await api('log'); break;
      case 'toggle-raw': S.raw = !S.raw; break;
      case 'archive-create': await chooseArchive(true); return;
      case 'archive-use': await chooseArchive(false); return;
      case 'wizard-pick': await wizardPickFolder(); return;
      case 'wizard-back': S.wizard.step = Math.max(1, S.wizard.step - 1); break;
      case 'wizard-next': S.wizard.step = 4; break;
      case 'wizard-preview': {
        S.wizard.name = (document.getElementById('wiz-name') || {}).value || S.wizard.name;
        await wizardPreview();
        return;
      }
      case 'wizard-start': await wizardStartProtect(); return;
      case 'pause': await pauseProject(name, el.getAttribute('data-pause') === 'true'); return;
      case 'moment': S.moment = { atIso: el.getAttribute('data-at') }; await loadTree(el.getAttribute('data-at')); return;
      case 'load-picked': {
        const v = (document.getElementById('pick-moment') || {}).value;
        const ms = momentFromInput(v);
        if (ms === null) {
          S.error = t('bad_moment') + (v ? ': ' + v : '');
          break;
        }
        const iso = isoUtc(ms);
        S.moment = { atIso: iso };
        await loadTree(iso);
        return;
      }
      case 'select-all': S.selected = new Set((S.tree || []).map((f) => f.path)); break;
      case 'restore-dest': {
        const p = await pickFolder(t('restore_to'));
        if (p) S.restoreTo = p;
        break;
      }
      case 'restore': await restoreSelected(); return;
      case 'export': await exportHistory(); return;
      case 'import': await importExport(); return;
      case 'reveal': await revealFolder(el.getAttribute('data-path')); return;
      // ---- round 297
      case 'tab': S.tab = el.getAttribute('data-tab'); S.diff = null; render(); return;
      case 'open-file': await loadVersions(el.getAttribute('data-path')); return;
      case 'show-version': await loadContentAt(el.getAttribute('data-at')); return;
      case 'cmp-version': await compareVersion(el.getAttribute('data-at'), el.getAttribute('data-prev')); return;
      case 'cmp-current-version': {
        // No `to`: the core compares the moment with the folder as it is now (diff --current).
        await api('project/diff?name=' + encodeURIComponent(S.project) +
          '&from=' + encodeURIComponent(el.getAttribute('data-at')))
          .then((d) => { S.diff = d; })
          .catch((e) => { S.error = e.message; });
        render();
        return;
      }
      case 'run-compare': await runCompare(); return;
      case 'check': {
        const deep = el.getAttribute('data-deep') === '1';
        S.check = await api('check?deep=' + (deep ? '1' : '0'));
        break;
      }
      case 'audit': S.audit = await api('audit'); break;
      case 'quarantine': S.quarantine = await api('quarantine'); break;
      case 'gc': S.gc = await api('gc'); break;
      case 'load-size': S.size = await api('size'); break;
      case 'load-recent': S.recent = await api('recent'); break;
      case 'load-suggest': S.suggest = await api('suggest'); break;
      case 'load-health': S.health = await api('doctor'); break;
      case 'load-config': S.configAll = await api('config/all'); break;
      case 'last-good': S.lastGood = await api('project/last-good?name=' + encodeURIComponent(S.project)); break;
      case 'note-save': {
        const text = (document.getElementById('pl-note') || {}).value || '';
        await api('project/note', { body: { name: S.project, text: text } });
        S.noteDraft = text;
        toast(t('done'));
        break;
      }
      case 'mark-save': {
        const label = (document.getElementById('pl-mark') || {}).value || '';
        const r = await api('project/mark', { body: { name: S.project, label: label } });
        toast(r && r.result ? (r.result.label || t('done')) : t('done'));
        S.projectDetail = null;
        break;
      }
      case 'apply-filters': S.filtersResult = await api('project/apply-filters', { body: { name: S.project } }); break;
      case 'relink': {
        const p = await pickFolder(t('relink_run'));
        if (!p) return;
        if (!(await askConfirm(t('relink_run'), t('relink_confirm')))) return;
        await api('project/relink', { body: { name: S.project, path: p, confirm: true } });
        S.projectDetail = null;
        await refreshBoot();
        break;
      }
      case 'remove-project': {
        if (!(await askConfirm(t('remove_run'), t('remove_confirm')))) return;
        await api('project/remove', { body: { name: S.project, confirm: true } });
        S.project = null; S.projectDetail = null; S.tree = null;
        await refreshBoot();
        S.view = 'dashboard';
        return;
      }
      case 'drill':
        await drillNow();
        return;
      case 'recover': {
        const name = S.toolProject || S.project;
        if (!(await askConfirm(t('recover_run'), t('recover_confirm')))) return;
        S.recovered = await api('recover', { body: { name: name, confirm: true } });
        break;
      }
      case 'retention-load':
      case 'retention-preview': {
        const name = S.toolProject || S.project;
        if (act === 'retention-load') {
          S.retention = await api('retention?name=' + encodeURIComponent(name));
        } else {
          const policy = (document.getElementById('ret-policy') || {}).value || (S.retention && S.retention.policy) || '';
          S.prune = await api('retention/apply', { body: { name: name, policy: policy, dryRun: true } });
        }
        break;
      }
      case 'retention-save': {
        const name = S.toolProject || S.project;
        const policy = (document.getElementById('ret-policy') || {}).value || '';
        await api('retention/set', { body: { name: name, policy: policy } });
        S.retention = await api('retention?name=' + encodeURIComponent(name));
        toast(t('retention_saved_note'));
        break;
      }
      case 'retention-apply': {
        const name = S.toolProject || S.project;
        const policy = (document.getElementById('ret-policy') || {}).value || (S.retention && S.retention.policy) || '';
        const before = await api('retention/apply', { body: { name: name, policy: policy, dryRun: true } });
        if (!(await askConfirm(t('retention_apply'), t('retention_apply_confirm') + ' ' + String(before.versionsKept) + ' / ' + String(before.versionsBefore) + ' · ' + t('dropped') + ' ' + String(before.dropped)))) return;
        S.prune = await api('retention/apply', { body: { name: name, policy: policy, dryRun: false, confirm: true } });
        S.retention = await api('retention?name=' + encodeURIComponent(name));
        S.projectDetail = null;
        break;
      }
      default: break;
    }
    render();
  } catch (e) {
    S.error = e.message;
    render();
  }
});

document.addEventListener('change', async (ev) => {
  const el = ev.target.closest('[data-act]');
  if (!el) return;
  const act = el.getAttribute('data-act');
  try {
    if (act === 'ext') {
      const ext = el.getAttribute('data-ext');
      const on = el.getAttribute('data-on') === '1';
      const pid = effectivePresetId();
      const preset = presetOf(pid);
      const inc = preset ? (preset.includeAfterPolicy || []) : [];
      const inPreset = inc.includes('*' + ext);
      const glob = '*' + ext;
      const nowChecked = el.checked;
      if (pid === 'custom') {
        if (nowChecked) S.wizard.editRemove = S.wizard.editRemove.filter((x) => x !== glob);
        else if (!S.wizard.editRemove.includes(glob)) S.wizard.editRemove.push(glob);
      } else if (nowChecked && !inPreset) {
        if (!S.wizard.editAdd.includes(glob)) S.wizard.editAdd.push(glob);
        S.wizard.editRemove = S.wizard.editRemove.filter((x) => x !== glob);
      } else if (!nowChecked && inPreset) {
        if (!S.wizard.editRemove.includes(glob)) S.wizard.editRemove.push(glob);
        S.wizard.editAdd = S.wizard.editAdd.filter((x) => x !== glob);
      } else if (nowChecked && inPreset) {
        S.wizard.editRemove = S.wizard.editRemove.filter((x) => x !== glob);
      } else {
        S.wizard.editAdd = S.wizard.editAdd.filter((x) => x !== glob);
      }
      el.setAttribute('data-on', on ? '0' : '1');
      return;
    }
    if (act === 'preset') {
      S.wizard.preset = el.value;
      S.wizard.editAdd = [];
      S.wizard.editRemove = [];
      render();
      return;
    }
    if (act === 'file') {
      const p = el.getAttribute('data-path');
      S.selected = S.selected || new Set();
      if (el.checked) S.selected.add(p); else S.selected.delete(p);
      const tr = el.closest('tr');
      if (tr) tr.classList.toggle('selected', el.checked);
      updateRestoreButtons();
      return;
    }
    if (act === 'lang') {
      LANG = el.value;
      localStorage.setItem('pl.lang', LANG);
      api('lang', { body: { lang: LANG } }).catch(() => {});
      render();
      return;
    }
    if (act === 'config-bool') {
      await setConfig(el.getAttribute('data-key'), el.checked);
      return;
    }
    if (act === 'pick-project') {
      S.toolProject = el.value;
      render();
      return;
    }
    if (act === 'filter-input') {
      S.filter = el.value;
      render();
      const again = document.getElementById('flt');
      if (again) { again.focus(); again.setSelectionRange(again.value.length, again.value.length); }
      return;
    }
    render();
  } catch (e) { S.error = e.message; render(); }
});

/* ------------------------------------------------------------------ boot */

async function boot() {
  LANG = chooseLang();
  try {
    await refreshBoot();
  } catch (e) {
    document.getElementById('main').innerHTML = '<div class="banner err"><div><h3>Connection</h3><p>' + esc(e.message) + '</p></div></div>';
    return;
  }
  if (S.boot.projects && S.boot.projects.length === 0) S.view = 'dashboard';
  await loadMenu();
  menuProject = S.project;
  render();
  setInterval(async () => {
    try {
      const wasRunning = S.boot && S.boot.watch && S.boot.watch.runningByApp;
      await refreshBoot();
      if (S.view === 'project' && S.project && !S.projectDetail) { /* keep */ }
      if (menuProject !== S.project) { await loadMenu(); menuProject = S.project; }
      render();
      if (!wasRunning && S.boot.watch.runningByApp === false) { /* nothing */ }
    } catch (e) { /* the server is gone; the window will notice on the next action */ }
  }, 3000);
}

boot();

// The macOS menu bar and the shield's menu call into the page by name: choosing an entry there must
// end in the same function the window's own menu bar and ⌘K use, or the three fronts would drift.
window.plMenu = function (id) { return runMenuItem(id); };
