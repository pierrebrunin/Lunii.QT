//! Translations of the strings built on the Rust side (status bar, log).
//!
//! The Slint strings (`@tr`) use the same `.po` files, bundled at compile time.
//! Messages coming from `lunii-core` are already formatted (`"Missing ri in /x"`):
//! they are translated by matching them against the `{}` templates of the catalog.

use std::collections::HashMap;
use std::sync::OnceLock;

const FR_PO: &str = include_str!("../lang/fr/LC_MESSAGES/lunii-gui.po");

struct Template {
    /// literal parts around the placeholders (n placeholders => n + 1 parts)
    parts: Vec<String>,
    translation: String,
}

#[derive(Default)]
struct Catalog {
    exact: HashMap<String, String>,
    templates: Vec<Template>,
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

fn po_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses `msgid` / `msgstr` pairs (single or multi-line strings).
fn parse_po(content: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let (mut id, mut text) = (None::<String>, None::<String>);
    let mut pending: Vec<(String, String)> = Vec::new();

    for line in content.lines().map(str::trim) {
        let quoted = |l: &str| {
            l.strip_prefix('"')
                .and_then(|l| l.strip_suffix('"'))
                .map(po_unescape)
        };
        if let Some(rest) = line.strip_prefix("msgid ") {
            if let (Some(i), Some(t)) = (id.take(), text.take()) {
                pending.push((i, t));
            }
            id = Some(quoted(rest).unwrap_or_default());
            text = None;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            text = Some(quoted(rest).unwrap_or_default());
        } else if line.starts_with('"') {
            let part = quoted(line).unwrap_or_default();
            if let Some(t) = text.as_mut() {
                t.push_str(&part);
            } else if let Some(i) = id.as_mut() {
                i.push_str(&part);
            }
        }
    }
    if let (Some(i), Some(t)) = (id, text) {
        pending.push((i, t));
    }
    for (i, t) in pending {
        if !i.is_empty() && !t.is_empty() {
            entries.push((i, t));
        }
    }
    entries
}

/// Splits a template on its `{...}` placeholders.
fn split_placeholders(s: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                parts.last_mut().unwrap().push('{');
                continue;
            }
            // skip format spec up to '}'
            for c2 in chars.by_ref() {
                if c2 == '}' {
                    break;
                }
            }
            parts.push(String::new());
        } else if c == '}' && chars.peek() == Some(&'}') {
            chars.next();
            parts.last_mut().unwrap().push('}');
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    parts
}

/// Replaces the placeholders of `template` with `args`, in order.
pub fn format_with(template: &str, args: &[&str]) -> String {
    let parts = split_placeholders(template);
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        out.push_str(part);
        if i + 1 < parts.len() {
            out.push_str(args.get(i).copied().unwrap_or(""));
        }
    }
    out
}

/// Matches `msg` against the literal parts of a template, returning the placeholder values.
fn match_template(parts: &[String], msg: &str) -> Option<Vec<String>> {
    let mut rest = msg.strip_prefix(parts[0].as_str())?;
    let mut values = Vec::new();
    for (i, part) in parts[1..].iter().enumerate() {
        let last = i == parts.len() - 2;
        if last {
            let value = rest.strip_suffix(part.as_str())?;
            values.push(value.to_string());
            rest = "";
        } else {
            if part.is_empty() {
                return None; // two adjacent placeholders: ambiguous
            }
            let pos = rest.find(part.as_str())?;
            values.push(rest[..pos].to_string());
            rest = &rest[pos + part.len()..];
        }
    }
    rest.is_empty().then_some(values)
}

/// Loads the catalog of `language` (only French is available). Returns `false`
/// when the language is not supported (English is then used).
pub fn init(language: &str) -> bool {
    if !language.to_lowercase().starts_with("fr") {
        return false;
    }
    let mut catalog = Catalog::default();
    for (id, text) in parse_po(FR_PO) {
        let parts = split_placeholders(&id);
        if parts.len() > 1 && parts.iter().any(|p| !p.trim().is_empty()) {
            catalog.templates.push(Template {
                parts,
                translation: text.clone(),
            });
        }
        catalog.exact.insert(id, text);
    }
    // longest templates first: the most specific match wins
    catalog
        .templates
        .sort_by_key(|t| std::cmp::Reverse(t.parts.iter().map(String::len).sum::<usize>()));
    let _ = CATALOG.set(catalog);
    true
}

/// Translation of a source string (unchanged if unknown).
pub fn tr(source: &str) -> String {
    CATALOG
        .get()
        .and_then(|c| c.exact.get(source).cloned())
        .unwrap_or_else(|| source.to_string())
}

/// Translates `template` then fills its placeholders.
pub fn trf(template: &str, args: &[&str]) -> String {
    format_with(&tr(template), args)
}

