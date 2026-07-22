using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// Worker-crash supervision, end to end with a real Python worker: a <see cref="SupervisedWorker"/>
/// transparently restarts a crashed child, restores its last captured state, and retries the request —
/// so a flaky module does not take the run down and its state survives. Requires python3 on PATH.
/// </summary>
public sealed class SupervisedWorkerTests
{
    [Fact]
    public async Task SupervisedWorker_AfterCrash_RestartsRestoresAndContinues()
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

        await using var supervised = await SupervisedWorker.StartAsync(
            token => StdioWorkerProcess.StartAsync(launch, token), arena, cts.Token);
        var classifier = new WorkerFrameClassifier(supervised, arena);

        await classifier.ClassifyAsync(Frame(1), cts.Token);           // count 1
        var second = await classifier.ClassifyAsync(Frame(2), cts.Token); // count 2
        Assert.Equal(2d, second.Measurement);

        // Capture state (the supervisor remembers it), then crash the worker.
        await ((ICheckpointable)classifier).CheckpointAsync(cts.Token);
        supervised.KillCurrentWorkerForTest();

        // The next call transparently restarts the worker, restores count 2, and continues at 3.
        var third = await classifier.ClassifyAsync(Frame(3), cts.Token);
        Assert.Equal(3d, third.Measurement);
        Assert.Equal("odd", third.Label);

        // And it keeps working afterwards.
        var fourth = await classifier.ClassifyAsync(Frame(4), cts.Token);
        Assert.Equal(4d, fourth.Measurement);
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
