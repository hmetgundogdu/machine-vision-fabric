using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// Warm pools (L.4): a pool pre-warms spare workers (process spawn + L.2 warmup) so a restart or scale-up
/// swaps in a ready spare instead of paying the cold-start on the hot path. Verified with a real Python
/// worker: the pool pre-warms to target, an acquire returns a handshaked worker, and a supervised worker
/// recovers from a crash via the pool with its state intact. Requires python3 on PATH.
/// See docs/module-lifecycle-design.md.
/// </summary>
public sealed class WarmWorkerPoolTests
{
    [Fact]
    public async Task Pool_PreWarmsToTarget_AndAcquireReturnsAReadyWorker()
    {
        var repo = FindRepoRoot();
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 8 });
        var launch = WarmupLaunch(repo, arena);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        await using var pool = await WarmWorkerPool.StartAsync(
            token => StdioWorkerProcess.StartAsync(launch, token), targetSpares: 2, cts.Token);

        // StartAsync awaits the initial warmups, so the pool is deterministically full here.
        Assert.Equal(2, pool.ReadyCount);

        await using var worker = await pool.AcquireAsync(cts.Token);
        Assert.Equal("py.warmup-classifier", worker.ModuleId); // a handshaked, warmed-up spare
    }

    [Fact]
    public async Task SupervisedWorker_WithWarmPool_RecoversFromCrashWithStateIntact()
    {
        var repo = FindRepoRoot();
        var moduleDir = Path.Combine(repo, "modules", "py-frame-counter");
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 8 });
        var launch = new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        Task<StdioWorkerProcess> Spawn(CancellationToken t) => StdioWorkerProcess.StartAsync(launch, t);
        var pool = await WarmWorkerPool.StartAsync(Spawn, targetSpares: 1, cts.Token);

        // The supervisor owns and disposes the pool; every restart pulls a pre-warmed spare.
        await using var supervised = await SupervisedWorker.StartAsync(Spawn, arena, cts.Token, pool);
        var classifier = new WorkerFrameClassifier(supervised, arena);

        await classifier.ClassifyAsync(Frame(1), cts.Token);
        var second = await classifier.ClassifyAsync(Frame(2), cts.Token);
        Assert.Equal(2d, second.Measurement);

        await ((ICheckpointable)classifier).CheckpointAsync(cts.Token);
        supervised.KillCurrentWorkerForTest();

        // Recovery pulls a warm spare from the pool, restores count 2, and continues at 3.
        var third = await classifier.ClassifyAsync(Frame(3), cts.Token);
        Assert.Equal(3d, third.Measurement);
        Assert.Equal("odd", third.Label);
    }

    private static WorkerLaunchInfo WarmupLaunch(string repo, SharedMemoryArena arena)
    {
        var moduleDir = Path.Combine(repo, "modules", "py-warmup-classifier");
        return new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath);
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
