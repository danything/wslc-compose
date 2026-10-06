//! compose ファイルで使う範囲の YAML パーサー。
//!
//! 対応: ブロックのマップ / シーケンス、フロー (`[a, b]`, `{k: v}`)、引用符付き / なしのスカラー、
//! コメント、ブロックスカラー (`|`, `>`)、アンカー / エイリアス / マージキー (`<<`)。
//! 非対応: 複数ドキュメント、タグ、複数行にまたがるプレーンスカラー、複雑なキー。

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    /// スカラーは文字列のまま持つ。`quoted` は引用符付きかどうか (`true` と `"true"` を区別する)
    Str {
        text: String,
        quoted: bool,
    },
    Seq(Vec<Value>),
    /// 順序を保つ
    Map(Vec<(String, Value)>),
}

impl Value {
    pub fn str(text: impl Into<String>) -> Self {
        Value::Str { text: text.into(), quoted: false }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str { text, .. } => Some(text),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> Option<&[Value]> {
        match self {
            Value::Seq(s) => Some(s),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// YAML 1.2 core schema の真偽値。compose は yes/no も受け付けないので true/false だけ
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Str { text, quoted: false } => match text.as_str() {
                "true" | "True" | "TRUE" => Some(true),
                "false" | "False" | "FALSE" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", tr!("{} 行目: {}", "line {}: {}", self.line, self.message))
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
struct Line {
    /// 1 始まりの行番号
    no: usize,
    indent: usize,
    text: String,
}

pub fn parse(src: &str) -> Result<Value> {
    let mut lines = Vec::new();
    let mut raw = Vec::new();
    for (i, l) in src.lines().enumerate() {
        let l = l.strip_suffix('\r').unwrap_or(l);
        raw.push(l.to_string());
        let indent = l.len() - l.trim_start_matches(' ').len();
        if l[indent..].starts_with('\t') {
            return Err(Error {
                line: i + 1,
                message: tr!("インデントにタブは使えない", "tabs cannot be used for indentation"),
            });
        }
        let text = strip_comment(&l[indent..]);
        let text = text.trim_end();
        if text.is_empty() || text == "---" {
            continue;
        }
        if text == "..." || (text.starts_with("---") && i > 0 && !lines.is_empty()) {
            return Err(Error {
                line: i + 1,
                message: tr!("複数ドキュメントには対応していない", "multiple documents are not supported"),
            });
        }
        lines.push(Line { no: i + 1, indent, text: text.to_string() });
    }
    let mut p = Parser { lines, raw, pos: 0, anchors: HashMap::new() };
    if p.lines.is_empty() {
        return Ok(Value::Null);
    }
    let indent = p.lines[0].indent;
    let v = p.block(indent)?;
    if let Some(l) = p.lines.get(p.pos) {
        return Err(Error { line: l.no, message: tr!("インデントが合わない", "inconsistent indentation") });
    }
    Ok(v)
}

/// 引用符の外にある、行頭か空白の直後の `#` 以降を落とす
fn strip_comment(s: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match quote {
            Some('"') if c == '\\' => {
                chars.next();
            }
            Some(q) if c == q => {
                // '' は単一引用符内のエスケープ
                if q == '\'' && chars.peek().map(|&(_, n)| n) == Some('\'') {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            Some(_) => {}
            None => {
                if c == '#' && prev_space {
                    return &s[..i];
                }
                // 引用符はトークンの先頭にあるときだけ引用として扱う (it's のような語中は除く)
                if (c == '"' || c == '\'') && prev_space_or_indicator(s, i) {
                    quote = Some(c);
                }
            }
        }
        prev_space = c == ' ';
    }
    s
}

fn prev_space_or_indicator(s: &str, i: usize) -> bool {
    match s[..i].chars().next_back() {
        None => true,
        Some(c) => matches!(c, ' ' | '[' | '{' | ',' | ':' | '-'),
    }
}

struct Parser {
    lines: Vec<Line>,
    raw: Vec<String>,
    pos: usize,
    anchors: HashMap<String, Value>,
}

impl Parser {
    fn err<T>(&self, line: usize, message: impl Into<String>) -> Result<T> {
        Err(Error { line, message: message.into() })
    }

    fn peek(&self) -> Option<&Line> {
        self.lines.get(self.pos)
    }

    fn block(&mut self, indent: usize) -> Result<Value> {
        match self.peek() {
            Some(l) if is_seq_item(&l.text) => self.seq(indent),
            Some(_) => self.map(indent),
            None => Ok(Value::Null),
        }
    }

    fn seq(&mut self, indent: usize) -> Result<Value> {
        let mut items = Vec::new();
        while let Some(l) = self.peek() {
            if l.indent != indent || !is_seq_item(&l.text) {
                break;
            }
            let no = l.no;
            let rest = l.text[1..].trim_start().to_string();
            let offset = l.text.len() - rest.len();
            if rest.is_empty() {
                self.pos += 1;
                items.push(self.nested(indent, no)?);
            } else if is_seq_item(&rest) || split_key(&rest).is_some() {
                // "- key: v" / "- - x" は、内容の位置を新しいインデントとして読み直す
                self.lines[self.pos] = Line { no, indent: indent + offset, text: rest };
                items.push(self.block(indent + offset)?);
            } else {
                self.pos += 1;
                items.push(self.inline(&rest, indent, no)?);
            }
        }
        Ok(Value::Seq(items))
    }

    fn map(&mut self, indent: usize) -> Result<Value> {
        let mut entries: Vec<(String, Value)> = Vec::new();
        let mut merges: Vec<Value> = Vec::new();
        while let Some(l) = self.peek() {
            if l.indent != indent {
                if l.indent > indent {
                    return self.err(l.no, tr!("インデントが深すぎる", "unexpected indentation"));
                }
                break;
            }
            if is_seq_item(&l.text) {
                break;
            }
            let no = l.no;
            let text = l.text.clone();
            let Some((key, rest)) = split_key(&text) else {
                return self.err(no, tr!("`key: value` の形になっていない: {text}", "expected `key: value`: {text}"));
            };
            let key = unquote_key(key, no)?;
            let rest = rest.trim().to_string();
            self.pos += 1;
            let value = if rest.is_empty() { self.nested(indent, no)? } else { self.inline(&rest, indent, no)? };
            if key == "<<" {
                match value {
                    Value::Map(_) => merges.push(value),
                    Value::Seq(vs) => merges.extend(vs),
                    _ => {
                        return self.err(
                            no,
                            tr!("<< にはマップかマップのリストを指定する", "<< takes a map or a list of maps"),
                        );
                    }
                }
                continue;
            }
            if entries.iter().any(|(k, _)| *k == key) {
                return self.err(no, tr!("キー `{key}` が重複している", "duplicate key `{key}`"));
            }
            entries.push((key, value));
        }
        // マージキーは、明示したキーを上書きしない。複数あるときは先のものが優先
        for m in merges {
            let Value::Map(ms) = m else {
                return self.err(0, tr!("<< にはマップを指定する", "<< takes a map"));
            };
            for (k, v) in ms {
                if !entries.iter().any(|(ek, _)| *ek == k) {
                    entries.push((k, v));
                }
            }
        }
        Ok(Value::Map(entries))
    }

    /// `key:` や `-` の後に値がないとき、次の行からの入れ子を読む
    fn nested(&mut self, indent: usize, no: usize) -> Result<Value> {
        let _ = no;
        match self.peek() {
            Some(n) if n.indent > indent => {
                let i = n.indent;
                self.block(i)
            }
            // YAML ではマップの値のシーケンスを同じインデントに書ける
            Some(n) if n.indent == indent && is_seq_item(&n.text) => self.seq_after_key(indent),
            _ => Ok(Value::Null),
        }
    }

    fn seq_after_key(&mut self, indent: usize) -> Result<Value> {
        self.seq(indent)
    }

    /// 同じ行に書かれた値 (アンカー・エイリアス・ブロックスカラー・フロー・スカラー)
    fn inline(&mut self, text: &str, indent: usize, no: usize) -> Result<Value> {
        let mut text = text.trim();
        let mut anchor = None;
        if let Some(rest) = text.strip_prefix('&') {
            let end = rest.find(' ').unwrap_or(rest.len());
            anchor = Some(rest[..end].to_string());
            text = rest[end..].trim_start();
        }
        let value = if text.is_empty() {
            self.nested(indent, no)?
        } else if let Some(name) = text.strip_prefix('*') {
            match self.anchors.get(name) {
                Some(v) => v.clone(),
                None => return self.err(no, tr!("アンカー `{name}` が定義されていない", "undefined anchor `{name}`")),
            }
        } else if text.starts_with('|') || text.starts_with('>') {
            self.block_scalar(text, indent, no)?
        } else if text.starts_with('[') || text.starts_with('{') {
            let mut src = text.to_string();
            // 複数行にまたがるフローは、括弧が閉じるまで行を足す
            while !flow_balanced(&src) {
                let Some(l) = self.peek() else {
                    return self.err(no, tr!("フローの括弧が閉じていない", "unclosed flow collection"));
                };
                src.push(' ');
                src.push_str(&l.text);
                self.pos += 1;
            }
            let mut fp = Flow { s: src.as_bytes(), i: 0, src: &src, no, anchors: &self.anchors };
            let v = fp.value()?;
            fp.ws();
            if fp.i != src.len() {
                return self
                    .err(no, tr!("フローの後ろに余計な文字がある", "unexpected characters after flow collection"));
            }
            v
        } else {
            scalar(text, no)?
        };
        if let Some(a) = anchor {
            self.anchors.insert(a, value.clone());
        }
        Ok(value)
    }

    fn block_scalar(&mut self, header: &str, indent: usize, no: usize) -> Result<Value> {
        let folded = header.starts_with('>');
        let chomp = &header[1..];
        if !matches!(chomp, "" | "-" | "+") {
            return self.err(
                no,
                tr!(
                    "ブロックスカラーのインデント指定には対応していない",
                    "block scalar indentation indicators are not supported"
                ),
            );
        }
        // コメント除去済みの行ではなく元の行を使う (# を含められるように)
        let mut body: Vec<String> = Vec::new();
        let mut block_indent = None;
        let mut idx = no; // raw は 0 始まり、no は 1 始まりなので raw[no] が次の行
        let mut last_consumed_line = no;
        while idx < self.raw.len() {
            let r = &self.raw[idx];
            let ind = r.len() - r.trim_start_matches(' ').len();
            if r.trim().is_empty() {
                body.push(String::new());
                idx += 1;
                continue;
            }
            if ind <= indent {
                break;
            }
            let bi = *block_indent.get_or_insert(ind);
            if ind < bi {
                break;
            }
            body.push(r[bi..].to_string());
            last_consumed_line = idx + 1;
            idx += 1;
        }
        while self.peek().is_some_and(|l| l.no <= last_consumed_line) {
            self.pos += 1;
        }
        while body.last().is_some_and(|l| l.is_empty()) {
            body.pop();
        }
        let mut text = if folded {
            let mut out = String::new();
            for (i, l) in body.iter().enumerate() {
                if i > 0 {
                    out.push(if l.is_empty() || body[i - 1].is_empty() { '\n' } else { ' ' });
                }
                out.push_str(l);
            }
            out
        } else {
            body.join("\n")
        };
        if chomp != "-" && !text.is_empty() {
            text.push('\n');
        }
        Ok(Value::Str { text, quoted: true })
    }
}

fn is_seq_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// 引用符と括弧の外にある最初の `: ` (または行末の `:`) で分ける
fn split_key(text: &str) -> Option<(&str, &str)> {
    let b = text.as_bytes();
    if matches!(b.first(), Some(b'[' | b'{')) {
        return None;
    }
    let mut i = 0;
    if matches!(b.first(), Some(b'"' | b'\'')) {
        let q = b[0];
        i = 1;
        while i < b.len() {
            if b[i] == b'\\' && q == b'"' {
                i += 2;
                continue;
            }
            if b[i] == q {
                break;
            }
            i += 1;
        }
        i += 1;
        if i > b.len() {
            return None;
        }
    }
    while i < b.len() {
        if b[i] == b':' && (i + 1 == b.len() || b[i + 1] == b' ') {
            return Some((text[..i].trim_end(), &text[i + 1..]));
        }
        i += 1;
    }
    None
}

fn unquote_key(key: &str, no: usize) -> Result<String> {
    match scalar(key, no)? {
        Value::Str { text, .. } => Ok(text),
        Value::Null => Ok(String::new()),
        _ => Err(Error { line: no, message: tr!("キーにはスカラーを使う", "keys must be scalars") }),
    }
}

fn scalar(text: &str, no: usize) -> Result<Value> {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix('"') {
        let Some(inner) = inner.strip_suffix('"') else {
            return Err(Error {
                line: no, message: tr!("二重引用符が閉じていない", "unclosed double quote")
            });
        };
        return Ok(Value::Str { text: unescape_double(inner, no)?, quoted: true });
    }
    if let Some(inner) = t.strip_prefix('\'') {
        let Some(inner) = inner.strip_suffix('\'') else {
            return Err(Error {
                line: no, message: tr!("単一引用符が閉じていない", "unclosed single quote")
            });
        };
        return Ok(Value::Str { text: inner.replace("''", "'"), quoted: true });
    }
    if matches!(t, "" | "~" | "null" | "Null" | "NULL") {
        return Ok(Value::Null);
    }
    Ok(Value::str(t))
}

fn unescape_double(s: &str, no: usize) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let e = chars
            .next()
            .ok_or_else(|| Error { line: no, message: tr!("末尾の \\ が不正", "trailing \\ is invalid") })?;
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            ' ' => out.push(' '),
            'u' | 'x' | 'U' => {
                let n = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let hex: String = chars.by_ref().take(n).collect();
                let c = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).ok_or_else(|| Error {
                    line: no,
                    message: tr!("不正なエスケープ \\{e}{hex}", "invalid escape \\{e}{hex}"),
                })?;
                out.push(c);
            }
            other => {
                return Err(Error {
                    line: no, message: tr!("不正なエスケープ \\{other}", "invalid escape \\{other}")
                });
            }
        }
    }
    Ok(out)
}

fn flow_balanced(s: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut esc = false;
    for c in s.chars() {
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == '\\' && q == '"' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

struct Flow<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
    no: usize,
    anchors: &'a HashMap<String, Value>,
}

impl Flow<'_> {
    fn err<T>(&self, m: &str) -> Result<T> {
        Err(Error { line: self.no, message: m.to_string() })
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Value> {
        self.ws();
        match self.s.get(self.i) {
            Some(b'[') => self.seq(),
            Some(b'{') => self.map(),
            Some(b'*') => {
                let start = self.i + 1;
                self.i = start;
                while self.i < self.s.len() && !matches!(self.s[self.i], b',' | b']' | b'}' | b' ') {
                    self.i += 1;
                }
                let name = &self.src[start..self.i];
                match self.anchors.get(name) {
                    Some(v) => Ok(v.clone()),
                    None => self.err(&tr!("アンカー `{name}` が定義されていない", "undefined anchor `{name}`")),
                }
            }
            Some(_) => self.scalar(),
            None => self.err(&tr!("値がない", "missing value")),
        }
    }

    fn scalar(&mut self) -> Result<Value> {
        let start = self.i;
        if matches!(self.s[self.i], b'"' | b'\'') {
            let q = self.s[self.i];
            self.i += 1;
            while self.i < self.s.len() {
                if q == b'"' && self.s[self.i] == b'\\' {
                    self.i += 2;
                    continue;
                }
                if self.s[self.i] == q {
                    if q == b'\'' && self.s.get(self.i + 1) == Some(&b'\'') {
                        self.i += 2;
                        continue;
                    }
                    break;
                }
                self.i += 1;
            }
            self.i += 1;
            if self.i > self.s.len() {
                return self.err(&tr!("引用符が閉じていない", "unclosed quote"));
            }
        } else {
            while self.i < self.s.len() && !matches!(self.s[self.i], b',' | b']' | b'}') {
                // フローマップのキーの区切り
                if self.s[self.i] == b':' && matches!(self.s.get(self.i + 1), Some(b' ') | None) {
                    break;
                }
                self.i += 1;
            }
        }
        scalar(&self.src[start..self.i], self.no)
    }

    fn seq(&mut self) -> Result<Value> {
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Seq(items));
                }
                None => return self.err(&tr!("`]` がない", "missing `]`")),
                _ => {}
            }
            items.push(self.value()?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {}
                _ => return self.err(&tr!("フローシーケンスの区切りが不正", "invalid separator in flow sequence")),
            }
        }
    }

    fn map(&mut self) -> Result<Value> {
        self.i += 1;
        let mut entries = Vec::new();
        loop {
            self.ws();
            match self.s.get(self.i) {
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Map(entries));
                }
                None => return self.err(&tr!("`}}` がない", "missing `}}`")),
                _ => {}
            }
            let key = match self.scalar()? {
                Value::Str { text, .. } => text,
                _ => return self.err(&tr!("フローマップのキーが不正", "invalid key in flow mapping")),
            };
            self.ws();
            let value = if self.s.get(self.i) == Some(&b':') {
                self.i += 1;
                self.value()?
            } else {
                Value::Null
            };
            entries.push((key, value));
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {}
                _ => return self.err(&tr!("フローマップの区切りが不正", "invalid separator in flow mapping")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> Value {
        Value::str(t)
    }

    #[test]
    fn nested_maps_and_seqs() {
        let v = parse(
            "services:\n  web:\n    image: nginx # comment\n    ports:\n      - \"8080:80\"\n      - 443:443\n    networks: [default, genkan]\n",
        )
        .unwrap();
        let web = v.get("services").unwrap().get("web").unwrap();
        assert_eq!(web.get("image"), Some(&s("nginx")));
        assert_eq!(
            web.get("ports"),
            Some(&Value::Seq(vec![Value::Str { text: "8080:80".into(), quoted: true }, s("443:443")]))
        );
        assert_eq!(web.get("networks"), Some(&Value::Seq(vec![s("default"), s("genkan")])));
    }

    #[test]
    fn seq_of_maps_and_same_indent_seq() {
        let v = parse("a:\n- x: 1\n  y: 2\n- z\nb: ~\n").unwrap();
        let a = v.get("a").unwrap().as_seq().unwrap();
        assert_eq!(a[0], Value::Map(vec![("x".into(), s("1")), ("y".into(), s("2"))]));
        assert_eq!(a[1], s("z"));
        assert_eq!(v.get("b"), Some(&Value::Null));
    }

    #[test]
    fn anchors_and_merge() {
        let v = parse(
            "x-base: &base\n  image: app\n  environment:\n    A: \"1\"\nservices:\n  one:\n    <<: *base\n    image: other\n  two: *base\n",
        )
        .unwrap();
        let one = v.get("services").unwrap().get("one").unwrap();
        assert_eq!(one.get("image"), Some(&s("other")));
        assert!(one.get("environment").is_some());
        assert_eq!(v.get("services").unwrap().get("two").unwrap().get("image"), Some(&s("app")));
    }

    #[test]
    fn quotes_comments_and_hash_in_value() {
        let v = parse("a: 'it''s # not comment'\nb: \"x\\ty\" # c\nc: foo#bar\nd: it's\n").unwrap();
        assert_eq!(v.get("a").unwrap().as_str(), Some("it's # not comment"));
        assert_eq!(v.get("b").unwrap().as_str(), Some("x\ty"));
        assert_eq!(v.get("c").unwrap().as_str(), Some("foo#bar"));
        assert_eq!(v.get("d").unwrap().as_str(), Some("it's"));
    }

    #[test]
    fn flow_map_and_multiline_flow() {
        let v = parse("a: {k: v, n: [1, \"2\"]}\nb: [\n  x,\n  y\n]\n").unwrap();
        assert_eq!(v.get("a").unwrap().get("k"), Some(&s("v")));
        assert_eq!(v.get("b").unwrap().as_seq().unwrap().len(), 2);
    }

    #[test]
    fn block_scalars() {
        let v = parse("a: |\n  line1\n  # kept\n  line2\nb: >-\n  x\n  y\nc: 1\n").unwrap();
        assert_eq!(v.get("a").unwrap().as_str(), Some("line1\n# kept\nline2\n"));
        assert_eq!(v.get("b").unwrap().as_str(), Some("x y"));
        assert_eq!(v.get("c"), Some(&s("1")));
    }

    #[test]
    fn colon_without_space_is_scalar() {
        let v = parse("volumes:\n  - ./src:/app:ro\n  - C:\\data:/data\nurl: http://x/y\n").unwrap();
        assert_eq!(v.get("volumes").unwrap().as_seq().unwrap()[0], s("./src:/app:ro"));
        assert_eq!(v.get("url"), Some(&s("http://x/y")));
    }

    /// 手元の compose ファイル群で試す: WSLC_COMPOSE_CORPUS=<dir> cargo test -- --ignored
    #[test]
    #[ignore]
    fn corpus() {
        let dir = std::env::var("WSLC_COMPOSE_CORPUS").expect("WSLC_COMPOSE_CORPUS");
        let mut failed = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let src = std::fs::read_to_string(&p).unwrap();
            match parse(&src) {
                Ok(v) if v.get("services").is_some() => {}
                Ok(_) => {
                    failed += 1;
                    eprintln!("{}: services がない", p.display());
                }
                Err(e) => {
                    failed += 1;
                    eprintln!("{}: {e}", p.display());
                }
            }
        }
        assert_eq!(failed, 0);
    }

    #[test]
    fn errors_have_line_numbers() {
        let e = parse("a: 1\n\tb: 2\n").unwrap_err();
        assert_eq!(e.line, 2);
        let e = parse("a: 1\na: 2\n").unwrap_err();
        assert_eq!(e.line, 2);
    }
}
