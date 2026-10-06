//! compose ファイルをプロジェクトのモデルに読み込む。

use std::path::{Path, PathBuf};

use crate::interp::{Env, parse_env_file};
use crate::yaml::{self, Value};

pub const FILE_NAMES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub dir: PathBuf,
    pub services: Vec<Service>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
    /// 対応していない設定の警告
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub image: Option<String>,
    pub build: Option<Build>,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub environment: Vec<(String, String)>,
    pub ports: Vec<String>,
    pub volumes: Vec<Mount>,
    pub tmpfs: Vec<String>,
    /// (プロジェクト内のネットワーク名, 追加の別名)
    pub networks: Vec<(String, Vec<String>)>,
    pub depends_on: Vec<(String, Condition)>,
    pub healthcheck: Option<Healthcheck>,
    pub labels: Vec<(String, String)>,
    pub container_name: Option<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub hostname: Option<String>,
    pub tty: bool,
    pub stdin_open: bool,
    pub shm_size: Option<String>,
    pub stop_signal: Option<String>,
    pub stop_grace_period: Option<String>,
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Build {
    pub context: PathBuf,
    pub dockerfile: Option<PathBuf>,
    pub args: Vec<(String, String)>,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    Started,
    Healthy,
    CompletedSuccessfully,
}

