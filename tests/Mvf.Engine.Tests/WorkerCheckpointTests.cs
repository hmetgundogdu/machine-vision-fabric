using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// Resume-after-restart, end to end with a real Python worker: a stateful module's state (a frame
/// count) is captured through a shared-memory slot (no base64), and restored into a <b>fresh</b> worker
/// that continues where the first left off. Requires python3 on PATH.
/// </summary>
public sealed class WorkerCheckpointTests
{
    [Fact]
    public async Task StatefulClassifier_CheckpointThenRestoreIntoFreshWorker_ContinuesCount()
    {
        var repo = FindRepoRoot();
        var moduleDir = Path.Combine(repo, "modules", "py-frame-counter");

        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 4 });
        var info = new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        // First worker: process two frames (count -> 2), then capture state.
        byte[] state;
        await using (var worker1 = await StdioWorkerProcess.StartAsync(info, cts.Token))
        {
            var classifier = new WorkerFrameClassifier(worker1, arena);
            await classifier.ClassifyAsync(Frame(1), cts.Token);
            var second = await classifier.ClassifyAsync(Frame(2), cts.Token);
            Assert.Equal(2d, second.Measurement);

            var captured = await ((ICheckpointable)classifier).CheckpointAsync(cts.Token);
            Assert.NotNull(captured);
            state = captured!;
        }

        // A fresh worker (as if the first had crashed and been restarted): restore, then the next frame
        // is count 3 — the state survived, it did not reset to 1.
        await using (var worker2 = await StdioWorkerProcess.StartAsync(info, cts.Token))
        {
            var classifier = new WorkerFrameClassifier(worker2, arena);
            await ((ICheckpointable)classifier).RestoreAsync(state, cts.Token);

            var third = await classifier.ClassifyAsync(Frame(3), cts.Token);
            Assert.Equal(3d, third.Measurement);
            Assert.Equal("odd", third.Label);
        }
    }

    [Fact]
    public async Task StatelessClassifier_Checkpoint_ReportsNoState()
    {
        var repo = FindRepoRoot();
        var moduleDir = Path.Combine(repo, "modules", "py-brightness-classifier");

        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 4 });
        var info = new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        await using var worker = await StdioWorkerProcess.StartAsync(info, cts.Token);
        var classifier = new WorkerFrameClassifier(worker, arena);

        var captured = await ((ICheckpointable)classifier).CheckpointAsync(cts.Token);
        Assert.Null(captured); // brightness classifier declares no on_checkpoint
    }

    private static IFrameEnvelope Frame(int seq) =>
        new BinaryFrameEnvelope("cam1", seq, $"f{seq}.bmp", [0], "image/bmp");

    private static string FindRepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "CLAUDE.md"))
                && Directory.Exists(Path.Combine(dir.FullName, "modules")))
            {
                return dir.FullName;
            }
            dir = dir.Parent;
        }
        throw new InvalidOperationException("Repository root (with CLAUDE.md + modules/) not found.");
    }
}
