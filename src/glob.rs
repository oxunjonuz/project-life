//! Tiny glob matcher: `*` within one path segment, `**` across segments,
//! `?` a single character. Enough for `.projectlifeignore`, `include`/`exclude` and folder lists.
//!
//! It is also on the hot path: every file in every cycle is tested against the secret, temporary and
//! profile patterns. ASCII text (which is what patterns and, nearly always, names are) therefore
//! takes a byte-wise path that allocates nothing; the character-wise version remains for anything
//! else, so `?` still means one character and not one byte.

pub fn glob_match(pat: &str, s: &str) -> bool {
    if pat.is_ascii() && s.is_ascii() {
        return match_bytes(pat.as_bytes(), s.as_bytes());
    }
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    match_at(&p, 0, &t, 0)
}

fn match_bytes(p: &[u8], t: &[u8]) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    while pi < p.len() {
        match p[pi] {
            b'*' => {
                let double = pi + 1 < p.len() && p[pi + 1] == b'*';
                let mut next = pi + if double { 2 } else { 1 };
                if double && next < p.len() && p[next] == b'/' {
                    if match_bytes(&p[next + 1..], &t[ti..]) {
                        return true;
                    }
                }
                while next < p.len() && p[next] == b'*' {
                    next += 1;
                }
                let mut k = ti;
                loop {
                    if match_bytes(&p[next..], &t[k..]) {
                        return true;
                    }
                    if k >= t.len() {
                        return false;
                    }
                    if !double && t[k] == b'/' {
                        return false;
                    }
                    k += 1;
                }
            }
            b'?' => {
                if ti >= t.len() || t[ti] == b'/' {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= t.len() || t[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == t.len()
}

fn match_at(p: &[char], mut pi: usize, t: &[char], mut ti: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            '*' => {
                let double = pi + 1 < p.len() && p[pi + 1] == '*';
                let mut next = pi + if double { 2 } else { 1 };
                // "**/" and "/**" make the segment optional
                if double && next < p.len() && p[next] == '/' {
                    // **/foo also matches foo
                    if match_at(p, next + 1, t, ti) {
                        return true;
                    }
                }
                while next < p.len() && p[next] == '*' {
                    next += 1;
                }
                let mut k = ti;
                loop {
                    if match_at(p, next, t, k) {
                        return true;
                    }
                    if k >= t.len() {
                        return false;
                    }
                    if !double && t[k] == '/' {
                        return false;
                    }
                    k += 1;
                }
            }
            '?' => {
                if ti >= t.len() || t[ti] == '/' {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= t.len() || t[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == t.len()
}

/// "Anywhere in the path" match, for patterns such as `*.pem` or `node_modules`.
pub fn matches_anywhere(pat: &str, path: &str) -> bool {
    if glob_match(pat, path) {
        return true;
    }
    if !pat.contains('/') {
        // a pattern without '/' is compared with every segment
        let mut rest = path;
        loop {
            let seg = match rest.find('/') {
                Some(i) => &rest[..i],
                None => rest,
            };
            if glob_match(pat, seg) {
                return true;
            }
            match rest.find('/') {
                Some(i) => rest = &rest[i + 1..],
                None => return false,
            }
        }
    }
    // a pattern with '/' is compared with every path suffix
    let mut start = 0usize;
    loop {
        if glob_match(pat, &path[start..]) {
            return true;
        }
        match path[start..].find('/') {
            Some(i) => start += i + 1,
            None => return false,
        }
    }
}

/// gitignore-style rules (`#`, blank lines, `!` negation, a trailing slash).
pub struct IgnoreRules {
    rules: Vec<(bool, String)>, // (negate, pattern)
}

impl IgnoreRules {
    pub fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let (neg, pat) = if let Some(rest) = l.strip_prefix('!') {
                (true, rest.trim().to_string())
            } else {
                (false, l.to_string())
            };
            let pat = if let Some(stripped) = pat.strip_suffix('/') {
                // "the folder and everything inside it"
                format!("{stripped}/**")
            } else {
                pat
            };
            rules.push((neg, pat));
        }
        IgnoreRules { rules }
    }

    pub fn empty() -> Self {
        IgnoreRules { rules: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Returns Some(pattern) when the path is ignored.
    pub fn ignored(&self, path: &str) -> Option<String> {
        let mut hit: Option<String> = None;
        for (neg, pat) in &self.rules {
            if matches_anywhere(pat, path) {
                hit = if *neg { None } else { Some(pat.clone()) };
            }
        }
        hit
    }
}
