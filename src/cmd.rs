//! サブコマンドの実装。

use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::compose::{Condition, Project, Service};
use crate::json::Json;
use crate::run::{self, Overrides};
use crate::wslc::{
    self, Container, LABEL_HASH, LABEL_NETWORK, LABEL_ONEOFF, LABEL_PROJECT, LABEL_SERVICE, LABEL_VOLUME,
};

type R = Result<(), String>;

fn st(v: &str) -> String {
    v.to_string()
}

fn info(msg: impl AsRef<str>) {
    eprintln!("{}", msg.as_ref());
}

// ---- 作成系 ------------------------------------------------------------

fn ensure_networks(p: &Project, services: &[&Service]) -> R {
    for n in &p.networks {
        if !services.iter().any(|s| s.networks.iter().any(|(k, _)| *k == n.key)) {
            continue;
        }
        if wslc::exists("network", &n.name) {
            continue;
        }
        if n.external {
            return Err(format!("外部ネットワーク `{}` がない (先に作る)", n.name));
        }
        let mut a = vec![st("network"), st("create")];
        if let Some(d) = &n.driver {
            a.extend([st("--driver"), d.clone()]);
        }
        if n.internal {
            a.push(st("--internal"));
        }
        for (k, v) in &n.labels {
            a.extend([st("--label"), format!("{k}={v}")]);
        }
        a.extend([
            st("--label"),
            format!("{LABEL_PROJECT}={}", p.name),
            st("--label"),
            format!("{LABEL_NETWORK}={}", n.key),
            n.name.clone(),
        ]);
        wslc::output(&a)?;
        info(format!("ネットワーク {} を作成", n.name));
    }
    Ok(())
}

fn ensure_volumes(p: &Project, services: &[&Service]) -> R {
    use crate::compose::MountSource;
    for v in &p.volumes {
        let used = services.iter().any(|s| s.volumes.iter().any(|m| m.source == MountSource::Volume(v.key.clone())));
        if !used || wslc::exists("volume", &v.name) {
            continue;
        }
        if v.external {
            return Err(format!("外部ボリューム `{}` がない (先に作る)", v.name));
        }
        let mut a = vec![st("volume"), st("create")];
        if let Some(d) = &v.driver {
            a.extend([st("--driver"), d.clone()]);
        }
        for (k, val) in &v.labels {
            a.extend([st("--label"), format!("{k}={val}")]);
        }
        a.extend([
            st("--label"),
            format!("{LABEL_PROJECT}={}", p.name),
            st("--label"),
            format!("{LABEL_VOLUME}={}", v.key),
            v.name.clone(),
        ]);
        wslc::output(&a)?;
        info(format!("ボリューム {} を作成", v.name));
    }
    Ok(())
}

fn image_id(image: &str) -> Option<String> {
    wslc::inspect("image", image).and_then(|j| j.get("Id").and_then(Json::as_str).map(str::to_string))
}

pub fn build_service(p: &Project, s: &Service, no_cache: bool, pull: bool) -> R {
    let Some(b) = &s.build else { return Ok(()) };
    let image = p.image_name(s);
    info(format!("{} をビルド ({image})", s.name));
    let mut a = vec![st("image"), st("build"), st("--tag"), image];
    if let Some(f) = &b.dockerfile {
        a.extend([st("--file"), f.display().to_string()]);
    }
    for (k, v) in &b.args {
        a.extend([st("--build-arg"), format!("{k}={v}")]);
    }
    if let Some(t) = &b.target {
        a.extend([st("--target"), t.clone()]);
    }
    if no_cache {
        a.push(st("--no-cache"));
    }
    if pull {
        a.push(st("--pull"));
    }
    a.push(b.context.display().to_string());
    wslc::run_ok(&a)
}

fn pull_image(image: &str) -> R {
    info(format!("{image} を取得"));
    wslc::run_ok(&[st("image"), st("pull"), st(image)])
}

/// イメージを用意して、その ID を返す
fn ensure_image(p: &Project, s: &Service, build: bool) -> Result<String, String> {
    let image = p.image_name(s);
    if s.build.is_some() && (build || image_id(&image).is_none()) {
        build_service(p, s, false, false)?;
    } else if s.build.is_none() && image_id(&image).is_none() {
        pull_image(&image)?;
    }
    image_id(&image).ok_or_else(|| format!("イメージ {image} がない"))
}

