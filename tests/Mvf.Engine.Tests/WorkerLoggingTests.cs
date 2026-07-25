using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// A co-located worker's <c>log</c> protocol message is forwarded to the host's log sink rather than
/// swallowed — the channel that lets a module's own logging reach the operator. Requires python3 on
/// PATH; the <c>py.invert-transformer</c> demo module emits a <c>log</c> line per frame.
/// </summary>
public sealed class WorkerLoggingTests
{
    [Fact]
    public async Task PythonWorker_LogMessage_ReachesOnLogSink()
    {
        var repo = FindRepoRoot();
        var moduleDir = Path.Combine(repo, "modules", "py-invert-transformer");

        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 4 });
        var info = new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "transformer.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath);

        var logs = new List<WorkerLogLine>();
        var gate = new object();

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        await using var worker = await StdioWorkerProcess.StartAsync(
            info,
            line => { lock (gate) { logs.Add(line); } },
            cts.Token);

        var transformer = new WorkerFrameTransformer(worker, arena);
        await transformer.TransformAsync(
            new BinaryFrameEnvelope("cam1", 1, "f1.bin", [1, 2, 3, 4], "application/octet-stream"),
            cts.Token);

        lock (gate)
        {
            Assert.Contains(logs, l => l.Level == "info" && l.Message.Contains("inverted"));
        }
    }

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