#[derive(Debug, Clone)]
pub struct Healthcheck {
    /// `None` は disable
    pub test: Option<String>,
    pub interval: Option<String>,
    pub timeout: Option<String>,
    pub retries: Option<String>,
    pub start_period: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MountSource {
    /// ホストの絶対パス
    Bind(PathBuf),
    /// プロジェクト内のボリューム名
    Volume(String),
    /// 名前なしボリューム
    Anonymous,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub source: MountSource,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub struct Network {
    /// compose ファイル内の名前
    pub key: String,
    /// wslc 上の名前
    pub name: String,
    pub external: bool,
    pub driver: Option<String>,
    pub internal: bool,
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Volume {
    pub key: String,
    pub name: String,
    pub external: bool,
    pub driver: Option<String>,
    pub labels: Vec<(String, String)>,
}

pub struct LoadOptions {
    pub file: Option<PathBuf>,
    pub project_name: Option<String>,
    pub env_files: Vec<PathBuf>,
    pub profiles: Vec<String>,
}

impl Project {
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn network(&self, key: &str) -> Option<&Network> {
        self.networks.iter().find(|n| n.key == key)
    }

    pub fn volume(&self, key: &str) -> Option<&Volume> {
        self.volumes.iter().find(|v| v.key == key)
    }

    /// 指定したサービスと、その依存を起動順 (依存が先) に並べる。空なら有効な全サービス
    pub fn ordered(&self, names: &[String], with_deps: bool) -> Result<Vec<&Service>, String> {
        let roots: Vec<String> = if names.is_empty() {
            self.services.iter().map(|s| s.name.clone()).collect()
        } else {
            for n in names {
                if self.service(n).is_none() {
                    return Err(tr!(
                        "サービス `{n}` がない (profiles で無効になっている可能性もある)",
                        "no such service `{n}` (it may be disabled by profiles)"
                    ));
                }
            }
            names.to_vec()
        };
        let mut out: Vec<&Service> = Vec::new();
        let mut visiting: Vec<String> = Vec::new();
        fn visit<'a>(
            p: &'a Project,
            name: &str,
            with_deps: bool,
            out: &mut Vec<&'a Service>,
            visiting: &mut Vec<String>,
        ) -> Result<(), String> {
            if out.iter().any(|s| s.name == name) {
                return Ok(());
            }
            if visiting.iter().any(|v| v == name) {
                return Err(tr!(
                    "depends_on が循環している: {} -> {name}",
                    "circular depends_on: {} -> {name}",
                    visiting.join(" -> ")
                ));
            }
            let s = p
                .service(name)
                .ok_or_else(|| tr!("depends_on の `{name}` がない", "depends_on refers to missing service `{name}`"))?;
            visiting.push(name.to_string());
            if with_deps {
                for (d, _) in &s.depends_on {
                    visit(p, d, with_deps, out, visiting)?;
                }
            }
            visiting.pop();
            out.push(s);
            Ok(())
        }
        for r in &roots {
            visit(self, r, with_deps, &mut out, &mut visiting)?;
        }
        Ok(out)
    }

    pub fn container_name(&self, s: &Service) -> String {
        s.container_name.clone().unwrap_or_else(|| format!("{}-{}-1", self.name, s.name))
    }

    pub fn image_name(&self, s: &Service) -> String {
        s.image.clone().unwrap_or_else(|| format!("{}-{}", self.name, s.name))
    }
}

pub fn find_file(start: &Path) -> Option<PathBuf> {
    // compose と同じく、カレントから親へたどって探す
    let mut dir = Some(start);
    while let Some(d) = dir {
        for n in FILE_NAMES {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
        dir = d.parent();
    }
    None
}

pub fn load(opts: &LoadOptions) -> Result<Project, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let file = match &opts.file {
        Some(f) => std::path::absolute(f).map_err(|e| e.to_string())?,
        None => find_file(&cwd).ok_or_else(|| tr!("{} が見つからない", "{} not found", FILE_NAMES.join(" / ")))?,
    };
    let dir = file.parent().unwrap_or(&cwd).to_path_buf();
    let src = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let env = Env::load(&dir, &opts.env_files)?;
    load_str(&src, &dir, &env, opts).map_err(|e| format!("{}: {e}", file.display()))
}

pub fn load_str(src: &str, dir: &Path, env: &Env, opts: &LoadOptions) -> Result<Project, String> {
    let root = yaml::parse(src).map_err(|e| e.to_string())?;
    let root = env.expand_value(&root)?;
    let mut warnings = Vec::new();

    let name = opts
        .project_name
        .clone()
        .or_else(|| env.get("COMPOSE_PROJECT_NAME").map(str::to_string))
        .or_else(|| root.get("name").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let name = normalize_project_name(&name);
    if name.is_empty() {
        return Err(tr!("プロジェクト名を決められない (-p で指定する)", "cannot determine the project name (use -p)"));
    }

    let mut profiles = opts.profiles.clone();
    if profiles.is_empty()
        && let Some(p) = env.get("COMPOSE_PROFILES")
    {
        profiles = p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }

    let mut networks = Vec::new();
    for (key, v) in root.get("networks").and_then(Value::as_map).unwrap_or_default() {
        let external = v.get("external").and_then(Value::as_bool).unwrap_or(false);
        let explicit = v.get("name").and_then(Value::as_str).map(str::to_string);
        networks.push(Network {
            key: key.clone(),
            name: explicit.unwrap_or_else(|| if external { key.clone() } else { format!("{name}_{key}") }),
            external,
            driver: v.get("driver").and_then(Value::as_str).map(str::to_string),
            internal: v.get("internal").and_then(Value::as_bool).unwrap_or(false),
            labels: kv_list(v.get("labels"), "networks.labels")?,
        });
    }

    let mut volumes = Vec::new();
    for (key, v) in root.get("volumes").and_then(Value::as_map).unwrap_or_default() {
        let external = v.get("external").and_then(Value::as_bool).unwrap_or(false);
        let explicit = v.get("name").and_then(Value::as_str).map(str::to_string);
        volumes.push(Volume {
            key: key.clone(),
            name: explicit.unwrap_or_else(|| if external { key.clone() } else { format!("{name}_{key}") }),
            external,
            driver: v.get("driver").and_then(Value::as_str).map(str::to_string),
            labels: kv_list(v.get("labels"), "volumes.labels")?,
        });
    }

    let services_v =
        root.get("services").and_then(Value::as_map).ok_or_else(|| tr!("services がない", "no services"))?;
    let mut services = Vec::new();
    let mut uses_default_network = false;
    for (sname, sv) in services_v {
        let s = service(sname, sv, dir, &mut warnings).map_err(|e| format!("services.{sname}: {e}"))?;
        if !s.profiles.is_empty()
            && !s.profiles.iter().any(|p| profiles.contains(p) || p == "*")
            && !profiles.iter().any(|p| p == "*")
        {
            continue;
        }
        if s.networks.iter().any(|(n, _)| n == "default") {
            uses_default_network = true;
        }
        services.push(s);
    }
    // 指定されたサービスが profiles で外れたサービスに依存しているときは、そのサービスも有効にする (compose と同じ)
    loop {
        let missing: Vec<String> = services
            .iter()
            .flat_map(|s| s.depends_on.iter().map(|(d, _)| d.clone()))
            .filter(|d| !services.iter().any(|s| &s.name == d))
            .collect();
        if missing.is_empty() {
            break;
        }
        let mut added = false;
        for m in missing {
            if let Some(sv) = services_v.iter().find(|(n, _)| *n == m).map(|(_, v)| v) {
                let s = service(&m, sv, dir, &mut warnings).map_err(|e| format!("services.{m}: {e}"))?;
                if s.networks.iter().any(|(n, _)| n == "default") {
                    uses_default_network = true;
                }
                services.push(s);
                added = true;
            } else {
                return Err(tr!("depends_on の `{m}` がない", "depends_on refers to missing service `{m}`"));
            }
        }
        if !added {
            break;
        }
    }

    if uses_default_network && !networks.iter().any(|n| n.key == "default") {
        networks.push(Network {
            key: "default".into(),
            name: format!("{name}_default"),
            external: false,
            driver: None,
            internal: false,
            labels: Vec::new(),
        });
    }
    for s in &services {
        for (n, _) in &s.networks {
            if !networks.iter().any(|x| &x.key == n) {
                return Err(tr!(
                    "services.{}: networks の `{n}` がトップレベルの networks にない",
                    "services.{}: network `{n}` is not defined in top-level networks",
                    s.name
                ));
            }
        }
        for m in &s.volumes {
            if let MountSource::Volume(v) = &m.source
                && !volumes.iter().any(|x| &x.key == v)
            {
                return Err(tr!(
                    "services.{}: volumes の `{v}` がトップレベルの volumes にない",
                    "services.{}: volume `{v}` is not defined in top-level volumes",
                    s.name
                ));
            }
        }
    }

    Ok(Project { name, dir: dir.to_path_buf(), services, networks, volumes, warnings })
}

/// compose と同じく、小文字・数字・`_`・`-` だけにする
pub fn normalize_project_name(n: &str) -> String {
    let s: String = n
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    s.trim_start_matches(['_', '-']).to_string()
}

const KNOWN_KEYS: &[&str] = &[
    "image",
    "build",
    "command",
    "entrypoint",
    "environment",
    "env_file",
    "ports",
    "volumes",
    "tmpfs",
    "networks",
    "depends_on",
    "healthcheck",
    "labels",
    "container_name",
    "working_dir",
    "user",
    "hostname",
    "tty",
    "stdin_open",
    "shm_size",
    "stop_signal",
    "stop_grace_period",
    "profiles",
    "restart",
    "init",
    "expose",
    "platform",
    "pull_policy",
];

fn service(name: &str, v: &Value, dir: &Path, warnings: &mut Vec<String>) -> Result<Service, String> {
    let m = v.as_map().ok_or_else(|| tr!("マップではない", "not a map"))?;
    for (k, _) in m {
        if !KNOWN_KEYS.contains(&k.as_str()) && !k.starts_with("x-") {
            warnings.push(tr!(
                "services.{name}.{k} には対応していないので無視する",
                "services.{name}.{k} is not supported and is ignored"
            ));
        }
    }
    for k in ["restart", "init"] {
        if v.get(k).is_some() {
            warnings.push(tr!(
                "services.{name}.{k} は wslc に対応する機能がないので無視する",
                "services.{name}.{k} has no wslc equivalent and is ignored"
            ));
        }
    }

    let build = match v.get("build") {
        None | Some(Value::Null) => None,
        Some(b @ Value::Str { .. }) => {
            Some(Build { context: resolve(dir, b.as_str().unwrap()), dockerfile: None, args: vec![], target: None })
        }
        Some(b) => {
            let context = resolve(dir, b.get("context").and_then(Value::as_str).unwrap_or("."));
            Some(Build {
                dockerfile: b.get("dockerfile").and_then(Value::as_str).map(|d| resolve(&context, d)),
                args: kv_list(b.get("args"), "build.args")?,
                target: b.get("target").and_then(Value::as_str).map(str::to_string),
                context,
            })
        }
    };
    let image = v.get("image").and_then(Value::as_str).map(str::to_string);
    if image.is_none() && build.is_none() {
        return Err(tr!("image か build が必要", "image or build is required"));
    }

    let mut environment = Vec::new();
    for f in str_list(v.get("env_file")) {
        let p = resolve(dir, &f);
        let src = std::fs::read_to_string(&p).map_err(|e| format!("env_file {}: {e}", p.display()))?;
        environment.extend(parse_env_file(&src).map_err(|e| format!("env_file {}: {e}", p.display()))?);
    }
    for (k, val) in env_list(v.get("environment"))? {
        environment.retain(|(ek, _)| *ek != k);
        // 値なしはホストの環境変数を渡す。ホストにもなければ設定しない
        match val {
            Some(val) => environment.push((k, val)),
            None => {
                if let Ok(h) = std::env::var(&k) {
                    environment.push((k, h));
                }
            }
        }
    }

    let mut volumes = Vec::new();
    for mv in v.get("volumes").and_then(Value::as_seq).unwrap_or_default() {
        volumes.push(mount(mv, dir)?);
    }

    let networks = match v.get("networks") {
        None | Some(Value::Null) => vec![("default".to_string(), vec![])],
        Some(Value::Seq(items)) => items.iter().filter_map(Value::as_str).map(|n| (n.to_string(), vec![])).collect(),
        Some(Value::Map(entries)) => entries.iter().map(|(n, cfg)| (n.clone(), str_list(cfg.get("aliases")))).collect(),
        _ => return Err(tr!("networks が不正", "invalid networks")),
    };

    let depends_on = match v.get("depends_on") {
        None | Some(Value::Null) => vec![],
        Some(Value::Seq(items)) => {
            items.iter().filter_map(Value::as_str).map(|n| (n.to_string(), Condition::Started)).collect()
        }
        Some(Value::Map(entries)) => entries
            .iter()
            .map(|(n, cfg)| {
                let c = match cfg.get("condition").and_then(Value::as_str) {
                    None | Some("service_started") => Condition::Started,
                    Some("service_healthy") => Condition::Healthy,
                    Some("service_completed_successfully") => Condition::CompletedSuccessfully,
                    Some(other) => {
                        return Err(tr!(
                            "depends_on.{n}.condition `{other}` には対応していない",
                            "depends_on.{n}.condition `{other}` is not supported"
                        ));
                    }
                };
                Ok((n.clone(), c))
            })
            .collect::<Result<_, String>>()?,
        _ => return Err(tr!("depends_on が不正", "invalid depends_on")),
    };

    let healthcheck = match v.get("healthcheck") {
        None | Some(Value::Null) => None,
        Some(h) => {
            let disabled = h.get("disable").and_then(Value::as_bool).unwrap_or(false);
            let test = match h.get("test") {
                _ if disabled => None,
                None => None,
                Some(Value::Str { text, .. }) => Some(text.clone()),
                Some(Value::Seq(items)) => {
                    let parts: Vec<String> = items.iter().filter_map(Value::as_str).map(str::to_string).collect();
                    match parts.first().map(String::as_str) {
                        Some("NONE") => None,
                        Some("CMD-SHELL") => Some(parts[1..].join(" ")),
                        Some("CMD") => Some(parts[1..].iter().map(|p| shell_quote(p)).collect::<Vec<_>>().join(" ")),
                        _ => {
                            return Err(tr!(
                                "healthcheck.test は CMD / CMD-SHELL / NONE で始める",
                                "healthcheck.test must start with CMD, CMD-SHELL or NONE"
                            ));
                        }
                    }
                }
                _ => return Err(tr!("healthcheck.test が不正", "invalid healthcheck.test")),
            };
            let get = |k: &str| h.get(k).and_then(Value::as_str).map(str::to_string);
            Some(Healthcheck {
                test,
                interval: get("interval"),
                timeout: get("timeout"),
                retries: get("retries"),
                start_period: get("start_period"),
            })
        }
    };

    let tmpfs = str_list(v.get("tmpfs"));
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Service {
        name: name.to_string(),
        image,
        build,
        command: command(v.get("command"))?,
        entrypoint: command(v.get("entrypoint"))?,
        environment,
        ports: v.get("ports").and_then(Value::as_seq).unwrap_or_default().iter().map(port).collect::<Result<_, _>>()?,
        volumes,
        tmpfs,
        networks,
        depends_on,
        healthcheck,
        labels: kv_list(v.get("labels"), "labels")?,
        container_name: get("container_name"),
        working_dir: get("working_dir"),
        user: get("user"),
        hostname: get("hostname"),
        tty: v.get("tty").and_then(Value::as_bool).unwrap_or(false),
        stdin_open: v.get("stdin_open").and_then(Value::as_bool).unwrap_or(false),
        shm_size: get("shm_size"),
        stop_signal: get("stop_signal"),
        stop_grace_period: get("stop_grace_period"),
        profiles: str_list(v.get("profiles")),
    })
}

fn resolve(base: &Path, p: &str) -> PathBuf {
    let p = if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
        PathBuf::from(home).join(rest)
    } else {
        base.join(p)
    };
    std::path::absolute(&p).unwrap_or(p)
}

fn is_host_path(s: &str) -> bool {
    s.starts_with('.')
        || s.starts_with('/')
        || s.starts_with('~')
        || s.starts_with('\\')
        || (s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic())
}

fn mount(v: &Value, dir: &Path) -> Result<Mount, String> {
    if let Some(s) = v.as_str() {
        // C:\x:/y:ro のドライブ文字のコロンで切らない
        let (src, rest) = if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
            let i = s[2..].find(':').map(|i| i + 2);
            match i {
                Some(i) => (Some(&s[..i]), &s[i + 1..]),
                None => (None, s),
            }
        } else {
            match s.split_once(':') {
                Some((a, b)) => (Some(a), b),
                None => (None, s),
            }
        };
        let (target, opts) = rest.split_once(':').unwrap_or((rest, ""));
        let read_only = opts.split(',').any(|o| o == "ro");
        let source = match src {
            None => MountSource::Anonymous,
            Some(p) if is_host_path(p) => MountSource::Bind(resolve(dir, p)),
            Some(n) => MountSource::Volume(n.to_string()),
        };
        return Ok(Mount { source, target: target.to_string(), read_only });
    }
    let target = v
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| tr!("volumes の target がない", "volumes: missing target"))?
        .to_string();
    let read_only = v.get("read_only").and_then(Value::as_bool).unwrap_or(false);
    let src = v.get("source").and_then(Value::as_str);
    let source = match (v.get("type").and_then(Value::as_str).unwrap_or("volume"), src) {
        ("bind", Some(p)) => MountSource::Bind(resolve(dir, p)),
        ("volume", Some(n)) => MountSource::Volume(n.to_string()),
        ("volume", None) => MountSource::Anonymous,
        (t, _) => return Err(tr!("volumes の type `{t}` には対応していない", "volumes: type `{t}` is not supported")),
    };
    Ok(Mount { source, target, read_only })
}

