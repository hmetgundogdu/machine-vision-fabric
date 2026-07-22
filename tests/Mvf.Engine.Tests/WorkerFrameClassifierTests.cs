using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// End-to-end proof of the polyglot path: a Python classifier runs as a separate process,
/// talks to the engine over local stdio JSON, and drops into the same IFrameClassifier the
/// .NET graph already uses. Requires python3 on PATH.
/// </summary>
public sealed class WorkerFrameClassifierTests
{
    [Fact]
    public async Task PythonWorker_ClassifiesFramesOverStdio()
    {
        var repo = FindRepoRoot();
        var moduleDir = Path.Combine(repo, "modules", "py-brightness-classifier");
        var info = new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"));

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        await using var worker = await StdioWorkerProcess.StartAsync(info, cts.Token);
        Assert.Equal("py.brightness-classifier", worker.ModuleId);

        var classifier = new WorkerFrameClassifier(worker);

        var dark = await classifier.ClassifyAsync(Frame(1, new byte[64]), cts.Token); // all-zero → black
        Assert.Equal("black", dark.Label);
        Assert.Equal(0d, dark.Measurement);

        var bright = await classifier.ClassifyAsync(
            Frame(2, Enumerable.Repeat((byte)255, 64).ToArray()), cts.Token); // all-255 → ok
        Assert.Equal("ok", bright.Label);
        Assert.Equal(255d, bright.Measurement);
        Assert.StartsWith("worker:py.brightness-classifier", bright.Source);
    }

    [Fact]
    public async Task PythonWorker_ClassifiesFramesOverSharedMemory()
    {
        // Same proof as above, but the frame bytes cross via the shared-memory arena (handle over
        // stdio) instead of base64 — the M2 data-plane path end to end with a real Python worker.
        var repo = FindRepoRoot();
        var moduleDir = Path.Combine(repo, "modules", "py-brightness-classifier");

        using var arena = SharedMemoryArena.Create(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 4 });
        var info = new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.FilePath);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        await using var worker = await StdioWorkerProcess.StartAsync(info, cts.Token);
        var classifier = new WorkerFrameClassifier(worker, arena);

        var dark = await classifier.ClassifyAsync(Frame(1, new byte[64]), cts.Token); // all-zero → black
        Assert.Equal("black", dark.Label);
        Assert.Equal(0d, dark.Measurement);

        var bright = await classifier.ClassifyAsync(
            Frame(2, Enumerable.Repeat((byte)255, 64).ToArray()), cts.Token); // all-255 → ok
        Assert.Equal("ok", bright.Label);
        Assert.Equal(255d, bright.Measurement);
    }

    private static IFrameEnvelope Frame(int seq, byte[] data) =>
        new BinaryFrameEnvelope("cam1", seq, $"f{seq}.bmp", data, "image/bmp");

    private static string FindRepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "CLAUDE.md"))
                && Directory.Exists(Path.Combine(dir.FullName, "src")))
            {
                return dir.FullName;
            }
            dir = dir.Parent;
        }
        throw new InvalidOperationException("Repository root (with CLAUDE.md + src/) not found.");
    }
}
