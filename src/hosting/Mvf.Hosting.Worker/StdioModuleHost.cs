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
        var worker = await StdioWorkerProcess.StartAsync(BuildLaunchInfo(activation), cancellationToken);
        return new WorkerFrameClassifier(worker, dataPlane);
    }

    public async Task<IFrameTransformer> CreateTransformerAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken)
    {
        var worker = await StdioWorkerProcess.StartAsync(BuildLaunchInfo(activation), cancellationToken);
        return new WorkerFrameTransformer(worker, dataPlane);
    }

    private WorkerLaunchInfo BuildLaunchInfo(OutOfProcessModuleActivation activation) =>
        activation.Runtime.ToLowerInvariant() switch
        {
            "python" => new WorkerLaunchInfo(
                Command: PythonCommand(),
                Args: [activation.EntryPath],
                WorkingDirectory: activation.WorkingDirectory,
                PythonPath: ResolvePythonSdkPath(activation.WorkingDirectory),
                ArenaPath: dataPlane.BackingPath),

            "node" => new WorkerLaunchInfo(
                Command: Environment.GetEnvironmentVariable("MVF_NODE") ?? "node",
                Args: [activation.EntryPath],
                WorkingDirectory: activation.WorkingDirectory,
                ArenaPath: dataPlane.BackingPath),

            _ => throw new NotSupportedException(
                $"Runtime '{activation.Runtime}' is not supported by the stdio worker host. Supported: python, node.")
        };

    private static string PythonCommand() =>
        Environment.GetEnvironmentVariable("MVF_PYTHON")
        ?? (OperatingSystem.IsWindows() ? "python" : "python3");

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