/// Translates an already formatted message (log messages from `lunii-core`).
pub fn tr_message(msg: &str) -> String {
    let Some(catalog) = CATALOG.get() else {
        return msg.to_string();
    };
    if let Some(t) = catalog.exact.get(msg) {
        return t.clone();
    }
    // multi-line errors (anyhow contexts): translate each line
    if msg.contains('\n') {
        return msg.lines().map(tr_message).collect::<Vec<_>>().join("\n");
    }
    // "context: cause" chains
    if let Some((head, tail)) = msg.split_once(": ")
        && (catalog.exact.contains_key(head)
            || catalog
                .templates
                .iter()
                .any(|t| match_template(&t.parts, head).is_some()))
    {
        return format!("{}: {}", tr_message(head), tr_message(tail));
    }
    for t in &catalog.templates {
        if let Some(values) = match_template(&t.parts, msg) {
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            return format_with(&t.translation, &refs);
        }
    }
    msg.to_string()
}

/// Selects the user language for Rust and Slint strings. Returns "fr" or "en".
pub fn apply_language() -> &'static str {
    let lang = if init(&system_language()) { "fr" } else { "en" };
    // "" selects the original (English) strings
    let _ = slint::select_bundled_translation(if lang == "fr" { "fr" } else { "" });
    lang
}

/// Language of the user (`LUNII_LANG` overrides the system locale).
pub fn system_language() -> String {
    std::env::var("LUNII_LANG")
        .ok()
        .or_else(sys_locale::get_locale)
        .unwrap_or_else(|| "en".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn placeholders() {
        assert_eq!(split_placeholders("a {} b {:d} c"), vec!["a ", " b ", " c"]);
        assert_eq!(format_with("x{}y{}", &["1", "2"]), "x1y2");
        let parts = split_placeholders("Missing {} in {}");
        assert_eq!(
            match_template(&parts, "Missing ri in /mnt/x").unwrap(),
            vec!["ri", "/mnt/x"]
        );
        assert!(match_template(&parts, "Something else").is_none());
    }

    #[test]
    fn french_catalog() {
        assert!(init("fr_FR"));
        assert_eq!(tr("Select your Lunii"), "Sélectionner votre Lunii");
        assert_eq!(
            tr_message("Missing ri in /mnt/x"),
            "ri manquant dans /mnt/x"
        );
        assert_eq!(
            tr_message("Not enough space left on Lunii (only 12MB)"),
            "Pas assez de place restant sur la Lunii (reste 12Mo)"
        );
        assert_eq!(tr_message("Totally unknown"), "Totally unknown");
    }

    /// String literals passed to the user facing calls of `source` (errors,
    /// contexts, info / warning / error logs), placeholders included.
    fn user_messages(source: &str) -> Vec<String> {
        const CALLS: [&str; 9] = [
            "bail!(",
            "anyhow!(",
            ".context(",
            "ctx.info(",
            "ctx.warn(",
            "ctx.error(",
            "ctx.info(format!(",
            "ctx.warn(format!(",
            "ctx.error(format!(",
        ];
        let mut out = Vec::new();
        for call in CALLS {
            let mut rest = source;
            while let Some(pos) = rest.find(call) {
                rest = &rest[pos + call.len()..];
                let Some(lit) = rest.trim_start().strip_prefix('"') else {
                    continue;
                };
                let mut msg = String::new();
                let mut chars = lit.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                msg.push(if n == 'n' { '\n' } else { n });
                            }
                        }
                        c => msg.push(c),
                    }
                }
                out.push(msg);
            }
        }
        out
    }

    /// The messages of `lunii-core` shown to the user have a French
    /// translation: they are translated by matching their English text, a
    /// message changed in `lunii-core` would silently lose it.
    #[test]
    fn core_messages_are_translated() {
        assert!(init("fr"));
        // internal errors, left in English (bug reports)
        const TECHNICAL: [&str; 2] = ["can't sync to MPEG frame", "embedded font"];
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lunii-core/src");
        let mut missing = Vec::new();
        let mut checked = 0;
        fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        // crypto errors: internal, left in English
        files.retain(|f| !f.ends_with("crypto.rs"));
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            // the tests of the file are not user facing
            let source = source.split("#[cfg(test)]").next().unwrap();
            for msg in user_messages(source) {
                // `{}`, `{name}`, `{:?}`... replaced by a value
                let mut sample = String::new();
                let mut in_placeholder = false;
                for c in msg.chars() {
                    match c {
                        '{' => in_placeholder = true,
                        '}' if in_placeholder => {
                            in_placeholder = false;
                            sample.push_str("X1");
                        }
                        _ if in_placeholder => {}
                        c => sample.push(c),
                    }
                }
                let text = sample.replace("X1", "");
                if !text.chars().any(char::is_alphabetic) || TECHNICAL.contains(&msg.as_str()) {
                    continue;
                }
                checked += 1;
                if tr_message(&sample) == sample {
                    missing.push(format!(
                        "{}: {msg}",
                        path.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
        // the extraction works (not an empty check)
        assert!(checked > 100, "only {checked} messages found");
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "{} untranslated lunii-core messages:\n{}",
            missing.len(),
            missing.join("\n")
        );
    }

    #[test]
    fn po_parsing() {
        let e = parse_po("msgid \"\"\nmsgstr \"h\"\n\nmsgid \"a\\nb\"\nmsgstr \"c\"\n\"d\"\n");
        assert_eq!(e, vec![("a\nb".to_string(), "cd".to_string())]);
    }
}
