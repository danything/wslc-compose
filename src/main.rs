//! compose.yaml を WSL コンテナ (wslc) で動かす。docker compose のよく使うサブコマンドだけを持つ。

#[macro_use]
mod i18n;
mod cmd;
mod compose;
mod interp;
mod json;
mod run;
mod signal;
mod wslc;
mod yaml;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn usage() -> &'static str {
    if i18n::ja() { USAGE_JA } else { USAGE_EN }
}

const USAGE_EN: &str = "\
Usage: wslc-compose [OPTIONS] <COMMAND> [ARGS]

Options:
  -f, --file <path>           compose file (default: search compose.yaml etc. from the current directory upward)
  -p, --project-name <name>   project name (default: name: / COMPOSE_PROJECT_NAME / directory name)
      --env-file <path>       .env used for interpolation (default: .env next to the compose file)
      --profile <name>        enable a profile (repeatable, or COMPOSE_PROFILES)

Commands:
  up [-d] [--build] [--force-recreate] [--no-deps] [--remove-orphans] [SERVICE...]
  down [-v] [--remove-orphans]
  ps [-a] [-q]
  logs [-f] [-n <lines>] [-t] [--no-log-prefix] [SERVICE...]
  build [--no-cache] [--pull] [SERVICE...]
  pull [SERVICE...]
  exec [-T] [-d] [-e K=V] [-u user] [-w dir] <SERVICE> <COMMAND...>
  run [--rm] [-d] [-T] [--no-deps] [--service-ports] [-p ports] [-e K=V] [--entrypoint cmd]
      [-u user] [-w dir] [--name name] [--build] <SERVICE> [COMMAND...]
  start | stop | restart | kill [SERVICE...]
  config [--services]          show the resolved config and the wslc commands to run
  version

Environment:
  WSLC_COMPOSE_BIN       path to wslc.exe
  WSLC_COMPOSE_VERBOSE   print the wslc commands being run
  WSLC_COMPOSE_LANG      ja / en (default: the Windows display language)
";

const USAGE_JA: &str = "\
使い方: wslc-compose [オプション] <コマンド> [引数]

オプション:
  -f, --file <path>           compose ファイル (既定: カレントから親へ compose.yaml などを探す)
  -p, --project-name <name>   プロジェクト名 (既定: name: / COMPOSE_PROJECT_NAME / ディレクトリ名)
      --env-file <path>       変数展開に使う .env (既定: compose ファイルと同じ場所の .env)
      --profile <name>        有効にする profile (複数可。COMPOSE_PROFILES でも指定できる)

コマンド:
  up [-d] [--build] [--force-recreate] [--no-deps] [--remove-orphans] [サービス...]
  down [-v] [--remove-orphans]
  ps [-a] [-q]
  logs [-f] [-n <行数>] [-t] [--no-log-prefix] [サービス...]
  build [--no-cache] [--pull] [サービス...]
  pull [サービス...]
  exec [-T] [-d] [-e K=V] [-u user] [-w dir] <サービス> <コマンド...>
  run [--rm] [-d] [-T] [--no-deps] [--service-ports] [-p ports] [-e K=V] [--entrypoint cmd]
      [-u user] [-w dir] [--name name] [--build] <サービス> [コマンド...]
  start | stop | restart | kill [サービス...]
  config [--services]          解決した設定と実行する wslc のコマンドを表示
  version

環境変数:
  WSLC_COMPOSE_BIN       wslc.exe の場所
  WSLC_COMPOSE_VERBOSE   実行する wslc のコマンドを表示する
  WSLC_COMPOSE_LANG      ja / en (既定: Windows の表示言語)
";

struct Args {
    items: Vec<String>,
    pos: usize,
}

impl Args {
    fn next(&mut self) -> Option<String> {
        let v = self.items.get(self.pos).cloned();
        self.pos += 1;
        v
    }

    fn value(&mut self, flag: &str) -> Result<String, String> {
        self.next().ok_or_else(|| tr!("{flag} には値が必要", "{flag} requires a value"))
    }

    fn rest(&mut self) -> Vec<String> {
        let r = self.items[self.pos.min(self.items.len())..].to_vec();
        self.pos = self.items.len();
        r
    }
}