fn wait_for(p: &Project, s: &Service, timeout: Duration) -> R {
    for (dep, cond) in &s.depends_on {
        if *cond == Condition::Started {
            continue;
        }
        let d = p.service(dep).ok_or_else(|| format!("`{dep}` がない"))?;
        let name = p.container_name(d);
        let start = Instant::now();
        let mut said = false;
        loop {
            let st = wslc::inspect("container", &name);
            let state =
                st.as_ref().and_then(|j| j.path("State.Status")).and_then(Json::as_str).unwrap_or("").to_string();
            match cond {
                Condition::Healthy => {
                    let h = st.as_ref().and_then(|j| j.path("State.Health.Status")).and_then(Json::as_str);
                    match h {
                        Some("healthy") => break,
                        None if !state.is_empty() => {
                            return Err(format!("{dep} にヘルスチェックがないので service_healthy を待てない"));
                        }
                        _ => {}
                    }
                    if state == "exited" {
                        return Err(format!("{dep} が終了した"));
                    }
                }
                Condition::CompletedSuccessfully => {
                    if state == "exited" {
                        let code =
                            st.as_ref().and_then(|j| j.path("State.ExitCode")).and_then(Json::as_i64).unwrap_or(-1);
                        if code == 0 {
                            break;
                        }
                        return Err(format!("{dep} が終了コード {code} で終わった"));
                    }
                }
                Condition::Started => unreachable!(),
            }
            if !said {
                info(format!("{} が {dep} を待っている", s.name));
                said = true;
            }
            if start.elapsed() > timeout {
                return Err(format!("{dep} を {} 秒待ったがだめだった", timeout.as_secs()));
            }
            thread::sleep(Duration::from_millis(500));
        }
    }
    Ok(())
}

pub struct UpOptions {
    pub detach: bool,
    pub build: bool,
    pub force_recreate: bool,
    pub no_deps: bool,
    pub remove_orphans: bool,
    pub wait_timeout: Duration,
}

