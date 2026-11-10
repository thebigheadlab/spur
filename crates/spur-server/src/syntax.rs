//! A small line lexer that tags text with semantic token names (`syntax.keyword`, ...).
//! It knows nothing about colours; frontends map the names through theme tokens.
//! Each line is lexed on its own, so multi-line strings and block comments are only
//! partly recognised. Tree-sitter replaces this later.

use spur_rpc::Span;

/// Lexing rules for one language.
pub(crate) struct Lang {
    pub name: &'static str,
    line_comment: &'static [&'static str],
    keywords: &'static [&'static str],
    /// Chars that open and close a string.
    quotes: &'static [char],
}

const RUST: Lang = Lang {
    name: "Rust",
    line_comment: &["//"],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ],
    quotes: &['"'],
};
const PYTHON: Lang = Lang {
    name: "Python",
    line_comment: &["#"],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True",
        "try", "while", "with", "yield",
    ],
    quotes: &['"', '\''],
};
const JS: Lang = Lang {
    name: "JavaScript",
    line_comment: &["//"],
    keywords: &[
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
        "super",
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
    ],
    quotes: &['"', '\'', '`'],
};
const GO: Lang = Lang {
    name: "Go",
    line_comment: &["//"],
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "false",
        "for",
        "func",
        "go",
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
    ],
    quotes: &['"', '`'],
};
const C: Lang = Lang {
    name: "C",
    line_comment: &["//"],
    keywords: &[
        "auto",
        "bool",
        "break",
        "case",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "false",
        "float",
        "for",
        "if",
        "include",
        "inline",
        "int",
        "long",
        "namespace",
        "nullptr",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "true",
        "typedef",
        "union",
        "unsigned",
        "using",
        "void",
        "while",
    ],
    quotes: &['"', '\''],
};
const TOML: Lang = Lang {
    name: "TOML",
    line_comment: &["#"],
    keywords: &["true", "false"],
    quotes: &['"', '\''],
};
const SHELL: Lang = Lang {
    name: "Shell",
    line_comment: &["#"],
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "return", "then", "while",
    ],
    quotes: &['"', '\''],
};
const JSON: Lang = Lang {
    name: "JSON",
    line_comment: &[],
    keywords: &["true", "false", "null"],
    quotes: &['"'],
};
const MARKDOWN: Lang = Lang {
    name: "Markdown",
    line_comment: &[],
    keywords: &[],
    quotes: &[],
};
const PLAIN: Lang = Lang {
    name: "Text",
    line_comment: &[],
    keywords: &[],
    quotes: &[],
};

/// Picks the language from a file name.
pub(crate) fn language(file_name: &str) -> &'static Lang {
    let ext = file_name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "rs" => &RUST,
        "py" => &PYTHON,
        "js" | "jsx" | "ts" | "tsx" | "mjs" => &JS,
        "go" => &GO,
        "c" | "h" | "cc" | "cpp" | "hpp" => &C,
        "toml" => &TOML,
        "sh" | "bash" | "zsh" => &SHELL,
        "json" => &JSON,
        "md" | "markdown" => &MARKDOWN,
        _ => &PLAIN,
    }
}

fn push(out: &mut Vec<Span>, text: &str, style: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.style.as_deref() == style
    {
        last.text.push_str(text);
        return;
    }
    out.push(Span {
        text: text.to_owned(),
        style: style.map(str::to_owned),
    });
}

/// Lexes one line into spans whose texts join back to `line`.
pub(crate) fn highlight(lang: &Lang, line: &str) -> Vec<Span> {
    let mut out = Vec::new();
    if lang.name == PLAIN.name {
        push(&mut out, line, None);
        return out;
    }
    if lang.name == MARKDOWN.name {
        let style = if line.starts_with('#') {
            Some("syntax.keyword")
        } else if line.trim_start().starts_with("```") || line.starts_with("    ") {
            Some("syntax.string")
        } else if line.trim_start().starts_with('>') {
            Some("syntax.comment")
        } else {
            None
        };
        push(&mut out, line, style);
        return out;
    }
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        let c = rest.chars().next().unwrap_or_default();
        if lang.line_comment.iter().any(|p| rest.starts_with(p)) {
            push(&mut out, rest, Some("syntax.comment"));
            break;
        }
        if lang.quotes.contains(&c) {
            // A lone quote in Rust is a lifetime or a char; `'a'` still reads fine.
            let mut end = rest.len();
            let mut escaped = false;
            for (j, ch) in rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == c {
                    end = j + ch.len_utf8();
                    break;
                }
            }
            push(&mut out, &rest[..end], Some("syntax.string"));
            i += end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            push(&mut out, &rest[..end], Some("syntax.number"));
            i += end;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let after = rest[end..].chars().next();
            let style = if lang.keywords.contains(&word) {
                Some("syntax.keyword")
            } else if after == Some('(') || after == Some('!') && lang.name == "Rust" {
                Some("syntax.function")
            } else if c.is_uppercase() {
                Some("syntax.type")
            } else {
                None
            };
            push(&mut out, word, style);
            i += end;
            continue;
        }
        let style = (!c.is_whitespace()).then_some("syntax.punctuation");
        push(&mut out, &rest[..c.len_utf8()], style);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(file: &str, line: &str) -> Vec<(String, String)> {
        let spans = highlight(language(file), line);
        assert_eq!(spans.iter().map(|s| &*s.text).collect::<String>(), line);
        spans
            .into_iter()
            .filter_map(|s| {
                let style = s.style?;
                Some((s.text, style.trim_start_matches("syntax.").to_owned()))
            })
            .collect()
    }

    #[test]
    fn rust_line() {
        let t = tags("a.rs", r#"pub fn new(x: u8) -> Self { "a\"b" } // hi"#);
        let has = |text: &str, tag: &str| t.iter().any(|(a, b)| a == text && b == tag);
        assert!(has("pub", "keyword") && has("fn", "keyword") && has("Self", "keyword"));
        assert!(has("new", "function"));
        assert!(has(r#""a\"b""#, "string"));
        assert!(has("// hi", "comment"));
    }

    #[test]
    fn other_languages_and_plain_text() {
        assert_eq!(tags("x.txt", "fn x() // y"), vec![]);
        assert!(tags("x.toml", "a = 12 # c").contains(&("12".into(), "number".into())));
        assert!(tags("x.md", "# Title").contains(&("# Title".into(), "keyword".into())));
        assert!(tags("x.py", "def f(): 'é' ").contains(&("'é'".into(), "string".into())));
        assert_eq!(language("Makefile").name, "Text");
        assert_eq!(tags("a.rs", ""), vec![]);
    }
}
