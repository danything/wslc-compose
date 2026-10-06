//! compose の変数展開と `.env` の読み込み。
//!
//! 対応: `$VAR` `${VAR}` `${VAR:-def}` `${VAR-def}` `${VAR:+alt}` `${VAR+alt}` `${VAR:?err}` `${VAR?err}` `$$`。
//! 既定値の中の入れ子 (`${A:-${B}}`) も展開する。

use std::collections::HashMap;
use std::path::Path;

use crate::yaml::Value;

pub struct Env {
    vars: HashMap<String, String>,
}

impl Env {
    /// プロセスの環境変数が `.env` より優先される (compose と同じ)
    pub fn load(project_dir: &Path, env_files: &[std::path::PathBuf]) -> Result<Self, String> {
        let mut vars = HashMap::new();
        let defaults = if env_files.is_empty() { vec![project_dir.join(".env")] } else { env_files.to_vec() };
        for f in &defaults {
            match std::fs::read_to_string(f) {
                Ok(src) => vars.extend(parse_env_file(&src).map_err(|e| format!("{}: {e}", f.display()))?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && env_files.is_empty() => {}
                Err(e) => return Err(format!("{}: {e}", f.display())),
            }
        }
        vars.extend(std::env::vars());
        // Windows には HOME がないので、${HOME} を使う compose ファイルのために USERPROFILE で補う
        if !vars.contains_key("HOME")
            && let Some(p) = vars.get("USERPROFILE").cloned()
        {
            vars.insert("HOME".into(), p);
        }
        Ok(Env { vars })
    }

    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Env { vars: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str)
    }

    pub fn expand(&self, s: &str) -> Result<String, String> {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            if let Some(r) = rest.strip_prefix('$') {
                out.push('$');
                rest = r;
            } else if let Some(r) = rest.strip_prefix('{') {
                let end = matching_brace(r).ok_or_else(|| format!("`${{` が閉じていない: {s}"))?;
                out.push_str(&self.braced(&r[..end])?);
                rest = &r[end + 1..];
            } else {
                let n = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
                if n == 0 {
                    out.push('$');
                } else {
                    out.push_str(self.get(&rest[..n]).unwrap_or(""));
                    rest = &rest[n..];
                }
            }
        }
        out.push_str(rest);
        Ok(out)
    }

    fn braced(&self, inner: &str) -> Result<String, String> {
        let n = inner.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(inner.len());
        let (name, op) = inner.split_at(n);
        if name.is_empty() {
            return Err(format!("変数名がない: ${{{inner}}}"));
        }
        let val = self.get(name);
        let set_nonempty = val.is_some_and(|v| !v.is_empty());
        let (colon, op) = match op.strip_prefix(':') {
            Some(o) => (true, o),
            None => (false, op),
        };
        let present = if colon { set_nonempty } else { val.is_some() };
        match op.chars().next() {
            None if !colon => Ok(val.unwrap_or("").to_string()),
            Some('-') => {
                if present {
                    Ok(val.unwrap_or("").to_string())
                } else {
                    self.expand(&op[1..])
                }
            }
            Some('+') => {
                if present {
                    self.expand(&op[1..])
                } else {
                    Ok(String::new())
                }
            }
            Some('?') => {
                if present {
                    Ok(val.unwrap_or("").to_string())
                } else {
                    let msg = self.expand(&op[1..])?;
                    Err(format!("{name} が必要: {}", if msg.is_empty() { "未設定" } else { &msg }))
                }
            }
            _ => Err(format!("不正な変数展開: ${{{inner}}}")),
        }
    }

    /// YAML の値の中のスカラーをすべて展開する (キーは展開しない。compose と同じ)
    pub fn expand_value(&self, v: &Value) -> Result<Value, String> {
        Ok(match v {
            Value::Str { text, quoted } => Value::Str { text: self.expand(text)?, quoted: *quoted },
            Value::Seq(items) => Value::Seq(items.iter().map(|i| self.expand_value(i)).collect::<Result<_, _>>()?),
            Value::Map(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(k, v)| {
                        // x- で始まる拡張フィールドは使う側で展開されるので、そのまま
                        if k.starts_with("x-") {
                            Ok((k.clone(), v.clone()))
                        } else {
                            Ok((k.clone(), self.expand_value(v)?))
                        }
                    })
                    .collect::<Result<_, String>>()?,
            ),
            Value::Null => Value::Null,
        })
    }
}

fn matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(i),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// `.env` / `env_file` の形式。`KEY=VALUE`、`export` 付き、引用符、`#` コメント
pub fn parse_env_file(src: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            // 値なしの KEY は、ホストの環境変数を渡す意味
            out.push((line.to_string(), std::env::var(line).unwrap_or_default()));
            continue;
        };
        let k = k.trim();
        if k.is_empty() || k.contains(' ') {
            return Err(format!("{} 行目: 不正な行", i + 1));
        }
        let v = v.trim();
        let v = if let Some(inner) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            inner.replace("\\n", "\n").replace("\\\"", "\"")
        } else if let Some(inner) = v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
            inner.to_string()
        } else {
            // 引用符なしは ` #` 以降がコメント
            match v.find(" #") {
                Some(n) => v[..n].trim_end().to_string(),
                None => v.to_string(),
            }
        };
        out.push((k.to_string(), v));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_forms() {
        let e = Env::from_pairs(&[("A", "a"), ("EMPTY", "")]);
        assert_eq!(e.expand("x$A-${A}").unwrap(), "xa-a");
        assert_eq!(e.expand("${MISSING:-d}|${EMPTY:-d}|${EMPTY-d}").unwrap(), "d|d|");
        assert_eq!(e.expand("${A:+y}|${MISSING:+y}").unwrap(), "y|");
        assert_eq!(e.expand("$$A ${MISSING:-${A}}").unwrap(), "$A a");
        assert!(e.expand("${MISSING:?need it}").unwrap_err().contains("need it"));
        assert_eq!(e.expand("${REGISTRY:-ghcr.io}/${IMAGE:-5ym/x}:${TAG:-latest}").unwrap(), "ghcr.io/5ym/x:latest");
    }

    #[test]
    fn env_file() {
        let v = parse_env_file("# c\nA=1\nexport B=\"two words\"\nC='x # y'\nD=z # comment\n").unwrap();
        assert_eq!(
            v,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "x # y".into()),
                ("D".into(), "z".into())
            ]
        );
    }
}
