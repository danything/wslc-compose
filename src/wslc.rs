//! wslc.exe の呼び出し。

use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;

use crate::json::{self, Json};

pub const LABEL_PROJECT: &str = "com.docker.compose.project";
pub const LABEL_SERVICE: &str = "com.docker.compose.service";
pub const LABEL_NUMBER: &str = "com.docker.compose.container-number";
pub const LABEL_ONEOFF: &str = "com.docker.compose.oneoff";
pub const LABEL_HASH: &str = "com.docker.compose.config-hash";
pub const LABEL_WORKDIR: &str = "com.docker.compose.project.working_dir";
pub const LABEL_NETWORK: &str = "com.docker.compose.network";
pub const LABEL_VOLUME: &str = "com.docker.compose.volume";

pub fn exe() -> &'static PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        if let Some(p) = std::env::var_os("WSLC_COMPOSE_BIN") {
            return PathBuf::from(p);
        }
        let default = PathBuf::from(r"C:\Program Files\WSL\wslc.exe");
        if default.is_file() { default } else { PathBuf::from("wslc") }
    })
}

pub fn verbose() -> bool {
    std::env::var_os("WSLC_COMPOSE_VERBOSE").is_some()
}

pub fn command(args: &[String]) -> Command {
    if verbose() {
        eprintln!("+ wslc {}", args.iter().map(|a| crate::compose::shell_quote(a)).collect::<Vec<_>>().join(" "));
    }
    let mut c = Command::new(exe());
    c.args(args);
    c
}

/// 出力を受け取る。失敗したら stderr を含めたエラーにする
pub fn output(args: &[String]) -> Result<String, String> {
    let out = command(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("wslc を実行できない ({}): {e}", exe().display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "wslc {} が失敗した: {}",
            args.first().map(String::as_str).unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 端末に出力をそのまま流す
pub fn run(args: &[String]) -> Result<ExitStatus, String> {
    command(args).status().map_err(|e| format!("wslc を実行できない ({}): {e}", exe().display()))
}

pub fn run_ok(args: &[String]) -> Result<(), String> {
    let st = run(args)?;
    if st.success() { Ok(()) } else { Err(format!("wslc {} が失敗した ({st})", args.join(" "))) }
}

/// 存在確認 (inspect が成功するか)
pub fn exists(kind: &str, name: &str) -> bool {
    command(&[kind.into(), "inspect".into(), name.into()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn inspect(kind: &str, name: &str) -> Option<Json> {
    let out = output(&[kind.into(), "inspect".into(), "--format".into(), "json".into(), name.into()]).ok()?;
    let v = json::parse(out.trim()).ok()?;
    match v {
        Json::Arr(mut a) if !a.is_empty() => Some(a.swap_remove(0)),
        Json::Obj(_) => Some(v),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
    pub labels: Vec<(String, String)>,
}

impl Container {
    pub fn label(&self, k: &str) -> Option<&str> {
        self.labels.iter().find(|(lk, _)| lk == k).map(|(_, v)| v.as_str())
    }

    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// プロジェクトのコンテナ一覧 (停止中も含む)
pub fn containers(project: &str) -> Result<Vec<Container>, String> {
    let out = output(&[
        "container".into(),
        "list".into(),
        "--all".into(),
        "--no-trunc".into(),
        "--filter".into(),
        format!("label={LABEL_PROJECT}={project}"),
        "--format".into(),
        "json".into(),
    ])?;
    let items = parse_list(&out)?;
    Ok(items
        .iter()
        .map(|c| {
            let s = |k: &str| c.get(k).and_then(Json::as_str).unwrap_or("").to_string();
            Container {
                id: s("ID"),
                name: s("Names"),
                image: s("Image"),
                state: s("State"),
                status: s("Status"),
                ports: s("Ports"),
                labels: parse_labels(&s("Labels")),
            }
        })
        .collect())
}

/// `list --format json` は 1 行 1 オブジェクトか配列のどちらか
pub fn parse_list(out: &str) -> Result<Vec<Json>, String> {
    let t = out.trim();
    if t.starts_with('[') {
        match json::parse(t).map_err(|e| e.to_string())? {
            Json::Arr(a) => Ok(a),
            _ => Ok(vec![]),
        }
    } else {
        json::parse_lines(t).map_err(|e| e.to_string())
    }
}

/// `a=b,c={"x":1},d=e` 形式。値の JSON の中のカンマで切らないように括弧の深さを見る
pub fn parse_labels(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let b = s.as_bytes();
    let mut push = |part: &str| {
        if let Some((k, v)) = part.split_once('=') {
            out.push((k.to_string(), v.to_string()));
        }
    };
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        push(&s[start..]);
    }
    out
}

pub fn list_names(kind: &str, label: &str, value: &str) -> Result<Vec<String>, String> {
    let out = output(&[
        kind.into(),
        "list".into(),
        "--filter".into(),
        format!("label={label}={value}"),
        "--format".into(),
        "json".into(),
    ])?;
    let items = parse_list(&out)?;
    Ok(items
        .iter()
        .filter_map(|n| n.get("Name").or_else(|| n.get("Names")).and_then(Json::as_str).map(str::to_string))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_with_json_values() {
        let l = parse_labels(
            r#"a=1,com.microsoft.wsl.container.metadata={"V1":{"Ports":[{"A":1},{"B":2}]}},maintainer=X <x@y>"#,
        );
        assert_eq!(l.len(), 3);
        assert_eq!(l[2], ("maintainer".into(), "X <x@y>".into()));
    }
}
