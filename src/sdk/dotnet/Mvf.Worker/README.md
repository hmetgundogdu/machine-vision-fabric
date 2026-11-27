# MachineVisionFabric.Worker

The .NET SDK for **worker modules**: out-of-process nodes that the engine launches as an
executable and drives over the process's stdio, with payloads in a shared-memory arena. It is
the .NET counterpart of the Python (`src/sdk/python/mvf_sdk`) and C++ (`src/sdk/cpp`) SDKs and
speaks the same protocol, so a .NET module can replace a Python or C++ one.

It is not `MachineVisionFabric.Sdk` (`../Mvf.Sdk`). That package builds in-process modules for the
.NET engine.

- **Control plane:** protocol v2 (binary frames, [`protocol/worker-v2.md`](../../../../protocol/worker-v2.md))
  when the engine offers it with `MVF_PROTOCOL=2`, otherwise v1 (JSON lines,
  [`protocol/README.md`](../../../../protocol/README.md)). One build works with every host.
- **Data plane:** the arena at `MVF_ARENA_PATH`. Payloads are `[192-byte descriptor | bytes]`,
  read in place and written into the engine's output slot. No bytes travel on the pipe.
- **No dependencies** beyond the base class library.

## Author API

```csharp
using Mvf.Worker;

return WorkerModule.RunProcessor("dotnet.invert-transformer", (payload, meta) =>
{
    var input = payload.Data;                  // ReadOnlySpan<byte> into the arena
    var inverted = new byte[input.Length];
    for (var i = 0; i < input.Length; i++)
        inverted[i] = (byte)(255 - input[i]);
    return Output.Blob(inverted);              // or Output.Tensor(bytes, type, shape); null drops
});
```

- `WorkerModule.RunClassifier(id, (payload, meta) => new Classification(label, measurement, unit))`
- `WorkerModule.RunAnalyzer(id, (payload, meta) => new AnalysisResult(Frame, Classification, Value))`
- `ModuleHooks { OnStart, OnCheckpoint, OnRestore }` for warmup and for state that survives a restart.
- `WorkerModule.Log(message, level)` writes a log line. Under v2 it goes to stderr, under v1 it is a `log` message.

`payload.Data` and `meta` are valid only during the callback: the SDK reuses them.

## Under protocol v2

- **The channel is protected.** The SDK keeps private duplicates of stdin and stdout for the
  protocol, then points stdout at stderr and stdin at the null device. This covers `Console`
  (`Console.SetOut`) and native code alike (`dup2` on Unix; `SetStdHandle` and the C runtime's
  descriptors on Windows). A stray `Console.WriteLine` or a native library's `printf` becomes a
  log line instead of corrupting the channel.
- **Hot path.** One `read` and one `write` per request, no JSON for a processor's result, and no
  per-frame string allocation for metadata.
- **macOS on Apple silicon.** The protocol descriptors stay inheritable by processes the module
  itself starts: `fcntl` is variadic and cannot be called through a fixed signature on that ABI.
  Every other platform marks them close-on-exec.

## Examples

`../examples/InvertTransformer` and `../examples/MeanAnalyzer` (the latter checkpoints a frame
count and deliberately writes to stdout from managed and native code). Build:

```bash
dotnet publish src/sdk/dotnet/examples/InvertTransformer -c Release -o rust/target/dotnet-sdk/InvertTransformer
```

The Rust runtime's tests run them through the real engine (`rust/crates/mvf-worker/tests/sdks.rs`).
A module's contract declares `runtime: executable` with the apphost as `entry`.

## License

Apache-2.0
