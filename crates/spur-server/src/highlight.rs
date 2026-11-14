//! Simple syntax colours: one generic tokeniser, configured per language by a small
//! table. It is line-local (a block comment or multi-line string is coloured on its
//! first line only) and emits only the fixed `syntax.*` names. Tree-sitter replaces
//! it later.

use spur_rpc::{Line, Span};

const KEYWORD: &str = "syntax.keyword";
const TYPE: &str = "syntax.type";
const FUNCTION: &str = "syntax.function";
const STRING: &str = "syntax.string";
const NUMBER: &str = "syntax.number";
const COMMENT: &str = "syntax.comment";
const PUNCT: &str = "syntax.punctuation";

/// How one language is tokenised.
struct Lang {
    keywords: &'static [&'static str],
    line_comment: Option<&'static str>,
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    /// Rust: `'x'` is a char, `'ident` a lifetime; `name!` is a macro call.
    rust: bool,
}

const SLASH_BLOCK: Option<(&str, &str)> = Some(("/*", "*/"));

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const C: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register", "return",
    "short", "signed", "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned",
    "void", "volatile", "while", "bool", "true", "false", "NULL",
];
const CPP: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "explicit",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "if",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "noexcept",
    "nullptr",
    "operator",
    "override",
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
    "throw",
    "true",
    "try",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];
const GO: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
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
const JAVA: &[&str] = &[
    "abstract",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "throws",
    "true",
    "try",
    "var",
    "void",
    "while",
];
const JS: &[&str] = &[
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
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];
const TS: &[&str] = &[
    "abstract",
    "any",
    "as",
    "async",
    "await",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "never",
    "new",
    "null",
    "number",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "static",
    "string",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unknown",
    "var",
    "void",
    "while",
    "yield",
];
const PYTHON: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];
const SHELL: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "return", "then", "until", "while",
];
const LITERALS: &[&str] = &["true", "false", "null"];

fn lang(id: &str) -> Option<Lang> {
    let slash = |keywords, quotes| Lang {
        keywords,
        line_comment: Some("//"),
        block_comment: SLASH_BLOCK,
        quotes,
        rust: false,
    };
    let hash = |keywords, quotes| Lang {
        keywords,
        line_comment: Some("#"),
        block_comment: None,
        quotes,
        rust: false,
    };
    Some(match id {
        "rust" => Lang {
            rust: true,
            ..slash(RUST, &['"'])
        },
        "c" => slash(C, &['"', '\'']),
        "cpp" => slash(CPP, &['"', '\'']),
        "go" => slash(GO, &['"', '\'', '`']),
        "java" => slash(JAVA, &['"', '\'']),
        "javascript" => slash(JS, &['"', '\'', '`']),
        "typescript" => slash(TS, &['"', '\'', '`']),
        "python" => hash(PYTHON, &['"', '\'']),
        "shell" => hash(SHELL, &['"', '\'']),
        "toml" => hash(&["true", "false"], &['"', '\'']),
        "yaml" => hash(LITERALS, &['"', '\'']),
        "json" => Lang {
            keywords: LITERALS,
            line_comment: None,
            block_comment: None,
            quotes: &['"'],
            rust: false,
        },
        // markdown and text stay plain.
        _ => return None,
    })
}

/// Replaces each line's spans (plain text from the editor) with highlighted ones
/// for language `lang_id`. The session highlights only the rows it sends: rows the
/// client copies from its last surface were highlighted when first sent, and a line's
/// colours depend only on its text and language.
pub fn apply(lang_id: &str, lines: &mut [Line]) {
    if lang(lang_id).is_none() {
        return;
    }
    for line in lines {
        let text: String = std::mem::take(&mut line.spans)
            .into_iter()
            .map(|s| s.text)
            .collect();
        line.spans = spans(lang_id, text);
    }
}

/// `text` (one line of a document in language `lang_id`) as highlighted spans.
pub fn spans(lang_id: &str, text: String) -> Vec<Span> {
    let Some(lang) = lang(lang_id) else {
        return vec![Span { text, hl: None }];
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut push = |s: &[char], hl: Option<&'static str>| {
        if s.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.hl.as_deref() == hl => last.text.extend(s),
            _ => out.push(Span {
                text: s.iter().collect(),
                hl: hl.map(str::to_owned),
            }),
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let (end, hl) = token(&lang, &chars, i);
        let end = end.max(i + 1).min(chars.len());
        push(&chars[i..end], hl);
        i = end;
    }
    if out.is_empty() {
        out.push(Span { text, hl: None });
    }
    out
}

