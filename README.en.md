# wslc-compose

[日本語](README.md) | English

Run `compose.yaml` on WSL containers (`wslc`). wslc has no Compose support and no Docker-compatible API yet,
so this implements the commonly used part of docker compose in Rust with no third-party crates.

```powershell
wslc-compose up -d
wslc-compose ps
wslc-compose logs -f web
wslc-compose exec web sh
wslc-compose run --rm web php artisan migrate
wslc-compose down -v
wslc-compose config   # show the wslc commands that would be run
```

See `wslc-compose --help` for all commands. Messages are in Japanese.

## Supported Compose features

- `services`: `image` `build` (`context` `dockerfile` `args` `target`) `command` `entrypoint` `environment` `env_file`
  `ports` `volumes` (bind / named / anonymous) `tmpfs` `networks` (`aliases`) `depends_on`
  (`service_started` / `service_healthy` / `service_completed_successfully`) `healthcheck` `labels` `container_name`
  `working_dir` `user` `hostname` `tty` `stdin_open` `shm_size` `stop_signal` `stop_grace_period` `profiles`
- Top-level `name`, `networks` (`external` `name` `driver` `internal`), `volumes` (`external` `name` `driver`)
- Variable interpolation (`${VAR:-default}` etc.) with `.env`, YAML anchors and merge keys
- `up` stores a hash of the config and image in a label and recreates only containers that changed

`restart` and `init` have no wslc equivalent; they are ignored with a warning.

## Development

No Rust toolchain on Windows is needed: the dev container cross-compiles for Windows with wslc.

```powershell
./dev/cargo.ps1 test
./dev/cargo.ps1 clippy --all-targets --target x86_64-pc-windows-gnu
./dev/cargo.ps1 build --release --target x86_64-pc-windows-gnu   # the exe lands in the wslc-compose-target volume
```

`examples/demo` exercises the main features (the web service waits for redis to become healthy).

## Release

Pushing a `v*` tag makes CI build on Windows and attach the exe and `SHA256SUMS` to a GitHub release.

## License

[AGPL-3.0-or-later](LICENSE)
