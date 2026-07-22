namespace Mvf.Hosting.Worker;

/// <summary>
/// How to launch an out-of-process module worker (local child process; no network).
/// </summary>
public sealed record WorkerLaunchInfo(
    string Command,
    IReadOnlyList<string> Args,
    string WorkingDirectory,
    string? PythonPath = null);