pub fn up(p: &Project, names: &[String], o: &UpOptions) -> R {
    let services = p.ordered(names, !o.no_deps)?;
    ensure_networks(p, &services)?;
    ensure_volumes(p, &services)?;
    let existing = wslc::containers(&p.name)?;
    orphans(p, &existing, o.remove_orphans)?;

    for s in &services {
        let id = ensure_image(p, s, o.build)?;
        let spec = run::spec(p, s, &id, &Overrides::default())?;
        let cur = existing.iter().find(|c| c.name == spec.name);
        match cur {
            Some(c) if !o.force_recreate && c.label(LABEL_HASH) == Some(spec.hash.as_str()) => {
                if c.running() {
                    info(format!("{} は最新", spec.name));
                } else {
                    wait_for(p, s, o.wait_timeout)?;
                    wslc::output(&[st("container"), st("start"), spec.name.clone()])?;
                    info(format!("{} を起動", spec.name));
                }
                continue;
            }
            Some(_) => {
                wslc::output(&[st("container"), st("remove"), st("--force"), spec.name.clone()])?;
                info(format!("{} を作り直す", spec.name));
            }
            None => {}
        }
        wait_for(p, s, o.wait_timeout)?;
        let mut a = vec![st("container"), st("run"), st("--detach")];
        a.extend(spec.args.iter().cloned());
        wslc::output(&a)?;
        for (net, aliases) in &spec.extra_networks {
            let mut c = vec![st("network"), st("connect")];
            for al in aliases {
                c.extend([st("--network-alias"), al.clone()]);
            }
            c.extend([net.clone(), spec.name.clone()]);
            wslc::output(&c)?;
        }
        info(format!("{} を起動", spec.name));
    }
    if o.detach {
        return Ok(());
    }
    // フォアグラウンド: ログを流し、Ctrl+C で停止する
    let names: Vec<String> = services.iter().map(|s| p.container_name(s)).collect();
    crate::signal::install();
    let width = names.iter().map(String::len).max().unwrap_or(0);
    let handles: Vec<_> =
        names.iter().enumerate().map(|(i, n)| follow_logs(n.clone(), prefix(n, width, i), true, None, false)).collect();
    loop {
        if crate::signal::interrupted() {
            info("\n停止中... (もう一度 Ctrl+C で強制終了)");
            break;
        }
        if handles.iter().all(|h| h.is_finished()) {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    let mut a = vec![st("container"), st("stop")];
    a.extend(names.iter().cloned());
    wslc::output(&a).map(|_| ())
}

fn orphans(p: &Project, existing: &[Container], remove: bool) -> R {
    for c in existing {
        let svc = c.label(LABEL_SERVICE).unwrap_or("");
        if c.label(LABEL_ONEOFF) == Some("True") || p.service(svc).is_some() {
            continue;
        }
        if remove {
            wslc::output(&[st("container"), st("remove"), st("--force"), c.name.clone()])?;
            info(format!("孤立したコンテナ {} を削除", c.name));
        } else {
            info(format!("警告: compose ファイルにないサービスのコンテナ {} がある (--remove-orphans で削除)", c.name));
        }
    }
    Ok(())
}

// ---- 停止・削除 ----------------------------------------------------------

pub fn down(p: &Project, volumes: bool, remove_orphans: bool) -> R {
    let existing = wslc::containers(&p.name)?;
    for c in &existing {
        let svc = c.label(LABEL_SERVICE).unwrap_or("");
        if !remove_orphans && p.service(svc).is_none() && c.label(LABEL_ONEOFF) != Some("True") {
            info(format!("警告: 孤立したコンテナ {} は残す (--remove-orphans で削除)", c.name));
            continue;
        }
        wslc::output(&[st("container"), st("remove"), st("--force"), c.name.clone()])?;
        info(format!("{} を削除", c.name));
    }
    for n in wslc::list_names("network", LABEL_PROJECT, &p.name)? {
        if p.networks.iter().any(|x| x.external && x.name == n) {
            continue;
        }
        wslc::output(&[st("network"), st("remove"), n.clone()])?;
        info(format!("ネットワーク {n} を削除"));
    }
    if volumes {
        for v in wslc::list_names("volume", LABEL_PROJECT, &p.name)? {
            wslc::output(&[st("volume"), st("remove"), v.clone()])?;
            info(format!("ボリューム {v} を削除"));
        }
    }
    Ok(())
}

/// start / stop / restart / kill をサービスのコンテナに
pub fn lifecycle(p: &Project, names: &[String], verb: &str) -> R {
    let mut services = p.ordered(names, false)?;
    if verb != "start" {
        services.reverse();
    }
    let existing = wslc::containers(&p.name)?;
    let targets: Vec<String> =
        services.iter().map(|s| p.container_name(s)).filter(|n| existing.iter().any(|c| &c.name == n)).collect();
    if targets.is_empty() {
        info("対象のコンテナがない");
        return Ok(());
    }
    let mut a = vec![st("container"), st(verb)];
    a.extend(targets.iter().cloned());
    wslc::output(&a)?;
    for t in targets {
        info(format!("{t}: {verb}"));
    }
    Ok(())
}

// ---- 参照系 --------------------------------------------------------------

pub fn ps(p: &Project, all: bool, quiet: bool) -> R {
    let list = wslc::containers(&p.name)?;
    let list: Vec<_> = list.iter().filter(|c| all || c.running()).collect();
    if quiet {
        for c in list {
            println!("{}", c.id);
        }
        return Ok(());
    }
    let rows: Vec<[String; 5]> = list
        .iter()
        .map(|c| {
            [
                c.name.clone(),
                c.label(LABEL_SERVICE).unwrap_or("").to_string(),
                c.image.clone(),
                c.status.clone(),
                c.ports.clone(),
            ]
        })
        .collect();
    table(&["NAME", "SERVICE", "IMAGE", "STATUS", "PORTS"], &rows);
    Ok(())
}

fn table<const N: usize>(head: &[&str; N], rows: &[[String; N]]) {
    let mut w = head.map(|h| h.chars().count());
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            out.push_str(c);
            if i + 1 < N {
                out.push_str(&" ".repeat(w[i] - c.chars().count() + 3));
            }
        }
        println!("{}", out.trim_end());
    };
    line(head.to_vec());
    for r in rows {
        line(r.iter().map(String::as_str).collect());
    }
}

pub struct LogOptions {
    pub follow: bool,
    pub tail: Option<String>,
    pub timestamps: bool,
    pub no_prefix: bool,
}

