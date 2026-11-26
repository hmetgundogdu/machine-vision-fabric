# Gate A (issue #16) on Windows: every required check plus the self-verifying foundation demo.
#   pwsh rust/scripts/gate-a.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
function Step($name, [scriptblock]$cmd) {
  Write-Host "`n== $name"
  & $cmd
  if ($LASTEXITCODE -ne 0) { throw "$name failed with exit code $LASTEXITCODE" }
}
Step 'fmt'    { cargo fmt --all -- --check }
Step 'clippy' { cargo clippy --workspace --all-targets --locked -- -D warnings }
if (Get-Command cmake -ErrorAction SilentlyContinue) {
  Step 'C++ SDK example' { cmake -S ../src/sdk/cpp -B target/cpp-sdk -DCMAKE_BUILD_TYPE=Release; cmake --build target/cpp-sdk --config Release }
}
Step 'tests'  { cargo test --workspace --locked }
Step 'demo'   { cargo run --release --locked -q -p mvf-cli -- run examples/live-update/pipeline.yaml --updates examples/live-update/updates.yaml --verify --expect-promotions 2 --expect-rollbacks 1 | Out-Null }
Write-Host "`n== Gate A checks passed"
