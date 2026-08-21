using System.Text.Json.Nodes;
using Mvf.Abstractions;

namespace Mvf.Hosting.Worker;

/// <summary>
/// <see cref="IOutOfProcessModuleHost"/> over local stdio (see <c>protocol/README.md</c>).
/// Translates a neutral <see cref="OutOfProcessModuleActivation"/> into a launch command for
/// the module's runtime, starts the child process, and wraps it as an
/// <see cref="IFrameClassifier"/>. Local only — no network.
///
/// <para>Uses the engine-owned <see cref="IDataPlane"/> (injected) so frames cross to the child as
/// handles rather than base64; the child maps the arena via the exported <c>MVF_ARENA_PATH</c>. The
/// host does not own the data plane — composition creates and disposes it.</para>
/// </summary>
public sealed class StdioModuleHost(IDataPlane dataPlane) : IOutOfProcessModuleHost
{
    public async Task<IFrameClassifier> CreateClassifierAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken)
    {
        var worker = await StartSupervisedAsync(activation, cancellationToken);
        return new WorkerFrameClassifier(worker, dataPlane);
    }

    public async Task<IFrameTransformer> CreateTransformerAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken)
    {
        var worker = await StartSupervisedAsync(activation, cancellationToken);
        return new WorkerFrameTransformer(worker, dataPlane);
    }

    public async Task<IFrameAnalyzer> CreateAnalyzerAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken)
    {
        var worker = await StartSupervisedAsync(activation, cancellationToken);
        return new WorkerFrameAnalyzer(worker, dataPlane);
    }

    // Starts the supervised worker, backed by a warm pool when MVF_WARM_SPARES > 0 so a restart swaps in a
    // pre-warmed spare instead of paying the cold-start (process spawn + model/device warmup). Default 0
    // keeps the original cold-restart behavior.
    private async Task<SupervisedWorker> StartSupervisedAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken)
    {
        var spawn = Spawn(activation);
        var warmSpares = ReadWarmSpares();
        var pool = warmSpares > 0
            ? await WarmWorkerPool.StartAsync(spawn, warmSpares, cancellationToken)
            : null;

        var worker = await SupervisedWorker.StartAsync(spawn, dataPlane, cancellationToken, pool);

        // The node's config, delivered before the first frame — this is the out-of-process equivalent of
        // handing an in-process module its config at OpenSession. Sent here rather than at spawn so it
        // also lands on a worker taken from the warm pool, which was started before this node existed.
        // Skipped silently when the module did not advertise the feature: it would not answer, and a
        // module that reads its config from a file of its own is not broken for lacking this.
        if (activation.Config is { } config && worker.SupportsConfigure)
        {
            try
            {
                await worker.ConfigureAsync(config, cancellationToken);
            }
            catch
            {
                await worker.DisposeAsync();
                throw;
            }
        }

        return worker;
    }

    private static int ReadWarmSpares() =>
        int.TryParse(Environment.GetEnvironmentVariable("MVF_WARM_SPARES"), out var n) && n > 0 ? n : 0;

    // A restartable spawn: the supervisor calls this to (re)launch the same module. The log sink is
    // captured here, so every restarted child forwards its logs/stderr the same way.
    private Func<CancellationToken, Task<StdioWorkerProcess>> Spawn(OutOfProcessModuleActivation activation)
    {
        var launch = BuildLaunchInfo(activation);
        var onLog = activation.OnLog;
        return token => StdioWorkerProcess.StartAsync(launch, onLog, token);
    }

    private WorkerLaunchInfo BuildLaunchInfo(OutOfProcessModuleActivation activation) =>
        activation.Runtime.ToLowerInvariant() switch
        {
            "python" => new WorkerLaunchInfo(
                Command: PythonCommand(),
                Args: [activation.EntryPath],
                WorkingDirectory: activation.WorkingDirectory,
                PythonPath: ResolvePythonSdkPath(activation.WorkingDirectory),
                ArenaPath: dataPlane.BackingPath,
                StartupBudget: ReadStartupBudget(activation.ModuleId)),

            "node" => new WorkerLaunchInfo(
                Command: Environment.GetEnvironmentVariable("MVF_NODE") ?? "node",
                Args: [activation.EntryPath],
                WorkingDirectory: activation.WorkingDirectory,
                ArenaPath: dataPlane.BackingPath,
                StartupBudget: ReadStartupBudget(activation.ModuleId)),

            // A compiled module (e.g. built with the C++ SDK): the entry *is* the executable.
            "native" => new WorkerLaunchInfo(
                Command: activation.EntryPath,
                Args: [],
                WorkingDirectory: activation.WorkingDirectory,
                ArenaPath: dataPlane.BackingPath,
                StartupBudget: ReadStartupBudget(activation.ModuleId)),

            _ => throw new NotSupportedException(
                $"Runtime '{activation.Runtime}' is not supported by the stdio worker host. Supported: python, node, native.")
        };

    private static string PythonCommand() =>
        Environment.GetEnvironmentVariable("MVF_PYTHON")
        ?? (OperatingSystem.IsWindows() ? "python" : "python3");

    private static TimeSpan? ReadStartupBudget(string moduleId)
    {
        var scopedKey = $"MVF_WORKER_STARTUP_BUDGET_SECONDS__{NormalizeModuleId(moduleId)}";
        if (TryReadSeconds(scopedKey, out var scoped))
        {
            return scoped;
        }

        return TryReadSeconds("MVF_WORKER_STARTUP_BUDGET_SECONDS", out var global)
            ? global
            : null;
    }

    private static bool TryReadSeconds(string key, out TimeSpan value)
    {
        value = default;
        var raw = Environment.GetEnvironmentVariable(key);
        if (!double.TryParse(raw, out var seconds) || seconds <= 0)
        {
            return false;
        }

        value = TimeSpan.FromSeconds(seconds);
        return true;
    }

    private static string NormalizeModuleId(string moduleId)
    {
        var chars = moduleId.Select(ch => char.IsLetterOrDigit(ch) ? char.ToUpperInvariant(ch) : '_').ToArray();
        return new string(chars);
    }

    /// <summary>
    /// Locates the Python SDK (<c>src/sdk/python</c>, which exports <c>mvf_sdk</c>) so the child's
    /// <c>PYTHONPATH</c> can import it. Honors <c>MVF_PYTHON_SDK</c>; otherwise walks up from the
    /// module directory to find the repo's SDK. Returns null if not found — a self-contained
    /// module that vendors its SDK still runs.
    /// </summary>
    private static string? ResolvePythonSdkPath(string workingDirectory)
    {
        var configured = Environment.GetEnvironmentVariable("MVF_PYTHON_SDK");
        if (!string.IsNullOrEmpty(configured) && Directory.Exists(configured))
        {
            return configured;
        }

        var dir = new DirectoryInfo(workingDirectory);
        while (dir is not null)
        {
            var candidate = Path.Combine(dir.FullName, "src", "sdk", "python");
            if (Directory.Exists(candidate))
            {
                return candidate;
            }

            dir = dir.Parent;
        }

        return null;
    }
}
