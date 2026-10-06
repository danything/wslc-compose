# wslc-compose

日本語 | [English](README.en.md)

`compose.yaml` を WSL コンテナ (`wslc`) で動かす。wslc には compose も Docker 互換 API もまだないので、
docker compose のよく使う範囲だけを、依存クレートなしの Rust で実装している。

## インストール

WSL コンテナ (`wslc`) が使える Windows 11 が必要。

```powershell
winget install danything.wslc-compose
```

または [Releases](https://github.com/danything/wslc-compose/releases) から `wslc-compose.exe` をダウンロードして `PATH` の通った場所に置く。

## 使い方

```powershell
wslc-compose up -d
wslc-compose ps
wslc-compose logs -f web
wslc-compose exec web sh
wslc-compose run --rm web php artisan migrate
wslc-compose down -v
wslc-compose config   # 実際に実行する wslc のコマンドを表示
```

全コマンドは `wslc-compose --help`。メッセージは Windows の表示言語が日本語なら日本語、それ以外は英語
(`WSLC_COMPOSE_LANG=ja|en` で切り替えられる)。

## 対応している compose の機能

- `services`: `image` `build` (`context` `dockerfile` `args` `target`) `command` `entrypoint` `environment` `env_file`
  `ports` `volumes` (バインド / 名前付き / 名前なし) `tmpfs` `networks` (`aliases`) `depends_on`
  (`service_started` / `service_healthy` / `service_completed_successfully`) `healthcheck` `labels` `container_name`
  `working_dir` `user` `hostname` `tty` `stdin_open` `shm_size` `stop_signal` `stop_grace_period` `profiles`
- トップレベルの `name` `networks` (`external` `name` `driver` `internal`) `volumes` (`external` `name` `driver`)
- 変数展開 (`${VAR:-default}` など) と `.env`、YAML のアンカー / マージキー
- `up` は設定とイメージのハッシュをラベルに持ち、変わったコンテナだけ作り直す

wslc に対応する機能がない `restart` と `init` は警告を出して無視する。

## 開発

Windows に Rust を入れず、wslc のコンテナで Windows 向けにクロスコンパイルする。

```powershell
./dev/cargo.ps1 test
./dev/cargo.ps1 clippy --all-targets --target x86_64-pc-windows-gnu
./dev/cargo.ps1 build --release --target x86_64-pc-windows-gnu   # exe は wslc-compose-target ボリュームの中
```

`examples/demo` で一通りの動作を確認できる (web が redis の healthy を待って起動する)。

## リリース

`v*` のタグを push すると、CI が Windows でビルドして exe と `SHA256SUMS` をリリースに添付する。

## ライセンス

[AGPL-3.0-or-later](LICENSE)
