// Sample worker analyzer: the mean byte of a frame, as a classification ("dark" / "bright") and as
// a JSON value, with the number of frames it has seen as checkpointed state.
//
// The .NET twin of src/sdk/cpp/examples/mean_analyzer.cpp. On its first frame it writes one line
// to stdout from managed code (Console.WriteLine) and one from native code (the C runtime's
// write), the way a library might. Under protocol v2 the SDK has already moved the control channel
// off stdout, so both lines land in the engine's logs and the channel is untouched
// (protocol/worker-v2.md). Under v1 the engine logs them as non-protocol output.

using System.Runtime.InteropServices;
using System.Text.Json.Nodes;
using Mvf.Worker;

ulong seen = 0;
var hooks = new ModuleHooks
{
    OnCheckpoint = () => Output.Blob(BitConverter.GetBytes(seen)),
    OnRestore = state =>
    {
        if (state.Data.Length == sizeof(ulong))
        {
            seen = BitConverter.ToUInt64(state.Data);
        }
    },
};

return WorkerModule.RunAnalyzer("dotnet.mean-analyzer", (payload, _) =>
{
    if (seen++ == 0)
    {
        Console.WriteLine("managed stdout: first frame");
        NativeStdout.Write("native stdout: first frame\n");
        WorkerModule.Log("analyzing");
    }

    ulong sum = 0;
    foreach (var b in payload.Data)
    {
        sum += b;
    }

    var mean = payload.Data.Length == 0 ? 0.0 : (double)sum / payload.Data.Length;
    return new AnalysisResult(
        Classification: new Classification(mean < 128 ? "dark" : "bright", mean, "mean-byte"),
        Value: new JsonObject { ["mean"] = mean, ["frames"] = seen });
}, hooks);

/// <summary>Writes to file descriptor 1 through the C runtime, as native code would.</summary>
internal static unsafe class NativeStdout
{
    public static void Write(string text)
    {
        var lib = OperatingSystem.IsWindows()
            ? NativeLibrary.Load("ucrtbase.dll")
            : NativeLibrary.GetMainProgramHandle();
        var write = (delegate* unmanaged<int, byte*, nint, nint>)NativeLibrary.GetExport(lib,
            OperatingSystem.IsWindows() ? "_write" : "write");
        var bytes = System.Text.Encoding.UTF8.GetBytes(text);
        fixed (byte* p = bytes)
        {
            write(1, p, bytes.Length);
        }
    }
}
