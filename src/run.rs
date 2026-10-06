//! サービスの設定を `wslc run` の引数に変換する。

use crate::compose::{MountSource, Project, Service};
use crate::wslc::{LABEL_HASH, LABEL_NUMBER, LABEL_ONEOFF, LABEL_PROJECT, LABEL_SERVICE, LABEL_WORKDIR};

pub struct RunSpec {
    pub name: String,
    /// `wslc run` に渡す引数 (`run` と `--detach` は含まない)
    pub args: Vec<String>,
    /// 最初のネットワーク以外に、起動後に接続するネットワーク (名前, 別名)
    pub extra_networks: Vec<(String, Vec<String>)>,
    pub hash: String,
}

pub struct Overrides {
    pub oneoff: bool,
    pub name: Option<String>,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    pub publish_ports: bool,
    pub tty: Option<bool>,
    pub interactive: Option<bool>,
    pub remove: bool,
    pub workdir: Option<String>,
    pub user: Option<String>,
}

impl Default for Overrides {
    fn default() -> Self {
        Overrides {
            oneoff: false,
            name: None,
            command: None,
            entrypoint: None,
            env: vec![],
            publish_ports: true,
            tty: None,
            interactive: None,
            remove: false,
            workdir: None,
            user: None,
        }
    }
}

/// `image_id` はハッシュに含めて、イメージが変わったら作り直す
pub fn spec(p: &Project, s: &Service, image_id: &str, o: &Overrides) -> Result<RunSpec, String> {
    let name = o.name.clone().unwrap_or_else(|| p.container_name(s));
    let mut a: Vec<String> = Vec::new();
    macro_rules! push {
        ($k:expr, $v:expr) => {{
            a.push($k.to_string());
            a.push($v);
        }};
    }
    push!("--name", name.clone());

    let mut labels = s.labels.clone();
    labels.push((LABEL_PROJECT.into(), p.name.clone()));
    labels.push((LABEL_SERVICE.into(), s.name.clone()));
    labels.push((LABEL_NUMBER.into(), "1".into()));
    labels.push((LABEL_ONEOFF.into(), if o.oneoff { "True" } else { "False" }.into()));
    labels.push((LABEL_WORKDIR.into(), p.dir.display().to_string()));

    let mut networks = s.networks.iter().map(|(key, aliases)| {
        let n = p.network(key).ok_or_else(|| tr!("ネットワーク `{key}` がない", "no such network `{key}`"))?;
        // サービス名でも引けるように、サービス名を別名に入れる (compose と同じ)
        let mut al = vec![s.name.clone()];
        al.extend(aliases.iter().cloned());
        Ok::<_, String>((n.name.clone(), al))
    });
    let mut extra_networks = Vec::new();
    if let Some(first) = networks.next() {
        let (net, aliases) = first?;
        push!("--network", net);
        if !o.oneoff {
            for al in aliases {
                push!("--network-alias", al);
            }
        }
        for n in networks {
            extra_networks.push(n?);
        }
    }

    for (k, v) in s.environment.iter().chain(o.env.iter()) {
        push!("--env", format!("{k}={v}"));
    }
    if o.publish_ports {
        for port in &s.ports {
            push!("--publish", port.clone());
        }
    }
    for m in &s.volumes {
        let ro = if m.read_only { ":ro" } else { "" };
        match &m.source {
            MountSource::Bind(path) => push!("--volume", format!("{}:{}{ro}", path.display(), m.target)),
            MountSource::Volume(key) => {
                let v = p.volume(key).ok_or_else(|| tr!("ボリューム `{key}` がない", "no such volume `{key}`"))?;
                push!("--volume", format!("{}:{}{ro}", v.name, m.target));
            }
            MountSource::Anonymous => push!("--volume", m.target.clone()),
        }
    }
    for t in &s.tmpfs {
        push!("--tmpfs", t.clone());
    }
    if let Some(h) = &s.healthcheck {
        match &h.test {
            None => a.push("--no-healthcheck".into()),
            Some(test) => {
                push!("--health-cmd", test.clone());
                if let Some(v) = &h.interval {
                    push!("--health-interval", v.clone());
                }
                if let Some(v) = &h.timeout {
                    push!("--health-timeout", v.clone());
                }
                if let Some(v) = &h.retries {
                    push!("--health-retries", v.clone());
                }
                if let Some(v) = &h.start_period {
                    push!("--health-start-period", v.clone());
                }
            }
        }
    }
    if let Some(w) = o.workdir.as_ref().or(s.working_dir.as_ref()) {
        push!("--workdir", w.clone());
    }
    if let Some(u) = o.user.as_ref().or(s.user.as_ref()) {
        push!("--user", u.clone());
    }
    if let Some(h) = &s.hostname {
        push!("--hostname", h.clone());
    }
    if let Some(v) = &s.shm_size {
        push!("--shm-size", v.clone());
    }
    if let Some(v) = &s.stop_signal {
        push!("--stop-signal", v.clone());
    }
    if let Some(v) = &s.stop_grace_period {
        push!("--stop-timeout", duration_secs(v)?.to_string());
    }
    for (k, v) in &labels {
        push!("--label", format!("{k}={v}"));
    }

    // entrypoint は wslc では実行ファイル 1 つだけなので、残りは command の前に回す
    let entrypoint = o.entrypoint.as_ref().or(s.entrypoint.as_ref());
    let mut tail: Vec<String> = Vec::new();
    if let Some(ep) = entrypoint
        && let Some((first, rest)) = ep.split_first()
    {
        push!("--entrypoint", first.clone());
        tail.extend(rest.iter().cloned());
    }
    if o.tty.unwrap_or(s.tty) {
        a.push("--tty".into());
    }
    if o.interactive.unwrap_or(s.stdin_open) {
        a.push("--interactive".into());
    }
    if o.remove {
        a.push("--rm".into());
    }

    let image = p.image_name(s);
    // ハッシュは名前とラベル以外の設定 + イメージ ID から作る
    let hash = hex(fnv1a(
        a.iter()
            .filter(|x| !x.starts_with(&format!("{LABEL_HASH}=")))
            .chain(std::iter::once(&image))
            .chain(std::iter::once(&image_id.to_string()))
            .chain(o.command.as_ref().or(s.command.as_ref()).into_iter().flatten())
            .chain(extra_networks.iter().map(|(n, _)| n)),
    ));
    if !o.oneoff {
        a.push("--label".into());
        a.push(format!("{LABEL_HASH}={hash}"));
    }

    a.push(image);
    let command = o.command.as_ref().or(s.command.as_ref());
    a.extend(tail);
    if let Some(c) = command {
        a.extend(c.iter().cloned());
    }
    Ok(RunSpec { name, args: a, extra_networks, hash })
}

