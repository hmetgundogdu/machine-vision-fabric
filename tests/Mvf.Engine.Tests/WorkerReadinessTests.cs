using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// Explicit worker readiness over stdio (L.2), end to end with a real Python worker. A module that warms
/// up asynchronously (loads a model / connects a device) sends <c>hello ready:false</c>, warms up, then
/// signals <c>ready</c>; the engine waits — bounded by a startup budget — before using it. A budget
/// overrun is a distinct <see cref="WorkerStartupException"/> (startup), not a mid-run crash (liveness).
/// Requires python3 on PATH. See docs/module-lifecycle-design.md.
/// </summary>
public sealed class WorkerReadinessTests
{
    [Fact]
    public async Task Worker_WithWarmup_SignalsReadyThenServes()
    {
        var repo = FindRepoRoot();
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 8 });
        var launch = Launch(repo, arena, startupBudget: TimeSpan.FromSeconds(30));

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        // StartAsync only returns once the worker has warmed up and signaled ready.
        await using var worker = await StdioWorkerProcess.StartAsync(launch, cts.Token);
        Assert.Equal("py.warmup-classifier", worker.ModuleId);

        var classifier = new WorkerFrameClassifier(worker, arena);
        var result = await classifier.ClassifyAsync(
            new BinaryFrameEnvelope("cam1", 1, "f1.bmp", [1, 2, 3], "image/bmp"), cts.Token);

        Assert.Equal("ok", result.Label);
        Assert.Equal(3d, result.Measurement); // three payload bytes, read after warmup
    }

    [Fact]
    public async Task Worker_WarmupOverrunsBudget_ThrowsStartupException()
    {
        var repo = FindRepoRoot();
        using var arena = new SharedMemoryArena(new SharedMemoryArenaOptions { SlotSize = 4096, SlotCount = 8 });

        // A warmup far longer than the budget: startup must fail fast and distinctly, not hang.
        var launch = Launch(repo, arena, startupBudget: TimeSpan.FromMilliseconds(300), warmupMs: 10_000);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(20));

        var started = DateTime.UtcNow;
        await Assert.ThrowsAsync<WorkerStartupException>(
            () => StdioWorkerProcess.StartAsync(launch, cts.Token));
        Assert.True(DateTime.UtcNow - started < TimeSpan.FromSeconds(5),
            "startup should fail near the budget, not wait for the full warmup");
    }

    private static WorkerLaunchInfo Launch(string repo, SharedMemoryArena arena, TimeSpan startupBudget, int? warmupMs = null)
    {
        var moduleDir = Path.Combine(repo, "modules", "py-warmup-classifier");
        var env = warmupMs is { } ms
            ? new Dictionary<string, string> { ["MVF_WARMUP_MS"] = ms.ToString() }
            : null;

        return new WorkerLaunchInfo(
            Command: "python3",
            Args: [Path.Combine(moduleDir, "classifier.py")],
            WorkingDirectory: moduleDir,
            PythonPath: Path.Combine(repo, "src", "sdk", "python"),
            ArenaPath: arena.BackingPath,
            StartupBudget: startupBudget,
            Environment: env);
    }

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