pub fn logs(p: &Project, names: &[String], o: &LogOptions) -> R {
    let services = p.ordered(names, false)?;
    let existing = wslc::containers(&p.name)?;
    let targets: Vec<String> =
        services.iter().map(|s| p.container_name(s)).filter(|n| existing.iter().any(|c| &c.name == n)).collect();
    if targets.is_empty() {
        info("ログを出すコンテナがない");
        return Ok(());
    }
    // 1 つで prefix なしなら、そのまま流す
    if targets.len() == 1 && o.no_prefix {
        let mut a = vec![st("container"), st("logs")];
        log_flags(&mut a, o.follow, o.tail.as_deref(), o.timestamps);
        a.push(targets[0].clone());
        return wslc::run_ok(&a);
    }
    crate::signal::install();
    let width = targets.iter().map(String::len).max().unwrap_or(0);
    let handles: Vec<_> = targets
        .iter()
        .enumerate()
        .map(|(i, n)| {
            follow_logs(
                n.clone(),
                if o.no_prefix { String::new() } else { prefix(n, width, i) },
                o.follow,
                o.tail.clone(),
                o.timestamps,
            )
        })
        .collect();
    while !handles.iter().all(|h| h.is_finished()) {
        if crate::signal::interrupted() {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn log_flags(a: &mut Vec<String>, follow: bool, tail: Option<&str>, timestamps: bool) {
    if follow {
        a.push(st("--follow"));
    }
    if let Some(t) = tail {
        a.extend([st("--tail"), t.to_string()]);
    }
    if timestamps {
        a.push(st("--timestamps"));
    }
}

static COLOR: AtomicBool = AtomicBool::new(true);

fn prefix(name: &str, width: usize, i: usize) -> String {
    const COLORS: [u8; 6] = [36, 33, 32, 35, 34, 31];
    let pad = " ".repeat(width - name.len());
    if COLOR.load(Ordering::Relaxed) && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{}m{name}{pad} |\x1b[0m ", COLORS[i % COLORS.len()])
    } else {
        format!("{name}{pad} | ")
    }
}

fn follow_logs(
    name: String,
    prefix: String,
    follow: bool,
    tail: Option<String>,
    timestamps: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut a = vec![st("container"), st("logs")];
        log_flags(&mut a, follow, tail.as_deref(), timestamps);
        a.push(name.clone());
        let child = wslc::command(&a).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let Ok(mut child) = child else { return };
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let p2 = prefix.clone();
        let t = thread::spawn(move || pipe_lines(err, &p2, true));
        pipe_lines(out, &prefix, false);
        let _ = t.join();
        let _ = child.wait();
    })
}

fn pipe_lines(r: impl std::io::Read, prefix: &str, stderr: bool) {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\n', '\r']);
                if stderr {
                    let _ = writeln!(std::io::stderr().lock(), "{prefix}{line}");
                } else {
                    let _ = writeln!(std::io::stdout().lock(), "{prefix}{line}");
                }
            }
        }
    }
}

// ---- 実行系 --------------------------------------------------------------

pub struct ExecOptions {
    pub no_tty: bool,
    pub detach: bool,
    pub env: Vec<String>,
    pub user: Option<String>,
    pub workdir: Option<String>,
}

pub fn exec(p: &Project, service: &str, cmd: &[String], o: &ExecOptions) -> Result<i32, String> {
    let s = p.service(service).ok_or_else(|| format!("サービス `{service}` がない"))?;
    let name = p.container_name(s);
    let mut a = vec![st("container"), st("exec")];
    if o.detach {
        a.push(st("--detach"));
    } else {
        a.push(st("--interactive"));
        if !o.no_tty && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            a.push(st("--tty"));
        }
    }
    for e in &o.env {
        a.extend([st("--env"), e.clone()]);
    }
    if let Some(u) = &o.user {
        a.extend([st("--user"), u.clone()]);
    }
    if let Some(w) = &o.workdir {
        a.extend([st("--workdir"), w.clone()]);
    }
    a.push(name);
    a.extend(cmd.iter().cloned());
    Ok(wslc::run(&a)?.code().unwrap_or(1))
}