/// `1m30s` / `10s` / `500ms` を秒に (切り上げ)
pub fn duration_secs(s: &str) -> Result<u64, String> {
    let mut total_ms: u64 = 0;
    let mut num = String::new();
    let mut chars = s.trim().chars().peekable();
    if s.trim().chars().all(|c| c.is_ascii_digit()) {
        return s.trim().parse().map_err(|_| tr!("不正な時間: {s}", "invalid duration: {s}"));
    }
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let mut unit = c.to_string();
        if c == 'm' && chars.peek() == Some(&'s') {
            unit.push(chars.next().unwrap());
        }
        let n: f64 = num.parse().map_err(|_| tr!("不正な時間: {s}", "invalid duration: {s}"))?;
        num.clear();
        let ms = match unit.as_str() {
            "h" => n * 3_600_000.0,
            "m" => n * 60_000.0,
            "s" => n * 1000.0,
            "ms" => n,
            _ => return Err(tr!("不正な時間: {s}", "invalid duration: {s}")),
        };
        total_ms += ms as u64;
    }
    if !num.is_empty() {
        return Err(tr!("不正な時間: {s}", "invalid duration: {s}"));
    }
    Ok(total_ms.div_ceil(1000))
}

fn fnv1a<'a>(parts: impl Iterator<Item = &'a String>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{LoadOptions, load_str};
    use crate::interp::Env;
    use std::path::Path;

    fn project(src: &str) -> Project {
        let opts = LoadOptions { file: None, project_name: Some("demo".into()), env_files: vec![], profiles: vec![] };
        load_str(src, Path::new("/proj"), &Env::from_pairs(&[]), &opts).unwrap()
    }

    #[test]
    fn builds_run_args() {
        let p = project(
            "services:\n  web:\n    image: nginx\n    entrypoint: [\"/bin/sh\", \"-c\"]\n    command: [\"echo hi\"]\n    ports: [\"8080:80\"]\n    networks: [default, extra]\n    volumes: [\"data:/d:ro\"]\nnetworks:\n  extra:\nvolumes:\n  data:\n",
        );
        let s = p.service("web").unwrap();
        let r = spec(&p, s, "sha256:x", &Overrides::default()).unwrap();
        let joined = r.args.join(" ");
        assert!(joined.starts_with("--name demo-web-1 --network demo_default --network-alias web"), "{joined}");
        assert!(joined.contains("--publish 8080:80"));
        assert!(joined.contains("--volume demo_data:/d:ro"));
        assert!(joined.contains("--entrypoint /bin/sh"));
        assert!(joined.ends_with("nginx -c echo hi"), "{joined}");
        assert_eq!(r.extra_networks, vec![("demo_extra".to_string(), vec!["web".to_string()])]);
        let r2 = spec(&p, s, "sha256:y", &Overrides::default()).unwrap();
        assert_ne!(r.hash, r2.hash, "イメージが変わればハッシュも変わる");
    }

    #[test]
    fn durations() {
        assert_eq!(duration_secs("1m30s").unwrap(), 90);
        assert_eq!(duration_secs("500ms").unwrap(), 1);
        assert_eq!(duration_secs("10").unwrap(), 10);
        assert!(duration_secs("5x").is_err());
    }
}