fn starts(chars: &[char], i: usize, pat: &str) -> bool {
    (i..)
        .zip(pat.chars())
        .all(|(k, p)| chars.get(k) == Some(&p))
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The token starting at `i`: where it ends and its highlight name.
fn token(lang: &Lang, chars: &[char], i: usize) -> (usize, Option<&'static str>) {
    let c = chars[i];
    let n = chars.len();
    if c.is_whitespace() {
        let end = (i..n).find(|&k| !chars[k].is_whitespace()).unwrap_or(n);
        return (end, None);
    }
    if let Some(lc) = lang.line_comment
        && starts(chars, i, lc)
    {
        return (n, Some(COMMENT));
    }
    if let Some((open, close)) = lang.block_comment
        && starts(chars, i, open)
    {
        let from = i + open.chars().count();
        let end = (from..n)
            .find(|&k| starts(chars, k, close))
            .map_or(n, |k| k + close.chars().count());
        return (end, Some(COMMENT));
    }
    if lang.rust && c == '\'' {
        return rust_quote(chars, i);
    }
    if lang.quotes.contains(&c) {
        return (string_end(chars, i, c), Some(STRING));
    }
    if c.is_ascii_digit() && (i == 0 || !is_ident(chars[i - 1])) {
        return (number_end(chars, i), Some(NUMBER));
    }
    if is_ident(c) {
        let end = (i..n).find(|&k| !is_ident(chars[k])).unwrap_or(n);
        let word = &chars[i..end];
        let hl = if lang
            .keywords
            .iter()
            .any(|k| k.len() == word.len() && k.chars().eq(word.iter().copied()))
        {
            Some(KEYWORD)
        } else if chars.get(end) == Some(&'(')
            || (lang.rust && chars.get(end) == Some(&'!') && chars.get(end + 1) != Some(&'='))
        {
            Some(FUNCTION)
        } else if c.is_uppercase() {
            Some(TYPE)
        } else {
            None
        };
        return (end, hl);
    }
    if c.is_ascii_punctuation() {
        return (i + 1, Some(PUNCT));
    }
    (i + 1, None)
}

/// The end of a string opened by `q` at `i`: after the closing quote, or the line's
/// end. A backslash escapes the next char.
fn string_end(chars: &[char], i: usize, q: char) -> usize {
    let mut k = i + 1;
    while k < chars.len() {
        match chars[k] {
            '\\' => k += 2,
            c if c == q => return k + 1,
            _ => k += 1,
        }
    }
    chars.len()
}

/// Rust after a `'`: a char literal (`'x'`, `'\n'`, `'\u{..}'`), else a lifetime.
fn rust_quote(chars: &[char], i: usize) -> (usize, Option<&'static str>) {
    let n = chars.len();
    match chars.get(i + 1) {
        Some('\\') => {
            let close = (i + 2..n).find(|&k| chars[k] == '\'' && chars[k - 1] != '\\');
            close.map_or((n, Some(STRING)), |k| (k + 1, Some(STRING)))
        }
        Some(_) if chars.get(i + 2) == Some(&'\'') => (i + 3, Some(STRING)),
        Some(&c) if is_ident(c) => {
            let end = (i + 1..n).find(|&k| !is_ident(chars[k])).unwrap_or(n);
            (end, Some(TYPE))
        }
        _ => (i + 1, Some(PUNCT)),
    }
}

/// The end of a number at `i`: decimal with `_`, fraction and exponent, or a `0x`,
/// `0o` or `0b` form; then any letter suffix (`u32`, `f64`).
fn number_end(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let at = |k: usize| chars.get(k).copied().unwrap_or(' ');
    let mut k = i;
    if at(i) == '0' && matches!(at(i + 1), 'x' | 'o' | 'b' | 'X' | 'O' | 'B') {
        k = i + 2;
        while k < n && (at(k).is_ascii_hexdigit() || at(k) == '_') {
            k += 1;
        }
    } else {
        while k < n && (at(k).is_ascii_digit() || at(k) == '_') {
            k += 1;
        }
        if at(k) == '.' && at(k + 1).is_ascii_digit() {
            k += 1;
            while k < n && (at(k).is_ascii_digit() || at(k) == '_') {
                k += 1;
            }
        }
        if matches!(at(k), 'e' | 'E') {
            let d = if matches!(at(k + 1), '+' | '-') {
                k + 2
            } else {
                k + 1
            };
            if at(d).is_ascii_digit() {
                k = d;
                while k < n && at(k).is_ascii_digit() {
                    k += 1;
                }
            }
        }
    }
    while k < n && is_ident(at(k)) {
        k += 1;
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spans as `name:text` (name without `syntax.`, `-` for none), whitespace-only
    /// spans left out.
    fn show(lang: &str, text: &str) -> Vec<String> {
        spans(lang, text.into())
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| {
                let name = s.hl.as_deref().map_or("-", |h| &h[7..]);
                format!("{name}:{}", s.text.trim())
            })
            .collect()
    }

    #[test]
    fn rust_line() {
        assert_eq!(
            show(
                "rust",
                "pub fn add<'a>(x: &'a str, n: u32) -> Vec<u8> { let c = '\\n'; v!(0x1F_u8, 1.5e3) } // hi"
            ),
            [
                "keyword:pub",
                "keyword:fn",
                "-:add",
                "punctuation:<",
                "type:'a",
                "punctuation:>(",
                "-:x",
                "punctuation::",
                "punctuation:&",
                "type:'a",
                "-:str",
                "punctuation:,",
                "-:n",
                "punctuation::",
                "-:u32",
                "punctuation:)",
                "punctuation:->",
                "type:Vec",
                "punctuation:<",
                "-:u8",
                "punctuation:>",
                "punctuation:{",
                "keyword:let",
                "-:c",
                "punctuation:=",
                "string:'\\n'",
                "punctuation:;",
                "function:v",
                "punctuation:!(",
                "number:0x1F_u8",
                "punctuation:,",
                "number:1.5e3",
                "punctuation:)",
                "punctuation:}",
                "comment:// hi",
            ]
        );
        assert_eq!(
            show("rust", "'x' '\\u{1F600}'"),
            ["string:'x'", "string:'\\u{1F600}'"]
        );
    }

    #[test]
    fn block_comments_and_strings_stop_at_the_line_end() {
        assert_eq!(
            show("c", "int x = 1; /* a */ y"),
            [
                "keyword:int",
                "-:x",
                "punctuation:=",
                "number:1",
                "punctuation:;",
                "comment:/* a */",
                "-:y"
            ]
        );
        assert_eq!(show("c", "/* open"), ["comment:/* open"]);
        assert_eq!(
            show("go", "s := `raw"),
            ["-:s", "punctuation::=", "string:`raw"]
        );
        assert_eq!(
            show("javascript", r#"f("a\"b", 'c')"#),
            [
                "function:f",
                "punctuation:(",
                r#"string:"a\"b""#,
                "punctuation:,",
                "string:'c'",
                "punctuation:)"
            ]
        );
    }

    #[test]
    fn hash_languages() {
        assert_eq!(
            show("python", "def f(x): return None  # note"),
            [
                "keyword:def",
                "function:f",
                "punctuation:(",
                "-:x",
                "punctuation:):",
                "keyword:return",
                "keyword:None",
                "comment:# note"
            ]
        );
        assert_eq!(
            show("toml", "name = \"spur\" # id"),
            ["-:name", "punctuation:=", "string:\"spur\"", "comment:# id"]
        );
        assert_eq!(
            show("shell", "if [ -n \"$x\" ]; then"),
            [
                "keyword:if",
                "punctuation:[",
                "punctuation:-",
                "-:n",
                "string:\"$x\"",
                "punctuation:];",
                "keyword:then"
            ]
        );
        assert_eq!(
            show("yaml", "on: true"),
            ["-:on", "punctuation::", "keyword:true"]
        );
    }

    #[test]
    fn other_languages() {
        assert_eq!(
            show("json", r#"{"a": [1, null]}"#),
            [
                "punctuation:{",
                "string:\"a\"",
                "punctuation::",
                "punctuation:[",
                "number:1",
                "punctuation:,",
                "keyword:null",
                "punctuation:]}"
            ]
        );
        assert_eq!(
            show("typescript", "const n: number = 2;"),
            [
                "keyword:const",
                "-:n",
                "punctuation::",
                "keyword:number",
                "punctuation:=",
                "number:2",
                "punctuation:;"
            ]
        );
        assert_eq!(
            show("java", "class A {}"),
            ["keyword:class", "type:A", "punctuation:{}"]
        );
        assert_eq!(show("cpp", "x2 = 3"), ["-:x2", "punctuation:=", "number:3"]);
        assert_eq!(show("markdown", "# Title \"x\""), ["-:# Title \"x\""]);
        assert_eq!(show("text", "fn main()"), ["-:fn main()"]);
    }

    #[test]
    fn spans_join_back_to_the_line() {
        for (lang, line) in [
            ("rust", "  let s = \"unterminated"),
            ("rust", "'"),
            ("c", "a/**/b"),
            ("python", "x = 'a\\"),
            ("rust", ""),
        ] {
            let text: String = spans(lang, line.into())
                .iter()
                .map(|s| s.text.as_str())
                .collect();
            assert_eq!(text, line);
        }
    }
}