fn port(v: &Value) -> Result<String, String> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    let target = v
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| tr!("ports の target がない", "ports: missing target"))?;
    let mut s = String::new();
    if let Some(ip) = v.get("host_ip").and_then(Value::as_str) {
        s.push_str(ip);
        s.push(':');
    }
    if let Some(p) = v.get("published").and_then(Value::as_str) {
        s.push_str(p);
        s.push(':');
    }
    s.push_str(target);
    if let Some(proto) = v.get("protocol").and_then(Value::as_str) {
        s.push('/');
        s.push_str(proto);
    }
    Ok(s)
}

fn command(v: Option<&Value>) -> Result<Option<Vec<String>>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str { text, .. }) => shell_split(text).map(Some),
        Some(Value::Seq(items)) => Ok(Some(items.iter().map(|i| i.as_str().unwrap_or("").to_string()).collect())),
        _ => Err(tr!("command / entrypoint が不正", "invalid command / entrypoint")),
    }
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Str { text, .. }) => vec![text.clone()],
        Some(Value::Seq(items)) => items.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => vec![],
    }
}

/// `KEY: value` のマップと `KEY=value` のリストの両方を受け付ける
fn kv_list(v: Option<&Value>, what: &str) -> Result<Vec<(String, String)>, String> {
    Ok(env_list(v).map_err(|e| format!("{what}: {e}"))?.into_iter().map(|(k, v)| (k, v.unwrap_or_default())).collect())
}

