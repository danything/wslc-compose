//! wslc の `--format json` の出力を読むための最小限の JSON パーサー。

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// 数値は使う箇所で必要な型に変換する
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `a.b.c` のようにたどる
    pub fn path(&self, path: &str) -> Option<&Json> {
        path.split('.').try_fold(self, |v, k| v.get(k))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) => n.parse().ok(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON: {}", self.0)
    }
}

impl std::error::Error for Error {}

pub fn parse(src: &str) -> Result<Json, Error> {
    let mut p = P { s: src.as_bytes(), i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(Error(format!("{} バイト目以降に余計な文字がある", p.i)));
    }
    Ok(v)
}

/// 1 行に 1 つずつ JSON が並ぶ出力 (`list --format json` など) を読む
pub fn parse_lines(src: &str) -> Result<Vec<Json>, Error> {
    src.lines().map(str::trim).filter(|l| !l.is_empty()).map(parse).collect()
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn err<T>(&self, m: &str) -> Result<T, Error> {
        Err(Error(format!("{} ({} バイト目)", m, self.i)))
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> Result<(), Error> {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            self.err(&format!("`{lit}` を期待した"))
        }
    }

    fn value(&mut self) -> Result<Json, Error> {
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => self.obj(),
            Some(b'[') => self.arr(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.eat("true").map(|_| Json::Bool(true)),
            Some(b'f') => self.eat("false").map(|_| Json::Bool(false)),
            Some(b'n') => self.eat("null").map(|_| Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                Ok(Json::Num(String::from_utf8_lossy(&self.s[start..self.i]).into_owned()))
            }
            _ => self.err("値がない"),
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return self.err("文字列が閉じていない");
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return self.err("不正なエスケープ");
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            // サロゲートペア
                            if (0xD800..0xDC00).contains(&cp) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            let ch = char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER);
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return self.err("不正なエスケープ"),
                    }
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).or_else(|_| self.err("UTF-8 ではない"))
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let h = self.s.get(self.i..self.i + 4).and_then(|h| std::str::from_utf8(h).ok());
        let Some(v) = h.and_then(|h| u32::from_str_radix(h, 16).ok()) else {
            return self.err("不正な \\u");
        };
        self.i += 4;
        Ok(v)
    }

    fn arr(&mut self) -> Result<Json, Error> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return self.err("`,` か `]` を期待した"),
            }
        }
    }

    fn obj(&mut self) -> Result<Json, Error> {
        self.i += 1;
        let mut entries = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Json::Obj(entries));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return self.err("キーを期待した");
            }
            let k = self.string()?;
            self.ws();
            self.eat(":")?;
            let v = self.value()?;
            entries.push((k, v));
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(entries));
                }
                _ => return self.err("`,` か `}` を期待した"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wslc_like_output() {
        let v = parse(r#"[{"Id":"abc","State":{"Running":true,"ExitCode":0,"Health":{"Status":"healthy"}},"Config":{"Labels":{"a":"bé"}}}]"#).unwrap();
        let c = &v.as_array().unwrap()[0];
        assert_eq!(c.path("State.Health.Status").unwrap().as_str(), Some("healthy"));
        assert_eq!(c.path("State.Running").unwrap().as_bool(), Some(true));
        assert_eq!(c.path("State.ExitCode").unwrap().as_i64(), Some(0));
        assert_eq!(c.path("Config.Labels.a").unwrap().as_str(), Some("bé"));
    }

    #[test]
    fn json_lines() {
        let v = parse_lines("{\"a\":1}\n\n{\"a\":2}\n").unwrap();
        assert_eq!(v.len(), 2);
    }
}
