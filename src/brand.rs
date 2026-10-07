//! Who made this program, what it is called, and what it is for.
//!
//! One source of truth, on purpose. The author's name, address and the sentence about what the
//! program is for have to appear in about a dozen places written in six different languages: Rust,
//! C, Objective-C, XML (`Info.plist`), a desktop entry, a Debian control file and Markdown. A string
//! typed by hand in twelve places is a string that drifts, and an attribution that has drifted is a
//! lie nobody checks.
//!
//! So the values live here. The core prints them (`pl version`, `pl version --json`), the three
//! desktop shells ask the core for them over the same route the page uses, and
//! `tools/brand_check.py` reads *this file* and then checks the shipped bytes of every other place
//! against it — including the compiled binaries. That checker has a control which deletes one
//! occurrence and requires the check to go red, because a check that cannot fail proves nothing.
//!
//! Spelling: the name and the address are exactly as the owner wrote them on 2026-10-07
//! (`автор проекта Oxunjon Ubaydllayev почта oxunjonub@gmail.com`). A name is not a detail I get to
//! correct; where an earlier file used a different spelling, `docs/BRAND.md` records it and the
//! owner decides.

/// The product name, as it appears in every window, package and menu.
pub const PRODUCT: &str = "Project Life";

/// The author. Verbatim from the owner's message of 2026-10-07 — one letter differs from the
/// spelling in `LICENSE` before this round; see `docs/BRAND.md`.
pub const AUTHOR: &str = "Oxunjon Ubaydllayev";

/// The author's address, verbatim from the same message.
pub const AUTHOR_EMAIL: &str = "oxunjonub@gmail.com";

/// One line: who to credit, in the form a licence header and a package maintainer field want.
pub fn by() -> String {
    format!("{AUTHOR} <{AUTHOR_EMAIL}>")
}

pub const COPYRIGHT: &str = "Copyright (c) 2026 Oxunjon Ubaydllayev and Aiodam";
pub const LICENCE: &str = "MIT";

pub const TAGLINE: &str = "Local. Private. Yours.";
pub const TAGLINE_RU: &str = "Локально. Приватно. Ваше.";

/// What the program is for, in one sentence: the owner's own words
/// (`это программа сделано когда агент что то удалил или сломал он все хранит`).
///
/// One line on purpose: `tools/brand.py` reads these constants for the build scripts and the three
/// C/Objective-C shells, and a multi-line Rust string would make that reader a parser. The rule is
/// therefore: **every constant in this file is one line**.
pub const WHAT_IT_IS: &str = "It is made for the moment an agent deletes or breaks something: it keeps everything — every version of every file you protect stays on your own disk and can be restored at any moment.";

pub const WHAT_IT_IS_RU: &str = "Она сделана на случай, когда агент что-то удалил или сломал: она хранит всё — каждая версия каждого защищённого файла лежит на вашем диске и может быть восстановлена в любой момент.";

/// The three questions the program answers, in one line each. Used by the About screen and by
/// `pl version`.
pub const ANSWERS: &str = "What was here? Give it back. Is it still protecting me?";

#[cfg(test)]
mod tests {
    use super::*;

    /// A name and an address that somebody typed are values, not prose: this asserts the two things
    /// a human notices if they break — the spelling and the shape of the address.
    #[test]
    fn the_author_and_the_address_are_the_owners_own_words() {
        assert_eq!(AUTHOR, "Oxunjon Ubaydllayev");
        assert_eq!(AUTHOR_EMAIL, "oxunjonub@gmail.com");
        assert_eq!(by(), "Oxunjon Ubaydllayev <oxunjonub@gmail.com>");
        // Exactly one '@', something on both sides, and no whitespace: the shape of an address.
        assert_eq!(AUTHOR_EMAIL.matches('@').count(), 1, "{AUTHOR_EMAIL}");
        let (local, domain) = AUTHOR_EMAIL.split_once('@').unwrap();
        assert!(!local.is_empty() && !domain.is_empty(), "{AUTHOR_EMAIL}");
        assert!(domain.contains('.'), "{AUTHOR_EMAIL}");
        assert!(!AUTHOR_EMAIL.contains(char::is_whitespace), "{AUTHOR_EMAIL}");
    }

    /// The sentence about what the program is for must say the agent, and must say that nothing is
    /// lost — those are the two halves of the owner's own description.
    #[test]
    fn the_sentence_about_what_it_is_for_says_what_the_owner_said() {
        for s in [WHAT_IT_IS, WHAT_IT_IS_RU] {
            assert!(s.to_lowercase().contains("агент") || s.to_lowercase().contains("agent"), "{s}");
            assert!(
                s.contains("хранит всё") || s.contains("keeps everything"),
                "the sentence must say that everything is kept: {s}"
            );        }
    }
}