/// `-n10` / `--tail=10` のような値付きの書き方を `-n 10` に分ける
fn split_inline(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut passthrough = false;
    for a in args {
        if passthrough {
            out.push(a);
            continue;
        }
        if a == "--" {
            passthrough = true;
            out.push(a);
            continue;
        }
        if let Some((k, v)) = a.strip_prefix("--").and_then(|r| r.split_once('=')) {
            out.push(format!("--{k}"));
            out.push(v.to_string());
        } else {
            out.push(a);
        }
    }
    out
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("{}", tr!("エラー: {e}", "error: {e}"));
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<u8, String> {
    let mut args = Args { items: split_inline(std::env::args().skip(1).collect()), pos: 0 };
    let mut opts = compose::LoadOptions { file: None, project_name: None, env_files: vec![], profiles: vec![] };
    let sub = loop {
        let Some(a) = args.next() else {
            print!("{}", usage());
            return Ok(2);
        };
        match a.as_str() {
            "-f" | "--file" => opts.file = Some(PathBuf::from(args.value(&a)?)),
            "-p" | "--project-name" => opts.project_name = Some(args.value(&a)?),
            "--env-file" => opts.env_files.push(PathBuf::from(args.value(&a)?)),
            "--profile" => opts.profiles.push(args.value(&a)?),
            "-h" | "--help" | "help" => {
                print!("{}", usage());
                return Ok(0);
            }
            "-v" | "--version" | "version" => {
                println!("wslc-compose {}", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            _ if a.starts_with('-') => return Err(tr!("不明なオプション {a}", "unknown option {a}")),
            _ => break a,
        }
    };

    let load = |o: &compose::LoadOptions| -> Result<compose::Project, String> {
        let p = compose::load(o)?;
        for w in &p.warnings {
            eprintln!("{}", tr!("警告: {w}", "warning: {w}"));
        }
        Ok(p)
    };
    let wait = Duration::from_secs(
        std::env::var("WSLC_COMPOSE_WAIT_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(300),
    );

    match sub.as_str() {
        "up" => {
            let mut o = cmd::UpOptions {
                detach: false,
                build: false,
                force_recreate: false,
                no_deps: false,
                remove_orphans: false,
                wait_timeout: wait,
            };
            let mut names = vec![];
            while let Some(a) = args.next() {
                match a.as_str() {
                    "-d" | "--detach" => o.detach = true,
                    "--build" => o.build = true,
                    "--force-recreate" => o.force_recreate = true,
                    "--no-deps" => o.no_deps = true,
                    "--remove-orphans" => o.remove_orphans = true,
                    "--wait" => o.detach = true,
                    _ if a.starts_with('-') => return Err(tr!("up: 不明なオプション {a}", "up: unknown option {a}")),
                    _ => names.push(a),
                }
            }
            cmd::up(&load(&opts)?, &names, &o)?;
        }
        "down" => {
            let (mut v, mut orphans) = (false, false);
            while let Some(a) = args.next() {
                match a.as_str() {
                    "-v" | "--volumes" => v = true,
                    "--remove-orphans" => orphans = true,
                    _ => return Err(tr!("down: 不明な引数 {a}", "down: unknown argument {a}")),
                }
            }
            cmd::down(&load(&opts)?, v, orphans)?;
        }
        "ps" => {
            let (mut all, mut quiet) = (false, false);
            while let Some(a) = args.next() {
                match a.as_str() {
                    "-a" | "--all" => all = true,
                    "-q" | "--quiet" => quiet = true,
                    _ => return Err(tr!("ps: 不明な引数 {a}", "ps: unknown argument {a}")),
                }
            }
            cmd::ps(&load(&opts)?, all, quiet)?;
        }
        "logs" => {
            let mut o = cmd::LogOptions { follow: false, tail: None, timestamps: false, no_prefix: false };
            let mut names = vec![];
            while let Some(a) = args.next() {
                match a.as_str() {
                    "-f" | "--follow" => o.follow = true,
                    "-n" | "--tail" => o.tail = Some(args.value(&a)?),
                    "-t" | "--timestamps" => o.timestamps = true,
                    "--no-log-prefix" => o.no_prefix = true,
                    _ if a.starts_with('-') => {
                        return Err(tr!("logs: 不明なオプション {a}", "logs: unknown option {a}"));
                    }
                    _ => names.push(a),
                }
            }
            cmd::logs(&load(&opts)?, &names, &o)?;
        }
        "build" => {
            let (mut no_cache, mut pull) = (false, false);
            let mut names = vec![];
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--no-cache" => no_cache = true,
                    "--pull" => pull = true,
                    _ if a.starts_with('-') => {
                        return Err(tr!("build: 不明なオプション {a}", "build: unknown option {a}"));
                    }
                    _ => names.push(a),
                }
            }
            cmd::build(&load(&opts)?, &names, no_cache, pull)?;
        }
        "pull" => cmd::pull(&load(&opts)?, &args.rest())?,
        "start" | "stop" | "restart" | "kill" => cmd::lifecycle(&load(&opts)?, &args.rest(), &sub)?,
        "exec" => {
            let mut o = cmd::ExecOptions { no_tty: false, detach: false, env: vec![], user: None, workdir: None };
            let service = loop {
                let a = args.next().ok_or_else(|| tr!("exec: サービス名が必要", "exec: service name required"))?;
                match a.as_str() {
                    "-T" | "--no-TTY" => o.no_tty = true,
                    "-d" | "--detach" => o.detach = true,
                    "-e" | "--env" => o.env.push(args.value(&a)?),
                    "-u" | "--user" => o.user = Some(args.value(&a)?),
                    "-w" | "--workdir" => o.workdir = Some(args.value(&a)?),
                    "-i" | "--interactive" | "-t" | "--tty" => {}
                    _ if a.starts_with('-') => {
                        return Err(tr!("exec: 不明なオプション {a}", "exec: unknown option {a}"));
                    }
                    _ => break a,
                }
            };
            let rest = strip_dashdash(args.rest());
            if rest.is_empty() {
                return Err(tr!("exec: コマンドが必要", "exec: command required"));
            }
            let code = cmd::exec(&load(&opts)?, &service, &rest, &o)?;
            return Ok(code.clamp(0, 255) as u8);
        }
        "run" => {
            let mut o = cmd::RunOptions {
                rm: false,
                detach: false,
                no_deps: false,
                no_tty: false,
                service_ports: false,
                publish: vec![],
                env: vec![],
                entrypoint: None,
                user: None,
                workdir: None,
                name: None,
                build: false,
                wait_timeout: wait,
            };
            let service = loop {
                let a = args.next().ok_or_else(|| tr!("run: サービス名が必要", "run: service name required"))?;
                match a.as_str() {
                    "--rm" => o.rm = true,
                    "-d" | "--detach" => o.detach = true,
                    "--no-deps" => o.no_deps = true,
                    "-T" | "--no-TTY" => o.no_tty = true,
                    "--service-ports" => o.service_ports = true,
                    "-p" | "--publish" => o.publish.push(args.value(&a)?),
                    "-e" | "--env" => {
                        let v = args.value(&a)?;
                        let (k, val) = match v.split_once('=') {
                            Some((k, val)) => (k.to_string(), val.to_string()),
                            None => (v.clone(), std::env::var(&v).unwrap_or_default()),
                        };
                        o.env.push((k, val));
                    }
                    "--entrypoint" => o.entrypoint = Some(args.value(&a)?),
                    "-u" | "--user" => o.user = Some(args.value(&a)?),
                    "-w" | "--workdir" => o.workdir = Some(args.value(&a)?),
                    "--name" => o.name = Some(args.value(&a)?),
                    "--build" => o.build = true,
                    "-i" | "--interactive" => {}
                    _ if a.starts_with('-') => return Err(tr!("run: 不明なオプション {a}", "run: unknown option {a}")),
                    _ => break a,
                }
            };
            let rest = strip_dashdash(args.rest());
            let code = cmd::run_once(&load(&opts)?, &service, &rest, &o)?;
            return Ok(code.clamp(0, 255) as u8);
        }
        "config" => {
            let services_only = matches!(args.next().as_deref(), Some("--services"));
            cmd::config(&load(&opts)?, services_only)?;
        }
        other => {
            return Err(tr!(
                "不明なコマンド {other} (wslc-compose --help)",
                "unknown command {other} (see wslc-compose --help)"
            ));
        }
    }
    Ok(0)
}

fn strip_dashdash(mut v: Vec<String>) -> Vec<String> {
    if v.first().is_some_and(|a| a == "--") {
        v.remove(0);
    }
    v
}
