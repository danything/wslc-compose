# 開発用コンテナで cargo を実行する。例: ./dev/cargo.ps1 test / ./dev/cargo.ps1 build --release
# target と cargo のキャッシュは wslc のボリュームに置く (bind mount 越しより速い)
$root = Split-Path $PSScriptRoot
$image = 'wslc-compose-dev:latest'
wslc image inspect $image *> $null
if ($LASTEXITCODE -ne 0) { wslc image build -t $image (Join-Path $root 'dev') | Out-Null }
wslc run --rm `
    -v "${root}:/src" `
    -v 'wslc-compose-target:/target' `
    -v 'wslc-compose-cargo:/usr/local/cargo/registry' `
    -e CARGO_TERM_COLOR=always `
    $image cargo @args
exit $LASTEXITCODE
