//! Light syntax colours: a small per-line lexer that knows comments, strings, numbers,
//! keywords, types and calls for a handful of languages, picked by file extension.
//!
//! This is a stopgap until parser-based highlighting arrives as an extension. It never
//! fails: unknown files get no colours.

/// What a char is, for colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    /// Plain text.
    Plain,
    /// A comment.
    Comment,
    /// A string or char literal.
    Str,
    /// A number.
    Num,
    /// A keyword.
    Keyword,
    /// A type name (capitalised identifier).
    Type,
    /// A name followed by `(` or `!(`: a call or definition.
    Call,
    /// Punctuation and operators.
    Punct,
}

/// A language's lexical rules.
#[derive(Debug)]
pub struct Lang {
    line_comment: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    keywords: &'static [&'static str],
    single_quote_strings: bool,
}

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const C_KW: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "goto",
    "if",
    "include",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "nullptr",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "true",
    "typedef",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];
const JS_KW: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];
const GO_KW: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "false",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];
const SH_KW: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "return", "then", "while",
];
const TOML_KW: &[&str] = &["true", "false"];

/// Rules for a file name, if Spur knows its language.
pub fn lang_for(path: &std::path::Path) -> Option<&'static Lang> {
    static RUST: Lang = Lang {
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        keywords: RUST_KW,
        single_quote_strings: false,
    };
    static C: Lang = Lang {
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        keywords: C_KW,
        single_quote_strings: true,
    };
    static JS: Lang = Lang {
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        keywords: JS_KW,
        single_quote_strings: true,
    };
    static GO: Lang = Lang {
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        keywords: GO_KW,
        single_quote_strings: true,
    };
    static PY: Lang = Lang {
        line_comment: &["#"],
        block_comment: None,
        keywords: PY_KW,
        single_quote_strings: true,
    };
    static SH: Lang = Lang {
        line_comment: &["#"],
        block_comment: None,
        keywords: SH_KW,
        single_quote_strings: true,
    };
    static TOML: Lang = Lang {
        line_comment: &["#"],
        block_comment: None,
        keywords: TOML_KW,
        single_quote_strings: true,
    };
    let ext = path.extension()?.to_str()?;
    Some(match ext {
        "rs" => &RUST,
        "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "cs" | "swift" | "kt" => &C,
        "js" | "jsx" | "ts" | "tsx" | "mjs" => &JS,
        "go" => &GO,
        "py" => &PY,
        "sh" | "bash" | "zsh" => &SH,
        "toml" | "yaml" | "yml" => &TOML,
        _ => return None,
    })
}

/// Colours one line, char by char. `in_block` carries an open block comment across
/// lines; it is updated for the next line.
#[allow(clippy::too_many_lines)]
pub fn line(lang: &Lang, text: &str, in_block: &mut bool) -> Vec<Tok> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![Tok::Plain; chars.len()];
    let starts = |i: usize, pat: &str| {
        pat.chars()
            .enumerate()
            .all(|(k, p)| chars.get(i + k) == Some(&p))
    };
    let mut i = 0;
    while i < chars.len() {
        if *in_block {
            let (_, close) = lang.block_comment.unwrap_or(("", ""));
            let start = i;
            while i < chars.len() && !(close.is_empty() || starts(i, close)) {
                i += 1;
            }
            if i < chars.len() {
                i += close.chars().count();
                *in_block = false;
            }
            out[start..i.min(chars.len())].fill(Tok::Comment);
            continue;
        }
        let c = chars[i];
        if lang.line_comment.iter().any(|p| starts(i, p)) {
            out[i..].fill(Tok::Comment);
            break;
        }
        if let Some((open, _)) = lang.block_comment
            && starts(i, open)
        {
            *in_block = true;
            out[i] = Tok::Comment;
            i += open.chars().count();
            out[i - open.chars().count()..i].fill(Tok::Comment);
            continue;
        }
        if c == '"' || (c == '\'' && lang.single_quote_strings) || c == '`' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(chars.len());
            out[start..i].fill(Tok::Str);
            continue;
        }
        // Rust char literals: 'a', '\n'. A lone quote is a lifetime; leave it plain.
        if c == '\'' {
            let end = if chars.get(i + 1) == Some(&'\\') {
                chars[i + 2..]
                    .iter()
                    .position(|&x| x == '\'')
                    .map(|p| i + 2 + p)
            } else if chars.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(e) = end {
                out[i..=e].fill(Tok::Str);
                i = e + 1;
                continue;
            }
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                // `0..n` is a range, not a float.
                if chars[i] == '.' && chars.get(i + 1) == Some(&'.') {
                    break;
                }
                i += 1;
            }
            out[start..i].fill(Tok::Num);
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let next = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
            let tok = if lang.keywords.contains(&word.as_str()) {
                Tok::Keyword
            } else if next == Some('(') || (next == Some('!') && chars.get(i + 1) == Some(&'(')) {
                Tok::Call
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                Tok::Type
            } else {
                Tok::Plain
            };
            out[start..i].fill(tok);
            continue;
        }
        if c.is_ascii_punctuation() {
            out[i] = Tok::Punct;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(path: &str, text: &str) -> String {
        let lang = lang_for(std::path::Path::new(path)).unwrap();
        let mut b = false;
        line(lang, text, &mut b)
            .iter()
            .map(|t| match t {
                Tok::Plain => '.',
                Tok::Comment => 'c',
                Tok::Str => 's',
                Tok::Num => 'n',
                Tok::Keyword => 'k',
                Tok::Type => 't',
                Tok::Call => 'f',
                Tok::Punct => 'p',
            })
            .collect()
    }

    #[test]
    fn rust_line() {
        assert_eq!(
            kinds("a.rs", r#"fn go(x: u8) -> Vec<u8> { "a\"b" } // hi"#),
            "kk.ffp.p...p.pp.tttp..p.p.ssssss.p.ccccc"
        );
        assert_eq!(kinds("a.rs", "for i in 0..10 {"), "kkk...kk.nppnn.p");
        assert_eq!(kinds("a.rs", "x: &'a str = 'q';"), ".p.pp......p.sssp");
    }

    #[test]
    fn block_comments_span_lines() {
        let lang = lang_for(std::path::Path::new("a.c")).unwrap();
        let mut b = false;
        assert_eq!(line(lang, "x /* a", &mut b)[2], Tok::Comment);
        assert!(b);
        let next = line(lang, "b */ y", &mut b);
        assert_eq!(next[0], Tok::Comment);
        assert_eq!(next[5], Tok::Plain);
        assert!(!b);
    }

    #[test]
    fn unknown_files_have_no_language() {
        assert!(lang_for(std::path::Path::new("notes.txt")).is_none());
        assert!(lang_for(std::path::Path::new("Makefile")).is_none());
    }
}