pub struct RunOptions {
    pub rm: bool,
    pub detach: bool,
    pub no_deps: bool,
    pub no_tty: bool,
    pub service_ports: bool,
    pub publish: Vec<String>,
    pub env: Vec<(String, String)>,
    pub entrypoint: Option<String>,
    pub user: Option<String>,
    pub workdir: Option<String>,
    pub name: Option<String>,
    pub build: bool,
    pub wait_timeout: Duration,
}

/// 1 回だけのコンテナ (`compose run`)
pub fn run_once(p: &Project, service: &str, cmd: &[String], o: &RunOptions) -> Result<i32, String> {
    let s = p.service(service).ok_or_else(|| format!("サービス `{service}` がない"))?;
    if !o.no_deps && !s.depends_on.is_empty() {
        let deps: Vec<String> = s.depends_on.iter().map(|(d, _)| d.clone()).collect();
        up(
            p,
            &deps,
            &UpOptions {
                detach: true,
                build: o.build,
                force_recreate: false,
                no_deps: false,
                remove_orphans: false,
                wait_timeout: o.wait_timeout,
            },
        )?;
        wait_for(p, s, o.wait_timeout)?;
    } else {
        let only = p.ordered(&[service.to_string()], false)?;
        ensure_networks(p, &only)?;
        ensure_volumes(p, &only)?;
    }
    let id = ensure_image(p, s, o.build)?;
    let tty = !o.no_tty && !o.detach && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let ov = Overrides {
        oneoff: true,
        name: Some(o.name.clone().unwrap_or_else(|| format!("{}-{}-run-{}", p.name, s.name, short_id()))),
        command: if cmd.is_empty() { None } else { Some(cmd.to_vec()) },
        entrypoint: o.entrypoint.as_ref().map(|e| crate::compose::shell_split(e)).transpose()?,
        env: o.env.clone(),
        publish_ports: o.service_ports,
        tty: Some(tty),
        interactive: Some(!o.detach),
        remove: o.rm,
        workdir: o.workdir.clone(),
        user: o.user.clone(),
    };
    let spec = run::spec(p, s, &id, &ov)?;
    let mut a = vec![st("container"), st("run")];
    if o.detach {
        a.push(st("--detach"));
    }
    for port in &o.publish {
        a.extend([st("--publish"), port.clone()]);
    }
    a.extend(spec.args.iter().cloned());
    if !spec.extra_networks.is_empty() {
        info("警告: run では最初のネットワークにだけつなぐ");
    }
    Ok(wslc::run(&a)?.code().unwrap_or(1))
}

fn short_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:06x}", (n ^ u128::from(std::process::id())) & 0xff_ffff)
}

pub fn build(p: &Project, names: &[String], no_cache: bool, pull: bool) -> R {
    let services = p.ordered(names, false)?;
    let mut any = false;
    for s in services.iter().filter(|s| s.build.is_some()) {
        build_service(p, s, no_cache, pull)?;
        any = true;
    }
    if !any {
        info("ビルドするサービスがない");
    }
    Ok(())
}

pub fn pull(p: &Project, names: &[String]) -> R {
    for s in p.ordered(names, false)?.iter().filter(|s| s.build.is_none()) {
        pull_image(&p.image_name(s))?;
    }
    Ok(())
}

/// 解決した設定と、実行する wslc のコマンドを表示する
pub fn config(p: &Project, services_only: bool) -> R {
    if services_only {
        for s in &p.services {
            println!("{}", s.name);
        }
        return Ok(());
    }
    println!("# project: {}  ({})", p.name, p.dir.display());
    for n in &p.networks {
        println!("# network {} -> {}{}", n.key, n.name, if n.external { " (external)" } else { "" });
    }
    for v in &p.volumes {
        println!("# volume {} -> {}{}", v.key, v.name, if v.external { " (external)" } else { "" });
    }
    for s in p.ordered(&[], true)? {
        if let Some(b) = &s.build {
            println!("# {} は {} からビルド", s.name, b.context.display());
        }
        let spec = run::spec(p, s, "<image-id>", &Overrides::default())?;
        let args: Vec<String> = spec.args.iter().map(|a| crate::compose::shell_quote(a)).collect();
        println!("wslc container run --detach {}", args.join(" "));
        for (net, _) in &spec.extra_networks {
            println!("wslc network connect {net} {}", spec.name);
        }
    }
    Ok(())
}