fn env_list(v: Option<&Value>) -> Result<Vec<(String, Option<String>)>, String> {
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Map(entries)) => {
            Ok(entries.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string))).collect())
        }
        Some(Value::Seq(items)) => Ok(items
            .iter()
            .filter_map(Value::as_str)
            .map(|s| match s.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (s.to_string(), None),
            })
            .collect()),
        _ => Err(tr!("マップか `KEY=value` のリストにする", "expected a map or a list of `KEY=value`")),
    }
}

/// POSIX シェル風の分割 (compose が文字列の command を分けるのと同じ)
pub fn shell_split(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err(tr!("引用符が閉じていない: {s}", "unclosed quote: {s}")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err(tr!("引用符が閉じていない: {s}", "unclosed quote: {s}")),
                        },
                        Some(c) => cur.push(c),
                        None => return Err(tr!("引用符が閉じていない: {s}", "unclosed quote: {s}")),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    cur.push(c);
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> LoadOptions {
        LoadOptions { file: None, project_name: None, env_files: vec![], profiles: vec![] }
    }

    #[test]
    fn loads_typical_file() {
        let env = Env::from_pairs(&[("TAG", "1.2")]);
        let src = r#"
name: My_App
services:
  web:
    build:
      context: .
      args:
        UID: "1000"
    image: app:${TAG:-latest}
    command: php artisan serve --host "0.0.0.0"
    ports: ["8080:80"]
    environment:
      - A=1
      - B=two=2
    volumes:
      - ./src:/app:ro
      - data:/data
      - /cache
    networks: [default, genkan]
    depends_on:
      db:
        condition: service_healthy
    restart: always
  db:
    image: mariadb
    healthcheck:
      test: ["CMD", "healthcheck.sh", "--connect"]
      interval: 5s
  tool:
    image: busybox
    profiles: [manual]
networks:
  genkan:
    external: true
volumes:
  data:
"#;
        let p = load_str(src, Path::new("/proj"), &env, &opts()).unwrap();
        assert_eq!(p.name, "my_app");
        assert_eq!(p.services.len(), 2, "profiles のサービスは除く");
        let web = p.service("web").unwrap();
        assert_eq!(web.image.as_deref(), Some("app:1.2"));
        assert_eq!(web.command.as_deref().unwrap(), ["php", "artisan", "serve", "--host", "0.0.0.0"]);
        assert_eq!(web.environment, vec![("A".into(), "1".into()), ("B".into(), "two=2".into())]);
        assert_eq!(web.volumes[1].source, MountSource::Volume("data".into()));
        assert_eq!(web.volumes[2].source, MountSource::Anonymous);
        assert!(web.volumes[0].read_only);
        assert_eq!(web.depends_on, vec![("db".into(), Condition::Healthy)]);
        assert_eq!(p.network("genkan").unwrap().name, "genkan");
        assert_eq!(p.network("default").unwrap().name, "my_app_default");
        assert_eq!(p.volume("data").unwrap().name, "my_app_data");
        assert_eq!(
            p.service("db").unwrap().healthcheck.as_ref().unwrap().test.as_deref(),
            Some("healthcheck.sh --connect")
        );
        assert!(p.warnings.iter().any(|w| w.contains("restart")));
        let order: Vec<_> = p.ordered(&["web".into()], true).unwrap().iter().map(|s| s.name.clone()).collect();
        assert_eq!(order, ["db", "web"]);
    }

    #[test]
    fn windows_bind_paths() {
        let m = mount(&Value::str(r"C:\data:/data:ro"), Path::new(r"C:\proj")).unwrap();
        assert_eq!(m.target, "/data");
        assert!(m.read_only);
        assert!(matches!(m.source, MountSource::Bind(_)));
    }

    #[test]
    fn shell_split_quotes() {
        assert_eq!(shell_split(r#"sh -c 'echo "a b"' x\ y"#).unwrap(), ["sh", "-c", "echo \"a b\"", "x y"]);
    }
}
